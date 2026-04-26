//! File watcher for synchronous cache eviction.
//!
//! Implements the file watching model from PRD §4.4:
//! - On external file changes: hash + compare, evict cache if mismatch
//! - On Pathfinder-initiated writes: cache updated synchronously (no watcher needed)

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc as tokio_mpsc;

/// Events emitted by the file watcher.
#[derive(Debug, Clone)]
pub enum FileEvent {
    /// A file was modified externally (content hash changed).
    Modified(PathBuf),
    /// A file was created.
    Created(PathBuf),
    /// A file was deleted.
    Deleted(PathBuf),
}

/// File watcher that monitors the workspace for external changes.
///
/// Uses the `notify` crate for cross-platform file system events.
/// The watcher runs in a background thread and sends events via a channel.
pub struct FileWatcher {
    _watcher: RecommendedWatcher,
}

impl FileWatcher {
    /// Start watching the workspace root.
    ///
    /// Returns the watcher and a receiver channel for file events.
    ///
    /// # Errors
    /// Returns an error if the file watcher cannot be initialized.
    pub fn start(
        workspace_root: &Path,
    ) -> Result<(Self, tokio_mpsc::UnboundedReceiver<FileEvent>), FileWatcherError> {
        let (tx, rx) = tokio_mpsc::unbounded_channel();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    for path in &event.paths {
                        let file_event = match event.kind {
                            EventKind::Create(_) => Some(FileEvent::Created(path.clone())),
                            EventKind::Modify(_) => Some(FileEvent::Modified(path.clone())),
                            EventKind::Remove(_) => Some(FileEvent::Deleted(path.clone())),
                            _ => None,
                        };
                        if let Some(fe) = file_event {
                            tracing::debug!(
                                path = %path.display(),
                                kind = ?event.kind,
                                "file event dispatched"
                            );
                            // Log if the send fails (receiver dropped)
                            if let Err(_) = tx.send(fe) {
                                tracing::warn!(
                                    path = %path.display(),
                                    "file watcher: channel send failed - receiver dropped, cache may be stale"
                                );
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "file watcher received error event");
                }
            }
        })
        .map_err(FileWatcherError::InitFailed)?;

        watcher
            .watch(workspace_root, RecursiveMode::Recursive)
            .map_err(FileWatcherError::WatchFailed)?;

        tracing::info!(
            workspace = %workspace_root.display(),
            "File watcher started"
        );

        Ok((Self { _watcher: watcher }, rx))
    }
}

/// Abstraction over file-system event sources.
///
/// Implement this trait to provide a test double that injects events
/// without requiring OS file-system infrastructure.
pub trait FileEventSource: Send + 'static {
    /// Drain all pending events into `tx`.
    ///
    /// Implementations may block, poll, or iterate a pre-loaded list.
    fn drain(&mut self, tx: &tokio_mpsc::UnboundedSender<FileEvent>);
}

/// In-memory test double for [`FileEventSource`].
///
/// Pre-load events via [`InMemoryFileEventSource::new`] and call
/// [`drain`](FileEventSource::drain) to publish them. Useful for unit
/// tests that must verify event-handling behaviour without OS support.
pub struct InMemoryFileEventSource {
    events: Vec<FileEvent>,
}

impl InMemoryFileEventSource {
    /// Create a new source pre-loaded with `events`.
    #[must_use]
    pub fn new(events: Vec<FileEvent>) -> Self {
        Self { events }
    }
}

impl FileEventSource for InMemoryFileEventSource {
    fn drain(&mut self, tx: &tokio_mpsc::UnboundedSender<FileEvent>) {
        for event in self.events.drain(..) {
            // Best-effort — receiver may have been dropped in tests
            let _ = tx.send(event);
        }
    }
}

/// Compute the SHA-256 hash of a file's content incrementally.
///
/// Used for hash-compare cache eviction: when a file watcher event fires,
/// we hash the current content and compare to the cached hash.
///
/// Reads the file in chunks to bound memory usage and formats the result
/// consistently with `VersionHash`.
///
/// # Errors
/// Returns an error if the file cannot be read.
pub async fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let hash = hasher.finalize();
    Ok(format!("sha256:{hash:x}"))
}

/// File watcher errors.
#[derive(Debug, thiserror::Error)]
pub enum FileWatcherError {
    #[error("failed to initialize file watcher: {0}")]
    InitFailed(notify::Error),

    #[error("failed to watch directory: {0}")]
    WatchFailed(notify::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_hash_file_produces_sha256() {
        let temp = std::env::temp_dir().join("pathfinder_hash_test");
        let _ = std::fs::create_dir_all(&temp);
        let file_path = temp.join("test.txt");
        std::fs::write(&file_path, "hello world").expect("should write");

        let hash = hash_file(&file_path).await.expect("should hash");
        assert!(hash.starts_with("sha256:"));
        // SHA-256 of "hello world" is deterministic
        assert!(hash.contains("b94d27b9934d3e08a52e52d7"));

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn test_hash_file_different_content_different_hash() {
        let temp = std::env::temp_dir().join("pathfinder_hash_diff_test");
        let _ = std::fs::create_dir_all(&temp);

        let file1 = temp.join("a.txt");
        let file2 = temp.join("b.txt");
        std::fs::write(&file1, "content A").expect("should write");
        std::fs::write(&file2, "content B").expect("should write");

        let hash1 = hash_file(&file1).await.expect("should hash");
        let hash2 = hash_file(&file2).await.expect("should hash");
        assert_ne!(hash1, hash2);

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn test_hash_file_missing_returns_error() {
        let result = hash_file(Path::new("/nonexistent/file.txt")).await;
        assert!(result.is_err());
    }

    // ── In-memory FileEventSource ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_in_memory_source_drains_all_events() {
        let events = vec![
            FileEvent::Created(PathBuf::from("src/new_file.rs")),
            FileEvent::Modified(PathBuf::from("src/lib.rs")),
            FileEvent::Deleted(PathBuf::from("src/old_file.rs")),
        ];
        let mut source = InMemoryFileEventSource::new(events);

        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        source.drain(&tx);

        // All three events should be available immediately
        let e1 = rx.try_recv().expect("created event");
        let e2 = rx.try_recv().expect("modified event");
        let e3 = rx.try_recv().expect("deleted event");
        assert!(matches!(e1, FileEvent::Created(_)));
        assert!(matches!(e2, FileEvent::Modified(_)));
        assert!(matches!(e3, FileEvent::Deleted(_)));
        assert!(
            rx.try_recv().is_err(),
            "channel should be empty after drain"
        );
    }

    #[tokio::test]
    async fn test_in_memory_source_drain_is_idempotent() {
        // After a drain, a second drain should emit nothing.
        let events = vec![FileEvent::Created(PathBuf::from("file.rs"))];
        let mut source = InMemoryFileEventSource::new(events);

        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        source.drain(&tx);
        source.drain(&tx); // second drain — must be empty

        let _ = rx.try_recv().expect("first event present");
        assert!(
            rx.try_recv().is_err(),
            "second drain should produce no new events"
        );
    }
}
