# PATCH-006: Investigation Spikes — Semantic Paths, TS LSP, Grep Noise

Date: 2026-06-20
Source: All 7 agent assessment reports
Status: Research Complete — All Spikes Resolved

## Problem Statement

Three findings require investigation before a fix can be specified. These
are time-boxed research spikes, not implementation deliverables. Each spike
produces a findings document that may spawn follow-up implementation patches.

Findings documents should be saved to:
`docs/requirements/patches/20260620-agent-feedback-remediation/findings/`

---

## SPIKE A: Reproduce Semantic Path ::super:: Inconsistency

Priority: P1
Effort: Time-boxed to 2 hours
Risk: N/A (research only)

**Background**:

The Pathfinder report (testing against Pathfinder's own Rust codebase)
flagged this as the #1 issue:
- `search`/`locate` return `LspClient.new`
- `trace` requires `super::LspClient.new`
- Agents hit SYMBOL_NOT_FOUND until they use the "Did you mean" suggestion

Deep code analysis found NO `::super::` handling in the semantic path
parser. `SemanticPath` in `types.rs` uses `split_once("::")` for
file/symbol separation and `.` for symbol chain. No `super` keyword
processing exists.

**Possible explanations**:
1. Tree-sitter captures `super::` as part of symbol name in Rust
   `impl super::Type` blocks
2. The symbol chain stored by one tool includes `super::` while another
   strips it
3. The reporter had a different path issue (wrong file, wrong nesting)
   and attributed it to `::super::`

**Investigation steps**:

1. Find all `impl super::` or `impl crate::` blocks in the Pathfinder
   codebase:
   ```bash
   grep -rn 'impl super::' crates/
   grep -rn 'impl crate::' crates/
   ```

2. For each found impl block, extract the symbol's semantic path using:
   - `search(mode="symbol", query="SymbolName")` → note the
     `semantic_path`
   - `locate(semantic_path=that_path)` → verify it resolves
   - `trace(semantic_path=that_path)` → verify it works or fails

3. Specifically test `LspClient.new` in
   `crates/pathfinder-lsp/src/client/lifecycle.rs`:
   - Search for the symbol
   - Feed the returned path to `trace`
   - Observe if SYMBOL_NOT_FOUND occurs

4. If **reproducible**:
   - Identify which tool adds the `super::` prefix
   - Check tree-sitter symbol extraction
   - Determine fix scope and create follow-up patch

5. If **NOT reproducible**:
   - Document as "could not reproduce" with specific steps tried
   - Add the retry workflow to SKILL.md anyway (good defensive
     guidance — covered by PATCH-001 Deliverable D)

**Expected output**: Findings document (1-2 pages):
- Reproducible? Yes/No
- If yes: root cause, affected code, proposed fix
- If no: what was tested, why it may have been a one-time issue

---

## SPIKE B: TypeScript Language Server Call Hierarchy Support

Priority: P1
Effort: Time-boxed to 2 hours
Risk: N/A (research only)

**Background**:

All 4 tool reports confirm TypeScript lacks call hierarchy support. The
current TS server is `typescript-language-server` (wraps tsserver),
configured in `plugin.rs:132-162`. Call hierarchy methods exist in the
Lawyer trait but the TS server doesn't advertise the capability →
`trace()` always falls back to grep for TypeScript.

**Investigation steps**:

1. Check `typescript-language-server` capabilities:
   - https://github.com/typescript-language-server/typescript-language-server
   - Does latest version support `callHierarchyProvider`?
   - Check open issues/PRs about call hierarchy

2. Check Volar capabilities:
   - https://github.com/vuejs/language-tools
   - Volar supports Vue + TS — does it support call hierarchy?
   - Viable replacement or complement?

3. Check tsserver directly:
   - Does tsserver support call hierarchy natively?
   - Is this a tsserver limitation or wrapper limitation?

4. Check alternative TS language servers:
   - `@vtsls/language-server` (maintained fork with more features)
   - Any other actively maintained alternatives?

5. If a server supports call hierarchy:
   - What version is required?
   - What config changes needed in `plugin.rs`?
   - Migration risk?

**Expected output**: Decision document:
- Which TS servers support call hierarchy (if any)
- Recommended action: switch server, add alternative, or document
  limitation
- Migration risk assessment
- Follow-up patch if actionable

---

## SPIKE C: Grep Fallback Noise Reduction Assessment

Priority: P2
Effort: Time-boxed to 3 hours
Risk: N/A (research only)

**Background**:

When grep fallback activates (TS missing call hierarchy, Go warm-up),
results include unrelated symbols with same name from different
files/scopes. Reports flag this as confusing.

Current grep fallback in `impact.rs`:
- `grep_reference_fallback`: single ripgrep search, max 20 results,
  takes top 10
- `grep_outgoing_fallback`: parses call candidates from source,
  sequential ripgrep per candidate (max 4 each)
- No scope filtering — same-named symbols in unrelated files included

**Investigation steps**:

1. **Characterize the noise**:
   - Run `trace()` with grep fallback on a known symbol
   - Count true positives vs false positives in results
   - What types of false positives? (same-name imports, variables,
     type annotations, comments)

2. **Assess tree-sitter scope filtering feasibility**:
   - Can we use tree-sitter to check if a grep match is in a relevant
     scope? (same module/class as target)
   - Performance cost of tree-sitter parsing per match?

3. **Assess file-level filtering**:
   - Filter grep results to files that import or reference the target
     file → eliminates matches in completely unrelated files
   - Import graph analysis: already available in codebase?

4. **Assess confidence scoring**:
   - Instead of binary include/exclude, add confidence score per match:
     - Same file = high confidence
     - File that imports target = medium confidence
     - Unrelated file = low confidence
   - Agent can filter by confidence threshold

**Expected output**: Feasibility assessment:
- Current noise level (% false positives in typical grep fallback)
- Recommended approach (tree-sitter scope, import graph, confidence
  scoring, or combination)
- Estimated implementation effort
- Risk assessment
- Follow-up patch if approach is viable

---

## Dependency Order

All 3 spikes are independent. Can be done in parallel or any order.

Spike A should be done first (highest friction per reports).
Spike B informs whether TS grep fallback is permanent or temporary.
Spike C can be done any time — lower priority.

## Session Allocation

| Spike | Suggested Session | Time Box |
|-------|-------------------|----------|
| A | Session 2 (with PATCH-002) | 2 hours |
| B | Session 3 (with PATCH-004) | 2 hours |
| C | Session 4 (with PATCH-005) | 3 hours |

Total effort: ~7 hours (time-boxed)

## Output Location

All findings documents go to:
```
docs/requirements/patches/20260620-agent-feedback-remediation/findings/
  SPIKE-A-semantic-path-reproduction.md
  SPIKE-B-typescript-call-hierarchy.md
  SPIKE-C-grep-noise-assessment.md
```

Each findings document may reference a follow-up implementation patch
if the investigation reveals an actionable fix.
