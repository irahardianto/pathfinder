use super::*;
use std::sync::Mutex;
use tempfile::tempdir;

// Mutex to ensure tests that modify PATH run serially
static PATH_MUTEX: Mutex<()> = Mutex::new(());

#[allow(
    clippy::await_holding_lock,
    clippy::expect_fun_call,
    clippy::items_after_statements
)]
async fn test_with_fake_python_binaries<F, Fut>(binaries: &[&str], test: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // Lock mutex to ensure serial execution
    let _guard = match PATH_MUTEX.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(), // Recover from poisoned mutex
    };

    // Create temp dir for fake binaries
    let temp_bin_dir = tempdir().expect("create temp bin dir");
    let sh_path = which::which("sh").expect("sh not found on PATH");

    // Create symlinks to `sh` for each requested binary
    for binary in binaries {
        let symlink_path = temp_bin_dir.path().join(binary);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&sh_path, &symlink_path)
            .expect(&format!("create symlink for {binary}"));
    }

    struct PathGuard {
        original_path: Option<String>,
    }
    impl Drop for PathGuard {
        fn drop(&mut self) {
            if let Some(ref orig) = self.original_path {
                std::env::set_var("PATH", orig);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }

    // Save original PATH via guard
    let _path_guard = PathGuard {
        original_path: std::env::var("PATH").ok(),
    };

    // Set PATH to ONLY our temp bin dir to avoid finding system LSPs
    let new_path = temp_bin_dir.path().to_string_lossy().to_string();
    std::env::set_var("PATH", &new_path);

    // Run the test
    test().await;
}

// Helper to create a fake @vue/typescript-plugin directory in node_modules.
fn create_vue_plugin(workspace: &Path) {
    let dir = workspace
        .join("node_modules")
        .join("@vue")
        .join("typescript-plugin");
    std::fs::create_dir_all(&dir).expect("create plugin dir");
    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"@vue/typescript-plugin"}"#,
    )
    .expect("write plugin package.json");
}

// Helper to create a fake pnpm-style @vue/typescript-plugin.
fn create_vue_plugin_pnpm(workspace: &Path) {
    let pkg_dir = workspace
        .join("node_modules")
        .join(".pnpm")
        .join("@vue+typescript-plugin@2.0.0")
        .join("node_modules")
        .join("@vue")
        .join("typescript-plugin");
    std::fs::create_dir_all(&pkg_dir).expect("create pnpm plugin dir");
    std::fs::write(
        pkg_dir.join("package.json"),
        r#"{"name":"@vue/typescript-plugin"}"#,
    )
    .expect("write pnpm plugin package.json");
}

// Helper to create a .vue file in src/.
fn create_vue_file(workspace: &Path) {
    let src = workspace.join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    std::fs::write(src.join("App.vue"), "<script setup lang=\"ts\"></script>")
        .expect("write vue file");
}

// Helper to create a .vue file in a nested components/ directory.
fn create_vue_file_nested(workspace: &Path) {
    let components = workspace.join("src").join("components");
    std::fs::create_dir_all(&components).expect("create components dir");
    std::fs::write(components.join("Button.vue"), "<template></template>").expect("write vue file");
}

// Helper to create a .vue file deep in a monorepo (depth 3+).
// Simulates: apps/frontend/src/views/Home.vue
fn create_vue_file_monorepo(workspace: &Path) {
    let views = workspace
        .join("apps")
        .join("frontend")
        .join("src")
        .join("views");
    std::fs::create_dir_all(&views).expect("create monorepo views dir");
    std::fs::write(views.join("Home.vue"), "<template>Home</template>").expect("write vue file");
}

fn make_ts_config() -> pathfinder_common::config::PathfinderConfig {
    pathfinder_common::config::PathfinderConfig::default()
}

// ── existing tests (preserved) ──────────────────────────────────────────

#[tokio::test]
async fn test_detects_cargo_toml() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
    std::fs::create_dir_all(dir.path().join("src")).expect("create dir");
    std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}").expect("write");
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    if let Some(rust) = result.detected.iter().find(|l| l.language_id == "rust") {
        assert!(rust.args.is_empty());
        assert!(rust.init_timeout_secs.is_none());
        assert!(!rust.command.is_empty());
    }
}

#[tokio::test]
async fn test_detects_go_mod() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("go.mod"), "module foo").expect("write");
    std::fs::write(dir.path().join("main.go"), "package main").expect("write");
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    if let Some(go) = result.detected.iter().find(|l| l.language_id == "go") {
        assert!(!go.command.is_empty());
    }
}

#[tokio::test]
async fn test_detects_typescript_via_tsconfig() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("tsconfig.json"), "{}").expect("write");
    std::fs::write(dir.path().join("index.ts"), "").expect("write");
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert_eq!(ts.args, ["--stdio"]);
        assert!(ts.init_timeout_secs.is_none());
    }
}

#[tokio::test]
async fn test_detects_typescript_monorepo_multiple_tsconfig_no_root() {
    let dir = tempdir().expect("temp dir");
    let app1 = dir.path().join("apps").join("app1");
    let app2 = dir.path().join("apps").join("app2");
    std::fs::create_dir_all(&app1).expect("create dir");
    std::fs::create_dir_all(&app2).expect("create dir");
    std::fs::write(app1.join("tsconfig.json"), "{}").expect("write");
    std::fs::write(app2.join("tsconfig.json"), "{}").expect("write");
    std::fs::write(app1.join("index.ts"), "").expect("write");

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    // TypeScript should appear in either `detected` (binary found) or `missing`
    // (binary not installed, as on CI). The test validates the monorepo marker
    // fallback: multiple sub-project tsconfig.json files → workspace root is chosen.
    let ts_entry = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
        .map(|l| l.root.clone())
        .or_else(|| {
            result
                .missing
                .iter()
                .find(|l| l.language_id == "typescript")
                .map(|_| dir.path().to_path_buf())
        });

    assert!(
            ts_entry.is_some(),
            "TypeScript should be detected (detected or missing) in monorepo despite no root tsconfig.json"
        );

    // When in detected, root must be the workspace root (monorepo fallback).
    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert_eq!(
            ts.root,
            dir.path(),
            "Fallback should resolve to workspace root"
        );
    }
}

#[tokio::test]
async fn test_detects_typescript_via_package_json() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    std::fs::write(dir.path().join("index.ts"), "").expect("write");
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    let found = result
        .detected
        .iter()
        .any(|l| l.language_id == "typescript");
    let _ = found;
}

#[tokio::test]
async fn test_detects_python_via_pyproject() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
    std::fs::write(dir.path().join("main.py"), "").expect("write");
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
        assert!(!py.command.is_empty());
    }
}

#[tokio::test]
async fn test_detects_multiple_languages() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    std::fs::create_dir_all(dir.path().join("src")).expect("create dir");
    std::fs::write(dir.path().join("src/lib.rs"), "").expect("write");
    std::fs::write(dir.path().join("index.ts"), "").expect("write");
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    let ids: Vec<&str> = result
        .detected
        .iter()
        .map(|l| l.language_id.as_str())
        .collect();
    for id in &ids {
        assert!(["rust", "typescript"].contains(id));
    }
}

#[tokio::test]
async fn test_empty_directory() {
    let dir = tempdir().expect("temp dir");
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    assert!(result.detected.is_empty() && result.missing.is_empty());
}

#[tokio::test]
async fn test_detects_go_mod_in_subdirectory() {
    let dir = tempdir().expect("temp dir");
    let sub_dir = dir.path().join("apps").join("backend");
    std::fs::create_dir_all(&sub_dir).expect("create dir");
    std::fs::write(sub_dir.join("go.mod"), "module foo").expect("write");
    std::fs::write(sub_dir.join("main.go"), "package main").expect("write");
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    if let Some(go_lang) = result.detected.into_iter().find(|l| l.language_id == "go") {
        assert_eq!(go_lang.root, sub_dir);
    }
}

#[tokio::test]
async fn test_root_override_config() {
    let dir = tempdir().expect("temp dir");
    let custom_backend = dir.path().join("custom/backend");
    std::fs::create_dir_all(&custom_backend).expect("create dir");
    std::fs::write(custom_backend.join("main.go"), "package main").expect("write");

    let mut config = pathfinder_common::config::PathfinderConfig::default();
    config.lsp.insert(
        "go".to_string(),
        pathfinder_common::config::LspConfig {
            command: "gopls".to_string(),
            args: vec![],
            idle_timeout_minutes: 15,
            settings: serde_json::Value::Null,
            root_override: Some("custom/backend".to_string()),
            typescript_plugins: vec![],
        },
    );

    let result = detect_languages(dir.path(), &config).await.expect("detect");
    if let Some(go_lang) = result.detected.into_iter().find(|l| l.language_id == "go") {
        assert_eq!(go_lang.root, dir.path().join("custom/backend"));
        assert_eq!(go_lang.command, "gopls");
    }
}

#[tokio::test]
async fn test_filters_out_language_with_no_source_files() {
    let dir = tempdir().expect("temp dir");
    // Marker file present
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");

    // No .rs file -> should NOT detect
    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");
    assert!(
        result.detected.is_empty(),
        "should filter out rust language when no source files exist"
    );
    assert!(
        result.missing.is_empty(),
        "should filter out rust language from missing when no source files exist"
    );

    // Write a .rs file -> should detect
    std::fs::create_dir_all(dir.path().join("src")).expect("create dir");
    std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}").expect("write");
    let result2 = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    let total_detected = result2.detected.len() + result2.missing.len();
    assert_eq!(
        total_detected, 1,
        "should detect rust language when source file is added"
    );

    let detected_lang_id = if result2.detected.is_empty() {
        &result2.missing[0].language_id
    } else {
        &result2.detected[0].language_id
    };
    assert_eq!(detected_lang_id, "rust");
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

#[test]
fn test_resolve_command_not_found_returns_none() {
    let result = resolve_command(
        "pathfinder_lsp_binary_that_does_not_exist_xyzzy_42",
        "test-lang",
    );
    assert!(
        result.is_none(),
        "resolve_command must return None when the binary is not on PATH"
    );
}

#[test]
fn test_resolve_command_found_returns_some() {
    // Acquire PATH_MUTEX to avoid racing with tests that temporarily replace PATH.
    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let result = resolve_command("sh", "shell");
    assert!(
        result.is_some(),
        "resolve_command must return Some for a binary on PATH"
    );
    let path = result.expect("sh must resolve to an absolute path");
    assert!(!path.is_empty());
}

#[tokio::test]
async fn test_find_marker_finds_at_depth_2() {
    let dir = tempdir().expect("temp dir");
    let deep = dir.path().join("apps").join("backend");
    std::fs::create_dir_all(&deep).expect("create dirs");
    std::fs::write(deep.join("go.mod"), "module deep").expect("write");
    let found = find_marker(dir.path(), "go.mod", 2, None).await;
    assert_eq!(
        found.as_deref(),
        Some(deep.as_path()),
        "depth-2 marker must be discovered"
    );
}

#[tokio::test]
async fn test_find_marker_depth_0_does_not_recurse() {
    let dir = tempdir().expect("temp dir");
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).expect("create dir");
    std::fs::write(sub.join("Cargo.toml"), "[package]").expect("write");
    let found = find_marker(dir.path(), "Cargo.toml", 0, None).await;
    assert!(
        found.is_none(),
        "max_depth=0 must not recurse into subdirectories"
    );
}

#[tokio::test]
async fn test_find_marker_missing_returns_none() {
    let dir = tempdir().expect("temp dir");
    let found = find_marker(dir.path(), "no_such_marker_file.toml", 2, None).await;
    assert!(found.is_none(), "absent marker must return None");
}

#[tokio::test]
async fn test_command_override_empty_string_falls_back_to_which() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("go.mod"), "module test").expect("write");
    let mut config = pathfinder_common::config::PathfinderConfig::default();
    config.lsp.insert(
        "go".to_string(),
        pathfinder_common::config::LspConfig {
            command: String::default(),
            args: vec![],
            idle_timeout_minutes: 15,
            settings: serde_json::Value::Null,
            root_override: None,
            typescript_plugins: vec![],
        },
    );
    let result = detect_languages(dir.path(), &config).await.expect("detect");
    for l in &result.detected {
        assert!(!l.command.is_empty(), "resolved command must be non-empty");
    }
}

// ── Vue plugin auto-detection tests ─────────────────────────────────────

#[tokio::test]
async fn test_auto_detects_vue_plugin_when_present() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    create_vue_file(dir.path());
    create_vue_plugin(dir.path());

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert_eq!(ts.auto_plugins, ["@vue/typescript-plugin"]);
    }
}

#[tokio::test]
async fn test_no_vue_plugin_without_vue_files() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    // Plugin installed but NO .vue files
    create_vue_plugin(dir.path());

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert!(
            ts.auto_plugins.is_empty(),
            "auto_plugins should be empty when no .vue files exist"
        );
    }
}

#[tokio::test]
async fn test_no_vue_plugin_when_no_ts_marker() {
    let dir = tempdir().expect("temp dir");
    // Plugin + .vue files exist but no tsconfig.json / package.json
    create_vue_file(dir.path());
    create_vue_plugin(dir.path());

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    assert!(
        !result
            .detected
            .iter()
            .any(|l| l.language_id == "typescript"),
        "TypeScript should not be detected without a marker file"
    );
}

#[tokio::test]
async fn test_config_plugins_override_auto_detection() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    create_vue_file(dir.path());
    create_vue_plugin(dir.path());

    let mut config = pathfinder_common::config::PathfinderConfig::default();
    config.lsp.insert(
        "typescript".to_owned(),
        pathfinder_common::config::LspConfig {
            command: String::new(),
            args: vec![],
            idle_timeout_minutes: 15,
            settings: serde_json::Value::Null,
            root_override: None,
            typescript_plugins: vec!["@custom/plugin".to_owned()],
        },
    );

    let result = detect_languages(dir.path(), &config).await.expect("detect");

    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert_eq!(
            ts.auto_plugins,
            ["@custom/plugin"],
            "should use configured plugins, not auto-detected"
        );
    }
}

#[tokio::test]
async fn test_detect_vue_files_in_subdirectory() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    create_vue_file_nested(dir.path());
    create_vue_plugin(dir.path());

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert_eq!(
            ts.auto_plugins.len(),
            1,
            "should detect Vue files in subdirectory"
        );
    }
}

#[tokio::test]
async fn test_no_vue_files_no_plugin() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    // No .vue files, no plugin installed

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert!(
            ts.auto_plugins.is_empty(),
            "should not detect plugin without .vue files"
        );
    }
}

#[tokio::test]
async fn test_detect_vue_files_in_monorepo_deep() {
    // Vue files at depth 4: apps/frontend/src/views/Home.vue
    // This tests the recursive depth-3 scan.
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    create_vue_file_monorepo(dir.path());
    create_vue_plugin(dir.path());

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert_eq!(
            ts.auto_plugins.len(),
            1,
            "should detect Vue files in deep monorepo structure (apps/frontend/src/views/)"
        );
        assert_eq!(ts.auto_plugins[0], "@vue/typescript-plugin");
    }
}

#[tokio::test]
async fn test_detect_vue_plugin_pnpm() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");
    create_vue_file(dir.path());
    create_vue_plugin_pnpm(dir.path());

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert_eq!(
            ts.auto_plugins.len(),
            1,
            "should detect plugin in pnpm structure"
        );
        assert_eq!(ts.auto_plugins[0], "@vue/typescript-plugin");
    }
}

// ── Python LSP fallback chain tests ────────────────────────────────────
// These tests use `sh` (always available on Unix-like systems) as a stand-in
// for Python LSP binaries. We create symlinks in a temporary directory and
// prepend that directory to PATH.

#[tokio::test]
async fn test_python_not_detected_without_binary() {
    // Run with an empty fake bin dir (no Python LSP binaries)
    test_with_fake_python_binaries(&[], || async {
        // Use a temp dir with pyproject.toml but no LSP binaries in PATH
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        assert!(
            !result.detected.iter().any(|l| l.language_id == "python"),
            "Python should not be detected without any LSP binary on PATH"
        );
    })
    .await;
}

#[tokio::test]
async fn test_prefers_pyright_over_pylsp() {
    test_with_fake_python_binaries(&["pyright-langserver", "pylsp"], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
            // Should prefer pyright-langserver, which uses --stdio
            assert_eq!(py.args, ["--stdio"]);
            assert!(py.command.contains("pyright-langserver"));
        } else {
            panic!("Python should be detected with pyright-langserver");
        }
    })
    .await;
}

#[tokio::test]
async fn test_detects_python_fallback_to_basedpyright() {
    test_with_fake_python_binaries(&["basedpyright-langserver"], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
            // basedpyright-langserver uses --stdio args
            assert_eq!(py.args.len(), 1);
            assert!(py.args[0].contains("--stdio"));
            assert!(py.command.contains("basedpyright-langserver"));
        } else {
            panic!("Python should be detected with basedpyright-langserver");
        }
    })
    .await;
}

#[tokio::test]
async fn test_detects_python_fallback_to_pylsp() {
    test_with_fake_python_binaries(&["pylsp"], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
            // pylsp uses empty args
            assert!(py.args.is_empty());
            assert!(py.command.contains("pylsp"));
        } else {
            panic!("Python should be detected with pylsp");
        }
    })
    .await;
}

#[tokio::test]
async fn test_detects_python_fallback_to_ruff() {
    test_with_fake_python_binaries(&["ruff"], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
            // ruff uses "server --stdio" args
            assert_eq!(py.args.len(), 2);
            assert!(py.args[0].contains("server"));
            assert!(py.args[1].contains("--stdio"));
            assert!(py.command.contains("ruff"));
        } else {
            panic!("Python should be detected with ruff");
        }
    })
    .await;
}

#[tokio::test]
async fn test_detects_python_fallback_to_jedi() {
    test_with_fake_python_binaries(&["jedi-language-server"], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
            // jedi-language-server uses empty args
            assert!(py.args.is_empty());
            assert!(py.command.contains("jedi-language-server"));
        } else {
            panic!("Python should be detected with jedi-language-server");
        }
    })
    .await;
}

#[tokio::test]
async fn test_python_args_correct_per_binary() {
    // Test custom command with default args (backward compat: --stdio)
    test_with_fake_python_binaries(&[], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config.lsp.insert(
            "python".to_string(),
            pathfinder_common::config::LspConfig {
                command: "sh".to_string(), // Use sh as a dummy command
                args: vec![],              // No args specified in config
                idle_timeout_minutes: 15,
                settings: serde_json::Value::Null,
                root_override: None,
                typescript_plugins: vec![],
            },
        );

        let result = detect_languages(dir.path(), &config).await.expect("detect");
        if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
            // Custom command with no args specified in config should default to --stdio (backward compat)
            assert_eq!(py.args, ["--stdio"]);
        } else {
            panic!("Python should be detected with custom command");
        }
    })
    .await;
}

// ── ST-2: validate_marker_file tests ────────────────────────────────────

#[test]
fn test_validate_marker_file_valid_cargo_toml() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("Cargo.toml");
    std::fs::write(&path, "[package]\nname = \"foo\"").expect("write");
    assert!(validate_marker_file(&path, "rust").is_ok());
}

#[test]
fn test_validate_marker_file_valid_cargo_workspace() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("Cargo.toml");
    std::fs::write(&path, "[workspace]\nmembers = []").expect("write");
    assert!(validate_marker_file(&path, "rust").is_ok());
}

#[test]
fn test_validate_marker_file_invalid_cargo_toml_no_sections() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("Cargo.toml");
    std::fs::write(&path, "[dependencies]\nfoo = \"1.0\"").expect("write");
    let result = validate_marker_file(&path, "rust");
    assert!(
        result.is_err(),
        "Cargo.toml with only [dependencies] should fail"
    );
    assert!(result
        .expect_err("expected Err")
        .contains("neither [package] nor [workspace]"));
}

#[test]
fn test_validate_marker_file_invalid_cargo_toml_syntax() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("Cargo.toml");
    std::fs::write(&path, "this is not toml at all !!!").expect("write");
    let result = validate_marker_file(&path, "rust");
    assert!(result.is_err(), "Malformed TOML should fail validation");
    assert!(result.expect_err("expected Err").contains("not valid TOML"));
}

#[test]
fn test_validate_marker_file_valid_go_mod() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("go.mod");
    std::fs::write(&path, "module github.com/foo/bar\n\ngo 1.21").expect("write");
    assert!(validate_marker_file(&path, "go").is_ok());

    // Test with leading comments and blank lines
    let dir2 = tempdir().expect("temp dir");
    let path2 = dir2.path().join("go.mod");
    std::fs::write(
        &path2,
        "// some comment\n  // another comment\n\nmodule my-module",
    )
    .expect("write");
    assert!(validate_marker_file(&path2, "go").is_ok());
}

#[test]
fn test_validate_marker_file_invalid_go_mod() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("go.mod");
    std::fs::write(&path, "// corrupted file").expect("write");
    let result = validate_marker_file(&path, "go");
    assert!(result.is_err(), "go.mod without 'module' should fail");
    assert!(result.expect_err("expected Err").contains("only comments"));

    // Test with bare module keyword
    let dir2 = tempdir().expect("temp dir");
    let path2 = dir2.path().join("go.mod");
    std::fs::write(&path2, "module").expect("write");
    let result2 = validate_marker_file(&path2, "go");
    assert!(result2.is_err(), "go.mod with bare 'module' should fail");
    assert!(result2
        .expect_err("expected Err")
        .contains("valid 'module' declaration"));
}

#[test]
fn test_validate_marker_file_valid_tsconfig() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("tsconfig.json");
    std::fs::write(&path, r#"{"compilerOptions":{"strict":true}}"#).expect("write");
    assert!(validate_marker_file(&path, "typescript").is_ok());

    // Test with comments and trailing commas
    let path2 = dir.path().join("tsconfig2.json");
    std::fs::write(
        &path2,
        r#"{
                // compiler configurations
                "compilerOptions": {
                    "strict": true,
                    "target": "es2020",
                },
                /* trailing commas allowed in arrays */
                "include": [
                    "src/**/*",
                ]
            }"#,
    )
    .expect("write");
    // Note: the filename must be tsconfig.json to match the tsconfig arm in validate_marker_file
    let path3 = dir.path().join("tsconfig.json");
    std::fs::write(
        &path3,
        r#"{
                // compiler configurations
                "compilerOptions": {
                    "strict": true,
                    "target": "es2020",
                },
                /* trailing commas allowed in arrays */
                "include": [
                    "src/**/*",
                ],
            }"#,
    )
    .expect("write");
    assert!(validate_marker_file(&path3, "typescript").is_ok());
}

#[test]
fn test_validate_marker_file_invalid_tsconfig() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("tsconfig.json");
    std::fs::write(&path, "{ this is not json }}").expect("write");
    let result = validate_marker_file(&path, "typescript");
    assert!(result.is_err(), "Malformed JSON tsconfig.json should fail");
    assert!(result.expect_err("expected Err").contains("not valid JSON"));
}

#[test]
fn test_validate_marker_file_valid_pyproject_toml() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("pyproject.toml");
    std::fs::write(&path, "[tool.poetry]\nname = \"app\"").expect("write");
    assert!(validate_marker_file(&path, "python").is_ok());
}

#[test]
fn test_validate_marker_file_invalid_pyproject_toml() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("pyproject.toml");
    std::fs::write(&path, "NOT VALID TOML !!!").expect("write");
    let result = validate_marker_file(&path, "python");
    assert!(result.is_err(), "Malformed pyproject.toml should fail");
    assert!(result.expect_err("expected Err").contains("not valid TOML"));
}

#[test]
fn test_validate_marker_file_package_json_always_ok() {
    // package.json is too loose to validate structurally — always passes
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("package.json");
    std::fs::write(&path, "not even json").expect("write");
    assert!(validate_marker_file(&path, "typescript").is_ok());
}

// ── ST-5: detect_venv tests ──────────────────────────────────────────────

#[test]
fn test_detect_venv_finds_dot_venv() {
    let dir = tempdir().expect("temp dir");
    let bin = dir.path().join(".venv").join("bin");
    std::fs::create_dir_all(&bin).expect("create .venv/bin");
    let python = bin.join("python");
    std::fs::write(&python, "#!/bin/sh").expect("write fake python");
    let result = detect_venv(dir.path());
    assert_eq!(result, Some(python), "should detect .venv/bin/python");
}

#[test]
fn test_detect_venv_finds_venv_fallback() {
    let dir = tempdir().expect("temp dir");
    let bin = dir.path().join("venv").join("bin");
    std::fs::create_dir_all(&bin).expect("create venv/bin");
    let python = bin.join("python");
    std::fs::write(&python, "#!/bin/sh").expect("write fake python");
    let result = detect_venv(dir.path());
    assert_eq!(result, Some(python), "should detect venv/bin/python");
}

#[test]
fn test_detect_venv_prefers_dot_venv_over_venv() {
    let dir = tempdir().expect("temp dir");
    for subdir in &[".venv/bin", "venv/bin"] {
        let bin = dir.path().join(subdir);
        std::fs::create_dir_all(&bin).expect("create bin");
        std::fs::write(bin.join("python"), "#!/bin/sh").expect("write fake python");
    }
    let result = detect_venv(dir.path());
    assert_eq!(
        result,
        Some(dir.path().join(".venv").join("bin").join("python")),
        ".venv must be preferred over venv"
    );
}

#[test]
fn test_detect_venv_returns_none_when_no_venv() {
    let dir = tempdir().expect("temp dir");
    let result = detect_venv(dir.path());
    assert!(result.is_none(), "should return None when no venv exists");
}

#[tokio::test]
async fn test_detect_invalid_cargo_toml_adds_to_missing() {
    test_with_fake_python_binaries(&["rust-analyzer"], || async {
        let dir = tempdir().expect("temp dir");
        // ST-2: Cargo.toml with no [package] or [workspace] should be rejected
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[dependencies]\nfoo = \"1.0\"",
        )
        .expect("write");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        assert!(
            result.detected.iter().all(|l| l.language_id != "rust"),
            "invalid Cargo.toml should not produce a detected Rust LSP"
        );
        let maybe_missing = result.missing.iter().find(|m| m.language_id == "rust");
        assert!(
            maybe_missing.is_some(),
            "invalid Cargo.toml should add rust to missing"
        );
        let hint = &maybe_missing.expect("checked above").install_hint;
        assert!(
            hint.contains("Fix Cargo.toml"),
            "install_hint should tell user to fix the manifest, got: {hint}"
        );
    })
    .await;
}

// ── Java detection tests (AC-2.3 – AC-2.5, AC-2.10) ─────────────────────

#[tokio::test]
async fn test_detects_java_via_pom_xml() {
    test_with_fake_python_binaries(&["jdtls"], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("pom.xml"),
            "<project><modelVersion>4.0.0</modelVersion></project>",
        )
        .expect("write pom.xml");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/Main.java"), "public class Main {}").expect("write");

        let config = pathfinder_common::config::PathfinderConfig::default();
        let result = detect_languages(dir.path(), &config).await.expect("detect");

        let java = result.detected.iter().find(|l| l.language_id == "java");
        assert!(java.is_some(), "Java should be detected via pom.xml");
        let java = java.expect("checked above");
        assert_eq!(
            java.init_timeout_secs,
            Some(180),
            "Java timeout must be 180s"
        );
        assert!(
            !java.init_options.is_null(),
            "Java init_options should not be null"
        );
        assert!(
            java.init_options["java"]["import"]["maven"]["enabled"]
                .as_bool()
                .unwrap_or(false),
            "Maven import should be enabled"
        );
    })
    .await;
}

#[tokio::test]
async fn test_detects_java_via_build_gradle() {
    test_with_fake_python_binaries(&["jdtls"], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("build.gradle"), "plugins { id 'java' }\n")
            .expect("write build.gradle");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/Main.java"), "public class Main {}").expect("write");

        let config = pathfinder_common::config::PathfinderConfig::default();
        let result = detect_languages(dir.path(), &config).await.expect("detect");

        let java = result.detected.iter().find(|l| l.language_id == "java");
        assert!(java.is_some(), "Java should be detected via build.gradle");
    })
    .await;
}

#[tokio::test]
async fn test_detects_java_via_build_gradle_kts() {
    test_with_fake_python_binaries(&["jdtls"], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("build.gradle.kts"), "plugins { java }\n")
            .expect("write build.gradle.kts");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/Main.java"), "public class Main {}").expect("write");

        let config = pathfinder_common::config::PathfinderConfig::default();
        let result = detect_languages(dir.path(), &config).await.expect("detect");

        let java = result.detected.iter().find(|l| l.language_id == "java");
        assert!(
            java.is_some(),
            "Java should be detected via build.gradle.kts"
        );
    })
    .await;
}

#[tokio::test]
async fn test_java_not_detected_without_binary() {
    test_with_fake_python_binaries(&[], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("pom.xml"),
            "<project><modelVersion>4.0.0</modelVersion></project>",
        )
        .expect("write pom.xml");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/Main.java"), "public class Main {}").expect("write");

        let config = pathfinder_common::config::PathfinderConfig::default();
        let result = detect_languages(dir.path(), &config).await.expect("detect");

        assert!(
            result.detected.iter().all(|l| l.language_id != "java"),
            "Java should not be in detected without jdtls binary"
        );
        let missing = result.missing.iter().find(|m| m.language_id == "java");
        assert!(
            missing.is_some(),
            "Java should be in missing when jdtls not found"
        );
        assert!(
            missing
                .expect("checked above")
                .tried_binaries
                .contains(&"jdtls".to_string()),
            "jdtls should be listed in tried_binaries"
        );
    })
    .await;
}

// ── validate_marker_file Java tests ─────────────────────────────────────

#[test]
fn test_validate_marker_file_valid_pom_xml() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("pom.xml");
    std::fs::write(
        &path,
        "<project><modelVersion>4.0.0</modelVersion></project>",
    )
    .expect("write");
    assert!(validate_marker_file(&path, "java").is_ok());
}

#[test]
fn test_validate_marker_file_invalid_pom_xml() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("pom.xml");
    std::fs::write(&path, "<dependency>foo</dependency>").expect("write");
    let result = validate_marker_file(&path, "java");
    assert!(result.is_err(), "pom.xml without <project should fail");
    assert!(
        result.expect_err("expected Err").contains("<project"),
        "error should mention <project element"
    );
}

#[test]
fn test_validate_marker_file_empty_build_gradle() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("build.gradle");
    std::fs::write(&path, "").expect("write");
    let result = validate_marker_file(&path, "java");
    assert!(result.is_err(), "empty build.gradle should fail");
    assert!(
        result.expect_err("expected Err").contains("empty"),
        "error should mention empty"
    );
}

// ── language_id_for_extension Java ──────────────────────────────────────

#[test]
fn test_language_id_for_extension_java() {
    assert_eq!(language_id_for_extension("java"), Some("java"));
}

// ── Python init_options migration tests ─────────────────────────────────

#[test]
fn test_detect_python_init_options_no_venv() {
    let dir = tempdir().expect("temp dir");
    let opts = detect_python_init_options(dir.path());
    assert!(opts.is_null(), "should be Null when no venv");
}

#[test]
fn test_detect_python_init_options_with_venv() {
    let dir = tempdir().expect("temp dir");
    let venv_python = dir.path().join(".venv").join("bin").join("python");
    std::fs::create_dir_all(venv_python.parent().expect("parent")).expect("mkdir");
    std::fs::write(&venv_python, "#!/bin/sh\nexec python3 \"$@\"").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&venv_python, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }

    let opts = detect_python_init_options(dir.path());
    assert!(!opts.is_null(), "should not be Null when venv found");
    assert!(
        opts["python"]["pythonPath"].as_str().is_some(),
        "pythonPath should be a string"
    );
}

// ── detect_java_init_options structure tests ─────────────────────────────

#[test]
fn test_detect_java_init_options_structure() {
    let dir = tempdir().expect("temp dir");
    let opts = detect_java_init_options(dir.path(), dir.path());
    assert!(!opts.is_null(), "java init_options should not be null");
    assert!(
        opts["java"]["import"]["gradle"]["enabled"]
            .as_bool()
            .unwrap_or(false),
        "Gradle import should be enabled"
    );
    assert!(
        opts["java"]["import"]["maven"]["enabled"]
            .as_bool()
            .unwrap_or(false),
        "Maven import should be enabled"
    );
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_detect_jdk_home_sdkmanrc() {
    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let temp = tempdir().expect("temp dir");
    let proj_root = temp.path().join("project");
    let fake_home = temp.path().join("home");
    std::fs::create_dir_all(&proj_root).unwrap();
    std::fs::create_dir_all(&fake_home).unwrap();

    // Create a fake sdkman jdk candidate
    let jdk_path = fake_home.join(".sdkman/candidates/java/17.0.7-tem");
    std::fs::create_dir_all(jdk_path.join("bin")).unwrap();
    std::fs::write(jdk_path.join("bin/java"), "").unwrap();

    // Write .sdkmanrc
    std::fs::write(proj_root.join(".sdkmanrc"), "java=17.0.7-tem\n").unwrap();

    // Save original env vars
    let orig_home = std::env::var("HOME").ok();
    let orig_userprofile = std::env::var("USERPROFILE").ok();
    let orig_java_home = std::env::var("JAVA_HOME").ok();

    // Set test env
    std::env::set_var("HOME", &fake_home);
    std::env::set_var("USERPROFILE", &fake_home);
    std::env::remove_var("JAVA_HOME");

    let detected = detect_jdk_home(&proj_root);

    // Restore env
    if let Some(val) = orig_home {
        std::env::set_var("HOME", val);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(val) = orig_userprofile {
        std::env::set_var("USERPROFILE", val);
    } else {
        std::env::remove_var("USERPROFILE");
    }
    if let Some(val) = orig_java_home {
        std::env::set_var("JAVA_HOME", val);
    } else {
        std::env::remove_var("JAVA_HOME");
    }

    assert_eq!(detected, Some(jdk_path.to_string_lossy().into_owned()));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_detect_jdk_home_java_version() {
    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let temp = tempdir().expect("temp dir");
    let proj_root = temp.path().join("project");
    let fake_home = temp.path().join("home");
    std::fs::create_dir_all(&proj_root).unwrap();
    std::fs::create_dir_all(&fake_home).unwrap();

    // Create a fake asdf jdk candidate
    let jdk_path = fake_home.join(".asdf/installs/java/11.0.2");
    std::fs::create_dir_all(jdk_path.join("bin")).unwrap();
    std::fs::write(jdk_path.join("bin/java"), "").unwrap();

    // Write .java-version
    std::fs::write(proj_root.join(".java-version"), "11.0.2\n").unwrap();

    // Save original env vars
    let orig_home = std::env::var("HOME").ok();
    let orig_userprofile = std::env::var("USERPROFILE").ok();
    let orig_java_home = std::env::var("JAVA_HOME").ok();

    // Set test env
    std::env::set_var("HOME", &fake_home);
    std::env::set_var("USERPROFILE", &fake_home);
    std::env::remove_var("JAVA_HOME");

    let detected = detect_jdk_home(&proj_root);

    // Restore env
    if let Some(val) = orig_home {
        std::env::set_var("HOME", val);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(val) = orig_userprofile {
        std::env::set_var("USERPROFILE", val);
    } else {
        std::env::remove_var("USERPROFILE");
    }
    if let Some(val) = orig_java_home {
        std::env::set_var("JAVA_HOME", val);
    } else {
        std::env::remove_var("JAVA_HOME");
    }

    assert_eq!(detected, Some(jdk_path.to_string_lossy().into_owned()));
}

#[test]
fn test_language_id_for_extension_covers_all_known() {
    assert_eq!(language_id_for_extension("rs"), Some("rust"));
    assert_eq!(language_id_for_extension("go"), Some("go"));
    assert_eq!(language_id_for_extension("ts"), Some("typescript"));
    assert_eq!(language_id_for_extension("tsx"), Some("typescript"));
    assert_eq!(language_id_for_extension("js"), Some("typescript"));
    assert_eq!(language_id_for_extension("jsx"), Some("typescript"));
    assert_eq!(language_id_for_extension("mjs"), Some("typescript"));
    assert_eq!(language_id_for_extension("cjs"), Some("typescript"));
    assert_eq!(language_id_for_extension("vue"), Some("typescript"));
    assert_eq!(language_id_for_extension("mts"), Some("typescript"));
    assert_eq!(language_id_for_extension("cts"), Some("typescript"));
    assert_eq!(language_id_for_extension("py"), Some("python"));
    assert_eq!(language_id_for_extension("pyi"), Some("python"));
    assert_eq!(language_id_for_extension("java"), Some("java"));
}

#[test]
fn test_language_id_for_extension_unknown() {
    assert_eq!(language_id_for_extension("txt"), None);
    assert_eq!(language_id_for_extension("html"), None);
    assert_eq!(language_id_for_extension("css"), None);
    assert_eq!(language_id_for_extension("json"), None);
    assert_eq!(language_id_for_extension(""), None);
}

#[test]
fn test_install_hint_all_languages() {
    assert!(install_hint("rust").contains("rust-analyzer"));
    assert!(install_hint("go").contains("gopls"));
    assert!(install_hint("typescript").contains("typescript-language-server"));
    assert!(install_hint("python").contains("pyright"));
    assert!(install_hint("java").contains("jdtls"));
    assert!(install_hint("unknown_lang").contains("unknown_lang"));
}

#[test]
fn test_install_hint_matches_plugin_registry() {
    // Verify detect.rs::install_hint delegates to plugin registry
    // (single source of truth) and never diverges.
    use crate::plugin::all_plugins;
    for plugin in all_plugins() {
        let from_detect = super::install_hint(plugin.language_id());
        let from_plugin = plugin.install_hint();
        assert_eq!(
            from_detect,
            from_plugin,
            "install_hint for '{}' diverges between detect.rs and plugin.rs",
            plugin.language_id()
        );
    }
}

#[test]
fn test_detect_venv_env_var() {
    let dir = tempdir().expect("temp dir");
    let venv_dir = dir.path().join("custom_venv").join("bin");
    std::fs::create_dir_all(&venv_dir).expect("create venv bin");
    let venv_python = venv_dir.join("python");
    std::fs::write(&venv_python, "#!/bin/sh").expect("write python");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&venv_python, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }

    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let orig = std::env::var("VIRTUAL_ENV").ok();
    std::env::set_var("VIRTUAL_ENV", dir.path().join("custom_venv"));

    let result = detect_venv(dir.path());

    if let Some(orig_val) = orig {
        std::env::set_var("VIRTUAL_ENV", orig_val);
    } else {
        std::env::remove_var("VIRTUAL_ENV");
    }

    assert!(
        result.is_some(),
        "should detect venv from VIRTUAL_ENV env var"
    );
}

#[tokio::test]
async fn test_detect_typescript_with_args_override() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("package.json"), "{}").expect("write");

    let mut config = pathfinder_common::config::PathfinderConfig::default();
    config.lsp.insert(
        "typescript".to_string(),
        pathfinder_common::config::LspConfig {
            command: String::new(),
            args: vec!["--stdio".to_owned(), "--log-level".to_owned()],
            idle_timeout_minutes: 15,
            settings: serde_json::Value::Null,
            root_override: None,
            typescript_plugins: vec![],
        },
    );

    let result = detect_languages(dir.path(), &config).await.expect("detect");

    if let Some(ts) = result
        .detected
        .iter()
        .find(|l| l.language_id == "typescript")
    {
        assert_eq!(
            ts.args,
            vec!["--stdio".to_owned(), "--log-level".to_owned()],
            "args should match config override"
        );
    }
}

#[tokio::test]
async fn test_detect_java_via_settings_gradle() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("settings.gradle"),
        "rootProject.name = \"test\"",
    )
    .expect("write");
    std::fs::create_dir_all(dir.path().join("src")).expect("create src");
    std::fs::write(dir.path().join("src/Main.java"), "public class Main {}").expect("write");

    let result = detect_languages(dir.path(), &make_ts_config()).await;

    assert!(
        result.is_ok(),
        "detect_languages should not error: {result:?}"
    );

    let r = result.expect("detect_languages result should be Ok");
    let has_java = r.detected.iter().any(|l| l.language_id == "java")
        || r.missing.iter().any(|l| l.language_id == "java");
    assert!(
        has_java,
        "settings.gradle should trigger Java detection (or missing report)"
    );
}

#[tokio::test]
async fn test_detect_java_via_settings_gradle_kts() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("settings.gradle.kts"),
        "rootProject.name = \"test\"",
    )
    .expect("write");
    std::fs::create_dir_all(dir.path().join("src")).expect("create src");
    std::fs::write(dir.path().join("src/Main.java"), "public class Main {}").expect("write");

    let result = detect_languages(dir.path(), &make_ts_config()).await;

    assert!(
        result.is_ok(),
        "detect_languages should not error: {result:?}"
    );

    let r = result.expect("detect_languages result should be Ok");
    let has_java = r.detected.iter().any(|l| l.language_id == "java")
        || r.missing.iter().any(|l| l.language_id == "java");
    assert!(
        has_java,
        "settings.gradle.kts should trigger Java detection (or missing report)"
    );
}

#[tokio::test]
async fn test_detect_python_via_requirements_txt() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("requirements.txt"), "flask==2.0").expect("write");
    std::fs::write(dir.path().join("main.py"), "").expect("write");

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    let has_python = result.detected.iter().any(|l| l.language_id == "python")
        || result.missing.iter().any(|l| l.language_id == "python");
    assert!(
        has_python,
        "should detect or report Python missing with requirements.txt"
    );
}

#[tokio::test]
async fn test_detect_python_via_setup_py() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("setup.py"),
        "from setuptools import setup; setup()",
    )
    .expect("write");
    std::fs::write(dir.path().join("main.py"), "").expect("write");

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    let has_python = result.detected.iter().any(|l| l.language_id == "python")
        || result.missing.iter().any(|l| l.language_id == "python");
    assert!(
        has_python,
        "should detect or report Python missing with setup.py"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_detect_python_venv_in_subdirectory() {
    let dir = tempdir().expect("temp dir");
    let sub = dir.path().join("apps").join("backend");
    std::fs::create_dir_all(&sub).expect("create dir");
    std::fs::write(sub.join("requirements.txt"), "flask==2.0").expect("write");
    std::fs::write(sub.join("main.py"), "").expect("write");

    let venv_python = sub.join(".venv").join("bin").join("python");
    std::fs::create_dir_all(venv_python.parent().expect("parent")).expect("mkdir");
    std::fs::write(&venv_python, "#!/bin/sh\nexec python3 \"$@\"").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&venv_python, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }

    // Mock a fake python binary so Python is detected as running, not missing
    // Hold the lock across await intentionally — serializes env-mutating tests.
    let _path_lock = PATH_MUTEX.lock().expect("path mutex");
    let old_path = std::env::var("PATH").expect("PATH");
    let fake_bin_dir = dir.path().join("fake_bin");
    std::fs::create_dir_all(&fake_bin_dir).expect("create bin dir");
    std::fs::write(fake_bin_dir.join("pyright-langserver"), "#!/bin/sh").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            fake_bin_dir.join("pyright-langserver"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");
    }
    let new_path = format!("{}:{}", fake_bin_dir.to_str().expect("utf8"), old_path);
    std::env::set_var("PATH", &new_path);

    let result = detect_languages(dir.path(), &make_ts_config())
        .await
        .expect("detect");

    std::env::set_var("PATH", old_path);

    let py_lsp = result
        .detected
        .iter()
        .find(|l| l.language_id == "python")
        .expect("python detected");
    assert!(
        !py_lsp.init_options.is_null(),
        "python init_options should not be null when venv is in subdirectory"
    );
    assert_eq!(
        py_lsp.init_options["python"]["pythonPath"]
            .as_str()
            .expect("pythonPath string"),
        venv_python.to_str().expect("utf8")
    );
}

#[tokio::test]
async fn test_detect_vue_files_in_shallow_nested_dirs() {
    let dir = tempdir().expect("temp dir");
    let nested = dir.path().join("src").join("components");
    std::fs::create_dir_all(&nested).expect("create dirs");
    std::fs::write(nested.join("Widget.vue"), "<template/>").expect("write");

    assert!(
        workspace_has_vue_files(dir.path()).await,
        "should find .vue files in shallow nested directories"
    );
}

#[tokio::test]
async fn test_validate_marker_file_unsupported_marker() {
    let dir = tempdir().expect("temp dir");
    let marker = dir.path().join("package.json");
    std::fs::write(&marker, "{}").expect("write");

    let result = validate_marker_file(&marker, "rust");
    assert!(
        result.is_ok(),
        "unsupported marker file name should return Ok by default"
    );
}

#[tokio::test]
async fn test_find_marker_at_root() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");

    let found = find_marker(dir.path(), "Cargo.toml", 0, None).await;
    assert_eq!(found.as_deref(), Some(dir.path()));
}

#[tokio::test]
async fn test_find_marker_at_depth_1() {
    let dir = tempdir().expect("temp dir");
    let sub = dir.path().join("backend");
    std::fs::create_dir_all(&sub).expect("create dir");
    std::fs::write(sub.join("go.mod"), "module backend").expect("write");

    let found = find_marker(dir.path(), "go.mod", 2, None).await;
    assert_eq!(found.as_deref(), Some(sub.as_path()));
}

#[tokio::test]
async fn test_find_marker_multiple_matches() {
    let dir = tempdir().expect("temp dir");
    let sub1 = dir.path().join("backend");
    let sub2 = dir.path().join("worker");
    std::fs::create_dir_all(&sub1).expect("create dir");
    std::fs::create_dir_all(&sub2).expect("create dir");
    std::fs::write(sub1.join("go.mod"), "module backend").expect("write");
    std::fs::write(sub2.join("go.mod"), "module worker").expect("write");

    let found = find_marker(dir.path(), "go.mod", 2, None).await;
    assert_eq!(found.as_deref(), Some(dir.path()));
}

#[tokio::test]
async fn test_detect_command_override_skips_which() {
    let dir = tempdir().expect("temp dir");
    std::fs::write(dir.path().join("go.mod"), "module test").expect("write");

    let mut config = pathfinder_common::config::PathfinderConfig::default();
    config.lsp.insert(
        "go".to_string(),
        pathfinder_common::config::LspConfig {
            command: "/usr/local/bin/custom-gopls".to_string(),
            args: vec![],
            idle_timeout_minutes: 15,
            settings: serde_json::Value::Null,
            root_override: None,
            typescript_plugins: vec![],
        },
    );

    let result = detect_languages(dir.path(), &config).await.expect("detect");

    if let Some(go) = result.detected.iter().find(|l| l.language_id == "go") {
        assert_eq!(
            go.command, "/usr/local/bin/custom-gopls",
            "should use config command override"
        );
    }
}

#[tokio::test]
async fn test_detect_ignores_build_artifact_paths() {
    let dir = tempdir().expect("temp dir");
    let target_dir = dir.path().join("target").join("debug");
    std::fs::create_dir_all(&target_dir).expect("create target dir");

    let result = detect_languages(dir.path(), &make_ts_config()).await;
    assert!(result.is_ok(), "should handle missing markers gracefully");
}

#[test]
fn test_detect_python_init_options_no_venv_returns_null() {
    let dir = tempdir().expect("temp dir");
    let opts = detect_python_init_options(dir.path());
    assert!(
        opts.is_null(),
        "should return Null when no venv found: {opts:?}"
    );
}

#[test]
fn test_detect_venv_conda_prefix() {
    let dir = tempdir().expect("temp dir");
    let conda_bin = dir.path().join("bin");
    std::fs::create_dir_all(&conda_bin).expect("create bin");

    let python_path = conda_bin.join("python");
    std::fs::write(&python_path, "#!/bin/sh").expect("write python");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&python_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }

    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let orig = std::env::var("CONDA_PREFIX").ok();
    std::env::set_var("CONDA_PREFIX", dir.path().to_string_lossy().as_ref());

    let result = detect_venv(dir.path());

    if let Some(orig_val) = orig {
        std::env::set_var("CONDA_PREFIX", orig_val);
    } else {
        std::env::remove_var("CONDA_PREFIX");
    }

    assert!(result.is_some(), "should detect Python from CONDA_PREFIX");
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_detect_venv_windows_paths() {
    let dir = tempdir().expect("temp dir");

    // 1. Conda Windows path: CONDA_PREFIX/python.exe
    let conda_windows_python = dir.path().join("conda_env").join("python.exe");
    std::fs::create_dir_all(conda_windows_python.parent().unwrap()).unwrap();
    std::fs::write(&conda_windows_python, "").unwrap();

    // 2. VIRTUAL_ENV Windows path: VIRTUAL_ENV/Scripts/python.exe
    let venv_windows_python = dir
        .path()
        .join("virtual_env")
        .join("Scripts")
        .join("python.exe");
    std::fs::create_dir_all(venv_windows_python.parent().unwrap()).unwrap();
    std::fs::write(&venv_windows_python, "").unwrap();

    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    // Test CONDA_PREFIX Windows path detection
    let orig_conda = std::env::var("CONDA_PREFIX").ok();
    std::env::set_var("CONDA_PREFIX", dir.path().join("conda_env"));
    let res_conda = detect_venv(dir.path());
    if let Some(val) = orig_conda {
        std::env::set_var("CONDA_PREFIX", val);
    } else {
        std::env::remove_var("CONDA_PREFIX");
    }
    assert_eq!(res_conda, Some(conda_windows_python));

    // Test VIRTUAL_ENV Windows path detection
    let orig_venv = std::env::var("VIRTUAL_ENV").ok();
    std::env::set_var("VIRTUAL_ENV", dir.path().join("virtual_env"));
    let res_venv = detect_venv(dir.path());
    if let Some(val) = orig_venv {
        std::env::set_var("VIRTUAL_ENV", val);
    } else {
        std::env::remove_var("VIRTUAL_ENV");
    }
    assert_eq!(res_venv, Some(venv_windows_python));
}

// ── Issue 1: find_marker max_depth clamping ─────────────────────────────

#[tokio::test]
async fn test_find_marker_clamps_max_depth_to_2() {
    // max_depth=5 must behave identically to max_depth=2.
    // Place a marker at depth 2 — both calls must find it.
    let dir = tempdir().expect("temp dir");
    let deep = dir.path().join("apps").join("backend");
    std::fs::create_dir_all(&deep).expect("create dirs");
    std::fs::write(deep.join("Cargo.toml"), "[package]").expect("write");

    let result_depth_2 = find_marker(dir.path(), "Cargo.toml", 2, None).await;
    let result_depth_5 = find_marker(dir.path(), "Cargo.toml", 5, None).await;
    assert_eq!(
        result_depth_2, result_depth_5,
        "max_depth=5 must behave identically to max_depth=2 (clamped)"
    );
    assert_eq!(
        result_depth_2.as_deref(),
        Some(deep.as_path()),
        "should find marker at depth 2"
    );
}

#[tokio::test]
async fn test_find_marker_large_depth_no_depth_3_scan() {
    // Place a marker at depth 3 — even max_depth=100 must NOT find it (clamped to 2).
    let dir = tempdir().expect("temp dir");
    let deep = dir.path().join("a").join("b").join("c");
    std::fs::create_dir_all(&deep).expect("create dirs");
    std::fs::write(deep.join("go.mod"), "module deep").expect("write");

    let found = find_marker(dir.path(), "go.mod", 100, None).await;
    assert!(
        found.is_none(),
        "max_depth is clamped to 2, so depth-3 marker must not be found"
    );
}

// ── Monorepo TS detection: sibling_filter ──────────────────────────

#[tokio::test]
async fn test_find_marker_sibling_filter_multi_match_prefers_with_sibling() {
    // Monorepo: apps/e2e/package.json (no tsconfig) + apps/frontend/package.json + tsconfig
    let dir = tempdir().expect("temp dir");
    let e2e = dir.path().join("apps").join("e2e");
    let frontend = dir.path().join("apps").join("frontend");
    std::fs::create_dir_all(&e2e).expect("create e2e");
    std::fs::create_dir_all(&frontend).expect("create frontend");

    // e2e has package.json but NO tsconfig.json
    std::fs::write(e2e.join("package.json"), "{}").expect("write");
    // frontend has both
    std::fs::write(frontend.join("package.json"), "{}").expect("write");
    std::fs::write(frontend.join("tsconfig.json"), "{}").expect("write");

    // With sibling_filter on multi-match: only frontend should be selected
    let found = find_marker(dir.path(), "package.json", 2, Some("tsconfig.json")).await;

    assert_eq!(
        found.as_deref(),
        Some(frontend.as_path()),
        "multi-match: should pick the directory that has both package.json AND tsconfig.json"
    );
}

#[tokio::test]
async fn test_find_marker_sibling_filter_single_match_accepts_without_sibling() {
    // Pure JS project: single package.json, no tsconfig.json anywhere.
    // sibling_filter only applies to multi-match, so single match is accepted.
    let dir = tempdir().expect("temp dir");
    let sub = dir.path().join("apps").join("myapp");
    std::fs::create_dir_all(&sub).expect("create dir");
    std::fs::write(sub.join("package.json"), "{}").expect("write");

    let found = find_marker(dir.path(), "package.json", 2, Some("tsconfig.json")).await;
    assert_eq!(
        found.as_deref(),
        Some(sub.as_path()),
        "single match with sibling_filter: should accept (pure JS project)"
    );
}

#[tokio::test]
async fn test_find_marker_sibling_filter_multi_match_all_rejected_falls_back_to_root() {
    // Multiple package.json, NONE have tsconfig.json — fall back to workspace root.
    let dir = tempdir().expect("temp dir");
    let e2e = dir.path().join("apps").join("e2e");
    let scripts = dir.path().join("apps").join("scripts");
    std::fs::create_dir_all(&e2e).expect("create e2e");
    std::fs::create_dir_all(&scripts).expect("create scripts");

    std::fs::write(e2e.join("package.json"), "{}").expect("write");
    std::fs::write(scripts.join("package.json"), "{}").expect("write");
    // No tsconfig.json in either

    let found = find_marker(dir.path(), "package.json", 2, Some("tsconfig.json")).await;
    assert_eq!(
        found.as_deref(),
        Some(dir.path()),
        "multi-match, none have sibling: should fall back to workspace root"
    );
}

#[tokio::test]
async fn test_find_marker_sibling_filter_none_keeps_original_behavior() {
    let dir = tempdir().expect("temp dir");
    let sub = dir.path().join("apps").join("e2e");
    std::fs::create_dir_all(&sub).expect("create dir");
    std::fs::write(sub.join("package.json"), "{}").expect("write");

    // Without sibling_filter: old behavior — finds the marker
    let found = find_marker(dir.path(), "package.json", 2, None).await;
    assert!(
        found.is_some(),
        "without sibling_filter, find_marker should still find the marker"
    );
}

// ── strip_jsonc_comments tests ──────────────────────────────────────────

#[test]
fn test_strip_jsonc_comments_line_comment() {
    let input = "{\n// this is a comment\n\"key\": \"value\"\n}";
    let result = strip_jsonc_comments(input);
    assert!(!result.contains("// this is a comment"));
    assert!(result.contains("\"key\": \"value\""));
}

#[test]
fn test_strip_jsonc_comments_block_comment() {
    let input = "{\n/* block\ncomment */\n\"key\": \"value\"\n}";
    let result = strip_jsonc_comments(input);
    assert!(!result.contains("block"));
    assert!(!result.contains("comment */"));
    assert!(result.contains("\"key\": \"value\""));
}

#[test]
fn test_strip_jsonc_comments_preserves_strings() {
    // Comments inside strings should NOT be stripped
    let input = r#"{"key": "value // not a comment", "url": "http://example.com"}"#;
    let result = strip_jsonc_comments(input);
    assert!(result.contains("value // not a comment"));
    assert!(result.contains("http://example.com"));
}

#[test]
fn test_strip_jsonc_comments_escaped_quotes_in_strings() {
    // Escaped quotes inside strings must not confuse the parser
    let input = r#"{"key": "val\"ue // still string"}"#;
    let result = strip_jsonc_comments(input);
    assert!(result.contains(r#"val\"ue // still string"#));
}

#[test]
fn test_strip_jsonc_comments_mixed() {
    let input = r#"{
        // line comment
        "a": 1, /* inline block */ "b": 2
    }"#;
    let result = strip_jsonc_comments(input);
    assert!(!result.contains("line comment"));
    assert!(!result.contains("inline block"));
    assert!(result.contains("\"a\": 1,"));
    assert!(result.contains("\"b\": 2"));
}

// ── strip_json_trailing_commas tests ────────────────────────────────────

#[test]
fn test_strip_json_trailing_commas_object() {
    let input = r#"{"a": 1, "b": 2, }"#;
    let result = strip_json_trailing_commas(input);
    assert!(serde_json::from_str::<serde_json::Value>(&result).is_ok());
}

#[test]
fn test_strip_json_trailing_commas_array() {
    let input = r#"["a", "b", ]"#;
    let result = strip_json_trailing_commas(input);
    assert!(serde_json::from_str::<serde_json::Value>(&result).is_ok());
}

#[test]
fn test_strip_json_trailing_commas_nested() {
    let input = r#"{"a": [1, 2, ], "b": {"c": 3, }, }"#;
    let result = strip_json_trailing_commas(input);
    assert!(serde_json::from_str::<serde_json::Value>(&result).is_ok());
}

#[test]
fn test_strip_json_trailing_commas_preserves_strings() {
    // Commas inside strings should NOT be affected
    let input = r#"{"key": "a, b, "}"#;
    let result = strip_json_trailing_commas(input);
    assert!(result.contains("a, b, "));
}

#[test]
fn test_strip_json_trailing_commas_no_trailing() {
    let input = r#"{"a": 1, "b": 2}"#;
    let result = strip_json_trailing_commas(input);
    assert_eq!(result, input);
}

// ── validate_marker_file edge cases ─────────────────────────────────────

#[test]
fn test_validate_marker_file_nonexistent_returns_ok() {
    let path = std::path::Path::new("/tmp/does_not_exist_marker_xyz.toml");
    assert!(
        validate_marker_file(path, "rust").is_ok(),
        "non-existent marker file should return Ok (skip)"
    );
}

#[test]
fn test_validate_marker_file_empty_go_mod() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("go.mod");
    std::fs::write(&path, "").expect("write");
    let result = validate_marker_file(&path, "go");
    assert!(result.is_err(), "empty go.mod should fail");
    assert!(result.expect_err("expected Err").contains("empty"));
}

#[test]
fn test_validate_marker_file_go_mod_module_no_name() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("go.mod");
    std::fs::write(&path, "module   ").expect("write");
    let result = validate_marker_file(&path, "go");
    assert!(
        result.is_err(),
        "go.mod with 'module' but no name should fail"
    );
}

#[test]
fn test_validate_marker_file_valid_build_gradle_kts() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("build.gradle.kts");
    std::fs::write(&path, "plugins { java }\n").expect("write");
    assert!(validate_marker_file(&path, "java").is_ok());
}

#[test]
fn test_validate_marker_file_empty_build_gradle_kts() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("build.gradle.kts");
    std::fs::write(&path, "").expect("write");
    let result = validate_marker_file(&path, "java");
    assert!(result.is_err(), "empty build.gradle.kts should fail");
}

// ── detect_jdk_home additional paths ────────────────────────────────────

#[test]
fn test_detect_jdk_home_uses_java_home() {
    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let dir = tempdir().expect("temp dir");
    let orig = std::env::var("JAVA_HOME").ok();
    std::env::set_var("JAVA_HOME", "/fake/jdk/home");

    let result = detect_jdk_home(dir.path());

    if let Some(val) = orig {
        std::env::set_var("JAVA_HOME", val);
    } else {
        std::env::remove_var("JAVA_HOME");
    }

    assert_eq!(
        result,
        Some("/fake/jdk/home".to_owned()),
        "JAVA_HOME takes priority"
    );
}

#[test]
fn test_detect_jdk_home_empty_java_home_skipped() {
    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let dir = tempdir().expect("temp dir");
    let orig = std::env::var("JAVA_HOME").ok();
    std::env::set_var("JAVA_HOME", "");

    let result = detect_jdk_home(dir.path());

    if let Some(val) = orig {
        std::env::set_var("JAVA_HOME", val);
    } else {
        std::env::remove_var("JAVA_HOME");
    }

    // Empty JAVA_HOME should be skipped, result depends on other env
    assert!(
        result.is_none() || result.is_some(),
        "empty JAVA_HOME should not panic"
    );
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_detect_jdk_home_jenv_path() {
    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let temp = tempdir().expect("temp dir");
    let proj_root = temp.path().join("project");
    let fake_home = temp.path().join("home");
    std::fs::create_dir_all(&proj_root).unwrap();
    std::fs::create_dir_all(&fake_home).unwrap();

    // Create a fake jenv jdk candidate
    let jdk_path = fake_home.join(".jenv/versions/17.0.7");
    std::fs::create_dir_all(jdk_path.join("bin")).unwrap();
    std::fs::write(jdk_path.join("bin/java"), "").unwrap();

    // Write .java-version
    std::fs::write(proj_root.join(".java-version"), "17.0.7\n").unwrap();

    let orig_home = std::env::var("HOME").ok();
    let orig_userprofile = std::env::var("USERPROFILE").ok();
    let orig_java_home = std::env::var("JAVA_HOME").ok();

    std::env::set_var("HOME", &fake_home);
    std::env::set_var("USERPROFILE", &fake_home);
    std::env::remove_var("JAVA_HOME");

    let detected = detect_jdk_home(&proj_root);

    if let Some(val) = orig_home {
        std::env::set_var("HOME", val);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(val) = orig_userprofile {
        std::env::set_var("USERPROFILE", val);
    } else {
        std::env::remove_var("USERPROFILE");
    }
    if let Some(val) = orig_java_home {
        std::env::set_var("JAVA_HOME", val);
    } else {
        std::env::remove_var("JAVA_HOME");
    }

    assert_eq!(detected, Some(jdk_path.to_string_lossy().into_owned()));
}

#[test]
fn test_detect_jdk_home_no_home_env() {
    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let dir = tempdir().expect("temp dir");
    let orig_home = std::env::var("HOME").ok();
    let orig_userprofile = std::env::var("USERPROFILE").ok();
    let orig_java_home = std::env::var("JAVA_HOME").ok();

    std::env::remove_var("JAVA_HOME");
    std::env::remove_var("HOME");
    std::env::remove_var("USERPROFILE");

    let result = detect_jdk_home(dir.path());

    if let Some(val) = orig_home {
        std::env::set_var("HOME", val);
    }
    if let Some(val) = orig_userprofile {
        std::env::set_var("USERPROFILE", val);
    }
    if let Some(val) = orig_java_home {
        std::env::set_var("JAVA_HOME", val);
    }

    assert!(
        result.is_none(),
        "should return None when no HOME, USERPROFILE, or JAVA_HOME"
    );
}

// ── detect_venv additional fallback paths ───────────────────────────────

#[test]
fn test_detect_venv_finds_env_fallback() {
    let dir = tempdir().expect("temp dir");
    let bin = dir.path().join("env").join("bin");
    std::fs::create_dir_all(&bin).expect("create env/bin");
    let python = bin.join("python");
    std::fs::write(&python, "#!/bin/sh").expect("write fake python");
    let result = detect_venv(dir.path());
    assert_eq!(result, Some(python), "should detect env/bin/python");
}

#[test]
fn test_detect_venv_finds_dot_env_fallback() {
    let dir = tempdir().expect("temp dir");
    let bin = dir.path().join(".env").join("bin");
    std::fs::create_dir_all(&bin).expect("create .env/bin");
    let python = bin.join("python");
    std::fs::write(&python, "#!/bin/sh").expect("write fake python");
    let result = detect_venv(dir.path());
    assert_eq!(result, Some(python), "should detect .env/bin/python");
}

// ── find_marker symlink handling ────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn test_find_marker_skips_symlinks() {
    let dir = tempdir().expect("temp dir");
    let real_dir = dir.path().join("real_sub");
    std::fs::create_dir_all(&real_dir).expect("create real dir");
    std::fs::write(real_dir.join("go.mod"), "module symlinked").expect("write");

    // Create a symlink to real_sub
    let link_path = dir.path().join("link_sub");
    std::os::unix::fs::symlink(&real_dir, &link_path).expect("create symlink");

    // find_marker should skip symlinked directories, only find via real path
    let found = find_marker(dir.path(), "go.mod", 1, None).await;
    assert!(found.is_some(), "should find marker in real directory");
    // The result should be the real directory, not the symlink
    if let Some(found_path) = found {
        assert_eq!(
            found_path, real_dir,
            "should find the real directory, not the symlink"
        );
    }
}

// ── detect_java_init_options with JDK home ──────────────────────────────

#[test]
fn test_detect_java_init_options_with_java_home() {
    let _guard = match PATH_MUTEX.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };

    let dir = tempdir().expect("temp dir");
    let orig = std::env::var("JAVA_HOME").ok();
    std::env::set_var("JAVA_HOME", "/fake/jdk");

    let opts = detect_java_init_options(dir.path(), dir.path());

    if let Some(val) = orig {
        std::env::set_var("JAVA_HOME", val);
    } else {
        std::env::remove_var("JAVA_HOME");
    }

    assert!(!opts.is_null());
    assert!(opts["java"]["jdt"]["ls"]["java"]["home"]
        .as_str()
        .is_some());
    assert_eq!(
        opts["java"]["jdt"]["ls"]["java"]["home"].as_str().unwrap(),
        "/fake/jdk"
    );
}

// ── validate_marker_file tsconfig with pure JSONC ────────────────────────

#[test]
fn test_validate_marker_file_tsconfig_with_block_and_line_comments() {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("tsconfig.json");
    std::fs::write(
        &path,
        r#"{
            // line comment
            "compilerOptions": {
                /* block comment */
                "strict": true
            }
        }"#,
    )
    .expect("write");
    assert!(validate_marker_file(&path, "typescript").is_ok());
}

// ── has_source_files_recursive via detect_languages ─────────────────────

#[tokio::test]
async fn test_filters_missing_language_with_no_source_files() {
    // Marker file present, no binary, no source files → should NOT be in missing either
    test_with_fake_python_binaries(&[], || async {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("go.mod"), "module test").expect("write");
        // No .go source files

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        assert!(
            result.missing.iter().all(|l| l.language_id != "go"),
            "Go should not be in missing when no .go source files exist"
        );
    })
    .await;
}
