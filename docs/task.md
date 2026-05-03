# Pathfinder Task Tracker

## Legend
- `[ ]` = Not started
- `[/]` = In progress (Phase 1: Research)
- `[~]` = In progress (Phase 2: Implement)
- `[*]` = In progress (Phase 3: Integrate)
- `[x]` = Completed
- `[-]` = Blocked

---

## 2026-05-01: Cross-Language LSP Reliability — Group C (Observability)

### PATCH-005: Surface Per-Language Capabilities in lsp_health
- [x] Phase 1: Research log created (`docs/research_logs/group-c-observability-20260501.md`)
- [x] Phase 2: Implement
  - [x] Extend LspLanguageStatus with capability fields
  - [x] Extend LspLanguageHealth with strategy and capability fields
  - [x] Update validation_status_from_parts() to populate capabilities
  - [x] Wire up in lsp_health_impl()
  - [x] Write tests
- [x] Phase 3: Integrate
- [x] Phase 4: Verify

### PATCH-006: Add Probe-Based Readiness to lsp_health
- [x] Phase 1: Research log created (same as PATCH-005)
- [x] Phase 2: Implement
  - [x] Add probe_verified field to LspLanguageHealth
  - [x] Add probe_language_readiness() helper
  - [x] Add find_probe_file() helper
  - [x] Add parse_uptime_to_seconds() helper
  - [x] Wire up probe logic in lsp_health_impl()
  - [x] Write tests
- [x] Phase 3: Integrate
- [x] Phase 4: Verify

---

## Prior Work (Completed)

### Group A: Foundation (PATCH-001, PATCH-002)
- [x] PATCH-001: Diagnostics Strategy Abstraction
- [x] PATCH-002: Implement Push Diagnostics Listener

### Group B: Vue (PATCH-003, PATCH-004)
- [x] PATCH-003: TS Plugin System
- [x] PATCH-004: Add @vue/typescript-plugin Auto-Detection

---

---

## 2026-05-01: Additional Patches (Not Yet Implemented)

### Group D: Provisioning
- [x] PATCH-008: Surface Install Guidance for Missing LSPs
- [x] PATCH-009: End-to-End Python LSP Verification Test
  - Fixed pyright detection to use pyright-langserver (actual LSP binary)
  - Added Python integration test in lsp_client_integration.rs
  - Added Python name_column test in symbols.rs

### Group E: Polish
- [x] PATCH-010: Enrich lsp_health with Diagnostics Strategy Info
- [x] PATCH-011: Document Plugin Detection and Configuration

---

## Completed in This Session

### All Groups (Committed)
- [x] Group A: Foundation (PATCH-001, PATCH-002) - Diagnostics strategy, push diagnostics
- [x] Group B: Vue (PATCH-003, PATCH-004) - TS plugin system, @vue/typescript-plugin auto-detection
- [x] Group C: Observability (PATCH-005, PATCH-006) - Per-language capabilities, probe-based readiness
- [x] PATCH-007: Python LSP Detection Completeness - pyright -> pylsp -> ruff-lsp -> jedi-language-server fallback chain

---

## 2026-05-02: LSP-HEALTH-001 Reliability Hardening

### Task 1: Decouple Navigation Readiness from Indexing Completion (P0)
- [x] Add `navigation_ready` field to `LspLanguageStatus`
- [x] Update `validation_status_from_parts` to set navigation_ready from supports_definition
- [x] Two-phase readiness in `lsp_health_impl` (navigation_ready gates "ready", indexing_complete is additional signal)
- [x] Add `indexing_status` to `LspLanguageHealth` response
- [x] Add navigation_ready tests (3 in mod.rs, 2 in navigation.rs)

### Task 3: Improve Probe Reliability (P0)
- [x] Recursive depth-4 monorepo scan fallback in `find_probe_file`
- [x] Probe cache with TTL: positive indefinite, negative 60s expiry
- [x] `ProbeCacheEntry` struct with `is_valid()` TTL check
- [x] `ProbeAction` tri-state: UseCachedReady / SkipProbe / Probe
- [x] Tests for TTL cache behavior (4 new tests)

### Task 4+5: Cache Isolation and Accurate Warnings (P1)
- [x] gopls: GOCACHE + GOMODCACHE isolation
- [x] tsserver: TMPDIR isolation
- [x] Python: PYTHONPYCACHEPREFIX isolation
- [x] Language-specific isolation descriptions in concurrent LSP warning
- [x] Auto-add `.pathfinder/` to `.gitignore` via `ensure_pathfinder_in_gitignore()` (4 tests)

### Task 6: Progress Watcher Timeout Fallback (P2)
- [x] 30s `INDEXING_FALLBACK_TIMEOUT_SECS` sets `indexing_complete = true` if no progress received

### Documentation
- [x] Updated `docs/LSP-ARCHITECTURE.md` with TTL probe cache docs, auto-gitignore, timeout location
- [x] Updated `README.md` with concurrent LSP handling note and roadmap items
- [x] Added LSP-HEALTH-001 spec to `docs/requirements/patches/`

---
## 2026-05-02: GAP-003: Fix dedent_then_reindent for Nested Blocks
- [x] Phase 1: Research log created (`docs/research_logs/GAP-003-indentation-nested.md`)
- [x] Phase 2: Implement
  - [x] Add `anchor_to_column_zero()` helper function
  - [x] Add `dedent_by()` helper function
  - [x] Update `normalize_for_body_replace()` to call `anchor_to_column_zero()`
  - [x] Add tests for nested if-else indentation
  - [x] Add tests for relative indent preservation
- [x] Phase 3: Integrate (self-contained normalization change, no external integration needed)
- [x] Phase 4: Verify (all 137 tests in pathfinder-mcp-common pass)

### GAP-004: Append version_hash to Text Output of Read Tools
- [x] Phase 1: Research log created (`docs/research_logs/GAP-004-version-hash-text.md`)
- [x] Phase 2: Implement
  - [x] Update `read_source_file_impl()` to append version_hash to text output
  - [x] Update `read_symbol_scope_impl()` to append version_hash to text output
  - [x] Add tests for version_hash in text output
- [x] Phase 3: Integrate (self-contained output format change, no external integration needed)
- [x] Phase 4: Verify (all 202 tests in pathfinder-mcp pass)

---

## 2026-05-03: GAP-005 through GAP-008

### GAP-005: Fix delete_symbol for TypeScript Class Methods
- [x] Phase 1: Research (`docs/requirements/patches/20260502/GAP-005-delete-symbol-ts.md`)
- [x] Phase 2: Implement
  - [x] Replace raw `rg -l -w` with scout search in delete_symbol_impl
  - [x] Existing test_delete_symbol_cross_file_reference_warning updated for new scout-based check
- [x] Phase 3: Integrate (scout already used in production, no new adapter)
- [x] Phase 4: Verify (205 tests pass)

### GAP-006: Warn When insert_into Targets a Rust Struct
- [x] Phase 1: Research (`docs/requirements/patches/20260502/GAP-006-insert-into-rust-warning.md`)
- [x] Phase 2: Implement
  - [x] Add `warning` field to EditResponse
  - [x] Add `warning` field to FinalizeEditParams
  - [x] Add Rust struct detection in insert_into_impl
  - [x] Add test: warning appears for Rust struct target
  - [x] Add test: no warning for TypeScript class
  - [x] Add test: no warning for Rust impl block
- [x] Phase 3: Integrate (response field addition, backward-compatible)
- [x] Phase 4: Verify (205 tests pass)

### GAP-007: Add Offset Pagination to search_codebase
- [x] Phase 1: Research (`docs/requirements/patches/20260502/GAP-007-search-pagination.md`)
- [x] Phase 2: Implement
  - [x] Add `offset` field to SearchParams (pathfinder-search)
  - [x] Add global skip counter in MatchCollector (shared across files)
  - [x] Add `offset` to SearchCodebaseParams MCP handler
  - [x] Add `next_offset` hint in truncated response
  - [x] Add tests: pagination, beyond-results, truncation hint
- [x] Phase 3: Integrate (search layer change, no external adapters)
- [x] Phase 4: Verify (35 search tests, 205 mcp tests pass)

### GAP-008: Improve Error Responses with Remediation Hints
- [x] Phase 1: Research (`docs/requirements/patches/20260502/GAP-008-error-responses.md`)
- [x] Phase 2: Implement
  - [x] Add LspError hint with timeout/crash/generic branches
  - [x] Add LspTimeout hint with workaround text
  - [x] Add NoLspAvailable hint mentioning tree-sitter tools
  - [x] Add 5 tests for all new hints
- [x] Phase 3: Integrate (error message change only, no structural changes)
- [x] Phase 4: Verify (142 common tests pass)
