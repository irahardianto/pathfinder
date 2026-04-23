# ADR-001: Keep Mock Trait Implementations Explicit (No Macros)

**Status:** Accepted  
**Date:** 2026-04-23  
**Decision Makers:** Project maintainer + AI audit consensus  
**Revisit Trigger:** Trait surface exceeds 15 methods, or copy-paste divergence is observed

---

## Context

During the April 2026 quality remediation (Qlty reports), both `MockLawyer`
(`pathfinder-lsp/src/mock.rs`, ~591 lines) and `MockSurgeon`
(`pathfinder-treesitter/src/mock.rs`, ~268 lines) were flagged for structural
duplication. Each mock trait implementation contains repetitive patterns:

```rust
async fn method_name(&self, ...) -> Result<T, Error> {
    Self::pop_queued_result(&self.method_results).unwrap_or(Ok(default))
}
```

A `mock_trait_method!()` declarative macro was proposed to reduce ~850 combined
lines to ~350 lines (~60% reduction).

## Decision

**Keep mock trait implementations explicit. Do NOT introduce a macro.**

## Rationale

### 1. Readability Over DRY

Mock trait implementations are one of the few places where explicit verbosity aids
debugging. When a test fails, you immediately see what the mock does by reading its
body — no need to trace into a macro expansion.

### 2. IDE Support

Rust Analyzer understands explicit `async fn` implementations perfectly. Macros
(especially procedural ones) often break code navigation, hover documentation,
and refactoring tools. This directly impacts developer velocity.

### 3. Compiler Diagnostic Quality

When mock signatures drift from trait definitions (e.g., after adding a parameter),
the compiler produces clear error messages pointing to the exact mock function.
With a macro, errors would point to the macro call site with opaque expansion errors.

### 4. Low Maintenance Frequency

These mocks change rarely — only when the `Lawyer` or `Surgeon` trait surface
changes. The "duplication cost" is near-zero in practice because:
- `Lawyer` has ~12 methods (stable since March 2026)
- `Surgeon` has ~10 methods (stable since March 2026)
- Neither grows frequently

### 5. Proportional Crate Scope

At 591 and 268 lines respectively, neither mock file is excessively large for
test infrastructure. The files are self-contained and never imported outside their
own crate's test modules.

## Consequences

- Mock files will remain at their current size (~850 lines combined)
- New trait methods require copy-paste of the standard pattern (3–8 lines each)
- Pattern consistency relies on developer discipline rather than tooling enforcement

## Revisit Conditions

This decision should be revisited if ANY of these conditions become true:

1. **Trait surface growth:** Either `Lawyer` or `Surgeon` exceeds 15 methods
2. **Behavioral divergence:** Copy-paste errors cause mock behavior to diverge
   from the standard `lock → push/pop` pattern
3. **Team scaling:** Team grows large enough that onboarding consistency outweighs
   the debuggability advantage of explicit implementations
4. **Tooling improvement:** Rust macro diagnostic tooling improves enough that
   macro expansions produce developer-friendly error messages

## References

- Qlty Report 1: `docs/audits/20260422-qlty-report.md` — Clone group findings
- Qlty Report 2: `docs/audits/20260422-qlty-report2.md` — Mock duplication findings
- Remediation audit: 2026-04-23 post-remediation analysis
