#![allow(clippy::expect_used, clippy::unwrap_used, clippy::manual_assert)]

use crate::error::SurgeonError;
use crate::surgeon::{BodyRange, ExtractedSymbol, FullRange, Surgeon, SymbolRange};
use pathfinder_common::types::{SemanticPath, SymbolScope, VersionHash};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A mock implementation of the [`Surgeon`] trait for unit testing.
///
/// Records method calls and returns pre-configured results.
#[derive(Debug, Default)]
pub struct MockSurgeon {
    pub read_symbol_scope_results: Mutex<Vec<Result<SymbolScope, SurgeonError>>>,
    #[allow(clippy::type_complexity)]
    pub read_source_file_results:
        Mutex<Vec<Result<(String, VersionHash, String, Vec<ExtractedSymbol>), SurgeonError>>>,
    pub extract_symbols_results: Mutex<Vec<Result<Vec<ExtractedSymbol>, SurgeonError>>>,
    pub enclosing_symbol_results: Mutex<Vec<Result<Option<String>, SurgeonError>>>,
    pub generate_skeleton_results: Mutex<Vec<Result<crate::repo_map::RepoMapResult, SurgeonError>>>,
    #[allow(clippy::type_complexity)]
    pub resolve_body_range_results:
        Mutex<Vec<Result<(BodyRange, Vec<u8>, VersionHash), SurgeonError>>>,
    #[allow(clippy::type_complexity)]
    pub resolve_full_range_results:
        Mutex<Vec<Result<(FullRange, Vec<u8>, VersionHash), SurgeonError>>>,
    #[allow(clippy::type_complexity)]
    pub resolve_symbol_range_results:
        Mutex<Vec<Result<(SymbolRange, Vec<u8>, VersionHash), SurgeonError>>>,

    /// Pre-configured return values for `node_type_at_position`.
    /// Each call pops the next value (FIFO). Defaults to returning `"code"` when empty.
    pub node_type_at_position_results: Mutex<Vec<Result<String, SurgeonError>>>,

    // Call history
    pub read_symbol_scope_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    pub read_source_file_calls: Mutex<Vec<(PathBuf, PathBuf)>>,
    pub extract_symbols_calls: Mutex<Vec<(PathBuf, PathBuf)>>,
    pub enclosing_symbol_calls: Mutex<Vec<(PathBuf, PathBuf, usize)>>,
    #[allow(clippy::type_complexity)]
    pub generate_skeleton_calls: Mutex<Vec<(PathBuf, PathBuf, u32, u32, String, u32)>>,
    pub resolve_body_range_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    pub resolve_full_range_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    pub resolve_symbol_range_calls: Mutex<Vec<(PathBuf, SemanticPath)>>,
    pub node_type_at_position_calls: Mutex<Vec<(PathBuf, PathBuf, usize, usize)>>,
}

impl MockSurgeon {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl Surgeon for MockSurgeon {
    async fn read_symbol_scope(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<SymbolScope, SurgeonError> {
        self.read_symbol_scope_calls
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), semantic_path.clone()));

        let mut results = self
            .read_symbol_scope_results
            .lock()
            .expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to read_symbol_scope"
        );
        results.remove(0)
    }

    async fn read_source_file(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<(String, VersionHash, String, Vec<ExtractedSymbol>), SurgeonError> {
        self.read_source_file_calls
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), file_path.to_path_buf()));

        let mut results = self
            .read_source_file_results
            .lock()
            .expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to read_source_file"
        );
        results.remove(0)
    }

    async fn extract_symbols(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        self.extract_symbols_calls
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), file_path.to_path_buf()));

        let mut results = self.extract_symbols_results.lock().expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to extract_symbols"
        );
        results.remove(0)
    }

    async fn enclosing_symbol(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<String>, SurgeonError> {
        self.enclosing_symbol_calls
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), file_path.to_path_buf(), line));

        let mut results = self
            .enclosing_symbol_results
            .lock()
            .expect("mutex poisoned");
        if results.is_empty() {
            panic!("MockSurgeon: Unexpected call to enclosing_symbol");
        }
        results.remove(0)
    }

    #[allow(clippy::too_many_arguments)]
    async fn generate_skeleton(
        &self,
        _workspace_root: &Path,
        path: &Path,
        max_tokens: u32,
        depth: u32,
        visibility: &str,
        max_tokens_per_file: u32,
        _changed_files: Option<std::collections::HashSet<std::path::PathBuf>>,
        _include_extensions: Vec<String>,
        _exclude_extensions: Vec<String>,
    ) -> Result<crate::repo_map::RepoMapResult, SurgeonError> {
        self.generate_skeleton_calls.lock().unwrap().push((
            path.to_path_buf(),
            path.to_path_buf(), // target_path = path for mock
            max_tokens,
            depth,
            visibility.to_owned(),
            max_tokens_per_file,
        ));
        let mut results = self.generate_skeleton_results.lock().unwrap();
        if results.is_empty() {
            panic!("MockSurgeon: Unexpected call to generate_skeleton");
        }
        results.remove(0)
    }

    async fn resolve_body_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(BodyRange, Vec<u8>, VersionHash), SurgeonError> {
        self.resolve_body_range_calls
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), semantic_path.clone()));

        let mut results = self
            .resolve_body_range_results
            .lock()
            .expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to resolve_body_range"
        );
        results.remove(0)
    }

    async fn resolve_full_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(FullRange, Vec<u8>, VersionHash), SurgeonError> {
        self.resolve_full_range_calls
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), semantic_path.clone()));

        let mut results = self
            .resolve_full_range_results
            .lock()
            .expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to resolve_full_range"
        );
        results.remove(0)
    }

    async fn resolve_symbol_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(SymbolRange, Vec<u8>, VersionHash), SurgeonError> {
        self.resolve_symbol_range_calls
            .lock()
            .expect("mutex poisoned")
            .push((workspace_root.to_path_buf(), semantic_path.clone()));

        let mut results = self
            .resolve_symbol_range_results
            .lock()
            .expect("mutex poisoned");
        assert!(
            !results.is_empty(),
            "MockSurgeon: Unexpected call to resolve_symbol_range"
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
            // Default: treat as code (transparent for tests that don't configure this)
            Ok("code".to_owned())
        } else {
            results.remove(0)
        }
    }
}
