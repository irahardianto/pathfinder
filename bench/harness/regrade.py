#!/usr/bin/env python3
"""Re-apply the grader to already-completed runs.

Needed when a ground truth is corrected after the matrix has executed. Answer
text is stored in runs.jsonl, so navigation tasks can be re-scored without
spending another token. Mutation tasks are skipped: their score depends on the
state of a worktree that no longer exists, so they must be re-run instead.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from grader import grade_regex_all, grade_set_of_lines

BENCH_DIR = Path(__file__).resolve().parent.parent


class _Shim:
    """Minimal stand-in exposing the attributes the grader writes to."""

    def __init__(self, record: dict[str, Any]) -> None:
        self.answer = record.get("answer")
        self.final_text = record.get("final_text")
        self.score = record.get("score", 0.0)
        self.passed = record.get("passed", False)
        self.grade_detail = record.get("grade_detail", {})


def load_tasks(suites: list[str]) -> dict[str, dict[str, Any]]:
    tasks: dict[str, dict[str, Any]] = {}
    for name in suites:
        spec = json.loads((BENCH_DIR / "tasks" / f"{name}.json").read_text(encoding="utf-8"))
        for t in spec["tasks"]:
            tasks[t["id"]] = t
    return tasks


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True, type=Path)
    ap.add_argument("--suites", default="nav,edit")
    args = ap.parse_args()

    tasks = load_tasks([s.strip() for s in args.suites.split(",") if s.strip()])
    runs_path = args.results / "runs.jsonl"
    raw_lines = runs_path.read_text(encoding="utf-8").splitlines()
    records = [json.loads(line) for line in raw_lines if line.strip()]

    changed = 0
    for rec in records:
        task = tasks.get(rec["task_id"])
        if task is None:
            continue
        spec = task["grade"]
        if spec["type"] == "shell_gate":
            continue  # worktree state is gone; only a re-run can score this
        shim = _Shim(rec)
        if shim.answer is None:
            continue
        if spec["type"] == "set_of_lines":
            score, detail = grade_set_of_lines(shim.answer, spec)
        elif spec["type"] == "regex_all":
            score, detail = grade_regex_all(shim.answer, spec)
        else:
            continue
        if abs(score - rec["score"]) > 1e-9:
            changed += 1
            print(f"  {rec['run_id']:<44} {rec['score']:.2f} -> {score:.2f}")
        rec["score"] = score
        rec["passed"] = score >= 0.999
        rec["grade_detail"] = detail

    runs_path.write_text(
        "\n".join(json.dumps(r) for r in records) + "\n", encoding="utf-8"
    )
    print(f"regraded {len(records)} records, {changed} score changes -> {runs_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
