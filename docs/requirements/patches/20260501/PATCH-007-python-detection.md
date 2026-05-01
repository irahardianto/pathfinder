# PATCH-007: Verify Python LSP Detection Completeness

## Group: D (Provisioning) — Python Support
## Depends on: None

## Objective

Verify and extend Python LSP detection in `detect.rs` to cover all common Python
LSP servers. The current code only checks for `pyright`. This patch adds detection
for `pylsp` (python-lsp-server), `ruff-lsp`, and `jedi-language-server` as fallbacks.

## Severity: LOW — Python LSP is not broken, just limited detection

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/src/client/detect.rs` | Extend Python detection | Try pyright, then pylsp, then ruff-lsp, then jedi-language-server |
| 2 | `crates/pathfinder-lsp/src/client/detect.rs` tests | Add detection tests | Test fallback chain |

## Step 1: Extend Python Detection

**File:** `crates/pathfinder-lsp/src/client/detect.rs`

Replace the single `resolve_command("pyright", "python")` with a fallback chain:

```rust
// Python — pyproject.toml, setup.py, or requirements.txt (depth 2)
let py_root = match get_override!("python") {
    Some(r) => Some(r),
    None => find_marker(workspace_root, "pyproject.toml", 2)
        .await
        .or(find_marker(workspace_root, "setup.py", 2).await)
        .or(find_marker(workspace_root, "requirements.txt", 2).await),
};
if let Some(root) = py_root {
    // Try Python LSP servers in order of preference.
    // pyright: Fast, strict type checking, most popular for modern Python
    // pylsp: Community standard, plugin ecosystem, good all-rounder
    // ruff-lsp: Extremely fast, new, growing adoption
    // jedi-language-server: Mature, lightweight, pure Python
    let python_lsp_binaries = [
        ("pyright", vec!["--stdio".to_owned()]),
        ("pylsp", vec![]),
        ("ruff-lsp", vec![]),
        ("jedi-language-server", vec![]),
    ];

    let cmd = get_command_override!("python").or_else(|| {
        for (binary, default_args) in &python_lsp_binaries {
            if let Some(resolved) = resolve_command(binary, "python") {
                // Return the first one found, with its default args
                return Some((resolved, default_args.clone()));
            }
        }
        None
    });

    if let Some((command, default_args)) = cmd {
        let args = get_args!("python", default_args);
        detected.push(LanguageLsp {
            language_id: "python".to_owned(),
            command,
            args,
            root,
            init_timeout_secs: None,
            auto_plugins: vec![],
        });
    }
}
```

Note: This requires updating the `get_command_override!` macro usage since we now
return a tuple. Alternatively, extract the command resolution into a helper function
that returns `Option<(String, Vec<String>)>`.

## Step 2: Tests

- `test_detects_python_via_pyright` — pyproject.toml + pyright on PATH -> detected
- `test_detects_python_fallback_to_pylsp` — pyproject.toml + pylsp (no pyright) -> detected with pylsp
- `test_detects_python_fallback_to_ruff` — pyproject.toml + ruff-lsp only -> detected
- `test_detects_python_fallback_to_jedi` — pyproject.toml + jedi only -> detected
- `test_python_not_detected_without_binary` — pyproject.toml but no LSP on PATH -> not detected
- `test_prefers_pyright_over_pylsp` — both installed -> detects pyright
- `test_python_args_correct_per_binary` — pyright gets --stdio, pylsp gets empty args

## EXCLUSIONS

- `navigation.rs` — no changes
- `validation.rs` — no changes
- `lawyer.rs` — no changes
- `capabilities.rs` — no changes (pyright reports standard capabilities)

## Verification

```bash
cargo build --all
cargo test --all

# Fallback chain in detect.rs
grep -n "pyright\|pylsp\|ruff-lsp\|jedi" crates/pathfinder-lsp/src/client/detect.rs
# Expected: all four listed in preference order
```

## Expected Impact

- Python LSP works with any of the four most common Python language servers
- Users don't need to install a specific LSP — whatever they have works
- Detection preference follows quality/reliability ordering
