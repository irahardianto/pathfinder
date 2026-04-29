# Project Engineering Standards

> This file consolidates all rules from the minimal configuration. Gemini CLI auto-loads this as persistent context. All rules are always active — language-specific rules auto-apply based on file extensions in scope.

---

## Rule Priority (Conflict Resolution)

### Priority Order (highest to lowest)
1. **Security Mandate** — always wins
2. **Rugged Software Constitution** — foundational defensibility
3. **Code Completion Mandate** + **Logging Mandate** — validation and instrumentation non-negotiable
4. **Testability-First Design** — maintainability enables future
5. **Feature-specific** — language idioms, concurrency, CI/CD. Higher-priority rules win on conflict.
6. **PRD-gated** — feature-flags, gitops-kubernetes. Only when PRD explicitly requires. Confirm before activating.
7. **YAGNI / KISS** — only when no security/reliability/maintainability trade-off

### Common Conflicts

| Conflict | Resolution |
|---|---|
| YAGNI vs Security | Security wins. Input validation always needed. |
| KISS vs Testability | Testability wins. Interfaces enable testing. |
| Perf vs YAGNI | Measure first. Optimize only after profiling. |
| DRY vs Clarity | Clarity wins until 3+ duplications (Rule of Three). |
| Speed vs Logging | Logging wins. Silent failures = enemy. |
| YAGNI vs PRD-gated | PRD wins if explicitly required. |

When in doubt: *"Which choice = more defensible and maintainable?"* If equal -> simpler one (KISS).

---

## Security Mandate

Security = foundational requirement, not feature.

### Universal Principles
1. **Never trust user input** — validate all data from users/APIs/external sources server-side
2. **Deny by default** — explicit permission grants, never assume access
3. **Fail securely** — fail closed (deny) on errors, never open
4. **Defense in depth** — multiple layers, never single control

For implementation details (auth, validation, queries): see Security Principles section below.

---

## Rugged Software Constitution

### Core Philosophy
"I recognize that my code will be attacked." Generate defensibility, not just functionality.

### Commitments
1. **Responsible** — no happy-path-only code. Every input assumed malformed/malicious. Error handling = first-class feature.
2. **Defensible** — validate own state/inputs (Paranoid Programming). Fail securely (closed). Verify assumptions explicitly.
3. **Maintainable** — write for next year's reader, not today's compiler. Clarity over cleverness. Isolate complexity.

### 7 Rugged Habits
1. **Defense-in-depth** — validate at every boundary (API, DB, fn call). Never single-layer protection.
2. **Instrument for awareness** — code signals attacks/failures. Silent failures = enemy #1.
3. **Reduce attack surface** — remove unused code/deps/endpoints. Minimum public interface (Least Privilege).
4. **Design for failure** — assume DB down, network timeout, disk full. Circuit breakers, fallbacks.
5. **Clean up** — own acquired resources, ensure release. No TODO for security holes; fix or document risk.
6. **Verify defenses** — test unhappy paths as rigorously as happy.
7. **Adapt to ecosystem** — battle-tested libraries over custom. Community conventions for maintainability.

### Code Generation Rules
- **Refuse** insecure patterns (SQLi, hardcoded secrets, shell injection) even if asked.
- **Proactively** add validation, error handling, timeout logic even if not requested.
- **Explain** why defensive measures added.

---

## Code Completion Mandate

**Before marking any code task complete, run automated quality checks and remediate all issues.**

### Completion Workflow
1. **Generate** — write code
2. **Validate** — run language-appropriate quality checks
3. **Remediate** — fix all issues
4. **Verify** — re-run checks
5. **Deliver** — mark complete only after all checks pass

Never skip validation "to save time." Validation IS the work.

### Quality Commands per Language

| Language | Section |
|---|---|
| Go | `go vet ./...`, `staticcheck ./...`, `go test ./...` |
| TypeScript / Vue | `npx tsc --noEmit`, `npx eslint .`, `npm test` |
| Flutter / Dart | `dart analyze`, `flutter test` |
| Rust | `cargo clippy -- -D warnings`, `cargo test` |
| Python | `ruff check .`, `mypy .`, `pytest` |

### Failure Protocol
1. Read error output completely
2. Fix identified issues
3. Re-run failing command
4. Do not proceed until all checks pass

Never disable a lint rule or suppress a warning to pass. Fix root cause.

---

## Core Design Principles

### SOLID
- **SRP** — one reason to change per class/module/fn. If description needs "and" -> violates SRP.
- **OCP** — open for extension, closed for modification. Use composition + DI.
- **LSP** — subtypes substitutable for base types without breaking correctness.
- **ISP** — many small focused interfaces over one monolithic.
- **DIP** — depend on abstractions, not concretions. Core principle for testability-first.

### Essential Practices
- **DRY** — single authoritative representation. No duplicate logic/algorithms/business rules.
- **YAGNI** — no speculative features. Build for today, refactor when needs change.
- **KISS** — simple (easy to maintain) over clever. Complexity justified by actual requirements only.
- **Separation of Concerns** — distinct sections, minimal overlap, isolated modules/layers.
- **Composition over Inheritance** — delegation over class hierarchies. Interfaces/traits for polymorphism.
- **Least Astonishment** — follow established conventions. No surprising behavior.

---

## Architectural Patterns — Testability-First Design

### Core Principle
All code independently testable without running full application or external infra.

### Rule 1: I/O Isolation
Abstract ALL I/O behind interfaces/contracts: db queries, HTTP calls, file system, time/randomness, message queues.

### Rule 2: Pure Business Logic
Extract calculations, validations, transformations into pure fns: Input -> Output, no side effects, deterministic.

### Rule 3: Dependency Direction
Dependencies point inward toward business logic.

```
Infrastructure (DB, HTTP, Files, External APIs)
  ↓ depends on
Contracts/Interfaces (abstract ports, no implementation)
  ↓ depends on
Business Logic (pure fns, domain rules, NO infra deps)
```

### Pattern Discovery Protocol (MANDATORY before implementing ANY feature)
1. Search for: `Interface`, `Repository`, `Service`, `Store`, `Mock`
2. Examine 3 existing modules for consistency (db access, pure fns, testing patterns)
3. Document pattern (over 80% consistency required): "Following pattern from [module] modules"
4. If under 80% consistency: STOP and report fragmentation to human.

---

## Code Organization Principles

- Small focused fns (10-50 lines), single purpose
- Cognitive complexity under 10 for most fns
- Clear layer boundaries (presentation, business logic, data access)
- Design for testability from start, avoid tight coupling
- Naming conventions reveal intent without comments

### Module Boundaries
Feature-based organization with clear public interfaces:
- One feature = one directory
- Each module exposes public API (exported fns/classes)
- Internal implementation private
- Cross-module calls only through public API

---

## Error Handling Principles

1. **Never fail silently** — all errors handled explicitly (no empty catch). Catch = do something (log, return, transform, retry).
2. **Fail fast** — detect/report errors early. Validate at boundaries before processing.
3. **Provide context** — error codes, correlation IDs, actionable messages.
4. **Separate concerns** — different handlers for different types.
5. **Resource cleanup** — always clean up on error (close files, release connections, unlock).
6. **No information leakage** — sanitize for external consumption. No stack traces to users.

---

## Logging and Observability Mandate

### Every Operation Entry Point MUST Include Logging

**Operations (mandatory logging):**
API endpoints, background jobs, queue workers, event handlers, scheduled tasks, CLI commands, external service calls, database transactions.

**NOT operations (no direct logging):**
Pure business logic fns, utility/helper fns, data transformations/validators.

### Minimum 3 Log Points
1. **Start** — correlationId, userId, operation name
2. **Success** — duration, result identifiers
3. **Failure** — correlationId, error details, stack trace

### Mandatory Context
`correlationId` (UUID), `operation` (clear name), `duration` (ms), `userId` (when applicable), `error` (full context on failures).

---

## Concurrency and Threading Mandate

### When to Use
- **I/O-bound** — async I/O, event-driven, coroutines for network/file/db waits
- **CPU-bound** — OS threads or thread pools for heavy computation

### When NOT to Use
- Simple synchronous operations
- No measurable performance benefit

Concurrency adds significant complexity (races, deadlocks, debugging). Profile first — only add when measurable benefit exists.

---

## Testing Strategy

### Test Pyramid
- **Unit (70%)** — domain logic in isolation, mocked deps. Fast (under 100ms). Coverage over 85%.
- **Integration (20%)** — adapters against real infra (Testcontainers). Medium (100ms-5s).
- **E2E (10%)** — complete user journeys through all layers. Slow (5-30s).

### TDD: Red-Green-Refactor
1. Red: write failing test
2. Green: minimal code to pass
3. Refactor: clean up, tests stay green

### Test Organization

| Language | Unit | Integration |
|---|---|---|
| TS/JS | `*.spec.ts` | `*.integration.spec.ts` |
| Go | `*_test.go` | `*_integration_test.go` |
| Dart/Flutter | `*_test.dart` in `test/` | `*_integration_test.dart` |
| Python | `test_*.py` | `test_*_integration.py` |
| Rust | `#[cfg(test)] mod tests` inline | `tests/` at crate root |

---

## Security Principles

### OWASP Top 10 Enforcement
- **Broken Access Control** — deny by default. Validate permissions server-side every request.
- **Cryptographic Failures** — TLS 1.2+ everywhere. Encrypt PII/secrets at rest.
- **Injection** — ZERO TOLERANCE for string concatenation in queries. Parameterized queries only.
- **SSRF** — validate user-provided URLs against allowlist.

### Auth
- **Passwords** — Argon2id or Bcrypt (min cost 12). Never plain text.
- **Access Tokens** — short-lived (15-30 min), HS256 or RS256.
- **Refresh Tokens** — long-lived (7-30 days), rotate on use, `HttpOnly; Secure; SameSite=Strict`.
- **Rate Limiting** — strict on public endpoints. 5 attempts / 15 min.
- **RBAC** — permissions mapped to roles, not users. Check at route AND resource level.

### Input Validation
- "All input is evil until proven good."
- Validate against strict schema (Zod/Pydantic) at handler/port boundary.
- Allowlist good characters, never filter bad.

### Secrets
Never commit to git. Use `.env` (local) or Secret Managers (prod — Vault/GSM).

---

## Documentation Principles

### Self-Documenting Code
- Code shows WHAT, comments explain WHY.
- Comment when: complex business logic, non-obvious algorithms, bug workarounds, perf optimizations.

### Documentation Levels
1. **Inline** — explain WHY for complex code
2. **Function/method** — API contract (params, returns, errors)
3. **Module/package** — high-level purpose + usage
4. **README** — setup, usage, examples
5. **Architecture** — system design, component interactions

---

## Code Idioms and Conventions

### Universal Principle
Write idiomatic code for target language. Follow community conventions, not personal preferences.

### Anti-Patterns
- No "Java in Python" or "C in Go"
- No forcing OOP in functional languages
- No avoiding features because "unfamiliar"

### Language-Specific Rules
Load relevant skill when working in that language:
- Go → `.pi/skills/go-idioms/SKILL.md`
- TypeScript → `.pi/skills/typescript-idioms/SKILL.md`
- Vue 3 → `.pi/skills/vue-idioms/SKILL.md`
- Flutter/Dart → `.pi/skills/flutter-idioms/SKILL.md`
- Rust → `.pi/skills/rust-idioms/SKILL.md`
- Python → `.pi/skills/python-idioms/SKILL.md`

---

## Project Structure

**Philosophy:** Organize by FEATURE, not technical layer. Each feature = vertical slice.

**Universal Rule: Context -> Feature -> Layer**

1. **Level 1: Repository Scope** — root contains `apps/` grouping distinct applications.
2. **Level 2: Feature Organization** — vertical business slices. Anti-pattern: top-level technical layers.

### Adapting for Project Types

| Type | Layout |
|---|---|
| Monorepo (default) | `apps/backend/`, `apps/frontend/`, `apps/mobile/` |
| Single backend | Flatten: `cmd/`, `internal/` (Go) or `src/` (Rust) at root |
| Single frontend | Flatten: `src/` at root |
| Single mobile | Flatten: `lib/` at root |

Language-specific layouts in skill files.

---

## Orchestration Dispatch Protocol

> **Applies when:** Using the `/orchestrator` prompt template or manually dispatching sub-agents.

### Agent Routing

| Primitive | Agent Type | Rationale |
|-----------|-----------|-----------|
| SCOUT | Any agent (research mode) | Read-only codebase exploration |
| DESIGN | architect | Architecture decisions, contracts |
| BUILD | Domain-specific engineer | backend/frontend/mobile per MECE domains |
| TEST | test-automation-engineer | E2E, integration test infrastructure |
| REVIEW | qa-analyst + security-engineer + optional ux-reviewer | Quality gates |
| REMEDIATE | Domain-specific engineer | Matches BUILD agent for the domain |
| VERIFY | qa-analyst | Full test suite, lint, type check, build |
| DOCUMENT | technical-writer | Docs, API docs, changelogs |

### Prompt Templates

User-invoked workflow templates live in `.pi/prompts/`. Invoke with `/name` in the pi editor.

| Template | Command | Purpose |
|---|---|---|
| `orchestrator.md` | `/orchestrator` | Full build-feature state machine (chains all phases) |
| `1-research.md` | `/1-research` | Research phase (standalone or part of orchestrator) |
| `2-implement.md` | `/2-implement` | Implement phase (TDD cycle) |
| `3-integrate.md` | `/3-integrate` | Integrate phase (Testcontainers) |
| `4-verify.md` | `/4-verify` | Verify phase (lint, test, build, coverage) |
| `5-commit.md` | `/5-commit` | Ship phase (conventional commit) |
| `audit.md` | `/audit` | Code audit with cross-boundary analysis |
| `quick-fix.md` | `/quick-fix` | Hotfix/small bug fix (skip research) |
| `refactor.md` | `/refactor` | Safe code restructuring (incremental, behavior-preserving) |
| `perf-optimize.md` | `/perf-optimize` | Profile-driven performance optimization |
| `e2e-test.md` | `/e2e-test` | E2E testing with Playwright MCP |

### MCP Tools — Known Limitation
**Sub-agents may NOT have access to MCP tools.** MCP tools are only available in the main session. Do NOT instruct sub-agents to use MCP tools — they will fail. For tasks requiring MCP tools, execute those in the main session.

---

## Pathfinder Tool Routing

Canonical location for Pathfinder tool routing, addressing rules, and fallback details.
`APPEND_SYSTEM.md` §1 points here for bootstrap. Full tool chains and workflows: `.pi/skills/pathfinder-workflow/SKILL.md`. Prompt templates for task workflows (orchestrator, audit, refactor, quick-fix, etc.): `.pi/prompts/`.

### Pre-Flight Check

Before using Pathfinder tools, confirm they're available:
```
mcp({ server: "pathfinder" })  // If tools listed → available. If error → use built-in.
```

Do this once per session, not per task.

### Core Principle
Pathfinder operates at the **semantic level** (symbols, functions, classes). Built-in tools operate at **text level**. **Always prefer semantic tools for source code.**

### Tool Preference

| Action | Prefer (Pathfinder) | Instead of (Built-in) | Notes |
|---|---|---|---|
| Explore project structure | `get_repo_map` | directory listing | One call returns skeleton + version hashes |
| Search for code patterns | `search_codebase` | grep | Returns semantic paths + version hashes |
| Read a function or class | `read_symbol_scope` | read file | Exact symbol extraction, no context waste |
| Read function + dependencies | `read_with_deep_context` | Multiple reads | Source + callee signatures in one call. LSP-powered |
| Jump to a definition | `get_definition` | grep (approximation) | LSP-powered, follows imports/re-exports. Has grep fallback when degraded |
| Assess refactoring impact | `analyze_impact` | No equivalent | Maps callers + callees with BFS. LSP-powered |
| Edit a function body | `replace_body` | edit file | Semantic addressing + auto-indent + LSP validation |
| Edit entire declaration | `replace_full` | edit file | Includes signature/decorators/doc comments |
| Batch-edit multiple symbols | `replace_batch` | multiple edits | Atomic single-call with single OCC guard |
| Add code before/after symbol | `insert_before` / `insert_after` | edit file | Semantic anchor point + auto-spacing |
| Delete a function or class | `delete_symbol` | edit file | Handles decorators, doc comments, whitespace |
| Pre-check a risky edit | `validate_only` | no equivalent | Dry-run with LSP diagnostics |
| Create a new file | `create_file` | write file | Returns version_hash for subsequent edits |
| Edit config files | `write_file` | edit file | OCC-protected, supports search-and-replace |

### LSP-Dependent Tools and Degraded Mode

Three Pathfinder tools depend on LSP (Language Server Protocol) for precise results: `get_definition`, `analyze_impact`, and `read_with_deep_context`. When the LSP is unavailable or still indexing, these tools degrade gracefully:

- **`degraded: false`** — LSP confirmed the result. Trust it fully.
- **`degraded: true`** — Result is a best-effort approximation. Check `degraded_reason` for specifics:
  - `no_lsp` — No language server for this language. Install it or accept limited results.
  - `lsp_warmup_*` — LSP is still indexing. Empty results are UNVERIFIED (there may be callers/definitions the LSP hasn't found yet). Re-run after indexing completes.
  - `grep_fallback_*` — `get_definition` fell back to ripgrep search. Verify with `read_source_file`.
  - `lsp_error` — LSP returned an error. Results are from Tree-sitter/grep only.

**Key rule:** When `degraded: true`, do NOT treat empty results as confirmed-zero. Re-run the tool after a few seconds if the LSP is warming up.

### Addressing Rules
Semantic paths MUST include file path and `::`. Example: `src/main.rs::MyClass.my_function`

### Graceful Fallback
If Pathfinder unavailable, fall back to built-in tools transparently. Do not block.
