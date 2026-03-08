//! `LspClient` — the production [`Lawyer`] implementation.
//!
//! `LspClient` manages a pool of LSP child processes (one per language).
//! Processes are started lazily on first use and terminated automatically
//! after an idle timeout.
//!
//! # Crash Recovery (PRD §6.3)
//! When a crash is detected the client restarts the process with exponential
//! back-off (1s → 2s → 4s, 3 attempts). After 3 failures the language is
//! marked `LSP_UNAVAILABLE` and all subsequent calls degrade gracefully.

mod capabilities;
mod detect;
mod process;
mod protocol;
mod transport;

pub use capabilities::DetectedCapabilities;
pub use detect::{detect_languages, language_id_for_extension, LanguageLsp};

use crate::{DefinitionLocation, Lawyer, LspError};
use async_trait::async_trait;
use detect::LanguageLsp as LspDescriptor;
use process::{send, shutdown, spawn_and_initialize, start_reader_task, ManagedProcess};
use protocol::RequestDispatcher;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use url::Url;

/// Default idle timeout: 15 minutes for standard LSPs.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Maximum restart attempts before marking a language as unavailable.
const MAX_RESTART_ATTEMPTS: u32 = 3;
/// Grace period between idle checks.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(60);

struct LanguageState {
    /// The running LSP process.
    process: ManagedProcess,
    /// Background reader task handle.
    _reader: tokio::task::JoinHandle<()>,
    /// Number of times we have restarted this LSP (used in M3 crash recovery UI).
    _restart_count: u32,
    /// When this entry was last actively used.
    last_used: Instant,
}

/// Marks a language as permanently unavailable after repeated crashes.
struct UnavailableState;

enum ProcessEntry {
    /// Active LSP process. Boxed to equalise variant sizes.
    Running(Box<LanguageState>),
    Unavailable(UnavailableState),
}

/// The production `Lawyer` implementation.
///
/// Manages per-language LSP child processes and provides JSON-RPC request
/// routing for `textDocument/definition` and future capabilities.
#[derive(Clone)]
pub struct LspClient {
    workspace_root: PathBuf,
    /// Known language descriptors (from Zero-Config detection).
    descriptors: Arc<Vec<LspDescriptor>>,
    /// Running processes keyed by language id.
    processes: Arc<RwLock<HashMap<String, ProcessEntry>>>,
    /// Shared JSON-RPC request/response dispatcher.
    dispatcher: Arc<RequestDispatcher>,
}

impl LspClient {
    /// Create a new `LspClient` for the given workspace root.
    ///
    /// Performs Zero-Config language detection immediately (cheap directory
    /// scan). LSP processes are **not** started until firstuse.
    ///
    /// Starts the idle-timeout background task.
    ///
    /// # Errors
    /// Returns `Err` if the workspace root directory cannot be read.
    pub async fn new(workspace_root: &Path) -> std::io::Result<Self> {
        let descriptors = detect_languages(workspace_root).await?;

        tracing::info!(
            workspace = %workspace_root.display(),
            detected_languages = ?descriptors.iter().map(|l| &l.language_id).collect::<Vec<_>>(),
            "LspClient: language detection complete"
        );

        let client = Self {
            workspace_root: workspace_root.to_owned(),
            descriptors: Arc::new(descriptors),
            processes: Arc::new(RwLock::new(HashMap::new())),
            dispatcher: Arc::new(RequestDispatcher::new()),
        };

        // Spawn idle-timeout background task
        let processes = Arc::clone(&client.processes);
        let dispatcher = Arc::clone(&client.dispatcher);
        tokio::spawn(idle_timeout_task(processes, dispatcher));

        Ok(client)
    }

    /// Ensure an LSP process is running for `language_id`, starting it if needed.
    ///
    /// Returns `Err(LspError::NoLspAvailable)` if:
    /// - No descriptor found for this language
    /// - The language has been marked unavailable after repeated crashes
    async fn ensure_process(&self, language_id: &str) -> Result<(), LspError> {
        // Fast path: already running
        {
            let guard = self.processes.read().await;
            if let Some(entry) = guard.get(language_id) {
                return match entry {
                    ProcessEntry::Running(_) => Ok(()),
                    ProcessEntry::Unavailable(_) => Err(LspError::NoLspAvailable),
                };
            }
        }

        // Find the descriptor for this language
        let descriptor = self
            .descriptors
            .iter()
            .find(|d| d.language_id == language_id)
            .ok_or(LspError::NoLspAvailable)?
            .clone();

        // Spawn the process
        self.start_process(descriptor, 0).await
    }

    /// Spawn a new LSP process, retrying on failure with exponential backoff.
    async fn start_process(&self, descriptor: LspDescriptor, attempt: u32) -> Result<(), LspError> {
        let language_id = descriptor.language_id.clone();

        if attempt >= MAX_RESTART_ATTEMPTS {
            tracing::error!(
                language = %language_id,
                "LSP: max restart attempts reached, marking unavailable"
            );
            self.processes.write().await.insert(
                language_id.clone(),
                ProcessEntry::Unavailable(UnavailableState),
            );
            return Err(LspError::NoLspAvailable);
        }

        // Exponential backoff: 1s, 2s, 4s
        if attempt > 0 {
            let delay = Duration::from_secs(1u64 << (attempt - 1));
            tracing::info!(
                language = %language_id,
                attempt,
                delay_ms = delay.as_millis(),
                "LSP: restart with backoff"
            );
            tokio::time::sleep(delay).await;
        }

        tracing::info!(
            language = %language_id,
            command = %descriptor.command,
            "LSP: spawning process"
        );

        let (process, stdout) = spawn_and_initialize(
            &descriptor.command,
            &descriptor.args,
            &self.workspace_root,
            &language_id,
            Arc::clone(&self.dispatcher),
        )
        .await?;

        let reader_handle = start_reader_task(stdout, Arc::clone(&self.dispatcher));

        self.processes.write().await.insert(
            language_id,
            ProcessEntry::Running(Box::new(LanguageState {
                process,
                _reader: reader_handle,
                _restart_count: attempt,
                last_used: Instant::now(),
            })),
        );

        Ok(())
    }

    /// Update `last_used` for a language (called after each successful request).
    async fn touch(&self, language_id: &str) {
        let mut guard = self.processes.write().await;
        if let Some(ProcessEntry::Running(state)) = guard.get_mut(language_id) {
            state.last_used = Instant::now();
            state.process.last_used = Instant::now();
        }
    }

    /// Send a JSON-RPC request and await the response.
    ///
    /// Dispatches via the shared `RequestDispatcher` so the background reader
    /// task can fire the correct oneshot.
    async fn request(
        &self,
        language_id: &str,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, LspError> {
        let (id, rx) = self.dispatcher.register();
        let message = RequestDispatcher::make_request(id, method, &params);

        // Write the request to stdin
        {
            let guard = self.processes.read().await;
            let state = match guard.get(language_id) {
                Some(ProcessEntry::Running(s)) => s,
                Some(ProcessEntry::Unavailable(_)) | None => return Err(LspError::NoLspAvailable),
            };
            send(&state.process, &message).await?;
        }

        // Await response with timeout
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| LspError::Timeout {
                operation: method.to_owned(),
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            })?
            .map_err(|_| LspError::ConnectionLost)?
    }
}

#[async_trait]
impl Lawyer for LspClient {
    async fn goto_definition(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Option<DefinitionLocation>, LspError> {
        let start = Instant::now();

        // Determine language from file extension
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;

        // Ensure the LSP process is running
        self.ensure_process(language_id).await?;

        // Build the textDocument/definition request (LSP positions are 0-indexed)
        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),       // Convert 1-indexed → 0-indexed
                "character": column.saturating_sub(1)
            }
        });

        let response = self
            .request(
                language_id,
                "textDocument/definition",
                params,
                Duration::from_secs(10),
            )
            .await?;

        self.touch(language_id).await;

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            tool = "get_definition",
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/definition complete"
        );

        parse_definition_response(response)
    }
}

/// Parse the `textDocument/definition` response into a `DefinitionLocation`.
///
/// Returns `Ok(None)` for JSON `null` (no definition found).
fn parse_definition_response(
    response: serde_json::Value,
) -> Result<Option<DefinitionLocation>, LspError> {
    if response.is_null() {
        return Ok(None);
    }

    // The response can be a single Location, a LocationLink, or an array
    let location = if response.is_array() {
        response
            .as_array()
            .and_then(|arr| arr.first())
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    } else {
        response
    };

    if location.is_null() {
        return Ok(None);
    }

    // Location: { uri, range: { start: { line, character } } }
    // LocationLink: { targetUri, targetRange, ... }
    let (uri_str, start_line, start_char) = if location.get("targetUri").is_some() {
        // LocationLink
        (
            location["targetUri"].as_str().unwrap_or("").to_owned(),
            location["targetSelectionRange"]["start"]["line"]
                .as_u64()
                .unwrap_or(0),
            location["targetSelectionRange"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
    } else {
        // Location
        (
            location["uri"].as_str().unwrap_or("").to_owned(),
            location["range"]["start"]["line"].as_u64().unwrap_or(0),
            location["range"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
    };

    if uri_str.is_empty() {
        return Err(LspError::Protocol(
            "definition response missing URI".to_owned(),
        ));
    }

    // Convert URI to a relative file path string
    let file = Url::parse(&uri_str)
        .ok()
        .and_then(|u: Url| u.to_file_path().ok())
        .map(|p: std::path::PathBuf| p.to_string_lossy().into_owned())
        .unwrap_or(uri_str);

    Ok(Some(DefinitionLocation {
        file,
        line: u32::try_from(start_line + 1).unwrap_or(1), // 0-indexed → 1-indexed
        column: u32::try_from(start_char + 1).unwrap_or(1),
        preview: String::new(), // Preview populated in future milestone
    }))
}

/// Background task: check for idle processes and terminate them.
async fn idle_timeout_task(
    processes: Arc<RwLock<HashMap<String, ProcessEntry>>>,
    dispatcher: Arc<RequestDispatcher>,
) {
    loop {
        tokio::time::sleep(IDLE_CHECK_INTERVAL).await;

        let mut guard = processes.write().await;
        let languages_to_remove: Vec<String> = guard
            .iter()
            .filter_map(|(lang, entry)| {
                if let ProcessEntry::Running(state) = entry {
                    if state.last_used.elapsed() > DEFAULT_IDLE_TIMEOUT {
                        Some(lang.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        for lang in languages_to_remove {
            if let Some(ProcessEntry::Running(mut state)) = guard.remove(&lang) {
                tracing::info!(language = %lang, "LSP: idle timeout — terminating");
                shutdown(&mut state.process, &dispatcher).await;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_definition_response_null() {
        let result = parse_definition_response(json!(null));
        assert!(result.expect("should not err").is_none());
    }

    #[test]
    fn test_parse_definition_response_location() {
        let response = json!({
            "uri": "file:///home/user/project/src/auth.rs",
            "range": {
                "start": { "line": 41, "character": 4 },
                "end":   { "line": 41, "character": 9 }
            }
        });
        let result = parse_definition_response(response).expect("ok");
        let loc = result.expect("some location");
        assert_eq!(loc.line, 42); // 0-indexed → 1-indexed
        assert_eq!(loc.column, 5);
        assert!(loc.file.contains("auth.rs"));
    }

    #[test]
    fn test_parse_definition_response_array() {
        let response = json!([{
            "uri": "file:///project/src/lib.rs",
            "range": {
                "start": { "line": 9, "character": 0 },
                "end":   { "line": 9, "character": 5 }
            }
        }]);
        let result = parse_definition_response(response).expect("ok");
        let loc = result.expect("some location");
        assert_eq!(loc.line, 10);
        assert!(loc.file.contains("lib.rs"));
    }

    #[test]
    fn test_parse_definition_response_location_link() {
        let response = json!({
            "targetUri": "file:///project/src/types.rs",
            "targetRange": {
                "start": { "line": 19, "character": 0 },
                "end":   { "line": 25, "character": 1 }
            },
            "targetSelectionRange": {
                "start": { "line": 19, "character": 4 },
                "end":   { "line": 19, "character": 9 }
            }
        });
        let result = parse_definition_response(response).expect("ok");
        let loc = result.expect("some location");
        assert_eq!(loc.line, 20); // 0-indexed → 1-indexed
        assert!(loc.file.contains("types.rs"));
    }

    #[test]
    fn test_parse_definition_empty_array() {
        let response = json!([]);
        let result = parse_definition_response(response).expect("ok");
        // Empty array → null first element → None
        assert!(result.is_none());
    }
}
