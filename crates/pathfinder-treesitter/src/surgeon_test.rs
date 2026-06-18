use super::*;
use std::path::Path;
use pathfinder_common::types::{SemanticPath, SymbolScope};

struct DummySurgeon;

#[async_trait::async_trait]
impl Surgeon for DummySurgeon {
    async fn read_symbol_scope(
        &self,
        _workspace_root: &Path,
        _semantic_path: &SemanticPath,
    ) -> Result<SymbolScope, SurgeonError> {
        Err(SurgeonError::SymbolNotFound {
            path: String::new(),
            did_you_mean: vec![],
        })
    }

    async fn extract_symbols(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        Ok(vec![])
    }

    async fn enclosing_symbol(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: usize,
    ) -> Result<Option<String>, SurgeonError> {
        Ok(None)
    }

    async fn node_type_at_position(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: usize,
        _column: usize,
    ) -> Result<String, SurgeonError> {
        Ok("code".to_string())
    }

    async fn generate_skeleton(
        &self,
        _workspace_root: &Path,
        _path: &Path,
        _config: &crate::repo_map::SkeletonConfig<'_>,
    ) -> Result<crate::repo_map::RepoMapResult, SurgeonError> {
        unimplemented!()
    }

    async fn read_source_file(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
    ) -> Result<(String, String, Vec<ExtractedSymbol>), SurgeonError> {
        unimplemented!()
    }
}

#[tokio::test]
async fn test_surgeon_default_implementations() {
    let surgeon = DummySurgeon;
    let path = Path::new("dummy");

    // Test extract_symbols_preloaded default impl
    let res = surgeon
        .extract_symbols_preloaded(
            path,
            path,
            std::sync::Arc::new([]),
            std::time::SystemTime::now(),
        )
        .await;
    assert!(res.is_ok());

    // Test enclosing_symbol_detail default impl
    let detail = surgeon.enclosing_symbol_detail(path, path, 1).await;
    assert!(detail.is_ok());
    assert!(detail.unwrap().is_none());
}
