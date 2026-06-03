# pathfinder-treesitter Performance Analysis

## Baseline Benchmarks (2026-06-03)

### Symbol Extraction (extract_symbols_from_tree)

| Language | Size | Time | Throughput |
|---|---|---|---|
| Rust | 50 fns | 56.1 us | 50.4 MiB/s |
| Rust | 200 fns | 221 us | 50.9 MiB/s |
| Rust | 500 fns | 594 us | 50.2 MiB/s |
| Go | 50 fns | 76.7 us | 49.8 MiB/s |
| Go | 200 fns | 337 us | 48.5 MiB/s |
| Go | 500 fns | 1.07 ms | 46.2 MiB/s |
| Python | 50 fns | 115 us | 27.3 MiB/s |
| Python | 200 fns | 544 us | 23.1 MiB/s |
| Python | 500 fns | 2.10 ms | 11.9 MiB/s |
| TypeScript | 50 classes | 771 us | 48.7 MiB/s |
| TypeScript | 100 classes | 1.56 ms | 47.6 MiB/s |
| Java | 50 classes | 560 us | 48.5 MiB/s |
| Java | 100 classes | 1.07 ms | 50.8 MiB/s |

### Parsing (AstParser::parse_source)

| Language | Functions | Time | Throughput |
|---|---|---|---|
| Go | 500 | 429 us | 2.9 MiB/s |
| TypeScript | 500 | 2.07 ms | 669 KiB/s |
| Python | 500 | 1.52 ms | 908 KiB/s |
| Rust | 500 | 1.10 ms | 12.4 MiB/s |
| Java | 500 | 568 us | 2.2 MiB/s |
| JavaScript | 500 | 32.0 ms | 435 KiB/s |
| Rust | 100 | 211 us | 12.0 MiB/s |
| Rust | 1000 | 2.22 ms | 12.3 MiB/s |

### Cache Operations

| Operation | Time |
|---|---|
| Miss (parse Go 25KB) | 494 us |
| Hit (mtime fast-path Go) | 6.77 us |
| Miss (parse Vue SFC) | 76.0 us |
| Hit (mtime fast-path Vue) | 12.0 us |
| Singleflight (5 concurrent) | 658 us |

### did_you_mean

| Scenario | Symbols | Time |
|---|---|---|
| Near miss | 200 | 34.5 us |
| Far miss | 200 | 42.3 us |
| Exact match | 200 | 17.2 us |
| Near miss | 20 | 695 ns |

### Vue Zones

| Operation | Size | Time | Throughput |
|---|---|---|---|
| Scan small SFC | 256B | 774 ns | 311 KiB/s |
| Parse small SFC | 256B | 37.2 us | 1.3 MiB/s |
| Parse medium SFC | 1.6KB | 123 us | 13.1 MiB/s |
| Parse large 20 comp | 2.4KB | 192 us | 12.3 MiB/s |
| Parse large 50 comp | 5.6KB | 453 us | 12.6 MiB/s |
| Parse large 100 comp | 11KB | 878 us | 12.8 MiB/s |

## Top Offenders (ordered by expected impact)

1. **Double file read in generate_skeleton_text** — reads file for hash, then cache reads again for parse. 2x I/O per file.

2. **No parser reuse** — Parser::new() + set_language() per call. Rust 500-fn parse: 1.1ms includes parser init.

3. **did_you_mean runs Levenshtein unconditionally** — Even exact match costs 17us (200 symbols). Near miss: 35us.

4. **Per-recursion HashMap allocation** — SymbolExtractionContext creates fresh HashMap at every scope level.

5. **MultiZoneTree clone on cache hit** — Vue cache hit clones 3 trees + zones + source Arc (~12us).

6. **Sequential file processing in generate_skeleton_text** — No parallelism for independent file operations.

7. **std::sync::Mutex** — Poison handling adds ~200ns per lock; parking_lot eliminates this.

## Irreducible Floors

| Cost | Minimum | Notes |
|---|---|---|
| Tree-sitter parse (Rust) | ~2.2 us/fn | Must visit every byte |
| SHA-256 hash | ~1 us/KB | Cryptographic, no faster option |
| stat(2) syscall | ~500 ns | OS boundary for mtime check |
| Mutex lock/unlock | ~20 ns | parking_lot ceiling |
