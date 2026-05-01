# Cross-Language LSP Reliability Plan — 2026-05-01

Derived from: Rust LSP remediation retrospective + Go/TypeScript/Python gap analysis.
Source audit: "Pathfinder LSP Reliability Audit: Rust Remediation Learnings -> Go/TypeScript/Python Gap Analysis"
Architectural validation: Full code review of pathfinder-lsp, navigation, validation, and detect modules.

## Key Finding

The Rust fixes (PATCH-001 through PATCH-003 from 20260430) are ALREADY language-agnostic.
All three navigation tools (get_definition, analyze_impact, read_with_deep_context) share
the same Lawyer trait, same LspClient instance, same name_column positioning, same didOpen
lifecycle, and same empty-hierarchy probe logic. There is NO "split-path architecture."

The Go/TS failures stem from three distinct root causes:

1. **Diagnostics protocol mismatch** — validation uses pull diagnostics (LSP 3.17).
   gopls and typescript-language-server don't support it. They use push diagnostics
   (textDocument/publishDiagnostics). All validate_only and edit validation is skipped.

2. **TS LSP lacks framework plugins** — typescript-language-server doesn't understand
   .vue files natively. Without @vue/typescript-plugin, it returns errors for Vue SFCs.

3. **Python simply not installed** — pyright not on $PATH. Detection code is present
   and correct; no LSP to connect to.

These are NOT the same bugs as the Rust column-1 issue. They are protocol and
provisioning gaps in the agnostic channel, not missing per-language code paths.

## Architecture Decision: TS Plugin Path for Vue

Decision: Use @vue/typescript-plugin over Volar.

Rationale:
- Single TS LSP process handles .ts, .tsx, .vue, .jsx, .svelte (via plugins)
- Adding framework support = adding a plugin, not spawning a new LSP
- Lower resource usage (one process vs two)
- Simpler detection logic (one "typescript" entry, plugin config varies)
- Industry standard: VS Code, WebStorm, Neovim all use this approach
- Future: React (.tsx) works out of the box, Svelte via svelte2tsx plugin

The plugin is configured in the LSP `initialize` params via `initializationOptions`,
not in detect.rs spawning logic.

## Delivery Groups

| Group | Patches | Theme | Risk | Est. Effort | Impact |
|-------|---------|-------|------|-------------|--------|
| A — Foundation | PATCH-001, PATCH-002 | Diagnostics strategy abstraction + push diagnostics | Medium | 4h | Unblocks validate_only for Go/TS |
| B — Vue | PATCH-003, PATCH-004 | TS plugin system + @vue/typescript-plugin | Medium | 3h | Unblocks Vue LSP navigation |
| C — Observability | PATCH-005, PATCH-006 | Per-language capability surface + probe-based health | Low | 2h | Better agent self-adaptation |
| D — Provisioning | PATCH-007, PATCH-008, PATCH-009 | Python detection, install guidance, e2e verification | Low | 3h | Enables Python LSP |
| E — Polish | PATCH-010, PATCH-011 | lsp_health response quality + plugin detection docs | Low | 1.5h | Completes the feature set |

**Total estimated effort: ~13.5 hours across 5 delivery groups.**

## Dependency Graph

```
A-001 ──> A-002
                  (independent)
B-003 ──> B-004
                  (independent)
C-005 ──> C-006
                  (independent)
D-007 ──> D-008 ──> D-009
                  (independent)
E-010 ──> E-011
```

Groups A-E are independent. Within each group, patches must be applied in order.
Group D depends on having a Python project to test against.
Group B depends on Group A for validation to work end-to-end (but not for navigation).

## Patch Index

| Patch | Title | Group | Files Changed | Risk | Effort |
|-------|-------|-------|---------------|------|--------|
| [PATCH-001](./PATCH-001-diagnostics-strategy.md) | Diagnostics strategy enum in LspClient | A | 4 files | Medium | 2h |
| [PATCH-002](./PATCH-002-push-diagnostics.md) | Implement push diagnostics listener | A | 3 files | Medium | 2h |
| [PATCH-003](./PATCH-003-ts-plugin-system.md) | TS plugin configuration in initialize params | B | 3 files | Medium | 1.5h |
| [PATCH-004](./PATCH-004-vue-plugin.md) | Add @vue/typescript-plugin auto-detection | B | 2 files | Medium | 1.5h |
| [PATCH-005](./PATCH-005-capability-surface.md) | Surface per-language capabilities in lsp_health | C | 2 files | Low | 1h |
| [PATCH-006](./PATCH-006-probe-health.md) | Add probe-based readiness to lsp_health | C | 2 files | Low | 1h |
| [PATCH-007](./PATCH-007-python-detection.md) | Verify Python LSP detection completeness | D | 1 file | Low | 30 min |
| [PATCH-008](./PATCH-008-install-guidance.md) | Surface install guidance for missing LSPs | D | 3 files | Low | 1.5h |
| [PATCH-009](./PATCH-009-python-e2e.md) | End-to-end Python LSP verification test | D | 2 files | Low | 1h |
| [PATCH-010](./PATCH-010-health-response.md) | Enrich lsp_health with diagnostics strategy info | E | 2 files | Low | 45 min |
| [PATCH-011](./PATCH-011-plugin-detection-docs.md) | Document plugin detection and configuration | E | 2 files | Low | 45 min |

## Expected Impact Matrix (After Full Remediation)

| Tool / Feature | Rust | Go | TypeScript | Vue | Python |
|---------------|------|-----|------------|-----|--------|
| get_definition | Working (no change) | Working (no change) | Working (no change) | NEW: works | NEW: works |
| analyze_impact | Working (no change) | Working (no change) | Working (no change) | NEW: works | NEW: works |
| read_with_deep_context | Working (no change) | Working (no change) | Working (no change) | NEW: works | NEW: works |
| validate_only | Working (pull diag) | NEW: push diag | NEW: push diag | NEW: push diag | NEW: pull diag |
| edit validation | Working (pull diag) | NEW: push diag | NEW: push diag | NEW: push diag | NEW: pull diag |
| lsp_health accuracy | Good | Good | Good | Good | Good |
| Install guidance | N/A | N/A | N/A | NEW | NEW |

## Relationship to Prior Work

| Prior Doc | Relationship |
|-----------|-------------|
| PATCH-001..003 (20260430) | COMPLETED. Column-1 + name_column + empty probe. All language-agnostic. |
| PATCH-004..011 (20260430) | COMPLETED. Search, validation honesty, grep fallback. No overlap. |
| PATCH-008 (20260430) | Validation honesty. This plan EXTENDS it by adding push diagnostics. |
| PATCH-012 (20260430) | lsp_health tool. This plan EXTENDS it with per-language capabilities. |
| FEATURE-001 | New tools epic. Independent. |
| PRD v5.1 | LSP zero-config (S6). This plan implements the cross-language reliability. |

## Exclusions

1. **Svelte plugin** — Follows same pattern as Vue plugin. Deferred to avoid scope creep.
   Document as a follow-up task in PATCH-011.
2. **gopls specific initialization params** — gopls works with standard initialize.
   No special handling needed beyond push diagnostics.
3. **Multiple LSP instances per language** — e.g., separate Volar + tsserver for Vue.
   The plugin approach makes this unnecessary.
4. **Bundling LSP binaries** — pyright/typescript-language-server as optional deps.
   Out of scope. Install guidance (PATCH-008) is sufficient.
5. **OCC version hash consistency** — The original report claimed inconsistency but
   provided no evidence. The code uses `VersionHash::compute(&bytes)` consistently.
   No action needed unless a concrete reproduction is found.

## Global Verification (run after ALL patches in a group)

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## How to Use These Documents

Each PATCH is a self-contained, testable delivery. Execute in dependency order within
each delivery group. Groups are independent of each other.

1. OBJECTIVE — what and why (1 paragraph)
2. SCOPE — exact files and functions (table)
3. CURRENT CODE — verbatim snippets where relevant
4. TARGET CODE — exact replacement or new code
5. EXCLUSIONS — what NOT to touch
6. VERIFICATION — commands to confirm success
7. TESTS — new tests to add
