//! Automatic detection of languages and their Language Server Protocol (LSP) configurations.
//!
//! Iterates over all registered [`LanguagePlugin`] implementations to discover
//! language servers, marker files, and required configurations.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use pathfinder_common::config::PathfinderConfig;

use crate::plugin::{all_plugins, plugin_for_extension, plugin_for_language, LanguagePlugin};

#[allow(unused_imports)]
pub use crate::plugin::{
    detect_java_init_options, detect_jdk_home, detect_python_init_options, detect_venv,
    strip_json_trailing_commas, strip_jsonc_comments,
};

/// Result of language detection.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DetectionResult {
    /// Languages with markers AND binaries found.
    pub detected: Vec<LanguageLsp>,
    /// Languages with markers but no binary on PATH.
    pub missing: Vec<MissingLanguage>,
}

/// A language whose marker files were found but whose LSP binary is not on PATH.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MissingLanguage {
    /// Language ID (e.g. "rust", "python").
    pub language_id: String,
    /// Marker file that was found (e.g. "Cargo.toml").
    pub marker_file: String,
    /// All binary candidates tried and failed.
    pub tried_binaries: Vec<String>,
    /// Installation hint for this language.
    pub install_hint: String,
}

/// Identifies a language and the command used to spawn its LSP server.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LanguageLsp {
    /// Language ID (e.g. "rust", "go").
    pub language_id: String,
    /// LSP command binary path.
    pub command: String,
    /// CLI arguments for the LSP server.
    pub args: Vec<String>,
    /// Root directory for the language server.
    pub root: PathBuf,
    /// Optional initialization timeout in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_timeout_secs: Option<u64>,
    /// Auto-detected TypeScript plugins to load.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub auto_plugins: Vec<String>,
    /// Language-specific initialization options JSON.
    pub init_options: serde_json::Value,
}

/// Return an actionable install hint for each language.
#[must_use]
pub fn install_hint(language_id: &str) -> String {
    plugin_for_language(language_id).map_or_else(
        || format!("Install a language server for {language_id}"),
        |p| p.install_hint().to_string(),
    )
}

/// Map a file extension to its canonical language ID.
#[must_use]
pub fn language_id_for_extension(ext: &str) -> Option<&'static str> {
    plugin_for_extension(ext).map(LanguagePlugin::language_id)
}

/// Validate marker file contents using the language plugin (LSP-D7, LSP-B6).
pub(crate) fn validate_marker_file(marker_path: &Path, language_id: &str) -> Result<(), String> {
    if !marker_path.exists() {
        return Ok(());
    }
    let name = marker_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let contents =
        std::fs::read_to_string(marker_path).map_err(|e| format!("cannot read {name}: {e}"))?;
    plugin_for_language(language_id).map_or(Ok(()), |p| p.validate_marker(name, &contents))
}

// ── Marker scanning & Resolution (LSP-D7) ─────────────────────────────────────

fn has_marker(dir: &Path, marker: &str) -> bool {
    dir.join(marker).exists()
}

async fn is_symlink_entry(entry: &tokio::fs::DirEntry) -> bool {
    entry.file_type().await.is_ok_and(|ft| ft.is_symlink())
}

fn is_ignored_dir(name: &str) -> bool {
    const IGNORED: &[&str] = &[
        "node_modules",
        ".git",
        "target",
        ".pnpm",
        ".venv",
        "__pycache__",
        "dist",
        "build",
        "vendor",
    ];
    IGNORED.contains(&name)
}

async fn scan_children_for_marker(dir: &Path, marker: &str, depth: usize) -> Vec<PathBuf> {
    if depth == 0 {
        return Vec::new();
    }
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    let mut subdirs = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !is_symlink_entry(&entry).await && entry.metadata().await.is_ok_and(|m| m.is_dir()) {
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_ignored_dir)
            {
                if has_marker(&path, marker) {
                    matches.push(path.clone());
                }
                subdirs.push(path);
            }
        }
    }
    for sub in subdirs {
        matches.append(&mut Box::pin(scan_children_for_marker(&sub, marker, depth - 1)).await);
    }
    matches
}

fn refine_by_sibling(matches: &[PathBuf], sibling: &str) -> Vec<PathBuf> {
    matches
        .iter()
        .filter(|d| has_marker(d, sibling))
        .cloned()
        .collect()
}

fn resolve_multiple_matches(
    matches: &[PathBuf],
    marker: &str,
    sibling_filter: Option<&str>,
    base: &Path,
) -> PathBuf {
    if matches.len() <= 1 {
        return matches
            .first()
            .cloned()
            .unwrap_or_else(|| base.to_path_buf());
    }
    if let Some(sibling) = sibling_filter {
        let refined = refine_by_sibling(matches, sibling);
        if refined.len() == 1 {
            return refined[0].clone();
        }
    }
    tracing::warn!(
        marker,
        match_count = matches.len(),
        fallback = %base.display(),
        "Monorepo marker fallback to workspace root"
    );
    base.to_path_buf()
}

async fn find_marker(
    base: &Path,
    marker: &str,
    max_depth: usize,
    sibling: Option<&str>,
) -> Option<PathBuf> {
    if has_marker(base, marker) {
        return Some(base.to_path_buf());
    }
    let depth = max_depth.min(2);
    if depth == 0 {
        return None;
    }
    let matches = scan_children_for_marker(base, marker, depth).await;
    (!matches.is_empty()).then(|| resolve_multiple_matches(&matches, marker, sibling, base))
}

fn resolve_command(name: &str, lang: &str) -> Option<String> {
    which::which(name)
        .map(|p| p.to_string_lossy().into_owned())
        .ok()
        .or_else(|| {
            tracing::debug!("LSP binary not found for {lang}: {name}");
            None
        })
}

fn has_source_files_recursive<'a>(
    dir: &'a Path,
    extensions: &'a [&'a str],
    depth: usize,
) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
    Box::pin(async move {
        if depth > 4 {
            return false;
        }
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return false;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if is_symlink_entry(&entry).await {
                continue;
            }
            let path = entry.path();
            if let Ok(meta) = entry.metadata().await {
                if meta.is_file() {
                    if path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|ext| extensions.contains(&ext))
                    {
                        return true;
                    }
                } else if meta.is_dir()
                    && !path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(is_ignored_dir)
                    && has_source_files_recursive(&path, extensions, depth + 1).await
                {
                    return true;
                }
            }
        }
        false
    })
}

// ── Generic Plugin Language Detection (LSP-D7) ────────────────────────────────

fn resolve_plugin_binary(
    plugin: &dyn LanguagePlugin,
    override_cmd: Option<String>,
    lsp_cfg: Option<&pathfinder_common::config::LspConfig>,
    found_marker: Option<&str>,
) -> Result<(String, Vec<String>), Option<MissingLanguage>> {
    let lang = plugin.language_id();
    let has_override = override_cmd.is_some();
    if let Some(cmd) = override_cmd {
        let cfg_args = lsp_cfg.map(|c| c.args.clone()).unwrap_or_default();
        let final_args = if cfg_args.is_empty() {
            plugin
                .lsp_candidates()
                .first()
                .map(|c| c.default_args.iter().map(|s| (*s).to_owned()).collect())
                .unwrap_or_default()
        } else {
            cfg_args
        };
        return Ok((cmd, final_args));
    }

    let mut tried = Vec::new();
    for candidate in plugin.lsp_candidates() {
        tried.push(candidate.binary.to_owned());
        if let Some(path) = resolve_command(candidate.binary, lang) {
            return Ok((
                path,
                candidate
                    .default_args
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            ));
        }
    }

    let missing = if has_override {
        None
    } else {
        Some(MissingLanguage {
            language_id: lang.to_owned(),
            marker_file: found_marker.unwrap_or_default().to_owned(),
            tried_binaries: tried,
            install_hint: plugin.install_hint().to_owned(),
        })
    };
    Err(missing)
}

async fn detect_plugin_language(
    plugin: &dyn LanguagePlugin,
    workspace_root: &Path,
    config: &PathfinderConfig,
) -> (Option<LanguageLsp>, Option<MissingLanguage>) {
    let lang = plugin.language_id();
    let lsp_cfg = config.lsp.get(lang);
    let override_cmd = lsp_cfg.map(|c| c.command.clone()).filter(|c| !c.is_empty());
    let has_override = override_cmd.is_some();

    let mut detected_root = None;
    let mut found_marker = None;

    for &marker in plugin.marker_files() {
        let sibling = plugin.marker_sibling_requirement(marker);
        if let Some(root) = find_marker(
            workspace_root,
            marker,
            plugin.marker_search_depth() as usize,
            sibling,
        )
        .await
        {
            let marker_path = root.join(marker);
            if let Err(reason) = validate_marker_file(&marker_path, lang) {
                return (
                    None,
                    Some(MissingLanguage {
                        language_id: lang.to_owned(),
                        marker_file: marker.to_owned(),
                        tried_binaries: Vec::new(),
                        install_hint: format!("Fix {marker}: {reason}"),
                    }),
                );
            }
            detected_root = Some(root);
            found_marker = Some(marker.to_owned());
            break;
        }
    }

    if detected_root.is_none() && !has_override {
        return (None, None);
    }

    let root = if let Some(override_root) = lsp_cfg.and_then(|c| c.root_override.as_ref()) {
        workspace_root.join(override_root)
    } else {
        detected_root.unwrap_or_else(|| workspace_root.to_path_buf())
    };

    let (binary_path, args) =
        match resolve_plugin_binary(plugin, override_cmd, lsp_cfg, found_marker.as_deref()) {
            Ok(res) => res,
            Err(missing) => return (None, missing),
        };

    let init_options = plugin.initialization_options(&root, workspace_root);
    let auto_plugins = lsp_cfg
        .map(|c| c.typescript_plugins.clone())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            plugin
                .discover_server_plugins(&root)
                .into_iter()
                .map(|p| p.name)
                .collect()
        });

    let init_timeout_secs = if plugin.handshake_timeout() == std::time::Duration::from_secs(60) {
        None
    } else {
        Some(plugin.handshake_timeout().as_secs())
    };

    (
        Some(LanguageLsp {
            language_id: lang.to_owned(),
            command: binary_path,
            args,
            root,
            init_options,
            init_timeout_secs,
            auto_plugins,
        }),
        None,
    )
}

async fn filter_by_sources<T, F>(items: Vec<T>, workspace_root: &Path, get_lang: F) -> Vec<T>
where
    F: Fn(&T) -> &str,
{
    let mut filtered = Vec::with_capacity(items.len());
    for item in items {
        let lang = get_lang(&item);
        let has = if let Some(p) = plugin_for_language(lang) {
            has_source_files_recursive(workspace_root, p.file_extensions(), 0).await
        } else {
            true
        };
        if has {
            filtered.push(item);
        }
    }
    filtered
}

/// Detect all languages and available LSP servers in the workspace.
///
/// # Errors
///
/// Returns an error if filesystem directory traversal fails.
pub async fn detect_languages(
    workspace_root: &Path,
    config: &PathfinderConfig,
) -> std::io::Result<DetectionResult> {
    let mut detected = Vec::new();
    let mut missing = Vec::new();

    for plugin in all_plugins() {
        let (lsp, miss) = detect_plugin_language(plugin, workspace_root, config).await;
        if let Some(l) = lsp {
            detected.push(l);
        }
        if let Some(m) = miss {
            missing.push(m);
        }
    }

    let filtered_detected = filter_by_sources(detected, workspace_root, |l| &l.language_id).await;
    let filtered_missing = filter_by_sources(missing, workspace_root, |m| &m.language_id).await;

    Ok(DetectionResult {
        detected: filtered_detected,
        missing: filtered_missing,
    })
}

#[cfg(test)]
#[allow(clippy::unused_async)]
pub(crate) async fn workspace_has_vue_files(dir: &Path) -> bool {
    crate::plugin::check_has_vue_files(dir, 0)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "detect_test.rs"]
mod tests;
