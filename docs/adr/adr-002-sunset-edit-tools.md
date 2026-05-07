# ADR-002: Sunset Edit Tools — Pathfinder Becomes a Read-Only Discovery Engine

**Status:** Accepted  
**Date:** 2026-05-07  
**Decision Makers:** Project maintainer (Ira Hardianto) + multi-agent analysis consensus  
**Supersedes:** PRD v4.6 edit-tool roadmap  
**Scope:** Permanent removal of all mutation/edit capabilities from Pathfinder

---

## Context

Pathfinder v0.6 shipped with two distinct tool surfaces:

1. **Navigation & Discovery** (9 tools): `get_repo_map`, `search_codebase`, `read_symbol_scope`, `read_source_file`, `read_with_deep_context`, `get_definition`, `analyze_impact`, `lsp_health`, `read_file`
2. **Edit & Mutation** (10 tools): `replace_body`, `replace_full`, `replace_batch`, `insert_before`, `insert_after`, `delete_symbol`, `validate_only`, `create_file`, `delete_file`, `write_file`

The edit surface accounted for the majority of Pathfinder's complexity:

| Component | Edit-Only Code |
|---|---|
| Shadow Editor validation pipeline | ~800 lines — LSP-based `didOpen → didChange → pullDiagnostics` cycle with multiset diagnostic diffing |
| OCC (Optimistic Concurrency Control) | ~200 lines — version hash chain management across all edit tools |
| `Lawyer` trait (LSP lifecycle) | 7 of 16 methods exclusively for edit validation (`did_open`, `did_change`, `did_close`, `pull_diagnostics`, `collect_diagnostics`, `pull_workspace_diagnostics`, `range_formatting`) |
| `Surgeon` trait (AST mutation) | 5 of 11 methods exclusively for edits (`find_body_bytes`, `find_full_bytes`, `replace_bytes_preserving_tree`, `apply_batch_edits`, `find_insertion_point`) |
| Indentation normalization | 2 modules (`indent.rs`, `normalize.rs`) — ~400 lines of language-specific indent heuristics |
| Error taxonomy | 8 of 25 `PathfinderError` variants solely for edit failures |

This complexity created three strategic problems:

### 1. Multi-Language Scaling Wall

Every new language Pathfinder supports requires:
- Tree-sitter grammar integration (navigation — **unavoidable**)
- Symbol body/full range extraction (edit — **fragile per-grammar**)
- Indentation normalization rules (edit — **language-specific**)
- LSP diagnostics integration (edit — **server-specific behavior**)

Navigation scales linearly with new languages. Edit tooling scales **quadratically** — each grammar × each LSP server has unique edge cases. Adding Vue SFC support already required a custom `vue_zones.rs` preprocessor and text-targeting fallback for `<template>` / `<style>` zones.

### 2. Agents Already Have Edit Tools

Every major AI agent framework (Cursor, Windsurf, Cline, Gemini CLI) ships with built-in file editing primitives. These are battle-tested, framework-native, and maintained by the host platform. Pathfinder's edit tools were a parallel implementation that:
- Competed with (rather than complemented) the host's editor
- Required OCC hash management that agents frequently got wrong
- Introduced validation failures that confused agents into retry loops

### 3. Discovery Is the Actual Moat

Empirical testing showed that agents using Pathfinder's navigation tools outperform agents with only built-in tools by a significant margin:

- **3–5 tool calls** to understand a 100K LOC codebase (with Pathfinder navigation)
- **20–30 tool calls** for the same task (with built-in `grep` + `read`)

This advantage comes from AST-aware semantic addressing, LSP-powered cross-file navigation, and call hierarchy analysis — capabilities that no built-in agent tool provides. The edit tools, by contrast, offered marginal improvement over built-in editors while carrying enormous maintenance cost.

## Decision

**Permanently remove all edit/mutation capabilities from Pathfinder. The project becomes a specialized, read-only semantic discovery and navigation engine.**

### What Was Removed

| Category | Removed Items |
|---|---|
| **MCP Tools** | `replace_body`, `replace_full`, `replace_batch`, `insert_before`, `insert_after`, `delete_symbol`, `validate_only`, `create_file`, `delete_file`, `write_file` |
| **Validation Pipeline** | Shadow Editor lifecycle (`didOpen` → `didChange` → `pullDiagnostics` → diagnostic diffing) |
| **OCC Infrastructure** | Version hash chain enforcement, `base_version` parameter validation, `VERSION_MISMATCH` error flow |
| **Lawyer Trait Methods** | `did_open`, `did_change`, `did_close`, `pull_diagnostics`, `collect_diagnostics`, `pull_workspace_diagnostics`, `range_formatting` |
| **Surgeon Trait Methods** | `find_body_bytes`, `find_full_bytes`, `replace_bytes_preserving_tree`, `apply_batch_edits`, `find_insertion_point` |
| **Modules** | `indent.rs`, `normalize.rs` (indentation normalization) |
| **Dead Code** | `count_parse_errors()`, `count_error_nodes_recursive()` (replace_batch structural validation) |
| **Documentation** | All edit-tool workflows, OCC sections, validation override guidance from AGENTS.md, SKILL.md, README.md |

### What Remains (The 9-Tool Discovery Surface)

| Tool | Purpose | Engine |
|---|---|---|
| `get_repo_map` | Project skeleton with semantic paths | Tree-sitter |
| `search_codebase` | AST-filtered text search with semantic context | ripgrep + Tree-sitter |
| `read_symbol_scope` | Extract one symbol by semantic path | Tree-sitter |
| `read_source_file` | Full file + AST symbol hierarchy | Tree-sitter |
| `read_with_deep_context` | Symbol + dependency signatures | Tree-sitter + LSP |
| `get_definition` | Jump to definition | LSP + ripgrep fallback |
| `analyze_impact` | Caller/callee mapping (blast radius) | LSP + grep fallback |
| `lsp_health` | LSP status and diagnostics | LSP client |
| `read_file` | Raw file read for config files | Filesystem |

## Rationale

### Focus Multiplier

By removing the edit surface, the maintenance budget is concentrated entirely on making navigation excellent:
- **Deeper LSP integration** — invest in better warmup detection, richer degraded-mode fallbacks
- **More languages** — adding a language now requires only Tree-sitter grammar + LSP config, not the full edit stack
- **Better telemetry** — instrument `analyze_impact` and `search_codebase` for semantic confidence scoring

### Reliability Guarantee

The remaining 9 tools are read-only. They cannot corrupt files, lose data, or create merge conflicts. This makes Pathfinder safe to run in any environment without sandboxing concerns beyond path traversal (which is enforced by the existing `SandboxPolicy`).

### Leaner Context Budget

Agent documentation was reduced by 55%+ as part of this transition:
- Tool descriptions: ~6,800 chars → ~2,920 chars (57% reduction)
- Skill files: 385 lines → 170 lines (56% reduction)
- AGENTS.md routing: 48 lines → 33 lines (31% reduction)

Less documentation means more context window available for actual code.

## Consequences

### Positive

- **Codebase complexity drops significantly** — fewer traits, fewer error variants, no validation pipeline
- **New language support is tractable** — weeks instead of months per language
- **Agent integration is simpler** — no OCC hash management, no validation override decisions
- **Zero write-side bugs** — eliminated an entire class of potential data loss

### Negative

- **Agents must use built-in tools for edits** — no more semantic addressing for mutations
- **No atomic multi-symbol edits** — `replace_batch` had no built-in equivalent (agents use sequential edits)
- **No pre-edit validation** — `validate_only` dry-run capability is lost (agents rely on post-edit test runs)

### Neutral

- **Legacy error variants fully removed** — `VersionMismatch`, `FileAlreadyExists`, `BatchStructuralCorruption`, `ValidationFailed`, `InvalidTarget`, `MatchNotFound`, `AmbiguousMatch`, `TextNotFoundInContext` were all excised alongside the tools that produced them. The error taxonomy now contains only navigation-relevant variants.
- **`version_hash` fields remain** — still useful as content fingerprints for change detection, even without OCC

## Revisit Conditions

This decision should be revisited if ANY of these conditions become true:

1. **Agent frameworks drop built-in editors** — if the host editing primitives are removed or severely degraded
2. **Semantic edits become a competitive requirement** — if competing tools offer AST-aware editing that demonstrably outperforms built-in editors
3. **LSP write capabilities mature** — if LSP introduces standardized code action / workspace edit primitives that make edit tooling trivial to implement

## References

- PRD v4.6 (superseded edit-tool roadmap): `docs/requirements/`
- Audit 0026 (codebase health baseline): `docs/audits/`
- Ergonomics Addendum: `docs/requirements/patches/PATHFINDER_ERGONOMICS_ADDENDUM_2026-05-04.md`
- Agent adoption audit: `docs/audits/pathfinder-mcp-adoption-audit.md`
