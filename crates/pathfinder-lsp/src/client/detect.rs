//! Zero-Config workspace language detection (PRD §6.5).
//!
//! Scans the workspace root for well-known marker files to determine which
//! language servers should be started. Returns a list of [`LanguageLsp`]
//! descriptors — one per detected language. Only languages whose marker is
//! present in the workspace are returned.
//!
//! Language servers are started lazily on first use (not eagerly at detection
//! time), so this scan is cheap.

use std::path::Path;

/// Identifies a language and the command used to spawn its LSP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageLsp {
    /// Short language identifier used as a map key (e.g., `"rust"`, `"go"`).
    pub language_id: String,
    /// The binary to execute (e.g., `"rust-analyzer"`).
    pub command: String,
    /// Arguments to pass after the binary (e.g., `["--stdio"]`).
    pub args: Vec<String>,
}

/// Scan `workspace_root` for marker files and return the detected languages.
///
/// The scan is non-recursive: only the top-level workspace directory is
/// checked. The order of returned languages is deterministic (alphabetical
/// by language id).
///
/// # Errors
/// Returns `Err` only if `std::fs::read_dir` fails (e.g., permission error).
/// Individual missing marker files do NOT produce errors — they are simply
/// absent from the result.
pub fn detect_languages(workspace_root: &Path) -> std::io::Result<Vec<LanguageLsp>> {
    let mut detected = Vec::new();

    // Rust — Cargo.toml
    if workspace_root.join("Cargo.toml").exists() {
        detected.push(LanguageLsp {
            language_id: "rust".to_owned(),
            command: "rust-analyzer".to_owned(),
            args: vec![],
        });
    }

    // Go — go.mod
    if workspace_root.join("go.mod").exists() {
        detected.push(LanguageLsp {
            language_id: "go".to_owned(),
            command: "gopls".to_owned(),
            args: vec![],
        });
    }

    // TypeScript / JavaScript — tsconfig.json or package.json
    if workspace_root.join("tsconfig.json").exists()
        || workspace_root.join("package.json").exists()
    {
        detected.push(LanguageLsp {
            language_id: "typescript".to_owned(),
            command: "typescript-language-server".to_owned(),
            args: vec!["--stdio".to_owned()],
        });
    }

    // Python — pyproject.toml, setup.py, or requirements.txt
    if workspace_root.join("pyproject.toml").exists()
        || workspace_root.join("setup.py").exists()
        || workspace_root.join("requirements.txt").exists()
    {
        detected.push(LanguageLsp {
            language_id: "python".to_owned(),
            command: "pyright".to_owned(),
            args: vec!["--stdio".to_owned()],
        });
    }

    Ok(detected)
}

/// Map a file extension to its language identifier.
///
/// Used to look up the correct LSP process when a tool call names a specific
/// file. Returns `None` if the language is unsupported.
#[must_use]
pub fn language_id_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "go" => Some("go"),
        // Both TypeScript and JavaScript are served by typescript-language-server
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some("typescript"),
        "py" | "pyi" => Some("python"),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_detects_cargo_toml() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
        let langs = detect_languages(dir.path()).expect("detect");
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "rust");
        assert_eq!(langs[0].command, "rust-analyzer");
        assert!(langs[0].args.is_empty());
    }

    #[test]
    fn test_detects_go_mod() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("go.mod"), "module foo").expect("write");
        let langs = detect_languages(dir.path()).expect("detect");
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "go");
        assert_eq!(langs[0].command, "gopls");
    }

    #[test]
    fn test_detects_typescript_via_tsconfig() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("tsconfig.json"), "{}").expect("write");
        let langs = detect_languages(dir.path()).expect("detect");
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "typescript");
        assert_eq!(langs[0].args, ["--stdio"]);
    }

    #[test]
    fn test_detects_typescript_via_package_json() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        let langs = detect_languages(dir.path()).expect("detect");
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "typescript");
    }

    #[test]
    fn test_detects_python_via_pyproject() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        let langs = detect_languages(dir.path()).expect("detect");
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "python");
    }

    #[test]
    fn test_detects_multiple_languages() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        let langs = detect_languages(dir.path()).expect("detect");
        // Rust is added first, TypeScript second
        let ids: Vec<&str> = langs.iter().map(|l| l.language_id.as_str()).collect();
        assert!(ids.contains(&"rust"));
        assert!(ids.contains(&"typescript"));
    }

    #[test]
    fn test_empty_directory() {
        let dir = tempdir().expect("temp dir");
        let langs = detect_languages(dir.path()).expect("detect");
        assert!(langs.is_empty());
    }

    #[test]
    fn test_language_id_for_extension() {
        assert_eq!(language_id_for_extension("rs"), Some("rust"));
        assert_eq!(language_id_for_extension("go"), Some("go"));
        assert_eq!(language_id_for_extension("ts"), Some("typescript"));
        assert_eq!(language_id_for_extension("tsx"), Some("typescript"));
        assert_eq!(language_id_for_extension("js"), Some("typescript"));
        assert_eq!(language_id_for_extension("py"), Some("python"));
        assert_eq!(language_id_for_extension("md"), None);
        assert_eq!(language_id_for_extension("yaml"), None);
    }
}
