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

        self.wait_for_capability(
            language_id,
            |caps| caps.definition_provider,
            "definitionProvider",
        )
        .await?;

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

        self.wait_for_capability(
            language_id,
            |caps| caps.call_hierarchy_provider,
            "callHierarchyProvider",
        )
        .await?;

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

        self.wait_for_capability(
            language_id,
            |caps| caps.references_provider,
            "referencesProvider",
        )
        .await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),
                "character": column.saturating_sub(1)
            },
            // includeDeclaration: false — the definition site is surfaced separately
            // via `definition_site` in the find_all_references tool response.
            // Including it here causes a critical failure mode: when the LSP is in a
            // warmup/partially-indexed state and returns only 1 result, that single
            // result is the definition itself. The caller then returns it as an
            // authoritative reference list, silently giving the agent wrong data.
            "context": { "includeDeclaration": false }
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

        self.wait_for_capability(
            language_id,
            |caps| caps.implementation_provider,
            "implementationProvider",
        )
        .await?;

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
                    registrations_received: None,
                },
                |entry| entry.to_validation_status(&desc.command, &desc.language_id),
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
#[path = "lawyer_impl_test.rs"]
mod tests;
