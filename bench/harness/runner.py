#!/usr/bin/env python3
"""Pathfinder A/B benchmark runner.

Executes a matrix of (task x arm x repetition) agent runs through the opencode
CLI in headless JSON-event mode, captures per-run token accounting and tool
traces, and writes one JSON record per run to the results directory.

Design invariants (do not weaken these without updating bench/README.md):

1.  Every arm sees an identical task prompt, identical model, identical
    temperature and an identical agent system prompt. The ONLY variable is the
    toolset exposed to the model.
2.  Runs execute against a frozen git worktree snapshot, not the live repo.
3.  Mutation runs reset the snapshot to pristine before AND after execution.
4.  Execution order across the whole matrix is shuffled with a fixed seed so
    provider-side caching and drift cannot systematically favour one arm.
5.  Nothing is inferred from the model's prose. Grading is mechanical.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Any

from subagent_usage import root_session_from_stream, subtree_usage

BENCH_DIR = Path(__file__).resolve().parent.parent
REPO_ROOT = BENCH_DIR.parent

ARMS = {
    "baseline": BENCH_DIR / "configs" / "arm_baseline.json",
    "pathfinder": BENCH_DIR / "configs" / "arm_pathfinder.json",
    "both": BENCH_DIR / "configs" / "arm_both.json",
    "codegraph": BENCH_DIR / "configs" / "arm_codegraph.json",
    "cg_both": BENCH_DIR / "configs" / "arm_cg_both.json",
    # Multi-agent arms: same toolsets, but the orchestrator may delegate to
    # short-lived sub-agents with their own context windows.
    "ma_baseline": BENCH_DIR / "configs" / "arm_ma_baseline.json",
    "ma_pathfinder": BENCH_DIR / "configs" / "arm_ma_pathfinder.json",
    "ma_codegraph": BENCH_DIR / "configs" / "arm_ma_codegraph.json",
}


# ────────────────────────────── data model ──────────────────────────────


@dataclass
class ToolCall:
    """A single tool invocation observed in the event stream."""

    name: str
    input_chars: int
    output_chars: int
    duration_ms: int
    status: str
    error: str | None = None


@dataclass
class RunRecord:
    """Everything measurable about one agent run."""

    run_id: str
    suite: str
    task_id: str
    arm: str
    rep: int
    model: str
    started_at: float
    repo: str = ""
    wall_ms: int = 0
    exit_code: int = -1
    timed_out: bool = False

    # Token accounting, summed across every assistant step in the run.
    tok_input: int = 0
    tok_output: int = 0
    tok_reasoning: int = 0
    tok_cache_read: int = 0
    tok_cache_write: int = 0
    cost_usd: float = 0.0
    steps: int = 0

    # Tool behaviour.
    tool_calls: list[ToolCall] = field(default_factory=list)
    tool_errors: int = 0

    # Outcome.
    answer: str | None = None
    final_text: str | None = None
    score: float = 0.0
    passed: bool = False
    grade_detail: dict[str, Any] = field(default_factory=dict)

    # Sub-agent accounting. The event stream only covers the orchestrator
    # session, so a delegating arm would look artificially cheap without these.
    subagent_sessions: int = 0
    sub_tok_input: int = 0
    sub_tok_output: int = 0
    sub_tok_cache_read: int = 0
    sub_tok_cache_write: int = 0
    sub_cost_usd: float = 0.0
    root_session: str | None = None

    # Diagnostics.
    stream_error: str | None = None


# ────────────────────────────── snapshot mgmt ──────────────────────────────


def reset_snapshot(repo: Path) -> None:
    """Return the frozen worktree to a pristine state.

    Mutation tasks edit files in place. Without this the second repetition
    would start from the first repetition's output, which would silently
    invalidate every downstream measurement.
    """
    subprocess.run(
        ["git", "-C", str(repo), "checkout", "--", "."],
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["git", "-C", str(repo), "clean", "-fdq", "-e", "target", "-e", ".codegraph"],
        check=True,
        capture_output=True,
    )
    # AGENTS.md is deliberately absent from the benchmark snapshot: the live
    # repo's AGENTS.md contains a Pathfinder tool-routing section that would
    # bias the "both" arm and confuse the baseline arm with references to
    # tools it cannot call. Removing it isolates the independent variable to
    # the MCP server's own self-description.
    for name in ("AGENTS.md", "CLAUDE.md", "GEMINI.md", ".cursorrules"):
        f = repo / name
        if f.exists():
            f.unlink()


def snapshot_dirty(repo: Path) -> bool:
    out = subprocess.run(
        ["git", "-C", str(repo), "status", "--porcelain"],
        capture_output=True,
        text=True,
        check=True,
    )
    return bool(out.stdout.strip())


# ────────────────────────────── event parsing ──────────────────────────────


def _chars(value: Any) -> int:
    if value is None:
        return 0
    if isinstance(value, str):
        return len(value)
    try:
        return len(json.dumps(value, separators=(",", ":")))
    except (TypeError, ValueError):
        return len(str(value))


def parse_stream(raw: str, rec: RunRecord) -> None:
    """Fold the opencode JSON event stream into a RunRecord.

    The stream is newline-delimited JSON. Unparseable lines are counted rather
    than raised: a truncated stream still carries usable partial measurements,
    and silently dropping them would understate cost.
    """
    bad_lines = 0
    text_parts: list[str] = []
    tool_parts: dict[str, dict[str, Any]] = {}

    for line in raw.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            bad_lines += 1
            continue

        etype = evt.get("type")

        if etype == "error":
            err = evt.get("error", {})
            msg = err.get("data", {}).get("message") or err.get("name") or "unknown"
            rec.stream_error = str(msg)[:600]
            continue

        part = evt.get("part") or {}

        if etype == "step_finish":
            rec.steps += 1
            tokens = part.get("tokens") or {}
            cache = tokens.get("cache") or {}
            rec.tok_input += int(tokens.get("input") or 0)
            rec.tok_output += int(tokens.get("output") or 0)
            rec.tok_reasoning += int(tokens.get("reasoning") or 0)
            rec.tok_cache_read += int(cache.get("read") or 0)
            rec.tok_cache_write += int(cache.get("write") or 0)
            rec.cost_usd += float(evt.get("part", {}).get("cost") or 0.0)
            continue

        if etype == "text":
            txt = part.get("text")
            if txt:
                text_parts.append(txt)
            continue

        # Tool invocations arrive as {"type":"tool_use","part":{"type":"tool",...}}.
        # Keyed by callID so repeated status updates for one call collapse into
        # a single record rather than inflating the call count.
        if part.get("type") == "tool":
            pid = part.get("callID") or part.get("id") or str(uuid.uuid4())
            state = part.get("state") or {}
            tool_parts[pid] = {
                "name": part.get("tool") or state.get("title") or "unknown",
                "state": state,
            }
            continue

    for info in tool_parts.values():
        state = info["state"]
        status = state.get("status", "unknown")
        tstamp = state.get("time") or {}
        start = tstamp.get("start") or 0
        end = tstamp.get("end") or 0
        duration = max(0, int(end) - int(start)) if start and end else 0
        err = state.get("error")
        if err:
            rec.tool_errors += 1
        rec.tool_calls.append(
            ToolCall(
                name=str(info["name"]),
                input_chars=_chars(state.get("input")),
                output_chars=_chars(state.get("output")),
                duration_ms=duration,
                status=str(status),
                error=str(err)[:300] if err else None,
            )
        )

    rec.final_text = "\n".join(text_parts) if text_parts else None
    if bad_lines and rec.stream_error is None:
        rec.stream_error = f"{bad_lines} unparseable stream lines"


# ────────────────────────────── execution ──────────────────────────────


def run_once(
    *,
    suite: str,
    agent: str,
    task: dict[str, Any],
    arm: str,
    rep: int,
    model: str,
    repo: Path,
    mcp_bin: Path,
    timeout_s: int,
    results_dir: Path,
) -> RunRecord:
    rec = RunRecord(
        run_id=f"{task['id']}__{arm}__r{rep}",
        suite=suite,
        task_id=task["id"],
        arm=arm,
        rep=rep,
        model=model,
        started_at=time.time(),
        repo=repo.name if repo.name != "repo" else repo.parent.name,
    )

    reset_snapshot(repo)

    env = os.environ.copy()
    env["OPENCODE_CONFIG"] = str(ARMS[arm])
    env["PF_BENCH_REPO"] = str(repo)
    env["PF_BENCH_MCP_BIN"] = str(mcp_bin)
    env["CARGO_TARGET_DIR"] = str(repo.parent / "target")
    # Keep the agent out of any inherited opencode session state.
    env.pop("OPENCODE", None)
    env.pop("OPENCODE_PID", None)

    cmd = [
        "opencode",
        "run",
        "--pure",
        "--format",
        "json",
        "--model",
        model,
        "--agent",
        agent,
        "--auto",
        "--dir",
        str(repo),
        task["prompt"],
    ]

    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(repo),
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout_s,
        )
        raw, err_raw, rec.exit_code = proc.stdout, proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as exc:
        rec.timed_out = True
        raw = exc.stdout.decode("utf-8", "replace") if exc.stdout else ""
        err_raw = exc.stderr.decode("utf-8", "replace") if exc.stderr else ""
        rec.exit_code = -9
    rec.wall_ms = int((time.perf_counter() - t0) * 1000)

    parse_stream(raw, rec)

    # Fold in every descendant session. When an arm delegates, the orchestrator
    # stream shows only a fraction of the real spend — measured at ~half on a
    # single-subagent probe — so the totals below REPLACE the stream figures
    # rather than sitting beside them.
    root = root_session_from_stream(raw)
    if root:
        rec.root_session = root
        usage = subtree_usage(root)
        if usage.sessions > 1:
            rec.subagent_sessions = usage.sessions - 1
            rec.sub_tok_input = usage.input
            rec.sub_tok_output = usage.output
            rec.sub_tok_cache_read = usage.cache_read
            rec.sub_tok_cache_write = usage.cache_write
            rec.sub_cost_usd = usage.cost
            rec.tok_input = usage.input
            rec.tok_output = usage.output
            rec.tok_cache_read = usage.cache_read
            rec.tok_cache_write = usage.cache_write
            rec.cost_usd = usage.cost

    raw_dir = results_dir / "raw"
    raw_dir.mkdir(parents=True, exist_ok=True)
    (raw_dir / f"{rec.run_id}.jsonl").write_text(raw, encoding="utf-8")
    if err_raw.strip():
        (raw_dir / f"{rec.run_id}.stderr.log").write_text(err_raw, encoding="utf-8")

    return rec


def build_matrix(
    suites: list[dict[str, Any]],
    arms: list[str],
    reps: int,
    seed: int,
) -> list[tuple[dict[str, Any], dict[str, Any], str, int]]:
    matrix: list[tuple[dict[str, Any], dict[str, Any], str, int]] = []
    for suite in suites:
        for task in suite["tasks"]:
            for arm in arms:
                for rep in range(1, reps + 1):
                    matrix.append((suite, task, arm, rep))
    random.Random(seed).shuffle(matrix)
    return matrix


def main() -> int:
    ap = argparse.ArgumentParser(description="Pathfinder A/B benchmark runner")
    ap.add_argument("--repo", type=Path, default=None, help="frozen worktree snapshot")
    ap.add_argument(
        "--repo-map",
        default=None,
        help="per-task repos as name=path pairs, e.g. 'tokio=/x/tokio,juiceshop=/y/js'. "
             "A task's `repo` field selects which one it runs against, so one suite can "
             "span several codebases without confounding them into one snapshot.",
    )
    ap.add_argument("--mcp-bin", required=True, type=Path, help="pathfinder-mcp binary")
    ap.add_argument("--model", required=True)
    ap.add_argument("--arms", default="baseline,pathfinder,both")
    ap.add_argument("--suites", default="nav", help="comma separated suite files (no .json)")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--seed", type=int, default=1337)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument(
        "--out",
        required=True,
        type=Path,
        help="results directory — MUST be durable storage, not /tmp. Run records "
             "represent real API spend and are the only artifact here that cannot "
             "be regenerated.",
    )
    ap.add_argument("--only-task", default=None, help="restrict to one task id")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    repo_map: dict[str, Path] = {}
    if args.repo_map:
        for pair in args.repo_map.split(","):
            name, _, path = pair.partition("=")
            resolved = Path(path).expanduser().resolve()
            if not resolved.is_dir():
                print(f"repo not found: {name}={resolved}", file=sys.stderr)
                return 2
            repo_map[name.strip()] = resolved

    repo = args.repo.resolve() if args.repo else None
    mcp_bin = args.mcp_bin.resolve()
    if repo is None and not repo_map:
        print("provide --repo or --repo-map", file=sys.stderr)
        return 2
    if repo is not None and not repo.is_dir():
        print(f"snapshot not found: {repo}", file=sys.stderr)
        return 2
    if not mcp_bin.is_file():
        print(f"mcp binary not found: {mcp_bin}", file=sys.stderr)
        return 2
    if shutil.which("opencode") is None:
        print("opencode CLI not on PATH", file=sys.stderr)
        return 2

    out = args.out.resolve()
    if str(out).startswith(("/tmp/", "/var/tmp/")):
        print(
            f"refusing to write results to volatile storage: {out}\n"
            "Run records cost real money and /tmp does not survive a reboot. "
            "Point --out at a directory inside the repository.",
            file=sys.stderr,
        )
        return 2
    args.out = out

    arms = [a.strip() for a in args.arms.split(",") if a.strip()]
    for arm in arms:
        if arm not in ARMS:
            print(f"unknown arm: {arm}", file=sys.stderr)
            return 2

    suites: list[dict[str, Any]] = []
    for name in args.suites.split(","):
        name = name.strip()
        if not name:
            continue
        spec = json.loads((BENCH_DIR / "tasks" / f"{name}.json").read_text(encoding="utf-8"))
        if args.only_task:
            spec["tasks"] = [t for t in spec["tasks"] if t["id"] == args.only_task]
        if spec["tasks"]:
            suites.append(spec)

    if not suites:
        print("no tasks selected", file=sys.stderr)
        return 2

    matrix = build_matrix(suites, arms, args.reps, args.seed)
    args.out.mkdir(parents=True, exist_ok=True)
    runs_path = args.out / "runs.jsonl"

    done: set[str] = set()
    if runs_path.exists():
        for line in runs_path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                try:
                    done.add(json.loads(line)["run_id"])
                except (json.JSONDecodeError, KeyError):
                    pass

    print(f"matrix: {len(matrix)} runs | already done: {len(done)}")
    if args.dry_run:
        for suite, task, arm, rep in matrix:
            print(f"  {task['id']:<24} {arm:<12} rep{rep}")
        return 0

    # Import here so the module stays importable without the grader present.
    from grader import grade_run  # noqa: PLC0415

    t_start = time.time()
    for idx, (suite, task, arm, rep) in enumerate(matrix, 1):
        run_id = f"{task['id']}__{arm}__r{rep}"
        if run_id in done:
            print(f"[{idx}/{len(matrix)}] skip {run_id} (done)")
            continue

        task_repo = repo_map.get(task.get("repo", ""), repo)
        if task_repo is None:
            print(f"[{idx}/{len(matrix)}] {run_id} SKIP — no repo for '{task.get('repo')}'")
            continue

        print(f"[{idx}/{len(matrix)}] {run_id} ...", end="", flush=True)
        rec = run_once(
            suite=suite["suite"],
            agent=suite["agent"],
            task=task,
            arm=arm,
            rep=rep,
            model=args.model,
            repo=task_repo,
            mcp_bin=mcp_bin,
            timeout_s=args.timeout,
            results_dir=args.out,
        )
        grade_run(rec, task, task_repo)
        # Mutation tasks must not leak state into the next run.
        reset_snapshot(task_repo)

        with runs_path.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(asdict(rec)) + "\n")

        billed = rec.tok_input + rec.tok_output + rec.tok_cache_read + rec.tok_cache_write
        flag = "PASS" if rec.passed else "FAIL"
        sub = f" +{rec.subagent_sessions}sub" if rec.subagent_sessions else ""
        print(
            f" {flag} score={rec.score:.2f} tok={billed}{sub} "
            f"tools={len(rec.tool_calls)} {rec.wall_ms / 1000:.1f}s"
            + (f" ERR={rec.stream_error[:60]}" if rec.stream_error else "")
        )

    print(f"done in {(time.time() - t_start) / 60:.1f} min -> {runs_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
