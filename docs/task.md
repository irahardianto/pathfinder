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
- [ ] PATCH-010: Enrich lsp_health with Diagnostics Strategy Info
- [ ] PATCH-011: Document Plugin Detection and Configuration

---

## Completed in This Session

### All Groups (Committed)
- [x] Group A: Foundation (PATCH-001, PATCH-002) - Diagnostics strategy, push diagnostics
- [x] Group B: Vue (PATCH-003, PATCH-004) - TS plugin system, @vue/typescript-plugin auto-detection
- [x] Group C: Observability (PATCH-005, PATCH-006) - Per-language capabilities, probe-based readiness
- [x] PATCH-007: Python LSP Detection Completeness - pyright -> pylsp -> ruff-lsp -> jedi-language-server fallback chain
