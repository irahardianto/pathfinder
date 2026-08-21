#!/usr/bin/env python3
"""Execute every shell gate against a pristine worktree and report the outcome.

A gate has two failure modes and only one of them is obvious:

  - It never passes. An earlier revision piped out of `rg -q`, which emits
    nothing, so the gate was unsatisfiable by construction. Six runs failed it
    identically before anyone noticed.
  - It always passes. Then it is not testing the change at all and every run
    gets free credit.

Static inspection missed the first one (a naive scan for `|` cannot tell a
shell pipe from regex alternation or `||`), so this executes them instead.

On a pristine tree every gate that asserts the change was made MUST fail.
A gate that passes here is flagged: either it is vacuous, or the worktree is
dirty. Gates that are inherently satisfied on a clean tree — "no test files
were edited", "the crate compiles" — are declared in ALWAYS_TRUE_ON_CLEAN and
inverted, so they are checked to PASS instead.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent.parent

# Gates that describe a property of an untouched tree rather than a property
# of the change. These must PASS on clean; failing here means the fixture is
# broken, not the agent.
ALWAYS_TRUE_ON_CLEAN = {
    "compiles",
    "no_test_files_edited",
    "no_collateral_damage",
    "no_unwrap_introduced",
}


def run_gate(gate: dict, repo: Path, target: Path | None) -> tuple[int, str]:
    env = None
    if target:
        import os

        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(target)
    proc = subprocess.run(
        gate["cmd"],
        shell=True,
        cwd=str(repo),
        capture_output=True,
        text=True,
        timeout=int(gate.get("timeout", 900)),
        env=env,
    )
    return proc.returncode, (proc.stdout or proc.stderr or "")[-200:]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True, type=Path)
    ap.add_argument("--target", type=Path, default=None)
    ap.add_argument("--suites", default="edit,implement")
    args = ap.parse_args()

    repo = args.repo.resolve()
    dirty = subprocess.run(
        ["git", "-C", str(repo), "status", "--porcelain", "--", "crates"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if dirty:
        print(f"worktree is dirty, results meaningless:\n{dirty}", file=sys.stderr)
        return 2

    problems = 0
    for suite in args.suites.split(","):
        spec_path = BENCH_DIR / "tasks" / f"{suite.strip()}.json"
        if not spec_path.exists():
            continue
        spec = json.loads(spec_path.read_text(encoding="utf-8"))
        for task in spec["tasks"]:
            grade = task.get("grade", {})
            if grade.get("type") != "shell_gate":
                continue
            print(f"\n{task['id']}")
            for gate in grade["gates"]:
                name = gate["name"]
                expect_exit = int(gate.get("expect_exit", 0))
                rc, tail = run_gate(gate, repo, args.target)
                passed = rc == expect_exit
                inherent = name in ALWAYS_TRUE_ON_CLEAN
                ok = passed if inherent else not passed
                status = "ok" if ok else "PROBLEM"
                want = "must pass on clean" if inherent else "must fail on clean"
                print(f"  {status:<8}{name:<32}{want:<22}got={'pass' if passed else 'fail'}")
                if not ok:
                    problems += 1
                    if tail.strip():
                        print(f"           {tail.strip()[:160]}")

    print()
    if problems:
        print(f"{problems} gate(s) behave wrongly on a pristine tree")
        return 1
    print("all gates discriminate correctly on a pristine tree")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
