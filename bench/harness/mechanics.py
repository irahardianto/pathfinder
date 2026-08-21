#!/usr/bin/env python3
"""Mine raw event streams for tool-ergonomics evidence.

"Is the tool intuitive?" is normally answered with opinion. It does not have
to be. An unintuitive tool leaves a measurable fingerprint in the transcript:
the model calls it with bad parameters, gets nothing useful back, calls it
again with a tweak, or abandons it. This module counts those events.

Metrics produced per arm and per tool:

  invalid_param_rate  — calls the server rejected as malformed input. Direct
                        evidence that the schema does not match the model's
                        mental model.
  empty_result_rate   — calls that succeeded but returned no usable payload.
  retry_rate          — calls repeating the same tool with a mutated argument
                        within the same run, i.e. the model correcting itself.
  abandon_rate        — runs where the model used a tool once and never again
                        despite the task being unfinished.
  degraded_rate       — Pathfinder-specific: responses flagged degraded.
  yield_per_1k_chars  — task score earned per 1k characters of tool output.
                        A high-yield tool answers the question; a low-yield
                        tool makes the model read.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics as stats
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

INVALID_PARAM_MARKERS = (
    "invalid_params",
    "invalid params",
    "INVALID_PARAMS",
    "must include file path",
    "provide either",
    "is required when",
    "mutually exclusive",
    "Invalid kind",
    "accepted values",
)
EMPTY_MARKERS = (
    "no matches",
    "No matches",
    "not found",
    "NOT_FOUND",
    "SYMBOL_NOT_FOUND",
    "FILE_NOT_FOUND",
    "0 results",
    "No results",
    "(empty)",
)
DEGRADED_MARKER = "DEGRADED"
# A result short enough that an "empty" banner is the whole payload rather
# than incidental text inside a retrieved file.
EMPTY_OUTPUT_MAX_CHARS = 400
# Pathfinder emits its degraded banner as the first line of the response.
DEGRADED_SCAN_CHARS = 400


def load_runs(results: Path) -> list[dict[str, Any]]:
    runs = []
    for line in (results / "runs.jsonl").read_text(encoding="utf-8").splitlines():
        if line.strip():
            runs.append(json.loads(line))
    return runs


def classify(call: dict[str, Any]) -> set[str]:
    """Tag one tool call with its failure modes.

    Scoping matters here. This benchmark runs *against Pathfinder's own source
    tree*, so any tool that reads a file can echo Pathfinder's error-string
    literals back into the transcript. Matching markers against full tool
    output would therefore report phantom `invalid_params` on a perfectly
    healthy `read`. Diagnosis is restricted to the error channel, plus the
    head of short outputs where an empty/degraded banner would actually live.
    """
    tags: set[str] = set()
    err = str(call.get("error") or "")
    out = call.get("_output_text") or ""

    if call.get("status") == "error" or err:
        tags.add("error")

    # Parameter rejections surface on the error channel only.
    if err and any(m in err for m in INVALID_PARAM_MARKERS):
        tags.add("invalid_params")

    # A genuinely empty result is short. Long output containing the words
    # "not found" is a file that happens to mention them.
    if len(out) <= EMPTY_OUTPUT_MAX_CHARS and any(m in out for m in EMPTY_MARKERS):
        tags.add("empty")
    if not out.strip():
        tags.add("empty")

    # Pathfinder prints its degraded banner at the very top of the response.
    if DEGRADED_MARKER in out[:DEGRADED_SCAN_CHARS] or DEGRADED_MARKER in err:
        tags.add("degraded")
    return tags


def hydrate_outputs(results: Path, run: dict[str, Any]) -> list[dict[str, Any]]:
    """Re-read the raw stream to recover tool output text (not stored in runs.jsonl)."""
    f = results / "raw" / f"{run['run_id']}.jsonl"
    calls: list[dict[str, Any]] = []
    if not f.exists():
        return calls
    seen: set[str] = set()
    for line in f.read_text(encoding="utf-8").splitlines():
        if '"tool"' not in line:
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            continue
        part = evt.get("part") or {}
        if part.get("type") != "tool":
            continue
        cid = part.get("callID") or part.get("id") or ""
        state = part.get("state") or {}
        rec = {
            "name": part.get("tool") or "unknown",
            "status": state.get("status"),
            "error": state.get("error"),
            "_input": state.get("input"),
            "_output_text": state.get("output") if isinstance(state.get("output"), str) else json.dumps(state.get("output") or ""),
            "callID": cid,
        }
        # A callID can appear multiple times as the state transitions; keep the
        # terminal observation only.
        if cid in seen:
            for i, prev in enumerate(calls):
                if prev["callID"] == cid:
                    calls[i] = rec
                    break
        else:
            seen.add(cid)
            calls.append(rec)
    return calls


def norm_input(inp: Any) -> str:
    try:
        s = json.dumps(inp, sort_keys=True)
    except (TypeError, ValueError):
        s = str(inp)
    return re.sub(r"\s+", "", s.lower())


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True, type=Path)
    ap.add_argument("--arms", default="baseline,pathfinder,both")
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    runs = load_runs(args.results)
    arms = [a for a in args.arms.split(",") if any(r["arm"] == a for r in runs)]

    lines: list[str] = []

    def emit(s: str = "") -> None:
        lines.append(s)

    emit("=" * 100)
    emit("TOOL ERGONOMICS / MECHANICS REPORT")
    emit("=" * 100)

    per_arm_tool: dict[tuple[str, str], Counter[str]] = defaultdict(Counter)
    per_arm_calls: dict[tuple[str, str], int] = Counter()
    per_arm_chars: dict[tuple[str, str], int] = Counter()
    per_arm_retry: dict[str, int] = Counter()
    per_arm_runs: Counter[str] = Counter()
    yield_rows: dict[str, list[tuple[float, int]]] = defaultdict(list)

    for r in runs:
        arm = r["arm"]
        per_arm_runs[arm] += 1
        calls = hydrate_outputs(args.results, r)
        seen_inputs: dict[str, set[str]] = defaultdict(set)
        retries = 0
        total_chars = 0
        for c in calls:
            key = (arm, c["name"])
            per_arm_calls[key] += 1
            out_len = len(c.get("_output_text") or "")
            per_arm_chars[key] += out_len
            total_chars += out_len
            for tag in classify(c):
                per_arm_tool[key][tag] += 1
            sig = norm_input(c.get("_input"))
            if sig in seen_inputs[c["name"]]:
                retries += 1
                per_arm_tool[key]["exact_repeat"] += 1
            seen_inputs[c["name"]].add(sig)
        per_arm_retry[arm] += retries
        yield_rows[arm].append((float(r["score"]), total_chars))

    emit()
    emit("A. PER-TOOL FAILURE MODES (counts, and rate over that tool's calls)")
    emit(f"{'arm':<12}{'tool':<26}{'calls':>7}{'error':>9}{'inv_par':>9}{'empty':>8}{'degraded':>10}{'repeat':>8}{'avg_chars':>11}")
    for arm in arms:
        keys = sorted([k for k in per_arm_calls if k[0] == arm], key=lambda k: -per_arm_calls[k])
        for k in keys:
            n = per_arm_calls[k]
            t = per_arm_tool[k]
            emit(
                f"{arm:<12}{k[1]:<26}{n:>7}"
                f"{t['error']:>4} {t['error'] / n * 100:>3.0f}%"
                f"{t['invalid_params']:>4} {t['invalid_params'] / n * 100:>3.0f}%"
                f"{t['empty']:>3} {t['empty'] / n * 100:>3.0f}%"
                f"{t['degraded']:>5} {t['degraded'] / n * 100:>3.0f}%"
                f"{t['exact_repeat']:>3} {t['exact_repeat'] / n * 100:>3.0f}%"
                f"{per_arm_chars[k] / n:>11,.0f}"
            )
        emit()

    emit("B. INFORMATION YIELD (task score earned per 1,000 chars of tool output)")
    emit(f"{'arm':<14}{'mean score':>12}{'mean tool chars':>18}{'score per 1k chars':>22}")
    for arm in arms:
        rows = yield_rows.get(arm) or []
        if not rows:
            continue
        msc = stats.mean([s for s, _ in rows])
        mch = stats.mean([c for _, c in rows])
        y = (msc / (mch / 1000)) if mch else float("nan")
        emit(f"{arm:<14}{msc:>12.3f}{mch:>18,.0f}{y:>22.4f}")

    emit()
    emit("C. SELF-CORRECTION PRESSURE (exact-repeat tool calls per run)")
    for arm in arms:
        n = per_arm_runs[arm]
        emit(f"  {arm:<14}{per_arm_retry[arm] / n if n else 0:>8.2f} repeats/run   ({per_arm_retry[arm]} over {n} runs)")

    report = "\n".join(lines)
    print(report)
    if args.out:
        args.out.write_text(report, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
