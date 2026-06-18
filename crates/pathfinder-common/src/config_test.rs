
use super::*;
use tempfile::tempdir;

#[test]
fn test_default_config() {
    let config = PathfinderConfig::default();
    assert_eq!(config.log_level, "info");
    assert_eq!(config.search.max_results, 50);
    assert_eq!(config.repo_map.max_tokens, 16_000);
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
    let config_json = r#"{ "log_level": "trace" }"#;
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

#[test]
fn test_validation_invalid_log_level() {
    let json = r#"{ "log_level": "banana" }"#;
    let config: PathfinderConfig = serde_json::from_str(json).unwrap();
    assert!(config.validate().is_err());
}

#[test]
fn test_validation_zero_max_results() {
    let json = r#"{ "search": { "max_results": 0 } }"#;
    let config: PathfinderConfig = serde_json::from_str(json).unwrap();
    assert!(config.validate().is_err());
}

#[test]
fn test_validation_zero_max_tokens() {
    let json = r#"{ "repo_map": { "max_tokens": 0 } }"#;
    let config: PathfinderConfig = serde_json::from_str(json).unwrap();
    assert!(config.validate().is_err());
}

#[test]
fn test_validation_valid_cases() {
    let config = PathfinderConfig::default();
    assert!(config.validate().is_ok());
}

// ── Phase 2 §5: [lsp.java] config section ────────────────────────────────

/// Phase 2 §5: Full jdtls config block deserializes correctly via the
/// generic `lsp` `HashMap` (no Java-specific schema additions needed).
///
/// Verifies: command, args, `idle_timeout_minutes`, and settings all round-trip
/// through `serde_json`.
#[test]
fn test_java_lsp_config_section() {
    let json = r#"{
            "lsp": {
                "java": {
                    "command": "jdtls",
                    "args": ["--jvm-arg=-Xmx2G"],
                    "idle_timeout_minutes": 20,
                    "settings": {
                        "java": {
                            "format": { "enabled": true },
                            "import": {
                                "gradle": { "enabled": true },
                                "maven": { "enabled": true }
                            }
                        }
                    }
                }
            }
        }"#;

    let config: PathfinderConfig = serde_json::from_str(json).expect("should deserialize");
    assert!(
        config.lsp.contains_key("java"),
        "[lsp.java] key must be present"
    );

    let java_config = &config.lsp["java"];
    assert_eq!(java_config.command, "jdtls", "command must be jdtls");
    assert_eq!(
        java_config.args,
        vec!["--jvm-arg=-Xmx2G"],
        "args must round-trip"
    );
    assert_eq!(
        java_config.idle_timeout_minutes, 20,
        "idle timeout must be 20"
    );

    // settings blob preserved
    let settings = &java_config.settings;
    assert!(!settings.is_null(), "settings must not be null");
    assert!(
        settings.get("java").is_some(),
        "settings.java key must be present"
    );
}

/// Phase 2 §5: Minimal jdtls config (command only) uses defaults for
/// all optional fields.
#[test]
fn test_java_lsp_config_minimal() {
    let json = r#"{
            "lsp": {
                "java": {
                    "command": "jdtls"
                }
            }
        }"#;

    let config: PathfinderConfig = serde_json::from_str(json).expect("should deserialize");
    let java_config = &config.lsp["java"];

    assert_eq!(java_config.command, "jdtls");
    assert!(java_config.args.is_empty(), "args should default to empty");
    assert_eq!(
        java_config.idle_timeout_minutes,
        default_idle_timeout(),
        "idle_timeout_minutes must default to {}",
        default_idle_timeout()
    );
    assert!(
        java_config.settings.is_null(),
        "settings should default to null"
    );
    assert!(
        java_config.root_override.is_none(),
        "root_override should default to None"
    );
    assert!(
        java_config.typescript_plugins.is_empty(),
        "typescript_plugins should default to empty"
    );
}

/// Phase 2 §5: Java and TypeScript LSP configs can coexist in the same
/// config file without interference.
#[test]
fn test_java_and_typescript_lsp_configs_coexist() {
    let json = r#"{
            "lsp": {
                "java": {
                    "command": "jdtls",
                    "idle_timeout_minutes": 30
                },
                "typescript": {
                    "command": "typescript-language-server",
                    "args": ["--stdio"]
                }
            }
        }"#;

    let config: PathfinderConfig = serde_json::from_str(json).expect("should deserialize");
    assert_eq!(
        config.lsp.len(),
        2,
        "both java and typescript must be present"
    );

    assert_eq!(config.lsp["java"].command, "jdtls");
    assert_eq!(config.lsp["java"].idle_timeout_minutes, 30);

    assert_eq!(
        config.lsp["typescript"].command,
        "typescript-language-server"
    );
    assert_eq!(config.lsp["typescript"].args, vec!["--stdio"]);
}

#[tokio::test]
async fn test_load_rejects_invalid_config_from_file() {
    let temp = tempdir().expect("create tempdir");
    let config_json = r#"{ "log_level": "banana" }"#;
    std::fs::write(temp.path().join("pathfinder.config.json"), config_json)
        .expect("should write config");

    let result = PathfinderConfig::load(temp.path()).await;
    assert!(result.is_err());
    assert!(matches!(result, Err(ConfigError::ValidationError(_))));
}

#[test]
fn test_validation_mixed_case_log_level() {
    for level in ["INFO", "Debug", "TRACE", "Warn", "ERROR"] {
        let json = format!(r#"{{ "log_level": "{level}" }}"#);
        let config: PathfinderConfig = serde_json::from_str(&json).unwrap();
        assert!(
            config.validate().is_ok(),
            "log_level '{level}' should be accepted (case-insensitive)"
        );
    }
}

#[test]
fn test_validation_min_valid_values() {
    let json = r#"{ "search": { "max_results": 1 }, "repo_map": { "max_tokens": 1 } }"#;
    let config: PathfinderConfig = serde_json::from_str(json).unwrap();
    assert!(
        config.validate().is_ok(),
        "max_results=1 and max_tokens=1 should be valid"
    );
}

/// Verifies that a non-NotFound I/O error (e.g. path is a directory)
/// propagates as `ReadFailed` instead of being silently swallowed as defaults.
#[tokio::test]
async fn test_load_propagates_non_notfound_io_error() {
    let temp = tempdir().expect("create tempdir");
    // Create a directory named pathfinder.config.json — metadata() succeeds
    // but read_to_string() will fail with IsADirectory (not NotFound).
    let config_path = temp.path().join("pathfinder.config.json");
    std::fs::create_dir(&config_path).expect("should create dir");

    let result = PathfinderConfig::load(temp.path()).await;
    assert!(
        result.is_err(),
        "should fail when config path is a directory"
    );
    assert!(
        matches!(result, Err(ConfigError::ReadFailed { .. })),
        "expected ReadFailed, got: {result:?}"
    );
}
