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

/// Extract the basename from a path string, returning the path itself if none.
fn path_basename(path_str: &str) -> std::borrow::Cow<'_, str> {
    Path::new(path_str)
        .file_name()
        .map_or_else(|| path_str.into(), |f| f.to_string_lossy())
}

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

/// Pre-computed additional deny pattern. Classifies each raw config pattern
/// once at init time so `is_additional_denied()` performs zero allocations.
enum AdditionalDenyPattern {
    /// `*.ext` — match by file extension suffix (pre-computed `.ext` form).
    ExtensionGlob { dot_ext: String },
    /// `dir/` — directory pattern with pre-computed boundary variants.
    Directory {
        dir: String,
        dir_slash: String,
        slash_dir_slash: String,
        slash_dir: String,
    },
    /// Bare-word pattern — exact filename match only.
    Exact { pattern: String },
}

/// The sandbox enforcer. Checks file paths against the three-tier deny model.
pub struct Sandbox {
    /// Workspace root path. Used for normalizing same-workspace absolute paths.
    workspace_root: PathBuf,
    /// Compiled default deny patterns with overrides applied.
    effective_default_deny: Vec<String>,
    /// User-defined `.pathfinderignore` rules.
    user_ignore: Option<Gitignore>,
    /// Config-level additional deny patterns (pre-computed for zero-alloc checks).
    additional_deny: Vec<AdditionalDenyPattern>,
    /// Pre-computed slash-suffixed allowed paths from `GIT_ALLOWLIST`.
    git_allowlist_slash: Vec<String>,
}

impl Sandbox {
    /// Create a new sandbox enforcer.
    ///
    /// Reads `.pathfinderignore` from the workspace root if it exists.
    /// For unit tests that must not touch the file system, use
    /// [`Sandbox::with_user_rules`] instead.
    #[must_use]
    pub fn new(workspace_root: &Path, config: &SandboxConfig) -> Self {
        // `.exists()` is a synchronous stat(2) syscall. This is intentional:
        // `Sandbox::new` is called once at server startup, not on the hot path.
        // If Pathfinder is ever embedded in a multi-tenant async host, this
        // should move into `tokio::task::spawn_blocking`.
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

        // Pre-compute additional deny patterns so is_additional_denied()
        // performs zero heap allocations per check() call.
        let additional_deny = config
            .additional_deny
            .iter()
            .map(|pattern| {
                if let Some(ext) = pattern.strip_prefix("*.") {
                    AdditionalDenyPattern::ExtensionGlob {
                        dot_ext: format!(".{ext}"),
                    }
                } else if let Some(dir) = pattern.strip_suffix('/') {
                    AdditionalDenyPattern::Directory {
                        dir: dir.to_owned(),
                        dir_slash: format!("{dir}/"),
                        slash_dir_slash: format!("/{dir}/"),
                        slash_dir: format!("/{dir}"),
                    }
                } else {
                    AdditionalDenyPattern::Exact {
                        // CLONE: pattern is borrowed from config and needs to be owned by AdditionalDenyPattern struct
                        pattern: pattern.clone(),
                    }
                }
            })
            .collect();

        // Pre-compute slash-suffixed versions only for bare filename entries
        // (e.g. ".gitignore" -> ".gitignore/"). Directory entries like
        // ".github/workflows/" are handled by the `starts_with(allowed)`
        // branch before reaching `git_allowlist_slash`, so they don't need
        // a precomputed variant. Using an empty string for directory entries
        // keeps the index alignment with GIT_ALLOWLIST intact.
        let git_allowlist_slash = GIT_ALLOWLIST
            .iter()
            .map(|allowed| {
                if allowed.ends_with('/') {
                    String::new()
                } else {
                    format!("{allowed}/")
                }
            })
            .collect();

        Self {
            workspace_root: workspace_root.to_path_buf(),
            effective_default_deny,
            user_ignore,
            additional_deny,
            git_allowlist_slash,
        }
    }

    /// Check if a file path is accessible.
    ///
    /// # Errors
    /// Returns `PathfinderError::AccessDenied` if the path is blocked.
    pub fn check(&self, relative_path: &Path) -> Result<(), PathfinderError> {
        // Normalize same-workspace absolute paths by stripping workspace_root prefix
        let path_to_check = if relative_path.is_absolute() {
            // Try to strip the workspace root prefix
            if let Ok(stripped) = relative_path.strip_prefix(&self.workspace_root) {
                // Same-workspace absolute path: normalize to relative
                stripped
            } else {
                // Cross-workspace absolute path: deny
                return Err(PathfinderError::AccessDenied {
                    path: relative_path.to_path_buf(),
                    tier: SandboxTier::HardcodedDeny,
                });
            }
        } else {
            // Relative path: use as-is
            relative_path
        };

        // Protect against path traversal
        if path_to_check
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(PathfinderError::AccessDenied {
                path: relative_path.to_path_buf(),
                tier: SandboxTier::HardcodedDeny,
            });
        }

        let path_str = path_to_check.to_string_lossy();

        // Tier 1: Hardcoded deny (cannot be overridden)
        if self.is_hardcoded_denied(&path_str, path_to_check) {
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
        if self.is_user_denied(path_to_check) {
            return Err(PathfinderError::AccessDenied {
                path: relative_path.to_path_buf(),
                tier: SandboxTier::UserDefined,
            });
        }

        Ok(())
    }

    fn is_hardcoded_denied(&self, path_str: &str, path: &Path) -> bool {
        // Fast-reject: if path doesn't start with '.', it can't match any
        // hardcoded deny pattern (.git/*, .pem, .key, .pfx, .p12).
        // Only check extension for non-dot paths.
        if !path_str.starts_with('.') {
            if let Some(ext) = path.extension() {
                let ext_str = ext.to_string_lossy();
                if HARDCODED_DENY_EXTENSIONS
                    .iter()
                    .any(|e| ext_str.eq_ignore_ascii_case(e))
                {
                    return true;
                }
            }
            return false;
        }

        // Path starts with '.' — check git allowlist first (cheapest)
        for (i, allowed) in GIT_ALLOWLIST.iter().enumerate() {
            if allowed.ends_with('/') {
                // Directory pattern: use prefix match
                if path_str.starts_with(allowed) {
                    return false;
                }
            } else {
                // Bare filename: exact match or prefix with separator
                // Prevents ".gitignorex" from matching ".gitignore"
                if path_str == *allowed || path_str.starts_with(&self.git_allowlist_slash[i]) {
                    return false;
                }
            }
        }

        // Check against all hardcoded deny patterns.
        if HARDCODED_DENY_PATTERNS
            .iter()
            .any(|p| path_str.starts_with(*p))
        {
            return true;
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
        self.effective_default_deny
            .iter()
            .any(|p| Self::matches_default_pattern(p, path_str))
    }

    fn matches_default_pattern(pattern: &str, path_str: &str) -> bool {
        if pattern.ends_with('/') {
            Self::matches_directory_pattern(pattern, path_str)
        } else if pattern.contains('*') {
            Self::matches_wildcard_pattern(pattern, path_str)
        } else {
            Self::matches_exact_pattern(pattern, path_str)
        }
    }

    fn matches_directory_pattern(pattern: &str, path_str: &str) -> bool {
        let dir_prefix = pattern.trim_end_matches('/');
        path_str.starts_with(dir_prefix)
            && (path_str.len() == dir_prefix.len()
                || path_str.as_bytes().get(dir_prefix.len()) == Some(&b'/'))
    }

    fn matches_wildcard_pattern(pattern: &str, path_str: &str) -> bool {
        let Some(prefix) = pattern.strip_suffix('*') else {
            return false;
        };
        let basename = path_basename(path_str);
        basename.starts_with(prefix.trim_start_matches('/'))
    }

    fn matches_exact_pattern(pattern: &str, path_str: &str) -> bool {
        let basename = path_basename(path_str);
        basename == pattern || path_str == pattern
    }

    fn is_additional_denied(&self, path_str: &str) -> bool {
        for pattern in &self.additional_deny {
            match pattern {
                AdditionalDenyPattern::ExtensionGlob { dot_ext } => {
                    if path_str.ends_with(dot_ext.as_str()) {
                        return true;
                    }
                }
                AdditionalDenyPattern::Directory {
                    dir,
                    dir_slash,
                    slash_dir_slash,
                    slash_dir,
                } => {
                    // Match at start or after a path separator so "temp/" does not deny "src/template/"
                    if path_str == dir
                        || path_str.starts_with(dir_slash.as_str())
                        || path_str.contains(slash_dir_slash.as_str())
                        || path_str.ends_with(slash_dir.as_str())
                    {
                        return true;
                    }
                }
                AdditionalDenyPattern::Exact { pattern } => {
                    // Bare-word pattern: match only against the filename component, not the full path
                    // so that "secret" does not deny "src/secretariat/utils.rs"
                    let basename = std::path::Path::new(path_str)
                        .file_name()
                        .map_or(path_str, |f| f.to_str().unwrap_or(path_str));
                    if basename == pattern || path_str == pattern.as_str() {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn is_user_denied(&self, path: &Path) -> bool {
        self.user_ignore.as_ref().is_some_and(|ignore| {
            // Avoid live I/O stat: guess based on trailing slash, otherwise default to false
            let is_dir = path.to_string_lossy().ends_with('/');
            ignore.matched(path, is_dir).is_ignore()
        })
    }
}

#[cfg(test)]
#[path = "sandbox_test.rs"]
mod tests;
