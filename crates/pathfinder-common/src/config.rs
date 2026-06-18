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
    /// Returns an error if the file exists but contains invalid JSON or fails validation.
    pub async fn load(workspace_root: &Path) -> Result<Self, ConfigError> {
        let config_path = workspace_root.join("pathfinder.config.json");

        match tokio::fs::metadata(&config_path).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(workspace = %workspace_root.display(), "No pathfinder.config.json found, using defaults");
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(ConfigError::ReadFailed {
                    path: config_path,
                    source: e,
                });
            }
            Ok(_) => {}
        }

        let content =
            tokio::fs::read_to_string(&config_path)
                .await
                .map_err(|e| ConfigError::ReadFailed {
                    // CLONE: config_path is needed for the error struct while the original is moved later
                    path: config_path.clone(),
                    source: e,
                })?;

        let config: Self =
            serde_json::from_str(&content).map_err(|e| ConfigError::ParseFailed {
                path: config_path,
                source: e,
            })?;

        config.validate()?;

        tracing::info!(workspace = %workspace_root.display(), "Loaded configuration from pathfinder.config.json");
        Ok(config)
    }

    /// Validate configuration values.
    ///
    /// # Errors
    /// Returns a `ConfigValidationError` if any configuration value is invalid.
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        let log_level = self.log_level.to_lowercase();
        if !matches!(
            log_level.as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        ) {
            return Err(ConfigValidationError::InvalidLogLevel(
                // CLONE: self.log_level is returned in the error variant
                self.log_level.clone(),
            ));
        }
        if self.search.max_results == 0 {
            return Err(ConfigValidationError::InvalidMaxResults);
        }
        if self.repo_map.max_tokens == 0 {
            return Err(ConfigValidationError::InvalidMaxTokens);
        }
        Ok(())
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
    /// Maximum results returned by `search`.
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
    /// Default maximum tokens for `explore`.
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

/// Errors that can occur during configuration validation.
#[derive(Debug, thiserror::Error)]
pub enum ConfigValidationError {
    /// Invalid log level.
    #[error("invalid log level: '{0}' (expected 'trace', 'debug', 'info', 'warn', or 'error')")]
    InvalidLogLevel(String),

    /// Search results count is zero or negative.
    #[error("search.max_results must be greater than zero")]
    InvalidMaxResults,

    /// Repo map max tokens is zero or negative.
    #[error("repo_map.max_tokens must be greater than zero")]
    InvalidMaxTokens,
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

    /// Error when configuration validation fails.
    #[error("invalid configuration: {0}")]
    ValidationError(#[from] ConfigValidationError),
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

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
