# PATCH-015: Fix Agent-Facing Documentation Inaccuracies

## Group: D (Low) — Agent Self-Adaptation

## Objective

Fix 6 documentation inaccuracies in AGENTS.md and pathfinder-workflow skill that give
agents wrong guidance about Pathfinder tool behavior. These docs were written against
the intended behavior, but the actual behavior (documented in the ergonomics report)
differs in ways that cause agents to make wrong decisions.

## Severity: MEDIUM — wrong guidance = wrong agent behavior

## Scope

| # | File | Issue |
|---|------|-------|
| 1 | `.pi/AGENTS.md` | LSP degraded section says `degraded: false` = "trust fully" but read_with_deep_context lies |
| 2 | `.pi/AGENTS.md` | validate_only described as "Dry-run with LSP diagnostics" — missing "uncertain" status |
| 3 | `.pi/skills/pathfinder-workflow/SKILL.md` | Token efficiency pattern shows broken group_by_file + known_files combo |
| 4 | `.pi/skills/pathfinder-workflow/SKILL.md` | validation_skipped reasons list is incomplete |
| 5 | `.pi/skills/pathfinder-workflow/SKILL.md` | read_with_deep_context latency claim is misleading |
| 6 | `.pi/skills/pathfinder-workflow/SKILL.md` | validate_only error recovery says "edit was written to disk" — wrong for validate_only |

## Step 1: Fix AGENTS.md — LSP degraded section

**File:** `.pi/AGENTS.md`

**Find:**
```markdown
- **`degraded: false`** — LSP confirmed the result. Trust it fully.
- **`degraded: true`** — Result is a best-effort approximation. Check `degraded_reason` for specifics:
```

**Replace with:**
```markdown
- **`degraded: false`** — LSP confirmed the result. Trust it fully. Exception: `read_with_deep_context` may return `degraded: false` with 0 dependencies when the LSP is still warming up — if the result seems wrong (a function that clearly calls other functions shows 0 deps), re-run after a few seconds.
- **`degraded: true`** — Result is a best-effort approximation. Check `degraded_reason` for specifics:
```

## Step 2: Fix AGENTS.md — validate_only row

**Find in the Tool Preference table:**
```markdown
| Pre-check a risky edit | `validate_only` | no equivalent | Dry-run with LSP diagnostics |
```

**Replace with:**
```markdown
| Pre-check a risky edit | `validate_only` | no equivalent | Dry-run with LSP diagnostics. Returns `status: "passed"`, `"failed"`, `"uncertain"`, or `"skipped"`. `"uncertain"` means LSP returned empty diagnostics (could be warmup — not confirmed clean). `"skipped"` means no LSP available. Never trust `"uncertain"` or `"skipped"` as confirmation |
```

## Step 3: Fix pathfinder-workflow — token efficiency pattern

**File:** `.pi/skills/pathfinder-workflow/SKILL.md`

**Find:**
```markdown
**Token efficiency pattern:**
```
# After reading fileA.ts and fileB.ts, search without re-reading their content:
search_codebase(query="deprecated_api",
                known_files=["src/fileA.ts", "src/fileB.ts"],
                exclude_glob="**/*.test.*",
                group_by_file=true)
```
```

**Replace with:**
```markdown
**Token efficiency pattern:**
```
# After reading fileA.ts and fileB.ts, search without re-reading their content:
search_codebase(query="deprecated_api",
                known_files=["src/fileA.ts", "src/fileB.ts"],
                exclude_glob="**/*.test.*",
                group_by_file=true)
```

**Known issue:** When ALL matches are in `known_files` files, `file_groups` may appear empty despite `total_matches > 0`. This is a serialization bug. Workaround: if `total_matches > 0` but `file_groups` is empty, re-run with `group_by_file=false` and `known_files=[]` to get the full flat `matches` list.
```

## Step 4: Fix pathfinder-workflow — validation_skipped reasons

**Find:**
```markdown
          validation.validation_skipped_reason = "no_lsp" | "lsp_not_on_path" |
              "lsp_start_failed" | "lsp_crash" | "lsp_timeout" |
              "pull_diagnostics_unsupported"
```

**Replace with:**
```markdown
          validation.validation_skipped_reason = "no_lsp" | "lsp_not_on_path" |
              "lsp_start_failed" | "lsp_crash" | "lsp_timeout" |
              "pull_diagnostics_unsupported" |
              "empty_diagnostics_both_snapshots"
```

And add after the reason list:
```markdown
Special case: `"empty_diagnostics_both_snapshots"` — both pre- and post-edit
diagnostics were empty. This could mean the code is genuinely clean, OR the LSP
hasn't finished indexing. The `validation.status` will be `"uncertain"` (not
`"passed"`) to signal this ambiguity. Do NOT treat `"uncertain"` as confirmation.
```

## Step 5: Fix pathfinder-workflow — read_with_deep_context latency

**Find:**
```markdown
        → NOTE: First call after LSP start may take 5–60s while the server
          indexes. Pathfinder auto-retries once during warmup.
        → If degraded=true in metadata, LSP was unavailable or warming up;
          dependencies will be empty or incomplete but source is still returned.
```

**Replace with:**
```markdown
        → NOTE: LSP warmup is unpredictable. First call may take 5–60s,
          but some LSP servers (especially rust-analyzer on large codebases)
          may take several minutes to fully index. Pathfinder auto-retries
          once during warmup, but if the LSP is still cold after the retry,
          the tool returns 0 dependencies.
        → If degraded=true in metadata, LSP was unavailable or warming up;
          dependencies will be empty or incomplete but source is still returned.
        → If degraded=false but dependencies=[] and the function clearly calls
          other functions, the LSP may have returned a false confirmation.
          Re-run the tool after waiting 30s. This is a known edge case.
```

## Step 6: Fix pathfinder-workflow — validate_only error recovery

**Find:**
```markdown
### Validation Skipped

```
Response: validation.validation_skipped = true
          validation.validation_skipped_reason = "no_lsp" | "lsp_not_on_path" |
              "lsp_start_failed" | "lsp_crash" | "lsp_timeout" |
              "pull_diagnostics_unsupported"

This means: The edit was written to disk but was NOT validated by LSP.
```

**Replace with:**
```markdown
### Validation Skipped or Uncertain

```
Response: validation.validation_skipped = true
          validation.validation_skipped_reason = "no_lsp" | "lsp_not_on_path" |
              "lsp_start_failed" | "lsp_crash" | "lsp_timeout" |
              "pull_diagnostics_unsupported" |
              "empty_diagnostics_both_snapshots"

This means: For validate_only — the edit was NOT written to disk (dry-run only)
and LSP validation could not confirm the code is clean. For real edit tools —
the edit WAS written to disk but was NOT validated by LSP.
```

## Verification

```bash
# 1. Confirm docs are updated
grep -c "uncertain" .pi/AGENTS.md .pi/skills/pathfinder-workflow/SKILL.md
# Expected: at least 2 matches

grep -c "empty_diagnostics_both_snapshots" .pi/skills/pathfinder-workflow/SKILL.md
# Expected: at least 1 match

grep -c "known issue.*group_by_file\|serialization bug" .pi/skills/pathfinder-workflow/SKILL.md
# Expected: at least 1 match
```

## EXCLUSIONS — Do NOT Modify These

- `.pi/APPEND_SYSTEM.md` — the bootstrap instructions are correct
- `.pi/skills/pathfinder-first/SKILL.md` — the pre-flight check is correct
- `.pi/prompts/*.md` — these don't reference specific Pathfinder tools
