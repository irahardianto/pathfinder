# Do code-intelligence MCP servers make coding agents better?

A falsifiable, four-suite A/B benchmark of **Pathfinder** (LSP-backed) and **CodeGraph**
(pre-indexed graph) against a plain `grep` + `read` baseline.

Everything is mechanical. No score depends on reading the model's prose. Methodology and
limitations: [`bench/README.md`](../README.md). Raw records: `bench/results/*/runs.jsonl`.

| | |
|---|---|
| Runs | **485** |
| Models | Claude Sonnet 5, Claude Opus 5, hy3-free |
| Arms | `baseline` / `pathfinder` / `both` / `codegraph` / `cg_both` + 3 multi-agent variants |
| Suites | `nav` (shallow) · `deep` (architecture) · `resolve` (semantic accuracy) · `implement` (multi-file change) · `breadth` (large-repo survey, single- and multi-agent) |
| API spend | **$74.41** |
| Repos under test | this workspace (159 files) · tokio (793) · juice-shop (642), all pinned |

---

## The one-sentence result

**Neither tool is better than `grep` + `read`. Both are better *at something*, and which
one wins is decided entirely by how much discovery the question demands.**

---

## 1. The same tool is a win and a loss

Median tool calls to reach an identical answer, Claude Sonnet 5, same repo, same grader —
only the question shape changes:

| arm | DEEP architecture questions | SHALLOW lookups |
|---|---|---|
| grep + read | 11.5 | **2.0** |
| Pathfinder | 10.0 (−13%) | 2.0 (+0%) |
| **CodeGraph** | **5.5 (−52%)** | 3.5 (**+75%**) |

Mean cost per task, same runs:

| arm | SHALLOW | DEEP |
|---|---|---|
| grep + read | **$0.0305** | $0.1952 |
| Pathfinder | $0.0641 (+111%) | $0.3043 (+56%) |
| **CodeGraph** | $0.3049 (**+901%**) | **$0.1933 (−1%)** |

CodeGraph is **10x more expensive than grep on shallow lookups** and **break-even on deep
ones**. This is not a model artifact — it is the same model on both rows.

CodeGraph's own README states the mechanism: *"57–78% [cheaper] on questions the
file-reading agent needed 28–43 tool calls to answer, near-even where it got there in 7."*
Our shallow suite needs 2. We reproduced their break-even point from the other side.

---

## 2. "Surgical context" is round trips, not precision

Deep suite. Correctness identical across every arm (0.972).

| arm | tool calls | chars/call | **total chars read** | wall |
|---|---|---|---|---|
| grep + read | 13.7 | 4,773 | **65,228** | 61.5s |
| Pathfinder | 14.5 | 7,690 | 111,499 (+71%) | 49.5s |
| CodeGraph | **5.0** | 21,478 | 107,392 (**+65%**) | **38.6s** |

CodeGraph makes the agent read **65% more characters** to reach the *same* answer. Per
character it is 39% less information-dense than `grep`.

It is not returning better information. It is returning **more information in fewer round
trips** — and the round trips are what it optimises. That buys **−52% tool calls and −37%
wall time**, which is real and valuable. It does not buy precision, and it does not buy
dollars.

---

## 3. Why fewer tool calls did NOT make it cheaper

The intuition that fewer calls means better caching is backwards.

| arm | cache_read | cache_write | **write/read** | cost | **$/1k tok** |
|---|---|---|---|---|---|
| grep + read | 184,164 | 46,428 | **0.25** | $0.1952 | 0.0008 |
| CodeGraph | 90,424 | 57,944 | **0.64** | $0.1933 | **0.0013** |

CodeGraph processes **36% fewer tokens** but each token costs **63% more**. The two cancel.

Cache writes bill at ~1.25x input; cache reads at ~0.1x. Each 21,478-char `explore`
response is a **new block that must be written at premium price**, and with only 5 steps
there are just 4 later requests to amortise it over. The baseline writes small increments
and re-reads them across 13.7 steps.

**Fewer round trips means fewer opportunities to amortise, not more.**

---

## 4. The axis where Pathfinder wins

Four tasks where a plausible **wrong** answer is one `grep` away: trait impls behind a
fully-qualified path, a method that exists only after macro expansion, which concrete type
is wired in at runtime, and one method name defined on four types. Wrong answers penalised
1.5x versus omissions.

| arm | accuracy | perfect runs | median tokens | mean cost |
|---|---|---|---|---|
| grep + read | 0.950 | 7/8 | 37,234 | $0.0848 |
| **Pathfinder** | **1.000** | **8/8** | 95,219 | $0.1654 |
| PF + builtin | **1.000** | **8/8** | 96,087 | $0.1278 |
| CodeGraph | 0.919 | 5/8 | 354,252 | $0.3549 |

The mechanism is concrete and repeatable. Asked which concrete type implements `Lawyer` at
runtime, **CodeGraph answered `client/mod.rs` in both runs** — where the type is declared.
The trait impl actually lives in `client/lawyer_impl.rs`. Pathfinder, resolving through
rust-analyzer, was right both times. CodeGraph documents this: *"cross-file resolution is
best-effort name matching."*

**Honest caveat:** n=8 with a **single** graded error separating Pathfinder from the
baseline. Suggestive, not significant. CodeGraph being worse (3 imperfect runs, 9.5x the
tokens, 4.2x the cost) is the more solid half of this result.

---

## 5. Does better retrieval produce better code? No.

Two multi-file changes graded by `cargo check` and shell gates, not prose. `I1` requires
finding **all 12** production call sites of one method — miss one and the build gate fails,
so retrieval recall is enforced by the compiler.

| arm | I1 pass | I2 pass | median tokens (I2) | median calls (I2) | mean cost |
|---|---|---|---|---|---|
| grep + read | **2/2** | **2/2** | 1,792,392 | 49.0 | $0.505 |
| Pathfinder | **2/2** | **2/2** | 2,344,532 | 38.5 | $0.625 |
| CodeGraph | **2/2** | **2/2** | 1,822,455 | 38.5 | $0.483 |

**12/12 runs passed across all three arms.** Both tools cut tool calls on the threading
task (38.5 vs 49.0) but neither improved the outcome, because on a frontier model the
outcome was already saturated.

The hypothesis that better context yields better *code* is **not supported here** — though
this suite cannot refute it either, since every arm hit the ceiling.

---

## 6. The cost you cannot see

Tokens added to **every** request before any code is retrieved:

| arm | fixed schema tax |
|---|---|
| Pathfinder | **+7,666** |
| PF + builtin | +9,104 |
| CodeGraph | **+808** |
| CG + builtin | +2,246 |

Pathfinder ships **21,538 characters** of tool schema across 7 tools; 61% of it is JSON
parameter schema that duplicates guidance already in the tool description. CodeGraph ships
one tool plus a server-instructions blob: 6,145 characters total.

Mechanical, information-preserving edits would cut Pathfinder's schema by **72–80%**. But
in the deep suite the schema is only ~60% of its gap, so even a **zero-byte** schema leaves
Pathfinder at **+20% over baseline with no tool-call reduction**.

Related and unfixed by anything else: **every Gemini model rejects Pathfinder's tool schemas
outright** (`anyOf` with sibling keys is forbidden by Vertex function-calling). CodeGraph
loads fine. Pathfinder is currently unusable on Google's entire model family.

---

## 7. Reliability

`nav` suite, hy3-free, billed tokens:

| arm | p50 | max | max/p50 | blowups |
|---|---|---|---|---|
| grep + read | 16,535 | **880,089** | 53.2x | 1/27 |
| Pathfinder | 36,011 | 112,251 | **3.1x** | 1/27 |
| CodeGraph | 122,523 | 1,118,612 | 9.1x | 3/27 |

The baseline's 880k-token run consumed all 40 steps and scored **0** — a grep loop, with a
**39% exact-repeat rate** on that arm. Pathfinder's repeat rate was **0% across 102 calls**.

Pathfinder is the most predictable arm. CodeGraph on a weak model is the least.

---

## 8. The win does not survive scale — and it inverts

Suites 1–5 all ran on this 159-file workspace. Tier 5 moves to **tokio** (793 Rust files,
13,606 indexed nodes) and **juice-shop** (642 TS/JS files, 7,980 nodes), 5 tasks, n=10/arm.

| arm | accuracy | median tokens | median calls | median wall | mean cost |
|---|---|---|---|---|---|
| grep + read | 0.944 | **62,554** | **8.0** | **34s** | **$0.130** |
| Pathfinder | **0.949** | 125,266 | **7.5** | 34s | $0.252 |
| CodeGraph | 0.921 | **1,076,234** | **13.5** | **115s** | **$0.915** |

On the small repo CodeGraph cut tool calls **52%**. On these two it **raised them 69%**,
burned **17x the tokens**, cost **7x more**, took **3.4x longer**, and scored slightly worse.

The failure mode is visible in the transcripts. The worst run — `B5`, the whole-layer
authorization survey — issued **71 consecutive `codegraph_explore` calls**, consumed
3.6M tokens over 327 seconds, and cost $1.91. Median payload is 18,818 characters per call,
so by call ten the context is saturated and the agent starts re-querying instead of
converging.

Pathfinder posts its best result of the whole benchmark here: **fewest tool calls (7.5),
best accuracy (0.949), and the lowest self-correction pressure of any arm** — 0.0
exact-repeat calls per run against baseline's 0.4 and CodeGraph's 0.1 (and 4.9 per run
when counting near-duplicate inputs). It converges. It just costs 2x the baseline to do it.

### This contradicts a strong field report, and the contradiction is informative

The maintainer of this benchmark independently ran a **multi-agent** security-audit
workflow against juice-shop and measured **2.5 hours → 5 minutes** and **54 → 62 findings**
with CodeGraph. That is a 30x speedup where this suite measures a 3.4x slowdown.

Both can be true, and the difference points at the mechanism:

- **This benchmark is single-agent.** Every 18,818-character payload lands in one context
  window and stays there. Ten calls is ~190k characters of resident retrieval, and the
  model pays to re-read it on every subsequent turn.
- **A multi-agent workflow gives each subagent a fresh window.** One dense payload that
  fully answers one subagent's question is close to ideal — it is read once, summarised,
  and discarded with the subagent.

CodeGraph's own README documents exactly this trade: its responses leave *"about 80% more
retrieval context resident at the end of a session"*, and its instructions explicitly warn
that delegating exploration to file-reading sub-agents makes it *"overhead"*.

So the honest statement is not "CodeGraph is slow on big repos." It is:

> **A dense-payload index is priced per context window. Amortised across many short-lived
> subagent windows it is a large win; accumulated in one long single-agent window it is a
> large loss. This benchmark only measured the second case.**

That is the single most important untested variable left, and it is Tier 3.

---

## 9. Delegation is priced by task width — and it reconciles the field reports

Tier 3 re-runs the same 5 large-repo tasks with the orchestrator allowed to dispatch
sub-agents through the `task` tool. Sub-agent tokens are **not** in the orchestrator's event
stream, so they are recovered by walking `session.parent_id` in opencode's database; a
single-subagent probe showed the stream undercounting by **51%**, which would have made
delegation look free.

| arm | single-agent tokens | multi-agent tokens | delta | cost delta | wall delta |
|---|---|---|---|---|---|
| grep + read | 62,554 | 91,113 | **+46%** | +71% | +75% |
| Pathfinder | 125,266 | 201,044 | **+60%** | +45% | +78% |
| **CodeGraph** | 1,076,234 | **829,470** | **−23%** | +40% | +45% |

CodeGraph is the **only** arm whose token cost falls when work is delegated. That is the
dense-payload-per-window mechanism, confirmed.

But the average hides the actual rule. Broken out by how wide the question is:

| task | baseline | Pathfinder | CodeGraph |
|---|---|---|---|
| **survey 61 route files** | **−28%** | **−61%** | **−68%** |
| trace 1 auth chain | +16% | +58% | −65% |
| audit 1 pattern | +126% | −2% | +38% |
| enumerate 1 API | +85% | +71% | +100% |
| trace 1 call chain | +167% | +178% | +141% |

**Delegation pays only on the wide task, and there it pays most for the graph.** On narrow
questions it is pure orchestration overhead for everyone.

This resolves §8's contradiction. A field-measured multi-agent security audit of juice-shop
went 2.5h → 5min with CodeGraph; this suite measured CodeGraph 3.4x slower single-agent.
Both are correct:

> **A dense-payload index is priced per context window. Its cost is a function of how far
> the work splits. A whole-repo audit splits maximally and the index wins big. A single
> lookup does not split at all and the index is pure overhead.**

The remaining gap to the field report is scale of the *task*, not of the repo: the widest
task here surveys 61 files, while a full audit sweeps the entire application and cannot fit
in one context at all. That is the regime where single-agent does not merely lose, it fails.

---

## Verdict

### Pathfinder: cut to one narrow tool, or stop.

Across five suites it never significantly beat the baseline on accuracy — every CI spans
zero — and cost 1.2x–2.9x more everywhere. Its ceiling is *parity*: a perfect zero-byte
schema still leaves +20% in the deep regime. It is blocked on every Gemini model.

Two properties did survive repetition: it makes the agent **stop re-asking** (0% repeat rate
against a baseline's 39% on a weak model; 2.5% against 20.4% on large repos), and it is
**right about symbol resolution where name matching is confidently wrong** — CodeGraph
answered `client/mod.rs` instead of `client/lawyer_impl.rs` in both runs of the same task.

Those are worth roughly one narrow tool, not a seven-tool headless IDE.
Full plan with entry and kill conditions: [`PATHFINDER_PLAN.md`](PATHFINDER_PLAN.md).

### CodeGraph: adopt, but only inside a multi-agent workflow.

Three regimes, three different answers:

| regime | result |
|---|---|
| deep architecture question, small repo | **−52% tool calls, −37% wall** at equal accuracy |
| shallow lookup, small repo | **+75% tool calls, 10x cost** |
| any question, 600–800 file repo, single agent | **+69% tool calls, 17x tokens, 7x cost, 3.4x wall** |

Single-agent on a real codebase, it loses badly and the mechanism is context saturation,
not retrieval quality. But a field-measured multi-agent audit on one of those same repos
went 2.5h → 5min with *more* findings. Dense payloads are priced per window: fatal in one
long window, near-ideal in many short subagent windows.

**Adopt it behind a router and a subagent boundary.** Do not hand it to a single
long-running agent on a large repo. Keep `grep` and `read` enabled alongside — the
free-choice `cg_both` arm was the best-balanced configuration measured, scoring 1.000 on
shallow nav at $0.0496 against CodeGraph-only's $0.3049.

### The interesting path forward

1. **Router, not replacement.** The winning configuration was never a tool alone — it was a
   tool plus built-ins with the model free to choose. Cheap classification of query shape
   before retrieval is worth more than a better index.
2. **Cache-aware retrieval is an open problem.** One 21k-char block re-written per turn is
   the wrong shape for a cached-prefix API. Stable, append-only payloads would convert
   CodeGraph's round-trip win into an actual cost win.
3. **Resolution accuracy is undersold and unmeasured.** The `mod.rs` vs `lawyer_impl.rs`
   failure is invisible on every published benchmark and directly breaks refactoring. A
   hybrid — graph for breadth, LSP for the final resolution hop — is the unbuilt product.
4. **Everyone is benchmarking the wrong regime.** Published numbers use one open-ended
   question per repo, the regime where indexes always win. Real sessions are mostly shallow
   lookups, the regime where they lose 10x.

---

## Limitations

1. **Three repositories, two languages.** Suites 1–4 run on a 159-file Rust workspace;
   Tier 5 adds tokio (793 Rust files) and juice-shop (642 TS/JS files). No repo here
   approaches the 11k-file scale of CodeGraph's largest published benchmark.
2. **Correctness ceilings.** `nav`, `deep` and `implement` all saturated on frontier models.
   These suites cannot detect a quality advantage that exists.
3. **Small n on the decisive suites.** `deep` and `implement` are n=6 and n=4 per arm.
   Effect sizes are quoted with CIs; per-task numbers are indicative only.
4. **Task authorship bias.** Tasks were written by inspecting this codebase, and `deep` was
   written *after* reading CodeGraph's published methodology — deliberately, to replicate
   their regime, which biases in CodeGraph's favour on that suite.
5. **Single-agent, single-shot sessions — the biggest open gap.** Every run is one agent in
   one context window. A field-measured multi-agent run on juice-shop produced the opposite
   result (30x faster with CodeGraph) to Tier 5's single-agent measurement (3.4x slower).
   Until a multi-agent arm is measured, no conclusion here transfers to a subagent
   workflow, which is how most audit and review pipelines actually run.
6. **Five harness defects were found and fixed mid-project.** In order: results lost to
   `/tmp` reaping (~$12 of runs); a shell script edited while bash was still reading it;
   an unpassable `rg -q` gate that failed 6/6 runs identically; a *vacuous* gate that
   passed on a clean tree and handed every run free credit; and a set-grader that counted
   requested format headers as false positives. The first three were found by inspection
   after the fact; the last two were found by `bench/harness/validate_gates.py`, which
   executes every gate against a pristine tree and asserts each one fails-on-clean or
   passes-on-clean as declared. All affected runs were discarded and re-run, with
   `.bak` files retaining the originals.
