# 004. Response Contract: Typed Status and Per-Entry Provenance

**Date:** 2026-08-22
**Status:** Accepted
**Related:** `docs/requirements/pathfinder-v6/03-contracts.md`, scope card WS-4, findings `RSP-A1` `RSP-A5` `RSP-A6` `RSP-A7` `RSP-A8` `RSP-A9` `RSP-D5`

## Context

An audit of the response layer found that the honesty of a Pathfinder response is not a property anyone owns. It is distributed across five crates and modules, and fails in ways individually small and jointly fatal:

- The **text channel renders `null` as `0`.** `impact.rs:1515-1516` uses `map_or(0, Vec::len)` and prints `Incoming references: 0` for a response whose JSON says `incoming: null, incoming_verified: false`. The three fields carrying UNKNOWN are JSON-only. Models reading the text channel — the default for most MCP hosts — get a confident zero.
- **Three of four result item types carry no provenance.** Only `ImpactReference` has `confidence`. `ReferenceLocation`, `DeepContextDependency` and `SymbolOverviewImpactEntry` do not, so a grep guess and an LSP fact are structurally indistinguishable in `trace(scope=references)`, in `inspect` dependencies, and in `overview` — which actively *discards* the upstream confidence it was given.
- **`skip_serializing_if` on tri-state fields** encodes UNKNOWN as an absent key. Absence is indistinguishable from an older server, a schema migration, or a proxy stripping empty keys. `null` is a value the model must read; absence is something it must notice is not there.
- **`Some([])` co-exists with `verified: false`** in the same object, and the schema doc comments contradict the implementation — and those comments become the JSON-Schema `description` the agent reads.
- **Degraded guidance names tools being deleted.** 13 sites tell the agent to call `search`, `read`, `explore` or `lsp_health`. Each is a guaranteed extra turn, attacking convergence — the one property the benchmark validated.

Under MECE parallel dispatch this work gets split by file and therefore split by concern. No workstream would own *"is the response honest."*

## Options Considered

### Option A: Fix each defect inside the workstream owning the file
- **Pros:** no new workstream; respects existing ownership
- **Cons:** no single owner; every finding needs edits in two or more workstreams; schema, renderer and emit sites drift apart again
- **Effort:** Medium

### Option B: Encode `not_ready` as an MCP error
- **Pros:** clean separation of success and non-success
- **Cons:** maps to `-32603` INTERNAL_ERROR, which models treat as terminal; the error path has no structured `content` array, so heuristic candidates and retry parameters cannot travel; makes the most common transient case the only one on a different channel
- **Effort:** Low

### Option C: A dedicated response-contract workstream owning typed status and per-entry provenance end-to-end
- **Pros:** one owner for honesty; the contract is frozen before any workstream builds against it; illegal states become unrepresentable rather than merely undocumented
- **Cons:** a new workstream; requires splitting `server/types.rs` ownership by type category
- **Effort:** High

## Decision

**Option C**, with five frozen contracts specified before BUILD begins.

1. **`ToolStatus`** — one required, never-skipped discriminator on every response: `Ok | NotReady | Unavailable | Degraded`. `not_ready` is a **successful** result with a status field, not an MCP error. It carries `elapsed_secs`, a **never-null** `retry_after_secs`, `eta_secs`, `phase`, and `attempt`/`max_attempts` so the retry loop is provably bounded.
2. **`Confidence` and `Evidence<T>`** — provenance is a required, non-optional, per-entry field on all six result item types. `Evidence<T>` makes homogeneity a type invariant. `Some(Evidence{Heuristic, []})` is forbidden — a heuristic that found nothing is UNKNOWN, encoded as `None`.
3. **Dual-channel consistency** — both channels derive from one renderer. Text rendering is a total function of the tri-state, never of a count. `skip_serializing_if` banned on tri-state fields.
4. **Guidance names only tools that exist.**
5. **Truncation implies a usable continuation.**

Two corollaries worth stating explicitly:

- **`verified` is derived, never assigned.** `incoming_verified`/`outgoing_verified` and `compute_verified_flags` are deleted. `verified == (confidence == Verified)`.
- **A `not_ready` response may not populate any field an agent reads for "the answer."** Heuristic candidates under `not_ready` are opt-in, off by default, in a separately-named field. A `not_ready` response containing 12 grep candidates is one the agent will consume and never retry.

## Consequences

### Positive
- The `null` vs `[]` distinction holds in both channels, not just JSON.
- Illegal combinations become debug-assertion failures rather than silent wrong answers.
- Retry becomes a mechanism the agent can execute rather than advice it must interpret.
- Deleting `incoming_verified`/`outgoing_verified` also deletes a doc/code contradiction served to the agent as schema documentation.

### Negative
- `server/types.rs` is shared. Ownership splits by type category — parameters to the schema workstream, responses here — enforced by review rather than the filesystem. The one MECE compromise in the plan, and it is deliberate.
- Making `confidence` non-optional is a breaking change to every construction site. That is the point.

### Risks

**R1 — The payload diet deletes the field that makes heuristics triageable.** For a grep entry, `snippet` is the actual matched source line and the only way the agent can tell a real call from a comment. Stripping it means the agent either believes it or issues 20 follow-up calls; both regress the exact-repeat rate. Mitigation: Contract 2 Rule 5 makes snippet retention for heuristic entries non-negotiable, and re-validation instruments it as an explicit A/B.

**R2 — Text-channel wording is prompt engineering and will drift.** Mitigation: strings generated by a single function; five handlers that hand-write status prose are deleted; acceptance tests assert the first token and the presence of `UNKNOWN`.

## Related
- `docs/requirements/pathfinder-v6/03-contracts.md` — the full frozen specification
- ADR-003 — the readiness model producing `ToolStatus`
- Error Handling Principles: "provide context", "never fail silently"
