#!/usr/bin/env python3
"""Counterfactual: what would the benchmark have measured with a leaner schema?

Re-scores completed runs under the assumption that the tool-schema payload
shrinks by a given fraction. Because the schema sits in the request prefix and
is re-sent on every step, the saving is (tax_per_request x reduction x steps).

This is arithmetic on measured quantities, not a simulation of model behaviour:
it answers "how much of the gap is mechanically removable" and nothing more.
Behavioural effects of a shorter description (better or worse tool selection)
are explicitly out of scope and must be measured by re-running the benchmark.
"""

from __future__ import annotations

import argparse
import json
import statistics as stats
from collections import defaultdict
from pathlib import Path
from typing import Any


def billed(r: dict[str, Any]) -> float:
    return float(
        r["tok_input"] + r["tok_output"] + r["tok_cache_read"] + r["tok_cache_write"]
    )


def first_request_ctx(results: Path, run_id: str) -> float | None:
    f = results / "raw" / f"{run_id}.jsonl"
    if not f.exists():
        return None
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
        return float(v) if v > 0 else None
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True, type=Path)
    ap.add_argument("--suite", default="nav")
    ap.add_argument("--base", default="baseline")
    ap.add_argument(
        "--reductions",
        default="0.47,0.72",
        help="schema-size reduction fractions to model (see schema_audit.py variants)",
    )
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    runs = [
        json.loads(line)
        for line in (args.results / "runs.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    runs = [r for r in runs if r["suite"] == args.suite]

    firsts: dict[str, list[float]] = defaultdict(list)
    for r in runs:
        v = first_request_ctx(args.results, r["run_id"])
        if v is not None:
            firsts[r["arm"]].append(v)

    base_first = stats.median(firsts[args.base])
    base_med = stats.median([billed(r) for r in runs if r["arm"] == args.base])

    lines = [
        "=" * 96,
        f"COUNTERFACTUAL: LEANER TOOL SCHEMA  (suite={args.suite})",
        "=" * 96,
        f"baseline median billed tokens: {base_med:,.0f}",
        "",
        f"{'arm':<14}{'reduction':>11}{'tax/req':>10}{'med steps':>11}"
        f"{'saved':>10}{'proj. median':>14}{'vs baseline':>13}",
    ]

    for arm in sorted(firsts):
        if arm == args.base:
            continue
        arm_runs = [r for r in runs if r["arm"] == arm]
        tax = stats.median(firsts[arm]) - base_first
        actual_med = stats.median([billed(r) for r in arm_runs])
        lines.append(
            f"{arm:<14}{'0% (actual)':>11}{tax:>10,.0f}"
            f"{stats.median([r['steps'] for r in arm_runs]):>11.1f}"
            f"{0:>10,.0f}{actual_med:>14,.0f}{(actual_med / base_med - 1) * 100:>12.0f}%"
        )
        for frac in [float(x) for x in args.reductions.split(",")]:
            projected = []
            for r in arm_runs:
                saved = tax * frac * max(1, int(r["steps"]))
                projected.append(max(0.0, billed(r) - saved))
            med = stats.median(projected)
            lines.append(
                f"{'':<14}{frac * 100:>10.0f}%{tax * (1 - frac):>10,.0f}"
                f"{stats.median([r['steps'] for r in arm_runs]):>11.1f}"
                f"{tax * frac * stats.median([r['steps'] for r in arm_runs]):>10,.0f}"
                f"{med:>14,.0f}{(med / base_med - 1) * 100:>12.0f}%"
            )
        lines.append("")

    lines += [
        "Assumption: the schema payload is re-sent on every request, so the saving scales",
        "with step count. Reduction fractions come from schema_audit.py's simulation of",
        "information-preserving edits (A = 47%, D = 72%).",
    ]

    report = "\n".join(lines)
    print(report)
    if args.out:
        args.out.write_text(report, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
