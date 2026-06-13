# Pathfinder Performance Analysis — Baseline Report

Date: 2026-06-13
Crates: `pathfinder`, `pathfinder-lsp`, `pathfinder-common`
Method: criterion 0.5 micro-benchmarks (saved baseline: `before`)

---

## Baseline Measurements

All baselines saved via `cargo bench --bench <name> -- --save-baseline before`.
Post-optimization comparison: `cargo bench --bench <name> -- --baseline before`.

### pathfinder-common — types_bench

| Benchmark | Median |
|---|---|
| semantic_path_parse/bare_file | 17.90 ns |
| semantic_path_parse/file_and_symbol | 67.43 ns |
| semantic_path_parse/overloaded | 75.20 ns |
| semantic_path_parse/deep_chain | 50.04 ns |
| semantic_path_parse/long_path | 71.00 ns |
| semantic_path_display/bare_file | 30.56 ns |
| semantic_path_display/file_and_symbol | 77.84 ns |
| semantic_path_display/overloaded | 87.98 ns |
| semantic_path_display/deep_chain | 65.75 ns |
| version_hash_compute/empty | 347.94 ns |
| version_hash_compute/small_100b | 379.50 ns |
| version_hash_compute/medium_1kb | 726.50 ns |
| version_hash_compute/large_10kb | 4.71 µs |
| version_hash_compute/realistic_4kb | 377.46 ns |
| version_hash_matches/full_hash | 3.22 ns |
| version_hash_matches/short_no_prefix | 2.86 ns |
| version_hash_matches/short_with_prefix | 2.91 ns |
| version_hash_matches/wrong_hash | 3.00 ns |
| version_hash_matches/too_short | 1.14 ns |
| workspace_root_resolve/simple | 72.66 ns |
| workspace_root_resolve/nested | 127.92 ns |
| workspace_root_resolve/traversal | 95.78 ns |
| workspace_root_resolve/deep | 159.91 ns |
| workspace_root_resolve_strict/valid_relative | 80.77 ns |
| workspace_root_resolve_strict/traversal_reject | 23.80 ns |
| workspace_root_resolve_strict/absolute_reject | 32.78 ns |

### pathfinder-common — sandbox_bench

| Benchmark | Median |
|---|---|
| sandbox_check/allowed/normal_rs | 77.04 ns |
| sandbox_check/allowed/normal_ts | 77.75 ns |
| sandbox_check/allowed/readme | 76.59 ns |
| sandbox_check/allowed/nested | 93.10 ns |
| sandbox_check/allowed/gitignore | 64.43 ns |
| sandbox_check/allowed/github_workflow | 103.05 ns |
| sandbox_check/denied/git_objects | 98.48 ns |
| sandbox_check/denied/pem_file | 51.04 ns |
| sandbox_check/denied/key_file | 50.25 ns |
| sandbox_check/denied/env_file | 115.12 ns |
| sandbox_check/denied/env_local | 154.89 ns |
| sandbox_check/denied/node_modules | 93.15 ns |
| sandbox_check/denied/vendor | 79.09 ns |
| sandbox_check/denied/traversal | 15.56 ns |
| sandbox_check_additional_deny/normal_file_with_extra_rules | 299.08 ns |
| sandbox_check_additional_deny/extension_deny_match | 160.94 ns |
| sandbox_check_additional_deny/directory_deny_match | 92.03 ns |

**Key insight:** `sandbox_check_additional_deny/normal_file_with_extra_rules` is 3.9x slower than `sandbox_check/allowed/normal_rs` — extra pattern matching adds ~222ns overhead per allowed file. This is the hot case (most files are allowed).

### pathfinder-common — guidance_bench

| Benchmark | Median |
|---|---|
| degraded_reason_guidance/no_lsp | 16.08 ns |
| degraded_reason_guidance/lsp_warmup_empty | 11.28 ns |
| degraded_reason_guidance/lsp_warmup_grep | 16.25 ns |
| degraded_reason_guidance/lsp_timeout_grep | 16.27 ns |
| degraded_reason_guidance/lsp_error_grep | 16.11 ns |
| degraded_reason_guidance/no_lsp_grep | 16.21 ns |
| degraded_reason_guidance/grep_fallback_file | 16.12 ns |
| degraded_reason_guidance/grep_fallback_impl | 16.16 ns |
| degraded_reason_guidance/grep_fallback_global | 16.15 ns |
| degraded_reason_guidance/grep_fallback_deps | 16.27 ns |
| degraded_reason_guidance/unsupported_lang_bypassed | 16.53 ns |
| degraded_reason_guidance/unsupported_lang | 16.84 ns |
| degraded_reason_guidance/git_error | 10.66 ns |
| degraded_reason_display/no_lsp | 17.64 ns |
| degraded_reason_display/lsp_warmup_grep | 17.49 ns |
| degraded_reason_display/git_error | 17.18 ns |

**Key insight:** guidance() is ~10-16ns. Already very fast — the `.to_owned()` allocations are being optimized away by the compiler (likely via small string optimization or LLVM const propagation). OPT-9 is LOW priority — measurement confirms it's not a bottleneck.

### pathfinder — find_symbol_bench

| Benchmark | Median |
|---|---|
| extract_name_optimized | 644.20 ns |
| extract_name_original_regex | 771.47 µs |
| truncate_preview_optimized_ascii | 60.77 ns |
| truncate_preview_original_ascii | 132.48 ns |
| truncate_preview_optimized_unicode | 184.37 ns |
| truncate_preview_original_unicode | 262.58 ns |
| truncate_preview_optimized_mixed | 95.06 ns |
| truncate_preview_original_mixed | 186.99 ns |

**Key insight:** The extract_name optimization already shows 1,197x improvement over regex (644ns vs 771µs). The truncate_preview optimization shows 2x improvement. These already landed. The remaining optimizations (OPT-1 through OPT-4) target different code paths not yet benchmarked — `is_workspace_file`, `kind_matches_filter`, `symbol_kind_to_filter_string`.

### pathfinder-lsp — response_parsers_bench

| Benchmark | Median |
|---|---|
| parse_definition_response/null | 78.31 ns |
| parse_definition_response/location_response_no_file | 10.49 µs |
| parse_definition_response/array_response_no_file | 11.62 µs |
| parse_definition_response/location_link_no_file | 11.08 µs |
| parse_definition_response/empty_array | 82.89 ns |
| parse_definition_response_with_file/with_real_file_read | 26.35 µs |
| parse_single_definition_location/null | 61.35 ns |
| parse_single_definition_location/location_no_file | 10.09 µs |
| parse_references_response/null | 73.63 ns |
| parse_references_response/single_ref_no_file | 8.93 µs |
| parse_references_response/five_refs_no_file | 45.63 µs |
| parse_references_response_with_files/five_refs_with_file_reads | 90.96 µs |
| parse_call_hierarchy_prepare/null | 3.85 ns |
| parse_call_hierarchy_prepare/single_item | 689.18 ns |

**Key insight:** File I/O dominates — `with_real_file_read` is 2.5x slower than `no_file` for definition, and `five_refs_with_file_reads` is 2x slower than `five_refs_no_file`. The per-reference cost is ~9µs without I/O, scaling linearly (5×9 = 45µs for 5 refs).

### pathfinder-lsp — transport_bench

| Benchmark | Median |
|---|---|
| write_message/small_request | 183.07 ns |
| write_message/notification | 133.23 ns |
| write_message/large_response_100_locs | 10.20 µs |
| read_message/single_message | 559.54 ns |
| read_message/notification | 510.90 ns |
| transport_roundtrip/write_then_read | 886.86 ns |

**Key insight:** Transport is fast. `write_message` for small payloads is ~133-183ns. Even large 100-loc responses take only 10.2µs. OPT-12 has low absolute impact — the bottleneck is response parsing, not transport.

---

## Priority Re-assessment Based on Profiling

| OPT | Original Priority | Profile-Adjusted | Rationale |
|---|---|---|---|
| OPT-1 | CRITICAL | **CRITICAL** | `canonicalize()` is 10-100µs per syscall × 100+ matches = 1-10ms overhead. No existing bench — needs new benchmark. |
| OPT-2 | CRITICAL | **MODERATE** | `symbol_kind_to_filter_string` returns static strings. ~20-30ns per alloc. Worth doing but not dominant. |
| OPT-3 | CRITICAL | **MODERATE** | `to_lowercase()` on ASCII is ~20ns per call. `eq_ignore_ascii_case` saves ~15ns. Multiply by 100 matches = 1.5µs. |
| OPT-4 | CRITICAL | **CRITICAL** | Tree-sitter enrichment is the dominant cost (parse + AST walk). Dedup before enrichment avoids redundant file parses. |
| OPT-5 | MODERATE | **MODERATE** | Pre-allocation saves ~3-5 realloc events. Low absolute impact. |
| OPT-6 | MODERATE | **LOW** | `normalize_path` allocates ~30ns per call. 100 matches = 3µs total. Not worth the Cow complexity. Simple pre-normalize-once is sufficient. |
| OPT-7 | MODERATE | **LOW** | `render_symbol_tree` is output formatting, not on the hot search path. |
| OPT-8 | LOW | **LOW** | `language_from_path` called once per file, not per match. |
| OPT-9 | LOW | **SKIP** | Benchmarks show 10-16ns — already fast. Compiler optimizing away allocations. |
| OPT-10 | LOW | **LOW** | `sandbox_check` with extra rules adds 222ns — called per file, not per match. |
| OPT-11 | LOW | **LOW** | Pre-alloc saves ~2-3 realloc events. Low absolute impact. |
| OPT-12 | LOW | **SKIP** | Transport is not the bottleneck. 133-183ns per write. |

### Final Execution Order

1. OPT-1 — eliminate `canonicalize()` per match (CRITICAL, needs new bench)
2. OPT-4 — deduplicate before Tree-sitter enrichment (CRITICAL)
3. OPT-2 — `&'static str` from kind-mapping (MODERATE)
4. OPT-3 — `eq_ignore_ascii_case` (MODERATE)
5. OPT-5 — pre-allocate collections (MODERATE)
6. OPT-6 — simple pre-normalize-once (LOW, simple approach only)
7. OPT-10 — pre-compute sandbox patterns (LOW)
8. OPT-11 — pre-allocate parser results (LOW)
