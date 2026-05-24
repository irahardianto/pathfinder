# Epic 3: Tool Selection Clarity

**Priority**: P1
**Theme**: Reduce tool selection ceremony and decision fatigue
**Specs**: 5
**Estimated effort**: 1-2 days

---

## Problem Statement

Pathfinder has 15+ tools. Agents face 3 recurring decision points:
1. Which read tool? (`read_symbol_scope` vs `read_source_file` vs `read_file`)
2. Which search tool? (`search_codebase` vs `get_definition` vs `find_symbol`)
3. Which depth/parameters? (15+ tuning parameters across all tools)

The SKILL.md has a "Quick Reference" table that helps, but agents must read the full
skill document to find it. The table should be in the tool descriptions themselves.

---

## Spec 3.1: Add decision tree to tool descriptions

### Problem
Agents choose tools by reading descriptions. Current descriptions explain what each tool
does but don't help agents choose between similar tools or understand when to prefer one
over another.

### Files
- `crates/pathfinder/src/server.rs` — all tool description strings

### Changes

1. Add "When to use" guidance to each tool description:

**read_symbol_scope:**
```
Use when: You know the exact symbol and want only its source code (no surrounding context).
Alternative: Use read_source_file for full file content, or read_file for non-source files.
```

**read_source_file:**
```
Use when: You need a full file with AST symbol metadata, or want to discover available semantic paths.
Alternative: Use read_symbol_scope for a single symbol, or read_file for non-source files.
Use detail_level="source_only" to skip symbol metadata when you only need source text.
```

**read_file:**
```
Use when: Reading config files (YAML, TOML, JSON, .env, Dockerfile) or non-source files.
For source files (.rs, .ts, .go, .py, .vue): prefer read_source_file for AST metadata.
```

**search_codebase:**
```
Use when: Finding text/patterns across the codebase. Returns enclosing_semantic_path for each match.
Alternative: Use find_symbol to resolve a bare name to semantic paths.
Use get_definition to jump to where a symbol is defined (not just mentioned).
```

**find_symbol:**
```
Use when: You know a symbol name but not its file or full semantic path.
Returns ranked candidates with full semantic paths ready for other tools.
```

**get_definition:**
```
Use when: You need to find WHERE a symbol is defined. LSP-backed with grep fallback.
Alternative: Use search_codebase to find all mentions (not just definition).
```

**find_callers_callees:**
```
Use when: Understanding blast radius before refactoring. Shows who calls this symbol and what it calls.
Alternative: Use find_all_references for exhaustive reference enumeration (including non-call references).
```

**find_all_references:**
```
Use when: Finding ALL usages of a symbol (imports, type annotations, call sites, etc.).
Alternative: Use find_callers_callees for call-graph analysis (who calls whom).
```

2. Add "Quick Reference" summary to `get_repo_map` description (most-read tool):

```
Navigation quick reference:
- Find a symbol's file: find_symbol(name="SymbolName")
- Read one function: read_symbol_scope(semantic_path="file.rs::function")
- Read full file: read_source_file(filepath="file.rs")
- Find definition: get_definition(semantic_path="file.rs::symbol")
- Find all callers: find_callers_callees(semantic_path="file.rs::symbol")
- Find all usages: find_all_references(semantic_path="file.rs::symbol")
- Search code: search_codebase(query="pattern")
```

### Test Plan
- Visual inspection of tool descriptions in MCP tool list
- Verify each tool has "Use when" and "Alternative" guidance
- Verify quick reference is in get_repo_map description

### Acceptance Criteria
- All 12 tools have "Use when" guidance
- Tools with alternatives mention specific alternative tool names
- get_repo_map includes quick reference table

---

## Spec 3.2: Add usage examples to tool descriptions

### Problem
Tool descriptions explain parameters but don't show concrete usage examples. Agents
benefit more from examples than parameter descriptions.

### Files
- `crates/pathfinder/src/server.rs` — tool description strings

### Changes

1. Add 1-2 usage examples per tool:

**read_symbol_scope:**
```
Example: read_symbol_scope(semantic_path="src/auth.ts::AuthService.login")
```

**search_codebase:**
```
Example: search_codebase(query="login", path_glob="**/*.rs", filter_mode="code_only")
Example: search_codebase(query="TODO|FIXME", is_regex=true)
```

**find_callers_callees:**
```
Example: find_callers_callees(semantic_path="src/auth.ts::AuthService.login", max_depth=3)
```

2. Examples should use realistic file paths and symbol names (not "foo/bar").

### Test Plan
- Visual inspection of examples
- Verify examples use correct parameter names and types

### Acceptance Criteria
- Each tool has at least 1 concrete usage example
- Examples use correct syntax (parameter names match actual params)
- Examples cover the most common use case for each tool

---

## Spec 3.3: Simplify parameter surface with presets

### Problem
15+ tuning parameters across tools. Agents use defaults without understanding them.
Common pattern: agents don't know when to increase max_depth, max_references, etc.

### Files
- `crates/pathfinder/src/server.rs` — tool descriptions

### Changes

1. Add guidance text about when to override defaults:

**find_callers_callees description addition:**
```
Parameter guidance:
- max_depth=3 (default): Standard refactoring. Shows direct + 1-hop callers.
- max_depth=4-5: Large-scale API changes. Shows full transitive blast radius.
- max_references=50 (default): Caps output to prevent context overflow.
  Increase to 100-200 for exhaustive analysis on small codebases.
```

**get_repo_map description addition:**
```
Parameter guidance:
- max_tokens=16000 (default, auto-scales for large projects)
- max_tokens_per_file=2000 (default). Increase to 4000 for files with many symbols.
- visibility="public" (default). Use "all" to include private/test symbols.
```

**search_codebase description addition:**
```
Parameter guidance:
- max_results=50 (default). Increase for exhaustive searches; decrease for quick lookups.
- filter_mode="code_only" (default). Use "all" to include comments/strings.
- group_by_file=true to consolidate matches (recommended for multi-match edits).
```

### Test Plan
- Visual inspection of parameter guidance in descriptions

### Acceptance Criteria
- Each tool with tunable parameters includes guidance on when to override defaults
- Guidance is specific (not "adjust as needed")
- Default values are stated explicitly

---

## Spec 3.4: Add troubleshooting section to tool descriptions

### Problem
Agents encounter common failure modes but don't know how to recover. The SKILL.md has
troubleshooting guidance but agents don't always read it.

### Files
- `crates/pathfinder/src/server.rs` — tool descriptions

### Changes

1. Add "Common issues" section to relevant tools:

**All semantic-path tools:**
```
Common issues:
- SYMBOL_NOT_FOUND: Use find_symbol(name="SymbolName") to discover the correct path,
  or read_source_file(filepath, detail_level="symbols") to see available symbols.
- FILE_NOT_FOUND: Use search_codebase(query="filename") to find the correct path.
- DEGRADED results: Check lsp_health for warmup status. Use search_codebase as fallback.
```

**get_definition:**
```
Common issues:
- Returns wrong definition: grep fallback found a similar name. Check degraded_reason.
  Wait for LSP to warm up and retry.
- Returns no results: Symbol may be in a different file. Use find_symbol to locate it.
```

**find_callers_callees:**
```
Common issues:
- Empty callers with degraded=true: LSP not ready. Results are incomplete, NOT confirmed zero.
  Retry after warmup or use search_codebase(query="symbol_name") as heuristic fallback.
- Too many stdlib references: Use project_only=true (default) to filter. Check lsp_health.
```

### Test Plan
- Visual inspection of troubleshooting sections

### Acceptance Criteria
- Semantic-path tools include SYMBOL_NOT_FOUND recovery guidance
- LSP-dependent tools include degraded mode recovery guidance
- Guidance mentions specific alternative tools by name

---

## Spec 3.5: Improve tool description for find_callers_callees (former analyze_impact)

### Problem
The tool was renamed to `find_callers_callees` but the description still reads as "impact
analysis." Agents need the description to lead with the action ("find callers and callees")
not the abstract concept ("map blast radius").

### Files
- `crates/pathfinder/src/server.rs` — find_callers_callees description

### Changes

1. Rewrite description to lead with the concrete action:

```
Find all callers (incoming) and callees (outgoing) of a symbol — who calls this function 
and what does it call? Use before refactoring to understand the blast radius.

ALWAYS run this tool before recommending a refactor to check for unexpected callers.

IMPORTANT: semantic_path MUST include file path + '::' (e.g., 'src/mod.rs::func').
If unsure of the path, use find_symbol(name="func") to discover it first.

LSP-powered with grep fallback. Check degraded flag in response:
- degraded=false: LSP-confirmed, authoritative results
- degraded=true: grep heuristic, may over/under-count. Use search_codebase to verify.

Parameters:
- max_depth=3 (default): direct + 1-hop callers/callees
- project_only=true (default): excludes stdlib/external references
```

### Test Plan
- Visual inspection of new description
- Verify "ALWAYS run this tool" is prominent
- Verify degraded mode guidance is clear

### Acceptance Criteria
- Description leads with "find callers and callees" (action-oriented)
- Includes "ALWAYS run before refactoring" guidance
- Includes degraded mode interpretation guidance
- Mentions find_symbol as the discovery tool

---

## Execution Order

```
Spec 3.1 (decision tree in descriptions) -> 2 hours
Spec 3.2 (usage examples) -> 1 hour
Spec 3.5 (find_callers_callees description) -> 30 min
Spec 3.3 (parameter presets) -> 1 hour
Spec 3.4 (troubleshooting sections) -> 1 hour
```

Total: ~5.5 hours across 1-2 sessions
