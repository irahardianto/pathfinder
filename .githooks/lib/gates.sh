#!/usr/bin/env bash
# Shared helpers for Pathfinder's git hooks (pre-commit, pre-push).
#
# Sourced, not executed directly. Provides: color setup, gate result
# tracking/emission, the staged-file snapshot + helpers, the skip-list
# parser, working-tree isolation, and the forbidden-clippy-suppression
# regex shared by the structural-suppress gate and the baseline updater.
#
# Design mirrors the "graceful per-phase degradation" philosophy: a gate
# whose toolchain is absent is reported SKIPPED, never FAILED, so the hook
# works from day one and grows with the codebase without rewrites.

set -uo pipefail

# --- environment -------------------------------------------------------------
SKIP_RAW="${PATHFINDER_HOOK_SKIP:-}"
FAST="${PATHFINDER_HOOK_FAST:-0}"
RUN_ALL="${PATHFINDER_HOOK_RUN_ALL:-0}"
if [[ "${PATHFINDER_HOOK_NO_COLOR:-0}" == "1" ]] || [[ ! -t 1 ]]; then
  C_RED=""; C_YEL=""; C_GRN=""; C_CYN=""; C_DIM=""; C_RST=""
else
  C_RED=$'\033[31m'; C_YEL=$'\033[33m'; C_GRN=$'\033[32m'
  C_CYN=$'\033[36m'; C_DIM=$'\033[2m'; C_RST=$'\033[0m'
fi

declare -A FAILED_GATES=()
WARNED=0
SKIPPED=0

# Skip list parsed once. `secrets` and the Layer 1 (staged-diff) half of
# `structural-suppress` are never skippable (security-mandate.md / rule
# rule-priority.md #1 — security always wins).
declare -A SKIP_SET=()
if [[ -n "$SKIP_RAW" && "$RUN_ALL" != "1" ]]; then
  IFS=',' read -ra _toks <<<"$SKIP_RAW"
  for t in "${_toks[@]}"; do t="${t// /}"; [[ -n "$t" ]] && SKIP_SET["$t"]=1; done
fi

should_run() {
  # $1 = gate id. Returns 0 (run) / 1 (skip). `secrets` ignores skip flags.
  [[ "$1" == "secrets" ]] && return 0
  [[ "$RUN_ALL" == "1" ]] && return 0
  [[ -n "${SKIP_SET[$1]:-}" ]] && return 1
  return 0
}

# Branch-level fast alias: skip the slow / network-dependent gates only.
[[ "$FAST" == "1" ]] && { SKIP_SET["rust-test"]=1; SKIP_SET["audit"]=1; }

emit() { # level id msg
  local lvl="$1" id="$2"; shift 2
  case "$lvl" in
    fail) FAILED_GATES["$id"]=1; printf '%s[%sFAIL%s] %s%s%s — %s\n' "$C_RED" "$C_RST" "$C_RED" "$id" "$C_RST" "$C_DIM" "$*" >&2 ;;
    warn) WARNED=$((WARNED+1)); printf '%s[%sWARN%s] %s%s%s — %s\n' "$C_YEL" "$C_RST" "$C_YEL" "$id" "$C_RST" "$C_DIM" "$*" >&2 ;;
    skip) SKIPPED=$((SKIPPED+1)); printf '%s[%sSKIP%s] %s%s%s — %s\n' "$C_DIM" "$C_RST" "$C_DIM" "$id" "$C_RST" "$C_DIM" "$*" >&2 ;;
    *) printf '[%s] %s\n' "$lvl" "$*" ;;
  esac
}

pass() { printf '%s  [%sok%s]  %s%s%s\n' "$C_GRN" "$C_RST" "$C_GRN" "$1" "$C_RST" "${2:+ $C_DIM$2$C_RST}"; }

# --- staged file snapshot (honors `git add -p` partial staging) -------------
mapfile -d '' -t STAGED < <(git diff --cached --name-only --diff-filter=ACMR -z 2>/dev/null || true)

staged_any() { # ext glob(s) -> return 0 if any staged file matches
  local pat
  for pat in "$@"; do
    local f
    for f in "${STAGED[@]:-}"; do [[ "$f" == $pat ]] && return 0; done
  done
  return 1
}

blob_bytes() { # path -> staged blob size in bytes (O(1) object header lookup)
  git cat-file -s ":$1" 2>/dev/null || echo "0"
}

# --- forbidden clippy suppressions -------------------------------------------
# rust-idioms.md §3 "Clippy suppression policy — fix the code, don't silence
# the lint". These six lints are structural-soundness signals that must never
# be papered over with #[allow]/#[expect]. As of the Cargo.toml lint upgrade
# they are also `deny`d at the workspace level (with priority = 1), so a new,
# unattributed violation fails `cargo clippy` itself — this gate's job is to
# additionally block *new* suppression attributes from being introduced at
# all (staged-diff layer) and to ratchet down pre-existing ones (baseline
# layer, see update-suppression-baseline.sh).
FORBIDDEN_LINTS=(
  "too_many_lines"
  "cognitive_complexity"
  "too_many_arguments"
  "type_complexity"
  "struct_excessive_bools"
  "large_enum_variant"
)

forbidden_suppression_regex() {
  local pattern
  pattern=$(printf "%s|" "${FORBIDDEN_LINTS[@]}")
  pattern="clippy::(${pattern%|})"
  # Matches every attribute form that can suppress a lint:
  #   #[allow(...)], #![allow(...)], #[expect(...)], #![expect(...)]
  #   #[cfg_attr(cond, allow(...))], #![cfg_attr(cond, expect(...))]
  #   any of the above + , reason = "..."
  printf '%s' "#!?\\[(cfg_attr\\([^)]*,[ \t]*)?(allow|expect)\\(.*${pattern}"
}

# Test/bench/tooling code is held to different standards (matches
# .deepsource.toml's own test_patterns/exclude_patterns definitions, reused
# here for consistency rather than inventing a second definition of "test
# code"). Excluded from the structural-suppress gate's forbidden-lint scan.
is_suppression_exempt_path() {
  case "$1" in
    *_test.rs|*/tests/*|*/tests.rs|*/benches/*|crates/test-mock-lsp/*) return 0 ;;
    *) return 1 ;;
  esac
}

# =============================================================================
# WORKING-TREE ISOLATION
# =============================================================================
# cargo/clippy/typos/etc. read the filesystem — they have no concept of
# "staged". Left unguarded, they validate whatever is on disk, which is NOT
# what `git commit` is about to record whenever the working tree holds
# anything beyond the index (partial `git add -p` staging, or staged files
# sitting alongside unrelated unstaged edits). This makes the working tree
# match the index exactly for the duration of the gates, then restores it
# unconditionally on exit.
#
# Deliberately NOT `git stash --keep-index --include-untracked`: that
# combination has a reproducible spurious-conflict bug whenever a tracked
# file has both a staged change and a further unstaged edit on top —
# exactly the partial-staging case this exists to support. Instead:
#   1. Tracked files: diff the index tree against the working tree, save
#      that diff as a patch, reset the differing paths to the index's
#      content, and re-apply the saved patch on restore. No stash, no
#      3-way merge, no conflict surface.
#   2. Untracked files: moved to a temp directory and moved back.
#
# Escape hatch: PATHFINDER_HOOK_NO_ISOLATE=1 (isolation misbehaving only).
ISOLATE_PATCH_FILE=""
ISOLATE_TMPDIR=""
declare -a ISOLATE_CHANGED_TRACKED=()
declare -a ISOLATE_UNTRACKED=()
ISOLATE_APPLIED=0

_isolate_restore() {
  [[ "$ISOLATE_APPLIED" == "1" ]] || return 0
  if [[ -n "$ISOLATE_PATCH_FILE" && -s "$ISOLATE_PATCH_FILE" ]]; then
    if ! git apply "$ISOLATE_PATCH_FILE" 2>/dev/null; then
      printf '%s[WARN]%s could not auto-restore unstaged tracked changes — patch kept at %s; apply manually with `git apply %s`\n' \
        "$C_YEL" "$C_RST" "$ISOLATE_PATCH_FILE" "$ISOLATE_PATCH_FILE" >&2
      ISOLATE_PATCH_FILE=""
    fi
  fi
  local f
  for f in "${ISOLATE_UNTRACKED[@]:-}"; do
    [[ -z "$f" ]] && continue
    mkdir -p "$(dirname "$f")"
    if ! mv "$ISOLATE_TMPDIR/$f" "$f" 2>/dev/null; then
      printf '%s[WARN]%s could not auto-restore untracked file %s — check %s\n' "$C_YEL" "$C_RST" "$f" "$ISOLATE_TMPDIR" >&2
    fi
  done
  [[ -n "$ISOLATE_PATCH_FILE" ]] && rm -f "$ISOLATE_PATCH_FILE"
  [[ -n "$ISOLATE_TMPDIR" ]] && rm -rf "$ISOLATE_TMPDIR"
}

isolate_working_tree() {
  trap _isolate_restore EXIT
  if [[ "${PATHFINDER_HOOK_NO_ISOLATE:-0}" == "1" ]]; then
    emit warn isolate "PATHFINDER_HOOK_NO_ISOLATE=1 — gates run against the working tree, not the staged snapshot"
    return 0
  fi
  local _isolate_tree
  if ! _isolate_tree="$(git write-tree 2>/dev/null)"; then
    emit warn isolate "git write-tree failed — gates run against the working tree, not the staged snapshot"
    return 0
  fi
  mapfile -d '' -t ISOLATE_CHANGED_TRACKED < <(git diff-index --name-only -r -z --ignore-submodules "$_isolate_tree" 2>/dev/null)
  mapfile -d '' -t ISOLATE_UNTRACKED < <(git ls-files --others --exclude-standard -z 2>/dev/null)
  if [[ ${#ISOLATE_CHANGED_TRACKED[@]} -eq 0 && ${#ISOLATE_UNTRACKED[@]} -eq 0 ]]; then
    pass isolate "nothing to isolate — working tree already matches the index"
    return 0
  fi
  ISOLATE_PATCH_FILE="$(mktemp)"
  git diff-index --binary -r --no-color --no-ext-diff --ignore-submodules "$_isolate_tree" > "$ISOLATE_PATCH_FILE" 2>/dev/null
  ISOLATE_TMPDIR="$(mktemp -d)"
  local f
  for f in "${ISOLATE_UNTRACKED[@]:-}"; do
    [[ -z "$f" ]] && continue
    mkdir -p "$ISOLATE_TMPDIR/$(dirname "$f")"
    mv "$f" "$ISOLATE_TMPDIR/$f"
  done
  if [[ ${#ISOLATE_CHANGED_TRACKED[@]} -gt 0 ]]; then
    git checkout-index -f -- "${ISOLATE_CHANGED_TRACKED[@]}"
  fi
  ISOLATE_APPLIED=1
  pass isolate "working tree isolated to staged snapshot (${#ISOLATE_CHANGED_TRACKED[@]} tracked, ${#ISOLATE_UNTRACKED[@]} untracked set aside)"
}
