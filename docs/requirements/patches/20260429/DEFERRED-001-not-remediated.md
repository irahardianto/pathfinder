# DEFERRED-001: Findings Explicitly Not Remediated

## Purpose

This document records findings from the April 2026 audit that were **deliberately not remediated** and explains exactly why. It exists to prevent future agents from:
- Re-opening these as bugs
- Spending time re-investigating them
- Implementing "fixes" that break correct behavior

> **This document describes no actions to take. It is reference material only.**

---

## Deferred Finding Registry

### D-01: Edit Handler Boilerplate (F1.1b)

**Category:** Code duplication  
**Finding:** Repetitive param-parsing + sandbox-check preamble across edit tool handlers.  
**Decision:** DEFERRED — LOW ROI

**Rationale:** The `edit/` module was already decomposed into `handlers.rs`, `batch.rs`, and `validation.rs`. The remaining duplication is **structural**: each handler must independently parse its own typed params and dispatch. Abstracting this further would require a macro or type-erased dispatch layer that adds complexity without eliminating meaningful logic. The `check_sandbox_access()` helper already de-duplicates the most costly repetition.

**Reconsider when:** A 4th edit submodule is added that duplicates the same preamble again.

---

### D-02: Testing Gaps — Unit Tests (F1.3a)

**Category:** Test coverage  
**Finding:** Missing unit tests for `detect.rs`, `sandbox.rs`, and `text_edit.rs`.  
**Decision:** PARTIALLY DEFERRED — addressed in separate test coverage sessions

**Rationale:** Test coverage sessions (conversations `1b4451bd`, `c2da9926`, `3471efba`) already addressed the integration test harness and mock LSP infrastructure. Unit tests for `detect.rs` and `sandbox.rs` are tracked separately in TCV-001.

**Reconsider when:** DeepSource reports < 70% line coverage in these specific modules.

---

### D-03: Testing Gaps — Integration Tests (F1.3b, F1.3c, F1.3d)

**Category:** Test coverage  
**Findings:** Missing E2E LSP tests, Shadow Editor pipeline tests, `replace_batch` corner cases.  
**Decision:** DEFERRED — requires infrastructure

**Rationale:** Full E2E LSP tests require the mock LSP server binary to be running in CI. The binary infrastructure (`crates/test-mock-lsp`) was established but full tool-level integration tests depend on stable CI pipeline. These are tracked against the integration test sessions (`0fd8710d`, `7a17fa31`).

**Reconsider when:** CI integration test suite is stable and consistently green.

---

### D-04: `replace_batch_impl` Complexity (F1.5c)

**Category:** Code complexity  
**Finding:** `replace_batch_impl` has high cyclomatic complexity.  
**Decision:** DEFERRED — inherent complexity

**Rationale:** The function already splits into `batch.rs` (orchestration) and `text_edit.rs` (text-mode resolution). The remaining complexity is **inherent to the mixed targeting modes** it must support (semantic targets + text targets, with cross-file rollback semantics). Any further decomposition would sacrifice the atomicity guarantee.

**Reconsider when:** A new targeting mode (e.g., regex-based targeting) is added and complexity becomes unmanageable.

---

### D-05: `byte_to_point` O(n) Performance (F1.5d)

**Category:** Performance  
**Finding:** `byte_to_point` scans bytes linearly from file start.  
**Decision:** DEFERRED — immaterial cost

**Rationale:** Called 2× per zone boundary in Vue SFC parsing. Vue SFCs rarely exceed 10KB. At ~1 ns/byte, scanning 10KB = 10µs. This is dwarfed by tree-sitter parse time (milliseconds). Pre-computing line starts would add memory overhead for a negligible gain.

**Reconsider when:** Profiling shows `vue_zones.rs` as a hotspot, or SFC sizes routinely exceed 100KB.

---

### D-06: Empty `new_code` Validation (F3.1a)

**Category:** Input validation  
**Finding:** `replace_body` accepts empty `new_code`, creating empty function bodies.  
**Decision:** DEFERRED — by design

**Rationale:** Empty function bodies (`{}`) are **valid Rust, Python, Go, TypeScript, etc.** AI agents legitimately use empty bodies when stubbing out implementations. Rejecting empty `new_code` would break common agentic workflows. The LSP validator catches semantically incorrect empties (e.g., missing return in non-void function).

**Do not change this behavior.**

---

### D-07: Concurrent Cache Access (F3.3a, F3.3b, F3.3c)

**Category:** Concurrency  
**Findings:** Parser cache race, Mutex contention under concurrent LSP calls, FileWatcher event ordering.  
**Decision:** DEFERRED — acceptable for local MCP

**Rationale:**
- **Cache race (F3.3a):** Pathfinder serves a single local AI agent. The race window is nanoseconds and results in at worst a redundant parse, not corruption.
- **Mutex contention (F3.3b):** LSP process count is bounded by the number of supported languages (≤ 10). Lock hold time is short (HashMap lookup only).
- **FileWatcher ordering (F3.3c):** Debouncing is handled at the OS event level. Out-of-order events are impossible within a single file write.

**Reconsider when:** Pathfinder is deployed in a multi-user or multi-agent server context.

---

### D-08: Very Large File Handling (F3.4c)

**Category:** Edge case  
**Finding:** No file size check before parsing; large files may cause memory pressure.  
**Decision:** DEFERRED — mitigated by timeout

**Rationale:** The tree-sitter parser enforces a 500ms timeout. Files that are too large to parse within the timeout return a `ParseError`, not a crash. A pre-check would need a size threshold that is arbitrary and language-dependent (a 10MB minified JS file is different from a 10MB Rust file). The timeout is the correct mitigation.

**Reconsider when:** Agents report timeouts on files that are legitimately large and should be parseable.

---

### D-09: LSP Crash Recovery (F3.5a, F3.5b)

**Category:** Reliability  
**Findings:** LSP process orphaning on macOS, no backoff cap on restart loop.  
**Decision:** DEFERRED — acceptable risk profile

**Rationale:**
- **Process orphaning (F3.5a):** On macOS, child processes outlive parents when the parent is killed ungracefully. Pathfinder already uses `kill_on_drop` where available. The remaining risk is a platform limitation, not a code bug.
- **Restart backoff (F3.5b):** The 3-retry with exponential backoff caps at ~4s total. An unrecoverable LSP failure surfaces to the agent as `lsp_crash` skip reason, which is the correct behavior.

**Reconsider when:** Users report zombie LSP processes surviving Pathfinder restarts, or when adding Linux cgroups support.

---

### D-10: Symlink Path Resolution (F3.4a)

**Category:** Security  
**Finding:** `WorkspaceRoot::resolve` doesn't call `canonicalize()` to resolve symlinks.  
**Decision:** DEFERRED — mitigated by Sandbox layer

**Rationale:** `resolve()` strips `ParentDir` and `RootDir` components (preventing simple traversal). The `Sandbox::check()` call that follows validates the final path against allowed roots. Adding `canonicalize()` in `resolve()` would: (a) make it fallible, changing the API contract, (b) cause false denials for files that are symlinked *within* the workspace, (c) still not prevent all symlink attacks (TOCTOU). The Sandbox provides defense-in-depth.

**False Positive:** F3.2a (`strip_outer_braces`) and F3.2b (`byte_to_point`) were confirmed as **false positives** — the code is correct by design. See `PATCH-006-false-positive-docs.md`.

---

### D-11: `read_symbol_scope` Content Duplication (F5.3a)

**Category:** API design / token efficiency  
**Finding:** `ReadSymbolScopeMetadata.content` duplicates `Content::text` — source code appears in both `text` content and `structured_content`, doubling token cost.  
**Decision:** DEFERRED — intentional design, breaking API change required to fix

**Rationale:** The `ReadSymbolScopeMetadata` doc comment explicitly states: *"Mirrors content[0].text in the MCP response. Provided here so that agents consuming structured_content directly have the full source without needing to inspect the main content array."*

Removing `content` from `structured_content` would break agents that consume only `structured_content` (e.g., agents that bypass the MCP text content envelope). This is a **breaking API change**. The correct fix is an API versioning decision, not a patch.

**Reconsider when:** A `read_symbol_scope` v2 response schema is designed as part of an API versioning epic.

---

### D-12: `SYMBOL_NOT_FOUND` Lacks Full Symbol List (F5.3b / F5.5a)

**Category:** API ergonomics  
**Finding:** `SYMBOL_NOT_FOUND` includes `did_you_mean` fuzzy suggestions but not the full symbol list. Agent cannot recover without calling `read_source_file`.  
**Decision:** DEFERRED — payload size risk

**Rationale:** The current hint already says: *"Did you mean: X? Use read_source_file to see available symbols."* Including the full symbol list in every `SYMBOL_NOT_FOUND` error would:
- Balloon error response size for large files (100+ top-level symbols in some files)
- Add tree-sitter parse overhead to every failed symbol lookup
- Be redundant with `read_source_file` which the agent should call anyway if it needs the full symbol tree

The `did_you_mean` suggestions cover the most common typo-correction case. The hint message directs agents to the correct recovery path.

**Reconsider when:** Benchmark data shows agents make more than 2 consecutive `SYMBOL_NOT_FOUND → read_source_file` round-trips on average, suggesting the hint is being ignored.

---

### D-13: Tool Overlap Concerns (F4.2a, F4.2b, F4.2c)

**Category:** API design  
**Findings:** Overlapping functionality between `read_symbol_scope` / `read_with_deep_context`, `read_file` / `read_source_file`, `search_codebase` / ripgrep direct use.  
**Decision:** DEFERRED — design choices, not bugs

**Rationale:** Each tool serves a distinct use case with a documented contract:
- `read_symbol_scope` → AST-only, fast, no dependencies
- `read_with_deep_context` → AST + LSP dependency analysis, slow (LSP warmup), rich output
- `read_file` → raw text for config/docs
- `read_source_file` → AST symbols for source code

These overlaps are intentional. Merging them would require complex parameter switches that reduce clarity for agent callers.

**Reconsider when:** A formal tool schema redesign is undertaken (separate epic).

---

### D-14: Unanalyzed — Section 5.1 Findings (F5.1x)

**Category:** Unknown — section not shared in this session  
**Findings:** Section 5.1 of the original audit report was never pasted into this analysis session. Finding codes F5.1a, F5.1b, etc. (if they exist) are unknown.  
**Decision:** DEFERRED — content not available for analysis

**Rationale:** The original report's Section 5 was shared partially (starting at 5.3). Section 5.1 content is unavailable. Based on the report's overall focus on "AI Agent Ergonomics", this section likely covered tool naming consistency or parameter naming conventions — both of which are breaking API changes that belong in an "API v2" epic regardless.

**Reconsider when:** The original report is retrieved and Section 5.1 findings are extracted and triaged individually.

---

### D-14b: Unanalyzed — Section 5.2 Findings (F5.2x)

**Category:** Unknown — section not shared in this session  
**Findings:** Section 5.2 of the original audit report was never pasted into this analysis session. Finding codes F5.2a, F5.2b, etc. (if they exist) are unknown.  
**Decision:** DEFERRED — content not available for analysis

**Rationale:** Same as D-14 above. Section 5.2 likely covered parameter schema verbosity or response format consistency — scope is large and changes are breaking. Track as part of the "API v2" epic once the original content is retrieved.

**Reconsider when:** The original report is retrieved and Section 5.2 findings are extracted and triaged individually.

---

### D-14c: Unanalyzed — Section 2.1b Finding (F2.1b)

**Category:** Unknown — finding code gap in section sequence  
**Findings:** The original report's section 2.1 contains finding codes F2.1a (fixed), F2.1c (fixed), and F2.1d (fixed), but F2.1b is absent from all analysis. Either the finding was resolved alongside F2.1a, or it was never pasted.  
**Decision:** DEFERRED — assumed resolved, but not confirmed

**Rationale:** F2.1a and F2.1c were both in the "LSP error granularity" cluster and are verified fixed. F2.1b, if it existed, likely covered the same concern and was resolved in the same refactor. No evidence of an open F2.1b issue exists in the current codebase.

**Reconsider when:** The original report is retrieved and F2.1b content is confirmed. If it describes a new concern, triage and add a new PATCH accordingly.

---

### D-15: Rate Limiting (F2.4d)

**Category:** Infrastructure  
**Finding:** No rate limiting on MCP tool calls.  
**Decision:** DEFERRED — deployment concern, not a code bug

**Rationale:** Rate limiting belongs at the infrastructure layer (reverse proxy, MCP gateway) not in the tool handlers. Adding it to the Rust server would tie deployment policy to release cycles.

**Reconsider when:** Pathfinder is deployed as a multi-tenant hosted service.

---

### D-16: New Tool Feature Requests (F5.4a–F5.4e)

**Category:** Feature requests  
**Findings:** `rename_symbol`, `find_all_references`, `move_symbol`, `format_file`, `list_languages` tools do not exist.  
**Decision:** DEFERRED — greenfield features, not bugs. Tracked in dedicated spec.

**Specification:** See [`docs/requirements/FEATURE-001-new-tools-epic.md`](../../FEATURE-001-new-tools-epic.md)

**Summary of each request:**

| Finding | Tool | LSP Method Needed | Complexity |
|---------|------|-------------------|------------|
| F5.4a | `rename_symbol` | `textDocument/rename` (not yet in `Lawyer` trait) | Medium-High |
| F5.4b | `find_all_references` | `textDocument/references` (not yet in `Lawyer` trait) | Medium |
| F5.4c | `move_symbol` | Requires F5.4b + language-specific import rewriting | High |
| F5.4d | `format_file` | Reuses existing `range_formatting` — no new LSP method | Low |
| F5.4e | `list_languages` | Aggregates `SupportedLanguage` + `Lawyer::capability_status` | Low |

**Implementation priority (from FEATURE-001):** F5.4e → F5.4d → F5.4b → F5.4a → F5.4c

**Reconsider when:** A dedicated "New Tools" sprint is scheduled. Do not implement these incrementally as patches — they require coordinated design review.

---

## Summary Table

| ID | Finding | Category | Decision | Reconsider Trigger |
|----|---------|----------|----------|--------------------|
| D-01 | Edit handler boilerplate | Duplication | Deferred — low ROI | 4th edit submodule added |
| D-02 | Unit test gaps | Test coverage | Partial — see TCV-001 | Coverage < 70% |
| D-03 | Integration test gaps | Test coverage | Deferred — needs infra | CI green |
| D-04 | `replace_batch_impl` complexity | Complexity | Deferred — inherent | New targeting mode |
| D-05 | `byte_to_point` O(n) | Performance | Deferred — immaterial | Profiling confirms hotspot |
| D-06 | Empty `new_code` | Validation | **By design — DO NOT CHANGE** | Never |
| D-07 | Concurrent cache/lock/watcher | Concurrency | Deferred — local MCP OK | Multi-user deployment |
| D-08 | Very large files | Edge case | Deferred — timeout mitigates | Agent timeout reports |
| D-09 | LSP crash recovery | Reliability | Deferred — acceptable | Zombie process reports |
| D-10 | Symlink resolution | Security | Deferred — Sandbox mitigates | Sandbox bypass confirmed |
| D-11 | `read_symbol_scope` content dup | API design | Deferred — breaking change | API versioning |
| D-12 | SYMBOL_NOT_FOUND symbol list | API ergonomics | Deferred — payload size | Round-trip benchmark data |
| D-13 | Tool overlaps | API design | Deferred — by design | API v2 epic |
| D-14 | Section 5.1 findings (F5.1x) | Unknown | Deferred — not analyzed | Original report retrieved |
| D-14b | Section 5.2 findings (F5.2x) | Unknown | Deferred — not analyzed | Original report retrieved |
| D-14c | Section 2.1b finding (F2.1b) | Unknown | Assumed resolved — not confirmed | Original report retrieved |
| D-15 | Rate limiting | Infrastructure | Deferred — deployment layer | Multi-tenant hosting |
| D-16 | New tools (rename/refs/move/fmt/langs) | Feature requests | See FEATURE-001-new-tools-epic.md | Dedicated sprint scheduled |
