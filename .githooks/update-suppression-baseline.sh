#!/usr/bin/env bash
# Regenerates .githooks/suppression-baseline.txt — the ratchet baseline for
# the structural-suppress gate's Layer 2 (full-codebase) check.
#
# Why a baseline instead of a hard block: the Cargo.toml lint upgrade set
# too_many_lines/cognitive_complexity/too_many_arguments/type_complexity/
# struct_excessive_bools/large_enum_variant to `deny`, but pre-existing
# `#[allow]`/`#[expect]` overrides on genuinely large functions remain valid
# Rust (item-level attributes win over the crate-level deny). The pre-commit
# hook's structural-suppress gate:
#   - Layer 1 (staged diff): hard-fails on ANY newly added suppression line,
#     unconditionally, no baseline exception. This is what actually prevents
#     new debt.
#   - Layer 2 (full codebase): compares today's per-file suppression counts
#     against this baseline. A count going UP means something slipped through
#     Layer 1 (e.g. a rebase) or the baseline is stale — that fails. A count
#     going DOWN just passes (with a note) — it does NOT require running this
#     script, so paying down debt never blocks unrelated commits. Run this
#     script explicitly once you've fixed a violation, to lock the improvement
#     in and shrink the ratchet for good.
#
# Usage: .githooks/update-suppression-baseline.sh

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# shellcheck source=lib/gates.sh
source "$REPO_ROOT/.githooks/lib/gates.sh"

BASELINE_FILE="$REPO_ROOT/.githooks/suppression-baseline.txt"
REGEX="$(forbidden_suppression_regex)"

{
  printf '# Ratchet baseline for the structural-suppress gate (Layer 2).\n'
  printf '# Format: <count>\\t<file>. Regenerate with update-suppression-baseline.sh.\n'
  printf '# Do NOT hand-edit counts upward — that defeats the ratchet.\n'
  grep -rlE --include='*.rs' "$REGEX" crates/ 2>/dev/null | sort -u | while IFS= read -r f; do
    is_suppression_exempt_path "$f" && continue
    count="$(grep -cE "$REGEX" "$f" 2>/dev/null || echo 0)"
    [[ "$count" -gt 0 ]] && printf '%d\t%s\n' "$count" "$f"
  done
} > "$BASELINE_FILE"

total=$(awk -F'\t' '!/^#/{s+=$1} END{print s+0}' "$BASELINE_FILE")
files=$(awk -F'\t' '!/^#/{c++} END{print c+0}' "$BASELINE_FILE")
printf '%sBaseline updated:%s %d suppression(s) across %d file(s) — %s\n' \
  "${C_GRN:-}" "${C_RST:-}" "$total" "$files" "$BASELINE_FILE"
