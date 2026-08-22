# Pathfinder v6 — Audit Findings Register

**Status:** Frozen reference. Read-only during BUILD.
**Produced by:** three independent sovereign audits (LSP layer, Tree-sitter layer, search layer + response contract), each with full read access to the workspace at commit `b50e9a5`.
**Purpose:** every scope card in `02-scope-cards.md` cites finding IDs from this register. A task with no finding ID is not in scope. A finding with no owning scope card is a gap in the plan.

---

## How to use this document

1. Every claim carries `file:line`. **Verify before acting — the tree moves.**
2. Findings are **evidence**, not instructions. The instruction is in the scope card.
3. If a finding is wrong on inspection, **do not silently skip it** — record the correction here and escalate, because at least one scope card's acceptance depends on it.
4. `INFERENCE` marks a claim the auditor could not fully verify. Treat as a hypothesis to confirm first.

## ID scheme

| prefix | layer |
|---|---|
| `LSP-*` | `crates/pathfinder-lsp/**` + LSP call sites |
| `TS-*` | `crates/pathfinder-treesitter/**` + Surgeon consumers |
| `RSP-*` | `crates/pathfinder-search/**` + response contract |

| suffix | class |
|---|---|
| `A<n>` | Critical gap — produces a wrong answer today, plan did not address it |
| `B<n>` | Plan error — the v6 draft was factually wrong about the code |
| `C<n>` | Under-specified — directionally right, not executable as written |
| `D<n>` | Risk introduced by the plan's own changes |
| `E<n>` | Confirmed fine — checked, genuinely OK, do not re-audit |
| `Z<n>` | Consolidated index / cross-cutting inventory |

---

## 0. The three findings that stop work immediately

Build-breaking or invariant-destroying. Read before touching anything.

| id | one line | consequence if the draft were executed as written |
|---|---|---|
| `LSP-B0` | Deleting `idle_timeout_task` deletes shutdown **and** zombie reaping | `shutdown()` becomes a no-op; leaked jdtls JVMs on macOS; hung processes never detected |
| `RSP-A3` / `TS-B1` | `tools/symbols.rs` **is** the `inspect` handler | deletes one of the three surviving tools; build fails |
| `RSP-A4` | `tools/search.rs` and `tools/find_symbol.rs` are called by the survivors | deletes `search_codebase_impl` (trace fallback) and `find_symbol_impl_inner` (did-you-mean); build fails |

---

# LSP LAYER

## LSP-A — Critical gaps

### LSP-A1 — INV-1 covers empty; the real failure mode is PARTIAL
A warming rust-analyzer does not return empty. It returns *some*. Nothing stamps a partial-but-non-empty result as unverified.

- `impact.rs:790-791` — the only false-negative guard fires when **both** directions are empty: `if state.incoming.as_ref().is_none_or(Vec::is_empty) && state.outgoing.as_ref().is_none_or(Vec::is_empty)`
- `impact.rs:750-752` — on any non-empty `prepare`, sets `state.degraded = false; state.degraded_reason = None;` unconditionally, before BFS runs
- `impact.rs:787` — `state.lsp_bfs_ran = true`
- `impact.rs:1143-1151` `compute_verified_flags` — `(Some(_), false, _) => Some(true)`. Empty vec + `degraded=false` ⇒ CONFIRMED ZERO
- `references.rs:317-366` — the non-empty `Ok(locations)` arm does zero readiness checking; falls through to `references.rs:705-711` `degraded: false, lsp_readiness: Some("ready")`

Mechanism: outgoing calls resolve from the file-local syntax tree; incoming calls and cross-file references need the global index. Mid-index you get `outgoing=[3]`, `incoming=[]`. The both-empty guard does not fire. The agent receives `incoming_verified: true` on an empty list, which the project's own guidance defines as "safe to conclude no callers."

A request-entry `ensure_ready` does **not** fix this: a probe passing at t=0 says nothing about whether the incoming-call index is built.

### LSP-A2 — Request timeouts inside BFS are swallowed; independent fake-zero channel
At the `pathfinder-lsp` layer a timeout **is** typed and the caller **does** learn (`lifecycle.rs:1045-1063` → `LspError::Timeout`). The information is destroyed one layer up.

- `impact.rs:582-597` — the `Err(e)` arm inside the BFS loop increments a counter, logs `warn!`, and **continues**. Never returned.
- `impact.rs:600` — `(references, max_depth_reached)`. No error channel out of `bfs_call_hierarchy`.
- `impact.rs:521-528` — 30s wall clock (`BFS_TIMEOUT_SECS`, `impact.rs:19`) → `break`, partial results, no flag
- `impact.rs:531-539` — `BFS_CONSECUTIVE_FAILURE_LIMIT = 2` (`impact.rs:26`) → `break`, partial, no flag
- `impact.rs:648-659` — `let Ok(items) = self.lawyer.call_hierarchy_prepare(...) else { continue; }` — an entire implementation silently dropped
- `impact.rs:660-662` — `let Some(item) = items.first() else { continue; }` — same

A depth-3 BFS hop 7 timing out yields a silently truncated graph marked verified. Gating only the **first** request per handler does not reach hop 7: `call_hierarchy_request` calls `ensure_process` (`lifecycle.rs:1166`) then `request(..., 10s)` (`lifecycle.rs:1175`) with no readiness gate.

### LSP-A3 — Document sync does not exist, and removing idle-kill makes staleness permanent
- No `textDocument/didChange` anywhere in the workspace. `rg didChange` matches only `test-mock-lsp/src/main.rs:157` and a comment at `capabilities.rs:25`.
- `document.rs:144-151` — `did_open` sends `"version": 1` with disk content, once.
- `doc_versions` is `(String, AtomicI32)` (`mod.rs:363`) initialised to 1 (`document.rs:141`) and **never incremented**. The counter is dead.
- No file watching. `FileEvent` / `FileChangeType` (`types.rs:156-175`) are declared, documented as "for `workspace/didChangeWatchedFiles`", and have **zero** non-test references. The client capability is not declared in the initialize payload (`process.rs:843-856`).
- The only thing bounding staleness today is the 15-minute idle kill (`background.rs:17`) plus the `doc_versions` purge on every teardown (`background.rs:429-437`, `:353-362`, `:74-83`, `lifecycle.rs:987-1005`).

**H1 — open/close reordering makes it permanent.** `document.rs:39-80`: `Drop` removes the `doc_versions` entry synchronously at `:62`, then `tokio::spawn`s the didClose at `:74-78`. A concurrent `did_open` between the remove and the spawned send puts `didOpen(v1) → didOpen(v1) → didClose` on the wire. `doc_versions` says open; the server says closed. Every later `did_open` dedups at `document.rs:131` returning `Ok(false)` — **the file is never reopened for the rest of the session and every position query runs against a closed document.**

**H2 — TOCTOU on the dedup.** `document.rs:131` `contains_key` then `:139` `insert` is not atomic. Two concurrent calls both see false, both `didOpen` with no intervening `didClose` — the exact protocol violation the comment at `document.rs:126-137` exists to prevent — and both guards get `owns_open = true`, so both send `didClose`.

**H3 — external edits.** Files change from the user's editor, `git checkout`, a build. Per spec, a file the client has `didOpen`'d is the client's responsibility. Pathfinder never resolves it.

Severity: removing idle-kill converts a bounded 15-minute error window into an unbounded one.

### LSP-A4 — Malformed and result-less responses become confirmed zeros inside the dispatcher
- `protocol.rs:230` — `Ok(message["result"].clone())`. A response with neither `result` nor `error` yields `Value::Null`, flowing to `response_parsers.rs:112` (`Ok(None)`), `:224`, `:289`, `:339` (`Ok(Vec::new())`). **A protocol violation is laundered into a verified zero, below every tool.**
- `protocol.rs:197-205` — a response whose `id` is not a u64 logs `warn!` and returns. The pending oneshot is never resolved and never removed; the caller waits the full 10s/30s. Some servers echo the id as a string.
- `protocol.rs:179-194` — dispatch priority routes anything with both `id` and `method` to the server-request channel. A server echoing `method` in its response gets misrouted to `registration_watcher_task`, which replies `{"result": null}` (`background.rs:196-209`); the original caller times out.

### LSP-A5 — `positionEncoding` is never negotiated; byte columns are sent as UTF-16 offsets
- `rg positionEncoding|position_encoding|utf-16` across the workspace: **zero matches**. The initialize payload (`process.rs:837-857`) has no `general.positionEncodings`. Per LSP 3.17 the negotiated default is therefore `utf-16`.
- Outbound: `column.saturating_sub(1)` sent as `position.character` at `lawyer_impl.rs:68`, `:128`, `:224`, `:293`. `column` originates from `SymbolScope.name_column` (`references.rs:300`, `impact.rs:1033`, `deep_context.rs:56`), which is `Node::start_position().column` (`symbols.rs:144`, `:239`, `:821`, `:837`) — tree-sitter `Point.column` is a **byte** offset.
- Inbound: `range.start.character` read and `+1`'d at `response_parsers.rs:88-89`, `:155`, `:246-248`, `:372-376` with no decoding.

Any non-ASCII byte before the target identifier on the same line desynchronises the offset. The server resolves at the wrong position and returns nothing (fake zero) or the neighbouring symbol (confidently wrong resolution — the exact failure v6 exists to eliminate). Silent in both directions. Mirror of `TS-A2`.

### LSP-A6 — `goto_definition` is not a readiness proxy for callHierarchy/references
The drafted probe is `textDocument/definition`. Two tools already use that signal as a warm-up disambiguator and **disagree about what it means**:

- `deep_context.rs:85-88` — `if matches!(probe, Ok(Some(_))) { degraded = false; degraded_reason = None; }` with `dependencies` still EMPTY. `inspect` returns "zero dependencies, not degraded" whenever goto_definition happens to resolve while callHierarchy is cold.
- `impact.rs:847-868` — same probe, opposite conclusion: `state.degraded = true`.

`goto_definition` on a `didOpen`'d file is answerable from the file-local index. `callHierarchy/incomingCalls` and `textDocument/references` need the global index. Making the definition probe the Ready gate formalises the defect. Readiness must be **per-capability**.

### LSP-A7 — `locate` silently discards all but the first definition location
`response_parsers.rs:116-132` — when the response is an array, `arr.first().cloned()` (`:127`). The `tracing::debug!` at `:122` acknowledges the loss. `locate` uses this parser (`lawyer_impl.rs:98`).

For the `mod.rs` vs `lawyer_impl.rs` shape that is the v6 product thesis: if the server returns `[declaration, impl]`, Pathfinder picks the declaration and reports it as *the* answer with no indication a second location existed. **The data needed for the explicit `declared:` / `impl:` distinction is thrown away in the parser.**

### LSP-A8 — `request()` and `notify()` check the wrong liveness flag
- `lifecycle.rs:1029` and `:1081` gate on `state.reader_handle.is_finished()`.
- `mod.rs:53-54` documents that field: "stored as `reader_handle` for compatibility with stale-reader detection".
- The real flag is `state.reader_alive` (`mod.rs:59`), set false at `lifecycle.rs:659` and `background.rs:53`, read in exactly one place — `background.rs:57`.

Window: reader hits EOF → `cancel_for_language` (`process.rs:973`) → reader exits → supervisor begins teardown. Throughout teardown the supervisor handle is not finished, so `request()` issues into a dead pipe and blocks the full timeout. The resulting Timeout is relabelled "warming_up" by `references.rs:829-835` / `impact.rs:1056` — wrong cause, wrong guidance. One-line fix: read `reader_alive`.

### LSP-A9 — Silent drops and 1:1 defaults in `parse_references_response`
- `response_parsers.rs:348-350` — reference with no `uri` → `continue`, uncounted
- `response_parsers.rs:354-356` — `uri.to_file_path()` failure → `continue`. jdtls returns `jdt://contents/...` for anything in a JAR; those vanish with no signal. **Undercount presented as complete.**
- `response_parsers.rs:366-376` — missing `start.line`/`start.character` → `map_or(1, ...)`. A malformed range becomes a plausible, specific, wrong `file:1:1`.
- `response_parsers.rs:357-360` — when `strip_prefix(workspace_root)` fails the **absolute** path is used. `references.rs` applies no `is_workspace_file` filter (unlike `impact.rs:557-562`), so out-of-workspace paths are returned as workspace references.

### LSP-A10 — `$/progress` can be silently lost
`protocol.rs:16` `NOTIFICATION_CHANNEL_CAPACITY = 64`, a broadcast channel. `background.rs:161-167` handles `RecvError::Lagged(n)` with a warning and continues. rust-analyzer emits hundreds of `$/progress` during crate-graph build. A missed `end` means `indexing_complete` never flips via progress and falls back to the timeout heuristic (`lifecycle.rs:438-465`, documented as `IndexingCompletionSource::TimeoutFallback`, `types.rs:16-32`).

Consequence: any `ReadinessSignal::WorkDoneProgress` design is unsound on a lossy 64-slot channel. Either widen/replace the channel or state that a missed `end` degrades to probe-only.

### LSP-A11 — Every server→client request is answered `{"result": null}` (partly INFERENCE)
`protocol.rs:179-193` routes all id+method messages to the server-request channel. `background.rs:567-623` `extract_registration_action` sets `response_id: id` unconditionally (`:569`, `:621`) regardless of method and matches only register/unregister (`_ => {}` at `:615`). `background.rs:196-209` sends `build_registration_response(id)` = `{"jsonrpc":"2.0","id":id,"result":null}` (`:626-632`).

So `workspace/configuration` (spec: must return `LSPAny[]`), `workspace/workspaceFolders` (spec: folder list or null-meaning-none), `window/showMessageRequest`, `workspace/applyEdit` all receive `null`. `window/workDoneProgress/create` → null is correct; the others are not. `workspace/workspaceFolders` → null contradicts `workspace.workspaceFolders: true` declared at `process.rs:850`.

INFERENCE: gopls and jdtls both send `workspace/configuration`; a null reply typically silently changes analysis behaviour. Not verified against a live server.

## LSP-B — Plan errors

### LSP-B0 — HIGHEST BLAST RADIUS. Deleting `idle_timeout_task` destroys shutdown and zombie reaping
The v6 draft asserted "the existing `shutdown()` broadcast tears down every child LSP" and listed `idle_timeout_task` for deletion. Both halves are wrong.

`idle_timeout_task` (`background.rs:455-475`) **is** the shutdown handler. Its `tokio::select!` has exactly two arms:

```
463:  _ = shutdown_rx.recv() => { shutdown_all_processes(...).await; break; }
469:  () = tokio::time::sleep(IDLE_CHECK_INTERVAL) => {
470:        reap_dead_processes(...).await;
471:        reap_idle_processes(...).await;
472:  }
```

It is the **only** caller of `shutdown_all_processes` (`:465`) and the **only** caller of `reap_dead_processes` (`:470`). `LspClient::shutdown()` (`lifecycle.rs:258-271`) only flips an `AtomicBool` and sends on the broadcast; `lifecycle.rs:87` is the only `.subscribe()` in production code.

Delete the task as originally instructed and:
1. `shutdown()` becomes a no-op. No child is sent `shutdown`/`exit`, no pending request cancelled, no zombie reaped. On Linux `prctl(PR_SET_PDEATHSIG)` (`process.rs:1052-1064`) saves you; on macOS you leak a jdtls JVM and a rust-analyzer per host restart. The module doc at `process.rs:1049-1051` explicitly names "the idle timeout task will eventually clean up" as the relied-upon fallback.
2. `reap_dead_processes` is gone — a process that dies **without** its stdout closing (SIGSTOP, hung JVM) is never detected and its `ProcessEntry::Running` stays in the map forever. `request()` keeps issuing into it and timing out for the whole session. This is precisely the failure that "no idle-kill means a dead LSP is a hard failure" is supposed to guard against, and the guard is what gets deleted.

**Correct instruction:** keep the task, rename it `process_supervisor_task`, delete only `reap_idle_processes` (`background.rs:389-452`), `collect_idle_languages` (`:260-280`), and `DEFAULT_IDLE_TIMEOUT` (`:17`). Keep the select, keep `shutdown_all_processes`, keep `reap_dead_processes`.

### LSP-B1 — "The correct probe logic already exists in the wrong crate" is false
`health.rs:557 probe_language_readiness` does not check that the query answered. It checks that it did not error:

```
health.rs:594:  if result.is_err() { return (false, false); }
```

`Ok(None)` — the LSP explicitly saying "I cannot resolve this" — is treated as **PASS**. It is a **liveness** check, not a behavioural one.

Worse, the position is hardcoded:

```
health.rs:581:  .goto_definition(self.workspace_root.path(), &file_path, 1, 1)
```

Line 1, column 1. For Rust that is `//!` or `use`; for Go and Java, `package`. `Ok(None)` is the *expected* result, so **the probe passes for the wrong reason on every language.** Porting as-is yields a Ready gate that returns Ready for a server that has resolved nothing.

### LSP-B2 — "issue `textDocument/definition` on a known symbol" is not implementable in the target crate
`pathfinder-lsp` has no dependency on `pathfinder-treesitter`. `crates/pathfinder-lsp/Cargo.toml:20-40`: pathfinder-common, async-trait, thiserror, serde, serde_json, tokio, tracing, url, dashmap, parking_lot, schemars, command-group, which, toml.

The only symbol-position source in the workspace is `Surgeon`/`extract_symbols` in `pathfinder-treesitter`, which depends on nothing in `pathfinder-lsp` — so the edge is addable without a cycle, but it must be added deliberately and the owning scope card must include `Cargo.toml`.

`find_probe_file` (`health.rs:657-717`) returns a **file**, not a position. Porting it satisfies half the requirement; without a symbol position the probe degrades back to (1,1), i.e. `LSP-B1`.

### LSP-B3 — The capability-declaration premise is stale, and one required capability is not declared at all
Actual payload, `process.rs:843-856`:

```
"definition":    { "dynamicRegistration": false, "linkSupport": false }   <- already false
"references":    { "dynamicRegistration": true }                          <- true
"callHierarchy": { "dynamicRegistration": true }                          <- true
"publishDiagnostics": { ... }
```

**`textDocument.implementation` is absent entirely.** There is no `dynamicRegistration` to flip. A spec-strict server may omit `implementationProvider` when the client does not declare `textDocument.implementation` — which is the capability behind `goto_implementation` (`lawyer_impl.rs:264-330`), behind trait-method expansion (`impact.rs:621-629`), behind conformance C-6, which is the only measured differentiator against CodeGraph. **Declaring it is a prerequisite for the product thesis.**

Also missing: `textDocument.synchronization` (yet didOpen/didClose are sent), `workspace.configuration` (yet null replies are sent — `LSP-A11`), `workspace.didChangeWatchedFiles` (`LSP-A3`).

### LSP-B4 — `initialization_options()` cannot be a pure, argument-less data accessor
The drafted signature was `fn initialization_options(&self) -> Option<serde_json::Value>`. Reality:

- **Python:** `detect_python_init_options(&root)` (`detect.rs:399-409`) calls `detect_venv(root)` (`detect.rs:340-392`), which stats 8 candidate paths under the project root and reads `$CONDA_PREFIX` (`:361`) and `$VIRTUAL_ENV` (`:377`). Needs the project root. Does filesystem and environment I/O.
- **Java:** `detect_java_init_options(&detected_root, &workspace_root)` (`detect.rs:485-507`) calls `detect_jdk_home` (`detect.rs:412-477`) on **both** roots, reading `$JAVA_HOME`, `$HOME`/`$USERPROFILE`, `.sdkmanrc`, `.java-version`, and probing `~/.sdkman/`, `~/.asdf/`, `~/.jenv/` for `bin/java`. Needs two roots.
- **Python again, at spawn time:** `revalidate_python_init_options` (`process.rs:300-304`, `:986-1025`) re-runs venv detection because the venv may have been created or deleted since detection.

Minimum viable signature: `fn initialization_options(&self, project_root: &Path, workspace_root: &Path) -> serde_json::Value` plus a separate `fn revalidate_at_spawn(&self, opts: Value, project_root: &Path) -> Value`.

Direct consequence: **you cannot delete `PythonPlugin` or `JavaPlugin` in favour of TOML.** Their init-options logic is ~150 lines of Rust no TOML schema expresses. The achievable claim is "TOML for the data axes + an optional Rust hook per plugin", materially weaker than "a user adds a language without recompiling."

### LSP-B5 — Four divergence axes is wrong. There are at least eleven.
Exhaustive classification of every per-language conditional in the crate.

**Fits the four drafted axes:**

1. `init_options` construction — but the signature is wrong (`LSP-B4`). `detect.rs:399-409`, `:485-507`, `:1276`, `:1383`
2. `indexing_timeout_for_language` (`lifecycle.rs:26-34`) — readiness signal + budget
3. `grace_period_for_language` (`mod.rs:203-209`) — deleted by the plan
4. `init_timeout_secs` Java=180 (`detect.rs:1389`) — this is the **handshake** timeout consumed at `process.rs:334`, semantically distinct from a probe budget. A fifth field on the budget axis, not free.

**Does not fit — additional axes:**

**AXIS 5 — Manifest validation.** `validate_marker_file` (`detect.rs:241-323`): five distinct parsers. Cargo.toml must be TOML **and** contain `[package]`/`[workspace]` (`:260-269`); go.mod's first non-comment line must be `module <x>` (`:270-287`); tsconfig.json must be JSON after JSONC comment-stripping and trailing-comma-stripping (`:288-300`, helpers `:108-227`); pyproject.toml must be TOML (`:301-304`); pom.xml must contain `<project` (`:305-311`); build.gradle must be non-empty (`:312-319`). Sole producer of the `"Fix {marker}: {reason}"` hint (`detect.rs:929`, `:1028`, `:1138`, `:1266`, `:1373`). Not expressible as a TOML enum without an enum-of-validators, which is code.

**AXIS 6 — Marker sibling refinement.** `detect.rs:1088-1092`: `package.json` is only a TypeScript marker if a sibling `tsconfig.json` exists in the multi-match case. Threaded through `find_marker`'s `sibling_filter` (`detect.rs:673-696`) and `resolve_multiple_matches` (`:632-658`). A property of a **(language, marker) pair**, so it cannot live on a flat plugin field.

**AXIS 7 — Server-side plugin discovery.** `detect_typescript_plugins` (`detect.rs:802-838`) → `workspace_has_vue_files` (4-deep recursive scan with its own exclude list, `:711-766`) → `detect_ts_plugin` (npm + pnpm store layouts, `:772-797`). Flows into `LanguageLsp.auto_plugins` and mutates the initialize payload with a hardcoded tsserver block: `plugins: [...]` plus `tsserver.extraFileExtensions: [{extension:"vue", scriptKind:3}]` (`process.rs:802-832`).

**AXIS 8 — Spawn environment.** `process.rs:555-680`, four distinct env-var sets with their own directory layouts and pre-creation:

```
rust:       CARGO_TARGET_DIR = <root>/target/pathfinder-lsp              (:565-581)
go:         GOCACHE, GOMODCACHE = <root>/.pathfinder/gopls-cache/{build,mod}  (:593-621)
typescript: TMPDIR = <root>/.pathfinder/tsserver-tmp                     (:633-650)
python:     PYTHONPYCACHEPREFIX = <root>/.pathfinder/python-cache/pyc    (:661-679)
```

Plus `ensure_pathfinder_in_gitignore` called for go/ts/python/java but **not** rust (`:620`, `:649`, `:678`, `:701`).

**AXIS 9 — Spawn-time argv injection with a lifetime-bound resource.** `setup_java_env` (`process.rs:682-706`) appends `-data <dir>` to argv at spawn and returns a `std::fs::File` advisory lock (`resolve_jdtls_data_dir`, `process.rs:436-494`) that must be held for the process lifetime (`ManagedProcess._jdtls_lock`, `process.rs:153`). It also runs orphan-directory GC (`cleanup_orphaned_jdtls_data_dirs`, `:397-420`) using PID-liveness checks (`:371-395`). No trait method returning owned resources; no TOML expresses this.

**AXIS 10 — Spawn-time re-validation.** `process.rs:300-304` — Python only. A per-language hook at a *different lifecycle point* than `initialization_options`.

**AXIS 11 — Diagnostic UX strings.** `isolation_desc` (`lifecycle.rs:880-886` Linux, `:951-957` macOS) — a duplicated per-platform per-language string table.

**Verdict on the cost claim:** adding a language is a day's work only if it needs no manifest validation, no venv/JDK discovery, no cache isolation, no argv injection, and no server plugins. **That describes Go. It does not describe Python, Java, or TypeScript — the three that actually took the effort.**

### LSP-B6 — The WS-4 acceptance grep is simultaneously unsatisfiable and insufficient
`rg -n 'match .*language_id|"rust" =>' crates/pathfinder-lsp/src --glob '!*_test.rs'` must return nothing. Run today it returns 9 matches, three structurally un-removable:

```
lifecycle.rs:344   let attempt = match self.processes.remove(language_id) {
lifecycle.rs:1096  match self.processes.get(language_id) {
detect.rs:259      match (language_id, file_name) {     <- axis 5, cannot delete
```

`match self.processes.get(language_id)` is a map lookup, not a per-language branch. The criterion can never return nothing, so it will be silently relaxed or the code contorted to satisfy a grep.

Simultaneously it misses every `==` form:

```
process.rs:300  if params.language_id == "python"
process.rs:565  if isolate_target_dir && language_id == "rust"
process.rs:593  ... == "go"
process.rs:633  ... == "typescript"
process.rs:661  ... == "python"
process.rs:692  if language_id == "java"
```

and the tuple-match arms `("rust", "Cargo.toml") =>` (`detect.rs:260-319`) — the `"rust"` there is followed by `,`, not ` =>`.

### LSP-B7 — MECE file-ownership defects in the original workstream table
- `crates/pathfinder-lsp/src/client/process.rs` was owned by **nobody**, yet the capabilities payload (`:843-856`) and per-language matches live there.
- `crates/pathfinder-lsp/src/client/response_parsers.rs` owned by nobody, yet `LSP-A4`, `LSP-A7`, `LSP-A9` all live there.
- WS-4's acceptance grepped files owned by WS-2 (`lifecycle.rs:880-886`, `:951-957`; `mod.rs:203-209`).
- WS-2.1's deletion list named `probe_cache` / `lsp_started_at` on `PathfinderServer` (`server.rs:87`, `:90`) — a file owned by WS-1.
- "every per-language `match` in `capabilities.rs`" is **vacuous**: `rg language_id crates/pathfinder-lsp/src/client/capabilities.rs` → no matches.

### LSP-B8 — AC-2.2 and AC-2.3 cannot work: the crate has no mockable clock
Every clock in `pathfinder-lsp` is `std::time::Instant`:

```
mod.rs:41          LanguageState.spawned_at, UnavailableState.unavailable_since
lifecycle.rs:22    Duration, Instant
background.rs:14   Instant
process.rs:48      ManagedProcess.last_used
lawyer_impl.rs:13  Duration, Instant
```

`tokio::time::pause()` / `advance()` move `tokio::time::Instant` only. `state.transport.last_used().elapsed()` (`background.rs:268`, `:401`) reads ~0 no matter how far the mock clock advances.

Consequence: the "advance 60 minutes, assert still Ready" criterion **passes today, unmodified, with the idle timeout fully in place.** It proves nothing and would not catch a regression reintroducing idle-kill.

The crash/reprobe criterion is worse: the backoff mixes clocks. `ensure_process` gates on `state.unavailable_since.elapsed()` (std, `lifecycle.rs:359`, `:397`) while `start_process` sleeps with `tokio::time::sleep` (`lifecycle.rs:582`). Under `#[tokio::test(start_paused = true)]` the sleep returns instantly but std elapsed never advances, so `ensure_process` returns `NoLspAvailable` forever and the test hangs.

Fix required before any WS-3 acceptance is meaningful: inject a `Clock` trait, or migrate all of these to `tokio::time::Instant`.

### LSP-B9 — The WS-0 acceptance grep misses the most load-bearing false comment
`rg -i "lazil?y|on first use"` matches `server.rs:124`, `detect.rs:8`, `lawyer_impl.rs:340`, `lifecycle.rs:160`/`:229`. It does **not** match:

```
lifecycle.rs:49-50   /// Performs Zero-Config language detection immediately (cheap directory
                     /// scan). LSP processes are **not** started until first use.
```

"until first use" != "on first use". That comment sits directly on `LspClient::new` — the function that starts the idle task and whose caller immediately calls `warm_start()`.

Also missed, all becoming false after idle-kill removal:

```
mod.rs:4-5           //! Processes are started lazily on first use and terminated automatically
                     //! after an idle timeout.
lifecycle.rs:52      /// Starts the idle-timeout background task.
process.rs:7         //! - Idle `last_used` tracking for auto-termination (PRD 6.2)
process.rs:1049-1051 //   worst case is the child survives as an orphan (which the idle timeout
                     //   task will eventually clean up).
background.rs:7      //! - Idle timeout (process termination after inactivity)
```

The original "known offenders" list named 2 of ~7.

### LSP-B10 — `idle_timeout_minutes` is already dead; deleting it changes nothing
Declared at `config.rs:132`, read only in test fixtures (`detect_test.rs`, `lifecycle_test.rs`, `config_test.rs`, `tests/common/mod.rs`). Production uses the hardcoded `DEFAULT_IDLE_TIMEOUT` at `background.rs:17`. **The idle timeout has never been configurable.** Deleting the field is correct hygiene but is not evidence the behaviour changed — and an acceptance grep on the field name will pass even if `DEFAULT_IDLE_TIMEOUT`'s consumers survive under a different name.

## LSP-C — Under-specified

### LSP-C1 — "Only now does the language answer requests" has no mechanism
`ProcessEntry` (`mod.rs:92-96`) is two variants: `Running(Box<LanguageState>)` | `Unavailable(UnavailableState)`. `request()` gates on `let ProcessEntry::Running(state) = entry.value() else { NoLspAvailable }` (`lifecycle.rs:1025`); `notify()` identically (`:1078`). No third state, no readiness field on `LanguageState`.

**Places a language can be observed Ready without passing a gate** — all call `ensure_process`, which resolves on handshake:

```
document.rs:121    did_open                  -> ensure_process
lifecycle.rs:1166  call_hierarchy_request    -> ensure_process   (every BFS hop, LSP-A2)
lawyer_impl.rs:52  goto_definition           -> ensure_process
lawyer_impl.rs:112 call_hierarchy_prepare    -> ensure_process
lawyer_impl.rs:208 references                -> ensure_process
lawyer_impl.rs:277 goto_implementation       -> ensure_process
lifecycle.rs:284   force_respawn             -> start_process (bypasses ensure_process entirely)
lifecycle.rs:190   warm_start_for_languages  -> ensure_process  (PUBLIC on the Lawyer trait)
```

`Lawyer::warm_start_for_languages` is public API returning JoinHandles — anything holding an `Arc<dyn Lawyer>` can spawn without the gate.

**Additional hidden state:** `is_warm_start_complete()` is a single global `AtomicBool` (`mod.rs:364`). `warm_start_for_languages_and_track` sets it true when **its own subset** finishes (`lifecycle.rs:167-182`), or immediately if the subset is empty (`:133-135`). It is called with a subset from `repo_map.rs:321`. So: rust still initialising, agent calls `explore`, `tech_stack = ["typescript"]`, typescript already spawned → `to_start` empty → `warm_start_complete = true` at `lifecycle.rs:134` while rust-analyzer is 10s into a 90s index. `references.rs:481` and `definition.rs:140` both consult that flag as their readiness gate.

Needed: (a) name the state carrier — third `ProcessEntry` variant vs a field on `LanguageState`; (b) state that `request()`/`notify()` consult it; (c) enumerate exempt callers (the probe itself, `did_open` for the probe file, `shutdown`); (d) decide per-capability vs single boolean (`LSP-A6`).

### LSP-C2 — Budget arithmetic is unspecified; the new budget stacks on top of existing waits
Java `references` today: `wait_for_capability` polls up to 15s (`mod.rs:205` grace + `lifecycle.rs:1125-1147` 100ms loop) **before** the request, then a 30s request timeout (`lawyer_impl.rs:240`) — **45s worst case for one call**, and `symbol_overview` issues two (`overview.rs:77`, `:81`). The plan does not say whether `ensure_ready(10s)` replaces `wait_for_capability`, nor what the total per-tool wall clock is. INV-3 needs a number for the whole request, not just the gate.

### LSP-C3 — `.pathfinder/probe-cache.json` has no invalidation, concurrency, or gitignore rule
No rule for: probe file deleted/renamed, LSP binary upgraded, workspace moved. No concurrency rule — two Pathfinder instances on one workspace is real and the code already solved it once with an advisory lock for jdtls (`process.rs:436-494`). And `.pathfinder/` is only added to `.gitignore` on the cache-isolation paths (`process.rs:713-777`, called at `:620`/`:649`/`:678`/`:701`) — never for a Rust-only workspace, so the probe cache would be committed.

### LSP-C4 — "Degraded triggers respawn-and-reprobe internally" is unbounded
Today: supervisor inserts `Unavailable{backoff_attempt:1}` (`background.rs:106-112`) → `ensure_process` retries when `unavailable_since.elapsed() >= min(1<<attempt, 60)` (`lifecycle.rs:357-360`, `MAX_BACKOFF_SECS` at `background.rs:18`) → `start_process` resets attempt to 0 on the post-lock path (`lifecycle.rs:402`). Add a probe of up to `readiness_budget()` per cycle and a server that starts-then-dies gives an endless 60s-sleep / 120s-probe treadmill, with every `ensure_ready` caller blocking its own budget on top. Needs a terminal rule: N consecutive probe failures → `Degraded` is sticky, stop respawning, report guidance.

### LSP-C5 — `NotReady { eta }` has no source
The only ETA source is `indexing_progress_percent` (`mod.rs:66`, populated `background.rs:546-549`), which is `None` for any server reporting progress without a percentage and is silently lost on channel lag (`LSP-A10`). Specify what `eta` is when there is no progress data, and whether `phase` derives from `IndexingCompletionSource` (`types.rs:8-33`) or is new.

### LSP-C6 — "record the failure in `LanguageState` as `Degraded{reason}`" — there is no `LanguageState` on that path
`lifecycle.rs:639-645` inserts `ProcessEntry::Unavailable(UnavailableState{ unavailable_since, backoff_attempt })`. `UnavailableState` (`mod.rs:84-90`) has no reason field; `LanguageState` is only constructed on success (`lifecycle.rs:750-768`). Specify whether `UnavailableState` gains `reason: String` or the enum is restructured.

### LSP-C7 — "every ripgrep result labelled `confidence: heuristic`" is already half-done
`impact.rs:205` and `:283` already set `confidence: Some("heuristic")`; `:463` sets `Some("lsp")`. But `ReferenceLocation` (`types.rs:144-154`, and the server-side mirror used by `references.rs:184-189`) has **no confidence field at all** — grep-derived and LSP-derived references are structurally indistinguishable in `trace(scope=references)`. See `RSP-A7` / `RSP-C2`.

## LSP-D — Risks introduced by the plan's own changes

| id | risk |
|---|---|
| `LSP-D1` | See `LSP-B0`. Deleting `idle_timeout_task` deletes shutdown and zombie reaping. Highest single risk. |
| `LSP-D2` | See `LSP-A3`. Deleting idle-kill deletes the only document-state invalidation. Document coherence is a **hard prerequisite**, not a follow-up. |
| `LSP-D3` | Dead-code cascade fails CI under `-D warnings`. Orphaned by the idle deletion: `LspTransport::last_used`/`set_last_used` (`process.rs:71`,`:74`), `LspTransport::in_flight` (`:77`), `ManagedProcess.last_used`/`.in_flight` (`:145`,`:147`), `InFlightGuard` + Drop (`mod.rs:316-335`), `LspClient::touch` (`lifecycle.rs:974-980`). The plan named only `touch_language` and missed `touch()`, which has six call sites: `lawyer_impl.rs:88`,`:148`,`:185`,`:251`,`:313`, `document.rs:161`. |
| `LSP-D4` | Blocking `ensure_ready` behind the per-language init lock serialises cold start. `ensure_process` holds `init_locks[lang]` across spawn+handshake (`lifecycle.rs:384-389`, guard alive to `:436`). If `ensure_ready` shares that lock across the probe, three concurrent calls queue behind one probe and callers 2 and 3 hit their budget waiting for the **lock**, returning `NotReady` for a healthy server. Probe must run once, outside the lock, waiters via `tokio::sync::watch`. |
| `LSP-D5` | Deleting the grace period without deleting `wait_for_capability` breaks Java. `grace_period_for_language` (`mod.rs:203-209`) feeds `wait_for_capability` (`lifecycle.rs:1114`). With grace=0 it reduces to a hard precheck — `lifecycle.rs:1115-1123: if !check_cap(&caps) { return Err(UnsupportedCapability) }` — called before **every** navigation request (`lawyer_impl.rs:54`,`:114`,`:210`,`:279`). jdtls registers `callHierarchy` dynamically ~10-15s after `initialize`. Post-change a Java `trace` at t=3s returns `UnsupportedCapability` even though the behavioural probe would succeed. **`wait_for_capability` is not mentioned anywhere in the plan.** |
| `LSP-D6` | Half-deleting the compensation ladders makes tools strictly worse. Removing `deep_context.rs:99-140` (the retry) while keeping `deep_context.rs:85-88` (probe-passes ⇒ `degraded=false` with an EMPTY dependency list) moves `inspect` from "sometimes recovers" to "confidently returns zero dependencies." |
| `LSP-D7` | The 6-line detect loop drops five load-bearing behaviours: (1) `validate_marker_file` gate + `"Fix {marker}: {reason}"` — `detect.rs:919-931`,`1018-1029`,`1128-1139`,`1257-1273`,`1359-1380`; (2) `sibling_filter` for package.json — `:1088-1092`; (3) `resolve_multiple_matches` monorepo fallback — `:632-658`, invoked `:694`; (4) `has_source_files_recursive` post-filter — `:1430-1460`; (5) `has_override` suppression of the missing-entry — `:954`,`1053`,`1179`,`1293`,`1394`. **Target "1,526 → 150 lines" is unreachable with these preserved. Realistic: ~350-450.** |
| `LSP-D8` | TOML plugins + first-match-wins extension routing is a footgun. `plugin_for_extension` (`plugin.rs:288-293`) returns the first plugin claiming the ext; `TypeScriptPlugin` already claims `"vue"` (`plugin.rs:141`); `language_id_for_extension` (`detect.rs:1519-1521`) is the single router for **every** LSP call site. A user TOML plugin claiming `"ts"` silently reroutes all TypeScript traffic by insertion order. Needs a uniqueness assertion at registry build and a defined precedence rule. |
| `LSP-D9` | Porting `find_probe_file` imports language IDs that do not exist. `health.rs:658-666` keys on `"javascript"` and `"ruby"`; `:681-690` has candidate lists for both. Neither has a `LanguagePlugin` (`plugin.rs:265-273` registers rust, go, typescript, python, java). Same dead-ID problem in `indexing_timeout_for_language` (`lifecycle.rs:29`) and `grace_period_for_language` (`mod.rs:206`). |

## LSP-E — Confirmed fine (do not re-audit)

| id | verified OK |
|---|---|
| `LSP-E1` | `NoOpLawyer` (`no_op.rs:26-109`) returns `Err(NoLspAvailable)` from every navigation method. Zero fake zeros. Correct production fallback (`server.rs:115`). |
| `LSP-E2` | `LspError` taxonomy (`error.rs:11-75`) already distinguishes NoLspAvailable / Timeout{operation,timeout_ms} / Protocol / ServerError / ConnectionLost / UnsupportedCapability / Io. The typed vocabulary largely exists; the loss is in the navigation layer. |
| `LSP-E3` | Timeout handling in `request()` is spec-correct: `lifecycle.rs:1045-1063` removes the pending entry **and** spawns `$/cancelRequest` with the same id. |
| `LSP-E4` | Per-language request isolation is real. Pending requests tagged with `language_id` (`protocol.rs:78`); `cancel_for_language` (`:313-338`) retains other languages. A rust-analyzer crash does not cancel in-flight gopls work. |
| `LSP-E5` | No DashMap guard held across `.await`. `request()` scopes the `Ref` and clones the `Arc<dyn LspTransport>` before `send()` (`lifecycle.rs:1020-1036`); `notify()` drops explicitly at `:1088` with reasoning at `:1084-1086`. Reapers `.collect()` before mutating. No shard-lock deadlock found. |
| `LSP-E6` | Dispatch priority (`protocol.rs:150-248`) is correct and non-obvious: no-id → notification; id+method → server request; id-no-method → response. Prevents the classic JSON-RPC id collision. Batch arrays handled at `:152-157`. |
| `LSP-E7` | Process hardening is well done: `prctl(PR_SET_PDEATHSIG, SIGKILL)` in `pre_exec` with a correct SAFETY comment (`process.rs:1041-1064`), process group via `command-group` (`:524`), stderr→`/dev/null` with pipe-deadlock rationale (`:513`), absolute binary resolution via `which` (`detect.rs:515-540`). **The reader task is started BEFORE the initialize write** (`process.rs:315-327`) — the single most commonly-botched thing in an LSP client, and it is right. |
| `LSP-E8` | jdtls data-dir handling (`process.rs:397-494`) — advisory `try_lock`, PID-suffixed fallback, orphan GC on PID liveness. Careful work. |
| `LSP-E9` | Wire framing (`transport.rs:27-89`): case-insensitive `Content-Length`, unknown headers ignored per spec, 50MB body cap, `read_line` 0 → `ConnectionLost`. Correct. |
| `LSP-E10` | `"context": { "includeDeclaration": false }` on references (`lawyer_impl.rs:232`) with the comment at `:226-231` naming exactly this plan's fake-zero class. Right instinct, already applied. Keep it and the comment. |
| `LSP-E11` | Reader error classification (`process.rs:894-946`): protocol/malformed errors do **not** count toward `MAX_CONSECUTIVE_READER_ERRORS` (`:874`) — only IO errors do. A chatty non-conformant server does not get its reader killed. |
| `LSP-E12` | `apply_unregistration` (`capabilities.rs:304-349`) refuses to clear a statically-advertised capability, using the `static_flags` snapshot (`:206-213`). Easy to get wrong; correct here. |
| `LSP-E13` | `fromRanges` used as the ranges key for **both** incoming and outgoing call hierarchy (`lawyer_impl.rs:174`, `:191`). Correct per LSP 3.17. |
| `LSP-E14` | `is_alive()` (`process.rs:192-217`) conservatively reports not-alive on a contended `try_lock`, reasoning at `:194-197`. Right call for a liveness check. |

## LSP-Z1 — Consolidated: every empty/None conversion that becomes an indistinguishable zero

The single most useful artifact of the LSP audit. Each row is a place where "I could not answer" is converted to "the answer is nothing." **Work it as a checklist.**

**In `pathfinder-lsp`:**

```
protocol.rs:230              absent `result` on a response      -> Value::Null -> downstream zero   [LSP-A4]
protocol.rs:197-205          non-u64 id                         -> oneshot leaked -> timeout        [LSP-A4]
protocol.rs:179-194          id+method response                 -> misrouted -> timeout            [LSP-A4]
protocol.rs:170-172          notification, no subscriber        -> dropped silently
protocol.rs:190-192          server request, no subscriber      -> dropped; server waits forever
response_parsers.rs:112-114  null definition response           -> Ok(None)
response_parsers.rs:134-136  array whose first elem is null     -> Ok(None)
response_parsers.rs:127      array of N > 1                     -> all but first DISCARDED          [LSP-A7]
response_parsers.rs:194-196  null impl response                 -> Vec::new()
response_parsers.rs:201-203  unparseable location               -> silently skipped, uncounted
response_parsers.rs:224      null prepareCallHierarchy          -> Ok(Vec::new())    <- canonical
response_parsers.rs:289      null incoming/outgoingCalls        -> Ok(Vec::new())    <- canonical
response_parsers.rs:297-299  call missing `from`/`to`           -> continue, silent
response_parsers.rs:305-307  empty parsed item                  -> continue, silent (dead branch)
response_parsers.rs:310-322  missing fromRanges                 -> call_sites=[], no signal
response_parsers.rs:339-340  null references response           -> Ok(Vec::new())    <- canonical
response_parsers.rs:348-350  reference missing uri              -> continue, silent
response_parsers.rs:354-356  non-file:// uri (jdt://, zip)      -> continue, silent undercount      [LSP-A9]
response_parsers.rs:366-376  missing line/character             -> defaults to 1 -> wrong location  [LSP-A9]
capabilities.rs:145-148      absent capability key              -> false (typed, acceptable)
document.rs:53-56 / :58-60   Drop early-return                  -> didClose never sent, doc leaked  [LSP-A3]
document.rs:74-78            didClose send failure              -> `let _ =`, silent                [LSP-A3]
lifecycle.rs:1029 / :1081    wrong liveness flag                -> request into a dying process     [LSP-A8]
```

**In the navigation tools:**

```
deep_context.rs:85-88    empty callHierarchy + probe Ok(Some) -> degraded=false, deps=[]       [LSP-A6]
deep_context.rs:131-147  retry empty                          -> grep fallback (correctly flagged)
impact.rs:582-597        any LSP error during BFS             -> logged, swallowed, partial    [LSP-A2]
impact.rs:521-528        BFS 30s wall clock                   -> break, partial, no flag       [LSP-A2]
impact.rs:531-539        2 consecutive failures               -> break, partial, no flag       [LSP-A2]
impact.rs:648-659        per-impl prepare error               -> `else { continue }`, silent   [LSP-A2]
impact.rs:660-662        per-impl empty items                 -> `else { continue }`, silent   [LSP-A2]
impact.rs:708-719        goto_implementation returns []       -> falls through, NO degradation
                                                                 (this is C-6 returning the
                                                                  declaration, unflagged)
impact.rs:790-820        only degrades when BOTH empty        -> one-sided zero verified       [LSP-A1]
impact.rs:1148           (Some(vec![]), degraded=false)       -> verified: true                [LSP-A1]
references.rs:317-366    non-empty but partial                -> degraded=false, "ready"       [LSP-A1]
references.rs:481        gate is a GLOBAL warm-start flag     -> subset-set race               [LSP-C1]
definition.rs:69-93      retry succeeds                       -> degraded=false but
                                                                 lsp_readiness="warming_up"
                                                                 (self-contradictory metadata)
```

**Highest-value single change if only one is taken:** make `bfs_call_hierarchy` return a `TraversalOutcome` and make `compute_verified_flags` respect it (`LSP-A2`). That closes the one fake-zero channel a request-entry readiness gate provably cannot reach, and it is ~40 lines.

## LSP-Z2 — `test-mock-lsp` capability gap

None of the readiness acceptance criteria are assertable until this is built.

**Can it simulate today:**

```
slow indexing                  NO       only --init-delay-ms (config.rs:51), which delays the
                                        handshake, not post-handshake index build. Cannot make a
                                        server that handshakes fast and answers wrong.
empty-then-populated           NO       handlers are stateless and ignore params entirely
                                        (handlers.rs:52, :86, :92 all take `_params`).
                                        Every response is a compile-time constant.
dynamic-only capability reg    NO       the mock never sends a server->client request.
mid-session crash              PARTIAL  --crash-after=N (main.rs:86-92) exits(1) after N requests.
                                        Cannot crash on a specific method, after a delay, or hang.
request timeout                NO       no per-method delay flag. Cannot make a request hang.
malformed responses            NO       write_response always emits well-formed framing.
```

**Additional hard blockers:**

```
No textDocument/references handler       main.rs:141-199 has no arm; falls to `_ =>` at :193
                                         returning -32601. Conformance C-5 untestable.
No textDocument/implementation handler   same. C-6, the product thesis, untestable.
prepareCallHierarchy hardcoded null      handlers.rs:86-88 `json!(null)`. The mock can NEVER
                                         produce a non-empty call hierarchy, so C-4 and the
                                         "correct non-empty caller list" criterion can only be
                                         asserted in the negative.
incoming/outgoingCalls hardcoded []      handlers.rs:92-94
definition returns a fixed foreign URI   handlers.rs:57 `file:///mock/workspace/src/main.rs`,
                                         which never strip_prefixes against a tempdir workspace
                                         (response_parsers.rs:19-23), so `file` returns as the
                                         raw URI string.
No $/progress emission                   ReadinessSignal::WorkDoneProgress and
                                         progress_watcher_task untestable end-to-end.
No positionEncoding in capabilities      LSP-A5 cannot be regression-tested.
```

**Minimum mock capability that must exist first:**

1. **Scripted response timeline per method.** `--script=<json>`, entries of `{ method, after_ms | after_n_requests, response | "hang" | "error:<code>" | "crash" }`. One feature covers slow indexing, empty-then-populated, request timeout, method-specific crash, and `-32801`/`-32802` injection.
2. **Position-aware canned index.** `{ uri -> { (line,char) -> { definition, references[], prepareCallHierarchy[], incomingCalls[], outgoingCalls[], implementation[] } } }` so the mock returns real non-empty answers for a real fixture repo. Required for C-4, C-5, C-6, C-7.
3. **Server-initiated message emitter.** `--register-after-ms=N --register-method=...` → `client/registerCapability`. `--progress-script=[begin, report(pct)..., end]` → `window/workDoneProgress/create` + `$/progress`, also exercising the 64-slot lag path (`LSP-A10`). `--config-request` → `workspace/configuration`, making `LSP-A11` observable.
4. **Handlers for `textDocument/references` and `textDocument/implementation`.**
5. **Malformed-output injection at the framing layer.** `--malform=<none|bad-content-length|invalid-json|no-result-no-error|string-id|echo-method-in-response>`. The last three are exactly `LSP-A4`'s cases.
6. **`--position-encoding=<utf-8|utf-16>`** in the initialize response, plus a non-ASCII fixture, to regression-test `LSP-A5`.
7. **A mockable clock in `pathfinder-lsp`** (`LSP-B8`) — not a mock-LSP change, but without it two acceptance criteria are theatre.

---

# TREE-SITTER LAYER

Verdict up front: the load-bearing claim — Tree-sitter is **the addressing layer** — is correct. But the addressing layer is currently wrong in four independent ways the plan does not name, and the `tags.scm` cost model is wrong.

Grammar claims verified against vendored `node-types.json` / `queries/tags.scm` in `~/.cargo/registry/src/index.crates.io-*/`.

## TS-A — Critical gaps

### TS-A1 — `start_line` and `name_column` come from different rows; every annotated Java symbol is queried at a bogus position
All ten Tree-sitter→LSP handoffs pass the pair `(scope.start_line + 1, scope.name_column + 1)`:

```
definition.rs:64-65      goto_definition
definition.rs:456-459    goto_definition (second path)
references.rs:299-300    references
references.rs:309-310    goto_implementation      <- the product thesis (C-6)
references.rs:373-374    goto_definition probe
deep_context.rs:55-56    call_hierarchy_prepare
deep_context.rs:80-81    goto_definition
deep_context.rs:107-108  call_hierarchy_prepare retry
impact.rs:626-627        goto_implementation      <- C-6
impact.rs:842-843        call_hierarchy_prepare
impact.rs:1032-1033      call_hierarchy_prepare
```

`start_line` is `child.start_position().row` — the declaration node's **first** row (`symbols.rs:152`).
`name_column` is `resolve_name_node(child).start_position().column` — the **name** node's column, which may be on a different row (`symbols.rs:144`).

Nothing enforces same-row. `tree-sitter-java-0.23.5/src/node-types.json`:
```
method_declaration  children: ['annotation', 'marker_annotation', 'modifiers', 'throws']
class_declaration   children: ['modifiers']
modifiers           children: ['annotation', 'marker_annotation']
```
Annotations are **children**, so the node starts at the annotation:

```java
@Override                              // row 10 -> method_declaration.start_position().row = 10
public void login(String user) { }     // row 11 -> name_column = 12 (col of `login`)
```

LSP receives `Position { line: 10, character: 12 }`. Row 10 is `@Override` (9 chars). Character 12 is past end-of-line. jdtls clamps or returns null. **`goto_implementation` on an `@Override` method — the most common shape in Java — resolves to nothing or to whatever the clamp lands on.**

Under a readiness gate this gets **worse**: `ensure_ready` returns Ready, so the empty is stamped `verified: true` and declared a confirmed zero. C-6 is the assertion most likely to fail, and C-7 ("zero callers returns `[]` with `verified: true`") will actively certify the bug.

Also affects TypeScript: `tree-sitter-typescript-0.23.2` `class_declaration fields: ['body','decorator','name','type_parameters']` — `decorator` is a **field**. `@Injectable()` on the line above a **non-exported** class shifts the row. `@Injectable() export class X` is fine because the decorator attaches to `export_statement`.

Rust, Go, Python are clean here — `TS-E2`, `TS-E3`.

### TS-A2 — Tree-sitter byte columns handed to LSP as UTF-16 code units; no conversion exists anywhere
Mirror of `LSP-A5`, from the producing side.

- `crates/pathfinder-lsp/src/client/process.rs:843-856` — initialize payload has no `general.positionEncodings`. Per spec the encoding is therefore UTF-16.
- `rg -n "utf-16|utf16|positionEncoding|position_encoding"` across `pathfinder-lsp`, `pathfinder`, `pathfinder-treesitter` → **zero hits**.
- `lawyer_impl.rs:67-68`, `:127-128`, `:223-224`, `:292-293` — `"character": column.saturating_sub(1)`, passed verbatim.
- `tree-sitter-0.26.11/binding_rust/lib.rs:1023` and `:1143` — `column: position.column as usize / 2` in the UTF-16 **input** path. The `/2` exists precisely because the UTF-8 path (which Pathfinder uses — `AstParser::parse_source` takes `&[u8]`, `parser.rs:61-71`) reports raw bytes.
- `crates/pathfinder-treesitter/src/vue_zones.rs:76-81` documents it explicitly: *"`column` is **byte-based**, not character-based."*

Any non-ASCII byte to the left of the name on the declaration line shifts the column. `é` = 2 bytes / 1 UTF-16 unit → overshoot 1. Non-BMP emoji = 4 bytes / 2 units → overshoot 2. The LSP is asked about the wrong token and returns the wrong definition or none — silently, with no degradation signal.

Narrow for Rust/Go/Java (ASCII keyword prefixes). **Not** narrow for: Vue templates and JSX (`name_column: child.start_position().column` at `symbols.rs:1240` and `:1475`, on lines routinely carrying human-language text); any file with a UTF-8 BOM (every column on row 0 shifts by 3); Unicode identifiers (legal in all five languages); Kotlin/Ruby, the candidate new languages, where non-ASCII string literals in DSL-heavy code are common.

No test exists. `parser_test.rs:120 test_parse_with_unicode` asserts only `root.kind() == "source_file"` — never a column.

The LSP→LSP direction is consistent and fine (`TS-E8`). The break is exactly one boundary: `name_column`.

### TS-A3 — Rust `impl Trait for TypeDeclaredElsewhere` creates a phantom symbol at byte range `0..0`
`merge_rust_impl_blocks_recursive` step 3 (`symbols.rs:962-978`):

```rust
for (name, methods) in extracted_methods {
    if methods.is_empty() { continue; }
    syms.push(ExtractedSymbol {
        name: name.clone(),
        semantic_path: name.clone(),
        kind: SymbolKind::Impl,
        byte_range: 0..0,      // <-- phantom
        start_line: 0,
        end_line: 0,
        name_column: 0,
        ...
    });
}
```

Fires whenever an impl block's type is not declared in the same file. **Not hypothetical:** `crates/pathfinder-lsp/src/client/lawyer_impl.rs:17` is `impl Lawyer for LspClient {` and that file declares no `struct LspClient`. **That is the plan's own flagship example file.**

- `inspect("...lawyer_impl.rs::LspClient")` → `treesitter_surgeon.rs:150-156` slices `source.get(0..0)` → `Ok` with `content: ""`, `start_line: 0`, `end_line: 0`. An empty body returned as success. No error, no degradation flag.
- `locate` on the same path → `definition.rs:64-65` sends `(line 1, character 0)` → resolves byte 0, typically the first `use`. Confidently wrong.
- `trace` → `impact.rs:842-843` prepares call hierarchy at (0,0) → empty → under a readiness gate, `verified: true`.
- `find_enclosing_symbol_ref` (`symbols.rs:1076-1092`) matches this phantom for row 0 with span 0, **beating any real symbol**. `locate(file, line=1)` on such a file returns the phantom.

Exactly the "confidently and reproducibly wrong" failure mode the plan cites as its differentiator — present in Pathfinder itself, on the file used as the example.

### TS-A4 — Vue, JSX and CSS emit `::` inside the symbol chain; the paths are unparseable by the server's own grammar
Two separator conventions coexist in `symbols.rs`:

```
symbols.rs:264       build_path      -> s.push('.')        (the documented convention)
symbols.rs:1224-1228 Vue template    -> format!("template::{sym_name}") / "{parent_path}::{sym_name}"
symbols.rs:1363      JSX             -> format!("{fn_path}::return")
symbols.rs:1466      JSX             -> format!("{parent_path}::{sym_name}")
symbols.rs:1574      CSS at-rule     -> format!("{parent_path}::{sym_name}")
symbols.rs:1679      CSS selector    -> format!("{parent_path}::{sel_name}")
```

Round trip: `locate(file, line)` emits `format!("{file}::{sym}")` with the raw `semantic_path` (`semantic_path.rs:109`) → `App.vue::template::MyButton`. Parsing back: `SemanticPath::parse` (`types.rs:98`) splits on the **first** `::` → symbol part `template::MyButton`; `SymbolChain::parse` (`types.rs:151`) splits on `.` → one segment; `Symbol::parse` (`types.rs:193`) → `name: "template::MyButton"`; `resolve_symbol_chain` (`symbols.rs:890-898`) finds no such symbol (the real ones are `template` and its child `MyButton`). **`SymbolNotFound`.**

The server's own error hint confirms the paths are invalid: `error.rs:210-214` — *"only one '::' is allowed — between the file path and the symbol."* The docstring at `symbols.rs:1134-1136` tells agents to use `file.vue::template.ComponentName` — dot-separated. **Generation and documentation disagree.**

JSX is worse: `extract_jsx_children` writes into `&mut symbol.children` (`symbols.rs:190`), so JSX elements are **direct** children of the function. The resolvable chain is `App.div`. The emitted path is `App::return::div` — a segment (`return`) with no corresponding symbol at all.

Partial band-aid exists — `read_symbol_scope_enriched` auto-retries `::`→`.` (`navigation/mod.rs:640-657`) — but that yields `App.return.div`, which still has no `return` symbol. `enrich_did_you_mean` (`navigation/mod.rs:553-561`) suggests the same broken correction.

Related: `did_you_mean` returns **bare** symbol paths with no file prefix (`symbols.rs:1008` pushes `s.semantic_path`), while `enrich_did_you_mean` appends file-qualified paths from `find_symbol` (`navigation/mod.rs:597`). Once `find_symbol` is deleted, **every suggestion is bare and none is directly re-callable.**

### TS-A5 — Cache staleness; the plan makes sessions permanent and never mentions the cache
**Invalidation key** — two-stage, `get_or_parse_single_zone_impl`:
- `cache.rs:251-255` — if `entry.mtime == current_mtime && entry.lang == lang`, return cached immediately. **No content check.**
- `cache.rs:262-285` — only when mtime differs: read, SHA-256, compare `content_hash`, reparse on mismatch.

Content hashing is correct and well tested (`cache_test.rs:569`, `:632`). The hole is the fast path: **mtime unchanged + content changed = permanently stale**, with no test. `test_cache_miss_when_content_changes` (`cache_test.rs:632-659`) deliberately sleeps 10ms first (`:645`) to force an mtime change, so the same-mtime case is never exercised.

Triggers: coarse mtime granularity, `cp -p`, `rsync --times`, `touch -r`, mtime-preserving editors, bind mounts with clock skew, two writes in one tick.

**Eviction:** `LruCache` per `AstCache::new(max_entries)` (`cache.rs:145-153`). Both caches get the same capacity — `entries` **and** `vue_entries` are each `LruCache::new(cap)` (`:148-149`), so `new(100)` allows up to 200 entries. No byte-size bound; a tree is typically several times the source size.

**Capacity 100 on 10,000 files:** `server.rs:140` — `TreeSitterSurgeon::new(100)`. Today `explore`/`repo_map` walks thousands of files per call and thrashes it. After the tool cut, `locate`/`trace`/`inspect` touch a handful per query, so 100 is adequate for the surviving surface — an argument the plan could make and does not.

**No file watcher.** `rg -n "notify|watcher|inotify"` across all `Cargo.toml` → nothing.

**`invalidate()` is dead.** `cache.rs:450-453` exists, is `pub`, and has **zero** production callers. `TreeSitterSurgeon` does not expose it, so no consumer can reach it.

Severity: MEDIUM in isolation on ns-resolution filesystems. But removing the idle timeout on the rationale that "Pathfinder's lifetime is the host's lifetime" turns a short-lived cache into a session-lifetime cache and makes an unreachable `invalidate()` structural. **Combined: HIGH, and the plan is silent.**

### TS-A6 — Go: no receiver linkage, and package-level `const`/`var` are never extracted at all
**Receivers.** `tree-sitter-go-0.25.0` `method_declaration fields: ['body','name','parameters','receiver','result']`. Pathfinder never reads `receiver`. `language.rs:50` sets Go `impl_kinds: &[]`.

```go
func (s *Server) Handle() {}   // -> semantic_path "Handle"
func (c *Client) Handle() {}   // -> semantic_path "Handle#2"
```

The agent cannot address `Server.Handle`. It gets `Handle` and `Handle#2` with nothing saying which is which — structurally identical to the Rust impl-block problem the plan calls the one legitimate escape-hatch case, **and Go does not even have the workaround.**

**Constants and vars.** `language.rs:51` sets `constant_kinds: &["const_declaration","var_declaration"]`. But:
```
const_declaration  fields: []   children: ['const_spec']
const_spec         fields: ['name','type','value']
var_declaration    fields: []   children: ['var_spec','var_spec_list']
```
`resolve_name_node` (`symbols.rs:122-127`) tries `name`, then `identifier`, then `find_variable_declarator_name` (which looks for `"variable_declarator"` — a JS/TS node kind, `symbols.rs:374-384`). All three fail. Extraction falls through to `extract_symbols_recursive` (`symbols.rs:78-85`), which descends into `const_spec` — a kind in no list — and finds nothing.

**Go package constants and variables are invisible to `locate`, `trace` and `inspect`.** No test: `rg "const_declaration|var_declaration"` in `symbols_test.rs` finds only `:516`, a TypeScript assertion. Upstream `tree-sitter-go/queries/tags.scm:40-42` already handles this correctly.

### TS-A7 — `inspect` reports 0-indexed lines, `locate` reports 1-indexed, neither says so
- `SymbolScope.start_line`/`end_line` are 0-indexed (`pathfinder-common/src/types.rs:337-340`, from `child.start_position().row` at `symbols.rs:152-153`).
- `inspect` forwards them raw into `ReadSymbolScopeMetadata` (`tools/symbols.rs:280-281`). Field docs (`server/types.rs:351-354`) say only "Starting line number of the symbol in the source" — no indexing note.
- `locate(file, line)` requires `line >= 1` (`semantic_path.rs:42-47`) and echoes 1-indexed.
- `locate(semantic_path)` returns `def.line` = LSP line + 1 (`response_parsers.rs:367-370`), 1-indexed.

An agent reading `start_line: 41` from `inspect` and calling `locate(file, line=41)` lands one line early. **Across a 3-tool surface whose entire claim is addressing precision, an undocumented off-by-one between two of the three is a wrong-answer generator.**

### TS-A8 — Rust `#[test]` detection is unreachable code, and its tests pass for the wrong reason
`is_rust_test` (`symbols.rs:595-611`) iterates `node.named_children()` on a `function_item` looking for `attribute_item`/`inner_attribute_item`. `tree-sitter-rust-0.24.2/src/node-types.json`:
```
function_item          children: ['function_modifiers','visibility_modifier','where_clause']
_declaration_statement subtypes: [..., 'attribute_item', ..., 'function_item', ..., 'inner_attribute_item', ...]
```
Attributes are **siblings**, not children. **The loop can never match.** Rust test detection reduces to the name heuristic at `symbols.rs:612-616`: `name.starts_with("test_") || name.ends_with("_test")`. So `#[test] fn resolves_impl_method()` is classified `Function`, not `Test`.

**The tests are self-deceiving.** `symbols_test.rs:234-279` — every function it checks is *also* named `test_*`:
```
#[test]        fn test_with_attribute()        -> passes via name heuristic
#[tokio::test] async fn test_tokio_attribute() -> passes via name heuristic
```
Same at `:2260-2270` for `#[rstest]`/`#[test_case]`. The assertions at `:262` and `:268` claim to prove attribute detection and **prove nothing**. No negative control exists.

**Hardcoded visibility.** `extract_impl_block` never calls `detect_access_level`: `symbols.rs:830` gives every impl method `AccessLevel::Public`; `symbols.rs:846` gives every impl block `AccessLevel::Private`. `pub fn` and bare `fn` inside a Rust impl are indistinguishable, and `detect_rust_access_level` (`symbols.rs:415-444`) is dead for the majority of Rust methods.

Relevance: this is the state of the code the plan proposes to port. **Parts of the hand-written per-language logic are dead and its tests do not detect that.**

### TS-A9 — The `#N` overload counter resets at every intermediate AST node
`extract_symbols_recursive` builds a **fresh** `SymbolExtractionContext` with an empty `name_counts` on every call (`symbols.rs:305-313`). `process_child` recurses into any node that is not itself a recognised symbol kind (`symbols.rs:78-85`), writing into the **same** `out` vec with a **new** counter.

In TypeScript `export_statement` is such a node:
```ts
export function handle() {}   // fresh ctx   -> count 1 -> semantic_path "handle"
function handle() {}          // program ctx -> count 1 -> semantic_path "handle"
```
Two symbols, identical `semantic_path`. `resolve_symbol_chain` returns the first; `handle#2` would return the second, but nothing ever **emits** `handle#2`. **The second symbol is unaddressable and invisible.** Same shape in Go (`type_declaration` wraps `type_spec`) and in TS declaration merging.

Load-bearing subtlety: `make_unique_name` (`symbols.rs:717-729`) does **not** make the name unique. It returns the input verbatim and hands back the suffix separately; only `semantic_path` gets `#N`. `resolve_symbol_chain` matches on the **unsuffixed** `s.name` (`symbols.rs:891`), which is what makes the `overload_index` arithmetic work. Any refactor that "fixes" `make_unique_name` to actually produce unique names silently breaks overload resolution with no test failure.

### TS-A10 — Two impls of the same method name on one type collapse to one path
`extract_impl_block` uses a per-impl-block `method_name_counts` (`symbols.rs:799-800`); across blocks the counters are independent:

```rust
impl Display for Foo { fn fmt(&self, ..) {} }   // -> "Foo#2.fmt" -> merged -> "Foo.fmt"
impl Debug   for Foo { fn fmt(&self, ..) {} }   // -> "Foo#3.fmt" -> merged -> "Foo.fmt"
```

`merge_rust_impl_blocks_recursive` strips `#N` from the parent segment (`symbols.rs:921-930`) and appends both into `Foo.children` (`:956-958`). Both have `name == "fmt"` and `semantic_path == "Foo.fmt"`.

`resolve_symbol_chain` takes the **first** match (`symbols.rs:890-898`) — does not error, does not return all, does not report a second candidate. `AmbiguousSymbol` exists (`pathfinder-common/src/error.rs:130`) and is never raised from the resolution path. `Foo.fmt#2` **does** resolve to the second (the arithmetic is correct, `TS-E5`) but nothing emits `#2`, so the agent cannot learn it exists.

**The plan's stated differentiator is being right about which impl. This is the same failure mode, one level down.**

### TS-A11 — Vue `find_zone` truncates the template at the first nested `</template>`
`vue_zones.rs:98-141`. `find_zone` locates the first `<template`, then takes the first `</template>` after it as the end (`:131`): `let close_rel = text[content_start..].find(close_tag.as_str())?;`

Vue uses nested `<template>` constantly (`v-if`, `v-for`, `#slot`):
```vue
<template>
  <template v-if="x"><p/></template>   <-- content_end lands HERE
  <UserCard />                          <-- silently outside the parsed zone
</template>
```
Everything after the first nested close tag is excluded from `set_included_ranges`. **Silent partial extraction — no `degraded` flag** (`degraded` is only set on grammar/parse failure, `:252-272`).

Same function, `:147-148`: the docstring asserts *"Vue SFCs have at most one of each."* False. The spec allows one `<script>` **plus** one `<script setup>`, and any number of `<style>` blocks. Only the first of each is parsed.

Same first-match bug in the single-zone path: `language.rs:220` — `text[script_open_end..].find("</script>")` breaks on a JS string literal containing `"</script>"`.

Also: `parse_zone_with_grammar` calls `parser.parse(source, None)` (`vue_zones.rs:263`) with no `ParseOptions` and no progress callback — unlike `AstParser::parse_source`, which enforces `PARSE_TIMEOUT_MICROS` (`parser.rs:9`, `:48-59`). **Vue parsing has no timeout guard.**

### TS-A12 — No file-size guard; parse timeouts are hard errors AND are never cached
- `parser.rs:9` — `PARSE_TIMEOUT_MICROS = 500_000` (500 ms), enforced via progress callback (`:51-57`).
- On expiry, `parse_with_options` returns `None` → `SurgeonError::ParseError { reason: "Parser returned None (timed out or no language set)" }` (`parser.rs:73-76`). An honest error, not a wrong answer — good. But the message conflates timeout with misconfiguration.
- The error propagates out of `spawn_blocking` at `cache.rs:315` **before** `self.entries.lock().put(...)` at `:326`. Nothing is cached; `get_or_parse_inner` then removes the in-flight cell (`cache.rs:131-134`). **Every subsequent call re-burns the full 500 ms.** A 2MB generated `.ts` file costs 500 ms per call, forever, and errors each time.
- No `metadata.len()` check anywhere before `tokio::fs::read` + parse.

Benchmarks stop far short: `benches/parsing.rs:9-15`,`:46` max `generate_large_source(1000)` ~30KB; `benches/extraction.rs:101` sizes `[50, 200, 500]`, `:170` `[10, 50, 100]` classes, largest ~50KB. **Nothing above ~50KB. No guard at 1MB / 2MB / 10MB.**

### TS-A13 — Symbol extraction re-runs on every call; only the tree is cached
`cached_parse` (`treesitter_surgeon.rs:94-105`): on a tree cache **hit** it still clones tree and source and calls `extract_symbols_from_tree` in a fresh `spawn_blocking`. There is no symbol cache.

Every `locate`, `trace`, `inspect` pays a full AST walk plus the `Vec<ExtractedSymbol>` allocation tree (`symbols.rs:14` capacity 64, plus a `HashMap` per recursion level at `:312`), plus a blocking-pool round trip. `read_symbol_scope` pays it once then calls `resolve_symbol_chain` twice — target (`treesitter_surgeon.rs:130`) and parent chain (`:139`).

INFERENCE on magnitude; the structure is verifiable at `treesitter_surgeon.rs:94-105`. Extraction is benchmarked in isolation but never in the `cached_parse` composite, so the warm-cache number does not exist.

## TS-B — Plan errors

### TS-B1 — The tool deletion table deletes `inspect`
The draft listed `tools/symbols.rs + test | 1,020` for deletion. But `crates/pathfinder/src/server/tools/symbols.rs:21` is `pub(crate) async fn inspect_impl`; also `:199 inspect_impl_single`, `:221 read_symbol_scope_impl`. `server.rs` registers `#[tool(name = "inspect")]` whose body is `self.inspect_impl(params).await`.

LOC check: `symbols.rs` 311 + `symbols_test.rs` 709 = 1,020 — the figure matches exactly, so it is a **deliberate listing, not a typo about a different file**. Executing it removes one of the three surviving tools. Duplicate of `RSP-A3`.

### TS-B2 — "visibility and test detection become query captures" is not supported by any upstream `tags.scm`
Vendored reality:
- `tree-sitter-rust-0.24.2/queries/tags.scm` — 60 lines, **zero** visibility captures, **zero** test captures, no `const_item`/`static_item`
- `tree-sitter-go-0.25.0/queries/tags.scm` — no visibility captures
- `tree-sitter-python-0.25.0/queries/tags.scm` — **14 lines total**: class, function, call, module-level assignment
- `tree-sitter-java-0.23.5/queries/tags.scm` — 20 lines, no `modifiers`, no annotations
- `tree-sitter-typescript-0.23.2/queries/tags.scm` — 23 lines, no accessibility modifiers

None of the claimed captures exist upstream. "Copy a `.scm` file, adjust captures" is really "author the semantic captures yourself, in a language with no type checker, no tests until runtime, and worse tooling than Rust."

**Feasibility per item — the full enumeration:**

| `symbols.rs` item | line | query-expressible? | why |
|---|---|---|---|
| `determine_symbol_kind` | `:88` | PARTIAL | kind-by-node-kind is what `@definition.*` is for, but the tags vocabulary (function/method/class/interface/module/macro/constant/type) is coarser than `SymbolKind` (15 variants). Rust tags maps struct+enum+union+type_item all to `@definition.class` — **Struct/Enum/Class fidelity is LOST** and must be recovered in Rust by re-inspecting `node.kind()`. |
| `refine_class_kind` | `:325` | NO | Go `type_spec` → Interface vs Struct needs the `type` field's kind. A query *can* match `type: (interface_type)` (upstream Go tags `:34`,`:36` does) — but those patterns carry **no** `@definition.*` capture, so the kind is not expressed. Writable, but you write it. |
| `refine_constant_kind` | `:348` | YES | `value: [(arrow_function)(function_expression)]` — upstream JS tags `:44-61` already does it. |
| `has_arrow_function_value` | `:359` | YES | subsumed by the above. |
| `detect_access_level` dispatcher | `:390-403` | N/A | disappears if the six below are expressible. |
| `detect_rust_access_level` | `:415` | PARTIAL | `(visibility_modifier)` presence: yes. Distinguishing `pub`/`pub(crate)`/`pub(super)` needs `#match?` on modifier text — three near-duplicate patterns × every definition kind. Combinatorial blow-up. |
| `detect_go_access_level` | `:453` | YES | `(#match? @name "^[A-Z]")` / `"^_"`. Genuinely query-native. |
| `detect_ts_access_level` | `:488` | NO | Requires walking **up** the ancestor chain to find `export_statement`, stopping at `program`/`statement_block` (`symbols.rs:491-500`). Queries match downward. `(export_statement (class_declaration ...))` works but needs a mirror pattern per definition kind × exported/not, and **`export { foo }` re-export lists are structurally unreachable.** |
| `detect_python_access_level` | `:528` | YES | `#match?` on `^__.*__$` / `^__` / `^_`. |
| `detect_java_access_level` | `:566` | PARTIAL | `modifiers` keywords are **unnamed** nodes (comment at `:572` says so). `(modifiers "public")` is supported but needs one pattern per modifier per definition kind. |
| `is_rust_test` | `:595` | NO | Needs the **preceding sibling** `attribute_item` (`TS-A8`). Queries express adjacency only via `.` inside a shared parent; `#match?` on attribute text is needed for `#[tokio::test]` vs `#[test]` vs `#[rstest]`, and stacked attributes break the anchor. Marginal at best. |
| `is_python_test` | `:619` | PARTIAL | `(decorated_definition (decorator) @d (function_definition) @f)` + `#match?`. The current impl also walks parents (`:634-639`), which a query cannot. |
| `is_java_test` | `:648` | PARTIAL | same shape as Java visibility. |
| Go / TS `is_test` name arms | `:689`,`:704` | YES | `#match?`. |
| `extract_impl_block` | `:761` | NO | impl→method parent linkage. Upstream Rust tags captures `impl_item` as `@reference.implementation` (`:55-60`) and methods as `@definition.method` via `(declaration_list (function_item))` (`:19-21`) with **no connection between them**. Queries do not emit parent-child structure. |
| `merge_rust_impl_blocks` | `:908`,`:991` | NO | plan already concedes. |
| `extract_module_block` | `:202` | NO | same parent-linkage problem. `mod_item` is captured as `@definition.module` (rust tags `:33-34`) but its contents are separate, unlinked matches. |
| `extract_nested_symbols` | `:271` | NO | same. **The single biggest gap** — this is the generic nesting mechanism. |
| `extract_jsx_children` + helpers | `:1279-1480` | NO | ~200 LOC. No JSX tags.scm upstream. |
| `extract_template_symbols` + helpers | `:1180-1273`,`:1483-1522` | NO | `tree-sitter-html-0.23.2/queries/` contains ONLY `highlights.scm` and `injections.scm`. **No tags.scm exists.** |
| `extract_style_symbols` + helpers | `:1533-1693` | NO | `tree-sitter-css-0.25.0/queries/` contains ONLY `highlights.scm`. **No tags.scm exists.** |

Of 20 items: **5 cleanly query-expressible, 6 expressible with significant hand-authoring, 9 not.**

### TS-B3 — The "one `post_process` hook per language" cap is wrong; the inventory is incomplete
Hooks actually required given tags.scm output:

| language | hooks needed beyond nesting | why |
|---|---|---|
| **ALL** | parent-child nesting reconstruction | tags.scm emits a **flat** match list. `AuthService.login` requires containment analysis over match ranges. This is the addressing layer. |
| Rust | 3 | impl-block merge; struct/enum/union kind disambiguation (tags collapses to `@definition.class`); const/static (absent from tags) |
| Go | 2 | receiver→type binding (`TS-A6`); interface/struct kind from `type_spec.type` |
| Java | 2 | enum/record/annotation-type/constructor (all absent from tags); annotation-span trim so `start_line` is the name row (`TS-A1`) |
| TS/JS | 3 | `export` ancestry; constructor re-inclusion (upstream JS tags `:6` has `(#not-eq? @name "constructor")`); interface/enum/namespace/type-alias |
| Python | 1 | decorator-aware test classification |
| Vue | uncountable | 3 zones, 3 grammars, and HTML/CSS have no tags.scm at all |

**Every language exceeds the cap.** By the plan's own rule ("more than one hook means the query is wrong"), the query approach is wrong for all six.

The correct fix is not more hooks — it is recognising that **nesting is generic, not per-language**. A single containment pass over query match ranges (match A whose byte range is strictly inside match B's becomes B's child) handles Rust impls, Go receivers, TS classes, Java classes and Python classes uniformly, leaving genuinely one hook: Rust's cross-block impl merge.

### TS-B4 — "byte-identical symbol extraction before and after" is not achievable and should not be desired
The query approach changes behaviour in **both** directions, by construction.

Query finds symbols the current code misses:
- Go `const`/`var` — upstream `go/tags.scm:40-42` captures them; current code cannot (`TS-A6`)
- JS `assignment_expression` and `pair` arrow functions — `javascript/tags.scm:63-74`; no current handling
- Rust `union_item`, `macro_definition` — `rust/tags.scm:9`,`:38`; absent from `RUST_NODE_TYPES` (`language.rs:79-86`)

Query misses symbols the current code finds:
- JS/TS constructors — `javascript/tags.scm:6` has `(#not-eq? @name "constructor")`; `TS_NODE_TYPES.method_kinds` includes `method_definition` (`language.rs:64`)
- Rust `const_item`/`static_item` — in `RUST_NODE_TYPES.constant_kinds` (`language.rs:84`); absent from rust tags
- Rust `function_signature_item` — `language.rs:80`; absent from rust tags
- Java `enum_declaration`, `record_declaration`, `annotation_type_declaration`, `constructor_declaration` — all in `JAVA_NODE_TYPES` (`language.rs:89-96`); **none** in java tags
- TS `class_declaration`, `enum_declaration`, `type_alias_declaration`, `internal_module`, `function_declaration` — in `TS_NODE_TYPES` (`language.rs:55-68`); TS tags captures only `function_signature`, `method_signature`, `abstract_method_signature`, `abstract_class_declaration`, `module`, `interface_declaration`. **A plain `export class Foo { bar() {} }` in a `.ts` file matches NOTHING in `typescript/tags.scm`.**

The criterion is unsatisfiable. Worse, enforced literally it forces the query author to **reproduce the current bugs** (no Go constants, no Rust attribute test detection, phantom `0..0` impls) to keep the snapshot green.

**Replacement criterion:** differential review, not equality. Per-fixture diff of extracted symbols. Every ADDED symbol justified as a fix with a linked finding ID. Every REMOVED symbol justified as intentional or restored. **Zero unexplained deltas.** Round-trip and position assertions pass before and after.

### TS-B5 — `typescript/tags.scm` is a supplement, not a standalone file
23 lines capturing only TS-specific declaration forms. It relies on the consumer concatenating `tree-sitter-javascript/queries/tags.scm` **first**, because the TS grammar extends the JS grammar. That is how GitHub code-nav and other consumers use it.

A `LanguageGrammar` with a single `tags_query: &'static str` produces a TypeScript extractor that **cannot see classes, functions or methods**. The field must be `&'static [&'static str]` (ordered) or the files pre-concatenated, and the plan must say which.

### TS-B6 — `LanguageGrammar` cannot model any multi-grammar file, and the plan omits `injections.scm`
The multi-grammar problem is general, not a Vue quirk:

| shape | grammars per file |
|---|---|
| Vue SFC | 3 (TS + HTML + CSS) |
| Svelte / Astro | 3+ |
| HTML with inline `<script>`/`<style>` | 3 |
| Markdown code fences | N, determined at runtime by the fence tag |
| PHP / ERB / Jinja / Go templates | 2 |
| SQL/GraphQL/CSS in tagged template literals | 2 |
| Rust doc-tests in `///` blocks | 2 |

Tree-sitter's first-class mechanism is `queries/injections.scm`, and two vendored grammars already ship it: `tree-sitter-rust-0.24.2/queries/injections.scm` and `tree-sitter-javascript-0.25.0/queries/injections.scm`. **The plan cites `tags.scm` as "a first-class mechanism for exactly this problem" while omitting the sibling mechanism that solves the problem it defers.**

Correct shape:
```rust
pub struct LanguageGrammar {
    pub id: &'static str,
    pub extensions: &'static [&'static str],
    pub grammar: fn() -> tree_sitter::Language,
    pub tags_queries: &'static [&'static str],       // ordered; TS = [js_tags, ts_tags]
    pub injections_query: Option<&'static str>,      // generic multi-grammar answer
    pub post_process: Option<fn(&mut Vec<ExtractedSymbol>)>,
}
```
plus a generic injection resolver: for each injection match, look up the injected `LanguageGrammar` by id, `set_included_ranges` with the match range, parse, run that grammar's tags queries. `vue_zones.rs` already implements the hard half correctly (`TS-E6`).

### TS-B7 — The dead-code list is incomplete; two more Surgeon methods die with the cut
Verified sole callers:

| Surgeon method | sole caller | fate |
|---|---|---|
| `generate_skeleton` (`surgeon.rs:178`) | `tools/repo_map.rs:293` (`explore`) | DEAD — plan correct |
| `node_type_at_position` (`surgeon.rs:169`) | `tools/search.rs:322` (`search`) | **DEAD — plan misses.** Also kills `treesitter_surgeon.rs:275-312` and `is_comment_node`/`is_string_node` at `:332-366`. |
| `read_source_file` (`surgeon.rs:189`) | `tools/source_file.rs:160` (`read`) | **DEAD — plan misses.** Also kills `treesitter_surgeon.rs:177-193`. |
| `extract_symbols_preloaded` (`surgeon.rs:119`) | none in `crates/pathfinder/src` | **ALREADY DEAD** |
| `enclosing_symbol_detail` (`surgeon.rs:142`) | `find_symbol.rs:391` (del), `search.rs:304` (del), `navigation/mod.rs:531` (KEEPS) | KEEP |
| `extract_symbols` (`surgeon.rs:108`) | `definition.rs:575` (`compute_did_you_mean`) | KEEP |
| `read_symbol_scope` (`surgeon.rs:101`) | `symbols.rs:263`, `definition.rs:358`, `deep_context.rs:238`, `mod.rs:627`, `mod.rs:646` | KEEP |
| `enclosing_symbol` (`surgeon.rs:132`) | `semantic_path.rs:84` | KEEP |

Additionally deletable: `cache.rs:188` `get_or_parse_preloaded`, `:231` `get_or_parse_vue_preloaded` — only reachable via `extract_symbols_preloaded`. And `AstCache::invalidate` (`cache.rs:450`) — already unreachable (`TS-A5`); either wire it up or delete it.

**Must KEEP despite appearances:** `did_you_mean` (`symbols.rs:1001`) — live via `treesitter_surgeon.rs:132` on the `read_symbol_scope` error path that all three tools hit. `mock.rs` in full — `MockSurgeon` is used by six surviving test files. `vue_zones.rs` — see `TS-D5`.

### TS-B8 — The per-language inventory omits ~565 LOC in `symbols.rs`
Not listed in the plan, all inside `symbols.rs`:
- `extract_jsx_children` (`:1357`), `walk_jsx_return_sites` (`:1379`), `collect_jsx_elements` (`:1423`), `emit_jsx_symbol` (`:1442`), `resolve_jsx_tag_name` (`:1286`), `find_inner_function` (`:1334`) — ~200 LOC JSX
- `extract_template_symbols` (`:1180`), `walk_html_elements_flat` (`:1254`), `process_html_child_element` (`:1197`), `resolve_tag_name` (`:1483`) — ~140 LOC HTML
- `extract_style_symbols` (`:1533`), `walk_css_rules` (`:1549`), `extract_at_rule_name` (`:1596`), `parse_css_selector_name` (`:1617`), `extract_css_rule_set` (`:1655`) — ~160 LOC CSS
- `push_zone_symbol` (`:1102`), `extract_symbols_from_multizone` (`:1138`) — ~65 LOC

A third of the file, none with upstream query support. See `TS-D4`.

### TS-B9 — `resolve_symbol_chain`'s doc describes a mechanism that does not exist
`symbols.rs:852-872` documents an "impl-sibling fallback": *"when the next segment fails to resolve in the current symbol's children, we gather every sibling `Impl` symbol with the same base name ... and search their children as well."*

`symbols.rs:874-906` contains no such fallback. The loop matches by name and `overload_index` and returns `None` on first failure (`:900`). The behaviour was superseded by `merge_rust_impl_blocks`; the doc survived. **20 lines describing resolution semantics an implementer would rely on.** Belongs in the documentation-truth pass.

## TS-C — Under-specified

| id | gap |
|---|---|
| `TS-C1` | Conformance C-3 ("byte-identical to the file slice") asserts nothing. `read_symbol_scope` returns `source.get(symbol.byte_range)` with no trimming (`treesitter_surgeon.rs:150-162`) — it **is** a file slice, tautologically. It passes today for the phantom `0..0` symbol (`TS-A3`), which returns `""`. What C-3 must specify is the **boundary** — see the table below. |
| `TS-C2` | WS-9 has no **round-trip** assertion. C-1 and C-2 test each direction independently; neither tests composition. `TS-A4`, `TS-A7`, `TS-A9`, `TS-A10`, `TS-A11` all survive C-1 and C-2. |
| `TS-C3` | Registry ID sync does not survive contact with the enum. `SupportedLanguage` has 8 variants but `as_str()` produces 7 — `TypeScript` and `Tsx` both map to `"typescript"` (`language.rs:126`). `Vue` maps to `"vue"` but uses the TypeScript grammar (`:140`) and has **no LSP plugin**. `JavaScript` shares `TS_NODE_TYPES` (`:154`). An "identical ID sets" assertion fails on day one. Specify the relation as `grammar_id -> lsp_language_id`, N:1. |
| `TS-C4` | There is no cache workstream at all. See `TS-A5`. |
| `TS-C5` | There is no position-encoding requirement anywhere in the plan. See `TS-A2`. |
| `TS-C6` | Runtime `.so` loading omits ABI versioning. `tree_sitter::Language` has an ABI version (`language.abi_version()`, used at `tree-sitter-0.26.11/binding_rust/lib.rs:2380`); loading a `.so` built against a different ABI is a hard incompatibility, not graceful degradation. Reject any grammar whose `abi_version()` falls outside `MIN_COMPATIBLE_LANGUAGE_VERSION..=LANGUAGE_VERSION`, with a typed error naming both. |

**`inspect` boundary inconsistency (detail for `TS-C1`):**

| language | doc comments | attributes / annotations / decorators | `export` / visibility prefix |
|---|---|---|---|
| Rust | EXCLUDED (`///` is a sibling `line_comment`) | EXCLUDED (`attribute_item` is a sibling) | included (`visibility_modifier` is a child) |
| Python | INCLUDED (docstring is inside `body`) | EXCLUDED (extraction targets the inner `function_definition`) | n/a |
| Java | EXCLUDED (`/** */` is a sibling) | **INCLUDED** (`annotation`/`modifiers` are children) | INCLUDED |
| TypeScript | EXCLUDED | INCLUDED for non-exported decorated classes; excluded otherwise | **EXCLUDED** (`export_statement` is the parent) |
| Go | EXCLUDED (`(comment)* @doc` is a sibling; upstream `go/tags.scm:2-7` captures it, Pathfinder does not) | n/a | n/a |

An agent inspecting a Rust function gets no `#[derive]`, no `#[tokio::test]`, no doc comment. The equivalent Java method returns `@Override public void ...`. An exported TS class returns `class Foo {}` — **not valid to paste back**.

## TS-D — Risks introduced by the plan's own changes

| id | risk |
|---|---|
| `TS-D1` | "Byte-identical extraction" will be waived, and waiving it removes the only guard. Per `TS-B4` the criterion is unsatisfiable; the predictable outcome is the implementer hits the first mismatch, deletes the criterion, and ships an unguarded rewrite of the code path that decides whether every LSP answer is asked about the right byte. **Replace the criterion before work starts**, so the guard survives contact. |
| `TS-D2` | Query-based extraction loses parent-child nesting; every existing semantic path flattens. `tags.scm` emits an unordered flat list of `(pattern_index, capture, range)`. `AuthService.login` does not exist in query output — only `AuthService` and `login` as independent matches. If the containment pass is not implemented, **every hierarchical path in every agent's context becomes invalid at once.** Largest breakage risk in v6. |
| `TS-D3` | Query match ordering renumbers every `#N` overload index. `#N` is assigned by traversal order within a `SymbolExtractionContext` (`symbols.rs:721-728`, `:138-139`). `QueryCursor` returns matches in a pattern-and-node order that is NOT `named_children` order, and multiple patterns can match the same node. Every cached `Foo#2` becomes a different symbol. Mitigation: define `#N` as a deterministic function of `(byte_range.start, name)` sorted ascending, not of traversal order. |
| `TS-D4` | `symbols.rs <= 500 lines` is arithmetically unreachable while Vue/JSX/CSS live there. Current 1,701; ~565 are JSX+HTML+CSS+multizone with no upstream query support (`TS-B8`). Add the generic executor, the kind map, the containment pass, and per-language hooks and 500 is not attainable without first **moving** that surface out. Make relocation an explicit task (`symbols/vue.rs`, `symbols/jsx.rs`, `symbols/css.rs`) — **a line-count criterion with no relocation plan is an incentive to delete working code.** |
| `TS-D5` | Deleting `vue_zones.rs` deletes the only correct multi-grammar primitive in the codebase. It cannot be subsumed: HTML ships no `tags.scm`, CSS ships no `tags.scm`. **Verdict: KEEP and GENERALISE.** 278 lines of correct, well-documented code using `Parser::set_included_ranges` with global byte offsets so no downstream offset arithmetic is needed (`vue_zones.rs:1-6`, `:238-250`) — exactly the primitive an `injections.scm` engine needs. Rename `injection.rs`, replace the hardcoded three-zone `scan_vue_zones` (`:153-159`) with injections-driven range discovery, and Vue, Svelte, Astro, HTML, Markdown and template literals fall out of one mechanism. Fix `TS-A11`'s two bugs first. |
| `TS-D6` | The readiness contract will **upgrade the confidence of existing wrong answers.** Every wrong-position query in `TS-A1`, `TS-A2`, `TS-A3` returns `[]` from a fully-warm, fully-ready LSP. The gate will correctly certify those empties as verified zeros. The kill condition "cold-start confidently-wrong-empty: exactly 0" **measures the wrong axis** — the addressing bugs produce **warm-start** confidently-wrong-empties, which no listed criterion detects. Add a warm-path check against: a Java fixture with `@Override` methods, a Rust fixture with `impl Trait for TypeInAnotherFile`, and a fixture with non-ASCII on declaration lines. |

## TS-E — Confirmed fine (do not re-audit)

| id | verified OK |
|---|---|
| `TS-E1` | `name_column` **design** is right. Positioning the LSP cursor on the name identifier rather than the declaration start is correct; intent documented at `surgeon.rs:38-45`, `pathfinder-common/src/types.rs:341-347`, `definition.rs:457-458`; implementation at `symbols.rs:144` correctly resolves the name node. The defect is the ROW it is paired with (`TS-A1`) and the ENCODING (`TS-A2`), not the concept. |
| `TS-E2` | Rust declaration start rows are correct. `function_item children: ['function_modifiers','visibility_modifier','where_clause']` — attributes and doc comments are siblings, so `start_position().row` is the `fn`/`pub fn` row, same row as the name. Rust is clean for `TS-A1`. |
| `TS-E3` | Python decorator handling is correct. `decorated_definition` has field `definition` and no `name`, so `resolve_name_node` fails, extraction falls through to recursion (`symbols.rs:78-85`), and the inner `function_definition` is extracted with `start_line` on the `def` row. Test detection still works because `is_python_test` walks **up** to `decorated_definition` (`:634-639`). Slightly wasteful, correct. |
| `TS-E4` | Cache correctness **on the paths it covers** is good. Content-addressed invalidation via SHA-256 on mtime change (`cache.rs:262-285`) is the right design. Singleflight via `OnceCell` (`:107-138`) is correct including concurrent stress (`cache_test.rs:268`,`:311`,`:365`,`:416`). LRU eviction tested (`:55`,`:174`). Vue and non-Vue caches correctly separated (`cache.rs:96-98`, doc `:64-67`). Delete-between-stat-and-read handled and tested (`cache_test.rs:687`). Errors are `Clone` via `Arc<io::Error>` so the singleflight cell can hand out copies (`cache.rs:137`, tested `:667`). |
| `TS-E5` | Overload `#N` parse/resolve **arithmetic** is correct. `Symbol::parse("Foo#2")` → `overload_index: Some(2)` (`types.rs:198-203`); `resolve_symbol_chain` computes `target_idx = 2-1 = 1`, skips the first `Foo`, selects the second (`symbols.rs:886-898`); no-suffix → `unwrap_or(1)-1 = 0` → first match. The counter's SCOPE is the problem (`TS-A9`), not the arithmetic. |
| `TS-E6` | `vue_zones.rs` is the best code in the crate. Native `set_included_ranges` rather than slice-plus-offset-arithmetic, correctly reasoned in the module doc (`:1-6`). Global byte offsets come out right with zero downstream correction. `byte_to_point` correct with byte-column semantics documented (`:74-92`). Degraded handling for missing HTML/CSS grammars is non-fatal and flagged (`:226-273`). Two bugs (`TS-A11`) do not change the architectural verdict. |
| `TS-E7` | `inspect` does not normalise or trim. `read_symbol_scope` returns `std::str::from_utf8(source.get(byte_range))` verbatim (`treesitter_surgeon.rs:150-162`). No trim, no dedent, no re-indent. The gap is boundary definition (`TS-C1`), not normalisation. |
| `TS-E8` | The LSP→LSP column round trip is internally consistent. Responses parsed as `character + 1` (`response_parsers.rs:372-376`,`:79`,`:87`,`:243`), re-sent as `column - 1` (`lawyer_impl.rs:67-68`,`:127-128`,`:223-224`,`:292-293`). Both ends UTF-16, so re-querying an LSP-returned location (e.g. `impact.rs:653-654`) is correct. **The encoding break is confined to the one Tree-sitter→LSP boundary.** |
| `TS-E9` | `extract_vue_script`'s byte-for-byte padding is correct. `language.rs:230-236` emits one space per **byte** of the prefix, preserving newlines, so byte offsets and byte-columns align with the original SFC. Looks like a character/byte bug; is right for the invariant it maintains. |
| `TS-E10` | Parse timeout exists and fails closed. `parser.rs:9`,`:48-59` enforces 500 ms via progress callback and returns an error rather than a partial tree. Failing to an error beats a truncated AST. The problems are the uncached retry cost and the conflated message (`TS-A12`). |
| `TS-E11` | `repo_map` really is dead after the cut. `generate_skeleton` has exactly one production caller, `tools/repo_map.rs:293`, and `crate::repo_map::*` is referenced nowhere else in `crates/pathfinder/src`. Delete `repo_map.rs` (986) + `repo_map_test.rs` (1,767) + the trait method + the mock impl. |
| `TS-E12` | Every one of the plan's factual citations about this crate is accurate. Spot-checked and correct: `symbols.rs` 1,701 lines; all line numbers for `determine_symbol_kind`, `detect_access_level` and its six, `is_test_function` and its five, `extract_impl_block`, `merge_rust_impl_blocks`, `refine_class_kind`, `refine_constant_kind`, `has_arrow_function_value`, `extract_module_block`, `extract_nested_symbols`, `build_path`, `make_unique_name`, `strip_path_prefix`, `strip_generics`, `did_you_mean`, `find_enclosing_symbol`, `language.rs:106`/`:137`, `Cargo.toml:26-36`, `vue_zones.rs` and `repo_map.rs` sizes, `navigation/mod.rs:627`. **The verdict that Tree-sitter is the addressing layer and cannot be dropped is correct and well-evidenced.** |

---

# SEARCH LAYER + RESPONSE CONTRACT

Verdict up front: the claim that `pathfinder-search` needs no language work is **correct about the crate and wrong about the layer**. And the response contract — where "reliable for AI agents" is actually won — has no owner in the plan and no specification.

## RSP-A — Critical gaps

### RSP-A1 — The text channel collapses `null` into `0`. Highest blast radius.
The JSON channel implements the null-vs-empty contract. The text channel does not — it renders `None` (UNKNOWN) as the literal integer `0`, in every trace scope.

- `impact.rs:1515-1516` — `let inc_count = incoming.as_ref().map_or(0, Vec::len);` `None` → 0. Same for `out_count`.
- `impact.rs:1264` — `text_parts.push(format!("Incoming references: {}", params.inc_count));`; `:1277` same for outgoing. A response whose JSON is `{"incoming": null, "incoming_verified": false}` emits the text line **`Incoming references: 0`**.
- `impact.rs:1226-1318` — `render_callers_callees_text` never reads `metadata.hint`, `incoming_verified`, or `outgoing_verified`. **The three fields carrying the UNKNOWN signal are JSON-only.**
- `impact.rs:1184-1190` builds exactly the right warning — *"Callers AND callees are UNKNOWN — ... Do NOT treat null as zero"* — and drops it on the floor for text-reading agents.
- `overview.rs:397-400` — `let inc = imp.incoming.as_ref().map_or(0, Vec::len); ... format!("CALLERS: {inc} direct{deg}")`. Reachable whenever exactly one of incoming/outgoing is `None`, because `overview.rs:206-207` only returns `None` for the whole summary when **both** are. So `incoming: None, outgoing: Some([...])` renders `CALLERS: 0 direct (degraded)`.

The project stakes itself on "null = UNKNOWN, [] = CONFIRMED ZERO" (AGENTS.md and `server.rs:367-370`). Models reading the text channel — the default for most MCP hosts and the only one some models attend to — get `Incoming references: 0` with no verified flag and no hint. That is the exact confidently-wrong-empty the plan declares fatal, and **no workstream in the draft touched it**: the readiness gate fixes the LSP, not the renderer; the payload diet rewrites the renderer for cost, not correctness.

### RSP-A2 — A confidently-wrong-empty is reachable **today** on the non-degraded path, and the draft's remediation makes it more reachable
Path, in `find_all_references_impl`:

1. `references.rs:317` — LSP `references()` returns `Ok([])`, implementations empty
2. `references.rs:367` — enters the empty branch
3. `references.rs:368-376` — `goto_definition` probe
4. `references.rs:378` — `matches!(probe, Ok(Some(_)))` is **false** when the probe returns `Err(...)` (timeout, transport, capability). Falls through.
5. `references.rs:481` — `if !self.lawyer.is_warm_start_complete()`. A single global `AtomicBool` (`lawyer_impl.rs:370-373`) meaning only "handshake returned". On any warm-ish server it is `true`. **Guard does not fire.**
6. `references.rs:586+` normal path; `total_references == 0`
7. `references.rs:685-687` text: **"LSP confirmed: zero references or implementations for this symbol."**
8. `references.rs:690-698` hint: **"LSP confirmed zero references. This symbol may be unused..."**
9. `references.rs:705-711` metadata: `degraded: false`, `degraded_reason: None`, `lsp_readiness: Some("ready")`, `resolution_strategy: Some("lsp_references")`

An LSP-derived empty, produced while the probe was **erroring**, labelled not-degraded and ready, narrated as "LSP confirmed". **This is a kill-condition failure that exists at HEAD.**

**Compounding:** the draft's deletion list says delete `references.rs:367-380`. The actual ladder is `references.rs:367-583` — **two** ladders (`:367-473` probe-succeeded→grep, `DegradedReason::LspWarmupGrepFallback` at `:450`; `:475-583` `!is_warm_start_complete()`→grep, `LspWarmupEmptyUnverified` at `:555`). Deleting only `:367-380` leaves `:381-583` dangling and uncompilable. Deleting `:367-583` wholesale without rewriting `:685-698` leaves the wrong-empty text and hint intact and now **unguarded**.

### RSP-A3 — The deletion table deletes the `inspect` implementation
Duplicate of `TS-B1`, confirmed independently. `symbols.rs:1` — `//! inspect tool (symbol scope mode)`; `:21 inspect_impl`; `:199 inspect_impl_single`; `:221 read_symbol_scope_impl`; `server.rs:316` — `self.inspect_impl(params).await`. Build fails.

### RSP-A4 — The deletion table deletes `search_codebase_impl` and `find_symbol_impl_inner`, both called by surviving tools
`search_codebase_impl` — `search.rs:71`. Live callers among survivors:
- `references.rs:100` — `self.search_codebase_impl(search_params).await` (trace scope=references grep fallback)
- `impact.rs:166` — same (trace scope=callers grep fallback)

`find_symbol_impl_inner` — `find_symbol.rs:476`. Live caller:
- `mod.rs:593` — inside `enrich_did_you_mean`, called by `read_symbol_scope_enriched` (`mod.rs:632-634`), called by `references.rs:255`, `impact.rs:1372`, `deep_context.rs:456`

Beyond the compile error, deleting `search.rs` silently removes two capabilities the trace fallback depends on:
- `search.rs:115 enrich_matches` — Tree-sitter enrichment populating `SearchMatch.enclosing_semantic_path`, which `impact.rs:194-197` uses to build `ImpactReference.semantic_path`. Without it every heuristic caller degrades to `file::symbol_name` (`impact.rs:197` fallback) — **the tool stops telling the agent which function the candidate is in**, the most useful field in the fallback.
- `search.rs:134 apply_filter_mode` — the code-vs-comment classifier. See `RSP-B1`.

And deleting `find_symbol.rs` removes cross-file `did_you_mean` for all three surviving tools.

**Required before deletion:** extract `search_codebase_impl` (rename `grep_candidates`, strip the tool-response wrapper), `enrich_matches`, `apply_filter_mode`, and the `find_symbol_impl_inner` resolution core into `tools/navigation/grep_support.rs`.

### RSP-A5 — After the tool cut, every degraded response tells the agent to call a tool that no longer exists
`FallbackTool` has exactly two variants, both deleted:
- `common/types.rs:583-588` — `enum FallbackTool { Search, Read }`
- `common/types.rs:679`, `:694`, `:707` — `fallback_tool: Some(FallbackTool::Search)`
- `common/types.rs:714`, `:721` — `Some(FallbackTool::Read)`

It reaches **both** channels. JSON: `actionable_guidance.fallback_tool` on every degraded response. Text: `helpers.rs:138-142` — `parts.push(format!("fallback: use {fallback} for authoritative results"));` — rendered by `format_degraded_notice`, called from `impact.rs:1232`, `references.rs:465`/`:575`/`:812`/`:923`, `deep_context.rs:616`, `definition.rs:535`, `overview.rs:419`.

Plus 13 free-text strings naming deleted tools, all on surviving paths:

```
impact.rs:1169       "Use search(query='<symbol>') to find usages manually."      (hint, JSON)
impact.rs:1172       same, parameterised
impact.rs:1245       "   Workaround: Use search to find usages manually."         (TEXT)
impact.rs:1248       same, parameterised                                          (TEXT)
references.rs:551    "Use search to find usages, or retry after lsp_health reports ready."
                     -- names BOTH `search` and the deleted `health`
references.rs:780    "References unknown. Use search to manually find usages of ..."
references.rs:891    same
common/error.rs:165  "Use search(query=...) to find the correct path, or explore to see all files."
common/error.rs:178  "Workaround: use search + inspect (tree-sitter) instead of ..."
common/error.rs:258  same
common/error.rs:230  "Use search(mode=\"symbol\", query=...) ... or search(query=...)"
common/error.rs:235  "Use search if the symbol is in a different file, or read to see available
                      symbols in this file."
common/error.rs:270  "LSP error: {message}. Workaround: use search for text-based navigation"
```

**This is the worst possible failure for convergence — the one measured property.** The agent gets a degraded answer, is told what to do next, tries it, gets "unknown tool", and must re-plan. Every one is a guaranteed extra turn. The success criterion is "exact-repeat rate ≤ baseline, ideally ~0%"; this converts a large fraction of degraded responses into forced re-asks. **It is also the cheapest gap in this register to close.**

### RSP-A6 — Batch `locate` and batch `inspect` strip all provenance
`locate_entry` (`mod.rs:689-767`) deserializes the full `GetDefinitionResponse` then constructs a `LocateResultEntry` keeping only file/line/column/preview/resolution_strategy (`:705-715`). **Dropped:** `degraded`, `degraded_reason`, `actionable_guidance`, `lsp_readiness`, `warm_start_in_progress`. `LocateResultEntry` (`types.rs:1380-1407`) has no field for any of them; `BatchLocateResult` (`types.rs:1410-1420`) has no aggregate degraded flag or readiness.

The batch text channel (`mod.rs:816-869`) renders **zero** degradation signal: `:830-844` produces `semantic_path "X" -> file:L12 col:3 — preview` identically for a grep hit and an LSP hit. `format_degraded_notice` is never called in `locate_impl_batch`.

Now consider `grep_symbol_broad` (`definition.rs:857-901`): it searches the bare pattern `\b{name}\b` across `**/*` (`:859`, `:868`) and returns `result.matches[0]` — the first line in the repo, in path-sorted order, mentioning the name. Could be a comment, a string literal, an import, or an unrelated same-named symbol. Through the batch path it surfaces as `status: "ok"`, no degraded, no notice; the only tell is `resolution_strategy: "grep_broad"` buried in JSON.

Same for `inspect`: `InspectResultEntry` (`types.rs:1341-1364`) has `status`/`source`/`error` and no degradation fields; `symbols.rs:66-112` discards `ReadWithDeepContextMetadata.{degraded, degraded_reason, lsp_readiness, resolution_strategy}`, so a batch `inspect(include_dependencies=true)` whose deps came from `attempt_grep_fallback` (`deep_context.rs:227-319`) is byte-identical in shape to an LSP-derived one.

**The purest silent-wrong-answer vector in the codebase.** The agent asked "where is X" and got a confident `file:line` that may be a comment, with `status: "ok"`.

Additional: `grep_symbol_broad` should be deleted or gated. Returning the first `\b{name}\b` hit in the repo as a **definition** is not a heuristic, it is a coin flip presented as an answer.

### RSP-A7 — Three of four result item types have no confidence field; the boundary rule is unachievable as scoped
Only `ImpactReference` carries confidence: `types.rs:558-559` — `#[serde(skip_serializing_if="Option::is_none", default)] pub confidence: Option<String>` — `"lsp"` at `impact.rs:463`, `"heuristic"` at `:205` and `:283`. **This one is correct and is the model to copy.**

The other three have none:
- `ReferenceLocation` (`types.rs:728-738`) — file/line/column/snippet only. Produced by `grep_references_fallback` (`references.rs:184-189`) **and** by LSP references (`references.rs:347-355`). Both land in the **same** `FindAllReferencesMetadata.references` field (`references.rs:445` grep, `:701` LSP). Indistinguishable per item.
- `DeepContextDependency` (`types.rs:424-434`) — semantic_path/signature/file/line only. Grep deps pushed at `deep_context.rs:296-301`; LSP deps via `append_outgoing_deps` (`:327+`). Same `dependencies: Vec<...>` (`types.rs:447`). Indistinguishable.
- `SymbolOverviewImpactEntry` (`types.rs:84-95`) — has `direction`, no `confidence`. `overview.rs:213-232` copies from `ImpactReference` and **drops** `confidence`. So `trace scope=overview` destroys the one per-entry confidence signal the system has.

**Are LSP and grep results merged into the same list, and can an agent tell them apart:**

```
scope=callers    same list, BUT distinguishable per item via `confidence`
                 (impact.rs:205/283/463). In practice mutually exclusive: grep
                 REPLACES rather than appends (impact.rs:404, 415, 814, 818).
                 VERDICT: distinguishable. The only one that is.
scope=references same list, NOT distinguishable per item. Only whole-response
                 `degraded` + `resolution_strategy` (references.rs:449-457 vs 705-711).
scope=overview   same list, NOT distinguishable, AND upstream `confidence` is
                 discarded at overview.rs:213-232. Strictly worse than callers.
inspect deps     same list, NOT distinguishable per item.
locate           single mode: distinguishable via `degraded` + `resolution_strategy`
                 (definition.rs:675-683). Batch mode: NOT (RSP-A6).
```

Can they interleave in one list? At HEAD no, by construction — each fallback replaces the whole vector. But nothing in the type system enforces it, and `attempt_grep_fallback` takes `dependencies: &mut Vec<...>` and **appends** (`deep_context.rs:296`) — one refactor away from a genuinely mixed list with no per-item marker. INFERENCE: latent, not active. Fix structurally regardless.

### RSP-A8 — `Some([])` is produced from a grep miss and co-exists with `verified: false`
`impact.rs:867-868`, inside `handle_empty_call_hierarchy` when the goto_definition probe succeeded:
```rust
state.incoming = grep_in.or(Some(Vec::new()));
state.outgoing = grep_out.or(Some(Vec::new()));
```
When grep also finds nothing both become `Some([])`. `state.lsp_bfs_ran` is left at its `new()` default of `false` (`impact.rs:82`) — never set on this branch. `compute_verified_flags` (`:1137-1153`) evaluates `(Some(_), degraded=true, lsp_bfs_ran=false)` → `Some(false)`.

Emitted: `{"incoming": [], "incoming_verified": false, "degraded": true}`. Per the project's contract `[]` means CONFIRMED ZERO; per `incoming_verified: false` it means UNKNOWN. **Both in one object.** An agent reading `incoming` (the natural field) concludes zero callers — wrong: the LSP returned an empty hierarchy and grep found nothing, which jointly prove nothing.

**Compounding:** the schema doc for `incoming_verified` (`types.rs:587-588`) states *"`Some(false)`: callers are UNKNOWN. `incoming` is `null`."* False — the code produces `Some(false)` with `Some([])` (this path) and with `Some(vec)` (heuristic paths, `:814`/`:818`). And `types.rs:582-586` says `Some(true)` covers "heuristic grep results that successfully completed", which `compute_verified_flags` never produces. **Both doc comments contradict the implementation, and they become the machine-readable JSON-Schema `description` the agent reads.**

### RSP-A9 — `trace scope=overview` is strictly less safe than `scope=callers`, and uses `skip_serializing_if` on tri-state fields
`skip_serializing_if = "Option::is_none"` on a tri-state field **erases the distinction between UNKNOWN and ABSENT**. Full audit:

```
SAFE (no skip -- `null` is actually emitted):
  types.rs:574   FindCallersCalleesMetadata.incoming
  types.rs:579   FindCallersCalleesMetadata.outgoing
  types.rs:686   FindAllReferencesMetadata.references

UNSAFE (skip present -- UNKNOWN becomes an absent key):
  types.rs:26-27 SymbolOverviewResponse.references
  types.rs:71-72 ImpactSummary.incoming
  types.rs:76-77 ImpactSummary.outgoing
```

The doc comments on the unsafe three **explicitly acknowledge the semantics they destroy**: *"Absent (omitted from JSON) when LSP was unavailable — callers are unknown. Present as `[]` when LSP confirmed zero callers."* That makes "key not present" load-bearing — the weakest possible encoding, indistinguishable from an older server version, a schema migration, a proxy stripping empty keys, or a model simply not noticing. **`null` is a value the model must read; absence is a thing it must notice is not there.**

Overview additionally drops: `incoming_verified`/`outgoing_verified` (`ImpactSummary`, `types.rs:66-80`, has no such fields; `overview.rs:209-235` does not propagate them); per-entry `confidence` (`:213-232`); `hint` (`SymbolOverviewResponse`, `types.rs:15-50`, has no `hint`, so the "do NOT treat null as zero" string built at `impact.rs:1186-1190` is discarded entirely); `resolution_strategy` (absent).

And `overview.rs:422` emits, in the text channel, **`"DEGRADED: no (LSP-backed, authoritative)"`** whenever `!degraded`. Combined with `RSP-A2` that produces the maximally bad output: `REFERENCES: 0 total across 0 files` + `DEGRADED: no (LSP-backed, authoritative)`.

**Overview is the scope an agent reaches for when it wants one call to be enough — exactly the convergence case. It has the least provenance.**

### RSP-A10 — The grep fallback has no timeout and walks the entire workspace up to 40 times per call
**No timeout.** `BFS_TIMEOUT_SECS = 30` (`impact.rs:19`) bounds only the LSP BFS. There is no `tokio::time::timeout` anywhere around `scout.search` or `search_codebase_impl` — verified across `pathfinder-search/src` and all five navigation handlers.

**Walk cost.** `RipgrepScout::walk_files` (`ripgrep.rs:484-578`) builds and runs **two** complete `ignore::WalkBuilder` traversals per search — `:513` (gitignore OFF) and `:541` (gitignore ON). The first exists solely to compute `gitignored_skipped` (`:572`), consumed only by `SearchCodebaseResponse.gitignored_skipped` (`types.rs:185`) and `determine_search_hint` (`search.rs:187`) — both in `tools/search.rs`, which the plan deletes. **After the cut the first walk is 100% dead work on every grep fallback.**

**Fan-out per tool call, worst case:**
- `trace` degraded: `run_grep_fallbacks` (`impact.rs:365-389`) = 1 `search_codebase_impl` (`:166`) + `grep_outgoing_fallback` looping up to 20 candidates (`extract_call_candidates` truncates at 20, `mod.rs:166`; loop at `impact.rs:331-349`), each 1 `scout.search` (`:236-247`). **21 searches = 42 full workspace walks.**
- `inspect(include_dependencies=true)` degraded: `attempt_grep_fallback` loops candidates (`deep_context.rs:274-303`), each 1 `scout.search` (`:202-213`). **Up to 20 searches = 40 walks.**
- `locate` degraded: `fallback_definition_grep` (`definition.rs:590-631`). Strategy 1 iterates `definition_patterns` — up to 5 for Rust (`mod.rs:437-444`) — one search each (`:653-665`). Strategy 2 = 1 impl search + up to 10 per-file searches (`:708-757`). Strategies 3 and 4 = 1 each. **Up to 17 searches = 34 walks.**

**Interaction with the plan:** removing the idle timeout makes sessions unbounded, and the bounded-wait design makes grep fallback the **designated** path during cold start. So the plan increases how often the untimed 40-walk path runs, at exactly the moment the new mandatory cold-start suite measures it.

**Sub-answers, for the record:**
- Subprocess or library? **LIBRARY.** `grep-searcher`, `grep-regex`, `grep-matcher`, `ignore`, `globset` (`pathfinder-search/Cargo.toml:22-26`). No `Command::new("rg")` anywhere — the only spawns are LSP (`process.rs:508`), `ps` (`lifecycle.rs:906`), `git` (`common/git.rs:40`). No PATH check needed, no version skew, no shell, no injection surface.
- Literal queries escaped at `ripgrep.rs:459` (`regex::escape`). Globs via `globset::GlobBuilder::build()` with typed `SearchError::InvalidPattern` (`:349-353`, `:364-369`).
- ReDoS: not classic. `grep-regex` wraps the `regex` crate — finite automata, no backtracking, worst case linear. **But** `build_matcher` (`:455-474`) sets only `case_insensitive(false)`; it does not call `size_limit()` or `dfa_size_limit()`. Defaults apply (10MB program, 2MB lazy-DFA cache). A pathological pattern can burn CPU in compile and thrash the DFA cache with no wall-clock bound. Denial-of-throughput, not correctness.
- The regex LRU cache key is `format!("{pattern}\0ci=false")` (`:462`) with the suffix hardcoded — vestigial, harmless, but it will silently collide if anyone parameterises case sensitivity without updating the key.

### RSP-A11 — `inspect` error quality depends on an unrelated parameter; guaranteed re-ask
- `inspect(semantic_path=X)` → `read_symbol_scope_impl` → `self.surgeon.read_symbol_scope` **directly** (`symbols.rs:261-264`), error straight to `treesitter_error_to_error_data` (`:303`). No enrichment.
- `inspect(semantic_path=X, include_dependencies=true)` → `read_with_deep_context_impl` → `self.read_symbol_scope_enriched` (`deep_context.rs:455-457`), which applies `::`→`.` auto-correction (`mod.rs:640-657`) and cross-file did_you_mean (`mod.rs:578-613`).

**The same wrong path, in the same tool, either auto-corrects and succeeds or hard-fails** — depending on a flag about dependency loading. `trace` (`references.rs:255`, `impact.rs:1372`) and `locate`'s definition mode all use the enriched variant; plain `inspect` is the odd one out.

`inspect` is the tool an agent calls **after** `locate`/`trace` hands it a path. The Rust `::` vs `.` confusion is the most common addressing mistake — the code has a dedicated auto-corrector for it (`mod.rs:929-936`, described as eliminating "the 3-step retry cycle agents currently experience"). Plain `inspect` does not get it. **One-line fix.**

## RSP-B — Plan errors

### RSP-B1 — The search-layer language claim is wrong twice over
The draft states the code-vs-comment classifier is "the only language-aware part of search" and that it "deletes with the `search` tool".

**Wrong (a) — not the only one.** `crates/pathfinder/src/server/tools/navigation/mod.rs` is the language brain of the grep fallback layer, is on the trace/inspect/locate path, and is **not** deleted:

```
mod.rs:43-56    SOURCE_FILE_EXTENSIONS -- 12 hardcoded extensions. Used by is_source_file
                (:62-68), which filters every grep-fallback result: references.rs:125,
                impact.rs:189, impact.rs:253, deep_context.rs:284
mod.rs:78-119   is_workspace_file -- hardcoded Rust stdlib prefixes (std/, core/, alloc/,
                library/std/) plus node_modules/, vendor/
mod.rs:190-367  keywords_for_language -- per-language keyword tables for rust, go,
                typescript/javascript, python, java, vue, with a generic 7-keyword fallback
                at :365. Language #8 gets a 7-keyword filter and floods the candidate list.
mod.rs:372-383  language_to_file_glob -- match language { "rust" => "**/*.rs", ... }
mod.rs:389-394  java_resolve_pattern -- a 900-character hand-written Java regex
mod.rs:400-416  candidate_definition_pattern -- per-language definition regexes
mod.rs:433-513  definition_patterns -- per-extension definition regex tables
                (rs / ts,tsx,js,jsx / py / go / java / vue), 4-5 patterns each
definition.rs:731-744  a SECOND, inline, divergent copy of per-language method-definition
                regexes inside grep_impl_method
```

**Wrong (b) — the classifier does not delete with the tool.** Its real name is `apply_filter_mode` (`search.rs:349`); **there is no `apply_n` in the tree**. It is invoked from `search_codebase_impl` at `search.rs:134`, called by `references.rs:100` and `impact.rs:166`. Both construct `SearchParams { ..Default::default() }` (`references.rs:97`, `impact.rs:163`), and `SearchParams::default()` sets `filter_mode: default_filter_mode()` (`types.rs:1213`) = `FilterMode::CodeOnly` (`types.rs:1568-1570`). So the Tree-sitter code-vs-comment classifier is **on the live trace fallback path** and survives the cut by necessity — `impact.rs:142-143` even documents it.

**Consequence:** the language ceiling of the fallback path is 6 (`mod.rs:191-364`), not "any grammar". The "adding a language = one `.scm` + one registry entry + one fixture" claim is unachievable while `mod.rs` exists, because a new language also needs a keyword table, a file glob, a candidate pattern, a definition-pattern table, and an entry in `SOURCE_FILE_EXTENSIONS`. **Five more hand-written sites, in a file no workstream owned for this purpose.**

### RSP-B2 — Deletion table deletes surviving-tool code
See `RSP-A3` and `RSP-A4`. `cargo build --workspace --all-targets` cannot pass.

### RSP-B3 — The search workstream's file ownership cannot deliver its own acceptance criterion
Ownership was `crates/pathfinder-search/**`. The requirement is per-entry `confidence: heuristic` on every ripgrep-derived result. The types needing the field — `ReferenceLocation` (`types.rs:729`), `DeepContextDependency` (`types.rs:425`), `SymbolOverviewImpactEntry` (`types.rs:84`) — are in `crates/pathfinder/src/server/types.rs` (owned elsewhere), and the emit sites are in `tools/**` (owned elsewhere). **Three-way MECE violation on the one acceptance criterion that matters.**

### RSP-B4 — The search workstream's stated acceptance criteria are already satisfied at HEAD
"`pathfinder-search` has no dependency on `pathfinder-treesitter` and no language enum." Verified: no treesitter line in `pathfinder-search/Cargo.toml`; zero matches for `SupportedLanguage|language_id|"rust"|"python"|"typescript"` in `pathfinder-search/src`. **The workstream as written can be closed with no code change.** The only non-vacuous criterion is the confidence rule, which `RSP-B3` shows is out of scope.

### RSP-B5 — The readiness deletion span for `references.rs` is wrong
See `RSP-A2`. Actual: `:367-473` first ladder, `:475-583` second. 217 lines, two ladders, two distinct `DegradedReason` values. Deleting only `:367-380` leaves `:381-583` uncompilable.

### RSP-B6 — The prescribed `TraceScope` fix is already the code shape
`types.rs:1423-1433` is already `#[derive(Debug, Clone, Default, serde::Deserialize, schemars::JsonSchema)] #[serde(rename_all = "snake_case")]` with three fieldless variants (`lowercase` and `snake_case` are identical for single-word variants). INFERENCE: the `oneOf` is emitted by schemars because of the **per-variant doc comments** at `:1426`/`:1429`/`:1431`. The remedy is right but the snippet is misleading — **an executing agent may "apply" it as a no-op and mark the task done.** State the actual cause: strip the per-variant doc comments; keep the derive as-is.

### RSP-B7 — Accurate plan citations, for the record
`health.rs probe_language_readiness` is at `:557` not `:558` (trivial off-by-one). `health.rs:436-444` grace-period string confirmed verbatim. `impact.rs:903 " during LSP warmup"` confirmed. `health.rs` = 993 lines, `health_test.rs` = 3,957 lines — both match.

## RSP-C — Under-specified

`RSP-C1` (the `not_ready` contract) and `RSP-C2` (the heuristic/verified boundary) were written in full by the auditor and are **promoted verbatim into `03-contracts.md`** as Contracts 1 and 2. They are not repeated here — see that document, which is the frozen source of truth for both.

## RSP-D — Risks introduced by the plan's own changes

### RSP-D1 — The payload cut trades away CONVERGENCE, the property the benchmark validated
**Where the bytes go**, measured analytically against this repo's path lengths (file path 55 chars, typical LSP `detail` snippet 72 chars):

```
per ImpactReference, JSON  : 326 bytes
per ImpactReference, text  : 247 bytes
per reference, both        : 573 bytes
  refs=10 ->  5,730 chars   refs=25 -> 14,325   refs=50 -> 28,650
```

Against a measured median of 10,702 that implies ~18-19 references per typical call — consistent with `max_references` default 50 (`types.rs:1533`) split half/half (`impact.rs:1628-1630`) and `max_depth` default 3 (`types.rs:1528`).

Breakdown of the 573 bytes/ref:
- **183 bytes (32%) is pure redundancy.** 55 bytes: `file` is embedded verbatim inside `semantic_path` — every path is `format!("{}::{}", file, ...)` (`mod.rs:535`, `:537`, `impact.rs:197`) and `file` is *also* emitted as its own field (`types.rs:536`). 55 bytes: the text renderer prints it twice again — `impact.rs:1267-1270` `"  [depth={}] {} ({}:L{})"`. 73 bytes: `snippet` emitted in JSON (`types.rs:540`) **and** in text (`:1271-1273`).
- ~150 bytes is JSON key names and punctuation repeated per entry.
- ~240 bytes is the actual payload.

**2,000 is achievable — but not by the cut the plan proposes.** Three cuts get there with **zero information loss**:

1. **Stop duplicating the file path.** Drop `file` and let the agent split `semantic_path` on the first `::`, or drop the file prefix from `semantic_path`. Saves 110/573 = **19%**.
2. **Stop emitting the same reference in both channels.** `render_callers_callees_text` (`impact.rs:1263-1288`) reproduces `metadata.incoming`/`outgoing` in full, and `serialize_metadata` (`:1688`) emits them again. The stated rationale (`impact.rs:1595-1596`, "listing every reference so agents can act without parsing structured_content") is real, so make them **non-overlapping**: text owns the list; JSON owns counts, status, confidence, truncation, retry. Saves ~326/573 = **57%**.
3. **Defaults:** `max_depth` 3→1 (cap 2), `max_references` 50→20.

Combined: 20 refs × (573 − 110 − 326) = 2,740 chars for the list + ~400 header = ~3,100; depth-1 typically halves the ref count. **Median lands under 2,000 without deleting a single fact.**

**What must be kept, and why the plan's specific cut is dangerous.** The plan says "drop source previews — the agent calls `inspect` if it wants a body."

For **verified** entries this is fine and endorsed. Note that for LSP entries `snippet` is not a source preview at all — it is `CallHierarchyItem.detail` (`impact.rs:454-457`), rust-analyzer's containing-module string; `semantic_path` already carries that.

For **heuristic** entries it is a convergence disaster. A grep entry's `snippet` is `m.content` — the actual matched source line (`impact.rs:202`, `references.rs:188`), ripgrep-truncated to 1000 bytes (`ripgrep.rs:112`). **That line is the only thing letting the agent decide whether the candidate is a real call, a comment, an import, or a same-named symbol on a different type.** Strip it and the agent gets a bare `file::name` with `confidence: heuristic` and no way to triage. Two outcomes, both bad: it believes it (confidently wrong), or it does not (an `inspect` per candidate — up to 20 round trips — or a fall back to host grep). **Either way the exact-repeat rate goes up**, applied precisely to the degraded responses where the agent is least able to proceed alone.

Also must be kept: the `hint` field (currently JSON-only, `RSP-A1`; the "one fact per line" rewrite is the moment it is lost for good). And `references_truncated` + a continuation: `impact.rs:1543` computes `references_truncated` but there is **no `next_offset`** on `FindCallersCalleesMetadata` (contrast `SearchCodebaseResponse.next_offset`, `types.rs:169`), and `TraceParams.offset` is documented as "Applies to `scope=\"references\"` only; ignored for `callers`" (`server.rs:359`). **A truncated `callers` result today has no defined continuation** — a guaranteed re-ask, made more common by cutting `max_references` to 20.

### RSP-D2 — `overview` hardcodes depth 2, so the depth cap silently changes its behaviour
`overview.rs:180` sets `max_depth: 2` unconditionally. At a cap of 2, overview keeps its current cost while `callers` gets 2x cheaper — overview becomes the expensive scope and the payload median will be dominated by whichever scope the bench favours. State the intended overview depth explicitly (recommend 1) rather than letting it fall out of a hardcode.

### RSP-D3 — The bounded readiness wait multiplies with the unbounded grep fallback
10s `ensure_ready` + ripgrep fallback permitted under all three non-Ready states + up to 42 untimed full-workspace walks. The new cold-start suite is mandatory and graded. **If the search budgets do not land before re-validation, the cold-start suite will measure timeouts and the plan will read that as a readiness failure rather than a fallback-cost failure.**

### RSP-D4 — "Land it all or land none" is correct, but the deletion list is incomplete, which makes partial landing likely
The plan names three compensation sites. The actual sprawl is at least seven:

```
references.rs:367-473    first ladder
references.rs:475-583    second ladder
references.rs:685-698    the unguarded wrong-empty text/hint (RSP-A2)
impact.rs:789-820        both-empty heuristic
impact.rs:831-905        handle_empty_call_hierarchy
deep_context.rs:74-148   probe + 1s retry + grep
definition.rs:43-151     a 1s sleep-and-retry NOT MENTIONED IN THE PLAN AT ALL
                         -- definition.rs:57 tokio::time::sleep(Duration::from_secs(1))
```

An agent working from the plan's list lands four of seven and produces exactly the non-uniform state the plan forbids.

### RSP-D5 — The response contract spans five workstreams with disjoint ownership and therefore has no owner
It lives in `server/types.rs`, `server/tools/**`, `navigation/impact.rs`, `pathfinder-common/src/types.rs`, and `pathfinder-search/**`. **Every** finding in `RSP-A` requires edits in two or more. INFERENCE: under MECE-for-parallel-dispatch the contract work gets split by file and therefore split by concern, and no single workstream owns *"is the response honest"*. **Requires a dedicated workstream** owning `server/types.rs`, `server/helpers.rs`, and the response-construction halves of the navigation handlers, sequenced after readiness and before the payload diet, with the schema workstream restricted to **parameter** schemas only.

## RSP-E — Confirmed fine (do not re-audit)

| id | verified OK |
|---|---|
| `RSP-E1` | `crates/pathfinder-search` contains **zero** language-specific logic. `ripgrep.rs` (728 lines) is byte-level: `MatchCollector` (`:182-312`) operates on `&[u8]`; `decode_line` (`:107-123`) is UTF-8 only; `strip_line_endings` (`:126-135`) is CR/LF only. No comment syntax, no identifier regex — `\b` is supplied by callers (`references.rs:41`, `impact.rs:154`, `definition.rs:859`), not the engine. `searcher.rs` (40 lines), `types.rs` (121), `mock.rs` (134) all clean. Two caveats, neither fatal: (i) `BINARY_EXTENSIONS` (`ripgrep.rs:31-37`) includes language artifacts (`so`, `class`, `jar`, `pyc`, `wasm`) — build outputs, correctly excluded, and `is_binary_extension` (`:39-43`) is allowlist-by-omission so a new language's source ext is safe by default. (ii) `ALWAYS_EXCLUDED_DIRS`/`_DIR_NAMES` (`pathfinder-common/src/types.rs:17-35`, `:45-63`) **are** ecosystem-specific (`node_modules`, `target`, `vendor`, `__pycache__`, `.next`, `.turbo`, `.gradle`) and used at `ripgrep.rs:335`, `:421-427`. Per-ecosystem, not per-grammar; a place the "one entry per language" claim leaks. Note in the ADR, do not restructure. |
| `RSP-E2` | The search layer is injection-safe with no external binary dependency. Library not subprocess; no shell so shell injection is structurally impossible; literal queries escaped (`ripgrep.rs:459`); regex and glob compile failures returned as typed errors, not panics or silent empties (`:470`, `:353`, `:368`, `:376`); classic ReDoS impossible (finite automata); path escape prevented by `strip_prefix(&params.workspace_root)` skipping out-of-root entries (`:519`, `:547`); `spawn_blocking` (`:586`) keeps the sync walk off the runtime and maps `JoinError` to `SearchError::Engine` (`:697`) rather than swallowing it. Residual issues are throughput, not safety (`RSP-A10`). |
| `RSP-E3` | Removing the LSP idle timeout has **no effect** on the search layer. `RipgrepScout` is a zero-sized type (`ripgrep.rs:390-391`) — no caches, no handles, no state to go stale. `WalkBuilder` is constructed fresh per call (`:488`, `:497`), so `.gitignore` and directory contents are re-read every search; files created after startup are found. The only long-lived state is `REGEX_CACHE` (`:25-29`): a thread-local `LruCache<String, RegexMatcher>` bounded at 32, keyed by exact pattern, immutable values, on blocking-pool threads. Bounded, immutable, correctly keyed. **Safe under unbounded session lifetime.** |
| `RSP-E4` | `ImpactReference.confidence` is correct and is the model the other types should copy. `"lsp"` at `impact.rs:463`; `"heuristic"` at `:205`, `:283`. Every construction site sets it. `direction` additionally distinguishes `"incoming"` from `"incoming_heuristic"` (`:203`, `:281`, `:459-460`), a second redundant signal. Documented accurately at `types.rs:541-559`. |
| `RSP-E5` | The `Option<Vec<T>>` tri-state **is** correctly serialized for the two most important fields — `FindCallersCalleesMetadata.incoming` (`types.rs:574`) and `.outgoing` (`:579`) carry no `skip_serializing_if`, so `null` is genuinely emitted; same for `FindAllReferencesMetadata.references` (`:686`). Someone thought about this. The failures are the three overview fields (`RSP-A9`) and the text channel (`RSP-A1`) — **the JSON core is sound.** |
| `RSP-E6` | Error taxonomy and sandbox rejection are correct and actionable. Each case distinguishable with its own code and hint: nonexistent file → `FileNotFound` → -32602 (`helpers.rs:45`,`:51`), hint at `error.rs:158-162`, checked **before** the parse in all four handlers (`references.rs:241`, `impact.rs:1357`, `deep_context.rs:441`, `symbols.rs:247`) so a file deleted mid-session gives a clean FILE_NOT_FOUND, not a parse error; malformed path → `InvalidSemanticPath` with an `issue` (`helpers.rs:183-190`) and a dedicated hint builder (`error.rs:242`); bare file where a symbol is required → "use 'file.rs::symbol' format" (`helpers.rs:196-209`); sandbox denial → `AccessDenied` with the rejecting `tier` → -32001 (`helpers.rs:55`, tier in `details` at `error.rs:304`), checked before any I/O; binary file → `UnsupportedLanguage` → -32602, hint at `error.rs:154`; empty file → `SymbolNotFound` with separator advice still emitted (`error.rs:189-240`); **symbol with zero callers, verified → `Some([])` + hint at `impact.rs:1214-1219`** ("LSP confirmed zero incoming callers. This symbol may be an entry point, unused, or only called via dynamic dispatch/reflection") — correct, genuinely useful, and the C-7 case works. `pathfinder_to_error_data` embeds the structured error in `ErrorData.data` (`helpers.rs:26-37`) **and** flattens code+message+hint into the human message (`:68-93`), so both channels carry it. `serialize_metadata` (`:216-228`) logs on failure instead of silently emitting `Value::Null`. Two blemishes filed elsewhere: hints name deleted tools (`RSP-A5`), and `NoLspAvailable`/`LspTimeout` map to INTERNAL_ERROR -32603 (`:58-65`), the wrong code for a retryable readiness condition. |
| `RSP-E7` | `did_you_mean` machinery is well-built **where it is wired**. `enrich_did_you_mean` (`mod.rs:544-616`) does separator correction (`:553-561`) plus cross-file symbol resolution. The gaps are that plain `inspect` does not use it (`RSP-A11`) and that bare suggestions become unusable once `find_symbol` is deleted (`TS-A4`). |
