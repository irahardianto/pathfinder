# Research Log: LSP did_close and Tests
Date: 2026-03-09

## Finding F1: Missing `did_close` in Lawyer trait
- **Analysis:** `run_lsp_validation` in `edit.rs` opens a document on the LSP using `did_open`, performs diagnostics, and does not close it. This leaves the document open in the LSP server forever. We need to add `did_close(&self, workspace_root: &Path, file_path: &Path) -> Result<(), LspError>` to the `Lawyer` trait.
- **Implementation:** 
  - Add to `Lawyer` in `lawyer.rs`.
  - Implement in `MockLawyer` (`mock.rs`).
  - Implement in `LspClient` (`client/mod.rs`).
  - In `edit.rs`'s `run_lsp_validation`, ensure `did_close` is called for *every* exit path after a successful `did_open`. A `Drop` guard could be used, or simply adding it to every return path. A `Drop` guard is harder because `did_close` is async. Better to use a macro, or call it manually before returning.

## Finding F2: Missing tests for `run_lsp_validation`
- **Analysis:** `run_lsp_validation` currently has no unit tests. `edit.rs` has tests (`tests/edit.rs` or end of file) that use `MockLawyer`. We can add specific tests that call `run_lsp_validation` on a mock server. 
- **Tests to write:**
  - `test_run_lsp_validation_success`
  - `test_run_lsp_validation_no_lsp`

## Finding F3: Missing unit tests for `file_ops.rs`
- **Analysis:** `apply_replacements` and the overall handlers in `file_ops.rs` lack a test module.
- **Tests to write:**
  - In `file_ops.rs`, add an isolated `mod tests` block.
  - Test pure `apply_replacements` logic (success, no match, ambiguous match).
