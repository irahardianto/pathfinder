# Pathfinder: what the data says, and what to build next

Derived from **485 benchmark runs** across 6 suites, 3 repositories, 2 languages and
3 models. Every number below is traceable to `bench/results/*/runs.jsonl`.

This is not a pitch. It starts with the case against the project, because that case is
strong and any plan that does not survive it is not worth executing.

---

## 1. The case against continuing

### Pathfinder has never significantly beaten the baseline. Anywhere.

| suite | accuracy vs baseline | 95% CI | tokens | cost | tool calls |
|---|---|---|---|---|---|
| nav | **−0.065** | [−0.139, +0.000] | 2.9x | 2.1x | +0% |
| deep | +0.000 | [−0.042, +0.042] | 1.5x | 1.6x | −13% |
| resolve | +0.050 | [+0.000, +0.150] | 2.6x | 2.0x | −7% |
| implement | +0.000 | [0, 0] | 1.4x | 1.2x | −17% |
| breadth | +0.005 | [−0.070, +0.080] | 2.0x | 1.9x | −6% |

Five suites. Every confidence interval on accuracy spans zero. One of them
(`nav`) trends *against* Pathfinder. Meanwhile it costs **1.2x–2.9x more in every single
suite**.

### The cost ceiling is parity, not advantage

The token gap decomposes into fixed tool-schema overhead and real retrieval:

| regime | schema share of the gap | gap remaining at a **zero-byte** schema |
|---|---|---|
| nav (shallow) | 87–107% | ~0% |
| deep (architecture) | ~60% | **+20%** |

Even a perfect schema — 0 bytes, physically impossible with 7 tools — leaves Pathfinder
**+20% over baseline in the deep regime with no round-trip reduction**. The schema diet is
necessary and it is not sufficient.

### It is blocked on an entire model family

Every Gemini model tested (2.5-pro, 3.1-pro, 3.5-flash, 3.6-flash, 3.7-flash) **refuses to
load Pathfinder's tools**: `parameters.semantic_paths` uses `anyOf` with sibling keys, which
Vertex function-calling forbids. CodeGraph loads fine on all of them.

### It is positioned in the worst place on every axis

| | payload per call | tool calls | cost |
|---|---|---|---|
| grep + read | 4,307 chars | 8.0 | **cheapest** |
| **Pathfinder** | **10,702** | **7.5** | **middle** |
| CodeGraph | 18,818 | 13.5 | most |

Middle on payload, middle on cost, marginal on calls. It is neither the cheap option nor the
discovery option.

**An honest reading of the table above says: stop.**

---

## 2. The case for continuing — the two things that survived

Only two signals held up under repetition. Both are real, both are narrow, and neither is
what the project is currently built around.

### 2.1 Convergence — the strongest consistent signal in the dataset

Exact-repeat tool-call rate (the model re-issuing a query it already ran, i.e. the previous
answer did not satisfy it):

| suite | baseline | **Pathfinder** | CodeGraph |
|---|---|---|---|
| nav | 2.3% | **0.0%** | — |
| implement | 15.1% | **7.9%** | — |
| breadth (large repos) | 20.4% | **2.5%** | 4.9 repeats/run |
| nav, weak model | **39%** | **0%** | — |

On the weak model the baseline issued **34 exact-repeat `grep` calls out of 88**. Pathfinder
issued **0 out of 102**. On the two large repos the baseline repeated 20.4% of its calls;
Pathfinder repeated 2.5%.

This is why Pathfinder's tail is the flattest of any arm: worst-case/median of **3.1x**
against the baseline's **53.2x** (one baseline run consumed its entire step budget, burned
880k tokens, and scored 0).

**Pathfinder's measured identity is not speed and not accuracy. It is: the agent believes
the first answer and stops asking.**

### 2.2 Resolution precision — a systematic failure mode in the competition

The `resolve` suite targets cases where a plausible wrong answer is one grep away.

| arm | accuracy | perfect runs |
|---|---|---|
| grep + read | 0.950 | 7/8 |
| **Pathfinder** | **1.000** | **8/8** |
| CodeGraph | 0.919 | 5/8 |

The interesting part is not the score, it is the failure mode. Asked which concrete type
implements `Lawyer` at runtime, **CodeGraph answered `client/mod.rs` in both runs** — where
the type is *declared*. The trait impl lives in `client/lawyer_impl.rs`. It was confidently,
reproducibly wrong in the same way twice, because its cross-file resolution is documented as
*"best-effort name matching."* Pathfinder, resolving through rust-analyzer, was right both
times.

**Caveat, stated plainly:** a single graded error separates Pathfinder from the baseline at
n=8. That difference is not statistically significant. What *is* solid is CodeGraph being
worse, and the mechanism being systematic rather than random.

### 2.3 It handles TypeScript fine

Concern going in was that the LSP design would collapse on TS/JS. On juice-shop (642 TS/JS
files) Pathfinder scored **1.000 on 3 of 5 tasks** and posted the **lowest repeat rate of
any arm**. The LSP integration is not the liability it was feared to be.

---

## 3. What the market position actually is

The delegation experiment settles where each tool belongs. Token change when the agent may
delegate to sub-agents, ordered by how wide the question is:

| task | baseline | Pathfinder | CodeGraph |
|---|---|---|---|
| survey 61 route files | −28% | **−61%** | **−68%** |
| trace 1 auth chain | +16% | +58% | −65% |
| audit 1 pattern | +126% | −2% | +38% |
| enumerate 1 API | +85% | +71% | +100% |
| trace 1 call chain | +167% | +178% | +141% |

Discovery-heavy, wide, delegated work belongs to CodeGraph. Narrow, precise,
must-be-right work is where Pathfinder's two surviving properties live.

**These are complements, not competitors.** CodeGraph finds candidates fast and is
systematically wrong about which impl is which. Pathfinder is slow to explore and right
about exactly that.

---

## 4. The plan

Four phases, each with a measured entry condition and a kill condition. The same harness
validates every one, so no phase advances on opinion.

### Phase 1 — Stop the bleeding (mechanical, no design work)

| change | measured effect |
|---|---|
| Strip per-parameter prose duplicated from tool descriptions | −47% schema |
| Flatten `oneOf`/`anyOf` const unions to plain `enum` | −5% schema, **unblocks all Gemini models** |
| Cap tool descriptions at 6 lines | −24% schema |
| **All three** | **−72%: 5,983 → 1,674 tokens/request** |

Projected on measured deep-suite runs: **213,043 → 182,686 median tokens (+50% → +28% over
baseline).**

**Kill condition:** if the re-run shows the residual gap above +40% with no accuracy gain,
stop here and archive the project.

### Phase 2 — Delete four tools

`pathfinder_read` and `pathfinder_search` were matched or beaten by the host's own
`read`/`grep` on **every metric in every suite**, while carrying **35% of the schema
weight**. `explore` duplicates what a repo-map or `codegraph_explore` does better.
`health` is diagnostics, not capability.

Ship **three tools**: `locate`, `trace`, `inspect`. Schema drops to **~1,171 tokens (−80%)**.

This is also the configuration the frontier model chose on its own — in the free-choice arm
it reached for graph tools and left plain reads to the built-ins.

**Entry condition:** Phase 1 re-run shows the gap closing as projected.

### Phase 3 — Reposition as the verification layer

Stop competing on discovery. The data says Pathfinder loses that and will keep losing it.

Build the thing the benchmark says is missing: a **cheap, high-precision resolution check**
that a discovery tool or a human hands off to.

- Small payloads by construction — target **under 2,000 chars/call** versus today's 10,702.
  Answer "which impl, exactly" not "here is the neighbourhood."
- Explicitly designed to be called *after* `grep`/`codegraph_explore` produced candidates.
- Surface the `mod.rs` vs `lawyer_impl.rs` class of error as a first-class result: *"the
  name resolves here, but the trait impl you want is there."*

**Validation:** extend the `resolve` suite to 20+ tasks (n=8 is too small to have settled
anything) and require a **significant** accuracy win over both baseline and CodeGraph, with
CIs excluding zero. If accuracy ties at 20 tasks, the differentiator does not exist and the
project ends.

### Phase 4 — Hybrid, only if Phase 3 clears

CodeGraph for breadth, Pathfinder for the final resolution hop. The evidence for this is
already in hand: CodeGraph wins wide+delegated by 68%, and is systematically wrong on
resolution in a way Pathfinder is not.

That is a routing product, not an index product, and it is the only configuration in this
entire benchmark where both tools' measured strengths compose instead of overlapping.

---

## 5. What would make me say "stop" instead

- Phase 1 re-run leaves the gap above +40% with no accuracy gain.
- The extended `resolve` suite ties at n=20+. Convergence alone does not justify an MCP
  server; it justifies a lint.
- The `mod.rs`/`lawyer_impl.rs` failure turns out to be cheaply fixable in CodeGraph. Then
  the complement disappears and so does the niche.

---

## 6. Honest summary

The project as currently scoped — a 7-tool headless IDE that replaces `grep` and `read` —
is not supported by any measurement here. It is slower, more expensive, no more accurate,
and locked out of Google's model family.

What *is* supported, weakly but consistently, is that Pathfinder makes an agent **stop
re-asking**, and that it is **right about symbol resolution where name matching is
confidently wrong**. Those are two real properties that no other arm demonstrated.

They are worth roughly one narrow tool, built deliberately, and validated against a
resolve suite three times the current size. They are not worth the current surface area.

The honest framing for the path forward is not "Pathfinder was wrong." It is:

> **The retrieval problem is solved well enough by grep and by graphs. The resolution
> problem is not, and it is the one that silently breaks refactors. That is a smaller
> product than a headless IDE, and it is the one the data actually supports.**
