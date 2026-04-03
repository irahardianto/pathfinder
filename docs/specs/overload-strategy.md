# Overload Strategy for Pathfinder Language Support

**Status:** Active  
**Date:** 2026-04-03  
**Origin:** Pathfinder v5.1 hardening discussion — ensuring consistent symbol resolution behavior across all supported languages and future languages that support function/method overloading.

---

## Background

Function overloading — multiple methods with the same name but different signatures — is a language feature present in TypeScript, Java, C#, C++, Swift, and Kotlin. It is absent in Go, Python, and Rust (Rust achieves similar effects through traits, not overloads).

During v5.1 hardening, the question arose: **how should Pathfinder address overloaded symbols in its semantic path scheme?** This document captures the decision made and the strategy that MUST be applied uniformly to any future language that supports overloading.

---

## Current Language Coverage

| Language | Overloads? | Pathfinder Strategy |
|---|---|---|
| **TypeScript / TSX / JSX** | ✅ Yes (function overloads) | Ordered suffix: `fn[1]`, `fn[2]` |
| **JavaScript** | ❌ No (duck typing) | N/A |
| **Go** | ❌ No | N/A |
| **Python** | ❌ No (single dispatch by default) | N/A |
| **Rust** | Trait-based (not true overloads) | Impl blocks merged under parent type name (no suffix) |
| **Vue SFC** | ✅ Script block inherits TypeScript rules | Same as TypeScript |

---

## The Problem: Disambiguation vs. Usability

When a symbol appears multiple times in a file (e.g., two `execute()` overloads), the semantic path `src/service.ts::Service.execute` is **ambiguous** — it could refer to either one.

Two design options exist:

### Option A: Named Suffix (e.g., `execute[1]`, `execute[2]`)
Agents must specify the 1-indexed suffix to target a specific overload. No suffix = first overload (backward compatible).

### Option B: Merge All Overloads into One Symbol
Treat all overloads as a single addressable unit. The edit applies to the entire overload group (typically the implementation signature at the bottom).

---

## Decision: Named Suffix Strategy

**Pathfinder uses Option A — ordered numeric suffix for overloads.**

### Rationale

1. **Agents can differentiate.** An agent inspecting code may need to read or edit specifically the 2-argument overload signature, not all of them. Merging would hide this capability.

2. **Backward compatibility.** A path without a suffix (e.g., `Service.execute`) continues to resolve to the **first** occurrence. Existing agents and scripts continue to work.

3. **Discoverability.** `get_repo_map` and `read_source_file` return all overload variants in the symbol list, so agents can discover and enumerate them.

4. **Consistency with Vue multi-zone indexing.** The `[nth]` suffix pattern is already established in Pathfinder for other disambiguation needs (e.g., `div[2]` in JSX/TSX symbol paths). Reusing the same convention reduces the cognitive surface area.

---

## Implementation Rules (Mandatory for All Languages)

When adding support for a new language that has function/method overloads, MUST follow these rules:

### Rule 1: Detect Multiple Same-Named Symbols
In `extract_symbols_from_tree`, detect when two or more symbols share the same name within the same lexical scope (class, module, or file top-level).

### Rule 2: Apply 1-Indexed Numeric Suffix
When multiple same-named symbols are detected, suffix ALL of them:
- First occurrence → `fn[1]`
- Second occurrence → `fn[2]`
- Nth occurrence → `fn[N]`

> **Important:** Apply the suffix to ALL occurrences, not just the 2nd+. This ensures the numbering is stable when occurrences are added or removed.

### Rule 3: No-Suffix Alias Resolves to First
When an agent queries `Service.execute` (no suffix), resolve it to `execute[1]`. This preserves backward compatibility and means agents do not need to know about overloads to access the most common (first-defined) variant.

### Rule 4: Expose All Variants in Symbol Output
`get_repo_map`, `read_source_file`, and `search_codebase` must enumerate ALL overloads as distinct symbols. Agents must be able to discover them without prior knowledge.

### Rule 5: `did_you_mean` Includes Overloads
When `SYMBOL_NOT_FOUND` occurs for an overloaded name, `did_you_mean` suggestions must include all known overloads:
```json
{
  "did_you_mean": ["Service.execute[1]", "Service.execute[2]"]
}
```

---

## Reference Implementation: TypeScript Overloads

The canonical implementation reference is in `crates/pathfinder-treesitter/src/symbols.rs`:

- `test_extract_overloads` — verifies that overloaded functions receive `[1]`, `[2]` etc. suffixes
- `test_resolve_overloads` — verifies that no-suffix paths resolve to `[1]`

When implementing a new language, add equivalent tests using the same pattern.

---

## Languages NOT Using This Strategy

The following languages do NOT need the overload suffix because they do not support overloading:

- **Go** — uses unique function names; interface-based polymorphism is trait-like, not overloads
- **Python** — Python 3 has no native overloading; `@singledispatch` is rare
- **Rust** — Rust does NOT support function overloading. Methods in `impl` blocks are merged under the struct/enum name (the `#N` suffix on `impl` blocks is stripped entirely). See `merge_rust_impl_blocks` in `symbols.rs`.
- **JavaScript** — No static types; no overloads

---

## Future Languages: Onboarding Checklist

When adding Pathfinder support for a new language (e.g., Java, C#, Swift, Kotlin, C++):

1. **Check if the language has overloading** — consult the language spec.
2. **If yes**: implement the Named Suffix Strategy (Rules 1–5 above) in `extract_symbols_from_tree`.
3. **Add tests** mirroring `test_extract_overloads` and `test_resolve_overloads` from `symbols.rs`.
4. **Update this document** to add the new language to the coverage table.
5. **Update tool descriptions** in `server.rs` if the behavior has user-visible implications.

---

## Non-Goals

- **Method overriding** (inheritance polymorphism) is a separate concern. Pathfinder targets concrete implementations, not virtual dispatch chains. Class hierarchies across files are not unified into a single symbol tree.
- **Generic type specializations** (e.g., `Foo<string>` vs `Foo<number>`) are NOT treated as overloads. Type parameters are stripped from semantic paths.
- **Macro-generated symbols** (Rust procedural macros, TypeScript decorators) are out of scope for this strategy.
