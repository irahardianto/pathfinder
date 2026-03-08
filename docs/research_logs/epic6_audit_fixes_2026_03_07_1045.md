# Epic 6: Audit Fixes (2026-03-07 10:45)

## Overview
This log tracks the research and planning phase for resolving the 7 issues identified in `docs/audits/review-findings-pathfinder-all-2026-03-07-1045.md`.

## Findings to Resolve

1.  **F1 [ERR]**: `path_to_file_uri()` manually constructs `file://` URIs instead of using the `url::Url` crate.
    -   **Location**: `pathfinder-lsp/src/client/process.rs:216-229`
    -   **Fix**: Update `path_to_file_uri` to use `url::Url::from_file_path()` or `from_directory_path()`. This will correctly handle percent-encoding.

2.  **F2 [RES]**: `detect_languages()` uses blocking `std::fs` inside a synchronous function called during startup from an async executor.
    -   **Location**: `pathfinder-lsp/src/client/detect.rs`
    -   **Fix**: Change `detect_languages` to be `async` and use `tokio::fs::metadata(path).await.is_ok()` or similar non-blocking IO. Update `LspClient::new` to `async fn new(...)` and update callers (like `PathfinderServer::new`).

3.  **F3 [SEC]**: `lsp-types = "0.97"` declared in `pathfinder-lsp/Cargo.toml` but unused.
    -   **Location**: `pathfinder-lsp/Cargo.toml`
    -   **Fix**: Remove the dependency from `Cargo.toml`.

4.  **F4 [PAT]**: `path_to_file_uri()` uses blocking `path.is_dir()`.
    -   **Fix**: Automatically resolved by F1, because `url::Url` handles the trailing slash appropriately when using `from_directory_path`, or we can just make it async if we still need to check if it's a directory. Wait, actually `from_directory_path` ensures a trailing slash, but requires knowing it's a directory. We might need `tokio::fs::metadata(path).await?.is_dir()` if we still need to check. But since `process.rs` `spawn_and_initialize` is already `async`, making `path_to_file_uri` async is easy.

5.  **F5 [TEST]**: `process.rs` has no unit tests.
    -   **Location**: `pathfinder-lsp/src/client/process.rs`
    -   **Fix**: Add a `mod tests` in `process.rs` covering at least `path_to_file_uri`.

6.  **Nits**: `cargo fmt` not run on `pathfinder-lsp`.
    -   **Fix**: Run `cargo fmt --all`.
