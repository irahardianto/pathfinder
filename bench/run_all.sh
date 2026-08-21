#!/usr/bin/env bash
# End-to-end benchmark driver: snapshot -> build -> probe -> run -> report.
#
# Idempotent. Re-running skips completed run_ids, so an interrupted matrix can
# be resumed without discarding work.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH="${REPO_ROOT}/bench"

COMMIT="${COMMIT:-$(git -C "${REPO_ROOT}" rev-parse HEAD)}"
WORK="${WORK:-/tmp/opencode/pf-bench}"
MODEL="${MODEL:?set MODEL=provider/model}"
NAME="${NAME:-$(echo "${MODEL}" | tr '/@' '__')}"
REPS="${REPS:-3}"
SEED="${SEED:-20260817}"
SUITES="${SUITES:-nav,edit}"
ARMS="${ARMS:-baseline,pathfinder,both}"
TIMEOUT="${TIMEOUT:-900}"

SNAP="${WORK}/repo"
TARGET="${WORK}/target"
OUT="${BENCH}/results/${NAME}"

echo "==> snapshot ${COMMIT} -> ${SNAP}"
mkdir -p "${WORK}/logs"
if [[ ! -d "${SNAP}/.git" && ! -f "${SNAP}/.git" ]]; then
  git -C "${REPO_ROOT}" worktree add --detach "${SNAP}" "${COMMIT}"
fi
# The live AGENTS.md coaches the agent about Pathfinder. Removing it isolates
# the independent variable to the MCP server's own tool descriptions.
rm -f "${SNAP}/AGENTS.md"

echo "==> build server under test from the snapshot"
( cd "${SNAP}" && CARGO_TARGET_DIR="${TARGET}" cargo build --release -p pathfinder-mcp )
MCP_BIN="${TARGET}/release/pathfinder-mcp"

echo "==> warm the check cache so mutation-task timings are not dominated by cold builds"
( cd "${SNAP}" && CARGO_TARGET_DIR="${TARGET}" cargo check -p pathfinder-mcp-common --all-targets >/dev/null )

echo "==> compatibility probe"
python3 "${BENCH}/harness/probe_models.py" \
  --repo "${SNAP}" --mcp-bin "${MCP_BIN}" --models "${MODEL}" \
  --out "${OUT}/model_probe.json" || true

echo "==> run matrix  model=${MODEL} reps=${REPS} suites=${SUITES} arms=${ARMS}"
python3 -u "${BENCH}/harness/runner.py" \
  --repo "${SNAP}" --mcp-bin "${MCP_BIN}" --model "${MODEL}" \
  --suites "${SUITES}" --arms "${ARMS}" --reps "${REPS}" \
  --seed "${SEED}" --timeout "${TIMEOUT}" --out "${OUT}" \
  2>&1 | tee -a "${WORK}/logs/${NAME}.log"

echo "==> reports"
python3 "${BENCH}/harness/analyze.py"   --results "${OUT}" --arms "${ARMS}" --out "${OUT}/report.txt"
python3 "${BENCH}/harness/mechanics.py" --results "${OUT}" --arms "${ARMS}" --out "${OUT}/mechanics.txt"

echo "==> done: ${OUT}"
