//! Three-tier sandbox enforcement for file access control.
//!
//! Implements the access control model from PRD §4.3:
//! - **Tier 1 (Hardcoded Deny):** `.git/objects/`, `.git/HEAD`, `*.pem`, `*.key`, `*.pfx`
//! - **Tier 2 (Default Deny):** `.env`, `node_modules/`, `vendor/`, etc.
//! - **Tier 3 (User-Defined):** `.pathfinderignore` patterns
//!
//! Allowed from `.git/`: `.gitignore`, `.github/workflows/`, `.github/actions/`

use crate::config::SandboxConfig;
use crate::error::{PathfinderError, SandboxTier};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::{Path, PathBuf};

/// The hardcoded deny patterns. These CANNOT be overridden.
const HARDCODED_DENY_PATTERNS: &[&str] = &[
    ".git/objects/",
    ".git/HEAD",
    ".git/refs/",
    ".git/index",
    ".git/config",
    ".git/hooks/",
];

/// File extensions that are always denied (security-critical).
const HARDCODED_DENY_EXTENSIONS: &[&str] = &["pem", "key", "pfx", "p12"];

/// Paths explicitly allowed from `.git/`.
const GIT_ALLOWLIST: &[&str] = &[
    ".gitignore",
    ".github/workflows/",
    ".github/actions/",
    ".gitattributes",
];

/// Default deny patterns. Can be overridden via config.
const DEFAULT_DENY_PATTERNS: &[&str] = &[
    ".env",
    ".env.*",
    "secrets/",
    "node_modules/",
    "vendor/",
    "__pycache__/",
    "dist/",
    "build/",
];

/// The sandbox enforcer. Checks file paths against the three-tier deny model.
pub struct Sandbox {
    _workspace_root: PathBuf,
    /// Compiled default deny patterns with overrides applied.
    effective_default_deny: Vec<String>,
    /// User-defined `.pathfinderignore` rules.
    user_ignore: Option<Gitignore>,
    /// Config-level additional deny patterns.
    additional_deny: Vec<String>,
}

impl Sandbox {
    /// Create a new sandbox enforcer.
    ///
    /// Reads `.pathfinderignore` from the workspace root if it exists.
    /// For unit tests that must not touch the file system, use
    /// [`Sandbox::with_user_rules`] instead.
    #[must_use]
    pub fn new(workspace_root: &Path, config: &SandboxConfig) -> Self {
        // Load .pathfinderignore if it exists
        let ignore_path = workspace_root.join(".pathfinderignore");
        let user_ignore = if ignore_path.exists() {
            let mut builder = GitignoreBuilder::new(workspace_root);
            // Ignore errors on individual lines — best-effort parsing
            let _ = builder.add(&ignore_path);
            builder.build().ok()
        } else {
            None
        };

        Self::with_user_rules(workspace_root, config, user_ignore)
    }

    /// Create a sandbox enforcer with pre-loaded user ignore rules.
    ///
    /// This constructor performs **no disk I/O** and is intended for unit
    /// testing. Pass `None` for `user_ignore` to skip Tier 3 enforcement.
    #[must_use]
    pub fn with_user_rules(
        workspace_root: &Path,
        config: &SandboxConfig,
        user_ignore: Option<Gitignore>,
    ) -> Self {
        // Compute effective default deny by removing any allow_override entries
        let effective_default_deny: Vec<String> = DEFAULT_DENY_PATTERNS
            .iter()
            .filter(|pattern| !config.allow_override.iter().any(|a| a == *pattern))
            .map(|s| (*s).to_owned())
            .collect();

        Self {
            _workspace_root: workspace_root.to_path_buf(),
            effective_default_deny,
            user_ignore,
            additional_deny: config.additional_deny.clone(),
        }
    }

    /// Check if a file path is accessible.
    ///
    /// # Errors
    /// Returns `PathfinderError::AccessDenied` if the path is blocked.
    pub fn check(&self, relative_path: &Path) -> Result<(), PathfinderError> {
        let path_str = relative_path.to_string_lossy();

        // Tier 1: Hardcoded deny (cannot be overridden)
        if Self::is_hardcoded_denied(&path_str, relative_path) {
            return Err(PathfinderError::AccessDenied {
                path: relative_path.to_path_buf(),
                tier: SandboxTier::HardcodedDeny,
            });
        }

        // Tier 2: Default deny (overridable via config)
        if self.is_default_denied(&path_str) {
            return Err(PathfinderError::AccessDenied {
                path: relative_path.to_path_buf(),
                tier: SandboxTier::DefaultDeny,
            });
        }

        // Check additional deny from config
        if self.is_additional_denied(&path_str) {
            return Err(PathfinderError::AccessDenied {
                path: relative_path.to_path_buf(),
                tier: SandboxTier::DefaultDeny,
            });
        }

        // Tier 3: User-defined (.pathfinderignore)
        if self.is_user_denied(relative_path) {
            return Err(PathfinderError::AccessDenied {
                path: relative_path.to_path_buf(),
                tier: SandboxTier::UserDefined,
            });
        }

        Ok(())
    }

    fn is_hardcoded_denied(path_str: &str, path: &Path) -> bool {
        // Check if path is in the git allowlist first
        for allowed in GIT_ALLOWLIST {
            if path_str.starts_with(allowed) || path_str == *allowed {
                return false;
            }
        }

        // Check hardcoded deny patterns
        for pattern in HARDCODED_DENY_PATTERNS {
            if path_str.starts_with(pattern) {
                return true;
            }
        }

        // Check hardcoded deny extensions
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy();
            if HARDCODED_DENY_EXTENSIONS
                .iter()
                .any(|e| ext_str.eq_ignore_ascii_case(e))
            {
                return true;
            }
        }

        false
    }

    fn is_default_denied(&self, path_str: &str) -> bool {
        for pattern in &self.effective_default_deny {
            // Handle directory patterns (ending with /)
            if pattern.ends_with('/') {
                let dir_prefix = pattern.trim_end_matches('/');
                if path_str.starts_with(dir_prefix)
                    && (path_str.len() == dir_prefix.len()
                        || path_str.as_bytes().get(dir_prefix.len()) == Some(&b'/'))
                {
                    return true;
                }
            }
            // Handle wildcard patterns like ".env.*"
            else if pattern.contains('*') {
                if let Some(prefix) = pattern.strip_suffix("*") {
                    // Simple prefix wildcard
                    let basename = Path::new(path_str)
                        .file_name()
                        .map_or_else(|| path_str.to_string(), |f| f.to_string_lossy().to_string());
                    if basename.starts_with(prefix.trim_start_matches('/')) {
                        return true;
                    }
                }
            }
            // Handle exact file matches
            else {
                let basename = Path::new(path_str)
                    .file_name()
                    .map_or_else(|| path_str.to_string(), |f| f.to_string_lossy().to_string());
                if basename == *pattern || path_str == *pattern {
                    return true;
                }
            }
        }
        false
    }

    fn is_additional_denied(&self, path_str: &str) -> bool {
        for pattern in &self.additional_deny {
            // Simple glob matching for additional patterns
            if pattern.starts_with("*.") {
                let ext = pattern.trim_start_matches("*.");
                if path_str.ends_with(&format!(".{ext}")) {
                    return true;
                }
            } else if path_str.contains(pattern.as_str()) {
                return true;
            }
        }
        false
    }

    fn is_user_denied(&self, path: &Path) -> bool {
        if let Some(ignore) = &self.user_ignore {
            // Avoid live I/O stat: guess based on trailing slash, otherwise default to false
            let is_dir = path.to_string_lossy().ends_with('/');
            ignore.matched(path, is_dir).is_ignore()
        } else {
            false
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// Build a sandbox with no disk I/O and no user-defined ignore rules.
    ///
    /// Uses `with_user_rules` so tests are completely in-memory and avoid
    /// touching the real file system at the hardcoded `/tmp/test` path.
    fn default_sandbox() -> Sandbox {
        Sandbox::with_user_rules(
            std::env::temp_dir().as_path(),
            &SandboxConfig::default(),
            None,
        )
    }

    #[test]
    fn test_hardcoded_deny_git_objects() {
        let sandbox = default_sandbox();
        let result = sandbox.check(Path::new(".git/objects/abc123"));
        assert!(result.is_err());
        if let Err(PathfinderError::AccessDenied { tier, .. }) = result {
            assert!(matches!(tier, SandboxTier::HardcodedDeny));
        }
    }

    #[test]
    fn test_hardcoded_deny_pem_file() {
        let sandbox = default_sandbox();
        assert!(sandbox.check(Path::new("certs/server.pem")).is_err());
        assert!(sandbox.check(Path::new("keys/private.key")).is_err());
        assert!(sandbox.check(Path::new("cert.pfx")).is_err());
    }

    #[test]
    fn test_git_allowlist() {
        let sandbox = default_sandbox();
        assert!(sandbox.check(Path::new(".gitignore")).is_ok());
        assert!(sandbox.check(Path::new(".github/workflows/ci.yml")).is_ok());
        assert!(sandbox
            .check(Path::new(".github/actions/custom/action.yml"))
            .is_ok());
    }

    #[test]
    fn test_default_deny_env() {
        let sandbox = default_sandbox();
        assert!(sandbox.check(Path::new(".env")).is_err());
    }

    #[test]
    fn test_default_deny_node_modules() {
        let sandbox = default_sandbox();
        assert!(sandbox
            .check(Path::new("node_modules/express/index.js"))
            .is_err());
    }

    #[test]
    fn test_default_deny_vendor() {
        let sandbox = default_sandbox();
        assert!(sandbox.check(Path::new("vendor/github.com/pkg")).is_err());
    }

    #[test]
    fn test_allow_override() {
        let config = SandboxConfig {
            additional_deny: vec![],
            allow_override: vec![".env".to_owned()],
        };
        let sandbox = Sandbox::with_user_rules(std::env::temp_dir().as_path(), &config, None);
        // .env should now be allowed because it's in allow_override
        assert!(sandbox.check(Path::new(".env")).is_ok());
    }

    #[test]
    fn test_additional_deny() {
        let config = SandboxConfig {
            additional_deny: vec!["*.generated.ts".to_owned()],
            allow_override: vec![],
        };
        let sandbox = Sandbox::with_user_rules(std::env::temp_dir().as_path(), &config, None);
        assert!(sandbox.check(Path::new("src/schema.generated.ts")).is_err());
        // Normal TS files should be fine
        assert!(sandbox.check(Path::new("src/auth.ts")).is_ok());
    }

    #[test]
    fn test_normal_source_files_allowed() {
        let sandbox = default_sandbox();
        assert!(sandbox.check(Path::new("src/main.rs")).is_ok());
        assert!(sandbox.check(Path::new("src/auth.ts")).is_ok());
        assert!(sandbox.check(Path::new("README.md")).is_ok());
        assert!(sandbox.check(Path::new("Cargo.toml")).is_ok());
    }

    #[test]
    fn test_hardcoded_deny_cannot_be_overridden() {
        let config = SandboxConfig {
            additional_deny: vec![],
            allow_override: vec![".git/objects/".to_owned()],
        };
        let sandbox = Sandbox::with_user_rules(std::env::temp_dir().as_path(), &config, None);
        // Hardcoded deny cannot be overridden by allow_override
        assert!(sandbox.check(Path::new(".git/objects/abc")).is_err());
    }

    // ── Pure in-memory testability ────────────────────────────────────────────
    // These tests use `with_user_rules` to exercise the full sandbox logic
    // without any disk I/O — no `.pathfinderignore` on disk needed.

    #[test]
    fn test_with_user_rules_none_skips_tier3() {
        // No user-defined rules: Tier 3 always passes.
        let sandbox = Sandbox::with_user_rules(
            std::env::temp_dir().as_path(),
            &SandboxConfig::default(),
            None,
        );
        // A path that would be caught only by .pathfinderignore — must pass.
        assert!(sandbox.check(Path::new("some/custom/path.txt")).is_ok());
    }

    #[test]
    fn test_with_user_rules_injected_ignore() {
        // Build a Gitignore rule set in memory (workspace at temp_dir, no on-disk file needed).
        let workspace = std::env::temp_dir();
        let mut builder = GitignoreBuilder::new(&workspace);
        // Add a rule without a backing file — GitignoreBuilder::add_line is available.
        builder
            .add_line(None, "blocked_by_user.txt")
            .expect("valid pattern");
        let gitignore = builder.build().expect("valid gitignore");

        let sandbox = Sandbox::with_user_rules(
            workspace.as_path(),
            &SandboxConfig::default(),
            Some(gitignore),
        );
        // The injected rule blocks the path.
        assert!(sandbox.check(Path::new("blocked_by_user.txt")).is_err());
        // Other paths are unaffected.
        assert!(sandbox.check(Path::new("src/main.rs")).is_ok());
    }
}
