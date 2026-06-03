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
}

impl DocumentGuard {
    pub(crate) fn new(
        client: LspClient,
        workspace_root: std::path::PathBuf,
        file_path: std::path::PathBuf,
    ) -> Self {
        Self {
            client,
            workspace_root,
            file_path,
        }
    }
}

impl Drop for DocumentGuard {
    fn drop(&mut self) {
        let client = self.client.clone();
        let workspace = self.workspace_root.clone();
        let path = self.file_path.clone();
        tokio::spawn(async move {
            let _ = client.did_close(&workspace, &path).await;
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
        self.did_open(workspace_root, file_path, content).await?;
        Ok(DocumentGuard::new(
            self.clone(),
            workspace_root.to_path_buf(),
            file_path.to_path_buf(),
        ))
    }

    pub(crate) async fn did_open(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        content: &str,
    ) -> Result<(), LspError> {
        tracing::debug!(file = %file_path.display(), "LSP: did_open");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        self.doc_versions
            .insert(file_uri.to_string(), std::sync::atomic::AtomicI32::new(1));

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
        Ok(())
    }

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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::client::tests::make_running_client;
    use std::path::Path;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn test_did_open_sends_notification_and_tracks_version() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/main.rs");

        let result = client.did_open(workspace, file_path, "fn main() {}").await;
        assert!(result.is_ok(), "did_open should succeed: {result:?}");

        let notifications = fake.take_notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, "textDocument/didOpen");

        let file_uri = Url::from_file_path(workspace.join(file_path))
            .unwrap()
            .to_string();
        assert!(
            client.doc_versions.contains_key(&file_uri),
            "doc_versions should contain the opened file"
        );
    }

    #[tokio::test]
    async fn test_did_close_sends_notification_and_removes_version() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/main.rs");

        client
            .did_open(workspace, file_path, "fn main() {}")
            .await
            .unwrap();
        fake.take_notifications();

        let result = client.did_close(workspace, file_path).await;
        assert!(result.is_ok(), "did_close should succeed: {result:?}");

        let notifications = fake.take_notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, "textDocument/didClose");

        let file_uri = Url::from_file_path(workspace.join(file_path))
            .unwrap()
            .to_string();
        assert!(
            !client.doc_versions.contains_key(&file_uri),
            "doc_versions should not contain the closed file"
        );
    }

    #[tokio::test]
    async fn test_open_document_returns_document_guard() {
        let (client, _fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/main.rs");

        let guard = client
            .open_document(workspace, file_path, "fn main() {}")
            .await;
        assert!(guard.is_ok(), "open_document should return guard");
    }

    #[tokio::test]
    async fn test_document_guard_drop_sends_did_close() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/main.rs");

        {
            let _guard = client
                .open_document(workspace, file_path, "fn main() {}")
                .await
                .unwrap();
            fake.take_notifications();
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let notifications = fake.take_notifications();
        assert!(
            notifications
                .iter()
                .any(|(m, _)| m == "textDocument/didClose"),
            "DocumentGuard drop should send did_close: {notifications:?}"
        );
    }

    #[tokio::test]
    async fn test_did_open_unknown_extension_returns_no_lsp() {
        let (client, _fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/main.xyz");

        let result = client.did_open(workspace, file_path, "content").await;
        assert!(
            matches!(result, Err(LspError::NoLspAvailable)),
            "unknown extension should return NoLspAvailable: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_did_close_removes_doc_version_even_if_notify_fails() {
        let (client, fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/main.rs");

        client
            .did_open(workspace, file_path, "fn main() {}")
            .await
            .unwrap();
        fake.take_notifications();

        fake.kill();

        let result = client.did_close(workspace, file_path).await;
        assert!(
            result.is_err(),
            "did_close should fail when transport is dead"
        );

        let file_uri = Url::from_file_path(workspace.join(file_path))
            .unwrap()
            .to_string();
        assert!(
            !client.doc_versions.contains_key(&file_uri),
            "doc_versions should be removed even if notify fails"
        );
    }

    #[tokio::test]
    async fn test_doc_versions_inserted_on_did_open() {
        let (client, _fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/lib.rs");

        client
            .did_open(workspace, file_path, "pub fn hello() {}")
            .await
            .unwrap();

        let file_uri = Url::from_file_path(workspace.join(file_path))
            .unwrap()
            .to_string();
        let version = client.doc_versions.get(&file_uri).unwrap();
        assert_eq!(
            version.load(Ordering::Relaxed),
            1,
            "version should be 1 on open"
        );
    }

    #[tokio::test]
    async fn test_doc_versions_removed_on_did_close() {
        let (client, _fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/lib.rs");

        client
            .did_open(workspace, file_path, "pub fn hello() {}")
            .await
            .unwrap();

        let file_uri = Url::from_file_path(workspace.join(file_path))
            .unwrap()
            .to_string();
        assert!(client.doc_versions.contains_key(&file_uri));

        client.did_close(workspace, file_path).await.unwrap();
        assert!(!client.doc_versions.contains_key(&file_uri));
    }

    #[tokio::test]
    async fn test_multiple_opens_track_latest_version() {
        let (client, _fake) = make_running_client("rust");

        let workspace = Path::new("/workspace");
        let file_path = Path::new("src/lib.rs");

        client.did_open(workspace, file_path, "v1").await.unwrap();

        let file_uri = Url::from_file_path(workspace.join(file_path))
            .unwrap()
            .to_string();
        let v1 = client
            .doc_versions
            .get(&file_uri)
            .unwrap()
            .load(Ordering::Relaxed);
        assert_eq!(v1, 1);

        client.did_close(workspace, file_path).await.unwrap();
        client.did_open(workspace, file_path, "v2").await.unwrap();

        let v2 = client
            .doc_versions
            .get(&file_uri)
            .unwrap()
            .load(Ordering::Relaxed);
        assert_eq!(v2, 1, "version should reset to 1 on re-open");
    }

    #[tokio::test]
    async fn test_lawyer_did_open_no_lsp() {
        let (client, _fake) = make_running_client("rust");

        let result = client
            .did_open(
                Path::new("/workspace"),
                Path::new("src/main.xyz"),
                "fn main() {}",
            )
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }
}
