//! Configuration loading for Pathfinder.
//!
//! Supports zero-config defaults with optional `pathfinder.config.json`
//! override. See PRD §10 for the configuration schema.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Top-level Pathfinder configuration.
///
/// All fields have sensible defaults. An absent `pathfinder.config.json`
/// file is perfectly valid — Pathfinder works out of the box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathfinderConfig {
    /// Per-language LSP configurations.
    #[serde(default)]
    pub lsp: HashMap<String, LspConfig>,

    /// Sandbox configuration overrides.
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Search defaults.
    #[serde(default)]
    pub search: SearchConfig,

    /// Repo map defaults.
    #[serde(default)]
    pub repo_map: RepoMapConfig,

    /// Validation scope.
    #[serde(default)]
    pub validation: ValidationConfig,

    /// Log level.
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for PathfinderConfig {
    fn default() -> Self {
        Self {
            lsp: HashMap::new(),
            sandbox: SandboxConfig::default(),
            search: SearchConfig::default(),
            repo_map: RepoMapConfig::default(),
            validation: ValidationConfig::default(),
            log_level: default_log_level(),
        }
    }
}

impl PathfinderConfig {
    /// Load configuration from the workspace root.
    ///
    /// If `pathfinder.config.json` exists, it is loaded and merged with defaults.
    /// If it doesn't exist, defaults are returned.
    ///
    /// # Errors
    /// Returns an error if the file exists but contains invalid JSON.
    pub async fn load(workspace_root: &Path) -> Result<Self, ConfigError> {
        let config_path = workspace_root.join("pathfinder.config.json");

        if !config_path.exists() {
            tracing::debug!("No pathfinder.config.json found, using defaults");
            return Ok(Self::default());
        }

        let content =
            tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|e| ConfigError::ReadFailed {
                    path: config_path.clone(),
                    source: e,
                })?;

        let config: Self =
            serde_json::from_str(&content).map_err(|e| ConfigError::ParseFailed {
                path: config_path,
                source: e,
            })?;

        tracing::info!("Loaded configuration from pathfinder.config.json");
        Ok(config)
    }
}

/// LSP server configuration for a specific language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspConfig {
    /// Command to start the LSP server.
    pub command: String,

    /// Arguments to pass to the LSP server.
    #[serde(default)]
    pub args: Vec<String>,

    /// Idle timeout in minutes before auto-termination.
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_minutes: u64,

    /// Additional LSP settings (workspace/didChangeConfiguration).
    #[serde(default)]
    pub settings: serde_json::Value,

    /// Override the root directory used for this language server.
    ///
    /// Use this in monorepo layouts where the language marker file (e.g.,
    /// `go.mod`, `tsconfig.json`) is not at the workspace root. The path
    /// is relative to the workspace root.
    ///
    /// Example: `"apps/backend"` for a Go backend in a monorepo.
    #[serde(default)]
    pub root_override: Option<String>,

    /// TypeScript plugins to load via initializationOptions.
    ///
    /// Each entry is a plugin name that will be resolved from `node_modules`.
    /// Example: `"@vue/typescript-plugin"`
    #[serde(default)]
    pub typescript_plugins: Vec<String>,
}

/// Sandbox configuration overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Additional patterns to deny (on top of defaults).
    #[serde(default)]
    pub additional_deny: Vec<String>,

    /// Patterns to allow (override default deny list).
    #[serde(default)]
    pub allow_override: Vec<String>,
}

/// Search tool defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Maximum results returned by `search_codebase`.
    #[serde(default = "default_max_results")]
    pub max_results: usize,

    /// Default filter mode.
    #[serde(default = "default_filter_mode")]
    pub default_filter_mode: String,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            max_results: default_max_results(),
            default_filter_mode: default_filter_mode(),
        }
    }
}

/// Repo map defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMapConfig {
    /// Default maximum tokens for `get_repo_map`.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,

    /// Token counting method.
    #[serde(default = "default_token_method")]
    pub token_method: String,
}

impl Default for RepoMapConfig {
    fn default() -> Self {
        Self {
            max_tokens: default_max_tokens(),
            token_method: default_token_method(),
        }
    }
}

/// Validation scope configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    /// Validation scope for edit tools.
    #[serde(default = "default_validation_scope")]
    pub scope: String,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            scope: default_validation_scope(),
        }
    }
}

/// Configuration loading errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Error when reading the configuration file fails.
    #[error("failed to read config file {}: {source}", path.display())]
    ReadFailed {
        /// Path to the configuration file that failed to read.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// Error when parsing the configuration file fails.
    #[error("failed to parse config file {}: {source}", path.display())]
    ParseFailed {
        /// Path to the configuration file that failed to parse.
        path: std::path::PathBuf,
        /// Underlying JSON parsing error.
        source: serde_json::Error,
    },
}

fn default_log_level() -> String {
    "info".to_owned()
}

const fn default_idle_timeout() -> u64 {
    15
}

const fn default_max_results() -> usize {
    50
}

fn default_filter_mode() -> String {
    "code_only".to_owned()
}

const fn default_max_tokens() -> usize {
    16_000
}

fn default_token_method() -> String {
    "char_div_4".to_owned()
}

fn default_validation_scope() -> String {
    "workspace_wide".to_owned()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_default_config() {
        let config = PathfinderConfig::default();
        assert_eq!(config.log_level, "info");
        assert_eq!(config.search.max_results, 50);
        assert_eq!(config.repo_map.max_tokens, 16_000);
        assert_eq!(config.validation.scope, "workspace_wide");
        assert!(config.lsp.is_empty());
    }

    #[test]
    fn test_config_deserialization() {
        let json = r#"{
            "lsp": {
                "typescript": {
                    "command": "typescript-language-server",
                    "args": ["--stdio"],
                    "idle_timeout_minutes": 30
                }
            },
            "sandbox": {
                "additional_deny": ["*.generated.ts"],
                "allow_override": [".env.example"]
            },
            "search": {
                "max_results": 100
            },
            "log_level": "debug"
        }"#;

        let config: PathfinderConfig = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.search.max_results, 100);
        assert!(config.lsp.contains_key("typescript"));

        let ts_config = &config.lsp["typescript"];
        assert_eq!(ts_config.command, "typescript-language-server");
        assert_eq!(ts_config.idle_timeout_minutes, 30);
    }

    #[test]
    fn test_partial_config_uses_defaults() {
        let json = r#"{ "log_level": "warn" }"#;
        let config: PathfinderConfig = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(config.log_level, "warn");
        // Other fields should be defaults
        assert_eq!(config.search.max_results, 50);
        assert_eq!(config.repo_map.max_tokens, 16_000);
    }

    #[tokio::test]
    async fn test_load_missing_config_returns_defaults() {
        let temp = tempdir().expect("create tempdir");
        let config = PathfinderConfig::load(temp.path())
            .await
            .expect("should return defaults");
        assert_eq!(config.log_level, "info");
        // TempDir cleans up automatically on drop
    }

    #[tokio::test]
    async fn test_load_invalid_json_returns_error() {
        let temp = std::env::temp_dir().join("pathfinder_test_bad_json");
        let _ = std::fs::create_dir_all(&temp);
        std::fs::write(temp.join("pathfinder.config.json"), "not json").expect("should write");

        let result = PathfinderConfig::load(&temp).await;
        assert!(result.is_err());
        assert!(matches!(result, Err(ConfigError::ParseFailed { .. })));

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn test_load_valid_config_from_file() {
        let temp = tempdir().expect("create tempdir");
        let config_json = r#"{ "log_level": "trace", "idle_timeout": 30 }"#;
        std::fs::write(temp.path().join("pathfinder.config.json"), config_json)
            .expect("should write config");

        let config = PathfinderConfig::load(temp.path())
            .await
            .expect("should load valid config");
        assert_eq!(config.log_level, "trace");
        // Fields not in the JSON should retain defaults
        assert_eq!(config.search.max_results, 50);
    }

    #[test]
    fn test_default_idle_timeout_value() {
        assert_eq!(default_idle_timeout(), 15);
    }

    #[test]
    fn test_typescript_plugins_defaults_to_empty() {
        let config: LspConfig =
            serde_json::from_str(r#"{ "command": "tsserver" }"#).expect("should parse");
        assert!(config.typescript_plugins.is_empty());
    }

    #[test]
    fn test_typescript_plugins_deserialization() {
        let json = r#"{
            "lsp": {
                "typescript": {
                    "command": "typescript-language-server",
                    "typescript_plugins": ["@vue/typescript-plugin", "@angular/language-service"]
                }
            }
        }"#;

        let config: PathfinderConfig = serde_json::from_str(json).expect("should deserialize");
        let ts_config = &config.lsp["typescript"];
        assert_eq!(ts_config.typescript_plugins.len(), 2);
        assert_eq!(ts_config.typescript_plugins[0], "@vue/typescript-plugin");
        assert_eq!(ts_config.typescript_plugins[1], "@angular/language-service");
    }
}
