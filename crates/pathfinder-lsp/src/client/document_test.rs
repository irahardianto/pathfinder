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
    let entry = client.doc_versions.get(&file_uri).unwrap();
    assert_eq!(
        entry.value().0,
        "rust",
        "language_id should be stored with doc version"
    );
    assert_eq!(
        entry.value().1.load(Ordering::Relaxed),
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
        .value()
        .1
        .load(Ordering::Relaxed);
    assert_eq!(v1, 1);

    client.did_close(workspace, file_path).await.unwrap();
    client.did_open(workspace, file_path, "v2").await.unwrap();

    let v2 = client
        .doc_versions
        .get(&file_uri)
        .unwrap()
        .value()
        .1
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

/// Verifies that calling `did_open` twice on the same file WITHOUT an
/// intervening `did_close` only sends ONE `didOpen` notification.
/// This is the fix for the jdtls protocol-violation bug where
/// `trace(scope="overview")`'s sub-tools each called `open_document` on the same file.
#[tokio::test]
async fn test_did_open_dedup_skips_second_notification() {
    let (client, fake) = make_running_client("rust");

    let workspace = Path::new("/workspace");
    let file_path = Path::new("src/main.rs");

    // First open — should send didOpen, return true
    let opened = client
        .did_open(workspace, file_path, "fn main() {}")
        .await
        .unwrap();
    assert!(
        opened,
        "first did_open should return true (actually opened)"
    );
    let first_notifications = fake.take_notifications();
    assert_eq!(
        first_notifications.len(),
        1,
        "first open should send didOpen"
    );
    assert_eq!(first_notifications[0].0, "textDocument/didOpen");

    // Second open WITHOUT did_close — should be dedup'd, return false
    let opened2 = client
        .did_open(workspace, file_path, "fn main() { updated }")
        .await
        .unwrap();
    assert!(!opened2, "second did_open should return false (dedup'd)");
    let second_notifications = fake.take_notifications();
    assert_eq!(
        second_notifications.len(),
        0,
        "second open without close should be deduplicated — no notification sent"
    );

    // doc_versions should still contain the file (from first open)
    let file_uri = Url::from_file_path(workspace.join(file_path))
        .unwrap()
        .to_string();
    assert!(
        client.doc_versions.contains_key(&file_uri),
        "doc_versions should still track the open file"
    );
}

/// Verifies that `open_document` guard dedup works correctly in the
/// `trace(scope="overview")` scenario: first guard opens, second guard is dedup'd,
/// dropping dedup'd guard does NOT send `didClose`, dropping owning guard DOES.
#[tokio::test]
async fn test_open_document_guard_dedup_in_composite_tool() {
    let (client, fake) = make_running_client("rust");

    let workspace = Path::new("/workspace");
    let file_path = Path::new("src/main.rs");

    // Simulate symbol_overview opening the document
    let guard1 = client
        .open_document(workspace, file_path, "fn main() {}")
        .await
        .unwrap();
    assert!(guard1.owns_open, "first guard should own the open");
    let open_notifications = fake.take_notifications();
    assert_eq!(
        open_notifications.len(),
        1,
        "first guard should send didOpen"
    );

    // Simulate find_all_references_impl trying to open the same document
    let guard2 = client
        .open_document(workspace, file_path, "fn main() {}")
        .await
        .unwrap();
    assert!(!guard2.owns_open, "second guard should NOT own the open");
    let dedup_notifications = fake.take_notifications();
    assert_eq!(
        dedup_notifications.len(),
        0,
        "second guard should be dedup'd — no didOpen sent"
    );

    // Drop guard2 (dedup'd) — must NOT send didClose
    drop(guard2);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let after_guard2_drop = fake.take_notifications();
    assert_eq!(
        after_guard2_drop.len(),
        0,
        "dedup'd guard drop must NOT send didClose"
    );

    // Drop guard1 (owner) — MUST send didClose
    drop(guard1);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let after_guard1_drop = fake.take_notifications();
    assert!(
        after_guard1_drop
            .iter()
            .any(|(m, _)| m == "textDocument/didClose"),
        "owning guard drop should send exactly one didClose: {after_guard1_drop:?}"
    );
    assert_eq!(
        after_guard1_drop.len(),
        1,
        "should be exactly one notification (didClose), not more"
    );
}
