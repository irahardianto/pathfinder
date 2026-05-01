## 2026-04-30 - Shutdown Signal Test
**Learning:** The LSP client's `shutdown()` method had no unit test covering the actual firing of the shutdown signal over its internal `shutdown_tx` channel. This was a clear coverage gap for a public method with side effects.
**Action:** Implemented `test_shutdown_sends_signal` using a subscriber to verify the channel broadcast. Always verify that public fire-and-forget signal methods are actually tested.

## 2026-05-01 - Process Shutdown Execution Signal
**Learning:** The LSP client's `shutdown` method in `process.rs` lacked a test covering its process termination side-effect (`process.child.kill()`). This represents a gap in verifying process lifecycle management.
**Action:** Implemented `test_shutdown_terminates_process` using a dummy `sleep` process to verify `shutdown` successfully kills the underlying child process. Verify lifecycle functions actually affect the underlying resource.
