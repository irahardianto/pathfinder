//! AST-aware edit tools — `replace_body`, `replace_full`, `insert_before`, `insert_after`,
//! `delete_symbol`, and `validate_only`.
//!
//! All edit tools share a common pipeline:
//! 1. Parse semantic path
//! 2. Sandbox check
//! 3. Read file → OCC check (`base_version`)
//! 4. Resolve scope via Surgeon
//! 5. Normalize input (`normalize_for_body_replace` / `normalize_for_full_replace`)
//! 6. Indentation pre-pass (dedent → reindent to AST column)
//! 7. Splice normalized code into scope byte range
//! 8. **LSP validation** (`did_open` → `pull_diagnostics` → `did_change` → `pull_diagnostics` → diff)
//! 9. TOCTOU late-check: re-read, re-hash immediately before write
//! 10. `tokio::fs::write` (in-place, preserves inode)
//! 11. Compute and return new `version_hash`

use crate::server::types::EditValidation;
use pathfinder_common::types::{SemanticPath, VersionHash};

/// Result of the LSP validation step.
pub(crate) struct ValidationOutcome {
    pub(crate) validation: EditValidation,
    pub(crate) skipped: bool,
    pub(crate) skipped_reason: Option<String>,
    /// `true` when new errors were introduced and `ignore_validation_failures = false`.
    /// The caller must NOT write to disk in this case.
    pub(crate) should_block: bool,
}

/// Parameter struct for [`finalize_edit`] to reduce parameter count from 9 to 2.
pub(crate) struct FinalizeEditParams<'a> {
    tool_name: &'static str,
    semantic_path: &'a SemanticPath,
    raw_semantic_path_str: &'a str,
    source: &'a [u8],
    original_hash: &'a VersionHash,
    new_content: Vec<u8>,
    ignore_validation_failures: bool,
    start_time: std::time::Instant,
    resolve_ms: u128,
}

/// Selects which end of a resolved symbol range is used as the insertion point.
///
/// Passed to [`PathfinderServer::resolve_insert_position`] to distinguish
/// `insert_before` (start of symbol) from `insert_after` (end of symbol).
pub(crate) enum InsertEdge {
    /// Insert at `symbol_range.start_byte` (before the symbol) or file offset 0.
    Before,
    /// Insert at `symbol_range.end_byte` (after the symbol) or end-of-file.
    After,
}

#[derive(Debug)]
pub(crate) struct ResolvedEditFree {
    start_byte: usize,
    end_byte: usize,
    replacement: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct ResolvedEdit {
    start_byte: usize,
    end_byte: usize,
    replacement: Vec<u8>,
}

pub(super) mod batch;
pub(super) mod handlers;
pub(super) mod text_edit;
pub(super) mod validation;

#[cfg(test)]
pub(super) mod tests;
