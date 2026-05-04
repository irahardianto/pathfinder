#![allow(clippy::expect_used, clippy::unwrap_used, clippy::manual_assert)]

use crate::error::SurgeonError;
use crate::surgeon::{BodyRange, ExtractedSymbol, FullRange, ResolvedFile, Surgeon, SymbolRange};
use pathfinder_common::types::{SemanticPath, SymbolScope, VersionHash};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ── Dispatch pattern overview ───────────────────────────────────────────────
//
// MockSurgeon method bodies fall into two categories, each served by a shared
// private helper to eliminate boilerplate:
//
//   resolve_range_dispatch — for methods keyed by (workspace_root, semantic_path):
//     read_symbol_scope, resolve_body_range, resolve_body_end_range,
//     resolve_full_range, resolve_symbol_range.
//
//   file_path_dispatch — for methods keyed by (workspace_root, file_path):
//     read_source_file, extract_symbols.
//
// Special-case methods (enclosing_symbol, generate_skeleton, node_type_at_position)
// retain bespoke implementations due to non-standard argument shapes or
// default-fallback behaviour.

/// A mock implementation of the [`Surgeon`] trait for unit testing.
///
/// Records method calls and returns pre-configured results.
///
/// # Populating results
///
/// Push `Ok(...)` or `Err(...)` values into the `*_results` fields before calling
/// the mocked method. Each call pops the next result in FIFO order.
///
/// # Asserting calls
///
/// Inspect the `*_calls` fields after the call under test to verify that the correct
/// arguments were passed.
#[derive(Debug, Default)]
pub struct MockSurgeon {
    // ── Result queues ────────────────────────────────────────────────────────
    /// Pre-configured return values for reading symbol scopes.
    pub read_symbol_scope_results: Mutex<Vec<Result<SymbolScope, SurgeonError>>>,
    #[allow(clippy::type_complexity)]
    /// Pre-configured return values for reading source files.
    pub read_source_file_results:
        Mutex<Vec<Result<(String, VersionHash, String, Vec<ExtractedSymbol>), SurgeonError>>>,
    /// Pre-configured return values for extracting symbols.
    pub extract_symbols_results: Mutex<Vec<Result<Vec<ExtractedSymbol>, SurgeonError>>>,
    /// Pre-configured return values for finding enclosing symbols.
    pub enclosing_symbol_results: Mutex<Vec<Result<Option<String>, SurgeonError>>>,
    /// Pre-configured return values for generating repository skeletons.
    pub generate_skeleton_results: Mutex<Vec<Result<crate::repo_map::RepoMapResult, SurgeonError>>>,
    /// Pre-configured return values for resolving body ranges.
    pub resolve_body_range_results: Mutex<Vec<Result<(BodyRange, ResolvedFile), SurgeonError>>>,
    /// Pre-configured return values for resolving body end ranges.
    pub resolve_body_end_range_results:
        Mutex<Vec<Result<(crate::surgeon::BodyEndRange, ResolvedFile), SurgeonError>>>,
    /// Pre-configured return values for resolving full ranges.
    pub resolve_full_range_results: Mutex<Vec<Result<(FullRange, ResolvedFile), SurgeonError>>>,
    /// Pre-configured return values for resolving symbol ranges.
    pub resolve_symbol_range_results: Mutex<Vec<Result<(SymbolRange, ResolvedFile), SurgeonError>>>,
    /// Pre-configured return values for `node_type_at_position`.
    /// Defaults to returning `"code"` when the queue is empty.
    pub node_type_at_position_results: Mutex<Vec<Result<String, SurgeonError>>>,

    // ── Call history ─────────────────────────────────────────────────────────
    /// Recorded `(workspace_root, semantic_path)` for each `read_symbol_scope` call.
    pub read_symbol_scope_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    /// Recorded `(workspace_root, file_path)` for each `read_source_file` call.
    pub read_source_file_calls: Mutex<Vec<(PathBuf, PathBuf)>>,
    /// Recorded `(workspace_root, file_path)` for each `extract_symbols` call.
    pub extract_symbols_calls: Mutex<Vec<(PathBuf, PathBuf)>>,
    /// Recorded `(workspace_root, file_path, line)` for each `enclosing_symbol` call.
    pub enclosing_symbol_calls: Mutex<Vec<(PathBuf, PathBuf, usize)>>,
    /// Recorded arguments for each `generate_skeleton` call.
    #[allow(clippy::type_complexity)]
    pub generate_skeleton_calls:
        Mutex<Vec<(PathBuf, PathBuf, crate::repo_map::SkeletonConfig<'static>)>>,
    /// Recorded `(workspace_root, semantic_path)` for each `resolve_body_range` call.
    pub resolve_body_range_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    /// Recorded `(workspace_root, semantic_path)` for each `resolve_body_end_range` call.
    pub resolve_body_end_range_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    /// Recorded `(workspace_root, semantic_path)` for each `resolve_full_range` call.
    pub resolve_full_range_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    /// Recorded `(workspace_root, semantic_path)` for each `resolve_symbol_range` call.
    pub resolve_symbol_range_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    /// Recorded `(workspace_root, file_path, line, column)` for each `node_type_at_position` call.
    pub node_type_at_position_calls: Mutex<Vec<(PathBuf, PathBuf, usize, usize)>>,
}

impl MockSurgeon {
    /// Creates a new `MockSurgeon`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ── Shared dispatch helpers ───────────────────────────────────────────────

    /// Pop the next queued result for a method keyed by `(workspace_root, semantic_path)`.
    ///
    /// Records the call arguments in `calls_mutex`, then removes and returns the
    /// first entry from `results_mutex`. Panics with a descriptive message if the
    /// results queue is empty (i.e. an unexpected call was made).
    fn resolve_range_dispatch<T>(
        calls_mutex: &Mutex<Vec<(PathBuf, SemanticPath)>>,
        results_mutex: &Mutex<Vec<Result<T, SurgeonError>>>,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
        method_name: &str,
    ) -> Result<T, SurgeonError> {
        calls_mutex
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), semantic_path.clone()));

        let mut results = results_mutex.lock().expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to {method_name}"
        );
        results.remove(0)
    }

    /// Pop the next queued result for a method keyed by `(workspace_root, file_path)`.
    ///
    /// Records the call arguments in `calls_mutex`, then removes and returns the
    /// first entry from `results_mutex`. Panics with a descriptive message if the
    /// results queue is empty (i.e. an unexpected call was made).
    fn file_path_dispatch<T>(
        calls_mutex: &Mutex<Vec<(PathBuf, PathBuf)>>,
        results_mutex: &Mutex<Vec<Result<T, SurgeonError>>>,
        workspace_root: &Path,
        file_path: &Path,
        method_name: &str,
    ) -> Result<T, SurgeonError> {
        calls_mutex
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), file_path.to_path_buf()));

        let mut results = results_mutex.lock().expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to {method_name}"
        );
        results.remove(0)
    }
}

#[async_trait::async_trait]
impl Surgeon for MockSurgeon {
    // ── Semantic-path methods (keyed by workspace_root + semantic_path) ───────

    async fn read_symbol_scope(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<SymbolScope, SurgeonError> {
        Self::resolve_range_dispatch(
            &self.read_symbol_scope_calls,
            &self.read_symbol_scope_results,
            workspace_root,
            semantic_path,
            "read_symbol_scope",
        )
    }

    async fn resolve_body_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(BodyRange, ResolvedFile), SurgeonError> {
        Self::resolve_range_dispatch(
            &self.resolve_body_range_calls,
            &self.resolve_body_range_results,
            workspace_root,
            semantic_path,
            "resolve_body_range",
        )
    }

    async fn resolve_body_end_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(crate::surgeon::BodyEndRange, ResolvedFile), SurgeonError> {
        Self::resolve_range_dispatch(
            &self.resolve_body_end_range_calls,
            &self.resolve_body_end_range_results,
            workspace_root,
            semantic_path,
            "resolve_body_end_range",
        )
    }

    async fn resolve_full_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(FullRange, ResolvedFile), SurgeonError> {
        Self::resolve_range_dispatch(
            &self.resolve_full_range_calls,
            &self.resolve_full_range_results,
            workspace_root,
            semantic_path,
            "resolve_full_range",
        )
    }

    async fn resolve_symbol_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(SymbolRange, ResolvedFile), SurgeonError> {
        Self::resolve_range_dispatch(
            &self.resolve_symbol_range_calls,
            &self.resolve_symbol_range_results,
            workspace_root,
            semantic_path,
            "resolve_symbol_range",
        )
    }

    // ── File-path methods (keyed by workspace_root + file_path) ──────────────

    async fn read_source_file(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<(String, VersionHash, String, Vec<ExtractedSymbol>), SurgeonError> {
        Self::file_path_dispatch(
            &self.read_source_file_calls,
            &self.read_source_file_results,
            workspace_root,
            file_path,
            "read_source_file",
        )
    }

    async fn extract_symbols(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        Self::file_path_dispatch(
            &self.extract_symbols_calls,
            &self.extract_symbols_results,
            workspace_root,
            file_path,
            "extract_symbols",
        )
    }

    // ── Special-case methods ──────────────────────────────────────────────────

    async fn enclosing_symbol(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<String>, SurgeonError> {
        // Three-argument key; not covered by either generic dispatch helper.
        self.enclosing_symbol_calls
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), file_path.to_path_buf(), line));

        let mut results = self
            .enclosing_symbol_results
            .lock()
            .expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to enclosing_symbol"
        );
        results.remove(0)
    }

    async fn generate_skeleton(
        &self,
        _workspace_root: &Path,
        path: &Path,
        config: &crate::repo_map::SkeletonConfig<'_>,
    ) -> Result<crate::repo_map::RepoMapResult, SurgeonError> {
        // Bespoke: must convert the borrowed SkeletonConfig lifetime to 'static
        // before storing in the calls log.
        let static_config = crate::repo_map::SkeletonConfig {
            max_tokens: config.max_tokens,
            depth: config.depth,
            visibility: if config.visibility == "public" {
                "public"
            } else {
                "all"
            },
            max_tokens_per_file: config.max_tokens_per_file,
            changed_files: config.changed_files.clone(),
            include_extensions: config.include_extensions.clone(),
            exclude_extensions: config.exclude_extensions.clone(),
        };
        self.generate_skeleton_calls.lock().unwrap().push((
            path.to_path_buf(),
            path.to_path_buf(),
            static_config,
        ));

        let mut results = self.generate_skeleton_results.lock().unwrap();
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to generate_skeleton"
        );
        results.remove(0)
    }

    async fn node_type_at_position(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
        column: usize,
    ) -> Result<String, SurgeonError> {
        // Bespoke: returns a default value ("code") when the results queue is empty,
        // making it transparent for tests that don't care about node classification.
        self.node_type_at_position_calls
            .lock()
            .expect("mutex poisoned")
            .push((
                workspace_root.to_path_buf(),
                file_path.to_path_buf(),
                line,
                column,
            ));

        let mut results = self
            .node_type_at_position_results
            .lock()
            .expect("mutex poisoned");

        if results.is_empty() {
            Ok("code".to_owned())
        } else {
            results.remove(0)
        }
    }
}
