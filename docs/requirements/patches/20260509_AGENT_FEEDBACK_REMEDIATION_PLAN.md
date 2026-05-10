# Pathfinder Agent Feedback Remediation Plan

Date: 2026-05-09
Source: 4 independent agent reports from refactoring sessions using Pathfinder MCP tools

## Executive Summary

Four AI agents used Pathfinder during real refactoring sessions and reported consistent friction points. This plan validates each finding against the source code, identifies additional occurrences, and provides step-by-step remediation instructions that any agent can follow without ambiguity.

**Core theme:** Pathfinder excels at semantic navigation (discovery, understanding) but falls short at bulk refactoring workflows (exhaustive enumeration, trust in completeness, test-awareness). The gap is not architectural — it's in missing detail-level features and unclear signaling.

---

## Finding Validation Matrix

Each finding was validated by reading the actual Pathfinder source code:

| ID | Finding | Verdict | Impact | Effort |
|----|---------|---------|--------|--------|
| R1 | LSP degraded results silently unreliable | CONFIRMED | Critical | Medium |
| R2 | `analyze_impact` max_depth=2 too shallow | CONFIRMED | High | Low |
| R3 | `visibility="public"` hides tests by default | CONFIRMED | High | Low |
| R4 | No test discovery capability | CONFIRMED | High | Medium |
| R5 | No `find_all_references` tool | CONFIRMED | High | Medium |
| R6 | `read_source_file` lacks `source_only` mode | CONFIRMED | Medium | Low |
| R7 | Truncated skeleton loses non-class symbols | CONFIRMED | Medium | Low |
| R8 | `search_codebase` no completeness guarantee | CONFIRMED | High | Medium |
| R9 | Degraded signal not prominent/consistent | CONFIRMED | High | Medium |
| R10 | Semantic path discovery friction | CONFIRMED (by design) | Medium | Medium |
| R11 | `analyze_impact` name unclear | CONFIRMED | Medium | Low |

---

## R1: LSP Degraded Results Silently Unreliable

### Problem
Agents cannot distinguish "genuinely zero callers" from "LSP still warming up, results incomplete." The `degraded` flag exists in structured metadata but is easy to miss. When in doubt, agents fall back to grep — doing double work and negating Pathfinder's value.

### Evidence
- `analyze_impact` returns `null` (not `[]`) for incoming/outgoing when degraded (`navigation.rs:1229-1231`). `null` means "unknown", `[]` means "confirmed zero" — this is correct semantically but agents don't always distinguish JSON `null` from `[]`.
- `get_definition` does NOT have a `degraded` field in `GetDefinitionResponse` success path. Degradation is only signaled via `degraded_reason` being `Some(...)` — agents must check a separate field.
- `search_codebase` degraded flag is only in structured metadata, NOT in text output.
- Only `read_with_deep_context` and `analyze_impact` prepend a DEGRADED text warning. Other tools do not.

### Other Occurrences
- `navigation.rs:144-145` — `read_with_deep_context` defaults to `degraded=true`
- `navigation.rs:1234-1235` — `analyze_impact` defaults to `degraded=true`
- `search.rs:96-121` — `search_codebase` sets degraded but only in metadata
- `repo_map.rs:128-129` — `get_repo_map` degraded flag only for git errors

### Remediation

**Step 1: Add consistent degraded text prefix to ALL tool text outputs**

File: `crates/pathfinder/src/server/tools/navigation.rs`

For `get_definition` — add degraded prefix to text output in `get_def_to_call_result`:
```
Current:
  format!("{}:L{} col:{} — {}", def.file, def.line, def.column, preview)

New:
  if def.degraded {
    format!("DEGRADED ({reason}) — {}:L{} col:{} — {}", ...)
  } else {
    format!("{}:L{} col:{} — {}", ...)
  }
```

**Step 2: Add `lsp_readiness` to `analyze_impact` and `search_codebase` metadata**

File: `crates/pathfinder/src/server/types.rs`

Add `lsp_readiness: Option<String>` to `AnalyzeImpactMetadata` and `SearchCodebaseResponse`, populated the same way as in `ReadWithDeepContextMetadata` and `GetDefinitionResponse`.

Values: `"ready"`, `"warming_up"`, `"unavailable"` — consistent with existing pattern.

**Step 3: Add explicit trust guidance in `analyze_impact` text output**

File: `crates/pathfinder/src/server/tools/navigation.rs`

When `degraded=true`, the text output should say:
```
⚠️ LSP NOT READY — Results are INCOMPLETE. Zero callers does NOT mean "no callers".
Use search_codebase or lsp_health to verify before making refactoring decisions.
```

When `degraded=false` and results are empty:
```
LSP confirmed: zero callers/callees for this symbol.
```

**Step 4: Add `search_codebase` degraded text prefix**

File: `crates/pathfinder/src/server/tools/search.rs`

When `degraded=true`, prepend to the first match or add a notice:
Currently the text output is just the matches. Add a header line when degraded:
```
⚠️ DEGRADED ({reason}) — filter_mode was bypassed, all matches returned regardless of context
```

### Verification
1. Start Pathfinder against a Rust workspace
2. Immediately call `analyze_impact` before LSP warms up → verify degraded text prefix appears
3. Wait for LSP to warm, call again → verify "LSP confirmed" text appears for empty results
4. Call `get_definition` with no LSP → verify DEGRADED prefix in text output
5. Call `search_codebase` on a `.xyz` file → verify degraded prefix appears

---

## R2: `analyze_impact` max_depth=2 Too Shallow

### Problem
Default `max_depth=2` shows direct callers but not the full call chain. For multi-hop refactoring, agents need to see transitive impact. The max is 5 but agents don't discover this.

### Evidence
- Default: `types.rs:706-708` — `default_max_depth() -> u32 { 2 }`
- Clamp: `navigation.rs:1162-1164` — `params.max_depth.clamp(1, 5)`
- Tool description in `server.rs:282` says `max: 5` but doesn't recommend when to increase

### Other Occurrences
- Agent skill docs `SKILL.md:162` — documents `max_depth | 2 | BFS traversal depth (clamped 1-5)` but gives no guidance on when to use higher values

### Remediation

**Step 1: Change default max_depth from 2 to 3**

File: `crates/pathfinder/src/server/types.rs`

```rust
// Current:
pub const fn default_max_depth() -> u32 { 2 }

// New:
pub const fn default_max_depth() -> u32 { 3 }
```

Rationale: Depth 3 covers "caller → target → callee → callee-of-callee", which is the minimum for understanding whether a change propagates beyond immediate neighbors. Depth 2 misses the critical "does anyone downstream depend on the old behavior?" question. Depth 3 adds ~50% more references but catches the most common multi-hop patterns.

**Step 2: Update tool description to recommend depth based on task**

File: `crates/pathfinder/src/server.rs` — `analyze_impact` tool description

Add guidance:
```
Use max_depth=3 (default) for standard refactoring, max_depth=4-5 for large-scale
API changes where transitive callers matter. Higher depth increases result size but
reveals the full blast radius.
```

**Step 3: Update tests that hardcode max_depth=2**

File: `crates/pathfinder/src/server/types.rs` — `Default` impl for `AnalyzeImpactParams`
File: Any test that asserts `max_depth == 2`

Search for: `default_max_depth` references and update expected values.

### Verification
1. `cargo test -p pathfinder` — all tests pass with new default
2. Call `analyze_impact` without specifying `max_depth` → verify default is 3
3. Call with `max_depth=1` → verify it's clamped to 1
4. Call with `max_depth=5` → verify deep traversal works

---

## R3: `visibility="public"` Hides Tests by Default

### Problem
The default `visibility="public"` filters out test functions (which have `AccessLevel::Private` in Rust). Agents doing TDD must remember to pass `visibility="all"` every time. Test modules (`mod tests {}`) are also hidden.

### Evidence
- `types.rs:383` — `#[default]` on `Visibility::Public`
- `symbols.rs:378-399` — Rust: no `visibility_modifier` = `AccessLevel::Private`
- `repo_map.rs:110-133` — `filter_by_visibility` drops Private symbols
- `repo_map.rs:754-812` — Tests confirm: bare `mod` hidden in public, visible in all

### Other Occurrences
- `server.rs:369` — integration test explicitly sets `Visibility::Public`
- `repo_map.rs:277` — unit test default params use `Visibility::Public`
- Go: lowercase function names get `AccessLevel::Package` (visible in public), but `_" prefix names get `AccessLevel::Private` (hidden)
- Python: `_name` = Protected (visible in public), `__name` = Private (hidden)

### Remediation

**Step 1: Add `include_tests` parameter to `GetRepoMapParams`**

File: `crates/pathfinder/src/server/types.rs`

```rust
pub struct GetRepoMapParams {
    // ... existing fields ...
    
    /// Include test functions and test modules regardless of visibility filter.
    /// When `true`, symbols inside `mod tests {}` blocks and functions with
    /// `#[test]` attributes are always included, even with `visibility="public"`.
    /// Default: `true` (test functions are included by default).
    #[serde(default = "default_true")]
    pub include_tests: bool,
}

pub const fn default_true() -> bool { true }
```

**Step 2: Implement test-module detection in `filter_by_visibility`**

File: `crates/pathfinder-treesitter/src/repo_map.rs`

Modify `filter_by_visibility` to accept an `include_tests` parameter:

```rust
fn filter_by_visibility(
    symbols: Vec<ExtractedSymbol>,
    visibility: &str,
    include_tests: bool,
) -> Vec<ExtractedSymbol> {
    if visibility != "public" {
        return symbols;
    }
    symbols
        .into_iter()
        .filter(|sym| {
            // Always include test modules/functions when include_tests=true
            if include_tests && is_test_symbol(sym) {
                return true;
            }
            matches!(
                sym.access_level,
                AccessLevel::Public | AccessLevel::Protected
            )
        })
        .map(|mut sym| {
            sym.children = filter_by_visibility(sym.children, visibility, include_tests);
            sym
        })
        .collect()
}

fn is_test_symbol(sym: &ExtractedSymbol) -> bool {
    // Module named "tests" or "test"
    if sym.kind == SymbolKind::Module && matches!(sym.name.as_str(), "tests" | "test") {
        return true;
    }
    // Function with test-like name convention
    if sym.kind == SymbolKind::Function {
        let name = sym.name.as_str();
        if name.starts_with("test_") || name.starts_with("it_") || name.ends_with("_test") {
            return true;
        }
    }
    false
}
```

**Step 3: Thread `include_tests` through the skeleton generation pipeline**

Files to modify:
- `crates/pathfinder-treesitter/src/repo_map.rs` — `SkeletonConfig` add `include_tests: bool` field
- `crates/pathfinder-treesitter/src/surgeon.rs` — `Surgeon` trait: `generate_skeleton` method signature (or add to config)
- `crates/pathfinder/src/server/tools/repo_map.rs` — pass `params.include_tests` to skeleton config

**Step 4: Update tool description**

File: `crates/pathfinder/src/server.rs`

Add to `get_repo_map` description:
```
Use `include_tests=true` (default) to include test functions/modules regardless of
visibility filter, or `include_tests=false` to strictly follow visibility rules.
```

### Verification
1. Call `get_repo_map` with `visibility="public"` (default) → verify test functions appear
2. Call with `visibility="public", include_tests=false` → verify tests are hidden
3. Call with `visibility="all"` → verify all symbols visible regardless of `include_tests`
4. `cargo test -p pathfinder-treesitter` — all repo_map tests pass

---

## R4: No Test Discovery Capability

### Problem
Agents have no way to find tests for a specific function. They must manually `search_codebase` for the function name and hope to find test files. A dedicated tool or parameter would save significant time during TDD workflows.

### Evidence
- No `SymbolKind::Test` variant exists (`surgeon.rs:58-90`)
- No `find_tests` tool exists anywhere in the codebase
- Unchecked requirement: `20260427-002-rust-module-symbol-indexing.md:389` — "Test functions appear nested under `tests` in `read_source_file(detail_level="symbols")`"

### Other Occurrences
- `detect_access_level` has no attribute inspection (`symbols.rs:206`) — `#[test]` is not detected
- `SymbolKind` enum doesn't distinguish test functions from regular functions

### Remediation

**Step 1: Add `SymbolKind::Test` variant**

File: `crates/pathfinder-treesitter/src/surgeon.rs`

Add to `SymbolKind` enum:
```rust
pub enum SymbolKind {
    // ... existing variants ...
    Test,  // A test function (e.g., #[test] fn, @Test def, test_ prefix)
}
```

**Step 2: Add attribute detection to `detect_access_level` or create separate `detect_test_attribute`**

File: `crates/pathfinder-treesitter/src/symbols.rs`

Add a new function:
```rust
fn is_test_function(node: Node, lang: SupportedLanguage) -> bool {
    match lang {
        SupportedLanguage::Rust => {
            // Check for `attribute_item` child containing `test`
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "attribute_item" {
                    let text = node.tree().source_text_for_child(child.id());
                    if text.map_or(false, |t| t.contains("#[test]")) {
                        return true;
                    }
                }
            }
            false
        }
        SupportedLanguage::Python => {
            // Check for decorator containing "pytest" or "unittest"
            // Or function name starting with "test_"
            false // placeholder
        }
        SupportedLanguage::Go => {
            // Check for function name starting with "Test" (Go convention)
            // Or file ending with _test.go
            false // placeholder
        }
        _ => false,
    }
}
```

**Step 3: Classify test functions in `determine_symbol_kind`**

File: `crates/pathfinder-treesitter/src/symbols.rs`

Modify `determine_symbol_kind` to check for test attributes:
```rust
fn determine_symbol_kind(&self, node: Node, kind: &str) -> Option<SymbolKind> {
    if self.types.function_kinds.contains(&kind) {
        if is_test_function(node, self.lang) {
            return Some(SymbolKind::Test);
        }
        return Some(SymbolKind::Function);
    }
    // ... rest unchanged ...
}
```

**Step 4: Add `find_tests` parameter to `search_codebase`**

File: `crates/pathfinder/src/server/types.rs`

Add to `SearchCodebaseParams`:
```rust
/// When `true`, only return matches inside test functions/modules.
/// Useful for finding test coverage for a specific symbol.
/// Default: `false`.
#[serde(default)]
pub tests_only: bool,
```

File: `crates/pathfinder/src/server/tools/search.rs`

After enrichment, filter matches where `enclosing_semantic_path` contains a test symbol.

**Step 5: Add `find_tests_for` to `read_symbol_scope` description or as separate tool**

Option A (simpler): Add a hint in tool descriptions:
```
To find tests for a function, use search_codebase with tests_only=true
and the function name as query.
```

Option B (more discoverable): Add a dedicated `find_tests` tool. This is deferred to a future epic since it requires LSP `textDocument/references` support. Document this in the deferred findings.

### Verification
1. Run `get_repo_map` on a Rust file with `#[test] fn` → verify `Test` kind appears in skeleton
2. Run `search_codebase` with `tests_only=true` for a function name → verify only test matches returned
3. Run `read_source_file` with `detail_level="symbols"` → verify test functions are labeled as `Test` kind
4. `cargo test -p pathfinder-treesitter` — all symbol extraction tests pass

---

## R5: No `find_all_references` Tool

### Problem
`analyze_impact` is call-hierarchy based (callers/callees), not reference-based (all usages including type annotations, imports, comments). For exhaustive refactoring ("find ALL occurrences of field X"), agents need reference enumeration, not call hierarchy.

### Evidence
- `FEATURE-001-new-tools-epic.md:168-196` — `find_all_references` is a proposed tool with full spec
- `DEFERRED-001-not-remediated.md:251` — listed as deferred, requires `textDocument/references` in `Lawyer` trait
- Current `analyze_impact` uses `call_hierarchy_incoming/outgoing` which misses non-call references

### Other Occurrences
- `Lawyer` trait (`crates/pathfinder-lsp/src/lawyer.rs`) has no `find_references` method
- Grep fallback in `analyze_impact` searches for the symbol name but doesn't distinguish call sites from references

### Remediation

**Step 1: Add `find_references` to the `Lawyer` trait**

File: `crates/pathfinder-lsp/src/lawyer.rs`

```rust
#[async_trait]
pub trait Lawyer: Send + Sync {
    // ... existing methods ...
    
    /// Find all references to a symbol at the given position.
    /// Returns file, line, column for each reference.
    /// Falls back to empty Vec when LSP is unavailable.
    async fn find_references(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
        include_declaration: bool,
    ) -> Result<Vec<ReferenceLocation>, LspError>;
}
```

**Step 2: Implement in `LspClient`**

File: `crates/pathfinder-lsp/src/client/mod.rs`

Add LSP `textDocument/references` request handler. This is a standard LSP operation supported by rust-analyzer, gopls, typescript-language-server, and pylsp.

**Step 3: Add `find_all_references` MCP tool**

File: `crates/pathfinder/src/server.rs`

```rust
#[tool(
    name = "find_all_references",
    description = "Find all references to a symbol across the codebase — every file and line where the symbol is used, imported, or mentioned. Use for exhaustive refactoring to ensure no usage is missed. IMPORTANT: semantic_path MUST include file path + '::' (e.g., 'src/mod.rs::func'). Returns reference list with file, line, and snippet. LSP-powered with grep fallback for completeness."
)]
async fn find_all_references(
    &self,
    Parameters(params): Parameters<FindAllReferencesParams>,
) -> Result<Json<FindAllReferencesResponse>, ErrorData> {
    self.find_all_references_impl(params).await
}
```

**Step 4: Implement with LSP + grep double-check**

The implementation should:
1. Call LSP `textDocument/references` for authoritative results
2. Run `search_codebase` for the symbol name as a completeness check
3. Merge results, flagging any grep-only matches as `heuristic: true`
4. Return combined list with `degraded` flag when LSP was unavailable

This "trust but verify" approach addresses the completeness concern from agent reports.

### Verification
1. Call `find_all_references` on a well-known function → verify all call sites + type annotations found
2. Call with LSP unavailable → verify grep fallback works and results are marked `degraded`
3. Compare with manual `search_codebase` → verify no references are missed
4. Call on a private field → verify both constructor and access sites found

---

## R6: `read_source_file` Lacks `source_only` Mode

### Problem
`read_source_file` always appends a symbol tree to the output. For targeted verification during audits ("just show me lines 50-60"), the symbol tree doubles the output tokens. Agents fall back to `read_file` for this use case.

### Evidence
- `source_file.rs:172-181` — only 3 modes: `compact`, `symbols`, `full`
- Default is `compact` = source + flat symbols
- No way to get just the source without any symbol metadata

### Other Occurrences
- `compact` mode uses `map_symbols_compact` which creates flat symbol list (no children) — still doubles output for large files
- `full` mode uses `map_symbols` with nested children — even more overhead

### Remediation

**Step 1: Add `source_only` detail level**

File: `crates/pathfinder/src/server/tools/source_file.rs`

Add a new arm to the match:
```rust
match params.detail_level.as_str() {
    "source_only" => (Some(content), vec![]),  // Source code only, no symbols
    "symbols" => { /* existing */ }
    "full" => { /* existing */ }
    _ => (Some(content), map_symbols_compact(symbols)), // "compact" default
}
```

**Step 2: Update tool description**

File: `crates/pathfinder/src/server.rs`

Update `read_source_file` description:
```
detail_level: `compact` (default) = source + flat symbols, `source_only` = source code only
(no symbol metadata — use for targeted reading when you don't need structure),
`symbols` = tree only, `full` = source + nested AST.
```

**Step 3: Update default detail_level documentation**

File: `crates/pathfinder/src/server/types.rs`

Update `ReadSourceFileParams.detail_level` doc comment:
```rust
/// Detail level: "source_only", "compact", "symbols", or "full".
/// - "source_only" — source code only, no symbol metadata (lowest token cost)
/// - "compact" (default) — source + flat symbol list
/// - "symbols" — symbol tree only, no source
/// - "full" — source + nested symbol tree
```

### Verification
1. Call `read_source_file` with `detail_level="source_only"` → verify only source text returned
2. Call with `detail_level="source_only"` + `start_line=50, end_line=60` → verify minimal output
3. Verify `structured_content` still has `language` field but empty `symbols` array
4. `cargo test -p pathfinder` — all tests pass

---

## R7: Truncated Skeleton Loses Non-Class Symbols

### Problem
When a file exceeds `max_tokens_per_file`, `render_truncated_file_skeleton` only preserves Class and Struct names + method counts. Enums, Traits, Interfaces, top-level Functions, Constants, and Modules are silently dropped. The truncation notice says `[TRUNCATED DUE TO SIZE]` but doesn't list what was cut.

### Evidence
- `repo_map.rs:185-216` — only keeps `SymbolKind::Class` and `SymbolKind::Struct`
- All other symbol kinds are dropped without mention
- Test at `repo_map.rs:505-543` — verifies `class` and `methods omitted` but no coverage for dropped enums/traits

### Other Occurrences
- `symbols.rs` has 13 `SymbolKind` variants but truncated skeleton only preserves 2
- Vue zone symbols (Zone, Component, HtmlElement, CssSelector, CssAtRule) are also dropped

### Remediation

**Step 1: Preserve top-level symbol names of ALL kinds in truncated output**

File: `crates/pathfinder-treesitter/src/repo_map.rs`

Replace `render_truncated_file_skeleton` with:
```rust
fn render_truncated_file_skeleton(symbols: &[ExtractedSymbol]) -> String {
    let mut out = String::default();
    
    for sym in symbols {
        let prefix = match sym.kind {
            SymbolKind::Class => "class ",
            SymbolKind::Struct => "struct ",
            SymbolKind::Enum => "enum ",
            SymbolKind::Interface => "interface ",
            SymbolKind::Trait => "trait ",  // Will be SymbolKind::Interface for Rust traits
            SymbolKind::Function => "func ",
            SymbolKind::Method => "method ",
            SymbolKind::Constant => "const ",
            SymbolKind::Module => "mod ",
            SymbolKind::Impl => "impl ",
            SymbolKind::Zone => "zone ",
            SymbolKind::Component => "component ",
            SymbolKind::HtmlElement => "element ",
            SymbolKind::CssSelector => "selector ",
            SymbolKind::CssAtRule => "at-rule ",
        };
        
        let _ = writeln!(out, "{}{} // {}", prefix, sym.name, sym.semantic_path);
        
        // For class/struct/enum/interface, count children by kind
        if matches!(sym.kind, SymbolKind::Class | SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Interface) {
            let method_count = sym.children.iter().filter(|c| c.kind == SymbolKind::Method).count();
            let func_count = sym.children.iter().filter(|c| c.kind == SymbolKind::Function).count();
            let const_count = sym.children.iter().filter(|c| c.kind == SymbolKind::Constant).count();
            
            let mut omitted = Vec::new();
            if method_count > 0 { omitted.push(format!("{method_count} methods")); }
            if func_count > 0 { omitted.push(format!("{func_count} functions")); }
            if const_count > 0 { omitted.push(format!("{const_count} constants")); }
            if !omitted.is_empty() {
                let _ = writeln!(out, "  // ... {} omitted", omitted.join(", "));
            }
        }
    }
    
    if out.is_empty() {
        "// [TRUNCATED - NO SYMBOLS EXTRACTED]".to_string()
    } else {
        format!("// [TRUNCATED DUE TO SIZE]\n{out}")
    }
}
```

**Step 2: Update test to verify non-class symbols are preserved**

File: `crates/pathfinder-treesitter/src/repo_map.rs`

Add test:
```rust
#[test]
fn test_truncated_file_skeleton_preserves_enums_and_traits() {
    let symbols = vec![
        ExtractedSymbol { name: "MyEnum".into(), kind: SymbolKind::Enum, ... },
        ExtractedSymbol { name: "MyTrait".into(), kind: SymbolKind::Interface, ... },
        ExtractedSymbol { name: "helper".into(), kind: SymbolKind::Function, ... },
    ];
    let output = render_truncated_file_skeleton(&symbols);
    assert!(output.contains("enum MyEnum"));
    assert!(output.contains("interface MyTrait"));
    assert!(output.contains("func helper"));
}
```

### Verification
1. Create a large Rust file with enums, traits, functions, and structs
2. Call `get_repo_map` with low `max_tokens_per_file` → verify all symbol kinds appear in truncated output
3. Verify `[TRUNCATED DUE TO SIZE]` marker is still present
4. `cargo test -p pathfinder-treesitter` — all repo_map tests pass

---

## R8: `search_codebase` No Completeness Guarantee

### Problem
Unlike `get_repo_map` which provides `files_scanned`, `files_in_scope`, and `coverage_percent`, `search_codebase` gives no indication of how complete the search was. Agents can't tell if results are exhaustive or if files were skipped.

### Evidence
- `SearchCodebaseResponse` has `total_matches` and `truncated` but no coverage metrics
- `GetRepoMapMetadata` has `files_scanned`, `files_in_scope`, `coverage_percent` — search lacks equivalents
- Ripgrep may skip binary files, large files, and files matching `.gitignore` — agents are unaware

### Other Occurrences
- `search.rs:50-185` — response construction has no coverage info
- `ripgrep.rs` — the scout doesn't report how many files were searched vs. skipped

### Remediation

**Step 1: Add coverage metadata to `SearchCodebaseResponse`**

File: `crates/pathfinder/src/server/types.rs`

```rust
pub struct SearchCodebaseResponse {
    // ... existing fields ...
    
    /// Number of files that were searched.
    pub files_searched: usize,
    
    /// Number of files matching the path_glob that were available for search.
    /// When `files_searched < files_in_scope`, some files were skipped
    /// (binary, .gitignored, or permission-denied).
    pub files_in_scope: usize,
    
    /// Percentage of in-scope files that were actually searched.
    /// 100% means exhaustive search; lower values indicate skipped files.
    pub coverage_percent: u8,
}
```

**Step 2: Collect file stats from ripgrep**

File: `crates/pathfinder-search/src/ripgrep.rs`

Add file counting to `RipgrepScout::search`. The `ignore` crate's walker already tracks which files are visited vs. skipped. Expose these counts in `SearchResult`:

```rust
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub total_matches: usize,
    pub truncated: bool,
    pub files_searched: usize,    // NEW
    pub files_in_scope: usize,    // NEW
}
```

**Step 3: Surface coverage in text output**

File: `crates/pathfinder/src/server/tools/search.rs`

When `coverage_percent < 100`, add a notice:
```
Search covered 85% of in-scope files (102/120). Some files were skipped
(.gitignored, binary, or permission-denied). For exhaustive results,
verify with direct file reads.
```

### Verification
1. Call `search_codebase` → verify `files_searched`, `files_in_scope`, `coverage_percent` in response
2. Add a binary file to the workspace → verify it's counted in `files_in_scope` but not `files_searched`
3. Add a `.gitignored` file → verify coverage reflects the skip
4. `cargo test -p pathfinder-search` — all search tests pass

---

## R9: Degraded Signal Not Prominent/Consistent

### Problem
Degradation information is scattered across different locations (structured metadata, text prefixes, field names) and not all tools signal it consistently. Agents miss the degraded flag and treat incomplete results as authoritative.

### Evidence
- `read_with_deep_context` — prepends "DEGRADED MODE" text ✅
- `analyze_impact` — prepends degradation warning text ✅
- `get_definition` — NO degraded text prefix ❌
- `search_codebase` — NO degraded text prefix ❌
- `get_repo_map` — NO degraded text prefix ❌
- LSP readiness (`lsp_readiness`) exists in `ReadWithDeepContextMetadata` and `GetDefinitionResponse` but NOT in `AnalyzeImpactMetadata` or `SearchCodebaseResponse`

### Other Occurrences
- `lsp_health` tool is designed to be called at session start but agents rarely do
- The `degraded_tools` field in `LspLanguageHealth` lists degraded tools with severity — but this is only in `lsp_health` output, not in individual tool responses

### Remediation

**Step 1: Standardize degraded text prefix format**

All tools must prepend the same format when degraded:
```
⚠️ DEGRADED ({reason}) — {tool-specific guidance}
```

| Tool | Guidance when degraded |
|------|----------------------|
| `analyze_impact` | "Reference counts are UNRELIABLE. Zero does not mean confirmed no callers." |
| `get_definition` | "Result is from grep heuristic, not LSP. May not be the authoritative definition." |
| `read_with_deep_context` | "Dependencies may be incomplete. LSP was unavailable or still warming." |
| `search_codebase` | "Filter was bypassed; all matches returned regardless of context." |
| `get_repo_map` | "Git filter failed; full map returned instead of changed-files-only." |

**Step 2: Add `lsp_readiness` to all LSP-dependent tool responses**

Files to modify:
- `types.rs` — add `lsp_readiness: Option<String>` to `AnalyzeImpactMetadata`, `SearchCodebaseResponse`
- `navigation.rs` — populate `lsp_readiness` for `analyze_impact`
- `search.rs` — populate `lsp_readiness` for `search_codebase`

**Step 3: Auto-inject LSP status into first tool call response**

File: `crates/pathfinder/src/server/tools/navigation.rs` (or a middleware)

On the FIRST LSP-dependent tool call of a session, automatically append:
```
ℹ️ LSP Status: {status}. {guidance based on status}
```

This eliminates the need for agents to remember to call `lsp_health` separately.

### Verification
1. Call each tool with LSP unavailable → verify consistent ⚠️ DEGRADED prefix
2. Call each tool with LSP ready → verify no degraded prefix
3. Verify `lsp_readiness` field appears in structured_content for all 5 LSP-dependent tools
4. Verify first-call LSP status injection works

---

## R10: Semantic Path Discovery Friction

### Problem
Agents must construct semantic paths manually (`src/file.rs::Struct.method`). Getting the format wrong is the most common failure mode. `did_you_mean` exists but only fires on error, not proactively.

### Evidence
- `symbols.rs:796-849` — `did_you_mean` with Levenshtein distance exists
- `navigation.rs:681-696` — `compute_did_you_mean` only called on `get_definition` failure
- `server.rs:236` — `read_symbol_scope` description says "semantic_path MUST include file path + '::'" but doesn't explain how to discover the path
- Decision documented in `20260424-backlog-deferred-findings.md:90-93`: auto-fuzzy-resolution rejected as too risky

### Other Occurrences
- `read_symbol_scope` returns `SymbolNotFound` with `did_you_mean` from tree-sitter surgeon
- `get_definition` now calls `compute_did_you_mean` (previously returned empty suggestions — see existing remediation plan F3)
- `read_with_deep_context` and `analyze_impact` don't call `did_you_mean` on symbol resolution failure

### Remediation

**Step 1: Add `did_you_mean` to ALL semantic-path tools on failure**

Files to modify:
- `navigation.rs` — `read_with_deep_context_impl` and `analyze_impact_impl`: add `compute_did_you_mean` call before returning `SymbolNotFound`
- Currently only `get_definition_impl` has this. The other two tools just return the raw tree-sitter error.

**Step 2: Add "suggested semantic paths" hint in tool descriptions**

File: `crates/pathfinder/src/server.rs`

Update descriptions for `read_symbol_scope`, `read_with_deep_context`, `analyze_impact`, `get_definition`:
```
If unsure of the exact semantic path, use get_repo_map or read_source_file(detail_level="symbols")
first to discover available paths. Copy-paste semantic paths from those outputs.
```

**Step 3: Return `did_you_mean` in error responses for ALL semantic-path tools**

Ensure that when any tool returns `SymbolNotFound`, the error includes up to 3 `did_you_mean` suggestions. This is already implemented for `read_symbol_scope` (via tree-sitter error) and `get_definition` (via `compute_did_you_mean`). Add the same for `read_with_deep_context` and `analyze_impact`.

### Verification
1. Call `read_with_deep_context` with a slightly wrong semantic path → verify `did_you_mean` suggestions appear
2. Call `analyze_impact` with a slightly wrong semantic path → verify `did_you_mean` suggestions appear
3. Verify suggestion quality — suggestions should be close Levenshtein matches from the same file

---

## R11: `analyze_impact` Name Unclear

### Problem
The name `analyze_impact` reads as generic "impact analysis" — it could mean performance profiling, security auditing, or dependency analysis. Agents didn't instinctively reach for it during refactoring because the name doesn't convey "find callers and callees."

### Evidence
- Agent report: "I wasn't confident it would work well... The name analyze_impact reads more like 'what breaks if I change this function' — but the refactoring question I had was structural ('who constructs LanguageLsp?'), which felt more like a grep problem"
- Description says "Map the blast radius of a symbol: all callers (incoming) and all callees (outgoing)" — this IS clear once read, but the tool name doesn't match
- In the agent's mental model, "who constructs this?" = grep, not "impact analysis"

### Other Occurrences
- `SKILL.md:82` — "ALWAYS run analyze_impact before recommending a refactor" — agents don't follow this because the name doesn't match their mental workflow
- `AGENTS.md:23` — "Blast radius | analyze_impact | Callers + callees via LSP call hierarchy" — the table helps but only if agents read it

### Remediation

**Step 1: Rename `analyze_impact` to `find_callers_callees`**

File: `crates/pathfinder/src/server.rs`

```rust
// Current:
#[tool(name = "analyze_impact", ...)]

// New:
#[tool(name = "find_callers_callees", ...)]
```

**Step 2: Update description to lead with the action**

```rust
description = "Find all callers (incoming) and callees (outgoing) of a symbol — who calls this function and what does it call? Use before refactoring to understand the blast radius. IMPORTANT: semantic_path MUST include file path + '::' (e.g., 'src/mod.rs::func'). LSP-powered with grep fallback. Check `degraded` — when true, empty results may be due to LSP warmup, not genuinely zero callers."
```

**Step 3: Keep `analyze_impact` as an alias for backward compatibility**

Add a second tool registration that delegates to the same implementation:
```rust
#[tool(name = "analyze_impact", description = "Alias for find_callers_callees. Prefer find_callers_callees for clarity.")]
async fn analyze_impact_alias(...) { self.find_callers_callees_impl(params).await }
```

**Step 4: Update all documentation references**

Files to update:
- `SKILL.md` — replace `analyze_impact` with `find_callers_callees`
- `AGENTS.md` — update tool table
- All doc comments referencing `analyze_impact`

### Verification
1. Call `find_callers_callees` → verify same results as old `analyze_impact`
2. Call `analyze_impact` → verify alias works and returns same results
3. Verify new tool description is discoverable by agents

---

## Implementation Priority Order

Based on impact and effort, recommended implementation order:

| Priority | ID | Finding | Effort | Dependencies |
|----------|----|---------|--------|-------------|
| P0 | R9 | Consistent degraded signaling | Medium | None — foundational for trust |
| P1 | R2 | Increase default max_depth to 3 | Low | None |
| P1 | R6 | Add `source_only` detail level | Low | None |
| P1 | R7 | Fix truncated skeleton symbol loss | Low | None |
| P1 | R11 | Rename analyze_impact | Low | None |
| P2 | R3 | Add `include_tests` parameter | Low | R4 (partially — test detection) |
| P2 | R10 | Add `did_you_mean` to all tools | Medium | None |
| P3 | R4 | Add `SymbolKind::Test` + test detection | Medium | None |
| P3 | R8 | Add search coverage metadata | Medium | None |
| P4 | R1 | Full degraded trust system | Medium | R9 (prerequisite) |
| P4 | R5 | Add `find_all_references` tool | Medium | LSP `textDocument/references` support |

---

## Quick Wins (Can be done in a single session)

These changes are self-contained, low-risk, and high-impact:

1. **R2**: Change `default_max_depth() -> u32 { 2 }` to `{ 3 }` — one line
2. **R6**: Add `"source_only"` arm to `detail_level` match — 5 lines
3. **R7**: Fix `render_truncated_file_skeleton` to preserve all symbol kinds — ~30 lines
4. **R11**: Rename `analyze_impact` to `find_callers_callees` + alias — ~20 lines
5. **R10**: Add `compute_did_you_mean` calls to `read_with_deep_context_impl` and `analyze_impact_impl` — ~10 lines each

Total: ~80 lines of code changes for significant agent experience improvement.
