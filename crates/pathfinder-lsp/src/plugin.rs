//! LT-2: Language Plugin Trait — per-language behavior abstraction.
//!
//! Each supported language implements [`LanguagePlugin`], encapsulating:
//! - Language identification (ID, file extensions)
//! - LSP binary discovery (binary candidates, default args)
//! - Workspace detection (marker files, search depth)
//! - Manifest validation rules
//! - Install guidance for missing binaries
//! - LSP initialization options
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
    /// Example: Rust has one (`rust-analyzer`), Python has five
    /// (`pyright-langserver`, `pyright`, `pylsp`, `ruff`, `jedi-language-server`).
    fn lsp_candidates(&self) -> &[LspCandidate];

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
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "rust-analyzer",
            default_args: &[],
        }]
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
        &["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "mts", "cts"]
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

    fn install_hint(&self) -> &'static str {
        "Install typescript-language-server: npm install -g typescript-language-server typescript"
    }
}

/// Python language plugin — `pyright-langserver` / `pyright` / `pylsp` / `ruff` / `jedi-language-server`.
pub struct PythonPlugin;

/// Java language plugin — jdtls (Eclipse JDT Language Server).
///
/// Requires JDK 21+ to run jdtls. Supports Java 8–25 project analysis.
/// Marker files searched up to depth 2 to support monorepo layouts
/// (e.g. `services/backend/pom.xml`).
pub struct JavaPlugin;

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
                binary: "basedpyright-langserver",
                default_args: &["--stdio"],
            },
            LspCandidate {
                binary: "pylsp",
                default_args: &[],
            },
            LspCandidate {
                binary: "ruff",
                default_args: &["server", "--stdio"],
            },
            LspCandidate {
                binary: "jedi-language-server",
                default_args: &[],
            },
        ]
    }

    fn install_hint(&self) -> &'static str {
        "Install pyright-langserver: npm install -g pyright\nOr install pylsp: pip install python-lsp-server"
    }
}

impl LanguagePlugin for JavaPlugin {
    fn language_id(&self) -> &'static str {
        "java"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["java"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &[
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
        ]
    }

    fn marker_search_depth(&self) -> u32 {
        2 // Monorepo support
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "jdtls",
            default_args: &[],
        }]
    }

    fn install_hint(&self) -> &'static str {
        "Install jdtls: https://github.com/eclipse-jdtls/eclipse.jdt.ls#installation\n\
         Requires JDK 21+ to run. Use sdkman: sdk install java 21-tem && sdk install jdtls"
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
    &[
        &RustPlugin,
        &GoPlugin,
        &TypeScriptPlugin,
        &PythonPlugin,
        &JavaPlugin,
    ]
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
#[path = "plugin_test.rs"]
mod tests;
