# PATCH-004: Add @vue/typescript-plugin Auto-Detection

## Group: B (Vue) — TS Plugin Configuration
## Depends on: PATCH-003

## Objective

Wire up the Vue plugin auto-detection end-to-end and verify that Vue SFC files
get proper LSP support. This patch focuses on the detection heuristics, edge cases,
and the integration between detect.rs and the TS plugin system from PATCH-003.

## Severity: MEDIUM — completes Vue LSP integration

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/src/client/detect.rs` | Complete Vue plugin detection heuristics | Handle monorepo, workspaces, pnpm, global installs |
| 2 | `crates/pathfinder-lsp/src/client/detect.rs` | Detect Vue files in workspace | Only enable plugin when .vue files exist |
| 3 | `.pathfinder.toml` schema docs | Document typescript_plugins config | User-facing configuration reference |

## Step 1: Complete Detection Heuristics

**File:** `crates/pathfinder-lsp/src/client/detect.rs`

The plugin can be installed in several locations:

1. `node_modules/@vue/typescript-plugin` (standard npm/yarn)
2. `node_modules/.pnpm/@vue+typescript-plugin@*/node_modules/@vue/typescript-plugin` (pnpm)
3. Config override via `.pathfinder.toml` (always takes precedence)

Detection logic:

```rust
/// Find the resolve path for a TypeScript plugin.
///
/// Checks standard node_modules, pnpm structure, and falls back to
/// config override. Returns the plugin name (not path) — tsserver
/// resolves plugins by name from its own module resolution.
async fn detect_ts_plugin(workspace_root: &Path, plugin_name: &str) -> Option<String> {
    // Standard npm/yarn location
    let standard = workspace_root
        .join("node_modules")
        .join(plugin_name);
    if tokio::fs::metadata(&standard).await.is_ok() {
        return Some(plugin_name.to_owned());
    }

    // pnpm location (nested in .pnpm)
    let pnpm_dir = workspace_root.join("node_modules").join(".pnpm");
    if let Ok(mut entries) = tokio::fs::read_dir(&pnpm_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // pnpm uses + as separator: @vue+typescript-plugin@2.0.0
            if name_str.contains(&plugin_name.replace('/', "+").replace("@", "")) {
                return Some(plugin_name.to_owned());
            }
        }
    }

    None
}

/// Check if the workspace contains Vue single-file components.
async fn workspace_has_vue_files(workspace_root: &Path) -> bool {
    // Quick check: scan top-level src/ directory for .vue files.
    // Avoid full recursive scan for performance.
    let src_dir = workspace_root.join("src");
    let check_dir = if tokio::fs::metadata(&src_dir).await.is_ok() {
        src_dir
    } else {
        workspace_root.to_path_buf()
    };

    if let Ok(mut entries) = tokio::fs::read_dir(&check_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "vue" {
                    return true;
                }
            }
            // Check one level of subdirectories
            if path.is_dir() {
                if let Ok(mut sub) = tokio::fs::read_dir(&path).await {
                    while let Ok(Some(sub_entry)) = sub.next_entry().await {
                        if let Some(ext) = sub_entry.path().extension() {
                            if ext == "vue" {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}
```

Update the TypeScript detection block:

```rust
// After constructing the TypeScript LanguageLsp:
if let Some(root) = ts_root {
    let cmd = get_command_override!("typescript")
        .or_else(|| resolve_command("typescript-language-server", "typescript"));
    if let Some(command) = cmd {
        // Auto-detect Vue plugin
        let mut auto_plugins = Vec::new();

        // Check config override first
        let config_plugins: Vec<String> = config
            .lsp
            .get("typescript")
            .map(|c| c.typescript_plugins.clone())
            .unwrap_or_default();

        if !config_plugins.is_empty() {
            // User explicitly configured plugins — use those
            auto_plugins = config_plugins;
        } else if workspace_has_vue_files(workspace_root).await {
            // Auto-detect Vue plugin
            if let Some(plugin) = detect_ts_plugin(
                workspace_root,
                "@vue/typescript-plugin",
            ).await {
                tracing::info!(
                    "LSP: auto-detected @vue/typescript-plugin for Vue SFC support"
                );
                auto_plugins.push(plugin);
            }
        }

        detected.push(LanguageLsp {
            language_id: "typescript".to_owned(),
            command,
            args: get_args!("typescript", vec!["--stdio".to_owned()]),
            root,
            init_timeout_secs: None,
            auto_plugins,
        });
    }
}
```

## Step 2: Handle Monorepo Vue Detection

For monorepos where Vue files are in a subdirectory (e.g., `apps/frontend/`),
the detection should scan up to depth 2 (already supported by `find_marker`).
The Vue file check should also scan deeper:

```rust
/// Check for .vue files up to 3 levels deep from any workspace root.
async fn workspace_has_vue_files_deep(workspace_root: &Path) -> bool {
    // Recursively check for .vue files with depth limit
    async fn has_vue_recursive(dir: &Path, depth: usize) -> bool {
        if depth > 3 { return false; }
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else { return false };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "vue" { return true; }
            }
            if path.is_dir() && has_vue_recursive(&path, depth + 1).await {
                return true;
            }
        }
        false
    }
    has_vue_recursive(workspace_root, 0).await
}
```

## Step 3: Tests

### Unit Tests (detect.rs)

- `test_detect_vue_plugin_standard_npm` — node_modules/@vue/typescript-plugin exists
  -> auto_plugins contains "@vue/typescript-plugin"
- `test_detect_vue_plugin_pnpm` — .pnpm directory with vue+typescript-plugin
  -> auto_plugins contains "@vue/typescript-plugin"
- `test_no_vue_plugin_without_vue_files` — plugin installed but no .vue files
  -> auto_plugins is empty (don't load plugin unnecessarily)
- `test_config_plugins_override_auto_detection` — .pathfinder.toml has explicit
  typescript_plugins -> those used instead of auto-detection
- `test_detect_vue_files_in_subdirectory` — .vue files in src/components/
  -> detection returns true
- `test_no_vue_files_returns_false` — workspace with only .ts files
  -> detection returns false

## EXCLUSIONS — Do NOT Modify These

- `process.rs` — plugin passing in initialize is PATCH-003
- `capabilities.rs` — unchanged
- `navigation.rs` — unchanged
- `validation.rs` — unchanged

## Verification

```bash
# 1. Build
cargo build --all

# 2. Tests pass
cargo test --all

# 3. Detection logic covers npm and pnpm
grep -n "pnpm\|detect_ts_plugin" crates/pathfinder-lsp/src/client/detect.rs
# Expected: both npm and pnpm detection paths

# 4. Vue file detection exists
grep -n "workspace_has_vue_files" crates/pathfinder-lsp/src/client/detect.rs
# Expected: function definition + call in TS detection block

# 5. Manual test with Vue project:
#    - Create workspace with package.json + @vue/typescript-plugin
#    - Add a .vue file with <script setup lang="ts">
#    - Start Pathfinder
#    - Call lsp_health -> should show "typescript" with indexing
#    - Call get_definition on a Vue component prop -> should resolve
```

## Expected Impact

- Vue SFC files get full LSP navigation when @vue/typescript-plugin is installed
- Plugin is only loaded when .vue files exist (no overhead for pure TS projects)
- Monorepo support (Vue frontend + Go backend in same workspace)
- Config override for non-standard plugin installations
