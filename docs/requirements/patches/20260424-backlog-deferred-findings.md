# Deferred Findings Backlog

**Source:** April 2026 codebase audit  
**Status:** Triaged as real issues but deferred — not scheduled for implementation.  
**Process:** These should be re-evaluated before starting a new Epic or when symptoms become observable in production.

---

## G1-F2 — AST Cache Stampede

**Location:** `crates/pathfinder-treesitter/src/cache.rs`, `get_or_parse` (lines 56–60)

**Description:**  
Concurrent requests for the same file can race through the slow path (filesystem read + Tree-sitter parse) simultaneously because the mutex lock is dropped before the `await`. Multiple callers end up parsing the same file redundantly, then all racing to insert into the cache.

**Why deferred:**  
Pathfinder is a local, single-user MCP server. The stampede requires multiple concurrent requests for the *same* file simultaneously, which is unlikely in practice. The code already has a comment acknowledging this as an accepted v1 trade-off. The existing code is also correct and safe — just potentially wasteful under rare concurrent load.

**When to revisit:**  
- If Pathfinder becomes multi-tenant (shared server, multiple simultaneous users).  
- If profiling shows redundant parse work under heavy `search_codebase` load.

**Fix approach (when ready):**  
Replace the per-entry mutex approach with an `Arc<tokio::sync::OnceCell<CacheEntry>>` or a `DashMap` with `entry()` API to implement a single-flight pattern, ensuring only one parse is inflight per file path at any given time.

---

## G1-F6 — Vue SFC UTF-16 Column Drift

**Location:** `crates/pathfinder-treesitter/src/language.rs`, `extract_vue_script` (lines 180–186)

**Description:**  
`extract_vue_script` pads non-script zones with ASCII spaces (byte value `0x20`) to preserve byte offsets for Tree-sitter. When the Vue SFC template contains multi-byte UTF-8 characters, the space padding counts as fewer UTF-16 code units than the original characters. LSP diagnostics, which use UTF-16 columns, will drift when pointing into the padded zone.

**Why deferred:**  
This function is the **legacy single-zone Vue parser** path. The production pipeline for Vue tools uses `parse_vue_multizone` in `vue_zones.rs`, which does proper zone-based parsing with `VueZoneRange` offsets. LSP diagnostics target the script zone (not template), making this a very narrow edge case in practice.

**When to revisit:**  
- If Vue SFC support is extended to provide LSP diagnostics in template zones.
- If a user reports incorrect diagnostic line/column numbers in Vue files containing CJK or emoji in templates.

**Fix approach (when ready):**  
Replace `extract_vue_script` with multi-zone parsing across all code paths, or pad with UTF-16-equivalent byte sequences when replacing non-script content.

---

## G1-F10 — Optional `context_line` for Unique Text Edits

**Location:** `crates/pathfinder/src/server/tools/edit.rs`, `resolve_single_batch_edit` (lines 694–703)

**Description:**  
When using text-range targeting (`old_text`) in `replace_batch`, `context_line` is strictly required. If `old_text` appears exactly once in the file, `context_line` could be inferred automatically via a full-file scan, reducing agent friction.

**Why deferred:**  
The strict requirement is a deliberate safety guardrail. Making it optional introduces risk: (1) a full-file scan is needed to determine uniqueness, (2) if the text appears twice, the error message becomes more confusing than the current clear "context_line is required" error, (3) the ±25-line window is a useful scoping mechanism in its own right. The ergonomic gain does not outweigh the added complexity and failure modes.

**When to revisit:**  
- If agent ergonomics telemetry shows a high rate of `context_line` omission errors.
- If a clear fallback strategy with unambiguous error messages is designed.

**Fix approach (when ready):**  
If `context_line` is absent: scan the file for occurrences of `old_text`. If exactly one match, use its line as `context_line` and proceed. If zero or multiple, return a structured error with the line numbers of all occurrences.

---

## G1-F12 — Diff-Based Error Feedback in `VALIDATION_FAILED`

**Location:** `crates/pathfinder/src/server/tools/edit.rs`, `build_validation_outcome`

**Description:**  
When an edit introduces new LSP errors and fails validation, the response includes a list of introduced diagnostics (message + line number). Adding a unified diff of the attempted edit alongside the diagnostics would allow the agent to understand exactly what changed and correlate the diff line numbers with the diagnostic line numbers.

**Why deferred:**  
The current error response already contains sufficient information for most cases. Adding a diff increases response size and serialization complexity. The agent can reconstruct the change from context.

**When to revisit:**  
- If agent auto-correction success rates from validation errors are measured to be low.
- If the diff can be generated cheaply (e.g., reusing the already-computed `new_bytes` in `finalize_edit`).

**Fix approach (when ready):**  
In `finalize_edit`, when `should_block = true`, generate a unified diff between `source` and `new_bytes` using the `similar` crate and embed it in the `EditValidation.diff` field (optional string).

---

## G2-F2 — `get_definition` Usability Friction

**Location:** `crates/pathfinder/src/server/tools/definition.rs` (or equivalent)

**Description:**  
`get_definition` requires a precise semantic path including file path and symbol chain. Agents often attempt bare symbol names or partial paths, receiving unhelpful errors. The tool could attempt a fuzzy resolution step when the exact semantic path fails.

**Why deferred:**  
The existing `did_you_mean` infrastructure in `symbols.rs` already provides fuzzy match suggestions when a semantic path fails resolution. Auto-resolving a fuzzy match would risk silently targeting the wrong symbol, which is a worse failure mode than explicit user friction. The correct fix is documentation, not silent fuzzy resolution.

**When to revisit:**  
- If agent usage telemetry shows a high rate of failed `get_definition` calls due to path errors.
- If a two-step approach is implemented: attempt exact resolution, if it fails surface the `did_you_mean` suggestions prominently.

**Fix approach (when ready):**  
Improve the error message to include the closest `did_you_mean` suggestion and a concrete example of the correct semantic path format, derived from the actual file structure.
