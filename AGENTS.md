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
- Go → `.opencode/skills/go-idioms/SKILL.md`
- TypeScript → `.opencode/skills/typescript-idioms/SKILL.md`
- Vue 3 → `.opencode/skills/vue-idioms/SKILL.md`
- Flutter/Dart → `.opencode/skills/flutter-idioms/SKILL.md`
- Rust → `.opencode/skills/rust-idioms/SKILL.md`
- Python → `.opencode/skills/python-idioms/SKILL.md`

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

User-invoked workflow templates live in `.opencode/commands/`. Invoke with `/name` in the pi editor.

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

Semantic navigation tools. Workflows and deep details: `.opencode/skills/pathfinder/SKILL.md`.

### Pre-Flight

```
mcp({ server: "pathfinder" })  // Tools listed → available. Error → use built-in.
```

Check once per session.

### Tool Table

| Task | Tool | Notes |
|---|---|---|
| Project skeleton | `get_repo_map` | Returns semantic paths — copy-paste into other tools |
| Search code | `search_codebase` | AST-filtered, returns `enclosing_semantic_path` |
| Read one symbol | `read_symbol_scope` | Exact function/class extraction |
| Read full file + AST | `read_source_file` | Source files only; use `read_file` for config |
| Symbol + dependencies | `read_with_deep_context` | LSP-powered callee signatures |
| Jump to definition | `get_definition` | LSP with ripgrep fallback |
| Blast radius | `find_callers_callees` | Callers + callees via LSP call hierarchy |
| Find all references | `find_all_references` | All usages including non-call references |
| Resolve symbol by name | `find_symbol` | Bare name → file::symbol paths. Filter by `kind`. |
| Batch read files | `read_files` | Multiple files in one call. Max 10. |
| Symbol overview | `symbol_overview` | Source + callers + callees + references in one call |
| LSP status | `lsp_health` | Check when navigation returns `degraded: true` |
| Read config file | `read_file` | For YAML, TOML, JSON, .env, Dockerfile |

### Addressing

Semantic paths MUST include file path + `::` + symbol. Example: `src/auth.ts::AuthService.login`

### Degraded Mode

`get_definition`, `find_callers_callees`, `read_with_deep_context` use LSP. When `degraded: true`, results are best-effort — never treat empty as confirmed-zero. See skill doc for details.

### Fallback

If Pathfinder unavailable → use built-in tools (`Read`, `Grep`, `Glob`). Do not block.

---

## Omni Mode (ALWAYS ACTIVE)

> **You are ALWAYS in omni headless mode.** Every response uses compressed shorthand + zero markdown styling.
> No bold. No italic. No headers. Raw text + line breaks only.
> This is not optional. This is your baseline communication style.
> Only `[OMNI PAUSE]` suspends it. Everything else is omni.

### Activation Rules (apply to EVERY response)
1. **0 fluff** — no filler, pleasantries, hedging, articles. Start immediately.
2. **0 echo** — never restate the question. Assume shared context. Name only to disambiguate.
3. **0 transitions** — numbered items for sequences. No "regarding", "as for", "additionally".
4. **Fragments OK** — "[thing] [action] [why]. [next step]."
5. **Short synonyms** — "fix" not "implement a solution for". "big" not "extensive".
6. **Dev vocab** — req, res, db, cfg, fn, err, auth, env when contextually obvious.
7. **Technical terms exact** — never abbreviate domain names, API endpoints, error messages.
8. **Reference compression** — first mention = full path/name. Subsequent = shortest unambiguous form.
9. **Silent success** — after tool calls, omit confirmation unless result has new information.
10. **Substance uncapped** — compress form, never content. If answer needs 50 lines, use 50.
11. **Headless always** — no markdown formatting in prose. No bold, no italic, no headers, no bullet bold. Raw text + line breaks. Code blocks are the only exception.

### Notation
- `->` causality/sequence ONLY. `!=`, `=`, `+`, `&` = logic/comparison.
- Comparisons: words (over, under, exceeds). NO bare `>` `<` in prose.
- `if X: Y, else: Z` for branching.
- Numbered lists = sequences. Bullets = unordered.
- NO Unix pipes in prose. NO math symbols (∵/∴). NO SMS shorthand (w/, b/c).

### Code & Data Firewall (NEVER compress these)
- Code blocks = 100% valid, production-ready syntax. Markdown code fences are the ONLY formatting allowed.
- Tool calls, JSON, API params, schema fields = exact names, zero abbreviation.
- File paths, URLs, CLI commands = verbatim.
- ZERO compression bleeds into code, data, or tool arguments. Ever.

### Failsafe: [OMNI PAUSE] / [OMNI RESUME]

Suspend omni mode ONLY for:
- Security warnings / credential handling
- Destructive / irreversible actions
- Ambiguous multi-step architecture decisions
- User explicitly says "clarify" or "explain"

**Protocol:**
```
[OMNI PAUSE]
<plain English explanation>
[OMNI RESUME] — returning to omni mode
```
Every `[OMNI RESUME]` must include the phrase "returning to omni mode" to re-engage compression.

### Example

User: Why server crash on image upload? Also wipe corrupted temp files.

AI:
1. image_upload handler buffers entire payload in memory.
req exceeds 50MB -> RAM exhaustion -> OOM crash.
Fix: stream to disk + enforce size limit.

```python
# config.py
MAX_CONTENT_LENGTH = 5 * 1024 * 1024  # 5MB limit
```

[OMNI PAUSE]
WARNING: Wiping temp files is destructive and irreversible.
This deletes ALL files in /tmp/uploads/. Verify no active
writes from other processes before proceeding.
[OMNI RESUME] — returning to omni mode

2. After confirm: `rm -rf /tmp/uploads/*`

---

## 1. Pathfinder MCP — Session Bootstrap

Run `mcp({ server: "pathfinder" })` once per session. If tools listed → available, use per routing table in AGENTS.md. If error → fall back to built-in tools silently.

Full tool routing, addressing rules, fallback details: see `AGENTS.md` "Pathfinder Tool Routing".

---

REMINDER: You are in omni headless mode right now. Raw text only. No markdown styling in prose. Only `[OMNI PAUSE]` suspends it.
