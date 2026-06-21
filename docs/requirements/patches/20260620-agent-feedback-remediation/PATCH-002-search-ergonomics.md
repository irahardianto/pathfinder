# PATCH-002: Search Ergonomics — kind=type Alias & comments_only Naming

Date: 2026-06-20
Source: 7 independent agent assessment reports
Status: Implemented

## Problem Statement

Agents report two friction points with search:

1. **No umbrella kind for "all types"**: `kind=class` matches class, struct,
   interface but NOT enum. `kind=struct` matches struct ONLY. No single kind
   value covers all type-level constructs. Agents must make 4+ queries.

2. **Misleading filter_mode name**: `filter_mode=comments_only` also matches
   string literals. Code explicitly filters for `"comment" || "string"`.
   SKILL.md says "returns only comments" — factually wrong.

Both are small code changes with high ergonomic impact.

---

## DELIVERABLE A: Add kind=type Alias

Priority: P1
Effort: Low (30 minutes)
Risk: Low (additive, no breaking changes)

**Current behavior** in `find_symbol.rs` `kind_matches_filter()` (lines
649-698):

| kind value | Matches |
|-----------|---------|
| class | class, struct, interface |
| struct | struct ONLY |
| enum | enum ONLY |
| interface | interface, trait |
| (no umbrella) | — |

**Steps**:

1. In `crates/pathfinder-core/src/tools/find_symbol.rs`, function
   `is_valid_kind_filter()` (around line 121-141):
   - Add `"type"` to the valid_kinds list

2. In same file, function `kind_matches_filter()` (around line 649-698):
   - Add new arm:
   ```rust
   } else if filter.eq_ignore_ascii_case("type") {
       kind.eq_ignore_ascii_case("class")
           || kind.eq_ignore_ascii_case("struct")
           || kind.eq_ignore_ascii_case("interface")
           || kind.eq_ignore_ascii_case("trait")
           || kind.eq_ignore_ascii_case("enum")
   }
   ```

3. Update the INVALID_PARAMS error message to include `"type"` in the
   valid values list.

4. Add tests:
   - `test_kind_matches_filter_type_matches_struct`
   - `test_kind_matches_filter_type_matches_enum`
   - `test_kind_matches_filter_type_matches_class`
   - `test_kind_matches_filter_type_matches_interface`
   - `test_kind_matches_filter_type_matches_trait`
   - `test_kind_matches_filter_type_does_not_match_function`
   - `test_kind_matches_filter_type_does_not_match_constant`
   - `test_is_valid_kind_filter_type_accepted`

**Files to modify**:
- `crates/pathfinder-core/src/tools/find_symbol.rs`

**Acceptance**:
- `search(mode="symbol", kind="type")` returns all structs, enums,
  classes, interfaces, traits
- `kind=type` passes validation (no INVALID_PARAMS)
- `kind=type` does NOT match functions, constants, modules, impl

---

## DELIVERABLE B: Add non_code Alias for comments_only

Priority: P2
Effort: Low (30 minutes)
Risk: Low (backward compatible — comments_only still works)

**Problem**: `filter_mode=comments_only` also matches string literals.
Code in search.rs (around line 399-404):

```rust
FilterMode::CommentsOnly => matches
    .filter(|(_, t)| t.as_str() == "comment" || t.as_str() == "string")
```

**Design decision**: Keep `comments_only` as accepted value (backward
compatible). Add `non_code` as alias with identical behavior. Update
docs to describe the actual behavior.

**Steps**:

1. In `crates/pathfinder-common/src/types.rs` (or wherever FilterMode
   enum is defined):
   - Add parsing: `"non_code"` → `FilterMode::CommentsOnly`
   - The enum variant name can stay `CommentsOnly` internally

2. In search tool description (schema/description source code):
   - Update `filter_mode` description:
     `"comments_only / non_code: matches comments AND string literals
     (non-code content)"`

3. Add tests:
   - `test_filter_mode_non_code_alias_accepted`
   - `test_filter_mode_non_code_matches_strings`
   - `test_filter_mode_non_code_matches_comments`
   - `test_filter_mode_comments_only_still_works`

**Files to modify**:
- `crates/pathfinder-common/src/types.rs` (FilterMode parsing)
- Search tool description source

**Acceptance**:
- `filter_mode="non_code"` works identically to `"comments_only"`
- `filter_mode="comments_only"` still works (no breaking change)
- Documentation accurately describes behavior

---

## Dependency Order

A and B are independent — can be done in parallel.

A should be done before PATCH-001 Deliverable E (kind table docs) so the
docs can mention `kind=type` as available.

## Verification Plan

```bash
cargo test -p pathfinder-core
cargo clippy -- -D warnings
```

Manual verification:
- `search(mode="symbol", kind="type")` returns expected results
- `search(query="TODO", filter_mode="non_code")` returns same results as
  `filter_mode="comments_only"`

Total effort: ~1 hour
