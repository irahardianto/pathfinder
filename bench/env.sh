#!/usr/bin/env bash
# Build (or rebuild) the benchmark execution environment.
#
# Everything this script creates lives in /tmp and is disposable: git worktrees,
# cargo target dirs, CodeGraph indexes, the pathfinder-mcp binary. All of it is
# reconstructed from the pinned commit in about a minute.
#
# What must NEVER live in /tmp is the run records. Those cost real API spend and
# cannot be regenerated, so they are written to bench/results/ inside the repo
# and the runner refuses a /tmp --out path.
#
#   source bench/env.sh          # rebuild if missing, then export paths
#   FORCE=1 source bench/env.sh  # tear down and rebuild from scratch

set -uo pipefail

BENCH_ENV_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH_COMMIT="${BENCH_COMMIT:-fa1fe8778879e007316610cc5899ee7853880322}"

# /tmp is periodically reaped on this host — it wiped the execution
# environment out from under two separate in-flight runs. Results already live
# in the repo (the runner refuses a /tmp --out), but rebuilding worktrees and
# re-indexing mid-matrix kills the run, so the scratch tree moves somewhere
# durable too. Override with PF_BENCH_HOME if you want it elsewhere.
BENCH_HOME="${PF_BENCH_HOME:-${HOME}/.cache/pf-bench}"
mkdir -p "${BENCH_HOME}"
export PF_BENCH_A="${BENCH_HOME}/a"
export PF_BENCH_B="${BENCH_HOME}/b"

_bench_prepare() {
  local work="$1"
  local repo="${work}/repo"
  local target="${work}/target"

  mkdir -p "${work}/logs"

  if [[ "${FORCE:-0}" == "1" ]]; then
    git -C "${BENCH_ENV_ROOT}" worktree remove --force "${repo}" 2>/dev/null
    rm -rf "${repo}"
  fi

  # `worktree list` keeps stale entries after /tmp is cleared; prune them or
  # `worktree add` refuses the path as already registered.
  git -C "${BENCH_ENV_ROOT}" worktree prune

  if [[ ! -e "${repo}/.git" ]]; then
    echo "==> creating worktree ${repo} @ ${BENCH_COMMIT:0:7}"
    git -C "${BENCH_ENV_ROOT}" worktree add --detach "${repo}" "${BENCH_COMMIT}" >/dev/null
  fi

  # The live AGENTS.md contains Pathfinder tool-routing guidance that would
  # coach the free-choice arms and confuse the baseline arm with references to
  # tools it cannot call. Removing it isolates the independent variable to each
  # MCP server's own self-description.
  rm -f "${repo}/AGENTS.md"

  if [[ ! -x "${target}/release/pathfinder-mcp" ]]; then
    echo "==> building pathfinder-mcp for ${repo}"
    ( cd "${repo}" && CARGO_TARGET_DIR="${target}" cargo build --release -p pathfinder-mcp >/dev/null 2>&1 )
  fi

  # Warm the check cache so mutation-task timings measure the agent, not a
  # cold dependency build.
  ( cd "${repo}" && CARGO_TARGET_DIR="${target}" cargo check -p pathfinder-mcp-common --all-targets >/dev/null 2>&1 )

  if [[ ! -f "${repo}/.codegraph/codegraph.db" ]]; then
    echo "==> indexing CodeGraph in ${repo}"
    ( cd "${repo}" && codegraph init >/dev/null 2>&1 )
  fi

  echo "==> ready: ${repo}"
}

_bench_prepare "${PF_BENCH_A}"
_bench_prepare "${PF_BENCH_B}"

export PF_BENCH_A_REPO="${PF_BENCH_A}/repo"
export PF_BENCH_A_MCP="${PF_BENCH_A}/target/release/pathfinder-mcp"
export PF_BENCH_B_REPO="${PF_BENCH_B}/repo"
export PF_BENCH_B_MCP="${PF_BENCH_B}/target/release/pathfinder-mcp"
export PF_BENCH_RESULTS="${BENCH_ENV_ROOT}/bench/results"
