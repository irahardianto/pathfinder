# Project Conventions

## Directory Layout and Feature Organization
- Core logic is split into individual crates under `/crates`.
- Tests are co-located directly alongside production code inside `*_test.rs` files.
- Each production file `file.rs` includes the test module conditionally:
  ```rust
  #[cfg(test)]
  #[path = "file_test.rs"]
  mod tests;
  ```

## Interface and Mocking Conventions
- Mocking and test doubles are used for all I/O boundaries.
- `pathfinder-lsp` uses `FakeTransport` to capture JSON-RPC input/output.
- `pathfinder-search` uses custom mock searchers.
- `pathfinder-common` implements `FakeGitRunner` for sandboxed git operations.

## Error Handling and Validation
- Fallible functions must return `Result` with appropriate error types.
- Inputs must be validated at the boundary before processing.
- No silent failures. All failures must be logged or returned.
