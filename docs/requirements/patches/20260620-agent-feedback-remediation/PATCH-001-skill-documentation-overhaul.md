# PATCH-001: Skill & Documentation Overhaul

Date: 2026-06-20
Source: 7 independent agent assessment reports (4 tool reports, 3 skill reports)
Status: Implemented

## Problem Statement

Agents testing the latest Pathfinder version report documentation gaps that
cause real friction. The documentation fixes from the previous session
(conversation 1348cb13) added missing parameters but did NOT address
ergonomic complaints.

Key issues:
- SKILL.md exists at TWO identical locations (drift risk)
- Quick Reference table buried at line 376/409
- `mcp()` pseudocode pre-flight in all doc files is not executable
- Critical gotchas undocumented (kind=struct strict, comments_only includes strings)
- No "if X fails, try Y" fallback sequences

All changes are docs-only. Zero code changes. Zero risk.

---

## DELIVERABLE A: Consolidate Dual SKILL.md Locations

Priority: P1 (maintenance risk)
Effort: 5 minutes
Risk: None

**Problem**: SKILL.md exists at two identical locations (both 18193 bytes).
Both must be updated together or they drift.
- `.agents/skills/pathfinder/SKILL.md`
- `docs/agent_directives/skills/pathfinder/SKILL.md`

**Steps**:
1. Delete `.agents/skills/pathfinder/SKILL.md`
2. Create symlink:
   `.agents/skills/pathfinder/SKILL.md` →
   `../../../docs/agent_directives/skills/pathfinder/SKILL.md`
3. Verify: `cat .agents/skills/pathfinder/SKILL.md` shows content
4. Update any references that point to the wrong location

**Acceptance**: Single source of truth. Symlink resolves correctly.

---

## DELIVERABLE B: Restructure SKILL.md — Quick Reference to Top

Priority: P1 (all 3 skill reports request this)
Effort: 15 minutes
Risk: None

**Problem**: Quick Reference table is at line 376/409. All 3 reports say
it's the most-used section and should be at the top.

**Steps**:
1. Move Quick Reference table from bottom to immediately after
   frontmatter/introduction
2. Move "Null vs [] Distinction" critical warning to second position
3. Move "Budget Controls" table to third position
4. Keep detailed tool descriptions in the lower half
5. Slim down formal workflow sections — keep as "Detailed Workflows"
   reference, not the primary content

**New structure**:
```
1. Frontmatter (name, description)
2. Quick Reference Table (Task → Tool chain)
3. Critical: Null vs [] Distinction
4. Budget Controls Table
5. Semantic Path Format (brief)
6. Common Mistakes (expanded — Deliverable D)
7. Tool Descriptions (condensed)
8. Detailed Workflows (condensed)
9. Fallback Table (Pathfinder tool → built-in equivalent)
```

**Acceptance**: Quick Reference in the first 40 lines.

---

## DELIVERABLE C: Replace mcp() Pseudocode Pre-Flight

Priority: P2
Effort: 10 minutes
Risk: None

**Problem**: All doc files contain `mcp({ server: "pathfinder" })`
pseudocode that agents can't execute. Reports flagged this as confusing.

**Steps**:
1. In SKILL.md Pre-Flight section, replace pseudocode with:
   ```
   Pre-Flight: Call health() once at session start.
   If it returns results, Pathfinder is available.
   If health() fails or is not listed in available tools,
   fall back to built-in tools (grep, file read).
   ```
2. In `docs/agent_directives/instructions.md`: same replacement
3. In `docs/agent_directives/AGENTS.md`: same replacement
4. Document recommended change for consumer AGENTS.md files

**Files to modify**:
- `docs/agent_directives/skills/pathfinder/SKILL.md`
- `docs/agent_directives/instructions.md`
- `docs/agent_directives/AGENTS.md`

**Acceptance**: No pseudocode in any doc file. Pre-flight is actionable.

---

## DELIVERABLE D: Expand Common Mistakes Section

Priority: P1
Effort: 30 minutes
Risk: None

**Problem**: Common Mistakes section was added in previous session but
missing critical gotchas reported by all 7 assessment reports.

**Steps**: Add these entries to Common Mistakes in SKILL.md:

### 1. Semantic Path Retry Pattern (reported as #1 friction source)

```markdown
### Path Not Found? Use "Did You Mean"

If trace() or inspect() returns SYMBOL_NOT_FOUND, check the `hint` field.
It often contains a "Did you mean: X?" suggestion with the correct path.

Common cause: Rust impl blocks may use qualified names
(e.g., super::Type.method) that differ from what search/locate returns.

Recovery workflow:
1. Try the semantic path from search/locate
2. On SYMBOL_NOT_FOUND, use the "did you mean" suggestion
3. If no suggestion, use search(mode="symbol") to find the correct
   file::symbol format
```

### 2. kind=struct Is Strict

```markdown
### kind=struct Doesn't Match Enums

kind=struct matches ONLY structs. kind=enum matches ONLY enums.
For broad type-level search, use kind=class (matches class + struct +
interface) or search for each kind separately.

Note: kind=class does NOT match enums. There is no single kind that
matches all type-level constructs (until kind=type is added — see
PATCH-002).
```

### 3. comments_only Includes Strings

```markdown
### filter_mode=comments_only Also Matches String Literals

Despite the name, comments_only matches both comments AND string literals.
This is by design — both are "non-code" content.
If you need ONLY comments (no strings), there is currently no filter for
that. A non_code alias is planned (PATCH-002).
```

### 4. filepath vs paths in read

```markdown
### read() Has Two Parameters for File Input

- filepath: single file path (string)
- paths: array of file paths (max 10, batch mode)

Use one or the other, not both.
```

**Acceptance**: Common Mistakes covers the top agent friction points.

---

## DELIVERABLE E: Document kind Filter Match Semantics

Priority: P2
Effort: 10 minutes
Risk: None

**Problem**: SKILL.md kind table says `class` "also matches structs and
interfaces" but doesn't explain that `struct` is strict, `enum` is
standalone, and there's no umbrella alias.

**Steps**: Update the kind filter table to add a "Matches" column:

| Kind | Aliases | Matches |
|------|---------|---------|
| function | method, fn | function, method |
| class | (none) | class, struct, interface (broad OOP search) |
| struct | (none) | struct ONLY |
| interface | trait | interface, trait |
| enum | (none) | enum ONLY |
| constant | const, static, let | constant |
| module | mod, namespace | module |
| impl | (none) | impl ONLY |

Add note below table:
> To find all type-level constructs, use kind=class (covers
> class+struct+interface) and separately kind=enum. There is no single
> kind that matches all types (kind=type planned in PATCH-002).

**Acceptance**: Agent can look at the table and know exactly what each
kind matches without surprises.

---

## DELIVERABLE F: Add Agent Fallback Workflow Patterns

Priority: P2
Effort: 20 minutes
Risk: None

**Problem**: No "if X fails, try Y" patterns documented. Reports request
explicit fallback sequences.

**Steps**: Add "Fallback Patterns" section after Common Mistakes:

```markdown
## Fallback Patterns

When a tool fails or returns degraded results, use these recovery
sequences:

### inspect/trace returns SYMBOL_NOT_FOUND
1. Check hint for "Did you mean" suggestion → retry with suggested path
2. If no suggestion: search(mode="symbol", query="symbol_name")
   → get correct file::symbol
3. If search finds nothing: search(mode="text", query="symbol_name")
   → broader text search

### trace returns degraded=true with null incoming
1. Check degraded_reason:
   - lsp_warmup_*: retry after retry_after_seconds from
     actionable_guidance
   - no_lsp_*: results are heuristic, use search(mode="text")
     for verification
2. Do NOT treat null incoming as "zero callers" — it means UNKNOWN

### explore truncated (coverage < 100%)
1. Increase max_tokens as suggested in hint (or use
   suggested_max_tokens if available)
2. Or narrow scope with path parameter to specific subdirectory
3. Or use detail="files" for broader coverage at lower token cost

### health shows unavailable for a language
1. Check if language server is installed
2. Try health(action="restart") to restart stuck LSP
3. Fall back to search/read for that language (grep-based, no LSP)
```

**Acceptance**: Clear "if X, then Y" decision tree for common failures.

---

## DELIVERABLE G: Update instructions.md and AGENTS.md

Priority: P2
Effort: 15 minutes
Risk: None

**Steps**:
1. In `docs/agent_directives/instructions.md`:
   - Replace mcp() pseudocode (per Deliverable C)
   - Add one-line note about comments_only including strings
   - Add one-line note about kind=class broad matching

2. In `docs/agent_directives/AGENTS.md`:
   - Replace mcp() pseudocode (per Deliverable C)
   - Add note that kind=class matches struct+interface but NOT enum
   - Add note about comments_only behavior

**Files to modify**:
- `docs/agent_directives/instructions.md`
- `docs/agent_directives/AGENTS.md`

**Acceptance**: All 3 doc files are consistent on key gotchas.

---

## DELIVERABLE H: Update search.json Schema Description

Priority: P3
Effort: 5 minutes
Risk: None

**Steps**:
1. In the search tool schema (wherever tool descriptions are defined in
   crate source — look for the `filter_mode` and `kind` parameter
   descriptions):
   - Update `filter_mode` `comments_only` description:
     "Matches in comments AND string literals (non-code content)"
   - Update `kind` description to note:
     "kind=class matches class, struct, and interface. kind=struct
     matches only struct."

**Files to modify**:
- Search tool parameter schema in crate source
- MCP schema definition (if auto-generated from code, this happens
  automatically)

**Acceptance**: Tool schema descriptions are accurate.

---

## Dependency Order

```
A (consolidate SKILL.md) → B (restructure) → D (common mistakes)
                                              → E (kind table)
                                              → F (fallback patterns)
C (mcp pseudocode) — parallel with B
G (instructions/AGENTS) — depends on C, D, E
H (schema) — standalone
```

## Suggested Implementation Order

Batch 1 (15 min): A + C (quick setup)
Batch 2 (55 min): B + D + E (SKILL.md content)
Batch 3 (40 min): F + G + H (remaining docs)

Total effort: ~2 hours

## Verification Plan

- All changes are prose-only — no compilation needed
- `ls -la .agents/skills/pathfinder/SKILL.md` — verify symlink
- `grep -rn 'mcp({' docs/` — verify no pseudocode remains
- Read through final SKILL.md to verify section order makes sense
