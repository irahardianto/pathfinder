# Pathfinder MCP Ergonomics Remediation Plan

## Finding Validation Summary

Each finding from the evaluation report was validated against the source code.
Results categorized as: CONFIRMED (real issue, code proves it), NOT APPLICABLE
(tested external project behavior, not Pathfinder internals), or ALREADY FIXED
(issue existed but was resolved before this review).

---

## FINDING 1: Parameter Enum Values Mismatch Documentation
### Status: CONFIRMED — code is correct, documentation is wrong

The report says `visibility=exported` and `include_imports=external` fail.
Looking at `crates/pathfinder-common/src/types.rs`:

- `Visibility` enum has variants `Public` (default) and `All`, serialized as
  `snake_case` via serde: `"public"` and `"all"`.
- `IncludeImports` enum has `None`, `ThirdParty` (default), `All`, serialized
  as `"none"`, `"third_party"`, `"all"`.

The error messages are accurate — the report author used wrong values. However,
the tool descriptions in `crates/pathfinder/src/server.rs` do NOT enumerate the
valid values. The `get_repo_map` tool description says:
```
Use `visibility=all` for private symbols.
```
But doesn't mention that `public` is the only other valid value. Similarly for
`include_imports` — no enumeration of valid values in the description.

### Root Cause
Tool descriptions in `#[tool(description = "...")]` annotations don't enumerate
valid enum values. The `schemars::JsonSchema` derive generates a JSON schema,
but agents consuming the tool descriptions textually don't see the schema.

### Fix Location
`crates/pathfinder/src/server.rs` — the `get_repo_map` tool description string.

### Remediation
Update the tool description to explicitly list valid enum values:

```
description = "Get the structural skeleton of the project — an indented tree of symbols with their semantic paths. IMPORTANT: Copy-paste the exact semantic paths from the output into other Pathfinder tools. Use `max_tokens` (default 16000) and `max_tokens_per_file` (default 2000) to control coverage. Use `visibility=\"all\"` for all symbols or `visibility=\"public\"` (default) for exported/public only. Use `changed_since` (e.g. '3h', 'HEAD~5') to scope to recent changes. Use `include_extensions`/`exclude_extensions` to filter by language. Use `include_imports` with values `\"none\"`, `\"third_party\"` (default), or `\"all\"`."
```

### Verification
1. Run `cargo build` to confirm no compile errors
2. Start Pathfinder and call `get_repo_map` with `visibility="public"`,
   `visibility="all"`, `include_imports="none"`, `include_imports="third_party"`,
   `include_imports="all"` — all must succeed
3. Call with `visibility="exported"` — must return a clear error with valid
   values listed

---

## FINDING 2: False Positives in analyze_impact (TypeScript)
### Status: CONFIRMED — `.git/index` and `CLAUDE.md` can appear in grep fallback

The grep fallback in `analyze_impact_impl` (navigation.rs) uses `path_glob: "**/*"`
with no `exclude_glob`. This means:
- `.git/index` binary file — the `ignore` crate's WalkBuilder in ripgrep.rs
  has `.git_ignore(true)` but `.git_global(false)` and `.git_exclude(false)`.
  More importantly, `.git/index` is NOT in `.gitignore` — it's a git internal
  file. The `hidden(false)` setting in WalkBuilder means dot-dirs are walked.
  The sandbox blocks `.git/index` for direct file access, but ripgrep searches
  raw files without going through the sandbox.
- `CLAUDE.md` and other markdown docs — perfectly valid text files that grep
  can match when looking for symbol names.

### Root Cause
Two issues:
1. `RipgrepScout::walk_files` uses `.hidden(false)` which walks `.git/` directory.
   While `.gitignore` rules are respected, git internal files like `.git/index`
   are not in `.gitignore` and get walked.
2. The grep fallback in `analyze_impact_impl` uses `path_glob: "**/*"` with
   empty `exclude_glob`, so ALL files including `.git/` internals and docs get
   searched.

### Fix Location
1. `crates/pathfinder-search/src/ripgrep.rs` — `WalkBuilder` configuration
2. `crates/pathfinder/src/server/tools/navigation.rs` — grep fallback
   `SearchParams` in `analyze_impact_impl`

### Remediation

#### Part A: Add `.git/` exclusion in WalkBuilder (ripgrep.rs)

In `RipgrepScout::walk_files`, add `.git` directory filtering after the
WalkBuilder is created but before the walk begins:

```rust
let walker = WalkBuilder::new(&params.workspace_root)
    .hidden(false)
    .git_ignore(true)
    .git_global(false)
    .git_exclude(false)
    .build();
```

Change to filter out `.git/` paths in the loop:

```rust
for entry in walker.flatten() {
    let path = entry.path().to_path_buf();
    if !path.is_file() {
        continue;
    }

    // Skip .git/ internals — they are binary files not meant for code search
    let relative = match path.strip_prefix(&params.workspace_root) {
        Ok(r) => r.to_string_lossy().to_string(),
        Err(_) => continue,
    };
    if relative.starts_with(".git/") || relative.starts_with(".git\\") {
        continue;
    }
    // ... rest of existing logic
```

#### Part B: Add exclude_glob in analyze_impact grep fallback

In `analyze_impact_impl`, all three grep fallback blocks use:
```rust
path_glob: "**/*".to_owned(),
exclude_glob: String::default(),
```

Change to:
```rust
path_glob: "**/*".to_owned(),
exclude_glob: "**/*.{md,txt,json,yaml,yml,toml,cfg,lock}/**".to_owned(),
```

Wait — `exclude_glob` is a single glob pattern. We need to filter better.
Actually, looking at the code, `exclude_glob` is passed as a single globset
pattern. A better approach is to limit `path_glob` to source code files:

For Rust:
```rust
path_glob: "**/*.{rs,go,ts,tsx,py,js,jsx,vue}".to_owned(),
```

But this is language-dependent. The simplest robust fix is to add a post-filter
in the grep fallback that skips non-source-file matches:

```rust
.filter(|m| {
    let ext = std::path::Path::new(&m.file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    matches!(ext, "rs" | "go" | "ts" | "tsx" | "py" | "js" | "jsx" | "vue")
})
```

### Verification
1. `cargo test` — all existing tests pass
2. Run `analyze_impact` on a TypeScript symbol during LSP warmup (degraded mode)
3. Verify `.git/index` and `CLAUDE.md` do NOT appear in results
4. Verify legitimate `.ts` callers still appear

---

## FINDING 3: LSP Warmup Latency for TypeScript
### Status: CONFIRMED — architectural limitation, partial mitigation exists

TypeScript LSP (tsserver/pyright) has slower startup than rust-analyzer or gopls.
The code already has mitigations:
- `get_definition_impl` has a 3-second retry after LSP returns `None`
- `analyze_impact_impl` does a `goto_definition` probe to distinguish warm
  LSP (confirmed zero) from cold LSP (degraded)
- `lsp_health_impl` has probe-based readiness detection with cache
- `read_with_deep_context_impl` has the same probe logic

### Root Cause
TypeScript LSP indexing is inherently slower. The existing retry/probe pattern
is good but has gaps:
1. `read_with_deep_context` does NOT have the 3-second retry that
   `get_definition` has — it goes straight to degraded when call_hierarchy
   returns empty.
2. `analyze_impact` grep fallback doesn't retry with LSP after warmup.
3. The SKILL.md says "first call may take 5-30s" but there's no automatic
   retry in the tool itself.

### Fix Location
`crates/pathfinder/src/server/tools/navigation.rs` — `read_with_deep_context_impl`

### Remediation

Add a retry-with-delay to `read_with_deep_context_impl` mirroring the pattern
in `get_definition_impl`. When `call_hierarchy_prepare` returns empty items AND
the `goto_definition` probe also returns `None`, wait 3 seconds and retry the
call hierarchy once before degrading:

In `resolve_lsp_dependencies`, after the `Ok(_)` (empty items) branch where
the probe returns None:

```rust
Ok(_) => {
    // Empty call hierarchy — verify LSP is actually warm.
    let probe = self.lawyer.goto_definition(...).await;
    if matches!(probe, Ok(Some(_))) {
        // LSP warm — confirmed zero deps
        ...
    } else {
        // ADD RETRY: Wait and try call hierarchy again
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let retry_result = self.lawyer.call_hierarchy_prepare(...).await;
        if let Ok(retry_items) = retry_result {
            if !retry_items.is_empty() {
                self.append_outgoing_deps(&retry_items[0], ...).await;
                // Return with successful resolution
            }
        }
        // If retry also fails, degrade as before
        ...
    }
}
```

### Verification
1. Start Pathfinder against a TypeScript project
2. Immediately call `read_with_deep_context` — should get degraded result
3. Wait 5 seconds, call again — should get full result OR degraded with
   evidence of retry attempt in logs
4. Check `lsp_health` to confirm indexing completed

---

## FINDING 4: Degraded Tools Unclear Meaning
### Status: CONFIRMED — `degraded_tools` doesn't explain severity

The `degraded_tools` field in `lsp_health` response lists tool names like
`["analyze_impact", "read_with_deep_context"]` when call hierarchy is unsupported.
But this doesn't tell the agent what "degraded" means for each tool.

Looking at `compute_degraded_tools` in navigation.rs:
```rust
fn compute_degraded_tools(status: &LspLanguageStatus) -> Vec<String> {
    let mut degraded = Vec::new();
    if status.supports_definition != Some(true) {
        degraded.push("get_definition".to_owned());
    }
    if status.supports_call_hierarchy != Some(true) {
        degraded.push("analyze_impact".to_owned());
        degraded.push("read_with_deep_context".to_owned());
    }
    degraded
}
```

### Root Cause
The field lists tool names but doesn't describe the fallback behavior. An agent
seeing `degraded_tools: ["analyze_impact"]` doesn't know whether:
- The tool will fail entirely
- The tool will return partial/guessed results
- The tool needs a retry after warmup

### Fix Location
`crates/pathfinder/src/server/tools/navigation.rs` — `compute_degraded_tools` function
`crates/pathfinder/src/server/types.rs` — `LspLanguageHealth` struct

### Remediation

Replace `degraded_tools: Vec<String>` with a structured type:

```rust
pub struct DegradedToolInfo {
    /// Tool name (e.g., "analyze_impact")
    pub tool: String,
    /// What the agent should expect:
    /// - "unavailable" — tool will error, use alternatives
    /// - "grep_fallback" — tool returns heuristic results, verify manually
    /// - "warmup_pending" — retry after indexing completes
    /// - "partial" — some features work (e.g., definition works but not call hierarchy)
    pub severity: String,
    /// Human-readable description of the fallback behavior
    pub description: String,
}
```

Update `compute_degraded_tools`:

```rust
fn compute_degraded_tools(status: &LspLanguageStatus) -> Vec<DegradedToolInfo> {
    let mut degraded = Vec::new();
    if status.supports_definition != Some(true) {
        degraded.push(DegradedToolInfo {
            tool: "get_definition".to_owned(),
            severity: "grep_fallback".to_owned(),
            description: "Uses ripgrep heuristic instead of LSP. May find wrong definition.".to_owned(),
        });
    }
    if status.supports_call_hierarchy != Some(true) {
        degraded.push(DegradedToolInfo {
            tool: "analyze_impact".to_owned(),
            severity: "grep_fallback".to_owned(),
            description: "Uses text search instead of call hierarchy. May over/under-count references.".to_owned(),
        });
        degraded.push(DegradedToolInfo {
            tool: "read_with_deep_context".to_owned(),
            severity: "unavailable".to_owned(),
            description: "Returns source only, no dependency signatures. Use search_codebase as alternative.".to_owned(),
        });
    }
    degraded
}
```

### Verification
1. Call `lsp_health` with a language that has partial LSP support
2. Verify response includes `degraded_tools` with severity and description
3. Verify backward compatibility: clients expecting `Vec<String>` should still
   work (or this is a breaking change that needs versioning)

---

## FINDING 5: Semantic Path Format Inconsistency
### Status: CONFIRMED — paths require exact file reference

The report shows that `logic.go::CompleteLesson` fails but
`logic_completion.go::CompleteLesson` works. This is NOT a bug — it's
by design. Tree-sitter resolves symbols within the file they're defined in.
If `CompleteLesson` is defined in `logic_completion.go`, it can't be found
via `logic.go`.

### Root Cause
This is a documentation/ergonomics issue, not a code bug. The tool descriptions
and SKILL.md explain the `file_path::symbol_chain` format, but don't explain
that the file must be the ACTUAL file where the symbol is defined. Agents may
guess the wrong file.

### Fix Location
1. `crates/pathfinder/src/server.rs` — tool descriptions for `get_definition`,
   `read_symbol_scope`, `read_with_deep_context`, `analyze_impact`
2. `.pi/skills/pathfinder/SKILL.md` — add guidance
3. `crates/pathfinder-common/src/error.rs` — improve SYMBOL_NOT_FOUND hint

### Remediation

#### Part A: Improve SYMBOL_NOT_FOUND hint (error.rs)

The existing `hint()` for `SymbolNotFound` already suggests `read_source_file`
and provides `did_you_mean`. Add file-suggestion guidance:

```rust
Self::SymbolNotFound { semantic_path, did_you_mean } => {
    let separator_hint = ...; // existing logic
    
    // NEW: if no suggestions, suggest using search_codebase to find the symbol
    if did_you_mean.is_empty() {
        let symbol_name = semantic_path.split("::").last().unwrap_or(semantic_path);
        Some(format!(
            "Use search_codebase(query=\"{symbol_name}\") to find which file defines this symbol, \
             then use the correct file path in the semantic path.{}",
            separator_hint.unwrap_or("")
        ))
    } else {
        // existing logic
    }
}
```

#### Part B: Add path discovery guidance to tool descriptions

In the `get_definition` tool description, add:
```
If you don't know which file defines a symbol, use search_codebase first to locate it.
```

#### Part C: Update SKILL.md

Add a troubleshooting section:
```
## Common Mistakes

### Wrong File in Semantic Path
If `get_definition("logic.go::CompleteLesson")` returns SYMBOL_NOT_FOUND,
the symbol might be defined in a different file. Use:
1. search_codebase(query="CompleteLesson") to find the file
2. read_source_file(filepath="logic.go", detail_level="symbols") to see all symbols in that file
```

### Verification
1. Call `get_definition("wrong_file.go::SomeSymbol")` — should get SYMBOL_NOT_FOUND
   with hint suggesting `search_codebase`
2. Call `read_source_file` on a file — should list all symbols
3. Verify SKILL.md renders correctly

---

## FINDING 6: read_file vs read_source_file Overlap
### Status: NOT A BUG — intentional design, but documentation could be clearer

The report notes confusion between `read_file` and `read_source_file`. Looking
at the tool descriptions:
- `read_file`: "Use for config files (.env, YAML, TOML, Dockerfile, package.json)"
- `read_source_file`: "AST-only — only for source files (.rs, .ts, .tsx, .go, .py, .vue, .jsx, .js); use `read_file` for config/docs files"

This is already well-documented in the tool descriptions.

### Remediation
No code change needed. The existing descriptions are clear. If anything, add
to SKILL.md:

```
## Tool Selection: read_file vs read_source_file

| File Type | Tool | Why |
|-----------|------|-----|
| .rs, .go, .ts, .py, .vue | read_source_file | AST parsing, symbol extraction |
| .yaml, .toml, .json, .env, .md | read_file | Raw content, no AST needed |
| Unknown | read_file | Safe default, returns raw text |
```

---

## FINDING 7: Grep Fallback in analyze_impact Returns Non-Source Files
### Status: CONFIRMED — same root cause as Finding 2

This is the same issue as Finding 2 (false positives). The grep fallback in
`analyze_impact_impl` searches `**/*` without filtering out binary files or
non-source files.

### Remediation
Covered by Finding 2 fix (Part A and Part B).

---

## FINDING 8: Stdlib Verbosity in analyze_impact
### Status: CONFIRMED — minor quality-of-life issue

The BFS traversal in `analyze_impact` returns ALL call hierarchy entries,
including standard library calls like `fmt.Println`, `os.Open`, etc. For a
deep traversal (max_depth=3+), this can produce hundreds of references.

### Root Cause
No filtering of stdlib/external dependency references in the BFS results.
The LSP call hierarchy API returns all outgoing calls indiscriminately.

### Fix Location
`crates/pathfinder/src/server/tools/navigation.rs` — `bfs_call_hierarchy` method

### Remediation

Add a post-filter or configuration option to exclude stdlib references. The
simplest approach is to filter by file path — stdlib references typically
resolve to files outside the workspace:

```rust
// In bfs_call_hierarchy, after collecting references:
let references: Vec<ImpactReference> = references
    .into_iter()
    .filter(|r| {
        // Only include references within the workspace
        !r.file.starts_with('/') &&
        !r.file.contains("node_modules") &&
        !r.file.contains("vendor/")
    })
    .collect();
```

Wait — looking at the code more carefully, `files_referenced` is a HashSet
that tracks ALL files. The references themselves include workspace-internal
files. The LSP already returns file paths relative to the workspace. So the
stdlib issue would only appear if the LSP resolves calls to go standard
library source files (which gopls does by resolving to the Go SDK source).

A better fix: add a `include_stdlib: bool` parameter (default false) that
filters out references whose file path starts with known SDK paths:

```rust
// Heuristic: stdlib files are outside the workspace or in Go SDK paths
let is_stdlib = r.file.contains("/usr/local/go/")
    || r.file.contains("go/src/")
    || r.file.contains("node_modules/typescript/lib/")
    || r.file.contains("python3.")
    || r.file.contains(".rustup/toolchains/");
```

But this is fragile. A simpler approach: add a `source_files_only` flag that
filters to only files with source extensions:

```rust
let is_source = ["rs", "go", "ts", "tsx", "py", "js", "jsx", "vue"]
    .iter()
    .any(|ext| r.file.ends_with(&format!(".{ext}")));
```

### Verification
1. Run `analyze_impact` on a Go function that calls `fmt.Println`
2. With filter: `fmt.Println` should NOT appear in outgoing references
3. Without filter: all references appear as before

---

## Implementation Priority

| Priority | Finding | Impact | Effort |
|----------|---------|--------|--------|
| P0 | F2/F7: False positives (.git/index, docs in grep fallback) | High — wrong data | Low |
| P1 | F1: Enum values in tool descriptions | Medium — agent errors | Low |
| P5: Semantic path discovery hint | Medium — agent confusion | Low |
| P2 | F3: LSP warmup retry for read_with_deep_context | Medium — data loss | Medium |
| P3 | F4: Degraded tools severity info | Low — agent UX | Medium |
| P4 | F8: Stdlib filtering in analyze_impact | Low — verbose output | Low |
| — | F6: read_file vs read_source_file docs | None — already clear | None |

## Execution Order for AI Agents

### Step 1: Fix false positives (Finding 2/7)
1. Open `crates/pathfinder-search/src/ripgrep.rs`
2. In `walk_files`, add `.git/` path filtering after the WalkBuilder loop
3. Open `crates/pathfinder/src/server/tools/navigation.rs`
4. In all three grep fallback blocks in `analyze_impact_impl`, add source-file
   extension filtering to the `.filter()` chain
5. Run `cargo test`
6. Manually verify with degraded-mode `analyze_impact` call

### Step 2: Fix enum documentation (Finding 1)
1. Open `crates/pathfinder/src/server.rs`
2. Update the `get_repo_map` tool description string
3. Run `cargo build`

### Step 3: Improve semantic path hints (Finding 5)
1. Open `crates/pathfinder-common/src/error.rs`
2. Update the `SymbolNotFound` hint to suggest `search_codebase`
3. Open `.pi/skills/pathfinder/SKILL.md`
4. Add troubleshooting section for wrong-file-path scenarios
5. Run `cargo test`

### Step 4: Add warmup retry to read_with_deep_context (Finding 3)
1. Open `crates/pathfinder/src/server/tools/navigation.rs`
2. In `resolve_lsp_dependencies`, add 3-second retry after empty call hierarchy
   and failed probe (mirror the pattern in `get_definition_impl`)
3. Run `cargo test`

### Step 5: Enhance degraded_tools info (Finding 4)
1. Open `crates/pathfinder/src/server/types.rs`
2. Add `DegradedToolInfo` struct with `tool`, `severity`, `description` fields
3. Open `crates/pathfinder/src/server/tools/navigation.rs`
4. Update `compute_degraded_tools` to return `Vec<DegradedToolInfo>`
5. Update `LspLanguageHealth.degraded_tools` field type
6. Run `cargo test`
7. NOTE: This is a breaking API change — verify all callers

### Step 6: Add stdlib filtering (Finding 8)
1. Open `crates/pathfinder/src/server/tools/navigation.rs`
2. In `bfs_call_hierarchy`, filter outgoing references by source file extension
3. Run `cargo test`

### Step 7: Update SKILL.md (Finding 6)
1. Open `.pi/skills/pathfinder/SKILL.md`
2. Add tool selection table for `read_file` vs `read_source_file`
