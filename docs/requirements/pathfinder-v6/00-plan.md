# Pathfinder v6 — The Resolution Layer

**Status:** Approved for execution
**Supersedes:** `bench/results/PATHFINDER_PLAN.md` (adopted, amended)
**Audience:** an executing agentic system. Every workstream is independently verifiable.

## Document set

| file | role | mutability |
|---|---|---|
| `00-plan.md` (this) | mandate, invariants, DAG, ownership matrix, kill conditions | plan of record |
| `01-findings.md` | audit evidence register, every claim with `file:line` | **frozen reference** |
| `02-scope-cards.md` | per-workstream scope cards, tasks, acceptance | plan of record |
| `03-contracts.md` | frozen type contracts produced by WS-4, consumed by all | **frozen for BUILD** |

**Read order:** `00` → `03` → the scope card you own → the findings it cites.

---

## 1. Mandate

### 1.1 What the measurement says

Derived from **485 benchmark runs** across 6 suites, 3 repositories, 2 languages, 3 models. Every number traceable to `bench/results/*/runs.jsonl`.

Pathfinder has never significantly beaten the baseline on accuracy, anywhere:

| suite | accuracy vs baseline | 95% CI | tokens | cost | tool calls |
|---|---|---|---|---|---|
| nav | **−0.065** | [−0.139, +0.000] | 2.9x | 2.1x | +0% |
| deep | +0.000 | [−0.042, +0.042] | 1.5x | 1.6x | −13% |
| resolve | +0.050 | [+0.000, +0.150] | 2.6x | 2.0x | −7% |
| implement | +0.000 | [0, 0] | 1.4x | 1.2x | −17% |
| breadth | +0.005 | [−0.070, +0.080] | 2.0x | 1.9x | −6% |

Every accuracy CI spans zero. `nav` trends against. Cost is 1.2x–2.9x in every suite. Every Gemini model tested refuses to load the tool schema.

**Exactly two properties survived repetition.**

**(1) Convergence.** Exact-repeat tool-call rate — the model re-issuing a query it already ran:

| suite | baseline | Pathfinder |
|---|---|---|
| nav | 2.3% | **0.0%** |
| implement | 15.1% | **7.9%** |
| breadth (large repos) | 20.4% | **2.5%** |
| nav, weak model | **39%** | **0%** |

Worst-case/median tail of **3.1x** against the baseline's **53.2x**.

**(2) Resolution precision.** On `resolve`: Pathfinder 1.000 / 8-of-8 perfect; grep+read 0.950 / 7-of-8; CodeGraph 0.919 / 5-of-8. The interesting part is the *failure mode*: asked which concrete type implements `Lawyer` at runtime, CodeGraph answered `client/mod.rs` **in both runs** — where the type is declared. The impl lives in `client/lawyer_impl.rs`. Confidently, reproducibly wrong in the same way twice, because its cross-file resolution is "best-effort name matching." Pathfinder, resolving through rust-analyzer, was right both times.

Caveat stated plainly: a single graded error separates Pathfinder from the baseline at n=8. Not significant. What *is* solid is CodeGraph being worse, and the mechanism being systematic rather than random.

### 1.2 The mandate

> The retrieval problem is solved well enough by grep and by graphs. **The resolution problem is not**, and it is the one that silently breaks refactors. That is a smaller product than a headless IDE, and it is the one the data supports.

v6 rebuilds Pathfinder as the **verification layer**: a cheap, high-precision resolution check that a discovery tool or a human hands off to.

### 1.3 Target state

| dimension | today | v6 |
|---|---|---|
| MCP tools | 7 | 3 (`locate`, `trace`, `inspect`) |
| schema tokens/request | 5,983 | ≤ 1,200 |
| Gemini model family | blocked | loads |
| "LSP not ready" surfaced as | `[]` (indistinguishable from zero) | typed `not_ready` + bounded wait |
| readiness diagnostics | 4,950 LOC + a tool | request-path invariant, no tool |
| per-entry provenance | 1 of 6 result types | all 6, required, non-optional |
| adding a language | hand-written detect fn + enum arm + matches in 3 files + 5 fallback tables | 1 plugin + 1 grammar entry + queries + fixture |
| median `trace` payload | 10,702 chars | < 2,000 |

### 1.4 What changed after audit

Three sovereign audits (LSP, Tree-sitter, search+contract) reviewed the draft plan against the source. Result:

- **3 build-breaking errors** in the draft — see `01-findings.md` §0
- **~20 factual errors** about the code
- **~25 wrong-answer vectors** the draft did not address, several of which the draft's own changes would have made worse
- **2 new workstreams**: test infrastructure (WS-2) and response contract (WS-4)

The single most important correction: **the draft measured the wrong axis.** It set "cold-start confidently-wrong-empty: exactly 0" as its strictest kill condition. But `TS-A1`, `TS-A2` and `TS-A3` produce **warm-start** confidently-wrong-empties — an `@Override` Java method is queried at a position past end-of-line, and a fully-warm rust-analyzer correctly returns `[]`, which the new readiness gate would then certify as a verified zero. Fixing readiness without fixing addressing makes Pathfinder *more* confident and *equally* wrong.

---

## 2. Invariants

Every workstream is subordinate to these. A change violating one is rejected regardless of its benchmark result.

- **INV-1 — No overloaded empty.** No tool may return an empty result set when the underlying engine was not verified capable of producing a non-empty one. Not-ready is a distinct, typed, machine-readable state. Applies to **both** channels — a text channel rendering `null` as `0` violates this (`RSP-A1`).
- **INV-1a — No unverified non-empty.** A result set is `Confidence::Verified` only if (a) readiness returned `Ready`, **and** (b) no request producing it returned Timeout / ConnectionLost / ServerError, **and** (c) no traversal terminated on a budget or failure-limit break. Otherwise it is `partial` with an `omitted_reason`. (`LSP-A1`, `LSP-A2`)
- **INV-2 — Verified means probed.** Capability *declaration* — static or dynamic — never gates readiness. Only a successful **behavioural** probe against real workspace code does. The probe must exercise the capability class the caller needs, not a proxy (`LSP-A6`).
- **INV-3 — Slow-and-correct beats fast-and-wrong.** A bounded wait yielding the right answer is preferred over an immediate degraded one. Default budget 10s, configurable.
- **INV-4 — Language support is a CI gate, not an opinion.** A language is supported iff it passes the conformance suite (WS-9).
- **INV-5 — No new MCP tools.** The tool count ratchets down only. New capability is expressed as fields on the three survivors.
- **INV-6 — Every deletion deletes its tests.** Coverage percentage must not be defended by keeping dead code alive.
- **INV-7 — Addressing precedes readiness.** A position handed to the LSP must be inside the symbol's name token, in the encoding the server negotiated. Certifying answers to wrongly-positioned queries is worse than not certifying at all. (`TS-A1`, `TS-A2`, `TS-A3`, `TS-D6`)
- **INV-8 — Guidance names only tools that exist.** A degraded response telling the agent to call a deleted tool is a guaranteed extra turn, attacking the one measured property. (`RSP-A5`)

---

## 3. Workstreams

| id | name | one-line mandate |
|---|---|---|
| WS-0 | Documentation truth | stop the code lying about its own lifecycle |
| WS-1 | Schema diet | −72% schema, unblock Gemini |
| WS-2 | Test infrastructure | make the mock LSP and the clock capable of proving anything |
| WS-3 | LSP readiness contract | readiness becomes a request-path invariant |
| WS-4 | Response contract | one owner for "is the response honest" |
| WS-5 | Tool surface reduction | 7 tools → 3, without deleting the survivors |
| WS-6 | LSP plugin generalization | collapse `detect.rs`, name all 11 divergence axes |
| WS-7 | Tree-sitter addressing + scaling | fix the addressing layer, then scale it |
| WS-8 | Search layer re-scope | own the per-language fallback tables, budget the walks |
| WS-9 | Conformance suite | language support becomes a CI artifact |
| WS-10 | Trace cost | < 2,000 chars without losing a fact |
| WS-11 | Re-validation | prove it against the 485-run baseline |

---

## 4. DAG

```
LEVEL 0  (parallel, no dependencies)
  WS-0 docs truth
  WS-1 schema diet
  WS-2 test infrastructure          <- gates every acceptance below

LEVEL 1
  WS-3 LSP readiness contract       <- needs WS-2 (mock + clock)
  WS-7a Tree-sitter addressing fixes
        [position pairing, encoding, phantom impl, separator, cache]

LEVEL 2
  WS-4 response contract            <- needs WS-3 (ToolStatus source)
  WS-6 LSP plugin generalization    <- needs WS-3 (plugin trait shape)

LEVEL 3
  WS-5 tool surface reduction       <- needs WS-4 (contract in place first)
  WS-7b Tree-sitter query refactor  <- needs WS-7a
  WS-8 search layer re-scope        <- needs WS-5 (SearchResult shrink)

LEVEL 4
  WS-9 conformance suite            <- needs WS-6, WS-7b, WS-8
  WS-10 trace cost                  <- needs WS-4

LEVEL 5
  WS-11 re-validation               <- needs everything
```

### Sequencing rules that are not negotiable

1. **WS-2 before any acceptance claim.** `LSP-B8` and `LSP-Z2`: two of the drafted readiness criteria pass **today, unmodified, with the defect fully present**. Without a mockable clock and a scriptable mock LSP, WS-3's acceptance is theatre.
2. **WS-7a before WS-3's certification takes effect.** `TS-D6`: readiness certifies answers to wrongly-positioned queries. Either land WS-7a first, or gate `Confidence::Verified` behind WS-7a's conformance assertions.
3. **WS-4 before WS-5.** `RSP-A5`: deleting tools while guidance still names them attacks convergence.
4. **WS-8 after WS-5.** `RSP-A10` cut (a) changes `SearchResult`, which `tools/search.rs` reads. Sequence, do not parallelise.
5. **WS-3 lands whole or not at all.** `RSP-D4`: the compensation sprawl is seven sites, not three. A half-migrated readiness contract is strictly worse than today's uniformly-unreliable behaviour, because the failure mode becomes non-uniform and therefore unlearnable by the agent.

---

## 5. Ownership matrix

One writer per file. Write scopes disjoint at every level. Validate before dispatch.

| workstream | exclusive write scope | shared reads |
|---|---|---|
| WS-0 | doc comments only (no logic); `docs/**` | all |
| WS-1 | `crates/pathfinder/src/server.rs` (tool attrs only), **parameter** structs in `server/types.rs` | `03-contracts.md` |
| WS-2 | `crates/test-mock-lsp/**`, `crates/pathfinder-lsp/src/clock.rs` (new), `crates/pathfinder/tests/common/**` | all |
| WS-3 | `pathfinder-lsp/src/client/{lifecycle,background,capabilities,document,protocol,mod}.rs`, `pathfinder-lsp/src/lawyer.rs`, `pathfinder-lsp/src/client/process.rs`, `pathfinder-common/src/config.rs`, `crates/pathfinder-lsp/Cargo.toml` | `03-contracts.md`, WS-2 outputs |
| WS-4 | `crates/pathfinder/src/server/types.rs` (**response** types), `server/helpers.rs`, `pathfinder-common/src/types.rs`, `pathfinder-common/src/error.rs`, response-construction halves of `navigation/{definition,references,impact,deep_context,overview}.rs` | WS-3 outputs |
| WS-5 | `crates/pathfinder/src/server/tools/**` (deletions + `grep_support.rs` extraction) | WS-4 outputs |
| WS-6 | `pathfinder-lsp/src/plugin.rs`, `pathfinder-lsp/src/client/detect.rs`, `pathfinder-lsp/src/client/response_parsers.rs` | WS-3 outputs |
| WS-7 | `crates/pathfinder-treesitter/**` | — |
| WS-8 | `crates/pathfinder-search/**`, `crates/pathfinder/src/server/tools/navigation/mod.rs` | WS-5, WS-7 outputs |
| WS-9 | `crates/pathfinder/tests/conformance/**`, `crates/pathfinder/tests/fixtures/conformance/**` | all |
| WS-10 | text/JSON rendering functions in `navigation/impact.rs`, `navigation/overview.rs` | WS-4 outputs |
| WS-11 | `bench/**` | all |

### Ownership corrections from audit (`LSP-B7`, `RSP-B3`, `RSP-D5`)

- `process.rs` was owned by nobody → **WS-3**
- `response_parsers.rs` was owned by nobody, and `LSP-A4`/`A7`/`A9` live there → **WS-6**
- `navigation/mod.rs` holds ~380 lines of per-language fallback tables (`RSP-B1`) → **WS-8**
- `server/types.rs` is split: **parameter** structs → WS-1, **response** types → WS-4. The one shared file; the split is by type category, enforced by review.
- `navigation/*.rs` is split: response construction → WS-4, deletions → WS-5, rendering → WS-10. **Sequence; do not parallelise.**

---

## 6. Kill conditions

- **K-1** After WS-1 and a partial WS-11 re-run, the deep-suite token gap remains above +40% with no accuracy gain. *Evaluate before starting WS-3.*
- **K-2** The extended `resolve` suite ties at n ≥ 20. Convergence alone does not justify an MCP server; it justifies a lint.
- **K-3** The `mod.rs` / `lawyer_impl.rs` failure turns out to be cheaply fixable in CodeGraph. The complement disappears and so does the niche.
- **K-4** WS-3 + WS-7a cannot achieve zero confidently-wrong-empties on **both** the cold-start suite and the **warm-path** suite.
- **K-5** WS-11 shows the exact-repeat rate regressing above baseline. The payload diet or the tool cut traded away the one property measurement validated. Revert the offending cut; if it cannot be isolated, stop.

K-4 and K-5 are new to v6 and are the strictest. They are also the ones most worth failing on early.

---

## 7. Success criteria (WS-11)

| metric | target |
|---|---|
| schema tokens/request | ≤ 1,200 (from 5,983) |
| deep-suite token gap vs baseline | ≤ +15% (from +50%) |
| Gemini arms | load and complete |
| `resolve` accuracy @ n ≥ 20 | **significantly** above baseline AND CodeGraph, CIs excluding zero |
| exact-repeat rate | ≤ baseline, ideally still ~0% |
| cold-start confidently-wrong-empty | **exactly 0** |
| warm-path confidently-wrong-empty | **exactly 0** (new — `TS-D6`) |
| median `trace` payload | < 2,000 chars |
| conformance tier-1 languages | ≥ 5, CI-generated table |

---

## 8. Verification

Run at the end of **every** workstream. Validation is the work.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace --all-targets --all-features
cargo deny check
```

Global greps:

```bash
# WS-0 -- no stale lifecycle prose (note "until first use" as well as "on first use")
rg -i "lazil?y|on first use|until first use|idle timeout" \
   crates/pathfinder/src crates/pathfinder-lsp/src --glob '!*_test.rs'

# WS-1 / WS-5 -- schema budget + Gemini compatibility
cargo test -p pathfinder-mcp --test schema_budget

# WS-3 -- idle path removed, supervisor RETAINED
rg -n "DEFAULT_IDLE_TIMEOUT|reap_idle_processes|collect_idle_languages|touch_language|LspClient::touch" crates/
rg -n "shutdown_all_processes|reap_dead_processes" crates/pathfinder-lsp/src   # MUST still exist

# WS-4 -- contract enforcement
rg -n 'skip_serializing_if' crates/pathfinder/src/server/types.rs   # zero on tri-state fields
rg -n '\b(search|explore|read|lsp_health|health)\(' \
   crates/pathfinder/src crates/pathfinder-common/src --glob '!*_test.rs'

# WS-6 -- per-language conditionals gone (both match AND == forms)
rg -n 'match .*language_id|"(rust|go|typescript|python|java)"\s*(=>|==)' \
   crates/pathfinder-lsp/src --glob '!*_test.rs'
wc -l crates/pathfinder-lsp/src/client/detect.rs        # target <= 450, NOT 150 (LSP-D7)

# WS-7
cargo test -p pathfinder-mcp-treesitter --test symbol_extraction_snapshots
cargo test -p pathfinder-mcp-treesitter --test round_trip
wc -l crates/pathfinder-treesitter/src/symbols.rs       # measured AFTER vue/jsx/css relocation

# WS-9
cargo test -p pathfinder-mcp --test conformance
```

---

## 9. Notes for the executing system

1. **`01-findings.md` is evidence, not instructions.** Verify `file:line` before acting; the tree moves. If a finding is wrong on inspection, record the correction there and escalate — a scope card's acceptance depends on it.
2. **`03-contracts.md` is frozen.** A BUILD agent that believes a contract needs changing STOPS and escalates.
3. **Grep-based acceptance criteria must be satisfiable.** `LSP-B6` documents one that can never return zero. If a criterion cannot pass, that is a plan bug — escalate rather than contorting code to satisfy a regex.
4. **Line-count targets are not licences to delete features.** `TS-D4`. Where a target requires relocation, the relocation is an explicit task.
5. **Report deltas, not just completions.** Every handoff states: findings closed, findings deferred with reason, new findings discovered, and any acceptance criterion relaxed with justification.

---

## Appendix A — Traceability

Verified at authoring time. Re-run before dispatch if the findings register changes.

```bash
grep -ohE "\b(LSP|TS|RSP)-[A-EZ][0-9]+\b" \
  docs/requirements/pathfinder-v6/{00-plan,02-scope-cards,03-contracts}.md \
  docs/adr/adr-00{3,4,5}-*.md | sort -u > /tmp/cited.txt
grep -ohE "\b(LSP|TS|RSP)-[A-EZ][0-9]+\b" \
  docs/requirements/pathfinder-v6/01-findings.md | sort -u > /tmp/defined.txt
comm -23 /tmp/cited.txt /tmp/defined.txt   # dangling: MUST be empty
comm -13 /tmp/cited.txt /tmp/defined.txt   # orphans: MUST be E-class or listed below
```

**Result:** 0 dangling. 35 orphans, all accounted for: 27 `*-E<n>` confirmed-fine entries (correctly not actioned — they exist so the area is not re-audited), plus 8 cross-references to findings actioned elsewhere:

| orphan | points at | actioned in |
|---|---|---|
| `LSP-D1` | `LSP-B0` | WS-3.3 |
| `LSP-D2` | `LSP-A3` | WS-3.4 |
| `RSP-B2` | `RSP-A3`, `RSP-A4` | WS-5.0, WS-5.1 |
| `RSP-B5` | `RSP-A2` | WS-3.2 |
| `RSP-B7` | — (accurate-citations note, nothing to do) | n/a |
| `TS-C4` | `TS-A5` | WS-7a.8 |
| `TS-C5` | `TS-A2` | WS-7a.2 |
| `TS-D1` | `TS-B4` | WS-7b acceptance |

### The three findings that were nearly lost

Discovered only during the traceability pass — each was in the register but no scope card had claimed it:

- **`LSP-C2`** — the readiness budget stacks on top of `wait_for_capability` (15s) and the request timeout (30s), giving 45s for one Java `references` call and 90s for an `overview`. Now specified in WS-3.2.
- **`TS-A8`** — Rust `#[test]` detection is unreachable code and its tests pass via a name heuristic, so the query refactor would have faithfully reproduced the bug. Now WS-7b.4b, with a mandatory negative control.
- **`TS-A13`** — no symbol cache; every tool call re-runs the full AST walk on a tree-cache hit. Now WS-7b.4c.

This is the argument for the traceability pass being a required step, not a formality.
