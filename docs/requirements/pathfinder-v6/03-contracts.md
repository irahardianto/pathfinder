# Pathfinder v6 — Frozen Contracts

**Status:** FROZEN for BUILD. Read-only during implementation.
**Owner:** WS-4 (Response Contract) produces these types. Every other workstream consumes them.
**Change protocol:** a BUILD agent that believes a contract needs changing **STOPS and escalates**. It does not modify these types unilaterally. See `parallel-dispatch` skill, "Contracts Layer".

---

## Why this document exists

`RSP-D5` found that the response contract spans five workstreams with disjoint file ownership, so under MECE dispatch it gets split by file and therefore split by concern — and nobody owns *"is the response honest."*

These contracts are the answer. Specified once, in full, before any workstream starts, so every scope card builds against the same shape.

All of them serve one invariant:

> **INV-1 — No overloaded empty.** No tool may return an empty result set when the underlying engine was not verified capable of producing a non-empty one. Not-ready, unavailable, and degraded are distinct, typed, machine-readable states — never an empty array, never a bare `0`.

---

# CONTRACT 1 — `ToolStatus`: the readiness / provenance discriminator

Addresses: `LSP-A1`, `LSP-A2`, `LSP-A6`, `LSP-C1`, `LSP-C5`, `RSP-A1`, `RSP-A2`, `RSP-A5`, `RSP-A6`, `RSP-A8`, `RSP-A9`, `RSP-C1`.

`RSP-C1` is the source specification for this contract — the audit wrote it in full because the draft introduced a `Readiness` enum and never said what the agent receives. Decisions 1–5 below are that specification.

## Decision 1 — `not_ready` is a SUCCESSFUL `CallToolResult` with a status discriminator, NOT an MCP error

**(a) The existing error path is actively hostile to retry.** `pathfinder_to_error_data` (`helpers.rs:43-66`) maps `NoLspAvailable` and `LspTimeout` to `ErrorCode::INTERNAL_ERROR` (`-32603`). JSON-RPC `-32603` means "the server broke". Host harnesses and model post-training both treat it as terminal-and-not-my-fault; the learned behaviour is to abandon the tool, not retry it. Encoding "wait 8 seconds and ask again" as `-32603` is the worst available encoding.

**(b) `not_ready` must carry a payload** — elapsed, eta, phase, optionally heuristic candidates. `ErrorData.data` can hold JSON, but the text channel of an MCP error is a single message string; there is no structured `content` array on the error path (`helpers.rs:95`).

**(c) INV-1 says not-ready is "a distinct, typed, machine-readable state."** A state is a value. Errors are for "your request was invalid" and "I am broken." Neither applies.

**(d) Precedent already in the codebase.** Every degraded path today returns `CallToolResult::success` with `degraded: true` (`references.rs:461`, `impact.rs:1687`, `definition.rs:561`). Switching the *most common* transient case to the error channel makes the failure mode non-uniform.

**MCP errors remain reserved for terminal, input-fixing conditions:** malformed semantic path (`INVALID_PARAMS`), sandbox denial (`-32001`), file not found (`INVALID_PARAMS`).

## Decision 2 — one `status` field, on EVERY response from all three tools, never skipped

Replaces the current three-way muddle of `degraded: bool` + `degraded_reason` (13-variant enum) + `lsp_readiness` (free-form `Option<String>` produced by three copy-pasted functions: `impact.rs:1069-1091`, `deep_context.rs:535-551`, `overview.rs:357-374`).

```rust
/// Present on EVERY locate/trace/inspect response. Never skipped. Never absent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolStatus {
    /// Behavioural probe passed. Every result in this response is LSP-verified.
    Ok,

    /// The language server for this file is still becoming usable.
    /// Results, if any, are heuristic. RETRY THIS EXACT CALL.
    NotReady {
        /// Seconds already spent probing. Monotonic across retries.
        elapsed_secs: u32,
        /// Wall-clock seconds to wait before reissuing. ALWAYS present, never 0.
        /// The caller may sleep exactly this long and retry verbatim.
        retry_after_secs: u32,
        /// Best-known total. `null` when the server reports no progress.
        /// `null` means UNKNOWN -- it does NOT mean "soon".
        eta_secs: Option<u32>,
        /// "spawning" | "handshaking" | "indexing" | "probing"
        phase: String,
        /// Times this exact call has been answered NotReady, echoed from the
        /// client's `retry_attempt` param. Lets the agent bound its own loop.
        attempt: u32,
        /// Hard ceiling. At `attempt >= max_attempts` the next response is
        /// guaranteed Ok or Degraded, never NotReady. Default 3.
        max_attempts: u32,
    },

    /// No language server binary for this language. TERMINAL. Do not retry.
    Unavailable {
        language: String,
        install_hint: String,
        /// What to do instead. MUST name only tools that exist.
        alternative: String,
    },

    /// Server running but failed its behavioural probe within budget.
    /// Results, if any, are heuristic. Retrying is unlikely to help.
    Degraded {
        reason: String,
        retry_after_secs: Option<u32>,
    },
}
```

Request parameter — add to `LocateParams`, `TraceParams`, `InspectParams`:

```rust
    /// Echo back the `attempt` from a previous NotReady response. Default 0.
    /// The server uses this to enforce `max_attempts` and to escalate to
    /// Degraded rather than looping forever.
    #[serde(default)]
    pub retry_attempt: u32,
```

## Decision 3 — the five mechanisms that make an agent RETRY rather than give up or route around

All five required. Dropping any one produces either an abandoned tool or an unbounded loop.

**1. `retry_after_secs` is ALWAYS a concrete integer.** Never null, never a range, never prose. The existing `ActionableGuidance.retry_after_seconds` is `Option<u32>` (`common/types.rs:650-651`) and is `None` for four of thirteen reasons (`:678`, `:706`, `:713`, `:720`). An agent handed `retry_recommended: true, retry_after_seconds: null` has no defined behaviour.

**2. It is a SLEEP-AND-REISSUE contract, stated imperatively with the literal parameters.** Models retry when told the exact next action; they abandon when told "results may improve later."

**3. `attempt` / `max_attempts` bound the loop.** Without a ceiling the agent either loops forever or — more commonly — refuses to loop at all because it cannot prove termination. With `max_attempts: 3` visible, retrying is provably bounded and therefore safe. **The server MUST honour it.**

**4. `NotReady` MUST NOT carry a plausible-looking result list.** Heuristic candidates under `NotReady` are **opt-in per request and OFF by default**. A `NotReady` response containing 12 grep candidates is one the agent will consume and never retry — the candidates *are* the answer as far as the model is concerned. If included they go in `heuristic_candidates`, **never** `incoming`/`outgoing`/`references`/`dependencies`. **The single most important detail in this contract.**

**5. Guidance must NOT name a fallback tool under `NotReady`.** `ActionableGuidance` currently sets `fallback_tool: Some(Search)` for `LspWarmupGrepFallback` and `LspTimeoutGrepFallback` (`common/types.rs:694`). Telling the agent "retry in 30s" and "or use search" in the same breath guarantees it does the second. Under `NotReady`: `fallback_tool: None`, unconditionally.

## Decision 4 — exact text-channel wording

The status line MUST be the **first line of the first content block**, and MUST be **generated from the `ToolStatus` value by a single function in `helpers.rs`** — never hand-written per call site. (`format_degraded_notice`, `helpers.rs:121-149`, is already that function for the degraded case; the defect is five handlers hand-writing status prose *around* it — `references.rs:414-418`, `:547-553`, `:766`, `:780`; `impact.rs:1236-1252`, `:1254-1262`. Those must go.)

### `NotReady`

```
NOT_READY — rust-analyzer is indexing (phase=indexing, 8s elapsed, ~25s total).
No results are returned because an empty result here would be indistinguishable
from a verified zero.
ACTION: wait 8 seconds, then reissue this exact call with retry_attempt=1.
This call will return a real answer or a typed failure by attempt 3 of 3.
Do not substitute grep — it cannot answer "which implementation".
```

Rules: `NOT_READY` is the first token, uppercase, greppable. Numbers are digits. The ACTION sentence is imperative and names the parameter. The last line exists because it is the only thing that stops a capable model from "helpfully" routing around the wait — **and routing around the wait is exactly the behaviour INV-3 is buying out.**

### `Unavailable`

```
UNAVAILABLE — no language server for python on PATH. This will not change on retry.
INSTALL: pipx install pyright
Until then, locate/trace/inspect cannot verify Python symbols.
Use your host's grep and file-read tools for Python.
```

### `Degraded`

```
DEGRADED (probe_failed_within_budget: rust-analyzer did not resolve a known
definition within 120s) — 4 heuristic candidates below. Each may be a false
positive: same-named symbols, dynamic dispatch, or matches in comments.
Do NOT conclude "zero callers" from this response.
```

### `Ok`

`OK` prefix, then the answer. No apology, no hedge.

## Decision 5 — behaviour table

| `ensure_ready` outcome | tool behaviour |
|---|---|
| `Ready` | issue the LSP request; results are `Confidence::Verified` |
| `NotReady` | return `status: not_ready`. **Never** an empty result set. Include all six fields. Heuristic candidates only if explicitly requested, only in `heuristic_candidates`. |
| `Unavailable` | `status: unavailable` + install hint + alternative. Ripgrep fallback permitted, every entry `Confidence::Heuristic`. |
| `Degraded` | `status: degraded` + reason. Ripgrep fallback permitted, every entry `Confidence::Heuristic`. |

Default readiness budget 10s, configurable as `lsp.readiness_budget_secs`. The first call on a cold rust-analyzer **will block**. That is the intended trade (INV-3).

---

# CONTRACT 2 — `Confidence` and `Evidence<T>`: the heuristic / verified boundary

Addresses: `LSP-C7`, `RSP-A6`, `RSP-A7`, `RSP-A8`, `RSP-A9`, `RSP-B3`, `RSP-C2`, `TS-D6`.

`RSP-C2` is the source specification — the audit wrote it in full because the original plan stated the rule ("every ripgrep result labelled heuristic") and specified nothing enforceable. Rules 1–7 are that specification.

## Rule 1 — Confidence is a per-entry, non-optional, typed field

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Produced by an LSP request against a language whose behavioural probe passed.
    Verified,
    /// Produced by ripgrep and/or Tree-sitter pattern matching. A hypothesis.
    Heuristic,
}
```

Add `pub confidence: Confidence` — **required**, no `Option`, no `skip_serializing_if` — to:

| type | location | today |
|---|---|---|
| `ReferenceLocation` | `server/types.rs:729` | absent |
| `DeepContextDependency` | `server/types.rs:425` | absent |
| `SymbolOverviewImpactEntry` | `server/types.rs:84` | absent |
| `ImpactReference` | `server/types.rs:532` | `Option<String>` at `:558-559` — tighten to the enum, drop `Option` and the skip |
| `LocateResultEntry` | `server/types.rs:1380-1407` | absent (`RSP-A6`) |
| `InspectResultEntry` | `server/types.rs:1341-1364` | absent (`RSP-A6`) |

Non-optional is the point. `Option<String>` with `skip_serializing_if` means the default emitted state is "field absent", and absent is what an agent reads as "fine". A required enum makes it **impossible to construct a result item without deciding**.

## Rule 2 — Homogeneity is a type invariant, not a convention

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Evidence<T> {
    pub confidence: Confidence,
    pub items: Vec<T>,
}
```

`incoming`, `outgoing`, `references`, `dependencies` become `Option<Evidence<T>>`:

```
None                                               = UNKNOWN
Some(Evidence{ confidence: Verified,  items: [] }) = CONFIRMED ZERO
Some(Evidence{ confidence: Verified,  items: v  }) = verified results
Some(Evidence{ confidence: Heuristic, items: v  }) = candidates, unverified
Some(Evidence{ confidence: Heuristic, items: [] }) = FORBIDDEN
```

**A heuristic that found nothing is UNKNOWN — encode it as `None`.** That single rule kills `RSP-A8` by construction.

## Rule 3 — `verified` is derived, never independently assigned

Delete `incoming_verified` / `outgoing_verified` (`server/types.rs:591`, `:596`) and `compute_verified_flags` (`impact.rs:1137-1153`). They exist only because confidence was not carried on the data.

```
verified == (evidence.confidence == Confidence::Verified)
```

One source of truth. Also resolves the doc/code contradiction at `server/types.rs:582-596` (`RSP-A8`) by deleting both sides of it.

## Rule 4 — Response-level status gates entry-level confidence

```
status: Ok        + any item with confidence: Heuristic     -> ILLEGAL
status: NotReady  + any populated evidence field            -> ILLEGAL (Contract 1, D3.4)
status: Degraded  + any item with confidence: Verified      -> ILLEGAL
Evidence{ confidence: Heuristic, items: [] }                -> ILLEGAL (use None)
```

Enforce with `debug_assert!` inside a single `validate_response()` called from **every** handler immediately before `CallToolResult::success`, plus a release-mode `tracing::error!`. Plus a property test over the four handlers.

## Rule 5 — Heuristic items must be triageable

Per `RSP-D1`: an entry the agent cannot evaluate is **worse** than no entry, because it will be believed.

- Every `Heuristic` item MUST carry the raw matched line (`snippet`) and, where Tree-sitter enrichment succeeded, the enclosing `semantic_path`.
- `Verified` items need neither — for LSP entries `snippet` is `CallHierarchyItem.detail` (`impact.rs:454-457`), not source, and `semantic_path` already carries the module.

**This is the explicit exception to the payload diet and it is not negotiable** — it is the mechanism protecting the convergence property.

## Rule 6 — The text channel labels confidence per section, not per response

```
Callers: 3 VERIFIED
  crates/.../lawyer_impl.rs:18  LspClient.references
Callees: UNKNOWN (not verified — do NOT treat as zero)
```

```
Callers: 4 CANDIDATES (heuristic — grep, may include false positives)
  crates/.../mod.rs:88   > self.lawyer.references(path, line, col).await
```

Rendering MUST be a **total function of the tri-state, never of a count**:

```
None                            -> "Incoming callers: UNKNOWN (not verified — do NOT treat as zero)"
Some(Evidence{Verified, []})    -> "Incoming callers: 0 (verified zero)"
Some(Evidence{Verified, v})     -> "Incoming callers: {n} (verified)"
Some(Evidence{Heuristic, v})    -> "Incoming callers: {n} CANDIDATES (heuristic, unverified)"
```

**Never print a bare integer for a `None`.** Direct fix for `RSP-A1`, the highest-blast-radius finding in the register.

## Rule 7 — `skip_serializing_if` is banned on tri-state fields

Remove from `server/types.rs:26`, `:71`, `:76` (`RSP-A9`). Enforce with a schema test: **no field whose Rust type is `Option<Vec<_>>` or `Option<Evidence<_>>` may carry `skip_serializing_if`.**

---

# CONTRACT 3 — Dual-channel consistency

Addresses: `RSP-A1`, `RSP-A6`, `RSP-A9`.

MCP returns both `structured_content` and a text channel. Different models attend to different channels. **Any disagreement is a reliability bug.**

1. **Single renderer.** Both channels derive from the same in-memory response value, via functions in `helpers.rs`. No handler hand-writes status prose.
2. **Non-overlapping, not duplicated** (per `RSP-D1` cut 2): **text** owns the reference/dependency list, one fact per line — this is the agent-facing answer. **JSON** owns counts, `status`, `confidence`, truncation, pagination, retry parameters. It carries the list only when explicitly requested (`structured_only=true`).
3. **Every field carrying certainty must appear in the text channel** — `status`, per-list `confidence`, and `hint`. These are **additions**, and they must survive the payload diet.
4. **The word "authoritative" may only be emitted when every sub-call returned `Ready`.** `overview.rs:422` currently emits `"DEGRADED: no (LSP-backed, authoritative)"` whenever `!degraded`, which combines with `RSP-A2` to produce the worst output in the system.

## Acceptance tests

- `UNKNOWN` appears in the text channel for **every** response where an evidence field is `None`.
- `references: 0` / `callers: 0` **never** appears when the corresponding evidence field is `None`.
- The text channel's first token is one of `OK | NOT_READY | UNAVAILABLE | DEGRADED`, for every handler.
- No response with `status: not_ready` populates `incoming`, `outgoing`, `references`, or `dependencies`.
- Each of the four Contract 2 Rule 4 illegal combinations panics in debug.

---

# CONTRACT 4 — Guidance may only name tools that exist

Addresses: `RSP-A5`. Cheapest fix in the register, largest convergence protection.

1. Replace `FallbackTool::{Search, Read}` (`common/types.rs:583-588`) with `FallbackTool::{Locate, Trace, Inspect, HostGrep}`.
2. `HostGrep` renders as **"use your host's grep/read tools"**. Pathfinder must not pretend to own a text-search tool it deleted.
3. Sweep all free-text guidance strings — the 13 known sites are enumerated in `RSP-A5`.
4. Under `ToolStatus::NotReady`, `fallback_tool` is `None` unconditionally.

## Acceptance

```bash
rg -n '\b(search|explore|read|lsp_health|health)\(' \
   crates/pathfinder/src crates/pathfinder-common/src --glob '!*_test.rs'
```

must return zero hits **inside string literals emitted to an agent**. Run as part of the schema regression test.

---

# CONTRACT 5 — Pagination and truncation

Addresses: `RSP-D1` tail.

`impact.rs:1543` computes `references_truncated` but `FindCallersCalleesMetadata` has **no `next_offset`** (contrast `SearchCodebaseResponse.next_offset`, `server/types.rs:169`), and `TraceParams.offset` is documented as *"Applies to `scope=\"references\"` only; ignored for `callers`"* (`server.rs:359`).

**A truncated `callers` result today has no defined continuation. That is a guaranteed re-ask**, and cutting `max_references` to 20 makes truncation more common.

1. Add `next_offset: Option<u32>` to `FindCallersCalleesMetadata`.
2. Honour `TraceParams.offset` for `scope=callers`.
3. Any response with `*_truncated: true` MUST carry a usable `next_offset`.
4. The text channel states it: `"Showing 20 of 47. Reissue with offset=20 for the rest."`

## Acceptance

For every response where any `*_truncated` flag is true, `next_offset` is `Some`, and reissuing with that offset returns the next disjoint page.
