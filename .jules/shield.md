## 2026-04-30 - Shutdown Signal Test
**Learning:** The LSP client's `shutdown()` method had no unit test covering the actual firing of the shutdown signal over its internal `shutdown_tx` channel. This was a clear coverage gap for a public method with side effects.
**Action:** Implemented `test_shutdown_sends_signal` using a subscriber to verify the channel broadcast. Always verify that public fire-and-forget signal methods are actually tested.

## 2026-05-01 - Process Shutdown Execution Signal
**Learning:** The LSP client's `shutdown` method in `process.rs` lacked a test covering its process termination side-effect (`process.child.kill()`). This represents a gap in verifying process lifecycle management.
**Action:** Implemented `test_shutdown_terminates_process` using a dummy `sleep` process to verify `shutdown` successfully kills the underlying child process. Verify lifecycle functions actually affect the underlying resource.

## 2024-05-01 - Testing apply_filter_mode
**Learning:** Verified the `apply_filter_mode` behavior for filtering tree-sitter node classification in search matches. Learned that tree sitter classification labels `string` and `comment` are categorized under `CommentsOnly` filter mode, and `code` under `CodeOnly`.
**Action:** Always verify the tree-sitter string literals expected by internal matching algorithms before writing coverage tests to ensure the data matches.

## 2025-05-01 - URL Parsing Fallback Coverage
**Learning:** In LSP response parsers, URL parsing (e.g. `Url::parse(uri_str)`) can fail for missing schemes or non-file URLs. The fallback logic, such as returning the original raw `uri_str`, must be explicitly tested.
**Action:** Next time I add or verify parser coverage, I will ensure that not only the happy path but also the fallback behavior for malformed inputs (like missing schemes or invalid URLs) are tested.
