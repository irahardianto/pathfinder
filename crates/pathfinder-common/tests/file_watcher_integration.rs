#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args
)]

//! Integration tests for `FileWatcher` using real OS filesystem events.
//!
//! These tests use `tempfile::tempdir()` to create isolated workspaces and
//! perform real filesystem operations. The OS file watcher (`notify` crate)
//! must detect the changes within a reasonable timeout.
//!
//! # Design
//!
//! Unlike unit tests (which inject fake events), these tests exercise the
//! `notify` watcher backend directly — confirming that the event channel,
//! path reporting, and event kind classification all work on the target OS.
//!
//! # Timing
//!
//! File watcher events are OS-driven and may be delayed by kernel batching.
//! Each test uses a 5-second timeout to accommodate slow CI environments.
//! Do NOT tighten this timeout without verifying behavior on Linux and macOS.
//!
//! # Future agents
//!
//! Add new test cases here when adding new `FileEvent` variants or changing
//! the event dispatch logic in `file_watcher.rs`. Keep each test focused on
//! a single event type.

use pathfinder_common::file_watcher::{FileEvent, FileWatcher};
use std::time::Duration;
use tokio::time::timeout;

const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Helper: extract the path from any `FileEvent` variant.
fn event_path(event: &FileEvent) -> &std::path::Path {
    match event {
        FileEvent::Created(p) | FileEvent::Modified(p) | FileEvent::Deleted(p) => p.as_path(),
    }
}

/// Verify that creating a new file triggers a `FileEvent::Created` event.
#[tokio::test]
async fn test_file_watcher_detects_file_creation() {
    let workspace = tempfile::tempdir().expect("failed to create tempdir");
    let root = workspace.path();

    let (_watcher, mut rx) = FileWatcher::start(root).expect("FileWatcher::start failed");

    // Give the watcher a moment to register before writing the file.
    tokio::time::sleep(Duration::from_millis(100)).await;

    tokio::fs::write(root.join("new_file.rs"), "fn foo() {}")
        .await
        .expect("failed to write file");

    let event = timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for Create event")
        .expect("channel closed unexpectedly");

    // The event path should reference the created file.
    assert!(
        event_path(&event).ends_with("new_file.rs"),
        "Create event path should end with new_file.rs, got: {:?}",
        event_path(&event)
    );
}

/// Verify that modifying an existing file triggers a `FileEvent::Modified` event.
#[tokio::test]
async fn test_file_watcher_detects_file_modification() {
    let workspace = tempfile::tempdir().expect("failed to create tempdir");
    let root = workspace.path();

    // Pre-create the file so we test modification, not creation.
    let file_path = root.join("existing.rs");
    std::fs::write(&file_path, "initial content").expect("failed to pre-create file");

    let (_watcher, mut rx) = FileWatcher::start(root).expect("FileWatcher::start failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Drain any creation/modification events fired during watcher startup.
    // OS backends may re-emit events for pre-existing files.
    while rx.try_recv().is_ok() {}

    tokio::fs::write(&file_path, "modified content")
        .await
        .expect("failed to modify file");

    let event = timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for Modify event")
        .expect("channel closed unexpectedly");

    assert!(
        event_path(&event).ends_with("existing.rs"),
        "event path should reference existing.rs, got: {:?}",
        event_path(&event)
    );
}

/// Verify that deleting a file triggers a `FileEvent::Deleted` event.
#[tokio::test]
async fn test_file_watcher_detects_file_deletion() {
    let workspace = tempfile::tempdir().expect("failed to create tempdir");
    let root = workspace.path();

    let file_path = root.join("to_delete.rs");
    std::fs::write(&file_path, "fn delete_me() {}").expect("failed to create file");

    let (_watcher, mut rx) = FileWatcher::start(root).expect("FileWatcher::start failed");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Drain any initial events from watcher startup.
    while rx.try_recv().is_ok() {}

    tokio::fs::remove_file(&file_path)
        .await
        .expect("failed to delete file");

    let event = timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for Delete event")
        .expect("channel closed unexpectedly");

    assert!(
        event_path(&event).ends_with("to_delete.rs"),
        "Delete event path should reference to_delete.rs, got: {:?}",
        event_path(&event)
    );
}
