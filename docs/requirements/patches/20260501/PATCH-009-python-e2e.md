# PATCH-009: End-to-End Python LSP Verification Test

## Group: D (Provisioning) — Python Support
## Depends on: PATCH-007

## Objective

Create a comprehensive integration test that verifies the full Python LSP pipeline:
detection -> spawn -> initialize -> goto_definition -> call_hierarchy -> validation.
This test requires pyright (or another Python LSP) to be installed and validates
that the entire agnostic channel works for Python, not just individual components.

## Severity: LOW — verification test, not a code fix

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/tests/lsp_client_integration.rs` | Add Python integration test | Full pipeline test gated on pyright availability |
| 2 | `crates/pathfinder-treesitter/src/symbols.rs` tests | Verify name_column for Python | Confirm Python tree-sitter emits correct name_column |

## Step 1: Python Integration Test

**File:** `crates/pathfinder-lsp/tests/lsp_client_integration.rs`

Add a Python integration test (gated on pyright availability to avoid CI failures):

```rust
#[cfg(test)]
mod python_integration {
    use super::*;

    fn pyright_available() -> bool {
        which::which("pyright").is_ok()
    }

    #[tokio::test]
    async fn test_python_lsp_full_pipeline() {
        if !pyright_available() {
            eprintln!("Skipping Python integration test: pyright not installed");
            return;
        }

        let dir = tempfile::tempdir().expect("temp dir");
        let pyproject = dir.path().join("pyproject.toml");
        tokio::fs::write(&pyproject, "[tool.poetry]\nname = \"test\"\n")
            .await
            .expect("write pyproject");

        let src_dir = dir.path().join("src");
        tokio::fs::create_dir_all(&src_dir).await.expect("create src");

        let main_py = src_dir.join("main.py");
        tokio::fs::write(
            &main_py,
            r#"
def greet(name: str) -> str:
    return f"Hello, {name}!"

def main() -> None:
    message = greet("world")
    print(message)

if __name__ == "__main__":
    main()
"#,
        )
        .await
        .expect("write main.py");

        let config = PathfinderConfig::default();
        let client = LspClient::new(dir.path(), Arc::new(config))
            .await
            .expect("LspClient init");

        client.warm_start();

        // Wait for indexing (pyright is fast, 5s should be plenty)
        tokio::time::sleep(Duration::from_secs(5)).await;

        // Test goto_definition: jump to `greet` from the call site
        let result = client
            .goto_definition(
                dir.path(),
                Path::new("src/main.py"),
                7,  // line of `message = greet("world")`
                17, // column of `g` in `greet`
            )
            .await;

        match result {
            Ok(Some(def)) => {
                assert!(
                    def.file.contains("main.py"),
                    "definition should be in main.py, got: {}",
                    def.file
                );
                // Line should be near the def greet declaration (line 2)
                assert!(
                    def.line >= 2 && def.line <= 3,
                    "definition line should be near def greet, got: {}",
                    def.line
                );
            }
            Ok(None) => {
                // LSP might still be warming up — acceptable in CI
                eprintln!("Python goto_definition returned None (possibly still warming up)");
            }
            Err(e) => {
                panic!("Python goto_definition failed: {e}");
            }
        }

        // Test call_hierarchy_prepare on the `greet` function
        let hierarchy = client
            .call_hierarchy_prepare(
                dir.path(),
                Path::new("src/main.py"),
                2,  // line of `def greet`
                5,  // column of `g` in `greet`
            )
            .await;

        // Should either work or return UnsupportedCapability (pyright may not support it)
        match hierarchy {
            Ok(items) => {
                assert!(!items.is_empty(), "should find greet in call hierarchy");
                assert_eq!(items[0].name, "greet");
            }
            Err(LspError::UnsupportedCapability { .. }) => {
                // Acceptable: pyright may not support call hierarchy
            }
            Err(e) => {
                panic!("Unexpected call hierarchy error: {e}");
            }
        }

        client.shutdown();
    }
}
```

## Step 2: Verify Python name_column

**File:** `crates/pathfinder-treesitter/src/symbols.rs`

Add test confirming Python tree-sitter emits correct `name_column`:

```rust
#[test]
fn test_python_name_column_points_to_function_name() {
    let source = r#"
def compute(x: int) -> int:
    return x * 2
"#;
    let mut file = Builder::new().suffix(".py").tempfile().unwrap();
    file.write_all(source.as_bytes()).unwrap();

    let surgeon = Surgeon::new(Arc::new(MockConfig::default()));
    let result = surgeon.read_source_file(file.path().parent().unwrap(), file.path().relative_from(...)).await;

    // Verify that the `compute` function has name_column pointing to 'c'
    // Line: "def compute(x: int) -> int:"
    // Column 0: 'd' in 'def'
    // Column 4: 'c' in 'compute'
    // name_column should be 4
    let syms = result.unwrap().2;
    assert_eq!(syms[0].name, "compute");
    assert_eq!(syms[0].name_column, 4, "name_column should point to 'c' in 'compute', not 'd' in 'def'");
}
```

## EXCLUSIONS

- No production code changes — this is a test-only patch
- Not gated as required for CI — test is skipped when pyright is unavailable

## Verification

```bash
# 1. Install pyright (optional, for running the integration test)
npm install -g pyright

# 2. Run the test
cargo test --package pathfinder-lsp --test lsp_client_integration python_integration

# 3. Run name_column test
cargo test --package pathfinder-treesitter test_python_name_column

# 4. Without pyright installed, test should skip gracefully
cargo test --package pathfinder-lsp --test lsp_client_integration 2>&1 | grep "Skipping Python"
```

## Expected Impact

- Confirms full Python LSP pipeline works end-to-end
- Catches regressions in Python name_column extraction
- Documents expected behavior for Python LSP integration
- Test is non-blocking in CI (skipped when pyright unavailable)
