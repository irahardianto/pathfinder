//! `Lawyer` trait implementation for `LspClient`.
//!
//! Implements the full `Lawyer` trait by delegating to `LspClient`'s
//! internal methods and response parsers.

use crate::client::detect::language_id_for_extension;
use crate::client::LspClient;
use crate::types::{CallHierarchyCall, CallHierarchyItem, ReferenceLocation};
use crate::{DefinitionLocation, Lawyer, LspError};
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};
use url::Url;

#[async_trait]
impl Lawyer for LspClient {
    fn warm_start_for_languages(
        &self,
        language_ids: &[String],
    ) -> Vec<tokio::task::JoinHandle<()>> {
        LspClient::warm_start_for_languages(self, language_ids)
    }

    fn touch_language(&self, language_id: &str) {
        LspClient::touch_language(self, language_id);
    }

    async fn open_document(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
    ) -> Result<Box<dyn crate::lawyer::DocumentLease>, LspError> {
        let guard = LspClient::open_document(self, workspace_root, file_path, content).await?;
        Ok(Box::new(guard))
    }

    async fn goto_definition(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Option<DefinitionLocation>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "goto_definition", file = %file_path.display(), "LSP operation started");

        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;

        self.ensure_process(language_id).await?;

        // M-10: Check capability before sending request.
        let caps = self.capabilities_for(language_id)?;
        if !caps.definition_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "definitionProvider".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),
                "character": column.saturating_sub(1)
            }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/definition",
                params,
                Duration::from_secs(10),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "goto_definition", language = language_id, error = %e, "textDocument/definition failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            tool = "get_definition",
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/definition complete"
        );

        crate::client::response_parsers::parse_definition_response(response, workspace_root).await
    }

    async fn call_hierarchy_prepare(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<CallHierarchyItem>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "call_hierarchy_prepare", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let caps = self.capabilities_for(language_id)?;
        if !caps.call_hierarchy_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "callHierarchyProvider".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),
                "character": column.saturating_sub(1)
            }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/prepareCallHierarchy",
                params,
                Duration::from_secs(5),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "call_hierarchy_prepare", language = language_id, error = %e, "textDocument/prepareCallHierarchy failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/prepareCallHierarchy complete"
        );

        crate::client::response_parsers::parse_call_hierarchy_prepare_response(
            &response,
            workspace_root,
        )
    }

    async fn call_hierarchy_incoming(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        self.call_hierarchy_request(
            workspace_root,
            item,
            "call_hierarchy_incoming",
            "callHierarchy/incomingCalls",
            "from",
            "fromRanges",
        )
        .await
    }

    async fn call_hierarchy_outgoing(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        self.call_hierarchy_request(
            workspace_root,
            item,
            "call_hierarchy_outgoing",
            "callHierarchy/outgoingCalls",
            "to",
            "fromRanges",
        )
        .await
    }

    async fn references(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<ReferenceLocation>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "references", file = %file_path.display(), "LSP operation started");

        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;

        self.ensure_process(language_id).await?;

        // M-10: references uses the same definitionProvider capability.
        let caps = self.capabilities_for(language_id)?;
        if !caps.definition_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "definitionProvider (required for references)".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),
                "character": column.saturating_sub(1)
            },
            "context": { "includeDeclaration": true }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/references",
                params,
                Duration::from_secs(30),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "references", language = language_id, error = %e, "textDocument/references failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            tool = "references",
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/references complete"
        );

        crate::client::response_parsers::parse_references_response(&response, workspace_root).await
    }

    async fn goto_implementation(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<DefinitionLocation>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "goto_implementation", file = %file_path.display(), "LSP operation started");

        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;

        self.ensure_process(language_id).await?;

        // M-10: implementation uses definitionProvider capability.
        let caps = self.capabilities_for(language_id)?;
        if !caps.definition_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "definitionProvider (required for goto_implementation)".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),
                "character": column.saturating_sub(1)
            }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/implementation",
                params,
                Duration::from_secs(10),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "goto_implementation", language = language_id, error = %e, "textDocument/implementation failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            tool = "goto_implementation",
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/implementation complete"
        );

        Ok(
            crate::client::response_parsers::parse_definition_response_multi(
                &response,
                workspace_root,
            )
            .await,
        )
    }

    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, crate::types::LspLanguageStatus> {
        let mut status = std::collections::HashMap::new();
        for desc in self.descriptors.iter() {
            let lang_status = self.processes.get(&desc.language_id).map_or_else(
                || crate::types::LspLanguageStatus {
                    validation: true,
                    reason: format!("{} available (lazy start)", desc.command),
                    navigation_ready: None,
                    diagnostics_strategy: None,
                    indexing_complete: None,
                    uptime_seconds: None,
                    supports_definition: None,
                    supports_call_hierarchy: None,
                    supports_diagnostics: None,
                    supports_formatting: None,
                    server_name: None,
                    indexing_source: None,
                    indexing_duration_secs: None,
                    indexing_progress_percent: None,
                },
                |entry| entry.to_validation_status(&desc.command),
            );
            status.insert(desc.language_id.clone(), lang_status);
        }
        status
    }

    fn missing_languages(&self) -> Vec<crate::client::MissingLanguage> {
        self.missing_languages.iter().cloned().collect()
    }

    async fn force_respawn(&self, language_id: &str) -> Result<(), LspError> {
        LspClient::force_respawn(self, language_id).await
    }

    fn is_warm_start_complete(&self) -> bool {
        self.warm_start_complete
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn warm_start_for_languages_and_track(&self, language_ids: &[String]) {
        LspClient::warm_start_for_languages_and_track(self, language_ids);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::client::fake_transport::FakeTransport;
    use crate::client::tests::make_running_client;
    use std::sync::Arc;

    fn make_running_client_with_caps(language_id: &str) -> (LspClient, Arc<FakeTransport>) {
        let (client, fake) = make_running_client(language_id);

        if let Some(entry) = client.processes.get(language_id) {
            if let crate::client::ProcessEntry::Running(state) = entry.value() {
                let mut caps = state.live_capabilities.write();
                caps.call_hierarchy_provider = true;
                caps.definition_provider = true;
            }
        }

        (client, fake)
    }

    #[tokio::test]
    async fn test_lawyer_goto_definition_with_location_response() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        std::fs::create_dir_all(workspace.join("src")).ok();

        fake.set_response(
            "textDocument/definition",
            serde_json::json!({
                "result": {
                    "uri": "file:///workspace/src/auth.rs",
                    "range": {
                        "start": { "line": 41, "character": 4 },
                        "end": { "line": 41, "character": 9 }
                    }
                }
            }),
        );

        let result = client
            .goto_definition(workspace, Path::new("src/main.rs"), 10, 5)
            .await;

        assert!(result.is_ok(), "goto_definition should succeed: {result:?}");
        let loc = result.unwrap();
        assert!(loc.is_some(), "should return a location");
        let loc = loc.unwrap();
        assert_eq!(loc.line, 42);
        assert_eq!(loc.column, 5);
    }

    #[tokio::test]
    async fn test_lawyer_goto_definition_with_null_response() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");

        fake.set_response(
            "textDocument/definition",
            serde_json::json!({ "result": null }),
        );

        let result = client
            .goto_definition(workspace, Path::new("src/main.rs"), 10, 5)
            .await;

        assert!(result.is_ok(), "goto_definition should succeed: {result:?}");
        assert!(
            result.unwrap().is_none(),
            "null response should return None"
        );
    }

    #[tokio::test]
    async fn test_lawyer_goto_definition_with_array_response() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");

        fake.set_response(
            "textDocument/definition",
            serde_json::json!({
                "result": [{
                    "uri": "file:///workspace/src/lib.rs",
                    "range": {
                        "start": { "line": 9, "character": 0 },
                        "end": { "line": 9, "character": 5 }
                    }
                }]
            }),
        );

        let result = client
            .goto_definition(workspace, Path::new("src/main.rs"), 10, 5)
            .await;

        assert!(result.is_ok(), "goto_definition should succeed: {result:?}");
        let loc = result.unwrap();
        assert!(loc.is_some(), "array response should return first location");
        let loc = loc.unwrap();
        assert_eq!(loc.line, 10);
    }

    #[tokio::test]
    async fn test_lawyer_call_hierarchy_prepare_with_items() {
        let (client, fake) = make_running_client_with_caps("rust");

        let workspace = Path::new("/workspace");
        std::fs::create_dir_all(workspace.join("src")).ok();
        let file_path = workspace.join("src/main.rs");
        std::fs::write(&file_path, "fn main() {}").ok();

        let file_uri = Url::from_file_path(&file_path).unwrap().to_string();

        fake.set_response(
            "textDocument/prepareCallHierarchy",
            serde_json::json!({
                "result": [{
                    "name": "main",
                    "kind": 12,
                    "detail": "fn()",
                    "uri": file_uri,
                    "selectionRange": {
                        "start": { "line": 0, "character": 2 },
                        "end": { "line": 0, "character": 6 }
                    }
                }]
            }),
        );

        let result = client
            .call_hierarchy_prepare(workspace, Path::new("src/main.rs"), 1, 3)
            .await;

        assert!(
            result.is_ok(),
            "call_hierarchy_prepare should succeed: {result:?}"
        );
        let items = result.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "main");
        assert_eq!(items[0].kind, "function");

        let _ = std::fs::remove_file(&file_path);
    }

    #[tokio::test]
    async fn test_lawyer_call_hierarchy_incoming_with_calls() {
        let (client, fake) = make_running_client_with_caps("rust");

        let workspace = Path::new("/workspace");
        std::fs::create_dir_all(workspace.join("src")).ok();
        let caller_file = workspace.join("src/caller.rs");
        std::fs::write(&caller_file, "fn caller() {}").ok();

        let caller_uri = Url::from_file_path(&caller_file).unwrap().to_string();

        fake.set_response(
            "callHierarchy/incomingCalls",
            serde_json::json!({
                "result": [{
                    "from": {
                        "name": "caller",
                        "kind": 12,
                        "uri": caller_uri,
                        "selectionRange": {
                            "start": { "line": 0, "character": 2 },
                            "end": { "line": 0, "character": 8 }
                        }
                    },
                    "fromRanges": [
                        { "start": { "line": 5 }, "end": { "line": 5 } }
                    ]
                }]
            }),
        );

        let item = CallHierarchyItem {
            name: "main".to_owned(),
            kind: "function".to_owned(),
            detail: None,
            file: "src/main.rs".to_owned(),
            line: 1,
            column: 1,
            data: Some(serde_json::json!({"uri": "file:///test", "range": {"start": {"line": 0}}})),
        };

        let result = client.call_hierarchy_incoming(workspace, &item).await;

        assert!(
            result.is_ok(),
            "call_hierarchy_incoming should succeed: {result:?}"
        );
        let calls = result.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].item.name, "caller");
        assert_eq!(calls[0].call_sites, vec![6]);

        let _ = std::fs::remove_file(&caller_file);
    }

    #[tokio::test]
    async fn test_lawyer_call_hierarchy_outgoing_with_calls() {
        let (client, fake) = make_running_client_with_caps("rust");

        let workspace = Path::new("/workspace");
        std::fs::create_dir_all(workspace.join("src")).ok();
        let callee_file = workspace.join("src/callee.rs");
        std::fs::write(&callee_file, "fn callee() {}").ok();

        let callee_uri = Url::from_file_path(&callee_file).unwrap().to_string();

        fake.set_response(
            "callHierarchy/outgoingCalls",
            serde_json::json!({
                "result": [{
                    "to": {
                        "name": "callee",
                        "kind": 12,
                        "uri": callee_uri,
                        "selectionRange": {
                            "start": { "line": 0, "character": 2 },
                            "end": { "line": 0, "character": 8 }
                        }
                    },
                    "fromRanges": [
                        { "start": { "line": 10 }, "end": { "line": 10 } }
                    ]
                }]
            }),
        );

        let item = CallHierarchyItem {
            name: "main".to_owned(),
            kind: "function".to_owned(),
            detail: None,
            file: "src/main.rs".to_owned(),
            line: 1,
            column: 1,
            data: Some(serde_json::json!({"uri": "file:///test", "range": {"start": {"line": 0}}})),
        };

        let result = client.call_hierarchy_outgoing(workspace, &item).await;

        assert!(
            result.is_ok(),
            "call_hierarchy_outgoing should succeed: {result:?}"
        );
        let calls = result.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].item.name, "callee");
        assert_eq!(calls[0].call_sites, vec![11]);

        let _ = std::fs::remove_file(&callee_file);
    }

    #[tokio::test]
    async fn test_lawyer_references_with_locations() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        std::fs::create_dir_all(workspace.join("src")).ok();
        let file_path = workspace.join("src/main.rs");
        std::fs::write(&file_path, "fn main() { main(); }").ok();

        let file_uri = Url::from_file_path(&file_path).unwrap().to_string();

        fake.set_response(
            "textDocument/references",
            serde_json::json!({
                "result": [
                    {
                        "uri": file_uri,
                        "range": {
                            "start": { "line": 0, "character": 3 },
                            "end": { "line": 0, "character": 7 }
                        }
                    },
                    {
                        "uri": file_uri,
                        "range": {
                            "start": { "line": 0, "character": 13 },
                            "end": { "line": 0, "character": 17 }
                        }
                    }
                ]
            }),
        );

        let result = client
            .references(workspace, Path::new("src/main.rs"), 1, 4)
            .await;

        assert!(result.is_ok(), "references should succeed: {result:?}");
        let refs = result.unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].line, 1);
        assert_eq!(refs[1].line, 1);

        let _ = std::fs::remove_file(&file_path);
    }

    #[tokio::test]
    async fn test_lawyer_goto_implementation_with_locations() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");

        fake.set_response(
            "textDocument/implementation",
            serde_json::json!({
                "result": [{
                    "uri": "file:///workspace/src/impl.rs",
                    "range": {
                        "start": { "line": 5, "character": 0 },
                        "end": { "line": 5, "character": 10 }
                    }
                }]
            }),
        );

        let result = client
            .goto_implementation(workspace, Path::new("src/main.rs"), 10, 5)
            .await;

        assert!(
            result.is_ok(),
            "goto_implementation should succeed: {result:?}"
        );
        let locs = result.unwrap();
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].line, 6);
    }

    #[tokio::test]
    async fn test_lawyer_goto_definition_no_lsp() {
        let (client, _fake) = make_running_client("rust");

        let result = client
            .goto_definition(Path::new("/workspace"), Path::new("src/main.xyz"), 1, 1)
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_call_hierarchy_prepare_no_lsp() {
        let (client, _fake) = make_running_client("rust");

        let result = client
            .call_hierarchy_prepare(Path::new("/workspace"), Path::new("src/main.xyz"), 1, 1)
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }
}
