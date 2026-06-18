//! Document lifecycle management for LSP operations.
//!
//! Provides RAII guard (`DocumentGuard`) that ensures `did_close` is always
//! sent, even if the caller panics or returns early.

use crate::client::detect::language_id_for_extension;
use crate::client::LspClient;
use crate::client::ProcessEntry;
use crate::LspError;
use serde_json::json;
use url::Url;

pub struct DocumentGuard {
    pub(crate) client: LspClient,
    pub(crate) workspace_root: std::path::PathBuf,
    pub(crate) file_path: std::path::PathBuf,
    /// Whether this guard actually sent `didOpen` (true) or was dedup'd (false).
    /// Dedup'd guards skip `didClose` on drop to prevent protocol violations.
    pub(crate) owns_open: bool,
}

impl DocumentGuard {
    pub(crate) fn new(
        client: LspClient,
        workspace_root: std::path::PathBuf,
        file_path: std::path::PathBuf,
        owns_open: bool,
    ) -> Self {
        Self {
            client,
            workspace_root,
            file_path,
            owns_open,
        }
    }
}

impl Drop for DocumentGuard {
    fn drop(&mut self) {
        // Dedup'd guards (owns_open=false) must not send didClose — the owning
        // guard will handle cleanup. Sending didClose from a dedup'd guard would
        // cause a protocol violation (didClose for a document that the LSP still
        // considers open from the owning guard's perspective).
        if !self.owns_open {
            return;
        }

        let client = self.client.clone();
        let workspace = self.workspace_root.clone();
        let path = self.file_path.clone();

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(language_id) = language_id_for_extension(ext) else {
            return;
        };

        let Ok(file_uri) = Url::from_file_path(workspace.join(&path)) else {
            return;
        };

        client.doc_versions.remove(file_uri.as_str());

        let has_running = client
            .processes
            .get(language_id)
            .is_some_and(|e| matches!(e.value(), ProcessEntry::Running(_)));

        if has_running {
            let params = json!({
                "textDocument": { "uri": file_uri.as_str() }
            });
            let language_id = language_id.to_owned();
            tokio::spawn(async move {
                let _ = client
                    .notify(&language_id, "textDocument/didClose", params)
                    .await;
            });
        }
    }
}

impl crate::lawyer::DocumentLease for DocumentGuard {}

impl LspClient {
    /// Open a document and return a `DocumentGuard` that auto-closes it.
    ///
    /// # IW-3
    ///
    /// This is the preferred way to open documents for transient LSP queries.
    /// The returned guard calls `did_close` when it goes out of scope, ensuring
    /// no document leaks regardless of early returns or panics.
    ///
    /// # Errors
    /// Returns `Err` if `did_open` fails (process not running, I/O error, etc.).
    pub async fn open_document(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        content: &str,
    ) -> Result<DocumentGuard, LspError> {
        let actually_opened = self.did_open(workspace_root, file_path, content).await?;
        Ok(DocumentGuard::new(
            self.clone(),
            workspace_root.to_path_buf(),
            file_path.to_path_buf(),
            actually_opened,
        ))
    }

    /// Returns `true` if `didOpen` was actually sent, `false` if dedup'd.
    pub(crate) async fn did_open(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        content: &str,
    ) -> Result<bool, LspError> {
        tracing::debug!(file = %file_path.display(), "LSP: did_open");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        // Deduplication: if this document is already open (e.g., symbol_overview
        // opens the file and then find_all_references_impl tries to open it again),
        // skip sending a second didOpen. Sending duplicate didOpen without an
        // intervening didClose is an LSP protocol violation that can cause
        // undefined behavior in jdtls and other strict LSP servers.
        if self.doc_versions.contains_key(file_uri.as_str()) {
            tracing::debug!(
                file = %file_path.display(),
                "LSP: did_open skipped — document already open (dedup)"
            );
            return Ok(false);
        }

        self.doc_versions.insert(
            file_uri.to_string(),
            (language_id.to_owned(), std::sync::atomic::AtomicI32::new(1)),
        );

        let params = json!({
            "textDocument": {
                "uri": file_uri.as_str(),
                "languageId": language_id,
                "version": 1,
                "text": content
            }
        });

        if let Err(e) = self
            .notify(language_id, "textDocument/didOpen", params)
            .await
        {
            tracing::error!(language = language_id, error = %e, "textDocument/didOpen failed");
            self.doc_versions.remove(&file_uri.to_string());
            return Err(e);
        }
        self.touch(language_id);
        Ok(true)
    }

    #[allow(dead_code)] // Used by tests and available as pub(crate) API
    pub(crate) async fn did_close(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
    ) -> Result<(), LspError> {
        tracing::debug!(file = %file_path.display(), "LSP: did_close");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        // Always clean up doc_versions regardless of process state.
        self.doc_versions.remove(&file_uri.to_string());

        // Check if process is already running WITHOUT spawning a new one.
        // If the LSP crashed, re-spawning just to send didClose is wasteful —
        // the new instance doesn't know about this document anyway.
        let has_running = self
            .processes
            .get(language_id)
            .is_some_and(|e| matches!(e.value(), ProcessEntry::Running(_)));

        if !has_running {
            tracing::debug!(
                file = %file_path.display(),
                "LSP: did_close skipped notify — process not running, doc_versions cleaned up"
            );
            return Ok(());
        }

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() }
        });

        if let Err(e) = self
            .notify(language_id, "textDocument/didClose", params)
            .await
        {
            tracing::error!(language = language_id, error = %e, "textDocument/didClose failed");
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "document_test.rs"]
mod tests;
