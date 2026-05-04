//! LT-2: Language Plugin Trait — per-language behavior abstraction.
//!
//! Each supported language implements [`LanguagePlugin`], encapsulating:
//! - Language identification (ID, file extensions)
//! - LSP binary discovery (binary candidates, default args)
//! - Workspace detection (marker files, search depth)
//! - Manifest validation rules
//! - Diagnostic collection configuration (push timeouts from MT-2)
//! - LSP initialization options
//! - Install guidance for missing binaries
//!
//! # Design Rationale
//!
//! Before LT-2, per-language logic was scattered across `detect.rs`,
//! `capabilities.rs`, and `process.rs` as match arms on string language IDs.
//! This trait centralises that knowledge, making it straightforward to add
//! new languages and test each language's configuration in isolation.
//!
//! The trait is **object-safe** so implementations can be used as
//! `Box<dyn LanguagePlugin>` or `&dyn LanguagePlugin`.

use crate::client::PushDiagnosticsConfig;

/// Describes a candidate LSP binary with its default arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCandidate {
    /// Binary name to resolve via `which` (e.g., `"rust-analyzer"`, `"gopls"`).
    pub binary: &'static str,
    /// Default CLI arguments (e.g., `["--stdio"]`).
    pub default_args: &'static [&'static str],
}

/// Per-language LSP behaviour abstraction.
///
/// Implementations are pure data providers — no I/O, no async.
/// This makes them trivially testable and composable.
pub trait LanguagePlugin: Send + Sync {
    /// Short identifier used as a map key (e.g., `"rust"`, `"go"`, `"typescript"`, `"python"`).
    fn language_id(&self) -> &'static str;

    /// File extensions that this language handles.
    ///
    /// Used by [`language_id_for_extension`] and [`touch_language`] in LT-4.
    /// Example: `&["rs"]` for Rust, `&["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue"]` for TypeScript.
    fn file_extensions(&self) -> &'static [&'static str];

    /// Marker files that indicate this language is used in the workspace.
    ///
    /// Returned in priority order — detection stops at the first match.
    /// Example: `&["Cargo.toml"]` for Rust, `&["tsconfig.json", "package.json"]` for TypeScript.
    fn marker_files(&self) -> &'static [&'static str];

    /// Maximum directory depth to search for marker files.
    ///
    /// `0` = root only, `2` = root + up to 2 levels deep (for monorepos).
    fn marker_search_depth(&self) -> u32;

    /// LSP binary candidates in preference order.
    ///
    /// Detection tries each candidate via `which` and uses the first found.
    /// Example: Rust has one (`rust-analyzer`), Python has four
    /// (`pyright-langserver`, `pylsp`, `ruff-lsp`, `jedi-language-server`).
    fn lsp_candidates(&self) -> &[LspCandidate];

    /// Push diagnostics collection config for this language's server.
    ///
    /// Encapsulates the MT-2 per-server timing (`ceiling_ms`, `grace_ms`).
    /// `server_name` is the `serverInfo.name` from the LSP `initialize` response.
    fn push_diagnostics_config(&self, server_name: Option<&str>) -> PushDiagnosticsConfig;

    /// Human-readable install guidance when no LSP binary is found.
    fn install_hint(&self) -> &'static str;
}

// ── Concrete Implementations ──────────────────────────────────────────────────

/// Rust language plugin — `rust-analyzer` + `Cargo.toml`.
pub struct RustPlugin;

impl LanguagePlugin for RustPlugin {
    fn language_id(&self) -> &'static str {
        "rust"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &["Cargo.toml"]
    }

    fn marker_search_depth(&self) -> u32 {
        0
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "rust-analyzer",
            default_args: &[],
        }]
    }

    fn push_diagnostics_config(&self, _server_name: Option<&str>) -> PushDiagnosticsConfig {
        // rust-analyzer is fast at emitting diagnostics
        PushDiagnosticsConfig {
            ceiling_ms: 10_000,
            grace_ms: 300,
        }
    }

    fn install_hint(&self) -> &'static str {
        "Install rust-analyzer: https://rust-analyzer.github.io/"
    }
}

/// Go language plugin — `gopls` + `go.mod`.
pub struct GoPlugin;

impl LanguagePlugin for GoPlugin {
    fn language_id(&self) -> &'static str {
        "go"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &["go.mod"]
    }

    fn marker_search_depth(&self) -> u32 {
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "gopls",
            default_args: &[],
        }]
    }

    fn push_diagnostics_config(&self, _server_name: Option<&str>) -> PushDiagnosticsConfig {
        // gopls needs more time: analyses entire module graph
        PushDiagnosticsConfig {
            ceiling_ms: 20_000,
            grace_ms: 3_000,
        }
    }

    fn install_hint(&self) -> &'static str {
        "Install gopls: go install golang.org/x/tools/gopls@latest"
    }
}

/// TypeScript / JavaScript language plugin — `typescript-language-server` + `tsconfig.json` / `package.json`.
pub struct TypeScriptPlugin;

impl LanguagePlugin for TypeScriptPlugin {
    fn language_id(&self) -> &'static str {
        "typescript"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &["tsconfig.json", "package.json"]
    }

    fn marker_search_depth(&self) -> u32 {
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "typescript-language-server",
            default_args: &["--stdio"],
        }]
    }

    fn push_diagnostics_config(&self, _server_name: Option<&str>) -> PushDiagnosticsConfig {
        // tsserver can be slow on large projects
        PushDiagnosticsConfig {
            ceiling_ms: 30_000,
            grace_ms: 3_000,
        }
    }

    fn install_hint(&self) -> &'static str {
        "Install typescript-language-server: npm install -g typescript-language-server typescript"
    }
}

/// Python language plugin — `pyright-langserver` / `pylsp` / `ruff-lsp` / `jedi-language-server`.
pub struct PythonPlugin;

impl LanguagePlugin for PythonPlugin {
    fn language_id(&self) -> &'static str {
        "python"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &["pyproject.toml", "setup.py", "requirements.txt"]
    }

    fn marker_search_depth(&self) -> u32 {
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[
            LspCandidate {
                binary: "pyright-langserver",
                default_args: &["--stdio"],
            },
            LspCandidate {
                binary: "pylsp",
                default_args: &[],
            },
            LspCandidate {
                binary: "ruff-lsp",
                default_args: &[],
            },
            LspCandidate {
                binary: "jedi-language-server",
                default_args: &[],
            },
        ]
    }

    fn push_diagnostics_config(&self, server_name: Option<&str>) -> PushDiagnosticsConfig {
        // Pyright and pylsp both have moderate timing
        // Pyright, pylsp, and other Python LSPs all use the same timing.
        let _ = server_name;
        PushDiagnosticsConfig {
            ceiling_ms: 15_000,
            grace_ms: 1_000,
        }
    }

    fn install_hint(&self) -> &'static str {
        "Install pyright: npm install -g pyright\nOr install pylsp: pip install python-lsp-server"
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// All built-in language plugins.
///
/// Returns a static slice of all supported language plugins.
/// Used by `detect_languages` to iterate over known languages and by
/// `language_id_for_extension` to look up language IDs from file extensions.
#[must_use]
pub fn all_plugins() -> &'static [&'static dyn LanguagePlugin] {
    &[&RustPlugin, &GoPlugin, &TypeScriptPlugin, &PythonPlugin]
}

/// Look up a plugin by its language ID.
#[must_use]
pub fn plugin_for_language(language_id: &str) -> Option<&'static dyn LanguagePlugin> {
    all_plugins()
        .iter()
        .find(|p| p.language_id() == language_id)
        .copied()
}

/// Look up a plugin by file extension.
///
/// Returns the first plugin whose `file_extensions()` contains the given extension.
#[must_use]
pub fn plugin_for_extension(ext: &str) -> Option<&'static dyn LanguagePlugin> {
    all_plugins()
        .iter()
        .find(|p| p.file_extensions().contains(&ext))
        .copied()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ── Trait object safety ─────────────────────────────────────────────

    #[test]
    fn test_trait_is_object_safe() {
        // If this compiles, the trait is object-safe.
        let _: Box<dyn LanguagePlugin> = Box::new(RustPlugin);
        let _: &dyn LanguagePlugin = &GoPlugin;
    }

    // ── RustPlugin ──────────────────────────────────────────────────────

    #[test]
    fn test_rust_plugin_language_id() {
        assert_eq!(RustPlugin.language_id(), "rust");
    }

    #[test]
    fn test_rust_plugin_file_extensions() {
        assert_eq!(RustPlugin.file_extensions(), &["rs"]);
    }

    #[test]
    fn test_rust_plugin_marker_files() {
        assert_eq!(RustPlugin.marker_files(), &["Cargo.toml"]);
    }

    #[test]
    fn test_rust_plugin_marker_search_depth() {
        assert_eq!(RustPlugin.marker_search_depth(), 0);
    }

    #[test]
    fn test_rust_plugin_lsp_candidates() {
        let candidates = RustPlugin.lsp_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].binary, "rust-analyzer");
        assert!(candidates[0].default_args.is_empty());
    }

    #[test]
    fn test_rust_plugin_push_diagnostics_config() {
        let cfg = RustPlugin.push_diagnostics_config(Some("rust-analyzer"));
        assert_eq!(cfg.ceiling_ms, 10_000);
        assert_eq!(cfg.grace_ms, 300);
    }

    #[test]
    fn test_rust_plugin_install_hint() {
        let hint = RustPlugin.install_hint();
        assert!(hint.contains("rust-analyzer"));
    }

    // ── GoPlugin ────────────────────────────────────────────────────────

    #[test]
    fn test_go_plugin_language_id() {
        assert_eq!(GoPlugin.language_id(), "go");
    }

    #[test]
    fn test_go_plugin_file_extensions() {
        assert_eq!(GoPlugin.file_extensions(), &["go"]);
    }

    #[test]
    fn test_go_plugin_marker_files() {
        assert_eq!(GoPlugin.marker_files(), &["go.mod"]);
    }

    #[test]
    fn test_go_plugin_marker_search_depth() {
        assert_eq!(GoPlugin.marker_search_depth(), 2);
    }

    #[test]
    fn test_go_plugin_lsp_candidates() {
        let candidates = GoPlugin.lsp_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].binary, "gopls");
    }

    #[test]
    fn test_go_plugin_push_diagnostics_config() {
        let cfg = GoPlugin.push_diagnostics_config(Some("gopls"));
        assert_eq!(cfg.ceiling_ms, 20_000);
        assert_eq!(cfg.grace_ms, 3_000);
    }

    #[test]
    fn test_go_plugin_install_hint() {
        let hint = GoPlugin.install_hint();
        assert!(hint.contains("gopls"));
    }

    // ── TypeScriptPlugin ────────────────────────────────────────────────

    #[test]
    fn test_typescript_plugin_language_id() {
        assert_eq!(TypeScriptPlugin.language_id(), "typescript");
    }

    #[test]
    fn test_typescript_plugin_file_extensions() {
        let exts = TypeScriptPlugin.file_extensions();
        assert!(exts.contains(&"ts"));
        assert!(exts.contains(&"tsx"));
        assert!(exts.contains(&"js"));
        assert!(exts.contains(&"jsx"));
        assert!(exts.contains(&"mjs"));
        assert!(exts.contains(&"cjs"));
        assert!(exts.contains(&"vue"));
    }

    #[test]
    fn test_typescript_plugin_marker_files() {
        let markers = TypeScriptPlugin.marker_files();
        assert_eq!(markers, &["tsconfig.json", "package.json"]);
    }

    #[test]
    fn test_typescript_plugin_marker_search_depth() {
        assert_eq!(TypeScriptPlugin.marker_search_depth(), 2);
    }

    #[test]
    fn test_typescript_plugin_lsp_candidates() {
        let candidates = TypeScriptPlugin.lsp_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].binary, "typescript-language-server");
        assert_eq!(candidates[0].default_args, &["--stdio"]);
    }

    #[test]
    fn test_typescript_plugin_push_diagnostics_config() {
        let cfg = TypeScriptPlugin.push_diagnostics_config(Some("typescript-language-server"));
        assert_eq!(cfg.ceiling_ms, 30_000);
        assert_eq!(cfg.grace_ms, 3_000);
    }

    #[test]
    fn test_typescript_plugin_install_hint() {
        let hint = TypeScriptPlugin.install_hint();
        assert!(hint.contains("typescript-language-server"));
    }

    // ── PythonPlugin ────────────────────────────────────────────────────

    #[test]
    fn test_python_plugin_language_id() {
        assert_eq!(PythonPlugin.language_id(), "python");
    }

    #[test]
    fn test_python_plugin_file_extensions() {
        let exts = PythonPlugin.file_extensions();
        assert!(exts.contains(&"py"));
        assert!(exts.contains(&"pyi"));
    }

    #[test]
    fn test_python_plugin_marker_files() {
        let markers = PythonPlugin.marker_files();
        assert_eq!(markers, &["pyproject.toml", "setup.py", "requirements.txt"]);
    }

    #[test]
    fn test_python_plugin_marker_search_depth() {
        assert_eq!(PythonPlugin.marker_search_depth(), 2);
    }

    #[test]
    fn test_python_plugin_lsp_candidates() {
        let candidates = PythonPlugin.lsp_candidates();
        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[0].binary, "pyright-langserver");
        assert_eq!(candidates[0].default_args, &["--stdio"]);
        assert_eq!(candidates[1].binary, "pylsp");
        assert_eq!(candidates[2].binary, "ruff-lsp");
        assert_eq!(candidates[3].binary, "jedi-language-server");
    }

    #[test]
    fn test_python_plugin_push_diagnostics_config_pyright() {
        let cfg = PythonPlugin.push_diagnostics_config(Some("pyright"));
        assert_eq!(cfg.ceiling_ms, 15_000);
        assert_eq!(cfg.grace_ms, 1_000);
    }

    #[test]
    fn test_python_plugin_push_diagnostics_config_pylsp() {
        let cfg = PythonPlugin.push_diagnostics_config(Some("pylsp"));
        assert_eq!(cfg.ceiling_ms, 15_000);
        assert_eq!(cfg.grace_ms, 1_000);
    }

    #[test]
    fn test_python_plugin_push_diagnostics_config_unknown_server() {
        let cfg = PythonPlugin.push_diagnostics_config(Some("ruff-lsp"));
        assert_eq!(cfg.ceiling_ms, 15_000);
        assert_eq!(cfg.grace_ms, 1_000);
    }

    #[test]
    fn test_python_plugin_install_hint() {
        let hint = PythonPlugin.install_hint();
        assert!(hint.contains("pyright"));
        assert!(hint.contains("pylsp"));
    }

    // ── Registry ────────────────────────────────────────────────────────

    #[test]
    fn test_all_plugins_contains_all_four_languages() {
        let plugins = all_plugins();
        assert_eq!(plugins.len(), 4);
        let ids: Vec<&str> = plugins.iter().map(|p| p.language_id()).collect();
        assert!(ids.contains(&"rust"));
        assert!(ids.contains(&"go"));
        assert!(ids.contains(&"typescript"));
        assert!(ids.contains(&"python"));
    }

    #[test]
    fn test_plugin_for_language_found() {
        let plugin = plugin_for_language("rust").unwrap();
        assert_eq!(plugin.language_id(), "rust");
    }

    #[test]
    fn test_plugin_for_language_not_found() {
        assert!(plugin_for_language("java").is_none());
    }

    #[test]
    fn test_plugin_for_extension_rs() {
        let plugin = plugin_for_extension("rs").unwrap();
        assert_eq!(plugin.language_id(), "rust");
    }

    #[test]
    fn test_plugin_for_extension_go() {
        let plugin = plugin_for_extension("go").unwrap();
        assert_eq!(plugin.language_id(), "go");
    }

    #[test]
    fn test_plugin_for_extension_ts() {
        let plugin = plugin_for_extension("ts").unwrap();
        assert_eq!(plugin.language_id(), "typescript");
    }

    #[test]
    fn test_plugin_for_extension_vue() {
        let plugin = plugin_for_extension("vue").unwrap();
        assert_eq!(plugin.language_id(), "typescript");
    }

    #[test]
    fn test_plugin_for_extension_py() {
        let plugin = plugin_for_extension("py").unwrap();
        assert_eq!(plugin.language_id(), "python");
    }

    #[test]
    fn test_plugin_for_extension_unknown() {
        assert!(plugin_for_extension("java").is_none());
    }

    // ── Cross-validation with existing code ─────────────────────────────

    #[test]
    fn test_plugins_match_language_id_for_extension() {
        // Verify that the plugin registry returns the same language_id
        // as the existing language_id_for_extension function for all known extensions.
        use crate::client::language_id_for_extension;

        for ext in &[
            "rs", "go", "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "py", "pyi",
        ] {
            let from_fn = language_id_for_extension(ext);
            let from_plugin = plugin_for_extension(ext).map(LanguagePlugin::language_id);
            assert_eq!(
                from_fn,
                from_plugin,
                "Mismatch for extension .{ext}: fn={from_fn:?}, plugin={from_plugin:?}"
            );
        }
    }

    #[test]
    fn test_plugins_match_push_config_for_known_servers() {
        // Verify that the plugin's push_diagnostics_config matches
        // the existing push_collection_config_for for known server names.
        use crate::client::DetectedCapabilities;

        let cases = [
            ("rust", Some("rust-analyzer")),
            ("go", Some("gopls")),
            ("typescript", Some("typescript-language-server")),
            ("python", Some("pyright")),
            ("python", Some("pylsp")),
        ];

        for (lang, server_name) in cases {
            let plugin = plugin_for_language(lang).unwrap();
            let from_plugin = plugin.push_diagnostics_config(server_name);
            let from_fn = DetectedCapabilities::push_collection_config_for(server_name);

            assert_eq!(
                from_plugin.ceiling_ms, from_fn.ceiling_ms,
                "ceiling_ms mismatch for {lang}/{server_name:?}"
            );
            assert_eq!(
                from_plugin.grace_ms, from_fn.grace_ms,
                "grace_ms mismatch for {lang}/{server_name:?}"
            );
        }
    }

    // ── Unique language IDs ─────────────────────────────────────────────

    #[test]
    fn test_all_plugins_have_unique_language_ids() {
        let plugins = all_plugins();
        let ids: Vec<&str> = plugins.iter().map(|p| p.language_id()).collect();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "Duplicate language IDs found: {ids:?}"
        );
    }

    #[test]
    fn test_no_extension_overlap_between_plugins() {
        // Each extension should map to exactly one plugin.
        let plugins = all_plugins();
        let mut seen = std::collections::HashMap::new();
        for plugin in plugins {
            for ext in plugin.file_extensions() {
                if let Some(existing) = seen.insert(*ext, plugin.language_id()) {
                    panic!(
                        "Extension .{ext} claimed by both '{existing}' and '{}'",
                        plugin.language_id()
                    );
                }
            }
        }
    }
}
