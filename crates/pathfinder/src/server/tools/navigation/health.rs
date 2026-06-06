//! LSP health check and probe-based readiness verification.
//!
//! Provides `lsp_health_impl` which reports per-language LSP status
//! (`ready`, `warming_up`, `starting`, `unavailable`, `degraded`) along with
//! capability signals and degraded tool information.

use crate::server::helpers::{pathfinder_to_error_data, serialize_metadata};
use crate::server::PathfinderServer;
use pathfinder_common::error::PathfinderError;

/// Re-probe interval for "ready" languages to check liveness.
/// Re-probes every 2 minutes to detect LSPs that became non-responsive after
/// initial readiness (e.g., stuck indexing, memory pressure, internal deadlock).
const LIVENESS_PROBE_INTERVAL_SECS: u64 = 120;

impl PathfinderServer {
    /// Check LSP health status.
    ///
    /// Tests whether LSP navigation tools (`get_definition`, `analyze_impact`,
    /// `read_with_deep_context`) will return real data or degraded results.
    /// Agents should call this once at session start to choose their strategy.
    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self, params), fields(language = ?params.language))]
    pub(crate) async fn lsp_health_impl(
        &self,
        params: crate::server::types::LspHealthParams,
    ) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
        // IW-4: Handle action="restart" before the normal health query flow.
        if params.action.as_deref() == Some("restart") {
            let lang = match &params.language {
                Some(l) => l.clone(),
                None => {
                    return Err(pathfinder_to_error_data(&PathfinderError::IoError {
                        message: "lsp_health action='restart' requires 'language' to be set"
                            .to_owned(),
                    }));
                }
            };
            tracing::info!(language = %lang, "lsp_health: restart requested by agent");
            match self.lawyer.force_respawn(&lang).await {
                Ok(()) => {
                    tracing::info!(language = %lang, "lsp_health: restart successful");
                }
                Err(e) => {
                    tracing::warn!(language = %lang, error = %e, "lsp_health: restart failed");
                }
            }
            // Fall through to return updated health status after restart attempt.
        }

        let capability_status = self.lawyer.capability_status().await;

        let mut languages = Vec::new();
        let mut overall_status = "unavailable";

        for (lang, status) in &capability_status {
            if let Some(ref filter) = params.language {
                if lang != filter {
                    continue;
                }
            }

            // LSP-HEALTH-001: Two-phase readiness model
            // Primary gate: navigation_ready (initialize handshake + definitionProvider)
            // indexing_complete is an ADDITIONAL signal, not a requirement.
            let (status_str, uptime) = if status.navigation_ready == Some(true) {
                // Navigation is functional — report ready regardless of indexing status.
                // This makes get_definition, analyze_impact available immediately after
                // initialize completes, without waiting for WorkDoneProgressEnd.
                ("ready", status.uptime_seconds.map(format_uptime))
            } else if status.navigation_ready == Some(false)
                || status.indexing_complete == Some(false)
            {
                // Process is running but navigation is not yet functional (e.g.,
                // supports_definition=false) OR indexing still in progress but
                // navigation_ready is not confirmed. Still warming up.
                ("warming_up", status.uptime_seconds.map(format_uptime))
            } else if status.uptime_seconds.is_some() {
                // Process exists but no capability info yet (lazy start)
                ("starting", status.uptime_seconds.map(format_uptime))
            } else {
                ("unavailable", None)
            };

            // Compute indexing_status: independent signal for agents that want to wait
            // for full indexing. None when process not running.
            let indexing_status = match status.indexing_complete {
                Some(true) => Some("complete".to_owned()),
                Some(false) => Some("in_progress".to_owned()),
                None => None,
            };

            match status_str {
                "ready" => overall_status = "ready",
                "warming_up" if overall_status != "ready" => {
                    overall_status = "warming_up";
                }
                "starting" if overall_status != "ready" && overall_status != "warming_up" => {
                    overall_status = "starting";
                }
                _ => {}
            }

            languages.push(crate::server::types::LspLanguageHealth {
                language: lang.clone(),
                status: status_str.to_owned(),
                uptime,
                diagnostics_strategy: status.diagnostics_strategy.clone(),
                supports_call_hierarchy: status.supports_call_hierarchy,
                supports_diagnostics: status.supports_diagnostics,
                supports_definition: status.supports_definition,
                indexing_status,
                navigation_ready: status.navigation_ready,
                probe_verified: false,
                install_hint: None,
                indexing_progress_percent: status.indexing_progress_percent,
                degraded_tools: compute_degraded_tools(status),
                indexing_source: status.indexing_source.clone(),
                indexing_duration_secs: status.indexing_duration_secs,
            });
        }

        // PATCH-006: Probe-based readiness check
        // For languages that have been running for a while but still show warming_up,
        // fire a probe to verify actual readiness.
        //
        // Also handles the edge case where navigation_ready = Some(false) but the
        // LSP may actually be functional (e.g., capability detection was inaccurate
        // during early initialize).
        for lang_health in &mut languages {
            if lang_health.status == "warming_up" {
                // Check probe cache first — avoid redundant LSP calls.
                // Positive entries are cached indefinitely; negative entries
                // expire after PROBE_NEGATIVE_TTL_SECS (60s) to allow the LSP
                // to finish starting and be re-probed later.
                let cache_action = {
                    let cache = self
                        .probe_cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match cache.get(&lang_health.language) {
                        Some(entry) if entry.is_valid() && entry.success => {
                            // Valid positive entry — reuse cached result
                            ProbeAction::UseCachedReady
                        }
                        Some(entry) if entry.is_valid() && !entry.success => {
                            // Valid negative entry — skip probe, LSP still starting
                            ProbeAction::SkipProbe
                        }
                        Some(_) => {
                            // Expired negative entry — allow re-probe
                            ProbeAction::Probe
                        }
                        None => ProbeAction::Probe,
                    }
                };

                match cache_action {
                    ProbeAction::UseCachedReady => {
                        "ready".clone_into(&mut lang_health.status);
                        lang_health.probe_verified = true;
                        if overall_status != "ready" {
                            overall_status = "ready";
                        }
                        continue;
                    }
                    ProbeAction::SkipProbe => {
                        continue;
                    }
                    ProbeAction::Probe => {}
                }

                let uptime_secs = parse_uptime_to_seconds(lang_health.uptime.as_deref());
                if let Some(secs) = uptime_secs {
                    if secs > 10 {
                        // LSP has been running for 10+ seconds but still warming_up.
                        // This likely means progress notifications aren't being emitted.
                        // Fire a lightweight probe.
                        let probe_result =
                            self.probe_language_readiness(&lang_health.language).await;
                        if probe_result {
                            "ready".clone_into(&mut lang_health.status);
                            lang_health.probe_verified = true;
                            // Cache the successful probe result (indefinite TTL)
                            self.probe_cache
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(
                                    lang_health.language.clone(),
                                    crate::server::ProbeCacheEntry::new(true),
                                );
                            // Update overall status
                            if overall_status != "ready" {
                                overall_status = "ready";
                            }
                        } else {
                            // Cache negative result with TTL — allows re-probe after
                            // the LSP finishes starting
                            self.probe_cache
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(
                                    lang_health.language.clone(),
                                    crate::server::ProbeCacheEntry::new(false),
                                );
                        }
                    }
                }
            }
        }

        // LIVENESS PROBE for "ready" languages
        // Verify that languages that were "ready" at initialization are still responsive.
        // This catches LSPs that become non-responsive after initial readiness
        // (e.g., stuck indexing, memory pressure, internal deadlock).
        for lang_health in &mut languages {
            if lang_health.status != "ready" {
                continue;
            }

            // Check liveness cache
            let cache_action = {
                let cache = self
                    .probe_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match cache.get(&lang_health.language) {
                    Some(entry) if entry.is_valid() && entry.success => {
                        // Positive entry — check if it's time for a re-probe
                        if entry.age_secs() < LIVENESS_PROBE_INTERVAL_SECS {
                            ProbeAction::UseCachedReady
                        } else {
                            ProbeAction::Probe // Stale — re-probe
                        }
                    }
                    Some(entry) if entry.is_valid() && !entry.success => ProbeAction::SkipProbe,
                    Some(_) => {
                        ProbeAction::Probe // Expired
                    }
                    None => ProbeAction::Probe, // Never probed (shouldn't happen for "ready")
                }
            };

            match cache_action {
                ProbeAction::UseCachedReady => {
                    lang_health.probe_verified = true;
                    continue;
                }
                ProbeAction::SkipProbe => continue,
                ProbeAction::Probe => {}
            }

            // Run the same probe as warming_up
            // Note: find_probe_file returns None if no source file exists.
            // In this case, we skip the probe and don't downgrade the status.
            // The language remains "ready" based on capability status alone.
            let probe_result = match self.find_probe_file(&lang_health.language) {
                Some(_) => self.probe_language_readiness(&lang_health.language).await,
                None => {
                    // No file to probe — skip liveness check, keep status as-is
                    continue;
                }
            };

            if probe_result {
                // Still alive — cache positive result
                lang_health.probe_verified = true;
                self.probe_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        lang_health.language.clone(),
                        crate::server::ProbeCacheEntry::new(true),
                    );
            } else {
                // LSP is dead! Downgrade from "ready" to "degraded"
                "degraded".clone_into(&mut lang_health.status);
                lang_health.probe_verified = false;
                // Cache negative result
                self.probe_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        lang_health.language.clone(),
                        crate::server::ProbeCacheEntry::new(false),
                    );
            }
        }

        // Downgrade overall status if all ready languages are now degraded
        if !languages.iter().any(|l| l.status == "ready") && overall_status == "ready" {
            overall_status = "degraded";
        }

        // PATCH-008: Add missing languages (markers found but no LSP binary)
        // These are languages where we detected marker files (Cargo.toml, pyproject.toml, etc.)
        // but no LSP binary is on PATH. We show them as "unavailable" with install hints.
        let missing_languages = self.lawyer.missing_languages();
        for missing in &missing_languages {
            if let Some(ref filter) = params.language {
                if &missing.language_id != filter {
                    continue;
                }
            }

            languages.push(crate::server::types::LspLanguageHealth {
                language: missing.language_id.clone(),
                status: "unavailable".to_owned(),
                uptime: None,
                diagnostics_strategy: None,
                supports_call_hierarchy: None,
                supports_diagnostics: None,
                supports_definition: None,
                indexing_status: None,
                navigation_ready: None,
                probe_verified: false,
                install_hint: Some(missing.install_hint.clone()),
                indexing_progress_percent: None,
                degraded_tools: vec![
                    crate::server::types::DegradedToolInfo {
                        tool: "analyze_impact".to_owned(),
                        severity: "unavailable".to_owned(),
                        description:
                            "No LSP available. Use search_codebase for manual reference search."
                                .to_owned(),
                    },
                    crate::server::types::DegradedToolInfo {
                        tool: "read_with_deep_context".to_owned(),
                        severity: "unavailable".to_owned(),
                        description:
                            "No LSP available. Returns source only, no dependency signatures."
                                .to_owned(),
                    },
                ],
                indexing_source: None,
                indexing_duration_secs: None,
            });
        }

        if languages.is_empty() && params.language.is_none() {
            overall_status = "unavailable";
        }

        let mut known_limitations = Vec::new();

        if !missing_languages.is_empty() {
            let langs: Vec<&str> = missing_languages
                .iter()
                .map(|m| m.language_id.as_str())
                .collect();
            known_limitations.push(format!(
                "Missing LSP binaries for: {}. Install them for full navigation support.",
                langs.join(", ")
            ));
        }

        for lang_health in &languages {
            if lang_health.supports_call_hierarchy == Some(false)
                && lang_health.supports_definition == Some(true)
            {
                known_limitations.push(format!(
                    "{}: call hierarchy not supported — analyze_impact uses grep fallback (less accurate)",
                    lang_health.language
                ));
            }
        }

        if !self.lawyer.is_warm_start_complete() {
            known_limitations.push(
                "LSP warm_start still in progress — results may be incomplete until indexing finishes"
                    .to_owned(),
            );
        }

        let response = crate::server::types::LspHealthResponse {
            status: overall_status.to_owned(),
            languages,
            warm_start_complete: self.lawyer.is_warm_start_complete(),
            known_limitations,
        };

        // Build a concise human-readable summary for the text channel.
        // Agents reading plain text get actionable status without parsing JSON.
        let mut lang_lines = Vec::new();
        for l in &response.languages {
            let mut detail_parts = Vec::new();
            if l.probe_verified {
                detail_parts.push("probe_verified".to_owned());
            }
            // Spec 5.3: Show indexing status with progress percentage
            if let Some(ref idx) = l.indexing_status {
                if let Some(pct) = l.indexing_progress_percent {
                    detail_parts.push(format!("indexing: {pct}%"));
                } else if idx == "complete" {
                    detail_parts.push("indexing: 100% complete".to_owned());
                } else {
                    detail_parts.push(format!("indexing: {idx}"));
                }
            } else if let Some(pct) = l.indexing_progress_percent {
                detail_parts.push(format!("indexing: {pct}%"));
            }
            if let Some(ref uptime) = l.uptime {
                detail_parts.push(format!("uptime: {uptime}"));
            }
            let details = if detail_parts.is_empty() {
                String::new()
            } else {
                format!(" ({})", detail_parts.join(", "))
            };
            lang_lines.push(format!("{}: {}{}", l.language, l.status, details));

            if !l.degraded_tools.is_empty() {
                let tools_with_severity: Vec<_> = l
                    .degraded_tools
                    .iter()
                    .map(|t| format!("{} ({})", t.tool, t.severity))
                    .collect();

                lang_lines.push(format!(
                    "  ⚠️ degraded_tools: {}",
                    tools_with_severity.join(", ")
                ));

                let mut reasons = Vec::new();
                if l.supports_definition != Some(true) {
                    reasons.push("supports_definition = false");
                }
                if l.supports_call_hierarchy != Some(true) {
                    reasons.push("supports_call_hierarchy = false");
                }
                if l.supports_diagnostics != Some(true) {
                    reasons.push("supports_diagnostics = false");
                }
                if !reasons.is_empty() {
                    lang_lines.push(format!(
                        "  → Reason: {}. Use search_codebase as fallback.",
                        reasons.join(", ")
                    ));
                }
            }
        }

        let text = if lang_lines.is_empty() {
            format!("LSP status: {} — no languages detected", response.status)
        } else {
            let mut parts = vec![
                format!("LSP status: {}", response.status),
                lang_lines.join("\n"),
            ];
            if !response.known_limitations.is_empty() {
                parts.push(format!(
                    "Known limitations:\n{}",
                    response
                        .known_limitations
                        .iter()
                        .map(|l| format!("  - {l}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
            parts.join("\n")
        };

        let mut res = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&response);
        Ok(res)
    }

    /// Probe whether an LSP is actually ready by attempting a lightweight operation.
    async fn probe_language_readiness(&self, language_id: &str) -> bool {
        let probe_file = self.find_probe_file(language_id);
        let Some(file_path) = probe_file else {
            return false;
        };

        let content = tokio::fs::read_to_string(self.workspace_root.path().join(&file_path))
            .await
            .unwrap_or_default();

        let _ = self
            .lawyer
            .open_document(self.workspace_root.path(), &file_path, &content)
            .await;

        // Wrap in a 5s budget — for a health probe we only need "does it respond",
        // not real data. This caps worst-case probe time instead of inheriting
        // the production goto_definition timeout (10s).
        let probe_timeout = std::time::Duration::from_secs(5);

        let result = tokio::time::timeout(
            probe_timeout,
            self.lawyer
                .goto_definition(self.workspace_root.path(), &file_path, 1, 1),
        )
        .await;

        let Ok(result) = result else {
            tracing::warn!(
                language = %language_id,
                timeout_secs = 5,
                "probe: goto_definition timed out — LSP not responsive"
            );
            return false;
        };

        if result.is_err() {
            return false;
        }

        let caps = self.lawyer.capability_status().await;
        if let Some(status) = caps.get(language_id) {
            if status.supports_call_hierarchy == Some(true) {
                let call_hierarchy_result = tokio::time::timeout(
                    probe_timeout,
                    self.lawyer.call_hierarchy_prepare(
                        self.workspace_root.path(),
                        &file_path,
                        1,
                        1,
                    ),
                )
                .await;

                let Ok(call_hierarchy_result) = call_hierarchy_result else {
                    tracing::warn!(
                        language = %language_id,
                        timeout_secs = 5,
                        "probe: call_hierarchy_prepare timed out — LSP partially responsive"
                    );
                    return false;
                };

                if call_hierarchy_result.is_err() {
                    tracing::warn!(
                        language = %language_id,
                        "probe: goto_definition succeeded but call_hierarchy_prepare failed — LSP may be partially responsive"
                    );
                    return false;
                }
            }
        }

        true
    }

    /// Find a well-known file in the workspace for probing language readiness.
    pub(crate) fn find_probe_file(&self, language_id: &str) -> Option<std::path::PathBuf> {
        let extensions: &[&str] = match language_id {
            "rust" => &["rs"],
            "go" => &["go"],
            "typescript" => &["ts", "tsx"],
            "javascript" => &["js", "jsx"],
            "python" => &["py"],
            "ruby" => &["rb"],
            "java" => &["java"],
            _ => return None,
        };

        // First try well-known paths (fast path)
        let candidates = match language_id {
            "rust" => vec!["src/main.rs", "src/lib.rs"],
            "go" => vec!["main.go", "cmd/main.go"],
            "typescript" => vec![
                "src/index.ts",
                "index.ts",
                "src/main.ts",
                "src/index.tsx",
                "index.tsx",
                "src/main.tsx",
            ],
            "javascript" => vec![
                "src/index.js",
                "index.js",
                "src/main.js",
                "src/index.jsx",
                "index.jsx",
                "src/main.jsx",
            ],
            "python" => vec!["src/__init__.py", "main.py", "setup.py", "__init__.py"],
            "ruby" => vec!["lib/main.rb", "main.rb"],
            "java" => vec!["src/main/java/Main.java"],
            _ => vec![],
        };

        for candidate in candidates {
            let path = self.workspace_root.path().join(candidate);
            if path.exists() {
                return Some(std::path::PathBuf::from(candidate));
            }
        }

        // LSP-HEALTH-001 Task 3.1: Fallback to depth-limited recursive scan for monorepos
        // Scans up to depth 4 looking for any file with matching extension.
        // Returns relative path to first match.
        self.find_file_by_extension_recursive(self.workspace_root.path(), extensions, 0, 4)
    }

    /// Recursive helper for `find_probe_file`: depth-limited scan for any file
    /// with matching extension. Returns relative path from workspace root.
    fn find_file_by_extension_recursive(
        &self,
        current_dir: &std::path::Path,
        extensions: &[&str],
        current_depth: usize,
        max_depth: usize,
    ) -> Option<std::path::PathBuf> {
        if current_depth > max_depth {
            return None;
        }

        let Ok(entries) = std::fs::read_dir(current_dir) else {
            return None;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };

            if metadata.is_dir() {
                // Skip hidden directories and common build/test dirs
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.')
                        || name == "node_modules"
                        || name == "target"
                        || name == "vendor"
                        || name == "dist"
                        || name == "build"
                        || name == "__pycache__"
                        || name == ".git"
                    {
                        continue;
                    }
                }
                // Recurse
                if let Some(found) = self.find_file_by_extension_recursive(
                    &path,
                    extensions,
                    current_depth + 1,
                    max_depth,
                ) {
                    return Some(found);
                }
            } else if metadata.is_file() {
                // Check extension
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if extensions.iter().any(|&e| e.eq_ignore_ascii_case(ext)) {
                        // Found a match - return relative path from workspace root
                        if let Ok(rel_path) = path.strip_prefix(self.workspace_root.path()) {
                            return Some(rel_path.to_path_buf());
                        }
                    }
                }
            }
        }
        None
    }
}

/// Format uptime in seconds as a human-readable string.
pub(super) fn format_uptime(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        let mins = seconds / 60;
        let secs = seconds % 60;
        if secs == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m{secs}s")
        }
    } else {
        let hours = seconds / 3600;
        let mins = (seconds % 3600) / 60;
        if mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{mins}m")
        }
    }
}

/// Decision from checking the probe cache for a language.
pub(super) enum ProbeAction {
    /// Cached positive result exists — upgrade to "ready" immediately.
    UseCachedReady,
    /// Cached negative result exists and hasn't expired — skip probing.
    SkipProbe,
    /// No cache entry or expired negative — perform a live probe.
    Probe,
}

/// Returns structured information about tools that lose LSP support for this language.
///
/// Each entry includes the tool name, severity level, and description of the fallback behavior.
pub(super) fn compute_degraded_tools(
    status: &pathfinder_lsp::types::LspLanguageStatus,
) -> Vec<crate::server::types::DegradedToolInfo> {
    let mut degraded = Vec::new();

    if status.supports_definition != Some(true) {
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "get_definition".to_owned(),
            severity: "grep_fallback".to_owned(),
            description:
                "Uses ripgrep heuristic instead of LSP. May find wrong definition or miss re-exports."
                    .to_owned(),
        });
    }

    if status.supports_call_hierarchy != Some(true) {
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "analyze_impact".to_owned(),
            severity: "grep_fallback".to_owned(),
            description:
                "Uses text search instead of call hierarchy. May over/under-count references."
                    .to_owned(),
        });
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "read_with_deep_context".to_owned(),
            severity: "unavailable".to_owned(),
            description:
                "Returns source only, no dependency signatures. Use search_codebase as alternative."
                    .to_owned(),
        });
    }

    degraded
}

pub(super) fn parse_uptime_to_seconds(uptime: Option<&str>) -> Option<u64> {
    let uptime = uptime?;
    let mut seconds = 0u64;

    // Parse hours
    if let Some(h_pos) = uptime.find('h') {
        let h_str = &uptime[..h_pos];
        if let Ok(h) = h_str.parse::<u64>() {
            seconds += h * 3600;
        }
    }

    // Parse minutes
    let min_part = if let Some(h_pos) = uptime.find('h') {
        &uptime[h_pos + 1..]
    } else {
        uptime
    };

    if let Some(m_pos) = min_part.find('m') {
        let m_str = &min_part[..m_pos];
        if let Ok(m) = m_str.parse::<u64>() {
            seconds += m * 60;
        }
    }

    // Parse seconds
    let sec_part = if let Some(m_pos) = min_part.find('m') {
        &min_part[m_pos + 1..]
    } else {
        // min_part already equals uptime when no 'h', so we can just use min_part
        min_part
    };

    if let Some(s_pos) = sec_part.find('s') {
        let s_str = &sec_part[..s_pos];
        if let Ok(s) = s_str.parse::<u64>() {
            seconds += s;
        }
    }

    Some(seconds)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::super::test_helpers::make_server_with_lawyer;
    use super::*;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    /// Extract `LspHealthResponse` from a `CallToolResult.structured_content`.
    fn unpack_health(res: rmcp::model::CallToolResult) -> crate::server::types::LspHealthResponse {
        serde_json::from_value(res.structured_content.expect("structured_content")).unwrap()
    }

    // ── PATCH-005: Per-Language Capabilities Tests ─────────────────────

    #[tokio::test]
    async fn test_lsp_health_includes_diagnostics_strategy() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        // No lawyer_clone needed - MockLawyer returns empty status
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // MockLawyer returns empty capability_status, so no languages should be returned
        // This tests the structure exists and doesn't panic
        let params = crate::server::types::LspHealthParams::default();
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.status, "unavailable");
        assert!(val.languages.is_empty());
    }

    #[tokio::test]
    async fn test_lsp_health_shows_push_for_go() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a Go LSP with push diagnostics
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "go".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(15),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(false),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("go".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let go_health = &val.languages[0];
        assert_eq!(go_health.language, "go");
        assert_eq!(go_health.status, "ready");
        assert_eq!(go_health.diagnostics_strategy, Some("push".to_string()));
        assert_eq!(go_health.supports_call_hierarchy, Some(true));
        assert_eq!(go_health.supports_diagnostics, Some(true));
    }

    #[tokio::test]
    async fn test_lsp_health_shows_pull_for_rust() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a Rust LSP with pull diagnostics
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(20),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert_eq!(rust_health.status, "ready");
        assert_eq!(rust_health.diagnostics_strategy, Some("pull".to_string()));
        assert_eq!(rust_health.supports_call_hierarchy, Some(true));
        assert_eq!(rust_health.supports_diagnostics, Some(true));
    }

    #[tokio::test]
    async fn test_lsp_health_shows_capabilities() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock an LSP with partial capabilities
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "typescript".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(10),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true), // TS supports call hierarchy
                supports_diagnostics: Some(true),

                supports_formatting: Some(false),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams::default();
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let ts_health = &val.languages[0];
        assert_eq!(ts_health.supports_definition, Some(true));
        assert_eq!(ts_health.supports_call_hierarchy, Some(true));
        assert_eq!(ts_health.supports_diagnostics, Some(true));
    }

    // ── PATCH-006: Probe-Based Readiness Tests ─────────────────────────

    #[tokio::test]
    async fn test_lsp_health_probe_upgrades_warming_up_to_ready() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        // Create a workspace with a main.rs file for probing
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/main.rs"),
            r#"fn main() { println!("Hello"); }"#,
        )
        .unwrap();

        // Mock a Rust LSP that's been warming up for 30 seconds
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(false), // Still warming up
                uptime_seconds: Some(30),       // 30 seconds - should trigger probe
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // Mock successful goto_definition response (LSP is ready)
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // With two-phase readiness model: navigation_ready = Some(true) means
        // status is immediately "ready" without waiting for indexing.
        // This is the fix for LSP-HEALTH-001: LSPs that support definitionProvider
        // should be usable immediately, without waiting for WorkDoneProgressEnd.
        // Liveness probe also runs for "ready" languages to verify
        // the LSP is still responsive.
        assert_eq!(val.status, "ready");
        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert_eq!(rust_health.status, "ready");
        assert_eq!(rust_health.uptime, Some("30s".to_string()));
        // indexing_status is still "in_progress" because we never saw WorkDoneProgressEnd
        assert_eq!(rust_health.indexing_status, Some("in_progress".to_string()));
        // With liveness probe, probe_verified should be true since
        // the probe ran and succeeded (LSP is responsive)
        assert!(rust_health.probe_verified);
    }

    #[tokio::test]
    async fn test_lsp_health_probe_keeps_warming_up_when_probe_fails() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        // Create a workspace with a main.rs file for probing
        // Create a workspace with a main.rs file for probing
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Mock a Rust LSP that's been warming up for 30 seconds
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(false), // Still warming up
                uptime_seconds: Some(30),       // 30 seconds - should trigger probe
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // Mock failed goto_definition response (LSP is not responsive)
        lawyer.set_goto_definition_result(Err(pathfinder_lsp::LspError::ConnectionLost));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // With liveness probe, when the LSP was "ready" but becomes
        // non-responsive, the status should be downgraded to "degraded".
        // This is the key improvement: detecting LSPs that die after initialization.
        assert_eq!(val.status, "degraded");
        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert_eq!(rust_health.status, "degraded");
        assert!(!rust_health.probe_verified);
    }

    #[tokio::test]
    async fn test_lsp_health_no_probe_for_recently_started() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Remove Rust files created by make_temp_workspace to prevent liveness probe
        let src_dir = ws_dir.path().join("src");
        let _ = std::fs::remove_file(src_dir.join("main.rs"));
        let _ = std::fs::remove_file(src_dir.join("auth.rs"));
        let _ = std::fs::remove_file(src_dir.join("token.rs"));
        let _ = std::fs::remove_file(src_dir.join("service.rs"));
        let _ = std::fs::remove_file(src_dir.join("user.rs"));
        let _ = std::fs::remove_file(src_dir.join("auth.go"));

        // Mock a Rust LSP that just started (5 seconds ago)
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(false), // Warming up
                uptime_seconds: Some(5),        // Only 5 seconds - should NOT trigger probe
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // Set a goto_definition result to verify it's not called
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // With two-phase readiness: navigation_ready = Some(true) means status
        // is immediately "ready" - uptime doesn't matter when capability is confirmed.
        assert_eq!(val.status, "ready");
        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert_eq!(rust_health.status, "ready");
        assert_eq!(rust_health.indexing_status, Some("in_progress".to_string()));
        assert!(!rust_health.probe_verified);
    }

    #[tokio::test]
    async fn test_lsp_health_no_probe_for_already_ready() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Remove Rust files created by make_temp_workspace to prevent liveness probe
        let src_dir = ws_dir.path().join("src");
        let _ = std::fs::remove_file(src_dir.join("main.rs"));
        let _ = std::fs::remove_file(src_dir.join("auth.rs"));
        let _ = std::fs::remove_file(src_dir.join("token.rs"));
        let _ = std::fs::remove_file(src_dir.join("service.rs"));
        let _ = std::fs::remove_file(src_dir.join("user.rs"));
        let _ = std::fs::remove_file(src_dir.join("auth.go"));

        // Mock a Rust LSP that's already ready
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true), // Ready
                uptime_seconds: Some(60),      // 60 seconds
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // Set a goto_definition result to verify it's not called
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // Status should be "ready" and probe not attempted
        assert_eq!(val.status, "ready");
        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.status, "ready");
        assert!(!rust_health.probe_verified);
    }

    #[tokio::test]
    async fn test_parse_uptime_to_seconds() {
        assert_eq!(parse_uptime_to_seconds(Some("5s")), Some(5));
        assert_eq!(parse_uptime_to_seconds(Some("1m30s")), Some(90));
        assert_eq!(parse_uptime_to_seconds(Some("2h15m")), Some(8100));
        assert_eq!(parse_uptime_to_seconds(Some("1h30m45s")), Some(5445));
        assert_eq!(parse_uptime_to_seconds(Some("1m")), Some(60));
        assert_eq!(parse_uptime_to_seconds(Some("1h")), Some(3600));
        assert_eq!(parse_uptime_to_seconds(None), None);
    }

    #[tokio::test]
    async fn test_find_probe_file() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        // Create some probe files
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

        // Remove Rust files created by make_temp_workspace to test "no Rust file" scenario
        let src_dir = ws_dir.path().join("src");
        let _ = std::fs::remove_file(src_dir.join("main.rs"));
        let _ = std::fs::remove_file(src_dir.join("auth.rs"));
        let _ = std::fs::remove_file(src_dir.join("token.rs"));
        let _ = std::fs::remove_file(src_dir.join("service.rs"));
        let _ = std::fs::remove_file(src_dir.join("user.rs"));
        let _ = std::fs::remove_file(src_dir.join("auth.go"));

        // Create test probe files
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(ws_dir.path().join("main.go"), "package main").unwrap();
        std::fs::write(src_dir.join("index.ts"), "export const x = 1;").unwrap();

        // Test finding probe files
        assert_eq!(
            server.find_probe_file("go"),
            Some(std::path::PathBuf::from("main.go"))
        );
        assert_eq!(
            server.find_probe_file("typescript"),
            Some(std::path::PathBuf::from("src/index.ts"))
        );
        assert_eq!(server.find_probe_file("rust"), None); // No Rust file
    }

    // ── LSP-HEALTH-001: Recursive Probe for Monorepos ───────────────────────

    #[tokio::test]
    async fn test_find_probe_file_recursive_monorepo() {
        // Test the fallback recursive scan for monorepo layouts where
        // files are at non-standard paths like apps/backend/cmd/main.go
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

        // Create a monorepo structure: Go file at apps/backend/cmd/server/main.go
        // (not at the standard main.go or cmd/main.go)
        std::fs::create_dir_all(
            ws_dir
                .path()
                .join("apps")
                .join("backend")
                .join("cmd")
                .join("server"),
        )
        .unwrap();
        std::fs::write(
            ws_dir
                .path()
                .join("apps")
                .join("backend")
                .join("cmd")
                .join("server")
                .join("main.go"),
            "package main\nfunc main() {}",
        )
        .unwrap();

        // Create a node_modules directory to test that it's skipped
        std::fs::create_dir_all(ws_dir.path().join("node_modules").join("react")).unwrap();
        std::fs::write(
            ws_dir
                .path()
                .join("node_modules")
                .join("react")
                .join("index.ts"),
            "export const React = {};",
        )
        .unwrap();

        // Test that recursive scan finds the Go file at non-standard path
        let probe = server.find_probe_file("go");
        assert!(probe.is_some(), "Should find Go file in monorepo structure");
        let probe_path = probe.unwrap();
        assert!(
            probe_path.to_str().unwrap().contains("main.go"),
            "Should find a main.go file, got: {probe_path:?}"
        );

        // Test that node_modules is skipped (should NOT find the TS file there)
        // This is a bit tricky to test without other TS files - let's just verify
        // the probe works for a standard pattern too by adding a deeper Python file
        std::fs::create_dir_all(ws_dir.path().join("tools").join("fath-factory").join("src"))
            .unwrap();
        std::fs::write(
            ws_dir
                .path()
                .join("tools")
                .join("fath-factory")
                .join("src")
                .join("__init__.py"),
            "",
        )
        .unwrap();

        let py_probe = server.find_probe_file("python");
        assert!(
            py_probe.is_some(),
            "Should find Python file in tools/ directory"
        );
    }

    // ── PATCH-008: Install Guidance Tests ─────────────────────────────────

    #[tokio::test]
    async fn test_lsp_health_includes_missing_languages_with_install_hint() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a detected language (TypeScript with running LSP)
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "typescript".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(false),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // Mock missing languages (Python and Go with markers but no LSP binaries)
        lawyer_clone.set_missing_languages(vec![
            pathfinder_lsp::client::MissingLanguage {
                language_id: "python".to_string(),
                marker_file: "pyproject.toml".to_string(),
                tried_binaries: vec!["pyright".to_string(), "pylsp".to_string()],
                install_hint: "Install pyright: npm install -g pyright".to_string(),
            },
            pathfinder_lsp::client::MissingLanguage {
                language_id: "go".to_string(),
                marker_file: "go.mod".to_string(),
                tried_binaries: vec!["gopls".to_string()],
                install_hint: "Install gopls: go install golang.org/x/tools/gopls@latest"
                    .to_string(),
            },
        ]);

        let params = crate::server::types::LspHealthParams::default();
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // Should have 3 languages total: 1 detected + 2 missing
        assert_eq!(val.languages.len(), 3);

        // Find the missing languages
        let python_health = val.languages.iter().find(|l| l.language == "python");
        let go_health = val.languages.iter().find(|l| l.language == "go");
        let ts_health = val.languages.iter().find(|l| l.language == "typescript");

        // TypeScript should be ready
        assert!(ts_health.is_some());
        assert_eq!(ts_health.unwrap().status, "ready");

        // Python and Go should be unavailable with install hints
        assert!(python_health.is_some());
        assert_eq!(python_health.unwrap().status, "unavailable");
        assert_eq!(
            python_health.unwrap().install_hint,
            Some("Install pyright: npm install -g pyright".to_string())
        );

        assert!(go_health.is_some());
        assert_eq!(go_health.unwrap().status, "unavailable");
        assert_eq!(
            go_health.unwrap().install_hint,
            Some("Install gopls: go install golang.org/x/tools/gopls@latest".to_string())
        );
    }

    #[tokio::test]
    async fn test_lsp_health_missing_language_filter_works() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // No detected languages, only missing ones
        lawyer_clone.set_capability_status(std::collections::HashMap::new());
        lawyer_clone.set_missing_languages(vec![
            pathfinder_lsp::client::MissingLanguage {
                language_id: "python".to_string(),
                marker_file: "pyproject.toml".to_string(),
                tried_binaries: vec!["pyright".to_string()],
                install_hint: "Install pyright".to_string(),
            },
            pathfinder_lsp::client::MissingLanguage {
                language_id: "rust".to_string(),
                marker_file: "Cargo.toml".to_string(),
                tried_binaries: vec!["rust-analyzer".to_string()],
                install_hint: "Install rust-analyzer".to_string(),
            },
        ]);

        // Filter by language = python
        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("python".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // Should only return Python, not Rust
        assert_eq!(val.languages.len(), 1);
        assert_eq!(val.languages[0].language, "python");
        assert_eq!(
            val.languages[0].install_hint,
            Some("Install pyright".to_string())
        );
    }

    // ── PATCH-010: Degraded Tools and Validation Latency Tests ─────────────

    #[tokio::test]
    async fn test_health_shows_degraded_tools_for_no_diagnostics() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock an LSP without diagnostics or call hierarchy support
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "go".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: None,
                supports_definition: Some(true),
                supports_call_hierarchy: None,
                supports_diagnostics: None,

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("go".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let go_health = &val.languages[0];
        assert_eq!(go_health.language, "go");

        // Check that degraded_tools contains analyze_impact with correct severity
        let analyze_impact = go_health
            .degraded_tools
            .iter()
            .find(|t| t.tool == "analyze_impact");
        assert!(
            analyze_impact.is_some(),
            "degraded_tools should include analyze_impact when call hierarchy unsupported"
        );
        let ai = analyze_impact.unwrap();
        assert_eq!(
            ai.severity, "grep_fallback",
            "analyze_impact should have severity=grep_fallback"
        );
        assert!(
            ai.description.contains("text search"),
            "analyze_impact description should mention text search fallback"
        );

        // Check that degraded_tools contains read_with_deep_context with correct severity
        let rwdc = go_health
            .degraded_tools
            .iter()
            .find(|t| t.tool == "read_with_deep_context");
        assert!(
            rwdc.is_some(),
            "degraded_tools should include read_with_deep_context when call hierarchy unsupported"
        );
        let rwdc = rwdc.unwrap();
        assert_eq!(
            rwdc.severity, "unavailable",
            "read_with_deep_context should have severity=unavailable"
        );
        assert!(
            rwdc.description.contains("source only"),
            "read_with_deep_context description should mention source-only limitation"
        );

        // validate_only no longer exists — degraded_tools only contains LSP navigation tools
        let has_validate_only = go_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "validate_only");
        assert!(
            !has_validate_only,
            "degraded_tools must not include the removed validate_only tool"
        );
    }

    #[tokio::test]
    async fn test_health_shows_empty_degraded_when_fully_capable() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a fully capable LSP
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert!(
            rust_health.degraded_tools.is_empty(),
            "degraded_tools should be empty when all capabilities supported, got: {:?}",
            rust_health.degraded_tools
        );
    }

    #[tokio::test]
    async fn test_health_shows_push_latency() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a push diagnostics language (Go)
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "go".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("go".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let go_health = &val.languages[0];
        assert_eq!(go_health.language, "go");
        assert!(
            go_health.degraded_tools.is_empty(),
            "fully capable LSP should have no degraded tools"
        );
    }

    #[tokio::test]
    async fn test_health_shows_pull_latency() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a pull diagnostics language (Rust)
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        result.expect("pull-diagnostics language should return successfully");
    }

    // ── LSP-HEALTH-001: Confidence Gradient Tests ─────────────────────────────

    #[tokio::test]
    async fn test_lsp_health_ready_but_still_indexing_shows_confidence_gradient() {
        // Simulate pyright: navigation_ready=true (definitionProvider confirmed),
        // but indexing_complete=false (no WorkDoneProgressEnd received).
        // The agent should see BOTH signals and make smart decisions.
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "python".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: false, // No diagnostics support
                reason: "LSP connected but does not support diagnostics".to_string(),
                navigation_ready: Some(true), // definitionProvider confirmed
                indexing_complete: Some(false), // Still indexing
                uptime_seconds: Some(5),
                diagnostics_strategy: Some("none".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(false),

                supports_formatting: Some(false),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("python".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        let py_health = &val.languages[0];
        // Status is "ready" because navigation_ready=true
        assert_eq!(py_health.status, "ready");
        // But indexing is still in progress — agent should see this
        assert_eq!(py_health.indexing_status, Some("in_progress".to_string()));
        // navigation_ready is surfaced so agent knows navigation is functional
        assert_eq!(py_health.navigation_ready, Some(true));
        // Diagnostics not available
        assert_eq!(py_health.diagnostics_strategy, Some("none".to_string()));
        // validate_only no longer exists — diagnostics absence only affects call hierarchy tools
        let has_validate_only = py_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "validate_only");
        assert!(!has_validate_only);
    }

    #[tokio::test]
    async fn test_lsp_health_fully_indexed_shows_complete_confidence() {
        // Simulate rust-analyzer after full indexing: both signals at max confidence.
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),  // Navigation ready
                indexing_complete: Some(true), // Indexing complete
                uptime_seconds: Some(120),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        let rust_health = &val.languages[0];
        assert_eq!(rust_health.status, "ready");
        // Both confidence signals at max
        assert_eq!(rust_health.navigation_ready, Some(true));
        assert_eq!(rust_health.indexing_status, Some("complete".to_string()));
        // No degraded tools
        assert!(rust_health.degraded_tools.is_empty());
    }

    // ── Probe cache TTL tests (LSP-HEALTH-001 findings 1+2) ──────────

    #[tokio::test]
    async fn test_probe_cache_positive_result_never_expires() {
        // Positive cache entries should be valid indefinitely
        let entry = crate::server::ProbeCacheEntry::new(true);
        assert!(entry.is_valid(), "positive entry should always be valid");
    }

    #[tokio::test]
    async fn test_probe_cache_negative_result_is_initially_valid() {
        // Negative cache entries should be valid immediately after creation
        let entry = crate::server::ProbeCacheEntry::new(false);
        assert!(entry.is_valid(), "fresh negative entry should be valid");
    }

    #[tokio::test]
    async fn test_probe_negative_cache_skips_reprobe() {
        // When a negative cache entry exists, lsp_health should skip probing
        // and keep the status as "warming_up" instead of hammering the LSP.
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Pre-populate cache with a negative result
        server
            .probe_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                "rust".to_string(),
                crate::server::ProbeCacheEntry::new(false),
            );

        // LSP running but not ready (navigation_ready = false)
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(false),
                indexing_complete: Some(false),
                uptime_seconds: Some(30), // Over 10s threshold
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(false),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        let rust_health = &val.languages[0];
        // Status should stay "warming_up" because cached negative result skipped the probe
        assert_eq!(rust_health.status, "warming_up");
        assert!(
            !rust_health.probe_verified,
            "should not be probe-verified when using negative cache"
        );
    }

    #[tokio::test]
    async fn test_probe_cache_positive_upgrades_to_ready() {
        // When a positive cache entry exists, lsp_health should upgrade to ready
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Pre-populate cache with a positive result
        server
            .probe_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                "rust".to_string(),
                crate::server::ProbeCacheEntry::new(true),
            );

        // LSP reports warming_up but cache has positive result
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(false),
                indexing_complete: Some(false),
                uptime_seconds: Some(30),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(false),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        let rust_health = &val.languages[0];
        assert_eq!(rust_health.status, "ready");
        assert!(
            rust_health.probe_verified,
            "should be probe-verified from cache"
        );
    }

    // ── Liveness Probe Tests ────────────────────────────────────

    #[tokio::test]
    async fn test_lsp_health_liveness_probe_downgrades_dead_lsp() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Create a file for probing
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Mock a "ready" LSP that was working but now times out
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(120),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // Mock goto_definition timeout (LSP is dead)
        lawyer.set_goto_definition_result(Err(pathfinder_lsp::LspError::Timeout {
            operation: "goto_definition".to_string(),
            timeout_ms: 10000,
        }));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        // Status should be downgraded to "degraded"
        assert_eq!(val.status, "degraded");
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.status, "degraded");
        assert!(!rust_health.probe_verified);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_lsp_health_liveness_probe_caches_positive() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Create a file for probing
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Mock a "ready" LSP that is still responsive
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(120),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // Mock successful goto_definition
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        // First call - should probe and cache
        let result1 = server
            .lsp_health_impl(crate::server::types::LspHealthParams {
                action: None,
                language: Some("rust".to_string()),
            })
            .await;
        let val1 = unpack_health(result1.expect("should succeed"));
        assert!(val1.languages[0].probe_verified);

        // Verify cache was populated
        let cache = server.probe_cache.lock().unwrap();
        assert!(cache.contains_key("rust"));
        let entry = cache.get("rust").unwrap();
        assert!(entry.success);
        drop(cache);

        // Second call - should use cache (no second probe)
        let call_count_before = lawyer.goto_definition_call_count();
        let result2 = server
            .lsp_health_impl(crate::server::types::LspHealthParams {
                action: None,
                language: Some("rust".to_string()),
            })
            .await;
        let val2 = unpack_health(result2.expect("should succeed"));
        assert!(val2.languages[0].probe_verified);
        // Goto definition should not be called again (cache hit)
        assert_eq!(lawyer.goto_definition_call_count(), call_count_before);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_liveness_probe_interval_skips_recent() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Create a file for probing
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Mock a "ready" LSP
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(120),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // Pre-populate cache with a recent positive entry (age < LIVENESS_PROBE_INTERVAL_SECS)
        let mut cache = server.probe_cache.lock().unwrap();
        cache.insert(
            "rust".to_string(),
            crate::server::ProbeCacheEntry::new(true),
        );
        drop(cache);

        // Mock goto_definition - should NOT be called due to cache
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };

        let call_count_before = lawyer.goto_definition_call_count();
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        // Should use cached result without probing
        assert!(val.languages[0].probe_verified);
        assert_eq!(lawyer.goto_definition_call_count(), call_count_before);
    }

    #[tokio::test]
    async fn test_lsp_health_probe_downgrades_when_call_hierarchy_hangs() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/main.rs"),
            r#"fn main() { println!("Hello"); }"#,
        )
        .unwrap();

        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(30),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),
                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
            },
        )]));

        // goto_definition succeeds (basic LSP works)
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        // call_hierarchy_prepare FAILS (LSP is hung for call hierarchy)
        lawyer.push_prepare_call_hierarchy_result(Err(pathfinder_lsp::LspError::Timeout {
            operation: "textDocument/prepareCallHierarchy".to_string(),
            timeout_ms: 5000,
        }));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(
            val.languages.len(),
            1,
            "should have exactly 1 language entry"
        );
        let rust_health = &val.languages[0];

        // The LSP should be downgraded from "ready" to "degraded" because
        // the call hierarchy probe failed even though goto_definition succeeded.
        assert_eq!(
            rust_health.status, "degraded",
            "should be degraded when call_hierarchy probe fails despite goto_definition succeeding"
        );
        assert!(
            !rust_health.probe_verified,
            "probe_verified must be false when call hierarchy probe fails"
        );
    }
}
