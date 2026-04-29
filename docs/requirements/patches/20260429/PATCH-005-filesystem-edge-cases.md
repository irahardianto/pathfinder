# PATCH-005: File System Edge Cases

## Status: COMPLETED (2026-04-29)

## Objective

Improve error messages when `read_file` encounters a binary (non-UTF-8) file. Currently, the error is a generic "failed to read file" which doesn't help agents understand the root cause or take corrective action.

## Severity: LOW — User-facing error improvement

---

## Scope

| # | File | Function | Action |
|---|------|----------|--------|
| 1 | `crates/pathfinder/src/server/tools/file_ops.rs` | `read_file_impl` | ADD binary file detection |

This patch modifies **1 file only**: `crates/pathfinder/src/server/tools/file_ops.rs`

---

## Current Code

**File:** `crates/pathfinder/src/server/tools/file_ops.rs`
**Function:** `read_file_impl`

Find the `read_to_string` call in `read_file_impl` (around line 320):

```rust
        let raw_content = match tfs::read_to_string(&absolute_path).await {
```

Look at the error handling block that follows. It will look something like:

```rust
            Err(e) => {
                // ... some error handling that returns a generic error
            }
```

---

## Target Code

Replace the error handling for `read_to_string` to distinguish binary files:

```rust
        let raw_content = match tfs::read_to_string(&absolute_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!(
                    tool = "read_file",
                    path = %relative_path.display(),
                    "file contains invalid UTF-8 (likely binary)"
                );
                return Err(io_error_data(
                    "file appears to be binary (not valid UTF-8). read_file only supports text files."
                ));
            }
            Err(e) => {
                tracing::warn!(
                    tool = "read_file",
                    path = %relative_path.display(),
                    error = %e,
                    "read_file: failed to read file"
                );
                return Err(io_error_data(format!(
                    "failed to read file '{}': {e}",
                    relative_path.display()
                )));
            }
        };
```

> **NOTE:** The exact variable names (`absolute_path`, `relative_path`) must match what's already in the function. Read the current function to verify the variable names before editing. The pattern may use `file_path` instead of `relative_path`.

---

## New Test

Add this test to the existing `mod tests` block in `file_ops.rs`, or to `crates/pathfinder/src/server.rs` test module if `file_ops` tests are there:

```rust
    #[tokio::test]
    async fn test_read_file_binary_returns_clear_error() {
        // Create a temp file with invalid UTF-8 bytes
        let dir = tempfile::tempdir().expect("tempdir");
        let binary_path = dir.path().join("binary.dat");
        std::fs::write(&binary_path, b"\xff\xfe\x00\x01\x80\x81").expect("write binary");

        // Build server pointing at the temp dir
        let server = PathfinderServer::with_engines(
            dir.path(),
            &Default::default(),
            Arc::new(pathfinder_search::RipgrepScout),
            Arc::new(pathfinder_treesitter::TreeSitterSurgeon::new()),
        );

        let result = server
            .read_file(ReadFileParams {
                filepath: "binary.dat".into(),
                start_line: None,
                max_lines: None,
            })
            .await;

        let err = result.unwrap_err();
        let msg = err.message.to_string();
        assert!(
            msg.contains("binary") || msg.contains("UTF-8"),
            "error should mention binary/UTF-8, got: {msg}"
        );
    }
```

> **NOTE:** Adjust import paths and constructor calls to match the existing test patterns in the file. If `PathfinderServer::with_engines` requires different parameters, use the pattern from existing tests (e.g., `make_server` helper).

---

## Verification

```bash
# 1. Confirm binary file error message exists
grep -n 'binary.*UTF-8\|not valid UTF-8' crates/pathfinder/src/server/tools/file_ops.rs

# Expected: at least 1 match

# 2. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Completion Criteria

- [ ] `read_file_impl` returns specific error for binary files
- [ ] Error message mentions "binary" and "UTF-8"
- [ ] Generic "failed to read file" error still handles other I/O errors
- [ ] New test added and passing
- [ ] `cargo test --all` passes
- [ ] `cargo clippy` passes with zero warnings
