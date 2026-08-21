#!/usr/bin/env python3
"""Turn raw run records into a defensible comparison report.

Reports effect sizes with bootstrap confidence intervals rather than bare
means. With n in the tens, a difference in means without an interval is not
evidence, and the whole point of this exercise is to replace opinion with
something falsifiable.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import statistics as stats
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Callable, Iterable

MCP_PREFIXES = ("pathfinder_", "codegraph_")
BUILTIN_NAV_TOOLS = {"read", "grep", "glob", "list"}


# ────────────────────────────── loading ──────────────────────────────


def load_runs(paths: Iterable[Path]) -> list[dict[str, Any]]:
    runs: list[dict[str, Any]] = []
    for p in paths:
        for line in p.read_text(encoding="utf-8").splitlines():
            if line.strip():
                runs.append(json.loads(line))
    return runs


def billed_tokens(r: dict[str, Any]) -> int:
    """Tokens the provider charges for.

    Cache reads are billed (at a discount) and cache writes are billed at a
    premium, so a comparison that only counts `input` would hide the real cost
    difference between a lean toolset and one that ships 6 KB of tool schema
    on every request.
    """
    return (
        int(r["tok_input"])
        + int(r["tok_output"])
        + int(r["tok_cache_read"])
        + int(r["tok_cache_write"])
    )


def context_tokens(r: dict[str, Any]) -> int:
    """Tokens that actually occupied the model's context window."""
    return int(r["tok_input"]) + int(r["tok_cache_read"]) + int(r["tok_cache_write"])


def tool_output_chars(r: dict[str, Any]) -> int:
    return sum(int(t["output_chars"]) for t in r["tool_calls"])


# ────────────────────────────── statistics ──────────────────────────────


def bootstrap_ci(
    sample: list[float],
    stat: Callable[[list[float]], float] = stats.mean,
    iters: int = 10000,
    alpha: float = 0.05,
    seed: int = 7,
) -> tuple[float, float]:
    if len(sample) < 2:
        v = stat(sample) if sample else float("nan")
        return (v, v)
    rng = random.Random(seed)
    n = len(sample)
    draws = sorted(stat([sample[rng.randrange(n)] for _ in range(n)]) for _ in range(iters))
    lo = draws[int((alpha / 2) * iters)]
    hi = draws[min(iters - 1, int((1 - alpha / 2) * iters))]
    return (lo, hi)


def bootstrap_diff_ci(
    a: list[float],
    b: list[float],
    iters: int = 10000,
    alpha: float = 0.05,
    seed: int = 11,
    stat: Callable[[list[float]], float] = stats.mean,
) -> tuple[float, float, float]:
    """CI for stat(b) - stat(a). Returns (point, lo, hi).

    `stat` defaults to the mean but should be set to the median for token
    counts: a single runaway agent loop can move a 27-run mean by 3x while
    saying nothing about typical behaviour.
    """
    point = stat(b) - stat(a)
    if len(a) < 2 or len(b) < 2:
        return (point, float("nan"), float("nan"))
    rng = random.Random(seed)
    na, nb = len(a), len(b)
    draws = sorted(
        stat([b[rng.randrange(nb)] for _ in range(nb)])
        - stat([a[rng.randrange(na)] for _ in range(na)])
        for _ in range(iters)
    )
    return (point, draws[int((alpha / 2) * iters)], draws[min(iters - 1, int((1 - alpha / 2) * iters))])


def cliffs_delta(a: list[float], b: list[float]) -> float:
    """Non-parametric effect size in [-1, 1]. Robust to the heavy tails that
    agent-run token counts always have."""
    if not a or not b:
        return float("nan")
    gt = sum(1 for x in b for y in a if x > y)
    lt = sum(1 for x in b for y in a if x < y)
    return (gt - lt) / (len(a) * len(b))


def delta_label(d: float) -> str:
    ad = abs(d)
    if math.isnan(d):
        return "n/a"
    if ad < 0.147:
        return "negligible"
    if ad < 0.33:
        return "small"
    if ad < 0.474:
        return "medium"
    return "large"


def wilson_ci(successes: int, n: int, z: float = 1.96) -> tuple[float, float]:
    if n == 0:
        return (float("nan"), float("nan"))
    p = successes / n
    denom = 1 + z * z / n
    centre = (p + z * z / (2 * n)) / denom
    margin = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / denom
    return (max(0.0, centre - margin), min(1.0, centre + margin))


# ────────────────────────────── aggregation ──────────────────────────────

METRICS: dict[str, Callable[[dict[str, Any]], float]] = {
    "billed_tokens": lambda r: float(billed_tokens(r)),
    "context_tokens": lambda r: float(context_tokens(r)),
    "output_tokens": lambda r: float(r["tok_output"]),
    "cost_usd": lambda r: float(r["cost_usd"]),
    "tool_calls": lambda r: float(len(r["tool_calls"])),
    "tool_output_chars": lambda r: float(tool_output_chars(r)),
    "steps": lambda r: float(r["steps"]),
    "wall_s": lambda r: float(r["wall_ms"]) / 1000.0,
    "score": lambda r: float(r["score"]),
    "tool_errors": lambda r: float(r["tool_errors"]),
}


def fmt(v: float) -> str:
    if math.isnan(v):
        return "n/a"
    if abs(v) >= 1000:
        return f"{v:,.0f}"
    if abs(v) >= 10:
        return f"{v:.1f}"
    if abs(v) >= 1:
        return f"{v:.2f}"
    return f"{v:.4f}"


def section(title: str) -> str:
    return f"\n{title}\n{'-' * len(title)}"


def per_arm_table(runs: list[dict[str, Any]], arms: list[str]) -> str:
    out = [
        section("1. PER-ARM AGGREGATE"),
        "Two rows per metric. `med` is the robust view (a single runaway agent loop",
        "can triple a 27-run mean); `mean` is what you actually pay in aggregate.",
        "",
    ]
    header = f"{'metric':<22}{'stat':<6}" + "".join(f"{a:>28}" for a in arms)
    out.append(header)
    for name, fn in METRICS.items():
        for label, stat in (("med", stats.median), ("mean", stats.mean)):
            row = f"{name if label == 'med' else '':<22}{label:<6}"
            for arm in arms:
                vals = [fn(r) for r in runs if r["arm"] == arm]
                if not vals:
                    row += f"{'-':>28}"
                    continue
                lo, hi = bootstrap_ci(vals, stat=stat)
                row += f"{fmt(stat(vals)) + ' [' + fmt(lo) + ',' + fmt(hi) + ']':>28}"
            out.append(row)

    row = f"{'pass_rate':<22}{'':<6}"
    for arm in arms:
        sub = [r for r in runs if r["arm"] == arm]
        k = sum(1 for r in sub if r["passed"])
        if not sub:
            row += f"{'-':>28}"
            continue
        lo, hi = wilson_ci(k, len(sub))
        row += f"{f'{k}/{len(sub)} ' + fmt(k / len(sub)) + ' [' + fmt(lo) + ',' + fmt(hi) + ']':>28}"
    out.append(row)
    return "\n".join(out)


def efficiency_table(runs: list[dict[str, Any]], arms: list[str]) -> str:
    """Cost per SOLVED task. The metric that actually decides the question:
    a cheap arm that fails is not cheap."""
    out = [section("2. COST OF CORRECTNESS (tokens per solved task)")]
    out.append(f"{'arm':<14}{'billed tok/run':>16}{'pass rate':>12}{'tok / solved':>16}{'usd / solved':>16}")
    for arm in arms:
        sub = [r for r in runs if r["arm"] == arm]
        if not sub:
            continue
        tok = stats.mean([billed_tokens(r) for r in sub])
        usd = stats.mean([r["cost_usd"] for r in sub])
        pr = sum(1 for r in sub if r["passed"]) / len(sub)
        per_solved = tok / pr if pr > 0 else float("inf")
        usd_solved = usd / pr if pr > 0 else float("inf")
        out.append(
            f"{arm:<14}{fmt(tok):>16}{fmt(pr):>12}"
            f"{(fmt(per_solved) if pr else 'inf'):>16}{(fmt(usd_solved) if pr else 'inf'):>16}"
        )
    return "\n".join(out)


def paired_effects(runs: list[dict[str, Any]], base: str, arms: list[str]) -> str:
    out = [
        section(f"3. EFFECT vs '{base}' (bootstrap CI on MEDIAN diff; Cliff's delta)"),
        "Median difference, because token distributions are heavy-tailed. Cliff's delta is",
        "the fraction of pairwise comparisons favouring one arm and is outlier-immune, so",
        "when it disagrees with the mean, believe Cliff's delta about typical behaviour and",
        "the mean about the bill.",
    ]
    for arm in arms:
        if arm == base:
            continue
        out.append(f"\n  {arm} vs {base}")
        out.append(
            f"    {'metric':<20}{'med delta':>14}{'95% CI':>26}{'rel%':>10}{'cliff':>9}  effect"
        )
        for name, fn in METRICS.items():
            a = [fn(r) for r in runs if r["arm"] == base]
            b = [fn(r) for r in runs if r["arm"] == arm]
            if len(a) < 2 or len(b) < 2:
                continue
            point, lo, hi = bootstrap_diff_ci(a, b, stat=stats.median)
            base_med = stats.median(a)
            rel = (point / base_med * 100) if base_med else float("nan")
            d = cliffs_delta(a, b)
            sig = "" if (math.isnan(lo) or (lo <= 0 <= hi)) else "  *"
            out.append(
                f"    {name:<20}{fmt(point):>14}"
                f"{'[' + fmt(lo) + ', ' + fmt(hi) + ']':>26}"
                f"{fmt(rel):>10}{fmt(d):>9}  {delta_label(d)}{sig}"
            )
    out.append("\n  * = 95% CI excludes zero.  cliff > 0 means the arm spends MORE than baseline.")
    return "\n".join(out)


def per_task_table(runs: list[dict[str, Any]], arms: list[str]) -> str:
    out = [section("4. PER-TASK BREAKDOWN (mean score | mean billed tokens | mean tool calls)")]
    task_ids = sorted({r["task_id"] for r in runs})
    out.append(f"{'task':<26}" + "".join(f"{a:>26}" for a in arms))
    for tid in task_ids:
        row = f"{tid:<26}"
        for arm in arms:
            sub = [r for r in runs if r["task_id"] == tid and r["arm"] == arm]
            if not sub:
                row += f"{'-':>26}"
                continue
            sc = stats.mean([r["score"] for r in sub])
            tk = stats.mean([billed_tokens(r) for r in sub])
            tc = stats.mean([len(r["tool_calls"]) for r in sub])
            row += f"{f'{sc:.2f} | {tk:,.0f} | {tc:.1f}':>26}"
        out.append(row)
    return "\n".join(out)


def tool_usage(runs: list[dict[str, Any]], arms: list[str]) -> str:
    out = [section("5. TOOL USAGE PROFILE")]
    for arm in arms:
        sub = [r for r in runs if r["arm"] == arm]
        if not sub:
            continue
        counter: Counter[str] = Counter()
        chars: Counter[str] = Counter()
        errs: Counter[str] = Counter()
        for r in sub:
            for t in r["tool_calls"]:
                counter[t["name"]] += 1
                chars[t["name"]] += int(t["output_chars"])
                if t.get("error"):
                    errs[t["name"]] += 1
        total = sum(counter.values()) or 1
        out.append(f"\n  {arm}  (n={len(sub)} runs, {total} tool calls)")
        out.append(f"    {'tool':<26}{'calls':>8}{'share':>9}{'avg out chars':>16}{'errors':>9}")
        for name, cnt in counter.most_common():
            out.append(
                f"    {name:<26}{cnt:>8}{cnt / total * 100:>8.1f}%"
                f"{chars[name] / cnt:>16,.0f}{errs[name]:>9}"
            )
    return "\n".join(out)


def adoption(runs: list[dict[str, Any]]) -> str:
    """Intuitivity proxy.

    In a free-choice arm both the host's built-in tools and an MCP server's
    tools are available, and nothing in the prompt or system message mentions
    the server. Whatever the model reaches for is therefore driven purely by
    the server's own tool descriptions — which is exactly the surface a user
    gets on install.
    """
    blocks: list[str] = []
    for free_arm in ("both", "cg_both"):
        sub = [r for r in runs if r["arm"] == free_arm]
        if sub:
            blocks.append(_adoption_block(sub, free_arm))
    return "\n".join(blocks)


def _adoption_block(sub: list[dict[str, Any]], free_arm: str) -> str:
    out = [section(f"6. SPONTANEOUS ADOPTION (arm '{free_arm}' — free choice, no prompt steering)")]
    mcp_calls = 0
    builtin_calls = 0
    runs_with_mcp = 0
    first_tool: Counter[str] = Counter()
    for r in sub:
        names = [t["name"] for t in r["tool_calls"]]
        if names:
            first_tool[names[0]] += 1
        mcp = sum(1 for n in names if n.startswith(MCP_PREFIXES))
        builtin = sum(1 for n in names if n in BUILTIN_NAV_TOOLS)
        mcp_calls += mcp
        builtin_calls += builtin
        if mcp:
            runs_with_mcp += 1
    total_nav = mcp_calls + builtin_calls
    lo, hi = wilson_ci(runs_with_mcp, len(sub))
    out.append(
        f"  runs that used >=1 MCP tool : {runs_with_mcp}/{len(sub)} "
        f"({runs_with_mcp / len(sub) * 100:.0f}%, 95% CI [{lo * 100:.0f}%, {hi * 100:.0f}%])"
    )
    if total_nav:
        out.append(
            f"  navigation call share       : MCP {mcp_calls}/{total_nav} "
            f"({mcp_calls / total_nav * 100:.0f}%), built-in {builtin_calls}/{total_nav} "
            f"({builtin_calls / total_nav * 100:.0f}%)"
        )
    out.append("  first tool reached for:")
    for name, cnt in first_tool.most_common():
        out.append(f"    {name:<28}{cnt:>5}  ({cnt / len(sub) * 100:.0f}%)")
    return "\n".join(out)


def failure_forensics(runs: list[dict[str, Any]], arms: list[str]) -> str:
    out = [section("7. FAILURE FORENSICS")]
    for arm in arms:
        sub = [r for r in runs if r["arm"] == arm]
        if not sub:
            continue
        no_answer = sum(1 for r in sub if r["answer"] is None)
        stream_err = sum(1 for r in sub if r["stream_error"])
        timeouts = sum(1 for r in sub if r["timed_out"])
        tool_err_runs = sum(1 for r in sub if r["tool_errors"] > 0)
        out.append(
            f"  {arm:<12} no_answer={no_answer:<4} stream_error={stream_err:<4} "
            f"timeout={timeouts:<4} runs_with_tool_error={tool_err_runs}"
        )
    failed = [r for r in runs if r["score"] < 0.999]
    out.append(f"\n  runs that did not fully solve the task ({len(failed)}/{len(runs)}):")
    for r in sorted(failed, key=lambda x: (x["score"], -billed_tokens(x)))[:25]:
        detail = r.get("grade_detail") or {}
        why = detail.get("error") or (
            f"missing={detail.get('missing')}" if detail.get("missing") else ""
        ) or (
            f"failed={len(detail.get('failed_patterns', []))}" if detail.get("failed_patterns") else ""
        ) or (
            ",".join(g["name"] for g in detail.get("gates", []) if not g["ok"])
        )
        out.append(
            f"    {r['task_id']:<24}{r['arm']:<12}score={r['score']:.2f} "
            f"tok={billed_tokens(r):>7,}  {str(why)[:110]}"
        )
    return "\n".join(out)


def token_decomposition(runs: list[dict[str, Any]], arms: list[str]) -> str:
    """Separates fixed schema overhead from variable retrieval cost.

    A tool server pays for itself only if the retrieval it saves exceeds the
    schema it injects into every single request. This table makes that
    trade-off explicit instead of burying it in a total.
    """
    out = [section("8. TOKEN DECOMPOSITION (where the tokens actually go)")]
    out.append(
        f"{'arm':<14}{'1st-step ctx':>14}{'tot ctx':>12}{'output':>10}"
        f"{'tool out chars':>16}{'chars/call':>12}{'steps':>8}"
    )
    for arm in arms:
        sub = [r for r in runs if r["arm"] == arm]
        if not sub:
            continue
        ctx = stats.mean([context_tokens(r) for r in sub])
        outp = stats.mean([r["tok_output"] for r in sub])
        toc = stats.mean([tool_output_chars(r) for r in sub])
        calls = stats.mean([len(r["tool_calls"]) for r in sub]) or 1
        stp = stats.mean([r["steps"] for r in sub])
        out.append(
            f"{arm:<14}{'see §9':>14}{fmt(ctx):>12}{fmt(outp):>10}"
            f"{fmt(toc):>16}{fmt(toc / calls):>12}{fmt(stp):>8}"
        )
    return "\n".join(out)


def schema_overhead(results_dir: Path, runs: list[dict[str, Any]], arms: list[str]) -> str:
    """Measure the fixed per-request cost of each arm's tool schema.

    Read the very first step_finish of each run: at that point the only thing
    in context is the system prompt, the tool definitions and the task prompt.
    Differencing that across arms isolates the MCP schema tax.
    """
    out = [section("9. FIXED SCHEMA OVERHEAD (first-request context tokens)")]
    per_arm: dict[str, list[float]] = defaultdict(list)
    raw_dir = results_dir / "raw"
    for r in runs:
        f = raw_dir / f"{r['run_id']}.jsonl"
        if not f.exists():
            continue
        for line in f.read_text(encoding="utf-8").splitlines():
            if '"step_finish"' not in line:
                continue
            try:
                evt = json.loads(line)
            except json.JSONDecodeError:
                continue
            tk = (evt.get("part") or {}).get("tokens") or {}
            cache = tk.get("cache") or {}
            first = int(tk.get("input") or 0) + int(cache.get("read") or 0) + int(cache.get("write") or 0)
            if first > 0:
                per_arm[r["arm"]].append(float(first))
            break
    out.append(f"{'arm':<14}{'median 1st-req ctx tokens':>28}{'n':>6}")
    for arm in arms:
        vals = per_arm.get(arm) or []
        if not vals:
            out.append(f"{arm:<14}{'no data':>28}{0:>6}")
            continue
        out.append(f"{arm:<14}{fmt(stats.median(vals)):>28}{len(vals):>6}")
    if per_arm.get("baseline") and per_arm.get("pathfinder"):
        tax = stats.median(per_arm["pathfinder"]) - stats.median(per_arm["baseline"])
        out.append(f"\n  pathfinder schema tax vs baseline : {tax:+,.0f} tokens on EVERY request")
    if per_arm.get("baseline") and per_arm.get("both"):
        tax = stats.median(per_arm["both"]) - stats.median(per_arm["baseline"])
        out.append(f"  both-arm schema tax vs baseline   : {tax:+,.0f} tokens on EVERY request")
    return "\n".join(out)


def matched_correctness(runs: list[dict[str, Any]], base: str, arms: list[str]) -> str:
    """Compare cost only among runs that actually got the right answer.

    Without this control an arm can look cheap simply by giving up early. Here
    every run in the pool scored 1.0, so the token difference is a pure
    retrieval-efficiency difference.
    """
    out = [section("10. COST AT MATCHED CORRECTNESS (score == 1.0 runs only)")]
    solved = [r for r in runs if r["score"] >= 0.999]
    if not solved:
        return "\n".join(out + ["  no fully-correct runs"])
    out.append(
        f"{'arm':<14}{'n solved':>9}{'med tok':>11}{'95% CI':>24}{'vs base':>10}"
        f"{'mean tok':>11}{'vs base':>10}{'cliff':>8}"
    )
    base_vals = [float(billed_tokens(r)) for r in solved if r["arm"] == base]
    for arm in arms:
        vals = [float(billed_tokens(r)) for r in solved if r["arm"] == arm]
        if not vals:
            continue
        lo, hi = bootstrap_ci(vals, stat=stats.median)
        if arm == base or len(base_vals) < 2 or len(vals) < 2:
            rel_med, rel_mean, d = "-", "-", float("nan")
        else:
            pm, _, _ = bootstrap_diff_ci(base_vals, vals, stat=stats.median)
            pa, _, _ = bootstrap_diff_ci(base_vals, vals)
            rel_med = f"{pm / stats.median(base_vals) * 100:+.0f}%"
            rel_mean = f"{pa / stats.mean(base_vals) * 100:+.0f}%"
            d = cliffs_delta(base_vals, vals)
        out.append(
            f"{arm:<14}{len(vals):>9}{fmt(stats.median(vals)):>11}"
            f"{'[' + fmt(lo) + ', ' + fmt(hi) + ']':>24}{rel_med:>10}"
            f"{fmt(stats.mean(vals)):>11}{rel_mean:>10}{fmt(d):>8}"
        )
    return "\n".join(out)


def tail_risk(runs: list[dict[str, Any]], arms: list[str]) -> str:
    """Predictability, not just average cost.

    An agent loop that occasionally burns 880k tokens on a task whose median is
    16k is a different product risk from one that reliably costs 36k. Budgeting,
    rate limits and user trust all key off the tail, not the mean.
    """
    out = [section("12. TAIL RISK / PREDICTABILITY (billed tokens)")]
    out.append(
        f"{'arm':<14}{'p50':>11}{'p90':>11}{'max':>12}{'max/p50':>10}"
        f"{'runs>3x p50':>13}{'step-cap hits':>15}"
    )
    for arm in arms:
        vals = sorted(float(billed_tokens(r)) for r in runs if r["arm"] == arm)
        if not vals:
            continue
        p50 = stats.median(vals)
        p90 = vals[min(len(vals) - 1, int(0.9 * len(vals)))]
        blow = sum(1 for v in vals if p50 and v > 3 * p50)
        # A run that consumed its entire step budget is an agent that never
        # converged, whatever it eventually printed.
        capped = sum(
            1
            for r in runs
            if r["arm"] == arm and r["steps"] >= (40 if r["suite"] == "nav" else 60)
        )
        out.append(
            f"{arm:<14}{p50:>11,.0f}{p90:>11,.0f}{max(vals):>12,.0f}"
            f"{(max(vals) / p50 if p50 else float('nan')):>10.1f}"
            f"{f'{blow}/{len(vals)}':>13}{f'{capped}/{len(vals)}':>15}"
        )
    return "\n".join(out)


def win_table(runs: list[dict[str, Any]], arms: list[str]) -> str:
    """Per task: which arm solved it, and which solved it cheapest."""
    out = [section("11. PER-TASK WINNER (cheapest arm that reached score 1.0)")]
    out.append(
        f"{'task':<26}  {'winner':<12}{'winner tok':>12}  {'runner-up':<12}{'gap':>8}  arms solving"
    )
    for tid in sorted({r["task_id"] for r in runs}):
        rows = []
        solvers = []
        for arm in arms:
            sub = [r for r in runs if r["task_id"] == tid and r["arm"] == arm]
            ok = [r for r in sub if r["score"] >= 0.999]
            if ok:
                solvers.append(f"{arm}({len(ok)}/{len(sub)})")
                rows.append((arm, stats.mean([billed_tokens(r) for r in ok])))
        if not rows:
            out.append(f"{tid:<26}  {'NONE':<12}{'-':>12}  {'-':<12}{'-':>8}  -")
            continue
        rows.sort(key=lambda x: x[1])
        w_arm, w_tok = rows[0]
        r_arm, r_tok = rows[1] if len(rows) > 1 else ("-", float("nan"))
        gap = f"{(r_tok / w_tok - 1) * 100:+.0f}%" if len(rows) > 1 and w_tok else "-"
        out.append(
            f"{tid:<26}  {w_arm:<12}{w_tok:>12,.0f}  {r_arm:<12}{gap:>8}  {', '.join(solvers)}"
        )
    return "\n".join(out)


def schema_attribution(
    results_dir: Path, runs: list[dict[str, Any]], base: str, arms: list[str]
) -> str:
    """Split each arm's extra token cost into fixed schema tax vs real retrieval.

    The tool definitions are re-sent on every request in the agent loop, so a
    fat schema is multiplied by step count. Separating the two tells you
    whether the fix is "make the tools terser" or "make the tools smarter",
    which are very different amounts of work.
    """
    out = [section("13. WHERE THE EXTRA TOKENS COME FROM (schema tax vs retrieval)")]

    first_ctx: dict[str, list[float]] = defaultdict(list)
    raw_dir = results_dir / "raw"
    for r in runs:
        f = raw_dir / f"{r['run_id']}.jsonl"
        if not f.exists():
            continue
        for line in f.read_text(encoding="utf-8").splitlines():
            if '"step_finish"' not in line:
                continue
            try:
                evt = json.loads(line)
            except json.JSONDecodeError:
                continue
            tk = (evt.get("part") or {}).get("tokens") or {}
            cache = tk.get("cache") or {}
            v = int(tk.get("input") or 0) + int(cache.get("read") or 0) + int(cache.get("write") or 0)
            if v > 0:
                first_ctx[r["arm"]].append(float(v))
            break

    if base not in first_ctx:
        return "\n".join(out + ["  no baseline data"])

    base_first = stats.median(first_ctx[base])
    base_tok = stats.median([float(billed_tokens(r)) for r in runs if r["arm"] == base])

    out.append(
        f"{'arm':<14}{'med total delta':>17}{'per-req tax':>13}{'med steps':>11}"
        f"{'tax x steps':>13}{'tax share':>11}{'retrieval delta':>17}"
    )
    for arm in arms:
        if arm == base or arm not in first_ctx:
            continue
        arm_tok = stats.median([float(billed_tokens(r)) for r in runs if r["arm"] == arm])
        steps = stats.median([float(r["steps"]) for r in runs if r["arm"] == arm])
        tax = stats.median(first_ctx[arm]) - base_first
        total_delta = arm_tok - base_tok
        tax_total = tax * steps
        share = (tax_total / total_delta * 100) if total_delta else float("nan")
        out.append(
            f"{arm:<14}{total_delta:>17,.0f}{tax:>13,.0f}{steps:>11.1f}"
            f"{tax_total:>13,.0f}{share:>10.0f}%{total_delta - tax_total:>17,.0f}"
        )
    out.append(
        "\n  Reading: 'tax x steps' is the fixed tool-schema cost re-sent on every request."
        "\n  'retrieval delta' is what remains — the genuine cost of the tools' own output."
    )
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True, type=Path)
    ap.add_argument("--arms", default="baseline,pathfinder,both")
    ap.add_argument("--base", default="baseline")
    ap.add_argument("--suite", default=None)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    runs_file = args.results / "runs.jsonl"
    runs = load_runs([runs_file])
    if args.suite:
        runs = [r for r in runs if r["suite"] == args.suite]
    if not runs:
        print("no runs")
        return 1

    arms = [a for a in args.arms.split(",") if any(r["arm"] == a for r in runs)]

    parts = [
        "=" * 100,
        "PATHFINDER A/B BENCHMARK REPORT",
        "=" * 100,
        f"runs={len(runs)}  arms={arms}  models={sorted({r['model'] for r in runs})}",
        f"tasks={sorted({r['task_id'] for r in runs})}",
        per_arm_table(runs, arms),
        efficiency_table(runs, arms),
        paired_effects(runs, args.base, arms),
        per_task_table(runs, arms),
        tool_usage(runs, arms),
        adoption(runs),
        failure_forensics(runs, arms),
        schema_overhead(args.results, runs, arms),
        matched_correctness(runs, args.base, arms),
        win_table(runs, arms),
        tail_risk(runs, arms),
        schema_attribution(args.results, runs, args.base, arms),
        "",
    ]
    report = "\n".join(p for p in parts if p)
    print(report)
    if args.out:
        args.out.write_text(report, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
