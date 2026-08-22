//! Document lifecycle management for LSP operations.
//!
//! Provides RAII guard (`DocumentGuard`) that ensures `did_close` is always
//! sent, even if the caller panics or returns early.

use crate::client::detect::language_id_for_extension;
use crate::client::DocumentEntry;
use crate::client::LspClient;
use crate::client::ProcessEntry;
use crate::LspError;
use serde_json::json;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::Ordering;
use url::Url;

enum OpenAction {
    SendOpen,
    SendChange(i32),
    Dedup,
}

/// RAII guard that manages LSP textDocument/didOpen and textDocument/didClose lifecycle.
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

        let file_uri_str = file_uri.to_string();
        let language_id = language_id.to_owned();

        tokio::spawn(async move {
            let has_running = client
                .processes
                .get(&language_id)
                .is_some_and(|e| matches!(e.value(), ProcessEntry::Running(_)));

            if has_running {
                let params = json!({
                    "textDocument": { "uri": file_uri_str.as_str() }
                });
                let _ = client
                    .notify(&language_id, "textDocument/didClose", params)
                    .await;
            }
            client.doc_versions.remove(&file_uri_str);
        });
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

    /// Returns `true` if `didOpen` was actually sent, `false` if dedup'd or updated via `didChange`.
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

        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        let hash = hasher.finish();

        let file_uri_str = file_uri.to_string();

        let action = match self.doc_versions.entry(file_uri_str.clone()) {
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(DocumentEntry {
                    language_id: language_id.to_owned(),
                    version: std::sync::atomic::AtomicI32::new(1),
                    content_hash: std::sync::atomic::AtomicU64::new(hash),
                });
                OpenAction::SendOpen
            }
            dashmap::mapref::entry::Entry::Occupied(o) => {
                let entry = o.get();
                let prev_hash = entry.content_hash.swap(hash, Ordering::SeqCst);
                if prev_hash == hash {
                    OpenAction::Dedup
                } else {
                    let next_ver = entry.version.fetch_add(1, Ordering::SeqCst) + 1;
                    OpenAction::SendChange(next_ver)
                }
            }
        };

        match action {
            OpenAction::SendOpen => {
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
                    self.doc_versions.remove(&file_uri_str);
                    return Err(e);
                }
                Ok(true)
            }
            OpenAction::SendChange(version) => {
                tracing::debug!(
                    file = %file_path.display(),
                    version,
                    "LSP: content changed on open — sending didChange"
                );
                let params = json!({
                    "textDocument": {
                        "uri": file_uri.as_str(),
                        "version": version
                    },
                    "contentChanges": [
                        { "text": content }
                    ]
                });

                if let Err(e) = self
                    .notify(language_id, "textDocument/didChange", params)
                    .await
                {
                    tracing::error!(language = language_id, error = %e, "textDocument/didChange failed");
                    return Err(e);
                }
                Ok(false)
            }
            OpenAction::Dedup => {
                tracing::debug!(
                    file = %file_path.display(),
                    "LSP: did_open skipped — document already open with identical content (dedup)"
                );
                Ok(false)
            }
        }
    }

    /// Broadcast watched file changes to the LSP server for `language_id`.
    ///
    /// # Errors
    /// Returns `LspError::NoLspAvailable` or `LspError::ConnectionLost` if communication fails.
    pub async fn did_change_watched_files(
        &self,
        language_id: &str,
        changes: Vec<crate::types::FileEvent>,
    ) -> Result<(), LspError> {
        let params = json!({
            "changes": changes
        });
        self.notify(language_id, "workspace/didChangeWatchedFiles", params)
            .await
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

        let file_uri_str = file_uri.to_string();

        // Check if process is already running WITHOUT spawning a new one.
        // If the LSP crashed, re-spawning just to send didClose is wasteful —
        // the new instance doesn't know about this document anyway.
        let has_running = self
            .processes
            .get(language_id)
            .is_some_and(|e| matches!(e.value(), ProcessEntry::Running(_)));

        if has_running {
            let params = json!({
                "textDocument": { "uri": file_uri.as_str() }
            });

            if let Err(e) = self
                .notify(language_id, "textDocument/didClose", params)
                .await
            {
                tracing::error!(language = language_id, error = %e, "textDocument/didClose failed");
                self.doc_versions.remove(&file_uri_str);
                return Err(e);
            }
        }

        self.doc_versions.remove(&file_uri_str);
        Ok(())
    }
}

#[cfg(test)]
#[path = "document_test.rs"]
mod tests;
