#!/usr/bin/env python3
"""Recover token usage from sub-agent sessions.

`opencode run --format json` streams events for the ORCHESTRATOR session only.
When the agent dispatches work through the `task` tool the child runs in its own
session, and none of its tokens appear in the parent's stream. Measuring a
multi-agent arm from the stream alone would therefore undercount it by exactly
the amount of work it delegated — which is most of the work, and which would
make delegation look free.

Sessions form a tree through `session.parent_id`, and every assistant message
carries its own `tokens` and `cost`. This walks the tree from an orchestrator
session id and returns the whole subtree's usage.
"""

from __future__ import annotations

import json
import os
import sqlite3
from dataclasses import dataclass, field
from pathlib import Path

DEFAULT_DB = Path(os.path.expanduser("~/.local/share/opencode/opencode.db"))


@dataclass
class Usage:
    """Token and cost totals for one session subtree."""

    input: int = 0
    output: int = 0
    reasoning: int = 0
    cache_read: int = 0
    cache_write: int = 0
    cost: float = 0.0
    assistant_messages: int = 0
    sessions: int = 0
    session_ids: list[str] = field(default_factory=list)

    @property
    def billed(self) -> int:
        return self.input + self.output + self.cache_read + self.cache_write

    def merge(self, other: "Usage") -> None:
        self.input += other.input
        self.output += other.output
        self.reasoning += other.reasoning
        self.cache_read += other.cache_read
        self.cache_write += other.cache_write
        self.cost += other.cost
        self.assistant_messages += other.assistant_messages
        self.sessions += other.sessions
        self.session_ids.extend(other.session_ids)


def _session_usage(cur: sqlite3.Cursor, session_id: str) -> Usage:
    u = Usage(sessions=1, session_ids=[session_id])
    for (raw,) in cur.execute("SELECT data FROM message WHERE session_id=?", (session_id,)):
        try:
            msg = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            continue
        if msg.get("role") != "assistant":
            continue
        tok = msg.get("tokens") or {}
        if not tok:
            continue
        cache = tok.get("cache") or {}
        u.input += int(tok.get("input") or 0)
        u.output += int(tok.get("output") or 0)
        u.reasoning += int(tok.get("reasoning") or 0)
        u.cache_read += int(cache.get("read") or 0)
        u.cache_write += int(cache.get("write") or 0)
        u.cost += float(msg.get("cost") or 0.0)
        u.assistant_messages += 1
    return u


def subtree_usage(session_id: str, db: Path = DEFAULT_DB, max_depth: int = 6) -> Usage:
    """Total usage for `session_id` plus every descendant session."""
    if not db.exists():
        return Usage()
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        cur = con.cursor()
        total = Usage()
        frontier = [session_id]
        seen: set[str] = set()
        for _ in range(max_depth):
            if not frontier:
                break
            nxt: list[str] = []
            for sid in frontier:
                if sid in seen:
                    continue
                seen.add(sid)
                total.merge(_session_usage(cur, sid))
                nxt.extend(
                    r[0] for r in cur.execute("SELECT id FROM session WHERE parent_id=?", (sid,))
                )
            frontier = nxt
        return total
    finally:
        con.close()


def root_session_from_stream(raw: str) -> str | None:
    """The orchestrator session id, taken from the first event that carries one."""
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            continue
        sid = evt.get("sessionID")
        if sid:
            return str(sid)
    return None


if __name__ == "__main__":
    import sys

    if len(sys.argv) < 2:
        print("usage: subagent_usage.py <session_id>", file=sys.stderr)
        raise SystemExit(2)
    u = subtree_usage(sys.argv[1])
    print(json.dumps(u.__dict__, indent=2, default=str))
