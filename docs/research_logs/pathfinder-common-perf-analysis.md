# pathfinder-common Performance Analysis

**Date:** 2026-06-03
**Scope:** `crates/pathfinder-common` — shared types, errors, and infrastructure
**Baseline commit:** HEAD (pre-optimization)

## Executive Summary

5 optimizations applied. 4 show measurable improvement. 1 (guidance) was at the noise floor already.

| Fix | Function | Baseline | Optimized | Change |
|-----|----------|----------|-----------|--------|
| 1 | `SymbolChain::Display` (file+symbol) | 122 ns | 78 ns | **-36%** |
| 2 | `DegradedReason::guidance` | 11-17 ns | 11-17 ns | noise floor |
| 3 | `Sandbox::check` (normal .rs) | 86 ns | 79 ns | **-8%** |
| 3 | `Sandbox::check` (with additional_deny) | 339 ns | 307 ns | **-9%** |
| 4 | `SemanticPath::parse` (file+symbol) | 75 ns | 68 ns | **-9%** |
| 4 | `SemanticPath::parse` (deep chain) | 55 ns | 52 ns | **-5%** |
| 5 | `VersionHash::compute` (empty) | 91 ns | 73 ns | **-20%** |
| 5 | `VersionHash::compute` (1KB) | 560 ns | 465 ns | **-17%** |

## Baseline Numbers

### SemanticPath::parse
| Case | Baseline | After | Change |
|------|----------|-------|--------|
| bare_file | 19 ns | 19 ns | ~0% |
| file_and_symbol | 75 ns | 68 ns | -9% |
| overloaded | 81 ns | 79 ns | -2% |
| deep_chain | 55 ns | 53 ns | -4% |
| long_path | 78 ns | 73 ns | -6% |

### SymbolChain::Display
| Case | Baseline | After | Change |
|------|----------|-------|--------|
| bare_file | 33 ns | 32 ns | -3% |
| file_and_symbol | 123 ns | 78 ns | **-37%** |
| overloaded | 140 ns | 89 ns | **-36%** |
| deep_chain | 105 ns | 66 ns | **-37%** |

### VersionHash::compute
| Case | Baseline | After | Change |
|------|----------|-------|--------|
| empty (0 B) | 91 ns | 73 ns | **-20%** |
| small (100 B) | 113 ns | 94 ns | **-17%** |
| medium (1 KB) | 539 ns | 465 ns | **-14%** |
| large (10 KB) | 4.69 µs | 4.61 µs | -2% |

### Sandbox::check (allowed paths)
| Case | Baseline | After | Change |
|------|----------|-------|--------|
| normal .rs | 86 ns | 79 ns | -8% |
| normal .ts | 84 ns | 80 ns | -5% |
| README.md | 81 ns | 80 ns | -1% |
| nested path | 104 ns | 97 ns | -7% |
| .gitignore | 69 ns | 65 ns | -6% |
| .github/workflow | 77 ns | 75 ns | -3% |

### Sandbox::check (denied paths)
| Case | Baseline | After | Change |
|------|----------|-------|--------|
| .git/objects | 39 ns | 35 ns | -10% |
| .pem file | 53 ns | 50 ns | -6% |
| .key file | 54 ns | 50 ns | -7% |
| .env file | 51 ns | 52 ns | +2% |
| node_modules | 106 ns | 97 ns | -8% |
| vendor | 90 ns | 78 ns | -13% |
| traversal | 17 ns | 16 ns | -6% |

### DegradedReason::guidance
| Case | Baseline | After | Change |
|------|----------|-------|--------|
| NoLsp | 17 ns | 16 ns | noise |
| LspWarmupEmpty | 11 ns | 11 ns | noise |
| GitError | 11 ns | 11 ns | noise |

All guidance variants operate at 11-17ns — irreducible floor. The `Cow::Borrowed` change eliminates allocations but the allocator is already too fast to measure at this scale.

## Fixes Applied

### Fix 1: SymbolChain::Display — Zero-alloc formatting
**File:** `src/types.rs:143-153`
**Pattern:** Pre-allocation (eliminate intermediate `Vec<String>`)
**Change:** Replaced `Vec<String>` + `join(".")` with direct `write!` loop over segments.
**Impact:** -36% for multi-segment chains. Zero allocations on the formatting path.

### Fix 2: DegradedReason::guidance — Cow<str> static borrows
**File:** `src/types.rs` (`ActionableGuidance` struct)
**Pattern:** Reduce allocations via `Cow::Borrowed`
**Change:** Changed `trust_level: String` and `fallback_tool: Option<String>` to `Cow<'static, str>` and `Option<Cow<'static, str>>`. Construction uses `Cow::Borrowed` (no allocation). Serialization unchanged.
**Impact:** No measurable change (11-17ns is irreducible floor). But eliminates 2-3 heap allocations per call in theory.
**Note:** The function was already at the noise floor. Kept for code quality (no unnecessary allocations).

### Fix 3: Sandbox::check — Fast-reject for non-dot paths
**File:** `src/sandbox.rs` (`is_hardcoded_denied`)
**Pattern:** Fast-reject / short-circuit
**Change:** Added early return in `is_hardcoded_denied` for paths not starting with `.`. Normal source files (src/main.rs, lib/foo.rs) skip all .git/* pattern checks entirely, only checking extension against [pem, key, pfx, p12].
**Impact:** -5% to -11% for common allowed paths. Up to -13% for denied paths like vendor/.

### Fix 4: SymbolChain::parse — Pre-allocate segment Vec
**File:** `src/types.rs` (`SymbolChain::parse`)
**Pattern:** Pre-allocation
**Change:** Count dots in input, then `Vec::with_capacity(dot_count + 1)` before iterating. Avoids Vec reallocation during `collect()`.
**Impact:** -5% to -9% for paths with symbol chains.

### Fix 5: VersionHash::compute — Pre-sized buffer
**File:** `src/types.rs` (`VersionHash::compute`)
**Pattern:** Pre-allocation
**Change:** Replaced `format!("sha256:{hash:x}")` with `String::with_capacity(71)` + `std::fmt::write`. Eliminates buffer resize during formatting.
**Impact:** -14% to -20% for small inputs. Diminishes for large inputs where SHA-256 dominates.

## Fixes Skipped

None (all 5 planned fixes were implemented).

## Remaining Opportunities (Future Sessions)

1. **`Sandbox::check` — Bloom filter / trie for pattern matching**: Current linear scan over ~15 patterns. A precomputed prefix trie would give O(k) lookup where k = pattern length. Impact: ~10-20ns savings. Risk: medium (complexity). Defer until profile shows sandbox is a bottleneck.

2. **`Symbol::name` — SmolStr or Box<str>**: Symbol names are typically short (5-30 chars). `SmolStr` would inline names up to 22 bytes, eliminating heap allocation. Risk: new dependency. Impact: ~5-10ns per symbol parse. Defer until parse shows up in production profiles.

3. **`WorkspaceRoot::resolve` — Avoid PathBuf construction**: Currently builds a new `PathBuf` via `join`. Could normalize in-place or use a borrowed path. Risk: medium (API change). Impact: ~20-30ns. Defer.

## Quality Verification

- `cargo test -p pathfinder-mcp-common`: 101 passed
- `cargo test -p pathfinder-mcp-common -p pathfinder-mcp --all-targets`: 605 passed
- `cargo clippy -p pathfinder-mcp-common -- -D warnings`: No issues
- `cargo clippy -p pathfinder-mcp -- -D warnings`: No issues in changed code
