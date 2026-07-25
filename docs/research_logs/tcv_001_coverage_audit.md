# TCV-001 Coverage Audit Research Log

Date: 2026-07-25
Author: Scout Agent
Target: TCV-001 (Phase 5 — Test Coverage Improvement)

---

## 1. Existing Coverage Analysis

We processed the existing coverage files in the workspace:
- `/home/irahardianto/works/projects/pathfinder/coverage_detail.txt` (Contains coverage detail for `pathfinder-lsp`)
- `/home/irahardianto/works/projects/pathfinder/lcov.info` (Contains coverage for all other workspace crates)

### 1.1 Core Business Logic Crates vs. CLI/Mocks

Uncovered production lines grouped by crate (excluding test, mock, and helper files):

| Crate | Uncovered Production Lines (est.) | Primary Files |
|---|---|---|
| **pathfinder-lsp** | 498 | `client/detect.rs`, `client/process.rs`, `client/lifecycle.rs`, `client/background.rs`, `client/lawyer_impl.rs`, `lawyer.rs`, `client/document.rs`, `client/capabilities.rs`, `client/response_parsers.rs`, `client/mod.rs` |
| **pathfinder-treesitter** | 176 | `symbols.rs`, `repo_map.rs`, `cache.rs`, `vue_zones.rs`, `treesitter_surgeon.rs`, `language.rs`, `parser.rs` |
| **pathfinder-common** | 60 | `types.rs`, `config.rs`, `git.rs`, `sandbox.rs` |
| **pathfinder-search** | 23 | `ripgrep.rs` |
| **pathfinder (CLI)** | 391 | `server/tools/navigation/definition.rs`, `server/tools/navigation/impact.rs`, `server/tools/navigation/mod.rs`, `main.rs`, `server/tools/symbols.rs`, `server/tools/navigation/deep_context.rs`, `server/tools/search.rs`, `server/tools/navigation/health.rs`, `server/tools/navigation/overview.rs`, `server/tools/find_symbol.rs`, `server/tools/read_files.rs` |

### 1.2 Uncovered Lines File Manifest (Production Only)

#### pathfinder-lsp (Total: 498 lines)
- `client/detect.rs` (147 lines): 178-180, 183-185, 250, 425, 441-442, 444, 455, 464-466, 513, 586, 596, 604, 606, 633, 644, 687, 690, 702, 704, 709, 744-745, 747, 749, 768, 772, 783, 788, 843, 851, 881, 912-933, 935, 938, 983-992, 1028, 1031, 1037, 1085-1094, 1114-1132, 1146, 1149, 1155, 1207-1215, 1239, 1253, 1256, 1262, 1303-1314, 1316, 1320, 1346, 1349, 1363-1365, 1379-1381, 1397, 1400, 1405, 1420, 1430
- `client/process.rs` (130 lines): 162-164, 170, 177, 179, 187, 189-190, 197, 203, 209-210, 214, 297, 302, 304, 314, 316-318, 320-330, 332-334, 336, 339, 344-351, 353, 372-373, 388, 395, 401, 403, 430-431, 436, 446, 457-458, 464, 471-473, 477, 518, 555, 557, 585, 587, 593, 595, 623, 625, 652, 654, 683, 711-713, 718, 724-725, 730, 745, 754-755, 784-785, 809-817, 932-940, 942-947, 950, 952-954, 957-958, 962-963, 995, 1044-1046, 1357-1358, 1403-1404, 1567, 1878, 2111
- `client/lifecycle.rs` (107 lines): 53-55, 136, 153, 200, 223, 300, 312, 360, 430, 444, 601, 626-627, 630-636, 640-641, 644-651, 654-655, 660, 662, 665-671, 673, 675-678, 680, 682-686, 689-700, 702, 707-718, 720-721, 723-744, 746, 836, 840, 844, 848, 857-860, 874
- `client/background.rs` (56 lines): 89, 93, 113, 148-156, 159-160, 205-206, 208-212, 218, 220, 222-226, 231, 233, 235-236, 312, 314, 330, 339-341, 353, 362, 371, 383-387, 399, 420, 425-427, 429, 431, 433, 440, 576
- `client/lawyer_impl.rs` (16 lines): 18-23, 25-27, 34, 37, 366, 368, 375-377
- `lawyer.rs` (15 lines): 107, 109, 183-185, 194-199, 206-208, 216
- `client/document.rs` (10 lines): 54, 58, 78, 132, 156-158, 189-190, 193
- `client/capabilities.rs` (8 lines): 240-241, 302-303, 305-306, 310, 315
- `client/response_parsers.rs` (5 lines): 149, 178, 304, 306, 359
- `client/mod.rs` (4 lines): 73-74, 132, 137

#### pathfinder-treesitter (Total: 176 lines)
- `symbols.rs` (79 lines): 73, 110, 206-212, 214, 218, 367-368, 380, 471-473, 475, 513-514, 548-550, 600-605, 607-609, 615, 630, 660, 668, 701, 708, 771, 774, 777, 808, 811, 814, 834, 879, 930, 965, 1014, 1109, 1322, 1389, 1476, 1506-1515, 1569, 1584-1587, 1598-1599, 1601-1603, 1609-1610, 1647, 1665, 1674, 1688
- `repo_map.rs` (33 lines): 178, 492, 494, 653, 707, 736-738, 811-813, 817-822, 837-839, 843-848, 929-934, 960
- `cache.rs` (23 lines): 263, 265, 270-272, 281, 318-320, 364, 366, 371-373, 383, 411-415, 421-423
- `vue_zones.rs` (18 lines): 122-123, 190, 200, 210, 253-260, 265-268, 271
- `treesitter_surgeon.rs` (16 lines): 60-62, 82-84, 118-120, 188, 191-193, 209-211
- `language.rs` (4 lines): 188, 207-208, 221
- `parser.rs` (3 lines): 39-41

#### pathfinder-common (Total: 60 lines)
- `types.rs` (51 lines): 396-397, 486, 492, 496, 501, 541, 554, 556, 559, 573, 578, 582, 599-607, 610-613, 649, 655-657, 662-664, 666, 670-672, 679, 683-685, 690-692, 697-699, 704-706, 708
- `config.rs` (5 lines): 65-69
- `git.rs` (3 lines): 79, 89, 109
- `sandbox.rs` (1 line): 298

#### pathfinder-search (Total: 23 lines)
- `ripgrep.rs` (23 lines): 226, 232, 235, 269, 307, 318, 387, 474, 478-479, 481, 497, 535, 563, 629-632, 649-651, 679, 698

---

## 2. Test Setup, Structure, and Conventions

All examined crates share an extremely consistent test architecture following the project's rules:

### 2.1 Test Co-location
For any production file `file.rs`, there is a corresponding `file_test.rs` file placed directly alongside it in the same directory.
- For example, `crates/pathfinder-lsp/src/client/detect.rs` compiles its test module via:
  ```rust
  #[cfg(test)]
  #[path = "detect_test.rs"]
  mod tests;
  ```
- This keeps the production source clean of test annotations, but keeps implementation and tests coupled side by side in the source tree.

### 2.2 Test double / Mock Infrastructure
Each domain has well-defined boundaries and custom test doubles:
- **pathfinder-lsp**: Exposes the `Lawyer` trait as its primary abstraction boundary. Tests can use `MockLawyer` (`mock.rs`) or configure a `FakeTransport` (`client/fake_transport.rs`) to capture JSON-RPC input/output.
- **pathfinder-treesitter**: Employs in-tree test fixtures. Contains its own `mock.rs` if needed for surgeon/parser configurations.
- **pathfinder-search**: Exposes a `mock.rs` containing a custom mock searcher.
- **pathfinder-common**: Implements `FakeGitRunner` for sandboxed git operation tests.
- **pathfinder**: Server and tool integration tests use a `make_server()` helper that builds a test server, swapping out the real Lawyer for a `MockLawyer` or `NoOpLawyer`, and real searcher for `MockScout`.

---

## 3. MECE Feature Clusters / Topics

The 499 (or 498) uncovered lines of core logic are mapped into five MECE (Mutually Exclusive, Collectively Exhaustive) feature clusters:

### Cluster 1: LSP Client Operations & Process Lifecycle (pathfinder-lsp)
- **Scope**: `crates/pathfinder-lsp/src/client/{lifecycle.rs, process.rs, background.rs, capabilities.rs, mod.rs}`
- **Responsibility**: Process spawning, IPC stream handling, async background monitoring, and connection cleanup/restarts on crash.
- **Root Cause of Gaps**: Process supervision relies on OS-level background tasks that are hard to test synchronously without mock transports or specific child process exit states.

### Cluster 2: Language Server Configuration & Detection (pathfinder-lsp)
- **Scope**: `crates/pathfinder-lsp/src/client/detect.rs`
- **Responsibility**: Inspecting file systems for workspace markers (Java `pom.xml`, Python virtualenvs, etc.) and selecting candidate servers.
- **Root Cause of Gaps**: Complex nested file marker paths and custom virtualenv discovery branches are not fully exercised by current default mock directory structures.

### Cluster 3: Semantic Node Traversals & Signature Parsing (pathfinder-treesitter)
- **Scope**: `crates/pathfinder-treesitter/src/{symbols.rs, repo_map.rs, vue_zones.rs, treesitter_surgeon.rs}`
- **Responsibility**: Tree-sitter AST navigation, mapping abstract nodes to symbol types, remapping Vue multi-zone boundaries, and performing code alterations.
- **Root Cause of Gaps**: Tree walking handles dozens of specific AST node types (macros, trait impls, const generics) which are not fully represented in standard test fixture files.

### Cluster 4: Trait Implementations & Infrastructure Utilities (pathfinder-common)
- **Scope**: `crates/pathfinder-common/src/{types.rs, config.rs, git.rs, sandbox.rs}`
- **Responsibility**: Common domain models, serialization/deserialization traits, error conversions, and command shell constraints.
- **Root Cause of Gaps**: Trait edge cases (Display formats, FromStr error handling for invalid strings, default structs, and serialization of optional/colliding fields) are skipped by happy-path tests.

### Cluster 5: Search Dispatch & Process Wrapping (pathfinder-search)
- **Scope**: `crates/pathfinder-search/src/ripgrep.rs`
- **Responsibility**: Invoking and parsing ripgrep processes, escaping regex queries, pagination, and multi-pattern searches.
- **Root Cause of Gaps**: Ripgrep arguments mapping for specific flag combinations (like multi-pattern escaping and file filters) is omitted by current tests.

---

## 4. Scout Research Findings Log

Below is the structured output for our findings, complying with the Scout agent standards.

### Finding 1: LSP Process Supervision & Background Tasks
- **Topic**: Lifecycles, Background monitors, and Crash supervision in `pathfinder-lsp`
- **Source**: `crates/pathfinder-lsp/src/client/lifecycle.rs`, `process.rs`, `background.rs`
- **Finding**: Async process supervision involves tokio tasks like `reader_supervisor_task`, `progress_watcher`, and `idle_timeout_task`. These tasks manage DashMaps of processes and clear stale open-document maps on crash. Current tests use simple constructors that bypass these monitors entirely.
- **Relevance**: Essential to design a specialized unit testing suite using `FakeLspTransport` to simulate specific client crashes, normal EOFs, and timeouts.
- **Confidence**: VERIFIED (confirmed via `coverage_detail.txt` lines 53-90 and `lifecycle_test.rs`).

### Finding 2: AST Node Coverage in Treesitter
- **Topic**: AST Walkers and Symbol Signature Extraction
- **Source**: `crates/pathfinder-treesitter/src/symbols.rs`
- **Finding**: Over 79 lines are uncovered in `symbols.rs` because our test fixtures lack certain uncommon Rust/Java elements like generic trait implementations, lifetimes, macros, and multi-line signatures.
- **Relevance**: Adding specialized mock code snippets (fixtures) for Tree-sitter to parse will instantly cover these branches.
- **Confidence**: VERIFIED (confirmed via `lcov.info` parsing and inspect of tree walking patterns).

### Finding 3: Trait Implementations for Common Types
- **Topic**: Domain Serialization and Error Conversions
- **Source**: `crates/pathfinder-common/src/types.rs`
- **Finding**: 51 lines are uncovered because traits like `FromStr` for `Visibility` and `SearchFilter`, custom `Serialize`/`Deserialize` for optional parameters, and custom `Hash` implementations have never been fed bad/edge-case inputs in unit tests.
- **Relevance**: These are quick-win files (Batch 5) that can be easily brought to 100% coverage using simple parameter-driven tests.
- **Confidence**: VERIFIED (confirmed via `types_test.rs` exploration).
