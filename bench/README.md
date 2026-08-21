# Pathfinder A/B Benchmark

A falsifiable answer to one question: **does exposing Pathfinder's MCP tools to a coding
agent make it cheaper, more accurate, or more effective at real work in this repository —
and by how much?**

Everything here is mechanical. No score depends on reading the model's prose, and no
conclusion depends on anyone's impression of how the tools "feel".

> **Results and recommendation: [`results/FINDINGS.md`](results/FINDINGS.md)**
> 150 runs, 2 models, $10.96 of API spend.

---

## 1. What is being compared

Three arms. Identical model, identical temperature, identical agent system prompt,
identical task text. The **only** independent variable is the toolset exposed to the model.

| Arm | Built-in `read`/`grep`/`glob`/`list` | Pathfinder MCP tools | Purpose |
|---|---|---|---|
| `baseline` | yes | **not loaded at all** | Control. What an agent does today. |
| `pathfinder` | **denied** | yes | Does Pathfinder work as a full replacement? |
| `both` | yes | yes | Does the model *choose* Pathfinder when free to? |

The `both` arm is the intuitivity measurement. Nothing in the prompt or the system message
mentions Pathfinder, so adoption there is driven purely by the MCP server's own tool
descriptions — which is exactly the surface a real user would install.

### Controls

- The frozen snapshot's `AGENTS.md` is **deleted**. The live repo's `AGENTS.md` contains a
  Pathfinder tool-routing section that would coach the `both` arm and confuse the
  `baseline` arm with references to tools it cannot call.
- `opencode run --pure` — no user plugins.
- `task`, `webfetch`, `websearch`, `skill`, `todowrite` are denied in every arm so that
  delegation and bookkeeping cannot distort token accounting.
- `bash` is denied for the whole navigation suite in every arm, so an arm cannot bypass its
  own toolset with `cat`/`rg`. It is enabled for the mutation suite in every arm, because
  `cargo check` is part of the job.
- Runs execute against a **frozen `git worktree`** at a pinned commit, reset to pristine
  before and after every run. The live working tree is never touched.
- The full `(task x arm x rep)` matrix is **shuffled with a fixed seed** before execution,
  so provider-side caching, rate limiting and model drift cannot systematically favour the
  arm that happened to run first.
- Every run is a fresh session. No `--continue`.

---

## 2. Task suites

Tasks are real questions you would ask while working in this codebase, and every one has a
ground truth that was verified by hand against the pinned commit.

### `nav` — read-only comprehension and navigation (9 tasks)

| Task | Capability under test |
|---|---|
| `N1_blast_radius` | Find all 12 production call sites of `Sandbox::check`, excluding tests/benches |
| `N2_tool_registry` | Enumerate the MCP tools in the `#[tool_router]` block and their delegates |
| `N3_error_localize` | Map a user-visible hint string back to file, function and enum variant |
| `N4_dispatch_chain` | Follow `trace` tool dispatch to its three scope handlers |
| `N5_signature_extract` | Extract the exact public API of `Sandbox` (and *not* the private fns) |
| `N6_line_to_symbol` | Resolve two `file:line` backtrace frames to enclosing function names |
| `N7_enum_exhaustive` | Count enum variants and identify which `match self` arms lack a catch-all |
| `N8_reverse_callgraph` | Two-hop reverse call graph, excluding self-recursion |
| `N9_needle_in_large_file` | Extract four facts about one method inside a 1,361-line file |

`N5` and `N7` are deliberately trap-laden: `N5` is scored down for listing private
functions, `N7` requires noticing a `_ => None` catch-all. Recall-only answers do not pass.

### `edit` — mutation under compiler constraints (1 task)

`E1_add_error_variant` adds a variant to `PathfinderError` and threads it through
`error_code()`, `hint()` and `to_details()`, then must survive
`cargo check -p pathfinder-mcp-common --all-targets` under `clippy::unwrap_used = deny`.
Graded by six shell gates including a collateral-damage check that fails the run if any
file outside `error.rs` was modified.

---

## 3. Grading

| Grader | Used by | Rule |
|---|---|---|
| `set_of_lines` | N1 | **F1** over the answer set. Precision counts: dumping every `.check(` hit including test files is not a correct answer, and a recall-only metric would reward it. |
| `regex_all` | N2–N9 | Fraction of required patterns matched, minus 0.25 per forbidden pattern hit. |
| `shell_gate` | E1 | Fraction of gate commands that exit as expected, run inside the mutated worktree. |

`passed` requires a score of 1.0. Partial scores are retained so a near-miss is
distinguishable from a total failure.

Answers are extracted from a `<answer>...</answer>` block. Malformed closing tags emitted
by weaker models are repaired before grading — applied identically to all arms, and it only
ever strips non-answer noise.

---

## 4. Metrics

**Cost**
- `billed_tokens` = input + output + cache_read + cache_write. Cache traffic is billed, and
  a comparison that counts only `input` would hide the cost of shipping ~6 KB of tool schema
  on every request.
- `context_tokens` = everything that occupied the context window.
- `cost_usd` as reported by the provider.

**Effort**
- `tool_calls`, `steps`, `tool_output_chars`, `wall_s`.

**Correctness**
- `score`, `pass_rate` (Wilson interval).
- **`tokens per solved task`** = mean billed tokens / pass rate. This is the headline
  number: an arm that is cheap because it fails is not cheap.

**Fixed overhead (§9 of the report)**
- Median context tokens on the *first* request of each run — before any retrieval has
  happened. Differencing this across arms isolates the MCP schema tax paid on every
  single request for the lifetime of the session.

**Ergonomics (`mechanics.py`)**
- Per-tool rates of: hard errors, invalid-parameter rejections, empty results, degraded
  responses, exact-repeat calls (self-correction pressure).
- **Information yield**: task score earned per 1,000 characters of tool output.

**Statistics**
- Means carry 95% bootstrap CIs (10,000 resamples).
- Between-arm differences carry a bootstrap CI on the mean difference plus **Cliff's delta**,
  a non-parametric effect size that survives the heavy right tail that agent token counts
  always have. A difference is only called out when its 95% CI excludes zero.
- Pass rates carry Wilson score intervals.

---

## 5. Reproducing

```bash
# 1. Frozen snapshot at a pinned commit
git worktree add --detach /tmp/opencode/pf-bench/repo <COMMIT>
rm -f /tmp/opencode/pf-bench/repo/AGENTS.md

# 2. Build the server under test from that exact snapshot
cd /tmp/opencode/pf-bench/repo
CARGO_TARGET_DIR=/tmp/opencode/pf-bench/target cargo build --release -p pathfinder-mcp

# 3. Compatibility gate: which models can even load the tool schemas
python3 bench/harness/probe_models.py \
  --repo /tmp/opencode/pf-bench/repo \
  --mcp-bin /tmp/opencode/pf-bench/target/release/pathfinder-mcp \
  --models "model-a,model-b"

# 4. Run the matrix (resumable — re-running skips completed run_ids)
python3 bench/harness/runner.py \
  --repo /tmp/opencode/pf-bench/repo \
  --mcp-bin /tmp/opencode/pf-bench/target/release/pathfinder-mcp \
  --model <PROVIDER/MODEL> --suites nav,edit --reps 3 \
  --seed 20260817 --out bench/results/<NAME>

# 5. Reports
python3 bench/harness/analyze.py   --results bench/results/<NAME> --out bench/results/<NAME>/report.txt
python3 bench/harness/mechanics.py --results bench/results/<NAME> --out bench/results/<NAME>/mechanics.txt
```

---

## 6. Known limitations

Stated up front so the numbers are not over-read.

1. **Single repository.** All tasks are Rust in this workspace. Pathfinder's value on
   TypeScript, Python or a polyglot monorepo is not measured here.
2. **Task authorship bias.** The tasks were written by inspecting this codebase. They lean
   toward navigation and comprehension because that is what Pathfinder targets — which
   biases *in favour* of Pathfinder, not against it.
3. **Nine navigation tasks is a small suite.** Per-task conclusions are weak; only the
   pooled effect sizes with CIs should be quoted.
4. **Provider-reported token counts** are taken at face value.
5. **LSP warm-up is not controlled.** rust-analyzer indexing state at the moment a run
   starts affects the `pathfinder` and `both` arms only. Shuffled execution spreads this
   across arms rather than eliminating it.
6. **One model family per run.** Results are reported per model; cross-model
   generalisation is only as strong as the number of models probed.
