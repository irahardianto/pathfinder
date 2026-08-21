#!/usr/bin/env python3
"""Probe which models can (a) be reached and (b) accept Pathfinder's MCP tool schemas.

This is a compatibility gate, not a performance test. A tool server that a
provider refuses to load is worth zero to any agent on that provider, so this
runs before the benchmark and its output belongs in the final report.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

BENCH_DIR = Path(__file__).resolve().parent.parent


def probe(model: str, config: Path, repo: Path, mcp_bin: Path, timeout: int) -> dict:
    env = os.environ.copy()
    env["OPENCODE_CONFIG"] = str(config)
    env["PF_BENCH_REPO"] = str(repo)
    env["PF_BENCH_MCP_BIN"] = str(mcp_bin)
    env.pop("OPENCODE", None)
    env.pop("OPENCODE_PID", None)

    cmd = [
        "opencode", "run", "--pure", "--format", "json",
        "--model", model, "--agent", "nav", "--auto", "--dir", str(repo),
        "Reply with exactly: PONG",
    ]
    try:
        proc = subprocess.run(
            cmd, cwd=str(repo), env=env, capture_output=True, text=True, timeout=timeout
        )
        raw = proc.stdout
    except subprocess.TimeoutExpired:
        return {"model": model, "status": "TIMEOUT", "detail": ""}

    for line in raw.splitlines():
        if not line.strip().startswith("{"):
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            continue
        if evt.get("type") == "error":
            msg = str(evt.get("error", {}).get("data", {}).get("message", ""))
            kind = "SCHEMA_REJECTED" if "any_of" in msg or "functionDeclaration" in msg else "ERROR"
            return {"model": model, "status": kind, "detail": msg[:220].replace("\n", " ")}
        if evt.get("type") == "step_finish":
            tk = evt["part"]["tokens"]
            cache = tk.get("cache") or {}
            ctx = int(tk.get("input") or 0) + int(cache.get("read") or 0) + int(cache.get("write") or 0)
            return {
                "model": model,
                "status": "OK",
                "detail": f"first_request_ctx={ctx} cost=${evt['part'].get('cost', 0):.5f}",
            }
    return {"model": model, "status": "NO_EVENTS", "detail": ""}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", required=True, help="comma separated")
    ap.add_argument("--repo", required=True, type=Path)
    ap.add_argument("--mcp-bin", required=True, type=Path)
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    configs = {
        "baseline": BENCH_DIR / "configs" / "arm_baseline.json",
        "pathfinder": BENCH_DIR / "configs" / "arm_pathfinder.json",
    }

    rows = []
    for model in [m.strip() for m in args.models.split(",") if m.strip()]:
        row = {"model": model}
        for arm, cfg in configs.items():
            res = probe(model, cfg, args.repo.resolve(), args.mcp_bin.resolve(), args.timeout)
            row[arm] = res
            print(f"{model:<48}{arm:<12}{res['status']:<18}{res['detail'][:80]}")
            sys.stdout.flush()
        rows.append(row)

    if args.out:
        args.out.write_text(json.dumps(rows, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
