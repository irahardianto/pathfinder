# 005. Language Support: Three Registries, Eleven Axes, One CI Gate

**Date:** 2026-08-22
**Status:** Accepted
**Related:** scope cards WS-6, WS-7, WS-8, WS-9; findings `LSP-B4` `LSP-B5` `TS-B2` `TS-B3` `TS-D2` `RSP-B1`

## Context

Pathfinder supports five languages. Adding a sixth has historically taken a sprint, and the reasons were assumed to be LSP-protocol divergence. Audit shows that is a third of the truth.

**Language support is `min(LSP plugins, Tree-sitter coverage, fallback tables)`** — three registries that must all agree, and only one has an abstraction.

1. **LSP layer.** A clean, pure-data `LanguagePlugin` trait exists with five implementations of ~40 lines each. But it is not the code path: `detect.rs` is 1,526 lines of hand-written per-language functions each re-implementing the same dance. Adding a language means writing a detect function, not adding a plugin. And per-server divergence is not four axes but **eleven** — including manifest validation with five distinct parsers, spawn environment with four distinct variable sets, argv injection returning a lifetime-bound advisory lock, and marker-sibling refinement that is a property of a (language, marker) *pair*.

2. **Tree-sitter layer.** Worse coupled, and it is the **addressing** layer — semantic paths, symbol extraction, body ranges. Grammars are compile-time Cargo dependencies behind a hardcoded enum. `symbols.rs` is 1,701 lines with per-language node-kind logic in scattered match arms, a third of which is JSX, HTML and CSS handling with no upstream query support at all.

3. **Fallback tables.** `navigation/mod.rs` holds ~380 lines of per-language keyword tables, file globs, and definition regexes, on the live fallback path for all three surviving tools. A sixth language gets a 7-keyword generic filter and floods its own candidate list.

The proposal to adopt tree-sitter `tags.scm` queries was investigated. It is partially sound and was oversold: of 20 per-language items in `symbols.rs`, five are cleanly query-expressible, six need significant hand-authoring, and nine are not expressible at all. Upstream tags files are far thinner than assumed — Python's is 14 lines total; a plain `export class Foo { bar() {} }` matches nothing in `typescript/tags.scm`.

## Options Considered

### Option A: Keep hand-written per-language code, add languages as needed
- **Pros:** no refactor risk; works for the five it covers
- **Cons:** cost per language stays a sprint; three registries drift; "supported" stays an opinion
- **Effort:** Low per language, unbounded in total

### Option B: Full data-driven plugins, no Rust per language
- **Pros:** users add languages without recompiling
- **Cons:** not achievable. Python's init options read `$CONDA_PREFIX` and stat eight paths; Java's read `.sdkmanrc` and probe sdkman/asdf/jenv on two roots; jdtls needs argv injection returning a `File` lock held for the process lifetime. No TOML schema expresses these.
- **Effort:** High, goal unreachable

### Option C: Pure `tags.scm` replacement of `symbols.rs`
- **Pros:** the ecosystem-standard mechanism; some things become genuinely declarative
- **Cons:** loses parent-child nesting entirely (queries emit a flat match list, so `AuthService.login` does not exist in the output); HTML and CSS ship no tags files; would silently change extraction in both directions
- **Effort:** High, with a large correctness hole

### Option D: Layered — generic mechanisms where they exist, honestly-counted hooks where they do not, CI as the definition of "supported"
- **Pros:** each layer scales as far as it actually can; cost per language is stated truthfully; support stops being an opinion
- **Cons:** more moving parts than a single clean abstraction; requires accepting that some per-language Rust is permanent
- **Effort:** High

## Decision

**Option D**, with four commitments.

1. **All eleven LSP divergence axes get trait methods**, including the three that cannot be pure data (`initialization_options` takes both roots and does I/O; `revalidate_at_spawn` runs at a different lifecycle point; `spawn_argv_hook` returns an owned resource). `detect.rs` collapses to a generic loop — target **≤ 450 lines, not 150**, because five behaviours (manifest validation, sibling refinement, monorepo fallback, source-file post-filter, override suppression) are load-bearing and must survive.

2. **Tree-sitter adopts queries plus one generic containment pass.** The key insight is that **nesting is generic, not per-language**: a match whose byte range is strictly inside another's becomes its child. One pass handles Rust impls, Go receivers, TS classes, Java classes and Python classes uniformly, leaving a small, named, per-language hook list — Rust 3, Go 2, Java 2, TS/JS 3, Python 1. The "one hook per language" cap is abandoned as unachievable by every language.

3. **`vue_zones.rs` is kept and generalised into an injection engine**, reversing the earlier plan to delete it. It is the only correct multi-grammar primitive in the codebase, and `injections.scm` — which the original plan omitted — is the general answer to Vue, Svelte, Astro, HTML, Markdown fences and tagged template literals.

4. **A language is supported iff it passes the conformance suite.** Sixteen assertions, four carrying the weight: goto-implementation resolves to the impl not the declaration; a cold-start query never returns a confident wrong empty; generate-parse-resolve round-trips to the same byte range; and a **warm** LSP never returns a confident wrong empty on annotated, non-ASCII or cross-file-impl fixtures. Tier 1 / Tier 2 / unsupported, table generated by CI.

## Consequences

### Positive
- Cost per language becomes measurable and honest: one grammar entry, one hand-written kind map, one to three post-process hooks, one plugin (TOML where possible), one fixture. Proven by adding Kotlin or Ruby end-to-end and recording the diff size as the baseline.
- The three registries are forced into agreement by a startup assertion and a CI test, so a language can never be half-supported — addressable but with no LSP, or spawnable but unaddressable.
- The README claim becomes defensible: "N conformance-verified languages", not "any LSP works."

### Negative
- Some per-language Rust is permanent. `PythonPlugin` and `JavaPlugin` cannot be replaced by TOML.
- The tags.scm win is smaller than hoped. Query authoring has no type checker and no tests until runtime; for six of twenty items it is harder than the Rust it replaces.
- The registry ID relation is N:1 (`TypeScript` and `Tsx` share an LSP id; `Vue` has a grammar but no LSP plugin), so the sync assertion is a mapping check, not an equality check.

### Risks

**R1 — The containment pass is not implemented and hierarchical paths flatten.** Every `Type.method` path in every agent's context becomes invalid simultaneously. Largest single breakage risk in v6. Mitigation: conformance C-14 (round trip) must pass before and after; the pass is an explicit named deliverable.

**R2 — "Byte-identical extraction before and after" gets waived, removing the only guard.** The criterion is unsatisfiable — the query approach finds symbols the current code misses (Go constants) and misses symbols it finds (Java records, TS classes) — so enforcing it literally would require reproducing current bugs. Mitigation: replaced up front with differential review, where every added or removed symbol is justified against a finding ID and zero deltas are unexplained.

**R3 — Line-count targets become an incentive to delete features.** ~565 lines of `symbols.rs` are JSX/HTML/CSS with no query support. Mitigation: relocation to `symbols/{vue,jsx,css}.rs` is an explicit task; the size target is measured after relocation.

**R4 — Extension routing collisions.** `plugin_for_extension` is first-match-wins and is the single router for every LSP call site; a user TOML plugin claiming `ts` would silently reroute all TypeScript traffic by insertion order. Mitigation: uniqueness assertion at registry build plus a defined precedence rule.

**R5 — Runtime `.so` grammar loading is arbitrary code execution.** Deferred to a separate, separately-reviewed change: opt-in config, trusted directory, checksum pinning, ABI-version rejection.

## Related
- ADR-003 — the readiness model, which per-language `readiness_budget` and the probe feed into
- `docs/requirements/pathfinder-v6/01-findings.md` — `TS-B2` carries the full per-item query-feasibility table
- Core Design Principles: Rule of Three, composition over inheritance
