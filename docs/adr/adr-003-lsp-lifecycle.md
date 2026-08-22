# 003. LSP Process Lifecycle: Eager Start, Probe-Gated Readiness, Host-Bound Lifetime

**Date:** 2026-08-22
**Status:** Accepted
**Related:** `docs/requirements/pathfinder-v6/00-plan.md`, scope card WS-3, findings `LSP-A1` `LSP-A2` `LSP-A3` `LSP-A6` `LSP-B0` `LSP-B1`

## Context

Pathfinder's value rests on two measured properties: the agent stops re-asking, and it is right about which implementation a symbol resolves to. Both are LSP-derived. Neither was reliable, because the LSP layer could return an empty array meaning "still indexing" and every caller read it as "confirmed zero."

Four defects produced this, none of which was lazy initialization (the code has started eagerly since PATCH-004; only the doc comments still claimed otherwise):

1. `warm_start_complete` meant "handshake returned", not "answers are correct." rust-analyzer returns `initialize` in ~200ms and is useless for 30s–3min afterwards. The flag's per-language error is three orders of magnitude.
2. No readiness gate on the request path. A warming server returns `[]`, not an error, so empty-during-indexing is byte-identical to confirmed-zero.
3. Compensation logic was duplicated per tool and mutually inconsistent — `deep_context.rs:85-88` and `impact.rs:847-868` draw **opposite** conclusions from the same `goto_definition` probe.
4. A 15-minute idle timeout silently killed processes; warm-start failures were logged and swallowed.

The `health` tool existed so an agent could detect all four manually. That is a diagnostic compensating for an unreliable primitive.

## Options Considered

### Option A: Keep `health`, improve its heuristics
- **Pros:** no change to the request path; lowest risk
- **Cons:** the agent must ask before every call to be safe, which no agent does; the tool is 4,950 LOC; the underlying lie remains
- **Effort:** Low

### Option B: Block the request until ready, unbounded
- **Pros:** answers always correct
- **Cons:** a cold jdtls can take minutes; an unbounded block is indistinguishable from a hang
- **Effort:** Low

### Option C: Probe-gated readiness + bounded blocking + typed `not_ready`
- **Pros:** empty is never overloaded; the agent gets a real answer or an honest, retryable state; `health` becomes unnecessary and deletes with ~4,950 LOC
- **Cons:** first call on a cold server blocks; needs a behavioural probe per capability class; needs document coherence as a prerequisite
- **Effort:** High

### Option D: Option C, but keep the idle timeout
- **Pros:** bounded memory
- **Cons:** optimises RAM (cheap) against correctness and first-call latency (expensive). A parked rust-analyzer costs a few hundred MB; a cold one costs a wrong answer or a 90s stall at an arbitrary future moment. Also, the idle-kill was the **only** thing bounding document staleness.
- **Effort:** High

## Decision

**Option C.** Specifically:

1. **Eager start stays**, and the doc comments are corrected to say so.
2. **Readiness is probe-gated.** A language is `Ready` only after a behavioural probe — a real navigation query at a real symbol position — succeeds. Capability *declaration*, static or dynamic, never gates readiness. This makes the static-vs-dynamic registration distinction unobservable, which is the only way it is reliable across servers we have not tested.
3. **Readiness is per-capability.** `goto_definition` on an open file is answerable from the file-local index; call hierarchy and references need the global index. One boolean is a lie.
4. **Requests block with a budget** (default 10s). On expiry the tool returns a typed `not_ready` with `retry_after_secs`, never an empty result set.
5. **The idle timeout is removed entirely.** Pathfinder's process lifetime is the MCP host's process lifetime. When the host exits, Pathfinder exits and tears down every child.
6. **`health` is deleted.** Its one non-readiness residual — "a language was detected but no binary is on PATH" — is configuration, emitted to the startup log plus a one-line notice on the first response of a session.

## Consequences

### Positive
- Empty means zero. The `null` vs `[]` distinction the project already documents becomes true in the implementation.
- ~4,950 LOC and one tool disappear together with the defect they compensated for.
- Cross-server reliability improves without per-server work, because the probe replaces capability negotiation as the source of truth.

### Negative
- The first call against a cold rust-analyzer or jdtls blocks up to the budget. Intended (INV-3), but a real first-call latency regression.
- Memory: one LSP process per detected language, held for the session. On a 5-language monorepo, the sum of those servers' resident memory. Users needing the old behaviour restart the host. Must be in the README.

### Risks

**R1 — Deleting `idle_timeout_task` destroys shutdown.** That task is also the shutdown handler and the only caller of `reap_dead_processes` (`LSP-B0`). Mitigation: rename it `process_supervisor_task`, delete only the idle reaping. Guarded by AC-3.4.

**R2 — Removing idle-kill removes the only document-state invalidation.** No `didChange` anywhere, the version counter is dead, no file watcher (`LSP-A3`). A bounded 15-minute staleness window becomes unbounded. Mitigation: document coherence (WS-3.4) is a **hard prerequisite**. Guarded by AC-3.7.

**R3 — Certifying answers to wrongly-positioned queries.** The gate stamps results `verified`; if Tree-sitter hands the LSP a bad position, a warm server correctly returns `[]` and we certify it (`TS-D6`). Mitigation: WS-7a lands first, or `Verified` is gated behind WS-7a's conformance assertions. Guarded by C-16.

**R4 — Unbounded respawn-and-reprobe.** A server that starts then dies produces a 60s-backoff / 120s-probe treadmill. Mitigation: N consecutive probe failures makes `Degraded` sticky.

**R5 — Probe behind the init lock serialises cold start.** Mitigation: probe runs once, outside the lock; waiters observe via `tokio::sync::watch`.

## Related
- `docs/requirements/pathfinder-v6/03-contracts.md` — Contract 1 (`ToolStatus`)
- Rugged Software Constitution: "instrument for awareness", "design for failure"
- Error Handling Principles: "never fail silently"
