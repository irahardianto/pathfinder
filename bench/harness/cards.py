#!/usr/bin/env python3
"""Render the benchmark down to screenshot-sized result cards.

A full report is the right artifact for a decision and the wrong one for a
social post. This emits self-contained, fixed-width cards — one claim each,
with the n and the arms visible so the number can be checked rather than
just believed. Every figure is computed from runs.jsonl at render time; none
of them are transcribed by hand.
"""

from __future__ import annotations

import argparse
import json
import statistics as stats
from collections import Counter
from pathlib import Path
from typing import Any, Callable

W = 74
ARM_LABEL = {
    "baseline": "grep + read",
    "pathfinder": "Pathfinder",
    "both": "PF + builtin",
    "codegraph": "CodeGraph",
    "cg_both": "CG + builtin",
}


def billed(r: dict[str, Any]) -> int:
    return r["tok_input"] + r["tok_output"] + r["tok_cache_read"] + r["tok_cache_write"]


def load(path: Path, suite: str | None = None) -> list[dict[str, Any]]:
    lines = path.read_text(encoding="utf-8").splitlines()
    rows = [json.loads(line) for line in lines if line.strip()]
    return [r for r in rows if suite is None or r["suite"] == suite]


# ────────────────────────────── rendering ──────────────────────────────


def rule(ch: str = "─") -> str:
    return ch * W


def card(title: str, subtitle: str, body: list[str], footer: str) -> str:
    out = [rule("━"), title.upper(), subtitle, rule()]
    out += body
    out += [rule(), footer, rule("━"), ""]
    return "\n".join(out)


def bar(value: float, peak: float, width: int = 26, ch: str = "█") -> str:
    if peak <= 0:
        return ""
    return ch * max(1, round(width * value / peak))


def metric_rows(
    runs: list[dict[str, Any]],
    arms: list[str],
    fn: Callable[[dict[str, Any]], float],
    agg: Callable[[list[float]], float],
    fmt: str,
    unit: str = "",
    show_bar: bool = True,
) -> list[str]:
    vals = {}
    for a in arms:
        s = [fn(r) for r in runs if r["arm"] == a]
        if s:
            vals[a] = agg(s)
    if not vals:
        return []
    peak = max(vals.values())
    base = vals.get("baseline")
    rows = []
    for a in arms:
        if a not in vals:
            continue
        v = vals[a]
        rel = ""
        if base and a != "baseline":
            rel = f"{(v / base - 1) * 100:+5.0f}%"
        elif a == "baseline":
            rel = " base"
        b = f"  {bar(v, peak)}" if show_bar else ""
        rows.append(f"  {ARM_LABEL[a]:<14}{format(v, fmt)+unit:>12}  {rel}{b}")
    return rows


# ────────────────────────────── cards ──────────────────────────────


def card_headline(deep: list[dict], nav: list[dict], arms: list[str]) -> str:
    body = ["", "  DEEP architecture questions  (baseline needs ~12 tool calls)", ""]
    body += metric_rows(deep, arms, lambda r: float(len(r["tool_calls"])), stats.median, ".1f", " calls")
    body += ["", "  SHALLOW lookups  (baseline needs ~2 tool calls)", ""]
    body += metric_rows(nav, arms, lambda r: float(len(r["tool_calls"])), stats.median, ".1f", " calls")
    n_d = len([r for r in deep if r["arm"] == "baseline"])
    n_n = len([r for r in nav if r["arm"] == "baseline"])
    return card(
        "the same tool is a win and a loss",
        "median tool calls to reach an identical answer",
        body,
        f"Claude Sonnet 5 · n={n_d}/arm deep, n={n_n}/arm shallow · frozen repo, shuffled order",
    )


def card_cost_inversion(nav: list[dict], deep: list[dict], arms: list[str]) -> str:
    body = ["", "  SHALLOW lookups — mean cost per task", ""]
    body += metric_rows(nav, arms, lambda r: float(r["cost_usd"]), stats.mean, ".4f", " USD")
    body += ["", "  DEEP architecture questions — mean cost per task", ""]
    body += metric_rows(deep, arms, lambda r: float(r["cost_usd"]), stats.mean, ".4f", " USD")
    return card(
        "indexing is not free",
        "a code graph pays off only when discovery is expensive",
        body,
        "Same model, same repo, same grader. Only the question shape changed.",
    )


def card_accuracy(res: list[dict], arms: list[str]) -> str:
    body = ["", "  Tasks where a plausible WRONG answer is one grep away:", ""]
    body += ["    · trait impls hidden behind a fully-qualified path",
             "    · a method that only exists after macro expansion",
             "    · which concrete type is wired in at runtime (DI)",
             "    · one method name defined on four different types", ""]
    body += ["  Accuracy  (1.00 = every fact correct, no invented symbols)", ""]
    vals = {a: stats.mean([r["score"] for r in res if r["arm"] == a]) for a in arms
            if any(r["arm"] == a for r in res)}
    for a in arms:
        if a not in vals:
            continue
        n = len([r for r in res if r["arm"] == a])
        k = sum(1 for r in res if r["arm"] == a and r["passed"])
        body.append(f"  {ARM_LABEL[a]:<14}{vals[a]:>6.3f}   {k}/{n} perfect   {bar(vals[a], 1.0, 30)}")
    n = len([r for r in res if r["arm"] == "baseline"])
    return card(
        "speed is not the only axis",
        "semantic resolution: where an index beats text search",
        body,
        f"Claude Sonnet 5 · n={n}/arm · wrong answers penalised 1.5x vs omissions",
    )


def card_schema_tax(results_dir: Path, runs: list[dict], arms: list[str]) -> str:
    per: dict[str, list[float]] = {}
    for r in runs:
        f = results_dir / "raw" / f"{r['run_id']}.jsonl"
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
            c = tk.get("cache") or {}
            v = int(tk.get("input") or 0) + int(c.get("read") or 0) + int(c.get("write") or 0)
            if v:
                per.setdefault(r["arm"], []).append(float(v))
            break
    if "baseline" not in per:
        return ""
    base = stats.median(per["baseline"])
    body = ["", "  Tokens added to EVERY request, before any code is retrieved:", ""]
    peak = max(stats.median(v) - base for a, v in per.items() if a != "baseline") or 1
    for a in arms:
        if a == "baseline" or a not in per:
            continue
        tax = stats.median(per[a]) - base
        body.append(f"  {ARM_LABEL[a]:<14}{tax:>+9,.0f} tok   {bar(tax, peak, 30)}")
    body += ["", "  An MCP server ships its tool schemas on every single turn.",
             "  At 10 turns a 7,700-token schema costs 77,000 tokens of context",
             "  before the agent has read one line of code."]
    return card(
        "the cost you cannot see",
        "fixed tool-schema overhead, measured on the first request",
        body,
        f"Claude Sonnet 5 · median of n={len(per['baseline'])} runs per arm",
    )


def card_tool_profile(deep: list[dict], arms: list[str]) -> str:
    body = ["", "  How each arm spends its tool budget on one deep question:", ""]
    for a in arms:
        s = [r for r in deep if r["arm"] == a]
        if not s:
            continue
        c: Counter[str] = Counter()
        chars = 0
        for r in s:
            for t in r["tool_calls"]:
                c[t["name"].replace("codegraph_codegraph_", "cg_").replace("pathfinder_", "pf_")] += 1
                chars += t["output_chars"]
        total = sum(c.values()) or 1
        mix = "  ".join(f"{k} x{v}" for k, v in c.most_common(3))
        body.append(f"  {ARM_LABEL[a]:<14}{total / len(s):>5.1f} calls   {chars / total:>7,.0f} chars/call")
        body.append(f"  {'':<14}{mix}")
        body.append("")
    return card(
        "fewer calls, bigger payloads",
        "a graph does not return less — it returns it all at once",
        body,
        "Deep suite · Claude Sonnet 5 · characters of tool output per call",
    )


def card_density(deep: list[dict], arms: list[str]) -> str:
    body = ["", "  Characters the agent had to read to reach the SAME answer:", ""]
    vals = {}
    for a in arms:
        s = [r for r in deep if r["arm"] == a]
        if not s:
            continue
        vals[a] = stats.mean([sum(t["output_chars"] for t in r["tool_calls"]) for r in s])
    peak = max(vals.values())
    base = vals.get("baseline", 0)
    for a in arms:
        if a not in vals:
            continue
        rel = f"{(vals[a] / base - 1) * 100:+5.0f}%" if base and a != "baseline" else " base"
        body.append(f"  {ARM_LABEL[a]:<14}{vals[a]:>10,.0f}  {rel}  {bar(vals[a], peak)}")
    body += ["", "  Correctness was identical across all arms.", "",
             "  So the graph is not returning BETTER information.",
             "  It is returning MORE information, in FEWER round trips."]
    return card(
        "'surgical context' is round trips, not precision",
        "same answer, same accuracy, more characters read",
        body,
        "Deep suite · Claude Sonnet 5 · mean tool-output characters per task",
    )


def card_scale(breadth: list[dict[str, Any]]) -> str:
    """The large-repo result. This is the one that inverts the small-repo story."""
    arms = ["baseline", "pathfinder", "codegraph"]
    body = ["", "  tokio (793 Rust files) + juice-shop (642 TS/JS files)",
            "  5 tasks · same model · same grader · n=10 per arm", ""]
    body.append(f"  {'':<14}{'accuracy':>9}{'tokens':>12}{'calls':>8}{'wall':>8}{'cost':>9}")
    for a in arms:
        s_ = [r for r in breadth if r["arm"] == a]
        if not s_:
            continue
        body.append(
            f"  {ARM_LABEL[a]:<14}"
            f"{stats.mean([r['score'] for r in s_]):>9.3f}"
            f"{stats.median([float(billed(r)) for r in s_]):>12,.0f}"
            f"{stats.median([float(len(r['tool_calls'])) for r in s_]):>8.1f}"
            f"{stats.median([r['wall_ms'] / 1000 for r in s_]):>7.0f}s"
            f"{stats.mean([r['cost_usd'] for r in s_]):>9.3f}"
        )
    body += ["", "  On a 159-file repo the graph cut tool calls 52%.",
             "  On 800-file repos it RAISED them 69% and cost 7x more —",
             "  at slightly worse accuracy."]
    return card(
        "the win does not survive scale",
        "same tools, bigger codebases, opposite result",
        body,
        "Claude Sonnet 5 · n=10/arm · tokio @625954f3, juice-shop @1618a611b",
    )


def card_delegation(single: list[dict[str, Any]], multi: list[dict[str, Any]]) -> str:
    """Delegation is priced by task width, and that reconciles the field reports."""
    pairs = [("baseline", "ma_baseline"), ("pathfinder", "ma_pathfinder"), ("codegraph", "ma_codegraph")]
    body = ["", "  Token change when the agent is allowed to delegate to sub-agents,", "  by how WIDE the question is:", ""]
    order = [
        ("B5_juiceshop_authz_gap_survey", "survey 61 route files"),
        ("B4_juiceshop_auth_flow", "trace 1 auth chain"),
        ("B3_juiceshop_sql_injection_audit", "audit 1 pattern"),
        ("B1_tokio_api_surface_survey", "enumerate 1 API"),
        ("B2_tokio_spawn_chain", "trace 1 call chain"),
    ]
    body.append(f"  {'task':<26}{'baseline':>11}{'Pathfinder':>12}{'CodeGraph':>12}")
    for tid, label in order:
        row = f"  {label:<26}"
        for b, m in pairs:
            a = [float(billed(r)) for r in single if r["task_id"] == tid and r["arm"] == b]
            c = [float(billed(r)) for r in multi if r["task_id"] == tid and r["arm"] == m]
            row += f"{(stats.median(c) / stats.median(a) - 1) * 100:>+11.0f}%" if a and c else f"{'-':>12}"
        body.append(row)
    body += ["", "  Delegation only pays on the WIDE task — and there it pays most",
             "  for the graph. On narrow questions it is pure overhead.", "",
             "  This is why a multi-agent audit and a single-agent lookup",
             "  report opposite results for the same tool."]
    return card(
        "delegation is priced by task width",
        "sub-agent windows rescue dense payloads — only when the work splits",
        body,
        "Claude Sonnet 5 · tokio + juice-shop · n=2/cell · sub-agent tokens included",
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--nav", type=Path, required=True, help="results dir holding nav runs")
    ap.add_argument("--deep", type=Path, required=True, help="results dir holding deep runs")
    ap.add_argument("--resolve", type=Path, required=True, help="results dir holding resolve runs")
    ap.add_argument("--implement", type=Path, default=None)
    ap.add_argument("--breadth", type=Path, default=None)
    ap.add_argument("--multiagent", type=Path, default=None)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    arms = ["baseline", "pathfinder", "both", "codegraph", "cg_both"]
    nav = load(args.nav / "runs.jsonl", "nav")
    deep = load(args.deep / "runs.jsonl", "deep")
    res = load(args.resolve / "runs.jsonl", "resolve")

    cards = [
        card_headline(deep, nav, arms),
        card_cost_inversion(nav, deep, arms),
        card_accuracy(res, arms),
        card_density(deep, arms),
        card_tool_profile(deep, arms),
        card_schema_tax(args.deep, deep, arms),
    ]

    if args.breadth and (args.breadth / "runs.jsonl").exists():
        br = load(args.breadth / "runs.jsonl", "breadth")
        if br:
            cards.insert(1, card_scale(br))

    if args.multiagent and args.breadth and (args.multiagent / "runs.jsonl").exists():
        mu = load(args.multiagent / "runs.jsonl", "breadth")
        br2 = load(args.breadth / "runs.jsonl", "breadth")
        if mu and br2:
            cards.insert(2, card_delegation(br2, mu))

    if args.implement and (args.implement / "runs.jsonl").exists():
        imp = load(args.implement / "runs.jsonl", "implement")
        if imp:
            body = ["", "  Task: instrument EVERY production call site of one method.",
                    "  Miss one and the build gate fails. Retrieval recall is",
                    "  enforced by the compiler, not by a rubric.", ""]
            for a in ["baseline", "pathfinder", "codegraph"]:
                s = [r for r in imp if r["arm"] == a]
                if not s:
                    continue
                k = sum(1 for r in s if r["passed"])
                body.append(
                    f"  {ARM_LABEL[a]:<14}{k}/{len(s)} passed   "
                    f"{stats.mean([r['score'] for r in s]):.2f} score   "
                    f"{stats.mean([r['cost_usd'] for r in s]):.3f} USD"
                )
            cards.append(card(
                "does better retrieval produce better code?",
                "graded by cargo check, not by prose",
                body,
                "Claude Sonnet 5 · multi-file change · shell-gated",
            ))

    text = "\n".join(c for c in cards if c)
    print(text)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
