#!/usr/bin/env bash
# Resume / extend a benchmark matrix.
#
# Safe to run any number of times. The runner appends one JSON record per
# completed run and skips any run_id it already sees, so an interrupted matrix
# picks up exactly where it stopped. The only work lost to an interruption is
# the single run that was in flight.
#
# Results are written under bench/results/<dataset>/ inside the repository.
# They are the only artifact here that costs real money, so they never live in
# /tmp — the runner rejects a /tmp --out path outright. Everything the runs
# execute against (worktree, cargo target, CodeGraph index, pathfinder-mcp
# binary) is disposable and rebuilt by bench/env.sh.
#
#   ./bench/resume.sh hy3          free model, nav+edit, completes the codegraph arms
#   ./bench/resume.sh sonnet-deep  frontier model, deep suite, all 5 arms
#   ./bench/resume.sh sonnet-nav   frontier model, nav+edit, all 5 arms
#   ./bench/resume.sh opus         legacy frontier dataset, codegraph arms only

set -euo pipefail

# Bash reads a script incrementally from disk. Editing this file while a run is
# in flight shifts byte offsets and corrupts execution mid-script. Re-exec the
# whole thing through a snapshot so long runs are immune to concurrent edits.
if [[ -z "${RESUME_SNAPSHOTTED:-}" ]]; then
  _snap="$(mktemp)"; cat "${BASH_SOURCE[0]}" > "${_snap}"
  RESUME_SNAPSHOTTED=1 exec bash "${_snap}" "$@"
fi

BENCH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${BENCH}/.." && pwd)"
WHICH="${1:?usage: resume.sh <hy3|sonnet|opus>}"

# Rebuild the disposable execution environment if /tmp was cleared.
# shellcheck source=/dev/null
source "${BENCH}/env.sh"

ALL_ARMS=baseline,pathfinder,both,codegraph,cg_both

case "${WHICH}" in
  hy3)
    MODEL="opencode/hy3-free";                     DATASET=hy3free
    REPO="${PF_BENCH_A_REPO}"; MCP="${PF_BENCH_A_MCP}"
    REPS=3; ARMS=codegraph,cg_both; SUITES=nav,edit; TIMEOUT=900 ;;
  sonnet-deep)
    MODEL="google-vertex/claude-sonnet-5@default"; DATASET=sonnet5
    REPO="${PF_BENCH_B_REPO}"; MCP="${PF_BENCH_B_MCP}"
    REPS=2; ARMS="${ALL_ARMS}"; SUITES=deep; TIMEOUT=1200 ;;
  sonnet-nav)
    MODEL="google-vertex/claude-sonnet-5@default"; DATASET=sonnet5
    REPO="${PF_BENCH_B_REPO}"; MCP="${PF_BENCH_B_MCP}"
    REPS=2; ARMS="${ALL_ARMS}"; SUITES=nav,edit; TIMEOUT=900 ;;
  opus)
    MODEL="google-vertex/claude-opus-5@default";   DATASET=opus5
    REPO="${PF_BENCH_B_REPO}"; MCP="${PF_BENCH_B_MCP}"
    REPS=2; ARMS=codegraph,cg_both; SUITES=nav,edit; TIMEOUT=900 ;;
  *)
    echo "unknown target: ${WHICH}" >&2; exit 2 ;;
esac

OUT="${BENCH}/results/${DATASET}"
mkdir -p "${OUT}"
BEFORE=$( [[ -f "${OUT}/runs.jsonl" ]] && wc -l < "${OUT}/runs.jsonl" || echo 0 )
echo "==> ${DATASET}: model=${MODEL} suites=${SUITES} arms=${ARMS} reps=${REPS}"
echo "==> ${BEFORE} runs already recorded in ${OUT}/runs.jsonl (these will be skipped)"

python3 -u "${BENCH}/harness/runner.py" \
  --repo "${REPO}" --mcp-bin "${MCP}" --model "${MODEL}" \
  --suites "${SUITES}" --arms "${ARMS}" --reps "${REPS}" \
  --seed 20260817 --timeout "${TIMEOUT}" --out "${OUT}"

echo "==> regenerating reports for ${DATASET}"
python3 "${BENCH}/harness/analyze.py"   --results "${OUT}" --arms "${ALL_ARMS}" --out "${OUT}/report_all.txt"
for suite in nav deep; do
  python3 "${BENCH}/harness/analyze.py" --results "${OUT}" --arms "${ALL_ARMS}" \
    --suite "${suite}" --out "${OUT}/report_${suite}.txt" 2>/dev/null || true
done
python3 "${BENCH}/harness/mechanics.py" --results "${OUT}" --arms "${ALL_ARMS}" --out "${OUT}/mechanics.txt"
echo "==> done: ${OUT}"
