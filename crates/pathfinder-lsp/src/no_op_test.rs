use super::*;
use std::path::PathBuf;

fn workspace() -> PathBuf {
    PathBuf::from("/workspace")
}

fn file() -> PathBuf {
    PathBuf::from("src/main.rs")
}

#[tokio::test]
async fn test_no_op_lawyer_is_warm_start_complete() {
    let lawyer = NoOpLawyer;
    assert!(
        !lawyer.is_warm_start_complete(),
        "NoOpLawyer should report warm_start as not complete"
    );
}

#[tokio::test]
async fn test_no_op_lawyer_goto_definition_returns_no_lsp() {
    let lawyer = NoOpLawyer;
    let result = lawyer.goto_definition(&workspace(), &file(), 1, 1).await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_no_op_lawyer_call_hierarchy_prepare_returns_no_lsp() {
    let lawyer = NoOpLawyer;
    let result = lawyer
        .call_hierarchy_prepare(&workspace(), &file(), 1, 1)
        .await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_no_op_lawyer_call_hierarchy_incoming_returns_no_lsp() {
    let lawyer = NoOpLawyer;
    let item = CallHierarchyItem {
        name: "foo".into(),
        kind: "function".into(),
        detail: None,
        file: "src/main.rs".into(),
        line: 1,
        column: 1,
        data: None,
    };
    let result = lawyer.call_hierarchy_incoming(&workspace(), &item).await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_no_op_lawyer_call_hierarchy_outgoing_returns_no_lsp() {
    let lawyer = NoOpLawyer;
    let item = CallHierarchyItem {
        name: "foo".into(),
        kind: "function".into(),
        detail: None,
        file: "src/main.rs".into(),
        line: 1,
        column: 1,
        data: None,
    };
    let result = lawyer.call_hierarchy_outgoing(&workspace(), &item).await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_no_op_lawyer_references_returns_no_lsp() {
    let lawyer = NoOpLawyer;
    let result = lawyer.references(&workspace(), &file(), 1, 1).await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_no_op_lawyer_goto_implementation_returns_no_lsp() {
    let lawyer = NoOpLawyer;
    let result = lawyer
        .goto_implementation(&workspace(), &file(), 1, 1)
        .await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_no_op_lawyer_open_document_returns_no_lsp() {
    let lawyer = NoOpLawyer;
    let result = lawyer
        .open_document(&workspace(), &file(), "fn main() {}")
        .await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_no_op_lawyer_capability_status_returns_empty_map() {
    let lawyer = NoOpLawyer;
    let result = lawyer.capability_status().await;
    assert!(result.is_empty());
}

#[test]
fn test_no_op_lawyer_missing_languages_returns_empty_vec() {
    let lawyer = NoOpLawyer;
    let result = lawyer.missing_languages();
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_no_op_lawyer_force_respawn_returns_no_lsp() {
    let lawyer = NoOpLawyer;
    let result = lawyer.force_respawn("rust").await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}
