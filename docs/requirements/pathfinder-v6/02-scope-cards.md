# Pathfinder v6 — Scope Cards

**Format:** `parallel-dispatch` skill scope-card format.
**Prerequisite reading:** `00-plan.md` (invariants, DAG, ownership) and `03-contracts.md` (frozen types).
**Every task cites a finding ID** from `01-findings.md`. A task with no finding ID is out of scope.

---

# LEVEL 0

## Scope Card: WS-0 — Documentation truth

- **Deliverables:** corrected doc comments across `pathfinder-lsp` and `server.rs`; `docs/adr/adr-003-lsp-lifecycle.md`
- **Write Scope:** doc comments and `//!` headers only, no logic; `docs/adr/**`
- **Shared Reads:** all
- **Hard Dependencies:** none

**Why first:** a stale comment claiming lazy initialization directly caused a wrong architectural diagnosis in planning. Doc drift on lifecycle semantics is a correctness defect, not style.

### Tasks

**0.1** `crates/pathfinder/src/server.rs:120-129` — the `new()` doc says LSP processes start **lazily**. False; `server.rs:103` calls `warm_start()` before the function returns. Replace with eager-start semantics **plus an explicit note that it changed**:

```rust
/// # Lifecycle
///
/// Zero-Config language detection runs synchronously during construction.
/// LSP processes are then started **eagerly**: `warm_start()` spawns every
/// detected language in parallel background tasks before this function
/// returns.
///
/// **Note (v6):** LSP start was lazy prior to PATCH-004 and the old doc
/// comment survived the change. It is eager. Readiness is a separate concern
/// from spawn -- see `LspClient::ensure_ready` and INV-1/INV-2.
```

**0.2** Fix all nine stale sites, not the two originally listed (`LSP-B9`):

| file:line | current text | status |
|---|---|---|
| `server.rs:124-125` | "started **lazily** — only when the first LSP-dependent tool call is made" | false today |
| `lifecycle.rs:49-50` | "LSP processes are **not** started **until first use**" | false today; **the most misleading comment in the crate**, and the original acceptance grep missed it |
| `detect.rs:8` | "Language servers are started lazily on first use" | false today |
| `lawyer_impl.rs:340` | reason string `"{} available (lazy start)"` | false today |
| `mod.rs:4-5` | "started lazily on first use and terminated automatically after an idle timeout" | both halves false after WS-3 |
| `lifecycle.rs:52` | "Starts the idle-timeout background task." | false after WS-3 |
| `process.rs:7` | "Idle `last_used` tracking for auto-termination (PRD 6.2)" | false after WS-3 |
| `process.rs:1049-1051` | "which the idle timeout task will eventually clean up" | false after WS-3 |
| `background.rs:7` | "Idle timeout (process termination after inactivity)" | false after WS-3 |

`lifecycle.rs:160` and `:229` describe the genuine **failure-retry** path. Reword precisely ("retried lazily if warm start failed"), do not delete.

**0.3** `TS-B9` — `symbols.rs:852-872` documents an "impl-sibling fallback" in `resolve_symbol_chain` that does not exist in `:874-906`. 20 lines describing resolution semantics an implementer would rely on. Delete or rewrite to match.

**0.4** Write `docs/adr/adr-003-lsp-lifecycle.md`. Repo convention is `docs/adr/adr-NNN-*.md`, **not** `docs/decisions/`.

### Acceptance

- The `00-plan.md` §8 grep returns only matches describing the failure-retry path, each with an adjacent clarifying comment.
- ADR-003 exists.
- Rows 5-9 describe text that becomes false only *after* WS-3. WS-0 may fix pre-emptively; WS-3 acceptance re-checks.

---

## Scope Card: WS-1 — Schema diet

- **Deliverables:** −72% schema; zero `anyOf`/`oneOf`; `crates/pathfinder/tests/schema_budget.rs`
- **Write Scope:** `crates/pathfinder/src/server.rs` (tool attributes only); **parameter** structs in `server/types.rs`
- **Shared Reads:** `03-contracts.md`
- **Hard Dependencies:** none
- **Explicitly NOT in scope:** response types in `server/types.rs` — those belong to WS-4. Shared file; split by type category, enforced by review.

### Tasks

**1.1** Strip per-parameter prose duplicated from tool descriptions. Measured −47%. Parameter docs state type, constraint, default. Anything explaining *when to use the tool* lives in the tool description, once.

**1.2** Cap tool descriptions at 6 lines. Measured −24%.

**1.3** Flatten `oneOf`/`anyOf` const unions to plain `enum`. Measured −5%, and **this is the Gemini unblock** — Vertex function-calling rejects `anyOf`/`oneOf` with sibling keys.

| construct | location | disposition |
|---|---|---|
| `ExcludeGlob` (`#[serde(untagged)]` → `anyOf` + sibling `description`) | `server/types.rs:1088` | deleted with `search` in WS-5 |
| `FilterMode`, `SearchMode` | `server/types.rs` | deleted with `search` in WS-5 |
| `Detail`, `Visibility` | `server/types.rs` | deleted with `explore` in WS-5 |
| `TraceScope` | `server/types.rs:1423-1433` | **survives — must be fixed here** |

**`RSP-B6` — the drafted fix for `TraceScope` is a no-op.** The type is *already* `#[derive(..., schemars::JsonSchema)] #[serde(rename_all = "snake_case")]` with three fieldless variants. The `oneOf` is emitted because of the **per-variant doc comments** at `:1426`, `:1429`, `:1431`. **Strip the per-variant doc comments; keep the derive as-is.** Move the explanations into the tool description.

**1.4** Write `crates/pathfinder/tests/schema_budget.rs`:
- serialise the full `tools/list` response
- assert total token count ≤ a committed budget constant
- assert **zero** `anyOf` and **zero** `oneOf` keys anywhere
- assert no `$defs` entry has both a `const` and a sibling `description`
- assert no field of type `Option<Vec<_>>` or `Option<Evidence<_>>` carries `skip_serializing_if` (`RSP-A9`; assertion here, field changes in WS-4)
- assert the Contract 4 guidance grep returns zero (`RSP-A5`; assertion here, string sweep in WS-4)

Permanent guard against schema creep and against re-blocking Gemini.

### Acceptance

- Budget ≤ 1,674 tokens post-WS-1, ≤ 1,200 post-WS-5.
- Tool list loads on `gemini-2.5-pro` and `gemini-3.x-flash` with no function-declaration error.

### Kill gate

**K-1 is evaluated here.** If the WS-1 + partial WS-11 re-run leaves the deep-suite gap above +40% with no accuracy gain, stop and archive. Do not proceed to WS-3.

---

## Scope Card: WS-2 — Test infrastructure

- **Deliverables:** a scriptable `test-mock-lsp`; an injectable `Clock`; conformance fixture scaffolding
- **Write Scope:** `crates/test-mock-lsp/**`, `crates/pathfinder-lsp/src/clock.rs` (new), `crates/pathfinder/tests/common/**`
- **Shared Reads:** all
- **Hard Dependencies:** none

**Why Level 0:** `LSP-Z2` and `LSP-B8`. The mock cannot simulate slow indexing, empty-then-populated responses, dynamic registration, hangs, or malformed frames. It has no `references` handler, no `implementation` handler, and `prepareCallHierarchy` is hardcoded `null` — so C-4 and C-6, the two assertions carrying the product thesis, can only be asserted in the negative. And there is no mockable clock, so **two of the drafted readiness acceptance criteria pass today, unmodified, with the defect fully present.** Nothing downstream can be proven until this lands.

### Tasks

**2.1 — Scriptable response timeline.** `--script=<json>`, entries of `{ method, after_ms | after_n_requests, response | "hang" | "error:<code>" | "crash" }`. Covers slow indexing, empty-then-populated, request timeout, method-specific crash, `-32801`/`-32802` injection.

**2.2 — Position-aware canned index.** `{ uri -> { (line,char) -> { definition, references[], prepareCallHierarchy[], incomingCalls[], outgoingCalls[], implementation[] } } }` so the mock returns **real non-empty** answers for a real fixture repo. Required for C-4, C-5, C-6, C-7.

**2.3 — Server-initiated message emitter.**
- `--register-after-ms=N --register-method=...` → `client/registerCapability` (required for AC-3.5)
- `--progress-script=[begin, report(pct)..., end]` → `window/workDoneProgress/create` + `$/progress`; also exercises the 64-slot lag path (`LSP-A10`)
- `--config-request` → `workspace/configuration`, making `LSP-A11` observable

**2.4 — Handlers for `textDocument/references` and `textDocument/implementation`.** Currently absent; `main.rs:141-199` falls to `_ =>` at `:193` returning `-32601`.

**2.5 — Malformed-output injection at the framing layer.** `--malform=<none|bad-content-length|invalid-json|no-result-no-error|string-id|echo-method-in-response>`. The last three are exactly `LSP-A4`'s cases.

**2.6 — `--position-encoding=<utf-8|utf-16>`** in the initialize response, plus a non-ASCII fixture, to regression-test `LSP-A5` / `TS-A2`.

**2.7 — Injectable clock (`LSP-B8`).** Every clock in `pathfinder-lsp` is `std::time::Instant`: `mod.rs:41` (`LanguageState.spawned_at`, `UnavailableState.unavailable_since`), `lifecycle.rs:22`, `background.rs:14`, `process.rs:48` (`ManagedProcess.last_used`), `lawyer_impl.rs:13`. `tokio::time::pause()` does not move these.

Either introduce a `Clock` trait injected at `LspClient::new`, or migrate all to `tokio::time::Instant`. The second is smaller; verify nothing depends on cross-runtime comparability first.

This also fixes a mixed-clock hazard: `ensure_process` gates on `state.unavailable_since.elapsed()` (std, `lifecycle.rs:359`, `:397`) while `start_process` sleeps with `tokio::time::sleep` (`:582`). Under `start_paused = true` the sleep returns instantly but std elapsed never advances, so a crash/reprobe test hangs forever.

### Acceptance

- Each of the six `--malform` modes produces a **typed** error, not a silent empty.
- `--script` can make a server handshake in 200ms and answer `prepareCallHierarchy` with `[]` for 20s then non-empty.
- `tokio::time::advance(60min)` observably moves `LanguageState` age.
- The mock can produce a non-empty call hierarchy and a non-empty implementation result against a fixture.

---

# LEVEL 1

## Scope Card: WS-3 — LSP readiness contract

- **Deliverables:** probe-gated readiness state machine; `Lawyer::ensure_ready`; idle timeout removed **without** losing shutdown; document coherence; capability declaration fixed; `health` retired
- **Write Scope:** `pathfinder-lsp/src/client/{lifecycle,background,capabilities,document,protocol,mod}.rs`, `pathfinder-lsp/src/client/process.rs`, `pathfinder-lsp/src/lawyer.rs`, `pathfinder-common/src/config.rs`, `crates/pathfinder-lsp/Cargo.toml`
- **Shared Reads:** `03-contracts.md`, WS-2 outputs
- **Hard Dependencies:** WS-2

**This is the core of v6. Land it whole or land none of it** (`RSP-D4`).

### 3.1 — Probe-gated readiness

```
Spawned -> Handshaked -> Probing -> Ready
                            └----> Degraded (budget exhausted)
```

- `Handshaked`: `initialize` returned. **Not** exposed as ready.
- `Probing`: pick a probe file, `didOpen`, issue the probe, retry with backoff (250ms → 500ms → 1s → 2s, cap 2s) until success or budget exhaustion.
- `Ready`: probe resolved. Only now does the language answer requests.
- `Degraded`: budget exhausted. Requests return `ToolStatus::Degraded`.

**`LSP-B1` — do not port `health.rs:557` as-is.** It checks `if result.is_err() { return (false, false); }` (`health.rs:594`) — so `Ok(None)`, the LSP explicitly saying "I cannot resolve this", is treated as **PASS**. And the position is hardcoded `(1, 1)` (`health.rs:581`), which for Rust is `//!` or `use`, for Go and Java is `package` — `Ok(None)` is the *expected* result there, so it passes for the wrong reason on every language. The ported probe must assert a **non-empty resolution at a real symbol position**.

**`LSP-B2` — the probe needs a symbol position, and `pathfinder-lsp` cannot compute one.** The crate has no `pathfinder-treesitter` dependency (`Cargo.toml:20-40`). The edge is addable without a cycle but must be added deliberately — **`crates/pathfinder-lsp/Cargo.toml` is in this card's write scope for that purpose.** `find_probe_file` (`health.rs:657-717`) returns a file, not a position.

**`LSP-A6` — readiness must be per-capability.** `goto_definition` on an open file is answerable from the file-local index; `callHierarchy/incomingCalls` and `textDocument/references` need the global index. Two tools already use the definition probe and reach **opposite** conclusions — `deep_context.rs:85-88` sets `degraded = false`, `impact.rs:847-868` sets `degraded = true`. Model as `Ready { definition, references, call_hierarchy, implementation }`.

**`LSP-C1` — name the state carrier.** `ProcessEntry` (`mod.rs:92-96`) is two variants and `request()`/`notify()` gate on `Running` (`lifecycle.rs:1025`, `:1078`). No third state, no readiness field. Decide: third variant, or a field on `LanguageState`. Then make `request()`/`notify()` consult it and enumerate exempt callers (the probe itself, `did_open` for the probe file, `shutdown`).

Eight bypass paths must be closed or exempted — all call `ensure_process`, which resolves on handshake:
```
document.rs:121    did_open
lifecycle.rs:1166  call_hierarchy_request     <- every BFS hop (LSP-A2)
lawyer_impl.rs:52  goto_definition
lawyer_impl.rs:112 call_hierarchy_prepare
lawyer_impl.rs:208 references
lawyer_impl.rs:277 goto_implementation
lifecycle.rs:284   force_respawn              <- bypasses ensure_process entirely
lifecycle.rs:190   warm_start_for_languages   <- PUBLIC on the Lawyer trait
```

**Delete the global-flag race.** `is_warm_start_complete()` is one `AtomicBool` (`mod.rs:364`) that `warm_start_for_languages_and_track` sets true when **its own subset** finishes (`lifecycle.rs:167-182`) or immediately if the subset is empty (`:133-135`). `references.rs:481` and `definition.rs:140` consult it as a readiness gate. Delete that function or make the flag per-language.

**`LSP-C3` — probe cache.** If `.pathfinder/probe-cache.json` is used, specify invalidation (probe file deleted/renamed, binary upgraded, workspace moved), concurrency (two instances on one workspace — already solved once with an advisory lock at `process.rs:436-494`), and gitignore (`.pathfinder/` is only gitignored on cache-isolation paths, never for a Rust-only workspace).

**`LSP-D4` — do not hold the init lock across the probe.** `ensure_process` holds `init_locks[lang]` across spawn+handshake (`lifecycle.rs:384-389`, guard alive to `:436`). If `ensure_ready` shares it, three concurrent calls queue behind one probe and callers 2 and 3 hit their budget waiting for the **lock**, returning `NotReady` for a healthy server. Probe once, outside the lock; waiters observe via `tokio::sync::watch`.

**Delete after porting:** `probe_cache` and `lsp_started_at` on `PathfinderServer` (`server.rs:87-90`), `ProbeCacheEntry`, `get_probe_interval`, `find_probe_file`, `parse_uptime_to_seconds`, `format_uptime`. **Note:** `server.rs` is WS-1's write scope — coordinate.

### 3.2 — Bounded blocking on request

Implement `Lawyer::ensure_ready(&self, language_id, budget) -> Readiness` per Contract 1. Every LSP-backed entry point calls it before its first LSP request and branches per the Contract 1 Decision 5 table.

**`LSP-A2` — the gate at handler entry is not sufficient.** BFS hops at depth ≥1 go straight to `call_hierarchy_request` → `ensure_process` → `request(..., 10s)` with no gate. Change `bfs_call_hierarchy` to return:

```rust
enum TraversalOutcome {
    Complete, DepthCapped, BudgetExhausted,
    RequestFailed { count: u32 }, LimitReached,
}
```

Any outcome other than `Complete` sets `partial: true` and forces `Confidence::Heuristic` (or `None`) for the affected direction. **Highest-value change in WS-3 — ~40 lines closing the one fake-zero channel a request-entry gate provably cannot reach.**

**`LSP-A1` — evaluate per-direction, not per-pair.** Delete the both-empty heuristic at `impact.rs:790-820`. Outgoing calls resolve from file-local syntax; incoming need the global index. `outgoing=[3], incoming=[]` does not trip a both-empty guard and gets stamped verified.

**Delete the compensation sprawl — all seven sites** (`RSP-D4`), not the three originally listed:
```
references.rs:367-473    first ladder
references.rs:475-583    second ladder
references.rs:685-698    the unguarded "LSP confirmed zero" text/hint (RSP-A2)
impact.rs:789-820        both-empty heuristic
impact.rs:831-905        handle_empty_call_hierarchy
deep_context.rs:74-148   probe + 1s retry + grep
definition.rs:43-151     a 1s sleep-and-retry NOT in the original plan (definition.rs:57)
```

**`LSP-D6` — do not half-delete.** Removing `deep_context.rs:99-140` (the retry) while keeping `:85-88` (probe-passes ⇒ `degraded=false` with an EMPTY dependency list) moves `inspect` from "sometimes recovers" to "confidently returns zero dependencies."

**`RSP-A2` — rewrite `references.rs:685-698`.** "LSP confirmed: zero references or implementations" and "LSP confirmed zero references" may only be emitted when `ensure_ready` returned `Ready`.

**`LSP-C2` — specify the end-to-end budget, not just the gate.** `ensure_ready(10s)` stacks **on top of** existing waits. Java `references` today: `wait_for_capability` polls up to 15s (`mod.rs:205` grace + `lifecycle.rs:1125-1147`) **before** the request, then a 30s request timeout (`lawyer_impl.rs:240`) — **45s worst case for one call**, and `symbol_overview` issues two (`overview.rs:77`, `:81`), so 90s.

State: whether `ensure_ready` **replaces** `wait_for_capability` (it should — INV-2, and `LSP-D5` requires removing the declaration precheck anyway); the total per-tool wall clock as a single configurable number; and that the budget is per **tool call**, not per LSP request, so a depth-2 BFS cannot multiply it. Recommended: `lsp.readiness_budget_secs = 10` for the gate, `lsp.request_budget_secs = 30` for the whole call.

**`LSP-Z1` — use the consolidated table as the checklist.** `01-findings.md` §`LSP-Z1` enumerates **every** empty/None conversion that becomes an indistinguishable zero — 23 sites in the LSP crate, 13 in the tools. Each row is either fixed or explicitly justified in the handoff.

### 3.3 — Remove the idle timeout — WITHOUT destroying shutdown

**`LSP-B0`. Read this before touching `background.rs`.**

`idle_timeout_task` (`background.rs:455-475`) **is** the shutdown handler. Its `select!` has two arms; it is the **only** caller of `shutdown_all_processes` (`:465`) and the **only** caller of `reap_dead_processes` (`:470`). `LspClient::shutdown()` (`lifecycle.rs:258-271`) only flips an `AtomicBool` and sends on a broadcast whose sole production subscriber is `lifecycle.rs:87`.

**Correct instruction:**
- **KEEP** the task. Rename `process_supervisor_task`.
- **KEEP** the `select!`, `shutdown_all_processes`, `reap_dead_processes`.
- **DELETE** `reap_idle_processes` (`background.rs:389-452`), `collect_idle_languages` (`:260-280`), `DEFAULT_IDLE_TIMEOUT` (`:17`).
- **DELETE** `touch_language` (`lifecycle.rs:242-252`) **and** `LspClient::touch` (`lifecycle.rs:974-980`) with its six call sites: `lawyer_impl.rs:88`, `:148`, `:185`, `:251`, `:313`, `document.rs:161` (`LSP-D3` — the original plan named only the first).
- **DELETE** `idle_timeout_minutes` (`config.rs:131-132`, `:256`). `LSP-B10`: already dead — production uses the hardcoded constant. Hygiene, **not** evidence the behaviour changed.

**`LSP-D3` — dead-code cascade under `-D warnings`.** Also orphaned: `LspTransport::last_used`/`set_last_used` (`process.rs:71`, `:74`), `LspTransport::in_flight` (`:77`), `ManagedProcess.last_used`/`.in_flight` (`:145`, `:147`), `InFlightGuard` + `Drop` (`mod.rs:316-335`).

**Strengthen crash-respawn.** With no idle-kill, a dead LSP is a hard failure. `reader_supervisor_task` must transition back to `Probing`, **not** directly to `Ready`, so INV-2 holds across a crash.

**`LSP-C4` — bound the respawn loop.** Backoff is `min(1<<attempt, 60)` (`lifecycle.rs:357-360`, `background.rs:18`) and `start_process` resets attempt to 0 post-lock (`:402`). Adding a probe of up to the full budget per cycle gives an endless treadmill. Terminal rule: N consecutive probe failures → `Degraded` sticky, stop respawning, report guidance.

**`LSP-A8` — fix the liveness check while here.** `lifecycle.rs:1029` and `:1081` gate on `state.reader_handle.is_finished()`; the real flag is `state.reader_alive` (`mod.rs:59`). One line in two places; prevents a full-timeout block against a dying process that then gets mislabelled "warming_up".

**README:** one LSP process per detected language for the session lifetime; on a 5-language monorepo expect the sum of those servers' RSS. Users wanting the old behaviour restart the host.

### 3.4 — Document coherence (`LSP-A3`) — HARD PREREQUISITE for 3.3

Removing idle-kill removes the **only** document-state invalidation. No `didChange` anywhere, `doc_versions`' `AtomicI32` never incremented, `FileEvent`/`FileChangeType` (`types.rs:156-175`) with zero non-test references.

- **3.4.1** Make `did_open` atomic. Replace `contains_key`+`insert` (`document.rs:131`, `:139`) with `DashMap::entry(...).or_insert_with(...)`; derive `owns_open` from whether this call created the entry. (H2)
- **3.4.2** Make `DocumentGuard::drop` ordered. Await the didClose send **before** removing the entry. Never remove-then-spawn (`document.rs:62` then `:74-78`). (H1 — this hazard makes a file permanently unqueryable for the whole session)
- **3.4.3** Content-hash at `did_open`. On subsequent opens re-hash; on mismatch send `didChange` (full sync, version+1) instead of dedup'ing. Makes the dead counter live.
- **3.4.4** Declare `workspace.didChangeWatchedFiles.dynamicRegistration`, honour `client/registerCapability` for it, send `FileEvent` batches. Types already exist.

### 3.5 — Capability declaration (`LSP-B3`)

Actual payload at `process.rs:843-856`:
```
"definition":    { "dynamicRegistration": false, ... }   <- already false
"references":    { "dynamicRegistration": true }         <- flip to false
"callHierarchy": { "dynamicRegistration": true }         <- flip to false
```

**`textDocument.implementation` is absent entirely.** No flag to flip. It is the capability behind `goto_implementation` → trait-method expansion (`impact.rs:621-629`) → C-6 → the only measured differentiator against CodeGraph. **Declaring it is a prerequisite for the product thesis.**

Also add `textDocument.synchronization`, `workspace.configuration` (`LSP-A11`), `workspace.didChangeWatchedFiles` (3.4.4).

**Three-layer policy on static vs dynamic:**
1. Declare `dynamicRegistration: false` for the four needed capabilities → most servers report statically.
2. Still honour `client/registerCapability` for servers that ignore that. Merge late registrations. **Never wait on them.**
3. **Never gate readiness on declaration at all (INV-2).** The probe makes the distinction unobservable — the only way it is reliable across untested servers.

Delete the grace-period logic and the `"dynamic capability registration may still be in progress"` string (`health.rs:436-444`).

**`LSP-D5` — you must also remove the declaration gate from the request path.** `grace_period_for_language` (`mod.rs:203-209`) feeds `wait_for_capability` (`lifecycle.rs:1114`). With grace=0 it reduces to a hard precheck — `lifecycle.rs:1115-1123` returns `UnsupportedCapability` — called before **every** navigation request (`lawyer_impl.rs:54`, `:114`, `:210`, `:279`). jdtls registers `callHierarchy` dynamically 10-15s after initialize, so a Java `trace` at t=3s would fail even though the probe would succeed. `wait_for_capability` was not mentioned in the original plan at all.

### 3.6 — Surface warm-start failures (`LSP-C6`)

`lifecycle.rs:156-162` swallows spawn failure into a `warn!`. It must reach the agent via `ensure_ready`. But there is no `LanguageState` on that path — `lifecycle.rs:639-645` inserts `ProcessEntry::Unavailable(UnavailableState{...})` and `UnavailableState` (`mod.rs:84-90`) has **no reason field**. Decide: add `reason: String`, or restructure the enum.

### 3.7 — Retire the `health` tool

| health did | v6 replacement |
|---|---|
| "is the LSP ready" | `ensure_ready` — the agent never asks, because answers are never lies |
| "which tools are degraded" | `ToolStatus` on every response |
| "restart a stuck LSP" | automatic: `Degraded` triggers respawn-and-reprobe, bounded per `LSP-C4` |

**One residual is not readiness:** "a language was detected but no LSP binary is on PATH" (`missing_languages` + install hints). Configuration. Emit to the startup log at `warn!` and as a one-line `setup_notice` on the **first** tool response of a session. Not a tool.

Delete `health.rs` (993), `health_test.rs` (3,957), `HealthParams`/`LspHealthResponse`/`LspLanguageHealth`/`DegradedToolInfo`, and `#[tool(name = "health")]` at `server.rs:385`.

**`LSP-D9` — do not port dead language IDs.** `health.rs:658-666` and `:681-690` key on `"javascript"` and `"ruby"`; neither has a `LanguagePlugin`. Same at `lifecycle.rs:29` and `mod.rs:206`.

### Acceptance

- **AC-3.1** `cold_start_never_returns_empty` — `trace` within 500ms of startup on a fixture with known callers returns `not_ready` **or** the correct non-empty list. Never empty with `Confidence::Verified`. Per language.
- **AC-3.2** `no_idle_termination` — advance the **injected** clock 60 minutes; the process map still holds a `Ready` entry. *(Uses WS-2.7. Without it this passes today with the defect present — `LSP-B8`.)*
- **AC-3.3** `crash_reprobes` — kill the child; transitions `Ready → Probing → Ready`, does not serve as `Ready` during `Probing`.
- **AC-3.4** `shutdown_still_works` — **new, guards `LSP-B0`.** After `shutdown()`, every child received `shutdown`/`exit`, no process remains, and `reap_dead_processes` still runs on the supervisor interval.
- **AC-3.5** `dynamic_registration_irrelevant` — mock advertises **zero** static capabilities, registers `definition` dynamically after 2s; still reaches `Ready` via probe with no grace period consulted.
- **AC-3.6** `partial_traversal_is_not_verified` — **new, guards `LSP-A2`.** Force a mid-BFS timeout; assert `partial: true` and the affected direction is not `Verified`.
- **AC-3.7** `document_edit_is_visible` — **new, guards `LSP-A3`.** Edit a fixture file between two `trace` calls; the second call's line numbers match.
- **AC-3.8** grep: idle path gone, supervisor retained.

---

# LEVEL 2

## Scope Card: WS-4 — Response contract

- **Deliverables:** `ToolStatus`, `Confidence`, `Evidence<T>` per `03-contracts.md`; dual-channel renderer; guidance sweep; pagination
- **Write Scope:** `crates/pathfinder/src/server/types.rs` (**response** types), `server/helpers.rs`, `pathfinder-common/src/types.rs`, `pathfinder-common/src/error.rs`, response-construction halves of `navigation/{definition,references,impact,deep_context,overview}.rs`
- **Shared Reads:** `03-contracts.md`, WS-3 outputs
- **Hard Dependencies:** WS-3

**Why this exists:** `RSP-D5`. The contract spans five workstreams with disjoint ownership; under MECE dispatch nobody owns *"is the response honest."* This card is that owner.

### Tasks

**4.1** Implement Contract 1 (`ToolStatus`) — type, `retry_attempt` param, all five retry mechanisms, exact text wording. Closes `LSP-C5`, `RSP-A2`.

**4.2** Implement Contract 2 (`Confidence`, `Evidence<T>`) — required field on all six result item types; homogeneity; delete `incoming_verified`/`outgoing_verified` and `compute_verified_flags` (`impact.rs:1137-1153`); `validate_response()` with the four illegal combinations. Closes `LSP-C7`, `RSP-A7`, `RSP-A8`.

**4.3** Implement Contract 3 (dual-channel). **Highest-blast-radius item in the register:** `RSP-A1` — `impact.rs:1515-1516` renders `None` as `0` via `map_or(0, Vec::len)`, and `:1264`/`:1277` print `Incoming references: 0`. `render_callers_callees_text` (`:1226-1318`) never reads `hint`, `incoming_verified`, `outgoing_verified`. Same at `overview.rs:397-400`. Rendering becomes a total function of the tri-state.

Also gate the word "authoritative" (`overview.rs:422`) on every sub-call returning `Ready`.

**4.4** Remove `skip_serializing_if` from `server/types.rs:26`, `:71`, `:76` (`RSP-A9`). Add missing overview fields: `incoming_verified`/`outgoing_verified` on `ImpactSummary`, `confidence` on `SymbolOverviewImpactEntry`, `hint` and `resolution_strategy` on `SymbolOverviewResponse`.

**4.5** Implement Contract 4 (guidance sweep). Replace `FallbackTool::{Search, Read}` (`common/types.rs:583-588`); sweep the 13 sites in `RSP-A5`. **Cheapest fix in the register, largest convergence protection** (INV-8).

**4.6** Implement Contract 5 (pagination). Add `next_offset` to `FindCallersCalleesMetadata`; honour `TraceParams.offset` for `scope=callers`.

**4.7** Fix batch provenance (`RSP-A6`). Add `confidence` + `readiness` + `degraded_reason` to `LocateResultEntry` (`types.rs:1380-1407`) and `InspectResultEntry` (`:1341-1364`); `heuristic_count` on the batch results; `[HEURISTIC]` suffix in text plus a leading `DEGRADED` block. **Delete or gate `grep_symbol_broad` (`definition.rs:857-901`)** — returning the first `\b{name}\b` hit in the repo as a *definition* is a coin flip presented as an answer.

**4.8** Fix error codes. `NoLspAvailable`/`LspTimeout` map to `-32603` (`helpers.rs:58-65`), which models treat as terminal. Under the new contract these become `ToolStatus` values on a successful result — assert the mapping is unreachable.

**4.9** Fix `inspect` error-quality asymmetry (`RSP-A11`). `symbols.rs:261` calls `self.surgeon.read_symbol_scope(...)` directly; replace with `self.read_symbol_scope_enriched(...)` so plain `inspect` gets the same `::`→`.` auto-correction and cross-file did-you-mean every other path has. **One line.**

### Acceptance

All five Contract 3 acceptance tests, plus:
- `rg -n 'skip_serializing_if' crates/pathfinder/src/server/types.rs` → zero on tri-state fields
- the Contract 4 guidance grep → zero
- each of the four Contract 2 Rule 4 illegal combinations panics in debug
- `*_truncated: true` implies `next_offset.is_some()` and the next page is disjoint

---

## Scope Card: WS-5 — Tool surface reduction

- **Deliverables:** 3 tools; ~13,300 LOC deleted; `grep_support.rs` extracted **first**
- **Write Scope:** `crates/pathfinder/src/server/tools/**`
- **Shared Reads:** WS-4 outputs
- **Hard Dependencies:** WS-4

### 5.0 — EXTRACT BEFORE DELETING (`RSP-A4`) — blocking

| function | defined | called by survivors |
|---|---|---|
| `search_codebase_impl` | `search.rs:71` | `references.rs:100`, `impact.rs:166` |
| `enrich_matches` | `search.rs:115` | via the above — populates `SearchMatch.enclosing_semantic_path`, used at `impact.rs:194-197` |
| `apply_filter_mode` | `search.rs:349` | via the above, `FilterMode::CodeOnly` by default (`types.rs:1213`, `:1568-1570`) |
| `find_symbol_impl_inner` | `find_symbol.rs:476` | `mod.rs:593` in `enrich_did_you_mean` → `references.rs:255`, `impact.rs:1372`, `deep_context.rs:456` |

Extract all four into `tools/navigation/grep_support.rs` (rename `search_codebase_impl` → `grep_candidates`, strip the tool-response wrapper). **Verify `rg -n 'search_codebase_impl|find_symbol_impl_inner' crates/pathfinder/src --glob '!*_test.rs'` returns only `grep_support.rs` and its callers BEFORE any deletion lands.**

Losing `enrich_matches` silently degrades every heuristic caller to `file::symbol_name` (`impact.rs:197` fallback) — the tool stops telling the agent *which function* the candidate is in.

### 5.1 — Delete tool surface

Remove `#[tool(...)]` at `server.rs:196` (`explore`), `:234` (`search`), `:266` (`read`), `:385` (`health`), and:

| module | LOC (impl + test) |
|---|---|
| `tools/repo_map.rs` + test | 1,359 |
| `tools/search.rs` + test | 1,541 |
| `tools/read_files.rs` + test | 1,794 |
| `tools/source_file.rs` + test | 1,222 |
| `tools/find_symbol.rs` + test | 2,452 |
| `tools/navigation/health.rs` + test | 4,950 |
| **total** | **≈13,318** |

**`RSP-A3` / `TS-B1` — `tools/symbols.rs` is NOT on this list.** It is the `inspect` handler (`:21 inspect_impl`, `:199`, `:221`; `server.rs:316`). The draft listed it at 1,020 LOC and the figure matched exactly, so it was deliberate, not a typo. **Rename it `tools/inspect.rs` in the same change** so the mistake cannot recur.

### 5.2 — Engine crates stay

Newly dead in `pathfinder-treesitter` (`TS-B7`, `TS-E11`):

| item | verified sole caller |
|---|---|
| `repo_map.rs` (986) + test (1,767) | `tools/repo_map.rs:293` — confirmed no other |
| `Surgeon::generate_skeleton` (`surgeon.rs:178`) | same |
| `Surgeon::node_type_at_position` (`surgeon.rs:169`) | `tools/search.rs:322` — **plan missed** |
| `Surgeon::read_source_file` (`surgeon.rs:189`) | `tools/source_file.rs:160` — **plan missed** |
| `Surgeon::extract_symbols_preloaded` (`surgeon.rs:119`) | none — **already dead** |
| `treesitter_surgeon.rs:275-312`, `:325-366` | via `node_type_at_position` |
| `treesitter_surgeon.rs:177-193` | via `read_source_file` |
| `cache.rs:188`, `:231` preloaded variants | via `extract_symbols_preloaded` |

**MUST KEEP:** `did_you_mean` (`symbols.rs:1001`) — live via `treesitter_surgeon.rs:132` on the error path all three tools hit. `enclosing_symbol_detail` — `navigation/mod.rs:531`. `mock.rs` in full — six surviving test files use `MockSurgeon`. `vue_zones.rs` — see WS-7.

### 5.3 — Protect the surviving properties

- **Convergence:** responses stay self-sufficient. WS-11 tracks exact-repeat rate; regression invalidates the cut (K-5).
- **Resolution:** the `mod.rs` vs `lawyer_impl.rs` distinction must be **explicit**. Implemented in WS-10.3.

### Acceptance

- `tools/list` returns exactly 3; schema ≤ 1,200 tokens; zero `anyOf`/`oneOf`.
- `cargo build --workspace --all-targets` clean, no dead-code warnings.
- Repo LOC drops by ≥ 14,000.
- `grep_support.rs` exists and the extraction grep passes.

---

## Scope Card: WS-6 — LSP plugin generalization

- **Deliverables:** `detect.rs` collapsed to a generic loop; `LanguagePlugin` extended to all divergence axes; TOML plugin declarations; `response_parsers.rs` hardened
- **Write Scope:** `pathfinder-lsp/src/plugin.rs`, `pathfinder-lsp/src/client/detect.rs`, `pathfinder-lsp/src/client/response_parsers.rs`
- **Shared Reads:** WS-3 outputs
- **Hard Dependencies:** WS-3

**The problem:** the abstraction exists but is not the code path. `plugin.rs` has a clean pure-data `LanguagePlugin` trait with 5 implementations (~40 lines each). But `detect.rs` is **1,526 lines of hand-written per-language functions** — `detect_rust` (`:876`), `detect_go` (`:975`), `detect_typescript` (`:1075`), `detect_python` (`:1203`), and the Java equivalent — each re-implementing the same 40-line dance through `get_command_override!`/`get_args!`. **That is why there are 5 languages.**

### 6.1 — Collapse `detect.rs`

All per-language `detect_*` delete. **`LSP-D7` — the naive 6-line loop drops five load-bearing behaviours.** All five must survive:

| behaviour | evidence |
|---|---|
| `validate_marker_file` gate + `"Fix {marker}: {reason}"` missing-entry | `detect.rs:919-931`, `:1018-1029`, `:1128-1139`, `:1257-1273`, `:1359-1380` |
| `sibling_filter` for `package.json` (TS only if a sibling `tsconfig.json` exists in the multi-match case) | `:1088-1092`, threaded through `:673-696`, `:632-658` |
| `resolve_multiple_matches` monorepo fallback | `:632-658`, invoked `:694` |
| `has_source_files_recursive` post-filter on both lists | `:1430-1460` |
| `has_override` suppression of the missing-entry | `:954`, `:1053`, `:1179`, `:1293`, `:1394` |

**Target: ≤ 450 lines, not 150.** The original figure was arrived at without these; 150 forces their deletion.

### 6.2 — Extend `LanguagePlugin` to ALL divergence axes

`LSP-B5` found **eleven** axes, not four.

```rust
pub trait LanguagePlugin: Send + Sync {
    // existing, pure data
    fn language_id(&self) -> &'static str;
    fn file_extensions(&self) -> &'static [&'static str];
    fn marker_files(&self) -> &'static [&'static str];
    fn marker_search_depth(&self) -> u32;
    fn lsp_candidates(&self) -> &[LspCandidate];
    fn install_hint(&self) -> &'static str;

    // AXIS 1 -- NOT pure data (LSP-B4). Python reads $CONDA_PREFIX/$VIRTUAL_ENV and
    // stats 8 paths (detect.rs:340-392); Java reads $JAVA_HOME, .sdkmanrc,
    // .java-version and probes sdkman/asdf/jenv on BOTH roots (detect.rs:412-477).
    fn initialization_options(&self, project_root: &Path, workspace_root: &Path)
        -> serde_json::Value { serde_json::Value::Null }

    // AXIS 10 -- a DIFFERENT lifecycle point. Python only today (process.rs:300-304).
    fn revalidate_at_spawn(&self, opts: serde_json::Value, project_root: &Path)
        -> serde_json::Value { opts }

    // AXIS 2 + readiness. NEVER a substitute for the probe (INV-2).
    fn readiness_signal(&self) -> ReadinessSignal { ReadinessSignal::ProbeOnly }
    fn readiness_budget(&self) -> Duration { Duration::from_secs(120) }
    // AXIS 4 -- the HANDSHAKE timeout, consumed at process.rs:334. Java = 180
    // (detect.rs:1389). Semantically distinct from the probe budget.
    fn handshake_timeout(&self) -> Duration { Duration::from_secs(60) }

    // jdtls emits `login(String) : void`, rust-analyzer `login`, pyright nests.
    fn normalize_symbol_name<'a>(&self, raw: &'a str) -> Cow<'a, str> { Cow::Borrowed(raw) }

    fn capability_quirks(&self) -> CapabilityQuirks { CapabilityQuirks::default() }

    // AXIS 5 -- five distinct parsers (detect.rs:241-323). Sole producer of the
    // "Fix {marker}: {reason}" hint. Not expressible as a TOML enum.
    fn validate_marker(&self, marker: &str, content: &str) -> Result<(), String> { Ok(()) }

    // AXIS 6 -- a property of a (language, marker) PAIR, not a flat field.
    fn marker_sibling_requirement(&self, marker: &str) -> Option<&'static str> { None }

    // AXIS 7 -- I/O + a hardcoded per-server JSON shape (detect.rs:802-838,
    // process.rs:802-832).
    fn discover_server_plugins(&self, root: &Path) -> Vec<ServerPlugin> { vec![] }

    // AXIS 8 -- four distinct env-var sets with directory pre-creation
    // (process.rs:555-680), plus ensure_pathfinder_in_gitignore for all but rust.
    fn spawn_env(&self, root: &Path) -> Vec<(OsString, OsString)> { vec![] }
    fn needs_gitignore_entry(&self) -> bool { true }

    // AXIS 9 -- argv injection returning a resource that must outlive the call.
    // jdtls appends `-data <dir>` and returns an advisory lock File held for the
    // process lifetime (process.rs:436-494, ManagedProcess._jdtls_lock at :153).
    fn spawn_argv_hook(&self, root: &Path, argv: &mut Vec<String>)
        -> Option<Box<dyn Any + Send>> { None }

    // AXIS 11 -- per-platform per-language diagnostic strings
    // (lifecycle.rs:880-886 Linux, :951-957 macOS).
    fn isolation_description(&self) -> Option<&'static str> { None }
}
```

After WS-6: **zero** per-language conditionals in `pathfinder-lsp`, in **both** `match` and `==` form (`LSP-B6`).

### 6.3 — TOML plugin declarations, honestly scoped

**`LSP-B4` — you cannot delete `PythonPlugin` or `JavaPlugin`.** Their init-options logic is ~150 lines of Rust no TOML expresses.

Achievable design is **TOML for the data axes + an optional Rust hook per plugin**:

```toml
[[language]]
id = "kotlin"
extensions = ["kt", "kts"]
markers = ["build.gradle.kts", "settings.gradle.kts"]
marker_depth = 2
lsp_candidates = [{ binary = "kotlin-language-server", args = [] }]
install_hint = "brew install kotlin-language-server"
readiness_budget_secs = 180
handshake_timeout_secs = 60
[language.quirks]
requires_did_open = true
```

**State the claim accurately in the README:** "we ship N conformance-verified languages; a language needing no venv/JDK discovery, no cache isolation, no argv injection and no server plugins can be added in TOML without recompiling." Not "any LSP works."

**`LSP-D8` — extension routing is a footgun.** `plugin_for_extension` (`plugin.rs:288-293`) is first-match-wins; `TypeScriptPlugin` already claims `"vue"` (`:141`); `language_id_for_extension` (`detect.rs:1519-1521`) is the single router for **every** LSP call site. A user plugin claiming `"ts"` silently reroutes all TypeScript traffic by insertion order. Add a **uniqueness assertion at registry build** and a defined precedence rule.

### 6.4 — Harden `response_parsers.rs` (`LSP-A4`, `LSP-A7`, `LSP-A9`)

Owned by nobody in the original plan; contains three wrong-answer vectors.

- **`LSP-A7`** — `:116-132` returns `arr.first().cloned()` (`:127`), discarding all other definition locations. **This is the data WS-10.3 needs** for the `declared:` vs `impl:` rendering. Return all.
- **`LSP-A4`** — in `dispatch_response_for_language` (`protocol.rs`, WS-3's scope; coordinate): require exactly one of `result`/`error`; otherwise resolve the oneshot with `LspError::Protocol`. Handle non-u64 ids rather than leaking the pending entry.
- **`LSP-A9`** — `:348-350`, `:354-356`: count and report silently-skipped references. jdtls returns `jdt://contents/...` for JAR contents; those vanish today, presenting an undercount as complete. `:366-376`: a missing `start.line` must not default to `1` — that turns a malformed range into a plausible, specific, wrong `file:1:1`. `:357-360`: when `strip_prefix` fails, do not emit the absolute path as a workspace reference (`references.rs` has no `is_workspace_file` filter, unlike `impact.rs:557-562`).

### 6.5 — `$/progress` channel (`LSP-A10`)

`NOTIFICATION_CHANNEL_CAPACITY = 64` (`protocol.rs:16`) is a broadcast channel; `background.rs:161-167` logs `Lagged(n)` and continues. rust-analyzer emits hundreds during crate-graph build. Either widen/replace the channel, or **state explicitly that a missed `end` degrades to probe-only** — `ReadinessSignal::WorkDoneProgress` is unsound on a lossy channel otherwise.

### Acceptance

- `wc -l crates/pathfinder-lsp/src/client/detect.rs` ≤ 450.
- The `00-plan.md` §8 WS-6 grep (both forms) returns nothing.
- A new language added purely via `.pathfinder.toml` (no recompile) is detected, spawned, reaches `Ready` — proven with `test-mock-lsp` under a synthetic language ID.
- All five existing languages still pass their conformance fixtures.
- Duplicate extension claims rejected at registry build.

---

# LEVEL 3

## Scope Card: WS-7 — Tree-sitter addressing and scaling

- **Deliverables:** correct addressing (7a); query-driven extraction with a generic containment pass (7b); cache lifetime contract
- **Write Scope:** `crates/pathfinder-treesitter/**`
- **Shared Reads:** WS-3 outputs (encoding negotiation result)
- **Hard Dependencies:** 7a — none. 7b — 7a.

**This is the real language ceiling, and it is worse than the LSP layer.** Support is `min(LSP plugins, Tree-sitter coverage)`. Tree-sitter is the addressing layer. WS-6 alone does not raise the ceiling — it moves the bottleneck.

## WS-7a — Addressing correctness (BLOCKING; before 7b and before WS-3's certification takes effect)

**`TS-D6` — why this is blocking.** Every wrong-position query in 7a returns `[]` from a fully-warm, fully-ready LSP. WS-3 will correctly certify those as **verified zeros**. The kill condition "cold-start confidently-wrong-empty = 0" does not detect them — they are **warm-start** failures (INV-7).

**7a.1 — Position pairing invariant (`TS-A1`).** Ten call sites pass `(scope.start_line + 1, scope.name_column + 1)` where `start_line` is the *declaration node's* first row (`symbols.rs:152`) and `name_column` is the *name node's* column (`symbols.rs:144`). Nothing enforces same-row. Every `@Override` Java method is queried past end-of-line.

The ten sites:
```
definition.rs:64-65, :456-459      goto_definition
references.rs:299-300              references
references.rs:309-310              goto_implementation   <- product thesis (C-6)
references.rs:373-374              goto_definition probe
deep_context.rs:55-56, :80-81, :107-108
impact.rs:626-627                  goto_implementation   <- C-6
impact.rs:842-843, :1032-1033      call_hierarchy_prepare
```

`ExtractedSymbol` MUST carry a single `name_point: tree_sitter::Point` from `resolve_name_node(...).start_position()`, replacing the pair at every handoff. `start_line`/`end_line` remain for **display and range extraction only** and MUST NOT be paired with `name_column`.

Add a debug assertion that `name_node.start_position().row >= node.start_position().row`, and a unit test per language for: `@Override` Java method, `@Service` Java class, `@Injectable()` **non-exported** TS class, `#[test]` Rust fn, `@pytest.fixture` Python fn, multi-line Go signature.

**7a.2 — Position encoding (`TS-A2` / `LSP-A5`).** `tree_sitter::Point.column` is a **byte** offset; LSP 3.17 defaults to **UTF-16** when unnegotiated, and `rg positionEncoding` across the workspace returns zero matches.

Declare `general.positionEncodings: ["utf-8","utf-16"]` (WS-3 owns `process.rs:843`; coordinate), record the server's choice in `LanguageState`, and add `fn to_lsp_character(line_bytes: &[u8], byte_col: usize, encoding: PositionEncoding) -> u32` applied at every `name_column` handoff. **Servers omitting `positionEncoding` get UTF-16 — do not assume UTF-8.**

**7a.3 — Phantom `0..0` impl symbols (`TS-A3`).** `merge_rust_impl_blocks_recursive` step 3 (`symbols.rs:962-978`) synthesises a symbol with `byte_range: 0..0`, `start_line: 0`, `name_column: 0` whenever an impl block's type is declared elsewhere — true of `lawyer_impl.rs:17`, **the plan's own flagship example**. `inspect` returns `Ok("")`; `locate` queries byte 0; `find_enclosing_symbol_ref` matches it for row 0 with span 0, beating every real symbol.

Either (a) retain the original `Impl` symbol with its real range, or (b) hoist methods to top level under `<Type>.<method>` with no synthetic parent. Add `debug_assert!(!s.byte_range.is_empty() || s.kind == SymbolKind::Zone)` and a runtime guard in `read_symbol_scope` returning a **typed error** rather than empty content.

**7a.4 — Separator invariant (`TS-A4`).** Six sites emit `::` inside the symbol chain (`symbols.rs:1224-1228`, `:1363`, `:1466`, `:1574`, `:1679`), producing paths the server's own parser rejects (`error.rs:210-214`: "only one '::' is allowed"). Route all through `build_path`. JSX additionally emits a `return` segment (`:1363`) corresponding to no symbol.

Assert `!sym.semantic_path.contains("::")` for every symbol from `extract_symbols_from_tree` and `extract_symbols_from_multizone`. Make `did_you_mean` return **file-qualified** paths (`symbols.rs:1008` pushes bare paths; once `find_symbol` is deleted every suggestion is un-callable).

**7a.5 — Line-index normalisation (`TS-A7`).** `inspect` reports 0-indexed `start_line` (`tools/symbols.rs:280-281`, from `pathfinder-common/src/types.rs:337-340`); `locate` reports 1-indexed. Neither documents it. Normalise all agent-facing line numbers to 1-indexed at the serialization boundary; keep 0-indexed only inside `pathfinder-treesitter`.

**7a.6 — Overload determinism (`TS-A9`, `TS-A10`, `TS-D3`).** `#N` is assigned by traversal order within a per-node `SymbolExtractionContext` (`symbols.rs:305-313`), which resets at every intermediate node — so `export function handle()` and `function handle()` both get `semantic_path: "handle"` and the second is unaddressable. Two impls of the same method on one type both collapse to `Foo.fmt` (`:921-930`, `:956-958`) with `resolve_symbol_chain` silently taking the first (`:890-898`) while `AmbiguousSymbol` (`pathfinder-common/src/error.rs:130`) is never raised.

Define `#N` as a deterministic function of `(byte_range.start, name)` sorted ascending, **not** traversal order. Emit `#N` whenever a duplicate exists. Raise `AmbiguousSymbol` or return all candidates rather than picking the first.

**Load-bearing subtlety, do not break:** `make_unique_name` (`symbols.rs:717-729`) does **not** make the name unique — it returns the input verbatim and only `semantic_path` gets `#N`. `resolve_symbol_chain` matches on the **unsuffixed** `s.name` (`:891`), which is what makes the arithmetic work (`TS-E5`). A "cleanup" that makes the name actually unique silently breaks overload resolution with no test failure.

**7a.7 — Vue zone scanning (`TS-A11`).** `find_zone` (`vue_zones.rs:98-141`) takes the first `</template>` after the first `<template` (`:131`), so a nested `<template v-if>` truncates the zone and everything after it silently vanishes with no `degraded` flag. The docstring at `:147-148` claims at most one of each zone; the spec allows `<script>` **plus** `<script setup>` and multiple `<style>` blocks. `language.rs:220` breaks on a JS string containing `"</script>"`. And `parse_zone_with_grammar` (`:263`) has **no parse timeout**, unlike `AstParser::parse_source` (`parser.rs:48-59`).

**7a.8 — Cache lifetime contract (`TS-A5`).** Blocking companion to WS-3.3, which makes sessions permanent.
- (a) Change the `cache.rs:251` fast path to compare `metadata.len()` as well as mtime. Size+mtime is strictly stronger at zero syscall cost — the `stat` already returned it. **Current hole: mtime unchanged + content changed = permanently stale, with no test** (`cache_test.rs:632-659` sleeps 10ms first, so the same-mtime case is never exercised).
- (b) Expose `Surgeon::invalidate(&self, file: &Path)`. `AstCache::invalidate` (`cache.rs:450-453`) exists, is `pub`, and has **zero** production callers because `TreeSitterSurgeon` does not expose it.
- (c) **Preferred:** add `notify` and drive invalidation from filesystem events. The only option genuinely compatible with session-lifetime processes.
- (d) Bound the cache by **bytes**, not entries. `AstCache::new(100)` allows up to 200 (`:148-149` — `entries` and `vue_entries` each get `cap`) with no size bound; a tree is several times the source size.
- (e) Test: write file, capture mtime, write **different** content, restore the mtime via `filetime::set_file_mtime`, assert a reparse.

**7a.9 — File-size guard (`TS-A12`).** No `metadata.len()` check before `tokio::fs::read` + parse, and a parse timeout is **never cached** (the error propagates out of `spawn_blocking` at `cache.rs:315` before the `put` at `:326`), so a 2MB generated `.ts` file re-burns 500ms on **every** call, forever. Add a size guard with a typed error, and cache the negative result. Split the conflated message at `parser.rs:73-76`.

### WS-7a Acceptance

- **C-11** every fixture symbol's `name_point` indexes a byte **inside that symbol's name identifier** in the raw file.
- **C-12** a fixture per language whose declaration line contains `"héllo 🌍"` before the symbol name; `locate(semantic_path)` resolves correctly.
- **C-13** `impl Trait for Foo` in `impl.rs`, `struct Foo` in `types.rs`; `inspect("impl.rs::Foo")` is a non-empty body **or** a typed error — never `Ok("")`.
- **C-14 (round trip)** `p = file + "::" + sym.semantic_path`; `SemanticPath::parse(p)` is `Some`; `resolve_symbol_chain` returns a symbol with the **same** `byte_range`. **Catches `TS-A4`, `TS-A7`, `TS-A9`, `TS-A10`, `TS-A11` in one assertion.**
- **C-15** `inspect(p).start_line == locate(p).line`.

## WS-7b — Query-driven extraction

**`TS-B2` — the `tags.scm` bet is smaller than claimed.** Of the 20 per-language items in `symbols.rs`, **5 cleanly query-expressible, 6 need significant hand-authoring, 9 not expressible.** Upstream tags files carry none of the assumed visibility or test captures: rust 60 lines with zero, python **14 lines total**, java 20 with no modifiers or annotations, typescript 23 with no accessibility. The full per-item table is in `01-findings.md` under `TS-B2` — **read it before starting.**

**7b.1 — Corrected `LanguageGrammar` (`TS-B5`, `TS-B6`):**

```rust
pub struct LanguageGrammar {
    pub id: &'static str,
    pub extensions: &'static [&'static str],
    pub grammar: fn() -> tree_sitter::Language,
    /// ORDERED. TypeScript MUST be [javascript_tags, typescript_tags] --
    /// typescript/tags.scm is a 23-line SUPPLEMENT assuming the JS file is
    /// concatenated first, because the TS grammar extends the JS grammar.
    pub tags_queries: &'static [&'static str],
    /// The generic multi-grammar mechanism. Two vendored grammars ship one.
    pub injections_query: Option<&'static str>,
    pub post_process: Option<fn(&mut Vec<ExtractedSymbol>)>,
}
```

**7b.2 — The generic containment pass. The key insight (`TS-B3`, `TS-D2`).**

`tags.scm` emits a **flat, unordered** list of `(pattern_index, capture, range)`. `AuthService.login` does not exist in query output — only `AuthService` and `login` as independent matches. **If the containment pass is not implemented, every hierarchical semantic path in every agent's context becomes invalid at once.** Largest breakage risk in v6.

Nesting is **generic, not per-language**: a match whose byte range is strictly inside another's becomes its child. One pass handles Rust impls, Go receivers, TS classes, Java classes and Python classes uniformly.

**7b.3 — `post_process` hooks, honestly counted (`TS-B3`).** The "one hook per language" cap is wrong — **every** language exceeds it. With the containment pass doing nesting:

| language | hooks | what |
|---|---|---|
| Rust | 3 | cross-block impl merge; struct/enum/union kind disambiguation (tags collapses all to `@definition.class`); const/static (absent from tags) |
| Go | 2 | receiver→type binding (`TS-A6`); interface/struct kind from `type_spec.type` |
| Java | 2 | enum/record/annotation-type/constructor (absent from tags); annotation-span trim so `start_line` is the name row |
| TS/JS | 3 | `export` ancestry; constructor re-inclusion (upstream JS tags `:6` has `(#not-eq? @name "constructor")`); interface/enum/namespace/type-alias |
| Python | 1 | decorator-aware test classification |

**7b.4 — Fix Go extraction (`TS-A6`).** Bind `method_declaration.receiver` to the owning type — Pathfinder never reads it and `language.rs:50` sets Go `impl_kinds: &[]`, so `func (s *Server) Handle()` and `func (c *Client) Handle()` become `Handle` and `Handle#2` with nothing saying which is which. Add `const_spec`/`var_spec` — `resolve_name_node` fails on `const_declaration` and extraction finds nothing, so **Go package constants and variables are currently invisible to all three tools**. Upstream `go/tags.scm:40-42` already handles them.

**7b.4b — Fix Rust test detection and impl visibility (`TS-A8`).** Two defects in code the refactor touches, both otherwise faithfully reproduced:

- `is_rust_test` (`symbols.rs:595-611`) iterates `node.named_children()` on a `function_item` looking for `attribute_item`. Per `tree-sitter-rust-0.24.2/src/node-types.json`, attributes are **siblings**: `function_item children: ['function_modifiers','visibility_modifier','where_clause']`. **The loop can never match.** Rust test detection is in fact the name heuristic at `:612-616` alone, so `#[test] fn resolves_impl_method()` is classified `Function`.
- The tests that "prove" attribute detection are self-deceiving: `symbols_test.rs:234-279` and `:2260-2270` check only functions **also** named `test_*`, so they pass via the name heuristic. **Add a negative control** — an attributed function whose name does not match — before changing anything.
- `extract_impl_block` never calls `detect_access_level`: `symbols.rs:830` gives every impl method `AccessLevel::Public`, `:846` gives every impl block `Private`. `pub fn` and bare `fn` inside a Rust impl are indistinguishable, and `detect_rust_access_level` (`:415-444`) is dead for most Rust methods.

**7b.4c — Add a symbol cache (`TS-A13`).** `cached_parse` (`treesitter_surgeon.rs:94-105`) caches the **tree** only. On a hit it still clones tree and source and re-runs `extract_symbols_from_tree` in a fresh `spawn_blocking`. Every `locate`, `trace` and `inspect` pays a full AST walk plus the allocation tree (`symbols.rs:14` capacity 64, plus a `HashMap` per recursion level at `:312`) plus a blocking-pool round trip. `read_symbol_scope` pays it once then calls `resolve_symbol_chain` twice (`:130`, `:139`).

Cache the extracted symbol vector alongside the tree, keyed identically (size + mtime + content hash per 7a.8a). Extraction is benchmarked only in isolation, never in the `cached_parse` composite — **add a warm-cache benchmark** so the improvement is measurable and WS-10's latency targets have a baseline.

**7b.5 — Relocate before measuring size (`TS-B8`, `TS-D4`).** ~565 lines of `symbols.rs` are JSX (`:1279-1480`), HTML (`:1180-1273`, `:1483-1522`), CSS (`:1533-1693`) and multizone (`:1102`, `:1138`) — **none with upstream query support** (`tree-sitter-html` ships only `highlights.scm` + `injections.scm`; `tree-sitter-css` only `highlights.scm`).

Extract to `symbols/vue.rs`, `symbols/jsx.rs`, `symbols/css.rs` as an **explicit task**. A line-count target with no relocation plan is an incentive to delete working code.

**7b.6 — KEEP and GENERALISE `vue_zones.rs` (`TS-D5`, reverses the draft).** Cannot be subsumed — the grammars ship no tags files. It is 278 lines of correct code using `Parser::set_included_ranges` with **global byte offsets**, so no downstream offset arithmetic is needed (`:1-6`, `:238-250`) — exactly the primitive an `injections.scm` engine needs.

Rename `injection.rs`. Replace the hardcoded three-zone `scan_vue_zones` (`:153-159`) with injections-driven range discovery. Vue, Svelte, Astro, HTML, Markdown fences and tagged template literals then fall out of **one** mechanism. Fix 7a.7's bugs first.

**7b.7 — Registry ID relation (`TS-C3`).** An "identical ID sets" assertion **fails on day one**: `SupportedLanguage` has 8 variants but `as_str()` produces 7 (`TypeScript` and `Tsx` both → `"typescript"`, `language.rs:126`); `Vue` → `"vue"` uses the TypeScript grammar (`:140`) and has **no LSP plugin**; `JavaScript` shares `TS_NODE_TYPES` (`:154`). Specify as `grammar_id -> lsp_language_id`, **N:1**.

**7b.8 — Runtime `.so` loading: DEFERRED, separate change.** Not in the same PR. When done: opt-in config, trusted directory, checksum pinning, `warn!` per load, **and ABI version rejection** (`TS-C6`) — `abi_version()` outside `MIN_COMPATIBLE_LANGUAGE_VERSION..=LANGUAGE_VERSION` is a hard incompatibility, not graceful degradation.

**7b.9 — `inspect` boundary definition (`TS-C1`).** The current wording ("byte-identical to the file slice") is tautological — `read_symbol_scope` **is** a file slice (`treesitter_surgeon.rs:150-162`), and it passes for the phantom `0..0` symbol returning `""`. What varies is the **boundary**, inconsistent per language (full table in `01-findings.md`): Rust excludes attributes and doc comments; Java **includes** annotations; exported TS **excludes** `export`, so the text is not valid to paste back.

Restate: a contiguous slice that (a) starts at or before the first attached decorator/annotation/attribute, (b) includes the preceding doc comment when `include_docs=true`, (c) includes the visibility/export prefix, (d) is syntactically self-contained. Requires an `attached_range` computation — **real work, budget it.**

### WS-7b Acceptance

- **Not** byte-identical extraction (`TS-B4` — unsatisfiable, and enforcing it forces the query author to *reproduce* current bugs). Instead: **differential review.** Per-fixture symbol diff; every ADDED symbol justified as a fix with a linked finding ID; every REMOVED symbol justified or restored; **zero unexplained deltas**. C-11 and C-14 pass before and after.
- Zero `match lang { SupportedLanguage::Rust => ... }` outside the grammar registry.
- Adding a language = one grammar entry + queries + hooks + fixture. **Prove it:** add Kotlin or Ruby end-to-end in a single commit and record the diff size as the new cost-per-language baseline. State the honest cost in the README: *"one `.scm` file plus one hand-written kind map plus one to three `post_process` hooks plus one fixture."*

---

## Scope Card: WS-8 — Search layer re-scope

- **Deliverables:** language tables driven from the registry; walk halved; budgets added
- **Write Scope:** `crates/pathfinder-search/**`, `crates/pathfinder/src/server/tools/navigation/mod.rs`
- **Shared Reads:** WS-5, WS-7 outputs
- **Hard Dependencies:** WS-5 (`SearchResult` shrink touches `tools/search.rs`)

**`RSP-B1` — the original scoping was correct about the crate and wrong about the layer, and its acceptance criteria were vacuous** (already satisfied at HEAD with no code change, `RSP-B4`).

`crates/pathfinder-search` genuinely contains **zero** language-specific logic (`RSP-E1`), is a library not a subprocess, is injection-safe, and is unaffected by session lifetime (`RSP-E2`, `RSP-E3`). **The coupling is one layer up**, in `navigation/mod.rs` (941 lines, ~380 per-language), which survives the cut and is called by all three surviving tools' fallback paths.

### 8.1 — Drive the per-language tables from the registry

```
mod.rs:43-56    SOURCE_FILE_EXTENSIONS      12 hardcoded exts; filters every fallback result
                                            (references.rs:125, impact.rs:189, :253,
                                             deep_context.rs:284)
mod.rs:78-119   is_workspace_file           hardcoded Rust stdlib prefixes + node_modules/, vendor/
mod.rs:190-367  keywords_for_language       6 languages, generic 7-keyword fallback at :365 --
                                            language #8 floods its own candidate list
mod.rs:372-383  language_to_file_glob       match language { "rust" => "**/*.rs", ... }
mod.rs:389-394  java_resolve_pattern        a 900-character hand-written Java regex
mod.rs:400-416  candidate_definition_pattern per-language definition regexes
mod.rs:433-513  definition_patterns         per-extension regex tables, 4-5 patterns each
definition.rs:731-744  a SECOND, inline, DIVERGENT copy inside grep_impl_method -- DELETE
```

Drive all from the WS-7 `LanguageGrammar` registry. **Until this lands, WS-7b's "one grammar entry + fixture" claim is false end-to-end.**

**Note:** `apply_filter_mode` (`search.rs:349`, **not** `apply_n` — that name does not exist in the tree) is Tree-sitter-driven and **survives** the cut, because `search_codebase_impl` is called by `references.rs:100` and `impact.rs:166` with `FilterMode::CodeOnly` by default. It moves to `grep_support.rs` in WS-5.0.

### 8.2 — Performance and budgets (`RSP-A10`)

- **(a) Delete the first walk.** `walk_files` (`ripgrep.rs:484-578`) runs **two** complete traversals per search — `:513` (gitignore off) and `:541` (on). The first exists solely to compute `gitignored_skipped` (`:572`), whose only consumers (`types.rs:185`, `search.rs:187`) are deleted by WS-5. **After WS-5 it is 100% dead work on every fallback.** Collapse to one; remove `binary_skipped`, `gitignored_skipped`, `other_skipped`, `files_in_scope` from `SearchResult` (`pathfinder-search/src/types.rs:104-116`).
- **(b) Wall-clock budget.** No `tokio::time::timeout` exists anywhere around `scout.search` or `search_codebase_impl`. Add `deadline: Instant` to `SearchParams`; check between files at `ripgrep.rs:617`; return `truncated: true`. Default 2s, `search.budget_ms`.
- **(c) Fan-out budget.** Worst case today: `trace` degraded = 21 searches = **42 full workspace walks** (`impact.rs:365-389` + up to 20 candidates from `mod.rs:166`); `inspect` degraded = up to 40; `locate` degraded = up to 34. Share **one** deadline (5s) and **one** counter (8) across `run_grep_fallbacks`, `attempt_grep_fallback` and `fallback_definition_grep`.
- **(d) Regex limits.** `build_matcher` (`ripgrep.rs:455-474`) sets only `case_insensitive(false)`. Set `size_limit` and `dfa_size_limit` — agent-supplied regex is untrusted input; classic ReDoS is impossible (finite automata) but a pathological pattern can burn compile CPU and thrash the 2MB DFA cache with no wall-clock bound.
- **(e) Batch candidate resolution.** `grep_outgoing_fallback` (`impact.rs:299`) and `attempt_grep_fallback` (`deep_context.rs:227`) issue N single-pattern searches over the same file set. One alternation over one walk is equivalent: O(1) walks instead of O(N).
- **(f)** Fix the vestigial cache key `format!("{pattern}\0ci=false")` (`ripgrep.rs:462`) — harmless today, silently collides the moment anyone parameterises case sensitivity.

**`RSP-D3` — sequence (b) and (c) before WS-11.** The cold-start suite makes grep fallback the designated path; without budgets it measures timeouts and the plan misreads that as a readiness failure.

### 8.3 — Confidence labelling

Every ripgrep-derived result carries `Confidence::Heuristic` per Contract 2. **The types live in WS-4's scope** — WS-8 supplies values, WS-4 owns fields (`RSP-B3`).

Per Contract 2 Rule 5, heuristic entries **retain their `snippet`**. Not negotiable.

### 8.4 — Documentation

Reclassify `pathfinder-search` from "tool backend" to "internal engine" in the crate `//!` docs. Note in ADR-003 that `ALWAYS_EXCLUDED_DIRS`/`_DIR_NAMES` (`pathfinder-common/src/types.rs:17-35`, `:45-63`) are per-**ecosystem**, not per-grammar — a place the "one entry per language" claim leaks. Do not restructure.

### Acceptance

- `navigation/mod.rs` has zero hardcoded per-language tables.
- `definition.rs:731-744` duplicate deleted.
- One walk per search; `SearchResult` shrunk.
- The fan-out budget caps a degraded `trace` at ≤ 8 searches.
- **C-12 (conformance):** the grep fallback returns correct results for the WS-7b new language with the LSP binary **forcibly absent**.

---

# LEVEL 4

## Scope Card: WS-9 — Conformance suite

- **Deliverables:** table-driven harness; fixtures for 6 languages; CI-generated language tier table
- **Write Scope:** `crates/pathfinder/tests/conformance/**`, `crates/pathfinder/tests/fixtures/conformance/**`
- **Shared Reads:** all
- **Hard Dependencies:** WS-6, WS-7b, WS-8

**Purpose:** make "which languages are mature" a CI artifact instead of an opinion (INV-4). Also gives WS-11 its regression harness for free.

### Fixture shape

One minimal repo per language under `tests/fixtures/conformance/<lang>/`, containing:
- a trait/interface **and** its concrete impl **in a different file** (the `mod.rs` / `lawyer_impl.rs` shape)
- a caller chain ≥ 2 deep
- a non-call reference (field access / type annotation / import)
- a symbol with genuinely zero callers
- an annotated/decorated declaration (`@Override`, `#[test]`, `@pytest.fixture`) — `TS-A1`
- a declaration line with non-ASCII before the symbol name — `TS-A2`
- an impl block whose type is declared elsewhere — `TS-A3`

### Required assertions

| # | assertion | defends |
|---|---|---|
| C-1 | `locate` on a semantic path returns the definition file:line | addressing |
| C-2 | `locate(file, line)` returns the correct enclosing semantic path | addressing |
| C-3 | `inspect` returns a self-contained slice per the 7b.9 boundary definition | `TS-C1` |
| C-4 | `trace(scope=callers)` returns the known caller, `Confidence::Verified` | call hierarchy |
| C-5 | `trace(scope=references)` includes the known **non-call** reference | references ≠ calls |
| **C-6** | **goto-implementation on the interface resolves to the impl file, NOT the declaration file** | **the v6 product thesis** |
| C-7 | zero-caller symbol returns `Evidence{Verified, []}` | INV-1 (real zero) |
| **C-8** | **cold start:** same query within 500ms of boot returns `not_ready` or the correct answer — never wrong-but-confident | INV-1 (fake zero) |
| C-9 | probe reaches `Ready` within `readiness_budget()` | WS-3.1 |
| C-10 | with the LSP binary absent, every tool returns `unavailable` + install hint, never a silent empty | INV-1 |
| C-11 | `name_point` indexes a byte inside the symbol's name identifier | `TS-A1`, INV-7 |
| C-12 | non-ASCII declaration line still resolves | `TS-A2`, INV-7 |
| C-13 | `impl Trait for TypeElsewhere` — `inspect` returns a body or a typed error, never `Ok("")` | `TS-A3` |
| **C-14** | **round trip:** generate path → parse → resolve → same `byte_range` | `TS-A4/A7/A9/A10/A11`, `TS-C2` |
| C-15 | `inspect(p).start_line == locate(p).line` | `TS-A7` |
| **C-16** | **warm path:** with the LSP fully `Ready`, no query returns a confidently-wrong empty on the annotated / non-ASCII / cross-file-impl fixtures | **`TS-D6`, INV-7** |

**C-6, C-8, C-14 and C-16 are the four that matter.** C-6 is the only measured differentiator. C-8 is the invariant WS-3 exists to establish. C-14 catches five findings at once. C-16 catches the class the original plan could not see.

### Tiering

- **Tier 1 (supported):** all 16 pass
- **Tier 2 (partial):** C-1..C-3, C-11, C-14, C-15 pass; some LSP assertion fails. Documented, shipped, gap named in the README.
- **Unsupported:** anything less. Not advertised.

### Acceptance

- CI runs the suite on every PR.
- README language table is **CI-generated**.
- Adding a language without adding its fixture **fails CI**.
- The WS-7b new language is the sixth fixture, proving the process.

---

## Scope Card: WS-10 — Trace cost

- **Deliverables:** median `trace` payload < 2,000 chars with zero information loss; explicit `declared:` vs `impl:` rendering
- **Write Scope:** text/JSON rendering functions in `navigation/impact.rs`, `navigation/overview.rs`
- **Shared Reads:** WS-4 outputs
- **Hard Dependencies:** WS-4

**`RSP-D1` — the draft's specific cut attacks convergence, the one measured property.** Read that finding in full first.

### Where the bytes actually go

```
per ImpactReference, JSON  : 326 bytes
per ImpactReference, text  : 247 bytes
per reference, both        : 573 bytes    <- 183 (32%) is pure redundancy
```

- 55 bytes: `file` is embedded verbatim **inside** `semantic_path` (`mod.rs:535`, `:537`, `impact.rs:197`) and **also** emitted as its own field (`types.rs:536`)
- 55 bytes: the text renderer prints it twice again — `impact.rs:1267-1270`
- 73 bytes: `snippet` in JSON (`types.rs:540`) **and** in text (`:1271-1273`)

### Tasks

**10.1 — Remove duplication, not facts.**
- **(a)** Emit each file path **once** per reference. ~19%, zero information loss.
- **(b)** Non-overlapping channels per Contract 3 Rule 2: **text owns the list; JSON owns counts, status, confidence, truncation, pagination.** The current duplication rationale (`impact.rs:1595-1596`, "so agents can act without parsing structured_content") is real — satisfy it by giving the list to text, not by emitting twice. ~57%.
- **(c)** Defaults: `max_depth` 1 (cap 2), `max_references` 20 (cap 500).

Combined: 20 refs × (573 − 110 − 326) ≈ 2,740 + ~400 header ≈ 3,100; depth-1 typically halves the count. **Median under 2,000 without deleting a single fact.**

**10.2 — What must be kept.**
- **Drop `snippet` for `Confidence::Verified` only.** For LSP entries it is `CallHierarchyItem.detail` (`impact.rs:454-457`) — the containing module, already in `semantic_path`.
- **RETAIN `snippet` for `Confidence::Heuristic`.** It is `m.content`, the actual matched source line (`impact.rs:202`, `references.rs:188`), and the **only** thing letting the agent decide whether a candidate is a real call, a comment, an import, or a same-named symbol on another type. Strip it and the agent either believes it (confidently wrong) or issues up to 20 `inspect` calls / falls back to host grep — **either way the exact-repeat rate rises** (K-5). Contract 2 Rule 5.
- **Retain and render `hint`.** Currently JSON-only (`RSP-A1`); the "one fact per line" rewrite is exactly where it would be lost for good.
- **Retain `*_truncated` and add the continuation** — Contract 5.

**10.3 — Make the differentiator explicit.** The single measured advantage over CodeGraph, currently only implicit:

```
resolution:
  queried:  Lawyer
  declared: crates/pathfinder-lsp/src/lawyer.rs:34
  impl:     crates/pathfinder-lsp/src/client/lawyer_impl.rs:18   <- the runtime type
  note:     name-matching tools commonly return the declaration site here
```

**Depends on WS-6.4 / `LSP-A7`** — `response_parsers.rs:116-132` currently discards every location but the first (`arr.first().cloned()` at `:127`), which is exactly the data this needs.

**10.4 — Post-discovery invocation shape.** Accept a list of candidate locations, return which is the runtime answer. The routing-product shape: CodeGraph or grep for breadth, Pathfinder for the final resolution hop.

**10.5 — `overview` depth (`RSP-D2`).** `overview.rs:180` hardcodes `max_depth: 2`. At a cap of 2, overview keeps its cost while `callers` halves, so overview becomes the expensive scope and dominates the payload median. **State the intended depth explicitly — recommend 1.**

### Acceptance

- Median `trace` payload < 2,000 chars on the bench corpus.
- `max_depth > 2` rejected with a delegation hint.
- Every C-6 conformance case emits the explicit `declared:` vs `impl:` distinction.
- `Confidence::Heuristic` entries always carry `snippet`.
- `hint` appears in the text channel whenever present.

---

# LEVEL 5

## Scope Card: WS-11 — Re-validation

- **Deliverables:** full re-run against the 485-run baseline; extended `resolve` suite; new cold-start and warm-path suites
- **Write Scope:** `bench/**`
- **Shared Reads:** all
- **Hard Dependencies:** WS-0 through WS-10

### Tasks

**11.1** Re-run all 6 suites, 3 repos, 3 models. **Same harness, no methodology change** — comparability with the 485-run baseline is the point.

**11.2** Add Gemini arms. Fully blocked before; WS-1.3 + WS-5 should unblock. **A Gemini arm that merely loads is itself a result.**

**11.3** Extend `resolve` from 8 to **≥ 20 tasks**. n=8 settled nothing. New tasks target the C-6 shape: interface declared in one file, implemented in another, with a plausible wrong answer one grep away.

**11.4 — Cold-start suite (mandatory).** Every task re-run with the first tool call within 5s of server start. Grade on correctness **and** on whether the tool ever returned a confidently-wrong empty. **Any single occurrence is a hard failure of v6 regardless of every other number.**

**11.5 — Warm-path suite (mandatory, new — `TS-D6`).** The cold-start suite measures the wrong axis for the addressing bugs. Run against a **fully-warm** LSP: the Java `@Override` fixture, the Rust `impl Trait for TypeInAnotherFile` fixture, the non-ASCII declaration fixture. Same hard-failure rule.

**11.6** Track exact-repeat rate explicitly. **If WS-5's smaller responses or WS-10's payload diet regress it, the cut was wrong and must be partially reverted (K-5).** Instrument the `snippet`-retention decision (10.2) as an explicit A/B so the cause is isolable.

### Acceptance

The success-criteria table in `00-plan.md` §7, in full. Every row.
