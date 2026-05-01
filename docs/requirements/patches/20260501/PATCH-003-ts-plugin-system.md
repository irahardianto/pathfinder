# PATCH-003: TypeScript Plugin System

## Group: B (Vue) — TS Plugin Configuration
## Depends on: None

## Objective

Add a mechanism to configure TypeScript LSP plugins via `initializationOptions`
in the `initialize` handshake. This enables `@vue/typescript-plugin` (for Vue SFC
support) and future plugins (Svelte, etc.) without spawning separate LSP processes.

Today, `typescript-language-server` is spawned with default initialization options.
It doesn't know about Vue files. By passing `plugins` in `initializationOptions`,
tsserver loads `@vue/typescript-plugin` and gains full Vue SFC understanding
(script, template, and style blocks).

## Severity: MEDIUM — unblocks Vue LSP navigation and analysis

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/src/client/detect.rs` | Add TS plugin detection | Scan for `@vue/typescript-plugin` in node_modules |
| 2 | `crates/pathfinder-lsp/src/client/process.rs` | Add initializationOptions to initialize request | Include plugins config for TS |
| 3 | `crates/pathfinder-common/src/config.rs` | Add `typescript_plugins` config field | Allow manual plugin config via .pathfinder.toml |

## Step 1: Define Plugin Configuration

**File:** `crates/pathfinder-common/src/config.rs`

Add to `LspConfig`:

```rust
/// TypeScript plugins to load via initializationOptions.
///
/// Each entry is a plugin name that will be resolved from node_modules.
/// Example: `["@vue/typescript-plugin"]`
#[serde(default)]
pub typescript_plugins: Vec<String>,
```

## Step 2: Detect Available Plugins

**File:** `crates/pathfinder-lsp/src/client/detect.rs`

Add auto-detection for Vue plugin. After the TypeScript detection block, check
if `@vue/typescript-plugin` is available in the workspace:

```rust
// Auto-detect Vue plugin for TypeScript LSP
// If workspace contains .vue files and the plugin is installed, add it.
let vue_plugin_detected = if ts_root.is_some() {
    // Check for @vue/typescript-plugin in node_modules
    let node_modules = workspace_root.join("node_modules");
    let vue_plugin_path = node_modules.join("@vue/typescript-plugin");
    tokio::fs::metadata(&vue_plugin_path).await.is_ok()
} else {
    false
};

// When constructing the TypeScript LanguageLsp, include detected plugins
if let Some(root) = ts_root {
    let cmd = get_command_override!("typescript")
        .or_else(|| resolve_command("typescript-language-server", "typescript"));
    if let Some(command) = cmd {
        let mut args = get_args!("typescript", vec!["--stdio".to_owned()]);

        // Plugins are passed via config, not args
        // The actual plugin list is built in process.rs from config + auto-detection

        detected.push(LanguageLsp {
            language_id: "typescript".to_owned(),
            command,
            args,
            root,
            init_timeout_secs: None,
        });
    }
}
```

Note: The plugin configuration is passed at the `initialize` handshake level,
not as CLI args. So `detect.rs` just needs to note that plugins are available.
The actual plugin list will be assembled from config + auto-detection and stored
in the `LanguageLsp` struct or resolved at spawn time.

Add `auto_plugins: Vec<String>` to `LanguageLsp`:

```rust
pub struct LanguageLsp {
    // ... existing fields ...

    /// Auto-detected TypeScript plugins to load during initialization.
    /// Populated by detect.rs when scanning the workspace.
    pub auto_plugins: Vec<String>,
}
```

## Step 3: Pass Plugins in Initialize Request

**File:** `crates/pathfinder-lsp/src/client/process.rs`

Update `spawn_and_initialize` to accept and pass plugin configuration:

Add parameter:

```rust
pub(super) async fn spawn_and_initialize(
    command: &str,
    args: &[String],
    project_root: &Path,
    language_id: &str,
    dispatcher: Arc<RequestDispatcher>,
    init_timeout_secs: Option<u64>,
    isolate_target_dir: bool,
    plugins: Vec<String>,  // NEW
) -> Result<(ManagedProcess, tokio::task::JoinHandle<()>), LspError> {
```

Update `build_initialize_request` to include `initializationOptions`:

```rust
async fn build_initialize_request(
    id: u64,
    project_root: &Path,
    plugins: &[String],
) -> Result<Value, LspError> {
    // ... existing code ...

    let initialization_options = if !plugins.is_empty() {
        // Build plugins array for typescript-language-server
        let plugin_entries: Vec<Value> = plugins.iter().map(|name| {
            json!({
                "name": name
            })
        }).collect();

        json!({
            "plugins": plugin_entries,
            // Tell tsserver to handle .vue files
            "tsserver": {
                "extraFileExtensions": [
                    { "extension": "vue", "scriptKind": "TS" }
                ]
            }
        })
    } else {
        json!({})
    };

    Ok(RequestDispatcher::make_request(
        id,
        "initialize",
        &json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "pathfinder", "version": "0.1.0" },
            "rootUri": workspace_uri,
            "workspaceFolders": [{ "uri": workspace_uri, "name": workspace_name }],
            "initializationOptions": initialization_options,
            "capabilities": {
                // ... existing capabilities ...
                "textDocument": {
                    // ... existing ...
                    "definition": { "dynamicRegistration": false, "linkSupport": false },
                    "publishDiagnostics": { "relatedInformation": false }
                },
            }
        }),
    ))
}
```

Update the call site in `mod.rs` `start_process` to pass plugins:

```rust
let plugins = descriptor.auto_plugins.clone();
let spawn_result = spawn_and_initialize(
    &descriptor.command,
    &descriptor.args,
    &descriptor.root,
    &language_id,
    Arc::clone(&self.dispatcher),
    descriptor.init_timeout_secs,
    isolate_target_dir,
    plugins,  // NEW
).await;
```

## Step 4: Tests

### Unit Tests (detect.rs)

- `test_auto_detects_vue_plugin_when_present` — workspace with node_modules/@vue/typescript-plugin
  -> LanguageLsp.auto_plugins contains "@vue/typescript-plugin"
- `test_no_vue_plugin_when_absent` — workspace without plugin -> auto_plugins is empty
- `test_no_vue_plugin_when_no_ts` — workspace without TS -> no plugin detection

### Unit Tests (process.rs)

- `test_initialize_includes_plugins_when_present` — plugins=["@vue/typescript-plugin"]
  -> initializationOptions.plugins contains entry
- `test_initialize_empty_when_no_plugins` — plugins=[] -> initializationOptions is empty object
- `test_initialize_includes_vue_file_extension` — when plugins present
  -> extraFileExtensions includes vue entry

## EXCLUSIONS — Do NOT Modify These

- `navigation.rs` — unchanged
- `validation.rs` — unchanged (push diagnostics is PATCH-002)
- `lawyer.rs` — trait unchanged
- Tree-sitter Vue handling — unchanged (already works)

## Verification

```bash
# 1. Build
cargo build --all

# 2. Tests pass
cargo test --all

# 3. Plugin detection logic exists
grep -n "auto_plugins" crates/pathfinder-lsp/src/client/detect.rs
# Expected: field population logic

# 4. Initialize request includes plugins
grep -n "initializationOptions" crates/pathfinder-lsp/src/client/process.rs
# Expected: plugins array construction

# 5. Manual test:
#    - Create a Vue workspace with @vue/typescript-plugin installed
#    - Start Pathfinder
#    - Call get_definition on a Vue component method
#    - Expected: definition resolved (not SYMBOL_NOT_FOUND)
```

## Expected Impact

- TypeScript LSP gains Vue SFC understanding via plugin
- `.vue` files get full LSP navigation (get_definition, analyze_impact, read_with_deep_context)
- Template and script block symbols are resolvable
- No changes to Rust, Go, or Python paths
- Foundation for future plugins (Svelte via `svelte2tsx`, etc.)
