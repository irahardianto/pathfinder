# Pathfinder v5 Implementation Audit: Epic E4 and E7

## Audit Objective
Conduct a thorough, file-by-file audit of the Pathfinder repository to verify the implementation of Epic E4 (Search Intelligence Improvements) and Epic E7 (OCC Ergonomics & Agent Experience Polish) as defined in `docs/requirements/pathfinder-v5-requirements.md`.

## Methodology
- Codebase exploration to identify implementations of E4 and E7 requirements.
- Line-by-line verification of the identified source files involving search (`search.rs`, `ripgrep.rs`) and OCC edit ergonomics (`edit.rs`, `file_ops.rs`, `error.rs`).
- Comparison against the Acceptance Criteria outlined in the PRD.

## Audit Findings

### Epic E4: Search Intelligence Improvements

#### E4.1 `known_files` Support
- **Status**: ⚠️ **PARTIAL (Implementation Gap Found)**
- **Details**: 
  - **Grouped results**: Correctly implemented. Matches for known files correctly land in `group.known_matches` as `KnownFileMatch`.
  - **Flat results**: **Implementation Gap.** The PRD specifies appending a `known: true` flag to the result object for flat results. Currently, `SearchMatch` (defined in `crates/pathfinder-search/src/types.rs`) lacks a `known` boolean field. Although `search.rs` correctly clears `content`, `context_before`, and `context_after` for known files, it does not append the `known: true` flag because the `SearchMatch` shape doesn't support it.
- **Reference**: `crates/pathfinder/src/server/tools/search.rs` (lines 108-115) & `crates/pathfinder-search/src/types.rs`.

#### E4.2 `group_by_file` Structure
- **Status**: ✅ **PASSED**
- **Details**: Accurately implemented. When `group_by_file` is true, the `FileGroup` output accumulates matches perfectly, maintaining token efficiency using a single `version_hash` string per group rather than per match. 

#### E4.3 `exclude_glob` Filter
- **Status**: ✅ **PASSED**
- **Details**: Implemented perfectly in the `walk_files` logic. The exclusion glob leverages the `globset` crate and intercepts file traversal immediately inside `RipgrepScout::walk_files` before any file bytes are read by the grep searcher.
- **Reference**: `crates/pathfinder-search/src/ripgrep.rs` (lines 313-318).

---

### Epic E7: OCC Ergonomics & Agent Experience Polish

#### E7.1 Version Hash Elision
- **Status**: ✅ **PASSED**
- **Details**: Tool documentation implemented perfectly. The `search_codebase`, `get_repo_map`, `read_symbol_scope`, `read_source_file`, and `analyze_impact` tool descriptions unambiguously state that their `version_hash` outputs are "immediately usable as base_version for edit tools — no additional read required."
- **Reference**: `crates/pathfinder/src/server.rs` tool registration macros.

#### E7.2 `lines_changed` Stateless Extraction
- **Status**: ✅ **PASSED**
- **Details**: Correctly adheres to the "best-effort" / stateless architecture constraint. The application correctly refrains from preemptively keeping the old tree in memory. Instead, inside `flush_edit_with_toctou`, upon hitting a version mismatch, it performs UTF-8 conversion of the pending input and the disk content, cleanly dispatching `compute_lines_changed` to fetch the delta for the `VERSION_MISMATCH` hint. Other non-TOCTOU mismatch errors sensibly receive `lines_changed: None`.
- **Reference**: `crates/pathfinder/src/server/tools/edit.rs` (lines 1114-1147) and `crates/pathfinder-common/src/error.rs`.

## Conclusion & Action Required
The implementation for Epic E7 is solid and entirely complies with PRD requirements, particularly fulfilling the stateless constraint on computing `lines_changed`.
However, **Epic E4 requires immediate remediation** to add the `known: bool` field to the `SearchMatch` struct, and to populate it conditionally inside `search_codebase_impl` during the flat mapping process. Wait until this is fixed before manual validation.
