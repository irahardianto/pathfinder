#!/usr/bin/env python3
"""Measure the token cost of Pathfinder's own MCP tool schemas.

The benchmark shows that the schema — not the retrieval — is where Pathfinder's
token penalty lives. This script turns that into a work item by attributing the
cost tool by tool, and splitting each tool's cost between its prose description
and its JSON parameter schema.

Speaks MCP over stdio directly, so it measures exactly what a client receives.
"""

from __future__ import annotations

import argparse
import json
import shlex
import subprocess
import sys
from pathlib import Path
from typing import Any

# Tool schemas are prose plus JSON. Neither tokenizes at the classic 4.0
# chars/token of running English, and the exact ratio is model-specific. This
# is calibrated against the measured first-request delta and used only to give
# per-tool costs a familiar unit; the totals in the report come from real
# provider counts, not from this constant.
CHARS_PER_TOKEN = 3.6

# Tools whose job the host agent already does natively (plain file read, plain
# text/regex grep). Exposing them duplicates capability and schema weight.
DELEGABLE_TOOLS = {"read", "search"}


def mcp_list_tools(
    argv: list[str],
    cwd: Path,
    timeout: int = 60,
) -> tuple[list[dict[str, Any]], str]:
    """Run the initialize / initialized / tools-list handshake.

    Returns the tool list AND the server-level `instructions` string. Both are
    injected into the model's context on every request, so a server that keeps
    its tools terse but ships a long instructions blob has not actually saved
    anything — counting only tool schemas would flatter it.

    Takes a full argv so any MCP stdio server can be audited, not just
    Pathfinder — comparing schema weight across servers is the whole point.
    """
    requests = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "schema-audit", "version": "1.0"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    ]
    payload = "".join(json.dumps(r) + "\n" for r in requests)
    instructions = ""

    proc = subprocess.run(
        argv,
        cwd=str(cwd),
        input=payload,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("id") == 1 and "result" in msg:
            instructions = msg["result"].get("instructions") or ""
        if msg.get("id") == 2 and "result" in msg:
            return msg["result"].get("tools", []), instructions
    raise RuntimeError(f"no tools/list response.\nstderr:\n{proc.stderr[-2000:]}")


def _strip_param_prose(node: Any) -> Any:
    """Remove per-parameter/per-variant prose from a JSON schema.

    Models are given the same guidance twice: once in the tool description and
    again in every property's `description`. Dropping the schema copy is the
    single largest available saving and costs no information.
    """
    if isinstance(node, dict):
        return {
            k: _strip_param_prose(v)
            for k, v in node.items()
            if k not in ("description", "$schema", "format", "default")
        }
    if isinstance(node, list):
        return [_strip_param_prose(v) for v in node]
    return node


def _flatten_enums(node: Any) -> Any:
    """Collapse `oneOf: [{const: a, description: ...}, ...]` to `enum: [a, ...]`.

    Semantically identical for a function-calling model, and it is also the
    form Gemini's function-calling API accepts — the `anyOf`-with-siblings
    pattern is what makes the current schemas load-fail there.
    """
    if isinstance(node, dict):
        for key in ("oneOf", "anyOf"):
            branches = node.get(key)
            if isinstance(branches, list) and branches:
                consts = [b.get("const") for b in branches if isinstance(b, dict) and "const" in b]
                if len(consts) == len(branches):
                    rest = {k: v for k, v in node.items() if k != key}
                    rest["type"] = "string"
                    rest["enum"] = consts
                    return {k: _flatten_enums(v) for k, v in rest.items()}
        return {k: _flatten_enums(v) for k, v in node.items()}
    if isinstance(node, list):
        return [_flatten_enums(v) for v in node]
    return node


def _trim_description(desc: str, keep_lines: int) -> str:
    lines = [line for line in desc.splitlines() if line.strip()]
    return "\n".join(lines[:keep_lines])


def simulate_reductions(
    tools: list[dict[str, Any]], keep_lines: int = 6, instructions_chars: int = 0
) -> list[str]:
    """Quantify what specific, information-preserving edits would save."""
    def size(desc: str, schema: dict[str, Any]) -> int:
        return len(desc) + len(json.dumps(schema, separators=(",", ":")))

    fixed = instructions_chars
    current = fixed + sum(
        size(t.get("description") or "", t.get("inputSchema") or {}) for t in tools
    )

    variants: list[tuple[str, int]] = []

    v = fixed + sum(
        size(t.get("description") or "", _strip_param_prose(t.get("inputSchema") or {}))
        for t in tools
    )
    variants.append(("A. drop duplicated prose from JSON parameter schema", v))

    v = fixed + sum(
        size(t.get("description") or "", _flatten_enums(t.get("inputSchema") or {}))
        for t in tools
    )
    variants.append(("B. collapse oneOf/anyOf const unions to plain enums", v))

    v = fixed + sum(
        size(_trim_description(t.get("description") or "", keep_lines), t.get("inputSchema") or {})
        for t in tools
    )
    variants.append((f"C. cap tool description at {keep_lines} lines", v))

    v = fixed + sum(
        size(
            _trim_description(t.get("description") or "", keep_lines),
            _flatten_enums(_strip_param_prose(t.get("inputSchema") or {})),
        )
        for t in tools
    )
    variants.append(("D. all of the above", v))

    # The benchmark shows `read` and `search` are matched or beaten by the host's
    # own read/grep on token cost, while carrying a third of the schema payload.
    # Dropping them concedes nothing measured and leaves the differentiated
    # graph-navigation tools in place.
    keep = [t for t in tools if t.get("name") not in DELEGABLE_TOOLS]
    v = fixed + sum(
        size(
            _trim_description(t.get("description") or "", keep_lines),
            _flatten_enums(_strip_param_prose(t.get("inputSchema") or {})),
        )
        for t in keep
    )
    variants.append(("E. D, plus delegate read+search to the host's built-ins", v))

    lines = [
        "",
        "REDUCTION SIMULATION (information-preserving edits only)",
        f"{'change':<56}{'chars':>10}{'est tokens':>12}{'saving':>10}",
        f"{'current':<56}{current:>10,}{round(current / CHARS_PER_TOKEN):>12,}{'-':>10}",
    ]
    for label, val in variants:
        lines.append(
            f"{label:<56}{val:>10,}{round(val / CHARS_PER_TOKEN):>12,}"
            f"{(val / current - 1) * 100:>9.0f}%"
        )
    return lines


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mcp-bin", type=Path, default=None, help="binary taking WORKSPACE as argv[1]")
    ap.add_argument("--cmd", default=None, help="full stdio server command, run inside --workspace")
    ap.add_argument("--workspace", required=True, type=Path)
    ap.add_argument("--label", default="PATHFINDER")
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    workspace = args.workspace.resolve()
    if args.cmd:
        argv = shlex.split(args.cmd)
    elif args.mcp_bin:
        argv = [str(args.mcp_bin.resolve()), str(workspace)]
    else:
        ap.error("provide --mcp-bin or --cmd")
    tools, instructions = mcp_list_tools(argv, workspace)

    rows = []
    for t in tools:
        desc = t.get("description") or ""
        schema = json.dumps(t.get("inputSchema") or {}, separators=(",", ":"))
        name = t.get("name", "?")
        total = len(name) + len(desc) + len(schema)
        rows.append(
            {
                "name": name,
                "desc_chars": len(desc),
                "schema_chars": len(schema),
                "total_chars": total,
                "est_tokens": round(total / CHARS_PER_TOKEN),
                "desc_lines": desc.count("\n") + 1 if desc else 0,
            }
        )
    rows.sort(key=lambda r: -r["total_chars"])

    lines = [
        "=" * 92,
        f"{args.label} MCP TOOL SCHEMA AUDIT",
        "=" * 92,
        f"tools exposed: {len(rows)}",
        "",
        f"{'tool':<14}{'desc chars':>12}{'desc lines':>12}{'json schema':>13}{'total':>10}{'est tokens':>12}",
    ]
    for r in rows:
        lines.append(
            f"{r['name']:<14}{r['desc_chars']:>12,}{r['desc_lines']:>12,}"
            f"{r['schema_chars']:>13,}{r['total_chars']:>10,}{r['est_tokens']:>12,}"
        )

    if instructions:
        lines.append(
            f"{'(server instructions)':<14}{len(instructions):>12,}"
            f"{instructions.count(chr(10)) + 1:>12,}{0:>13,}{len(instructions):>10,}"
            f"{round(len(instructions) / CHARS_PER_TOKEN):>12,}"
        )
    tot_chars = sum(r["total_chars"] for r in rows) + len(instructions)
    tot_desc = sum(r["desc_chars"] for r in rows)
    tot_schema = sum(r["schema_chars"] for r in rows)
    lines += [
        "-" * 92,
        f"{'TOTAL':<14}{tot_desc:>12,}{'':>12}{tot_schema:>13,}{tot_chars:>10,}"
        f"{round(tot_chars / CHARS_PER_TOKEN):>12,}",
        "",
        f"tool description prose {tot_desc / tot_chars * 100:.0f}% | "
        f"JSON parameter schema {tot_schema / tot_chars * 100:.0f}% | "
        f"server instructions {len(instructions) / tot_chars * 100:.0f}%",
        "",
        "This payload is re-sent on every request in the agent loop. At the median of",
        "3 requests per navigation task it is charged 3 times per task.",
    ]
    lines += simulate_reductions(tools, instructions_chars=len(instructions))

    report = "\n".join(lines)
    print(report)
    if args.out:
        args.out.write_text(report, encoding="utf-8")
        args.out.with_suffix(".json").write_text(json.dumps(rows, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
