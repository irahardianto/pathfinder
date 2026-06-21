//! LSP health check and probe-based readiness verification.
//!
//! Provides `lsp_health_impl` (internal) which reports per-language LSP status
//! (`ready`, `warming_up`, `starting`, `unavailable`, `degraded`) along with
//! capability signals and degraded tool information.

use crate::server::helpers::serialize_metadata;
use crate::server::PathfinderServer;

/// Re-probe interval for "ready" languages to check liveness.
/// Re-probes every 2 minutes to detect LSPs that became non-responsive after
/// initial readiness (e.g., stuck indexing, memory pressure, internal deadlock).
const LIVENESS_PROBE_INTERVAL_SECS: u64 = 120;

impl PathfinderServer {
    /// Check LSP health status.
    ///
    /// Tests whether LSP navigation tools (`locate`, `trace`, `inspect`)
    /// will return real data or degraded results.
    /// Agents should call this once at session start to choose their strategy.
    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self, params), fields(language = ?params.language))]
    pub(crate) async fn lsp_health_impl(
        &self,
        params: crate::server::types::HealthParams,
    ) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
        if let Some(ref action) = params.action {
            if action != "restart" {
                return Err(crate::server::helpers::invalid_params_error(
                    "invalid `action`: must be 'restart'",
                ));
            }
        }

        // IW-4: Handle action="restart" before the normal health query flow.
        if params.action.as_deref() == Some("restart") {
            let lang = match &params.language {
                Some(l) => l.clone(),
                None => {
                    return Err(crate::server::helpers::invalid_params_error(
                        "health action='restart' requires 'language' to be set",
                    ));
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

            // Track LSP start time
            if let Some(uptime_secs) = status.uptime_seconds {
                let now = std::time::Instant::now();
                let mut started_at_map = self
                    .lsp_started_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let calculated_start = now
                    .checked_sub(std::time::Duration::from_secs(uptime_secs))
                    .unwrap_or(now);
                if let Some(existing_start) = started_at_map.get(lang) {
                    let diff = if *existing_start > calculated_start {
                        *existing_start - calculated_start
                    } else {
                        calculated_start - *existing_start
                    };
                    if diff.as_secs() > 5 {
                        started_at_map.insert(lang.clone(), calculated_start);
                    }
                } else {
                    started_at_map.insert(lang.clone(), calculated_start);
                }
            } else {
                let mut started_at_map = self
                    .lsp_started_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                started_at_map.remove(lang);
            }

            // LSP-HEALTH-001: Two-phase readiness model
            // Primary gate: navigation_ready (initialize handshake + definitionProvider)
            // indexing_complete is an ADDITIONAL signal, not a requirement.
            let (status_str, uptime) = if status.navigation_ready == Some(true) {
                // Navigation is functional — report ready regardless of indexing status.
                // This makes locate, trace, inspect available immediately after
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
                navigation_tested: None,
                call_hierarchy_verified: false,
                install_hint: None,
                server_name: status.server_name.clone(),
                registrations_received: status.registrations_received,
                indexing_progress_percent: status.indexing_progress_percent,
                degraded_tools: compute_degraded_tools(status),
                indexing_source: status.indexing_source.clone(),
                indexing_duration_secs: status.indexing_duration_secs,
                last_probe_age_secs: None,
            });
        }

        // Unified liveness probing, cache evaluation, and force_probe handling
        for lang_health in &mut languages {
            if lang_health.status == "unavailable" {
                continue;
            }

            // Verify if a probe file exists for navigation testing
            let Some(_) = self.find_probe_file(&lang_health.language) else {
                continue;
            };

            let mut should_probe = false;
            let force_probe = params.force_probe.unwrap_or(false);

            if force_probe {
                should_probe = true;
            } else {
                let cache = self
                    .probe_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match cache.get(&lang_health.language) {
                    Some(entry) => {
                        let interval = self.get_probe_interval(&lang_health.language);
                        let threshold = std::cmp::min(30, interval);
                        if entry.age_secs() < threshold {
                            if entry.success {
                                if lang_health.status == "warming_up"
                                    || lang_health.status == "starting"
                                {
                                    lang_health.status = "ready".to_string();
                                }
                                lang_health.probe_verified = true;
                                lang_health.navigation_tested = Some(true);
                                lang_health.call_hierarchy_verified = entry.call_hierarchy_verified;
                                if overall_status != "ready" {
                                    overall_status = "ready";
                                }
                            } else {
                                if lang_health.status == "ready" {
                                    lang_health.status = "degraded".to_string();
                                }
                                lang_health.probe_verified = false;
                                lang_health.navigation_tested = Some(false);
                                lang_health.call_hierarchy_verified = false;
                            }
                        } else {
                            should_probe = true;
                        }
                    }
                    None => {
                        // No cache entry
                        if lang_health.status == "ready" {
                            should_probe = true;
                        } else {
                            let uptime_secs =
                                parse_uptime_to_seconds(lang_health.uptime.as_deref());
                            if uptime_secs.unwrap_or(0) > 10 {
                                should_probe = true;
                            }
                        }
                    }
                }
            }

            if should_probe {
                let (probe_result, call_hierarchy_verified) =
                    self.probe_language_readiness(&lang_health.language).await;

                // Cache the result
                self.probe_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        lang_health.language.clone(),
                        crate::server::ProbeCacheEntry::new(probe_result, call_hierarchy_verified),
                    );

                if probe_result {
                    lang_health.status = "ready".to_string();
                    lang_health.probe_verified = true;
                    lang_health.navigation_tested = Some(true);
                    lang_health.call_hierarchy_verified = call_hierarchy_verified;
                    if overall_status != "ready" {
                        overall_status = "ready";
                    }
                } else {
                    if lang_health.status == "ready" {
                        lang_health.status = "degraded".to_string();
                    }
                    lang_health.probe_verified = false;
                    lang_health.navigation_tested = Some(false);
                    lang_health.call_hierarchy_verified = false;
                }
            }
        }

        // Final pass: sync all probe details/status flags from the cache to ensure 100% consistency
        {
            let cache = self
                .probe_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for lang_health in &mut languages {
                // Skip unavailable languages — they have no LSP process, so
                // stale cache entries must not stamp probe_verified=true.
                if lang_health.status == "unavailable" {
                    continue;
                }
                if let Some(entry) = cache.get(&lang_health.language) {
                    lang_health.last_probe_age_secs =
                        Some(u32::try_from(entry.age_secs()).unwrap_or(u32::MAX));
                    lang_health.probe_verified = entry.success;
                    lang_health.navigation_tested = Some(entry.success);
                    lang_health.call_hierarchy_verified = entry.call_hierarchy_verified;
                    if entry.success {
                        if lang_health.status == "warming_up" || lang_health.status == "starting" {
                            lang_health.status = "ready".to_string();
                        }
                    } else if lang_health.status == "ready" {
                        lang_health.status = "degraded".to_string();
                    }
                } else {
                    lang_health.last_probe_age_secs = None;
                    lang_health.probe_verified = false;
                }
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
                navigation_tested: None,
                call_hierarchy_verified: false,
                install_hint: Some(missing.install_hint.clone()),
                server_name: None,
                registrations_received: None,
                indexing_progress_percent: None,
                degraded_tools: vec![
                    crate::server::types::DegradedToolInfo {
                        tool: "trace".to_owned(),
                        severity: "unavailable".to_owned(),
                        description: "No LSP available. Use search for manual reference search."
                            .to_owned(),
                    },
                    crate::server::types::DegradedToolInfo {
                        tool: "inspect".to_owned(),
                        severity: "unavailable".to_owned(),
                        description:
                            "No LSP available. Returns source only, no dependency signatures."
                                .to_owned(),
                    },
                ],
                indexing_source: None,
                indexing_duration_secs: None,
                last_probe_age_secs: None,
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
                let is_ts_js = lang_health.server_name.as_ref().is_some_and(|n| {
                    let lower = n.to_lowercase();
                    lower.contains("typescript")
                        || lower.contains("tsserver")
                        || lower.contains("vtsls")
                        || lower.contains("typescript-language-server")
                });

                if is_ts_js {
                    known_limitations.push(format!(
                        "{}: TypeScript/JavaScript language servers do not support call hierarchy. trace uses grep fallback (less accurate)",
                        lang_health.language
                    ));
                } else {
                    known_limitations.push(format!(
                        "{}: call hierarchy not supported — trace uses grep fallback (less accurate)",
                        lang_health.language
                    ));
                }
            }
        }

        if !self.lawyer.is_warm_start_complete() {
            known_limitations.push(
                "LSP warm_start still in progress — results may be incomplete until indexing finishes"
                    .to_owned(),
            );
        }

        // Flag languages in dynamic registration grace period
        for lang_health in &languages {
            if lang_health.navigation_ready.is_none() && lang_health.status != "unavailable" {
                known_limitations.push(format!(
                    "{}: dynamic capability registration may still be in progress — retry health in a few seconds",
                    lang_health.language
                ));
            }
        }

        // P2-6: Derive top-level indexing_complete from per-language status.
        // True when all languages have indexing_status != "in_progress".
        // Languages with no indexing info (None) are treated as complete
        // (they may be unavailable, so there's nothing to index).
        let indexing_complete = languages
            .iter()
            .all(|l| l.indexing_status.as_deref() != Some("in_progress"));

        let response = crate::server::types::LspHealthResponse {
            status: overall_status.to_owned(),
            languages,
            warm_start_complete: self.lawyer.is_warm_start_complete(),
            indexing_complete,
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
            if let Some(ref name) = l.server_name {
                detail_parts.push(format!("server: {name}"));
            }
            if let Some(regs) = l.registrations_received {
                if regs > 0 {
                    detail_parts.push(format!("registrations: {regs}"));
                }
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
                        "  → Reason: {}. Use search as fallback.",
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
    async fn probe_language_readiness(&self, language_id: &str) -> (bool, bool) {
        let probe_file = self.find_probe_file(language_id);
        let Some(file_path) = probe_file else {
            return (false, false);
        };

        let content = tokio::fs::read_to_string(self.workspace_root.path().join(&file_path))
            .await
            .unwrap_or_default();

        let _ = self
            .lawyer
            .open_document(self.workspace_root.path(), &file_path, &content)
            .await;

        // Wrap in a 2s budget — for a health probe we only need "does it respond",
        // not real data. This caps worst-case probe time instead of inheriting
        // the production goto_definition timeout (10s).
        let probe_timeout = std::time::Duration::from_secs(2);

        let result = tokio::time::timeout(
            probe_timeout,
            self.lawyer
                .goto_definition(self.workspace_root.path(), &file_path, 1, 1),
        )
        .await;

        let Ok(result) = result else {
            tracing::warn!(
                language = %language_id,
                timeout_secs = 2,
                "probe: goto_definition timed out — LSP not responsive"
            );
            return (false, false);
        };

        if result.is_err() {
            return (false, false);
        }

        let mut call_hierarchy_verified = false;
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
                        timeout_secs = 2,
                        "probe: call_hierarchy_prepare timed out — LSP partially responsive"
                    );
                    return (false, false);
                };

                if call_hierarchy_result.is_err() {
                    tracing::warn!(
                        language = %language_id,
                        "probe: goto_definition succeeded but call_hierarchy_prepare failed — LSP may be partially responsive"
                    );
                    return (false, false);
                }
                call_hierarchy_verified = true;
            }
        }

        (true, call_hierarchy_verified)
    }

    /// Get dynamic probe interval for a language based on ramp-up schedule.
    pub(crate) fn get_probe_interval(&self, language: &str) -> u64 {
        let started_at_map = self
            .lsp_started_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(started_at) = started_at_map.get(language) {
            let elapsed = started_at.elapsed().as_secs();
            if elapsed <= 60 {
                10
            } else if elapsed <= 300 {
                30
            } else {
                LIVENESS_PROBE_INTERVAL_SECS
            }
        } else {
            LIVENESS_PROBE_INTERVAL_SECS
        }
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
            // Java: cover Spring Boot, Micronaut, Quarkus, plain-Java conventions.
            // Most real projects use Application.java, App.java, or domain-specific names.
            "java" => vec![
                "src/main/java/Main.java",
                "src/main/java/App.java",
                "src/main/java/Application.java",
            ],
            _ => vec![],
        };

        for candidate in candidates {
            let path = self.workspace_root.path().join(candidate);
            if path.exists() {
                return Some(std::path::PathBuf::from(candidate));
            }
        }

        // LSP-HEALTH-001 Task 3.1: Fallback to depth-limited recursive scan for monorepos.
        // Java gets depth 8 because standard Maven/Gradle layout uses deep package paths:
        //   src/main/java/com/company/project/service/FooService.java = depth 7+
        // Other languages keep depth 4 (most are 2-3 levels deep).
        let max_depth = match language_id {
            "java" => 8,
            _ => 4,
        };
        self.find_file_by_extension_recursive(self.workspace_root.path(), extensions, 0, max_depth)
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

/// Returns structured information about tools that lose LSP support for this language.
///
/// Each entry includes the tool name, severity level, and description of the fallback behavior.
pub(super) fn compute_degraded_tools(
    status: &pathfinder_lsp::types::LspLanguageStatus,
) -> Vec<crate::server::types::DegradedToolInfo> {
    let mut degraded = Vec::new();

    // When the LSP is still warming up (navigation_ready is not yet confirmed true),
    // all LSP-backed tools should be flagged as degraded — even if capability flags
    // are not yet known (None). This closes the gap where warming_up status incorrectly
    // showed an empty degraded_tools list, misleading agents into thinking tools worked.
    let warming_up = status.navigation_ready != Some(true);

    if warming_up {
        // LSP is starting or indexing — all navigation tools operate in degraded mode.
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "locate".to_owned(),
            severity: "warming_up".to_owned(),
            description: "LSP still initializing. Uses grep heuristic until navigation_ready=true."
                .to_owned(),
        });
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "trace".to_owned(),
            severity: "warming_up".to_owned(),
            description: "LSP still initializing. Uses grep fallback until navigation_ready=true."
                .to_owned(),
        });
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "inspect".to_owned(),
            severity: "warming_up".to_owned(),
            description:
                "LSP still initializing. Returns source only (no dep signatures) until ready."
                    .to_owned(),
        });
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "trace(scope=\"references\")".to_owned(),
            severity: "warming_up".to_owned(),
            description: "LSP still initializing. Reference results may be incomplete until ready."
                .to_owned(),
        });
        return degraded;
    }

    // LSP is ready — only flag tools where specific capabilities are explicitly absent.
    if status.supports_definition != Some(true) {
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "locate".to_owned(),
            severity: "grep_fallback".to_owned(),
            description:
                "Uses ripgrep heuristic instead of LSP. May find wrong definition or miss re-exports."
                    .to_owned(),
        });
    }

    if status.supports_call_hierarchy != Some(true) {
        let is_ts_js = status.server_name.as_ref().is_some_and(|n| {
            let lower = n.to_lowercase();
            lower.contains("typescript")
                || lower.contains("tsserver")
                || lower.contains("vtsls")
                || lower.contains("typescript-language-server")
        });

        let trace_desc = if is_ts_js {
            "TypeScript/JavaScript language servers do not support call hierarchy. trace uses grep fallback (less accurate)."
                .to_owned()
        } else {
            "Uses text search instead of call hierarchy. May over/under-count references."
                .to_owned()
        };

        let inspect_desc = if is_ts_js {
            "TypeScript/JavaScript language servers do not support call hierarchy. inspect returns source only, no dependency signatures."
                .to_owned()
        } else {
            "Returns source only, no dependency signatures. Use search as alternative.".to_owned()
        };

        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "trace(scope=\"callers\")".to_owned(),
            severity: "grep_fallback".to_owned(),
            description: trace_desc,
        });
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "inspect(include_dependencies=true)".to_owned(),
            severity: "unavailable".to_owned(),
            description: inspect_desc,
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
#[path = "health_test.rs"]
mod tests;
