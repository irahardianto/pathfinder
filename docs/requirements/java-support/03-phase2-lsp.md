# Phase 2: LSP Integration (jdtls)

## Overview

Integrate Eclipse JDT Language Server (jdtls) as the Java LSP backend, enabling `get_definition`, `analyze_impact`, `call_hierarchy`, and `lsp_health` for Java projects.

## Prerequisites

- Phase 0 and Phase 1 must be merged
- JDK 21 LTS must be installed on the host
- jdtls must be installed on the host (discovered via `which jdtls` or configured in `.pathfinder.toml`)

## Pre-Implementation: jdtls Spike (MANDATORY)

Before full implementation, run a **4-hour time-boxed spike** to verify:
1. jdtls can be spawned via stdio and responds to LSP initialize
2. Determine exact launch command pattern (equinox launcher vs wrapper script)
3. Test `textDocument/definition` on a sample Maven/Gradle project
4. Document required `initializationOptions` structure
5. **Verify whether `workspace/configuration` handler is needed** — jdtls sends `workspace/configuration` requests to query settings dynamically. Without a handler, jdtls may use default settings or fail silently. The spike must determine if the current Pathfinder LSP client can safely ignore these requests or needs a no-op handler.

---

## 1. Language Plugin

**File**: `crates/pathfinder-lsp/src/plugin.rs`

### Add `JavaPlugin`

```rust
/// Java language plugin — jdtls (Eclipse JDT Language Server).
pub struct JavaPlugin;

impl LanguagePlugin for JavaPlugin {
    fn language_id(&self) -> &'static str { "java" }

    fn file_extensions(&self) -> &'static [&'static str] { &["java"] }

    fn marker_files(&self) -> &'static [&'static str] {
        &["pom.xml", "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"]
    }

    fn marker_search_depth(&self) -> u32 { 2 }  // Monorepo support

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
```

### Register in `all_plugins()`

```diff
 pub fn all_plugins() -> &'static [&'static dyn LanguagePlugin] {
-    &[&RustPlugin, &GoPlugin, &TypeScriptPlugin, &PythonPlugin]
+    &[&RustPlugin, &GoPlugin, &TypeScriptPlugin, &PythonPlugin, &JavaPlugin]
 }
```

### Update Tests

- `test_all_plugins_contains_all_four_languages` → rename to `test_all_plugins_contains_all_five_languages`
- Add `test_java_plugin_*` tests (language_id, file_extensions, marker_files, marker_search_depth, lsp_candidates, install_hint)
- Add `test_plugin_for_extension_java` → asserts `plugin_for_extension("java")` returns `"java"`

---

## 2. Language Detection

**File**: `crates/pathfinder-lsp/src/client/detect.rs`

### Add `language_id_for_extension` entry

```diff
+        "java" => Some("java"),
```

### Add `install_hint` entry

```diff
+        "java" => {
+            "Install jdtls: https://github.com/eclipse-jdtls/eclipse.jdt.ls\nRequires JDK 21+"
+                .to_string()
+        }
```

### Add Java detection block in `detect_languages()`

Add after the Python block, following the existing pattern:

```rust
// Java — pom.xml, build.gradle, build.gradle.kts (depth 2)
let (java_root, java_marker) = if get_override!("java").is_some() {
    (get_override!("java"), None)
} else if let Some(r) = find_marker(workspace_root, "pom.xml", 2).await {
    (Some(r), Some("pom.xml"))
} else if let Some(r) = find_marker(workspace_root, "build.gradle", 2).await {
    (Some(r), Some("build.gradle"))
} else if let Some(r) = find_marker(workspace_root, "build.gradle.kts", 2).await {
    (Some(r), Some("build.gradle.kts"))
} else {
    (find_marker(workspace_root, "settings.gradle", 2).await, Some("settings.gradle"))
};
```

> **CAVEAT**: `settings.gradle` / `settings.gradle.kts` can match non-Java Gradle projects (Android-only, Kotlin-only). This is an acceptable false positive — jdtls will simply find no Java sources and idle. If this becomes a problem, a future refinement can check for `.java` files in the `src/` directory.
if let Some(root) = java_root {
    let has_override = get_command_override!("java").is_some();
    let cmd = get_command_override!("java")
        .or_else(|| resolve_command("jdtls", "java"));
    if let Some(command) = cmd {
        // ST-2: validate marker before spawning
        if let Some(marker) = java_marker {
            let marker_path = root.join(marker);
            if let Err(reason) = validate_marker_file(&marker_path, "java") {
                tracing::warn!(language = "java", %reason, "ST-2: invalid manifest");
                missing.push(MissingLanguage {
                    language_id: "java".to_owned(),
                    marker_file: marker.to_string(),
                    tried_binaries: vec!["jdtls".to_string()],
                    install_hint: format!("Fix {marker}: {reason}"),
                });
            } else {
                detected.push(LanguageLsp {
                    language_id: "java".to_owned(),
                    command,
                    args: get_args!("java", vec![]),
                    root,
                    init_timeout_secs: Some(180), // jdtls is slow to start on large projects
                    auto_plugins: vec![],
                    init_options: detect_java_init_options(workspace_root),
                });
            }
        } else {
            detected.push(LanguageLsp {
                language_id: "java".to_owned(),
                command,
                args: get_args!("java", vec![]),
                root,
                init_timeout_secs: Some(180),
                auto_plugins: vec![],
                init_options: detect_java_init_options(workspace_root),
            });
        }
    } else if !has_override {
        missing.push(MissingLanguage {
            language_id: "java".to_owned(),
            marker_file: java_marker.unwrap_or("pom.xml or build.gradle").to_string(),
            tried_binaries: vec!["jdtls".to_string()],
            install_hint: install_hint("java"),
        });
    }
}
```

### Add `validate_marker_file` for Java

```rust
("java", "pom.xml") => {
    if contents.contains("<project") {
        Ok(())
    } else {
        Err("pom.xml does not contain <project element".to_owned())
    }
}
("java", "build.gradle" | "build.gradle.kts") => {
    // Gradle files are Groovy/Kotlin scripts — minimal structural validation
    if contents.is_empty() {
        Err("build.gradle is empty".to_owned())
    } else {
        Ok(())
    }
}
```

---

## 3. Generalize `LanguageLsp` for Init Options

**File**: `crates/pathfinder-lsp/src/client/detect.rs`

The current `python_path: Option<PathBuf>` field is Python-specific. Replace with a generic mechanism:

```diff
 pub struct LanguageLsp {
     pub language_id: String,
     pub command: String,
     pub args: Vec<String>,
     pub root: std::path::PathBuf,
     pub init_timeout_secs: Option<u64>,
     pub auto_plugins: Vec<String>,
-    pub python_path: Option<std::path::PathBuf>,
+    /// Language-specific initialization options passed to LSP `initialize` request.
+    /// Built by per-language detection functions (e.g., Python venv path, Java JDK config).
+    pub init_options: serde_json::Value,
 }
```

### Migrate Python usage

All places that set `python_path: None` change to `init_options: serde_json::Value::Null`.

The Python venv detection becomes:
```rust
let init_options = detect_venv(workspace_root)
    .map(|py_path| json!({
        "python": { "pythonPath": py_path.to_string_lossy().as_ref() }
    }))
    .unwrap_or(serde_json::Value::Null);
```

### Java Init Options

```rust
fn detect_java_init_options(workspace_root: &Path) -> serde_json::Value {
    let java_home = std::env::var("JAVA_HOME").ok();

    let mut settings = json!({
        "java": {
            "import": {
                "gradle": { "enabled": true },
                "maven": { "enabled": true }
            }
        }
    });

    if let Some(home) = java_home {
        settings["java"]["jdt"]["ls"]["java"]["home"] = json!(home);
    }

    settings
}
```

### Update `build_initialize_request`

**File**: `crates/pathfinder-lsp/src/client/process.rs`

Replace the `python_path`-specific branch with generic init_options:

```diff
-    python_path: Option<std::path::PathBuf>,
+    init_options: serde_json::Value,
...
-    let initialization_options = if !plugins.is_empty() {
-        json!({ "plugins": plugin_entries, "tsserver": { ... } })
-    } else if let Some(py_path) = python_path {
-        json!({ "python": { "pythonPath": py_path.to_string_lossy().as_ref() } })
-    } else {
-        json!({})
-    };
+    let mut initialization_options = if !plugins.is_empty() {
+        json!({ "plugins": plugin_entries, "tsserver": { ... } })
+    } else if !init_options.is_null() {
+        init_options
+    } else {
+        json!({})
+    };
```

---

## 4. jdtls Workspace Data Directory

jdtls requires a per-workspace data directory for indexing. Without it, jdtls fails or conflicts across projects.

**File**: `crates/pathfinder-lsp/src/client/process.rs`

In `spawn_lsp_child`, add jdtls-specific data directory arg. This is **NOT gated on `isolate_target_dir`** because jdtls always needs a data directory (unlike Rust/Go/TS/Python cache isolation which is only needed when concurrent LSPs are detected):

```rust
// jdtls always needs a unique data directory per workspace—not gated on
// isolate_target_dir because this is a functional requirement, not an
// isolation concern. Without -data, jdtls fails or shares state between projects.
if language_id == "java" {
    let data_dir = project_root.join(".pathfinder").join("jdtls-data");
    std::fs::create_dir_all(&data_dir).ok();
    cmd.arg("-data").arg(&data_dir);
}
```

> **NOTE**: `.pathfinder/` is not pre-existing in `.gitignore`. The function `ensure_pathfinder_in_gitignore()` auto-appends it at runtime when isolation creates files there. Since jdtls data dir creation is NOT gated on `isolate_target_dir`, we need to call `ensure_pathfinder_in_gitignore()` unconditionally for Java. Add after the jdtls block:
> ```rust
> if language_id == "java" {
>     ensure_pathfinder_in_gitignore(project_root);
> }
> ```

---

## 5. Detection Tests

Add to `crates/pathfinder-lsp/src/client/detect.rs` tests:

```
test_detects_java_via_pom_xml
test_detects_java_via_build_gradle
test_detects_java_via_build_gradle_kts
test_java_not_detected_without_binary
test_validate_marker_file_valid_pom_xml
test_validate_marker_file_invalid_pom_xml
test_validate_marker_file_empty_build_gradle
test_language_id_for_extension_java
```

---

## 6. Plugin Tests

Add to `crates/pathfinder-lsp/src/plugin.rs` tests:

```
test_java_plugin_language_id
test_java_plugin_file_extensions
test_java_plugin_marker_files
test_java_plugin_marker_search_depth
test_java_plugin_lsp_candidates
test_java_plugin_install_hint
test_plugin_for_extension_java
test_all_plugins_contains_all_five_languages (updated)
```

---

## Acceptance Criteria

- [ ] AC-2.1: `JavaPlugin` struct implements `LanguagePlugin` trait
- [ ] AC-2.2: `all_plugins()` includes `JavaPlugin`
- [ ] AC-2.3: `language_id_for_extension("java")` returns `Some("java")`
- [ ] AC-2.4: `detect_languages` detects Java via `pom.xml`, `build.gradle`, `build.gradle.kts`
- [ ] AC-2.5: `validate_marker_file` validates `pom.xml` structure
- [ ] AC-2.6: `LanguageLsp.python_path` replaced with generic `init_options: serde_json::Value`
- [ ] AC-2.7: Python venv detection still works after `init_options` migration
- [ ] AC-2.8: `build_initialize_request` passes Java init options to jdtls
- [ ] AC-2.9: jdtls data directory created at `.pathfinder/jdtls-data/`
- [ ] AC-2.10: `init_timeout_secs` set to 180s for Java (jdtls is slow)
- [ ] AC-2.11: All existing LSP tests pass (no regression from `init_options` migration)
- [ ] AC-2.12: `cargo test --workspace` passes
- [ ] AC-2.13: `cargo clippy --workspace` passes with zero warnings

## jdtls Version Compatibility

| jdtls Version | Java Analysis Range | JDK to Run jdtls | Notes |
|--------------|--------------------|--------------------|-------|
| 1.30+ | Java 8–21 | JDK 17+ | Stable |
| 1.35+ | Java 8–23 | JDK 21+ | Recommended |
| Latest | Java 8–25 | JDK 21+ | Use this |

**Recommendation**: Require JDK 21 LTS to run jdtls. Document this in `install_hint`.
