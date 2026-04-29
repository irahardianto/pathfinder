//! Diagnostic diffing — multiset comparison of pre- and post-edit diagnostics.
//!
//! This module implements PRD §5.10: the edit validation pipeline compares
//! diagnostics before and after an in-memory edit to determine whether new
//! errors were **introduced** by the change.
//!
//! # Hash key design
//! Line/column are intentionally **excluded** from the hash key because edits
//! shift positions. Two diagnostics are considered "the same" if they share
//! the same severity, code, message, and source file — regardless of where in
//! the file they appear.

use pathfinder_lsp::types::LspDiagnostic;
use std::collections::HashMap;

/// The result of diffing pre-edit and post-edit diagnostics.
#[derive(Debug, Default)]
pub struct DiagnosticDiff {
    /// Diagnostics that appeared **after** the edit (new problems).
    pub introduced: Vec<LspDiagnostic>,
    /// Diagnostics that disappeared **after** the edit (fixed problems).
    pub resolved: Vec<LspDiagnostic>,
}

impl DiagnosticDiff {
    /// Returns `true` if any introduced diagnostic is a blocking error (severity 1).
    pub fn has_new_errors(&self) -> bool {
        self.introduced.iter().any(LspDiagnostic::is_error)
    }
}

/// Compute the multiset difference between `pre` and `post` diagnostic sets.
///
/// Uses `(severity, code, message, file)` as the hash key — line/column are
/// excluded because edits shift positions.  Two diagnostics with the same key
/// but different positions count as the same diagnostic for diffing purposes.
pub fn diff_diagnostics(pre: &[LspDiagnostic], post: &[LspDiagnostic]) -> DiagnosticDiff {
    let pre_counts = build_counts(pre);
    let post_counts = build_counts(post);

    let introduced = collect_introduced(post, &pre_counts, &post_counts);
    let resolved = collect_resolved(pre, &pre_counts, &post_counts);

    DiagnosticDiff {
        introduced,
        resolved,
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// A stable hash key for a diagnostic that intentionally excludes position.
///
/// Diagnosed as `(severity_u8, code_option, message, file)`.
type DiagKey = (u8, Option<String>, String, String);

fn diag_key(d: &LspDiagnostic) -> DiagKey {
    (
        d.severity as u8,
        d.code.clone(),
        d.message.clone(),
        d.file.clone(),
    )
}

/// Build a `HashMap` counting occurrences of each diagnostic key.
fn build_counts(diags: &[LspDiagnostic]) -> HashMap<DiagKey, usize> {
    let mut counts: HashMap<DiagKey, usize> = HashMap::with_capacity(diags.len());
    for d in diags {
        *counts.entry(diag_key(d)).or_insert(0) += 1;
    }
    counts
}

/// Collect diagnostics in `post` that appear **more often** than in `pre`.
///
/// Each element in the returned vec is a representative `LspDiagnostic` for
/// one excess occurrence. Both count maps are passed in from the caller to
/// avoid redundant `HashMap` construction.
fn collect_introduced(
    post: &[LspDiagnostic],
    pre_counts: &HashMap<DiagKey, usize>,
    post_counts: &HashMap<DiagKey, usize>,
) -> Vec<LspDiagnostic> {
    let mut result = Vec::new();
    let mut emitted: HashMap<DiagKey, usize> = HashMap::new();
    for d in post {
        let key = diag_key(d);
        let pre = *pre_counts.get(&key).unwrap_or(&0);
        let post_count = *post_counts.get(&key).unwrap_or(&0);
        let excess = post_count.saturating_sub(pre);
        let done = *emitted.get(&key).unwrap_or(&0);
        if done < excess {
            result.push(d.clone());
            *emitted.entry(key).or_insert(0) += 1;
        }
    }
    result
}

/// Collect diagnostics in `pre` that appear **more often** than in `post`.
///
/// Both count maps are passed in from the caller to avoid redundant `HashMap`
/// construction.
fn collect_resolved(
    pre: &[LspDiagnostic],
    pre_counts: &HashMap<DiagKey, usize>,
    post_counts: &HashMap<DiagKey, usize>,
) -> Vec<LspDiagnostic> {
    let mut result = Vec::new();
    let mut emitted: HashMap<DiagKey, usize> = HashMap::new();
    for d in pre {
        let key = diag_key(d);
        let post = *post_counts.get(&key).unwrap_or(&0);
        let pre_count = *pre_counts.get(&key).unwrap_or(&0);
        let excess = pre_count.saturating_sub(post);
        let done = *emitted.get(&key).unwrap_or(&0);
        if done < excess {
            result.push(d.clone());
            *emitted.entry(key).or_insert(0) += 1;
        }
    }
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use pathfinder_lsp::types::LspDiagnosticSeverity;

    fn make_error(msg: &str) -> LspDiagnostic {
        LspDiagnostic {
            severity: LspDiagnosticSeverity::Error,
            code: None,
            message: msg.into(),
            file: "src/main.rs".into(),
            start_line: 1,
            end_line: 1,
        }
    }

    fn make_warning(msg: &str) -> LspDiagnostic {
        LspDiagnostic {
            severity: LspDiagnosticSeverity::Warning,
            code: None,
            message: msg.into(),
            file: "src/main.rs".into(),
            start_line: 2,
            end_line: 2,
        }
    }

    fn make_error_at(msg: &str, line: u32) -> LspDiagnostic {
        LspDiagnostic {
            severity: LspDiagnosticSeverity::Error,
            code: None,
            message: msg.into(),
            file: "src/main.rs".into(),
            start_line: line,
            end_line: line,
        }
    }

    #[test]
    fn test_diff_empty_pre_and_post() {
        let diff = diff_diagnostics(&[], &[]);
        assert!(diff.introduced.is_empty());
        assert!(diff.resolved.is_empty());
        assert!(!diff.has_new_errors());
    }

    #[test]
    fn test_diff_no_change() {
        let pre = vec![make_error("type mismatch")];
        let post = vec![make_error("type mismatch")];
        let diff = diff_diagnostics(&pre, &post);
        assert!(
            diff.introduced.is_empty(),
            "same error should not appear as introduced"
        );
        assert!(
            diff.resolved.is_empty(),
            "same error should not appear as resolved"
        );
    }

    #[test]
    fn test_diff_new_error_detected() {
        let pre = vec![];
        let post = vec![make_error("type mismatch")];
        let diff = diff_diagnostics(&pre, &post);
        assert_eq!(diff.introduced.len(), 1);
        assert_eq!(diff.introduced[0].message, "type mismatch");
        assert!(diff.has_new_errors());
        assert!(diff.resolved.is_empty());
    }

    #[test]
    fn test_diff_resolved_error_detected() {
        let pre = vec![make_error("type mismatch")];
        let post = vec![];
        let diff = diff_diagnostics(&pre, &post);
        assert!(diff.introduced.is_empty());
        assert_eq!(diff.resolved.len(), 1);
        assert_eq!(diff.resolved[0].message, "type mismatch");
        assert!(!diff.has_new_errors());
    }

    #[test]
    fn test_diff_excludes_line_column() {
        // Same error at different lines — should NOT appear as introduced/resolved
        let pre = vec![make_error_at("type mismatch", 5)];
        let post = vec![make_error_at("type mismatch", 20)]; // position shifted by edit
        let diff = diff_diagnostics(&pre, &post);
        assert!(
            diff.introduced.is_empty(),
            "shifted error should not appear as introduced"
        );
        assert!(
            diff.resolved.is_empty(),
            "shifted error should not appear as resolved"
        );
    }

    #[test]
    fn test_diff_multiset_counting() {
        // 2 pre, 3 post → 1 introduced
        let pre = vec![make_error("duplicate"), make_error("duplicate")];
        let post = vec![
            make_error("duplicate"),
            make_error("duplicate"),
            make_error("duplicate"),
        ];
        let diff = diff_diagnostics(&pre, &post);
        assert_eq!(diff.introduced.len(), 1);
        assert!(diff.resolved.is_empty());
    }

    #[test]
    fn test_diff_warning_does_not_block() {
        // New warning: introduced, but has_new_errors() = false
        let pre = vec![];
        let post = vec![make_warning("unused variable")];
        let diff = diff_diagnostics(&pre, &post);
        assert_eq!(diff.introduced.len(), 1);
        assert!(!diff.has_new_errors()); // warnings don't block
    }

    #[test]
    fn test_diff_mixed_introduced_and_resolved() {
        let pre = vec![make_error("old error")];
        let post = vec![make_error("new error")];
        let diff = diff_diagnostics(&pre, &post);
        assert_eq!(diff.introduced.len(), 1);
        assert_eq!(diff.introduced[0].message, "new error");
        assert_eq!(diff.resolved.len(), 1);
        assert_eq!(diff.resolved[0].message, "old error");
        assert!(diff.has_new_errors());
    }
}
