#!/usr/bin/env python3
"""Mechanical grading for benchmark runs.

Grading never inspects the model's reasoning or prose. It extracts the
<answer> block and applies deterministic checks, or (for mutation tasks) runs
shell gates against the mutated worktree. This keeps the scoring free of the
evaluator bias that makes most agent benchmarks unfalsifiable.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path
from typing import Any

ANSWER_RE = re.compile(r"<answer>(.*?)</answer>", re.DOTALL | re.IGNORECASE)
ANSWER_OPEN_RE = re.compile(r"<answer>(.*)\Z", re.DOTALL | re.IGNORECASE)
# Weaker models sometimes emit malformed harness tags after the answer body
# (e.g. `</arg_value:...>`), swallowing the closing </answer>. Stripping those
# is a grader-robustness fix, not a scoring concession: it is applied
# identically to every arm and only ever removes non-answer noise.
TRAILING_JUNK_RE = re.compile(r"</?(?:tool_calls?|arg_value|arg_key|answer)[^>]*>\s*\Z", re.IGNORECASE)
PATH_LINE_RE = re.compile(r"([A-Za-z0-9_./\-]+\.rs)\s*[:#]\s*(\d+)")


def extract_answer(text: str | None) -> str | None:
    """Pull the last <answer> block out of the model's final text.

    Falls back to an unterminated <answer> block so that a correct answer is
    not scored zero because the model corrupted its own closing tag.
    """
    if not text:
        return None
    matches = ANSWER_RE.findall(text)
    if matches:
        return matches[-1].strip()
    open_match = ANSWER_OPEN_RE.search(text)
    if not open_match:
        return None
    body = open_match.group(1)
    prev = None
    while body != prev:
        prev = body
        body = TRAILING_JUNK_RE.sub("", body.rstrip())
    return body.strip() or None


# ────────────────────────────── normalizers ──────────────────────────────


def _norm_path_line(line: str) -> str | None:
    m = PATH_LINE_RE.search(line)
    if not m:
        return None
    path = m.group(1).lstrip("./")
    return f"{path}:{int(m.group(2))}"


def _norm_lower_nospace(line: str) -> str | None:
    stripped = re.sub(r"^[\s\-*\d.)]+", "", line).strip()
    if not stripped:
        return None
    return re.sub(r"\s+", "", stripped.lower())


NORMALIZERS = {
    "path_line": _norm_path_line,
    "lower_nospace": _norm_lower_nospace,
}


# ────────────────────────────── grade strategies ──────────────────────────────


def grade_set_of_lines(answer: str, spec: dict[str, Any]) -> tuple[float, dict[str, Any]]:
    """Set-comparison scoring with an F1 score.

    Precision matters as much as recall: an agent that dumps every `.check(`
    hit in the repo including test files has not answered the question, and a
    pure-recall metric would reward that.
    """
    norm = NORMALIZERS[spec.get("normalize", "lower_nospace")]
    tol = int(spec.get("line_tolerance", 0))

    # Structured answers carry scaffolding — `findings:`, `total_x: 12` — that is
    # requested by the prompt and is not a claimed set member. Counting it as a
    # false positive would penalise an answer for following the format.
    ignore = [re.compile(p, re.IGNORECASE) for p in spec.get("ignore_lines", [])]
    body = [ln for ln in answer.splitlines() if not any(r.search(ln) for r in ignore)]

    expected_raw = spec["expected"]
    expected = {n for n in (norm(e) for e in expected_raw) if n}
    got = {n for n in (norm(line) for line in body) if n}

    if tol > 0 and spec.get("normalize") == "path_line":
        matched_exp: set[str] = set()
        matched_got: set[str] = set()
        for e in expected:
            e_path, e_line = e.rsplit(":", 1)
            for g in got:
                if g in matched_got:
                    continue
                g_path, g_line = g.rsplit(":", 1)
                if g_path.endswith(e_path) or e_path.endswith(g_path):
                    if abs(int(g_line) - int(e_line)) <= tol:
                        matched_exp.add(e)
                        matched_got.add(g)
                        break
        tp = len(matched_exp)
    else:
        tp = len(expected & got)
        matched_got = expected & got

    precision = tp / len(got) if got else 0.0
    recall = tp / len(expected) if expected else 0.0
    f1 = 2 * precision * recall / (precision + recall) if (precision + recall) else 0.0

    return f1, {
        "true_positives": tp,
        "expected_n": len(expected),
        "returned_n": len(got),
        "precision": round(precision, 4),
        "recall": round(recall, 4),
        "f1": round(f1, 4),
        "missing": sorted(expected - matched_got)[:20],
        "spurious": sorted(got - matched_got)[:20],
    }


def grade_regex_all(answer: str, spec: dict[str, Any]) -> tuple[float, dict[str, Any]]:
    """Every required pattern must match; any forbidden pattern zeroes nothing
    but is subtracted from the score, so partial credit stays informative."""
    required = spec["patterns"]
    hits = [bool(re.search(p, answer)) for p in required]
    score = sum(hits) / len(hits) if hits else 0.0

    forbidden = spec.get("forbid_patterns", [])
    violations = [p for p in forbidden if re.search(p, answer)]
    if violations:
        score = max(0.0, score - 0.25 * len(violations))

    return score, {
        "required_hit": sum(hits),
        "required_total": len(hits),
        "failed_patterns": [p for p, h in zip(required, hits, strict=True) if not h],
        "forbidden_violations": violations,
    }


FIELD_LABEL_RE = re.compile(r"^\s*[a-z_]+:\s")
SYMBOL_TOKEN_RE = re.compile(r"\b([a-z_][a-z0-9_]{4,}|[A-Z][A-Za-z0-9]{3,})\b")
_EXISTS_CACHE: dict[tuple[str, str], bool] = {}


def _symbol_exists(repo: Path, name: str) -> bool:
    """Does this identifier actually appear as a declaration in the repo?

    The hallucination check for open-ended architecture answers. An agent that
    invents a plausible-sounding `handle_request_dispatch` has not explained
    the system, and a pure recall metric would not notice.
    """
    key = (str(repo), name)
    if key in _EXISTS_CACHE:
        return _EXISTS_CACHE[key]
    # Deliberately permissive: a plain word-boundary match anywhere in the
    # Rust sources. A declaration-shaped regex misses enum variants written as
    # `SandboxTier::HardcodedDeny,` and would flag correct answers as
    # hallucinated. A false hallucination flag corrupts the score far worse
    # than letting a real one slip through, so this errs toward "exists".
    pattern = rf"\b{re.escape(name)}\b"
    proc = subprocess.run(
        ["rg", "-q", "--type", "rust", pattern, "crates"],
        cwd=str(repo),
        capture_output=True,
        timeout=60,
    )
    found = proc.returncode == 0
    _EXISTS_CACHE[key] = found
    return found


def grade_symbol_chain(
    answer: str, spec: dict[str, Any], repo: Path
) -> tuple[float, dict[str, Any]]:
    """Score an open-ended architecture explanation, mechanically.

    Four components, equally weighted where present:
      recall     - fraction of hand-verified required symbols named
      alternates - fraction of required_any groups satisfied by any member
      patterns   - fraction of exact-format assertions matched
      ordering   - are the ordered symbols named in the correct sequence
    A hallucination penalty is then subtracted for named identifiers that do
    not exist anywhere in the repository.
    """
    low = answer.lower()
    components: list[float] = []
    detail: dict[str, Any] = {}

    required = spec.get("required") or []
    if required:
        hit = [r for r in required if r.lower() in low]
        components.append(len(hit) / len(required))
        detail["required_missing"] = [r for r in required if r.lower() not in low]

    groups = spec.get("required_any") or []
    if groups:
        satisfied = [g for g in groups if any(alt.lower() in low for alt in g)]
        components.append(len(satisfied) / len(groups))
        detail["groups_missing"] = [g for g in groups if g not in satisfied]

    patterns = spec.get("required_patterns") or []
    if patterns:
        hits = [bool(re.search(p, answer)) for p in patterns]
        components.append(sum(hits) / len(hits))
        detail["patterns_failed"] = [p for p, h in zip(patterns, hits, strict=True) if not h]

    files = spec.get("required_files") or []
    if files:
        hit_f = [f for f in files if f.lower() in low]
        components.append(len(hit_f) / len(files))
        detail["files_missing"] = [f for f in files if f.lower() not in low]

    ordered = spec.get("ordered") or []
    if ordered:
        positions = [low.find(o.lower()) for o in ordered]
        in_order = all(p >= 0 for p in positions) and positions == sorted(positions)
        components.append(1.0 if in_order else 0.0)
        detail["order_ok"] = in_order

    score = sum(components) / len(components) if components else 0.0

    # Hallucination penalty. Only identifiers the answer presents as symbols
    # are checked, and only against the repository the run executed on.
    # Strip the answer template's own field labels (`gate:`, `hint_method:`,
    # ...) before extracting identifiers — they are formatting, not claims
    # about the codebase.
    body = "\n".join(
        line.split(":", 1)[1] if FIELD_LABEL_RE.match(line) else line
        for line in answer.splitlines()
    )
    candidates = {
        t
        for t in SYMBOL_TOKEN_RE.findall(body)
        if "_" in t or (t[:1].isupper() and not t.isupper())
    }
    invented = sorted(c for c in candidates if not _symbol_exists(repo, c))
    if invented:
        score = max(0.0, score - min(0.3, 0.05 * len(invented)))
    detail["invented_symbols"] = invented[:15]
    detail["components"] = [round(c, 3) for c in components]

    return score, detail


def grade_resolution(answer: str, spec: dict[str, Any]) -> tuple[float, dict[str, Any]]:
    """Precision-weighted scoring for semantic-resolution tasks.

    These tasks each have a textually-plausible wrong answer sitting right next
    to the correct one. Recall alone would score a tool that lists every
    same-named symbol it can find as highly as one that actually resolved the
    types, so confidently-wrong claims are penalised harder than omissions:
    each expected key is worth 1/n, and each spurious pattern hit costs 1.5/n.
    """
    expected = spec["expected"]
    hits = [bool(re.search(e["must_match"], answer)) for e in expected]
    recall = sum(hits) / len(hits) if hits else 0.0

    spurious = [p for p in spec.get("spurious_patterns", []) if re.search(p, answer)]
    penalty = 1.5 * len(spurious) / len(expected) if expected else 0.0

    return max(0.0, recall - penalty), {
        "resolved": [e["key"] for e, h in zip(expected, hits, strict=True) if h],
        "missed": [e["key"] for e, h in zip(expected, hits, strict=True) if not h],
        "recall": round(recall, 4),
        "spurious_hits": spurious,
        "penalty": round(penalty, 4),
    }


def grade_shell_gate(repo: Path, spec: dict[str, Any]) -> tuple[float, dict[str, Any]]:
    """Run each gate command inside the mutated worktree."""
    gates = spec["gates"]
    results: list[dict[str, Any]] = []
    passed = 0
    for gate in gates:
        expect = int(gate.get("expect_exit", 0))
        proc = subprocess.run(
            gate["cmd"],
            shell=True,
            cwd=str(repo),
            capture_output=True,
            text=True,
            timeout=int(gate.get("timeout", 900)),
        )
        ok = proc.returncode == expect
        passed += int(ok)
        results.append(
            {
                "name": gate["name"],
                "ok": ok,
                "exit": proc.returncode,
                "tail": (proc.stdout or proc.stderr or "")[-500:],
            }
        )
    return (passed / len(gates) if gates else 0.0), {"gates": results}


# ────────────────────────────── entrypoint ──────────────────────────────


def grade_run(rec: Any, task: dict[str, Any], repo: Path) -> None:
    """Populate rec.answer / rec.score / rec.passed / rec.grade_detail."""
    spec = task["grade"]
    gtype = spec["type"]

    if gtype == "shell_gate":
        score, detail = grade_shell_gate(repo, spec)
        rec.answer = extract_answer(rec.final_text)
        rec.score = score
        rec.passed = score >= 0.999
        rec.grade_detail = detail
        return

    answer = extract_answer(rec.final_text)
    rec.answer = answer
    if answer is None:
        rec.score = 0.0
        rec.passed = False
        rec.grade_detail = {"error": "no <answer> block in final message"}
        return

    if gtype == "set_of_lines":
        score, detail = grade_set_of_lines(answer, spec)
    elif gtype == "resolution":
        score, detail = grade_resolution(answer, spec)
    elif gtype == "symbol_chain":
        score, detail = grade_symbol_chain(answer, spec, repo)
    elif gtype == "regex_all":
        score, detail = grade_regex_all(answer, spec)
    else:
        raise ValueError(f"unknown grade type: {gtype}")

    rec.score = score
    rec.passed = score >= (0.85 if gtype in ("symbol_chain", "resolution") else 0.999)
    rec.grade_detail = detail
