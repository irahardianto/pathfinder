# PATCH-011: Document Plugin Detection and Configuration

## Group: E (Polish) — Documentation
## Depends on: PATCH-003, PATCH-004

## Objective

Document the TypeScript plugin system, Vue integration, and the overall
cross-language LSP architecture. This ensures future contributors understand
the agnostic channel design and how to add new framework plugins.

## Severity: LOW — documentation only

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `docs/research_logs/epic4_lsp.md` | Update with cross-language architecture | Document agnostic channel design |
| 2 | `docs/requirements/patches/20260501/ARCHITECTURE.md` | New file | LSP integration architecture reference |

## Step 1: Create Architecture Reference

**File:** `docs/requirements/patches/20260501/ARCHITECTURE.md`

Document:

1. Agnostic Channel Design
   - Lawyer trait as the integration boundary
   - LspClient as the production implementation
   - Per-language process management (spawn, lifecycle, idle timeout)
   - Language routing via file extension -> language_id

2. Diagnostics Strategy
   - Pull vs Push model
   - How strategy is detected from server capabilities
   - How validation pipeline routes based on strategy
   - Fallback chain: Pull -> Push -> Skip

3. TypeScript Plugin System
   - Plugin detection (npm, pnpm, config override)
   - Plugin configuration in initialize params
   - Supported plugins: @vue/typescript-plugin
   - How to add new plugins (Svelte, etc.)

4. Detection and Provisioning
   - Marker file scanning (Cargo.toml, go.mod, tsconfig.json, pyproject.toml)
   - Binary resolution via `which`
   - Fallback chain for Python (pyright -> pylsp -> ruff-lsp -> jedi)
   - Config overrides in .pathfinder.toml

5. Future Plugin Guide
   - Step-by-step: adding a new framework plugin for the TS LSP
   - Step-by-step: adding a completely new language
   - What to test: detection, spawn, capabilities, navigation, validation

## Step 2: Update Existing LSP Research Log

**File:** `docs/research_logs/epic4_lsp.md`

Add section at the top noting:
- Rust remediation is complete and language-agnostic
- The column-1 fix, empty probe, and didOpen lifecycle apply to all languages
- New languages need: detect.rs entry + tree-sitter grammar (already have both for Python)
- Diagnostics strategy is the remaining gap (pull vs push)

## EXCLUSIONS

- No production code changes
- No test changes

## Verification

```bash
# Files exist and are well-formed markdown
test -f docs/requirements/patches/20260501/ARCHITECTURE.md
head -5 docs/requirements/patches/20260501/ARCHITECTURE.md
```

## Expected Impact

- Future contributors understand the architecture without reading all the code
- Adding new languages or plugins has a clear guide
- The agnostic channel design is documented as intentional architecture
