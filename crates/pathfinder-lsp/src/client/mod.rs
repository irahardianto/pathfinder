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

use crate::types::{CallHierarchyCall, CallHierarchyItem, LspDiagnostic, LspDiagnosticSeverity};
use crate::{DefinitionLocation, Lawyer, LspError};
use async_trait::async_trait;
use detect::LanguageLsp as LspDescriptor;
use process::{send, shutdown, spawn_and_initialize, start_reader_task, ManagedProcess};
use protocol::RequestDispatcher;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
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
    restart_count: u32,
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
    /// Known language descriptors (from Zero-Config detection).
    descriptors: Arc<Vec<LspDescriptor>>,
    /// Running processes keyed by language id.
    processes: Arc<RwLock<HashMap<String, ProcessEntry>>>,
    /// Locks for concurrent initialization to prevent duplicate spawns.
    init_locks: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
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
    pub async fn new(
        workspace_root: &Path,
        config: std::sync::Arc<pathfinder_common::config::PathfinderConfig>,
    ) -> std::io::Result<Self> {
        let descriptors = detect_languages(workspace_root, &config).await?;

        tracing::info!(
            workspace = %workspace_root.display(),
            detected_languages = ?descriptors.iter().map(|l| &l.language_id).collect::<Vec<_>>(),
            "LspClient: language detection complete"
        );

        let client = Self {
            descriptors: Arc::new(descriptors),
            processes: Arc::new(RwLock::new(HashMap::new())),
            init_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
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

        // Acquire the init lock for this language to prevent duplicate spawn races
        let init_lock = {
            let mut locks = self.init_locks.lock().await;
            locks
                .entry(language_id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = init_lock.lock().await;

        // Double-check after acquiring lock
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

        let spawn_result = spawn_and_initialize(
            &descriptor.command,
            &descriptor.args,
            &descriptor.root,
            &language_id,
            Arc::clone(&self.dispatcher),
        )
        .await;

        let (process, stdout) = match spawn_result {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(
                    language = %language_id,
                    error = %e,
                    attempt,
                    "LSP: initialization failed — retrying"
                );
                // Recurse with attempt+1; the guard at the top of this function handles
                // exhaustion (attempt >= MAX_RESTART_ATTEMPTS) by inserting Unavailable.
                return Box::pin(self.start_process(descriptor, attempt + 1)).await;
            }
        };

        let reader_handle = start_reader_task(stdout, Arc::clone(&self.dispatcher));

        self.processes.write().await.insert(
            language_id,
            ProcessEntry::Running(Box::new(LanguageState {
                process,
                _reader: reader_handle,
                restart_count: attempt,
            })),
        );

        Ok(())
    }

    /// Update `last_used` for a language (called after each successful request).
    async fn touch(&self, language_id: &str) {
        let mut guard = self.processes.write().await;
        if let Some(ProcessEntry::Running(state)) = guard.get_mut(language_id) {
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
            .map_err(|_| {
                self.dispatcher.remove(id);
                LspError::Timeout {
                    operation: method.to_owned(),
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                }
            })?
            .map_err(|_| LspError::ConnectionLost)?
    }

    /// Send a JSON-RPC notification (fire-and-forget, no response expected).
    ///
    /// Notifications are sent on the stdin of the target process without
    /// registering a response waiter — the LSP doesn't reply to them.
    async fn notify(
        &self,
        language_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), LspError> {
        let message = RequestDispatcher::make_notification(method, &params);
        let guard = self.processes.read().await;
        match guard.get(language_id) {
            Some(ProcessEntry::Running(state)) => send(&state.process, &message).await,
            Some(ProcessEntry::Unavailable(_)) | None => Err(LspError::NoLspAvailable),
        }
    }

    /// Retrieve the detected capabilities for a language.
    ///
    /// Returns `Ok(caps)` when the process is running, else `NoLspAvailable`.
    async fn capabilities_for(&self, language_id: &str) -> Result<DetectedCapabilities, LspError> {
        let guard = self.processes.read().await;
        match guard.get(language_id) {
            Some(ProcessEntry::Running(state)) => Ok(state.process.capabilities.clone()),
            Some(ProcessEntry::Unavailable(_)) | None => Err(LspError::NoLspAvailable),
        }
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
        tracing::info!(tool = "goto_definition", file = %file_path.display(), "LSP operation started");

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

        let caps = self.capabilities_for(language_id).await?;
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
                Duration::from_secs(10),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "call_hierarchy_prepare", language = language_id, error = %e, "textDocument/prepareCallHierarchy failed");
                return Err(e);
            }
        };

        self.touch(language_id).await;

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/prepareCallHierarchy complete"
        );

        parse_call_hierarchy_prepare_response(&response, workspace_root)
    }

    async fn call_hierarchy_incoming(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "call_hierarchy_incoming", file = %item.file, "LSP operation started");
        let ext = Path::new(&item.file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let lsp_item = item.data.clone().ok_or_else(|| {
            LspError::Protocol("CallHierarchyItem missing original LSP data".to_owned())
        })?;

        let params = json!({ "item": lsp_item });

        let response = match self
            .request(
                language_id,
                "callHierarchy/incomingCalls",
                params,
                Duration::from_secs(30),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "call_hierarchy_incoming", language = language_id, error = %e, "callHierarchy/incomingCalls failed");
                return Err(e);
            }
        };

        self.touch(language_id).await;

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            language = language_id,
            elapsed_ms = elapsed,
            "callHierarchy/incomingCalls complete"
        );

        parse_call_hierarchy_calls_response(&response, workspace_root, "from", "fromRanges")
    }

    async fn call_hierarchy_outgoing(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "call_hierarchy_outgoing", file = %item.file, "LSP operation started");
        let ext = Path::new(&item.file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let lsp_item = item.data.clone().ok_or_else(|| {
            LspError::Protocol("CallHierarchyItem missing original LSP data".to_owned())
        })?;

        let params = json!({ "item": lsp_item });

        let response = match self
            .request(
                language_id,
                "callHierarchy/outgoingCalls",
                params,
                Duration::from_secs(30),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "call_hierarchy_outgoing", language = language_id, error = %e, "callHierarchy/outgoingCalls failed");
                return Err(e);
            }
        };

        self.touch(language_id).await;

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            language = language_id,
            elapsed_ms = elapsed,
            "callHierarchy/outgoingCalls complete"
        );

        parse_call_hierarchy_calls_response(&response, workspace_root, "to", "fromRanges")
    }

    async fn did_open(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
    ) -> Result<(), LspError> {
        tracing::info!(tool = "did_open", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

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
            tracing::error!(tool = "did_open", language = language_id, error = %e, "textDocument/didOpen failed");
            return Err(e);
        }
        self.touch(language_id).await;
        Ok(())
    }

    async fn did_change(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
        version: i32,
    ) -> Result<(), LspError> {
        tracing::info!(tool = "did_change", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        // Full content sync (TextDocumentSyncKind.Full = 1)
        let params = json!({
            "textDocument": {
                "uri": file_uri.as_str(),
                "version": version
            },
            "contentChanges": [{ "text": content }]
        });

        if let Err(e) = self
            .notify(language_id, "textDocument/didChange", params)
            .await
        {
            tracing::error!(tool = "did_change", language = language_id, error = %e, "textDocument/didChange failed");
            return Err(e);
        }
        self.touch(language_id).await;
        Ok(())
    }

    async fn did_close(&self, workspace_root: &Path, file_path: &Path) -> Result<(), LspError> {
        tracing::info!(tool = "did_close", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": {
                "uri": file_uri.as_str()
            }
        });

        if let Err(e) = self
            .notify(language_id, "textDocument/didClose", params)
            .await
        {
            tracing::error!(tool = "did_close", language = language_id, error = %e, "textDocument/didClose failed");
            return Err(e);
        }
        // Not touching `last_used` on close since this is a cleanup action.
        Ok(())
    }

    async fn pull_diagnostics(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "pull_diagnostics", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        // Check capability before sending the request
        let caps = self.capabilities_for(language_id).await?;
        if !caps.diagnostic_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "diagnosticProvider".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/diagnostic",
                params,
                Duration::from_secs(30),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "pull_diagnostics", language = language_id, error = %e, "textDocument/diagnostic failed");
                return Err(e);
            }
        };

        self.touch(language_id).await;

        let elapsed = start.elapsed().as_millis();
        tracing::debug!(
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/diagnostic complete"
        );

        parse_diagnostic_response(&response, file_path)
    }

    async fn pull_workspace_diagnostics(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "pull_workspace_diagnostics", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let caps = self.capabilities_for(language_id).await?;
        if !caps.workspace_diagnostic_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "workspaceDiagnosticProvider".to_owned(),
            });
        }

        // The params for workspace diagnostics are typically quite minimal
        let params = json!({});

        let response = match self
            .request(
                language_id,
                "workspace/diagnostic",
                params,
                Duration::from_secs(60), // Workspace diagnostics might take longer
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "pull_workspace_diagnostics", language = language_id, error = %e, "workspace/diagnostic failed");
                return Err(e);
            }
        };

        self.touch(language_id).await;

        let elapsed = start.elapsed().as_millis();
        tracing::debug!(
            language = language_id,
            elapsed_ms = elapsed,
            "workspace/diagnostic complete"
        );

        parse_workspace_diagnostic_response(&response, workspace_root)
    }

    async fn range_formatting(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        start_line: u32,
        end_line: u32,
    ) -> Result<Option<String>, LspError> {
        tracing::info!(tool = "range_formatting", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        // Check capability before sending the request
        let caps = self.capabilities_for(language_id).await?;
        if !caps.formatting_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "documentFormattingProvider".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        // LSP positions are 0-indexed, our API is 1-indexed
        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "range": {
                "start": { "line": start_line.saturating_sub(1), "character": 0 },
                "end":   { "line": end_line.saturating_sub(1),   "character": 0 }
            },
            "options": { "tabSize": 4, "insertSpaces": true }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/rangeFormatting",
                params,
                Duration::from_secs(10),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "range_formatting", language = language_id, error = %e, "textDocument/rangeFormatting failed");
                return Err(e);
            }
        };

        self.touch(language_id).await;

        tracing::info!(
            tool = "range_formatting",
            language = language_id,
            "textDocument/rangeFormatting complete"
        );

        if response.is_null() {
            return Ok(None);
        }

        if let Some(edits) = response.as_array() {
            tracing::debug!(
                language = language_id,
                edit_count = edits.len(),
                "LSP returned range formatting edits (currently ignored)"
            );
        }

        // response is an array of TextEdit objects; we currently don't apply them
        // (we just signal availability). The Tree-sitter indentation pre-pass
        // is sufficient. Return None to indicate "no formatted text substitution".
        Ok(None)
    }

    async fn capability_status(&self) -> HashMap<String, crate::types::LspLanguageStatus> {
        let mut status = HashMap::new();
        for desc in self.descriptors.iter() {
            let guard = self.processes.read().await;
            let lang_status = match guard.get(&desc.language_id) {
                Some(ProcessEntry::Running(state)) => {
                    if state.process.capabilities.diagnostic_provider {
                        crate::types::LspLanguageStatus {
                            validation: true,
                            reason: "LSP connected and supports validation".to_owned(),
                        }
                    } else {
                        crate::types::LspLanguageStatus {
                            validation: false,
                            reason: "LSP connected but does not support textDocument/diagnostic"
                                .to_owned(),
                        }
                    }
                }
                Some(ProcessEntry::Unavailable(_)) => crate::types::LspLanguageStatus {
                    validation: false,
                    reason: format!("{} failed to start or crashed repeatedly", desc.command),
                },
                None => {
                    // Lazy start: it hasn't crashed, and it was detected in the workspace
                    crate::types::LspLanguageStatus {
                        validation: true,
                        reason: format!("{} available (lazy start)", desc.command),
                    }
                }
            };
            status.insert(desc.language_id.clone(), lang_status);
        }
        status
    }
}

/// Parse the `textDocument/diagnostic` response into a `Vec<LspDiagnostic>`.
///
/// The response shape is: `{ "kind": "full", "items": [Diagnostic, ...] }`
/// or `{ "kind": "unchanged", "resultId": "..." }` for unchanged results.
fn parse_diagnostic_response(
    response: &serde_json::Value,
    file_path: &Path,
) -> Result<Vec<LspDiagnostic>, LspError> {
    // "unchanged" kind means diagnostics have not changed since last pull
    if response.get("kind").and_then(|k| k.as_str()) == Some("unchanged") {
        return Ok(vec![]);
    }

    let items = match response.get("items") {
        Some(serde_json::Value::Array(arr)) => arr,
        Some(_) => {
            return Err(LspError::Protocol(
                "diagnostics 'items' is not an array".to_owned(),
            ))
        }
        // Some LSPs return flat arrays (not wrapped in {kind, items})
        None => {
            if let Some(arr) = response.as_array() {
                return Ok(parse_diagnostic_items(arr, file_path));
            }
            return Ok(vec![]);
        }
    };

    Ok(parse_diagnostic_items(items, file_path))
}

/// Parse the `workspace/diagnostic` response into a flat `Vec<LspDiagnostic>`.
///
/// Response shape: `{ "items": [ { "uri": "...", "kind": "full", "items": [Diagnostic, ...] } ] }`
fn parse_workspace_diagnostic_response(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Result<Vec<LspDiagnostic>, LspError> {
    let mut all_diags = Vec::new();

    let items = response
        .get("items")
        .and_then(|i| i.as_array())
        .ok_or_else(|| {
            LspError::Protocol("workspace.diagnostic 'items' is missing or not an array".to_owned())
        })?;

    for doc_report in items {
        if doc_report.get("kind").and_then(|k| k.as_str()) == Some("unchanged") {
            continue;
        }

        let Some(uri_str) = doc_report.get("uri").and_then(|u| u.as_str()) else {
            continue;
        };

        // Convert URI to relative file path using workspace_root
        let file_path = match Url::parse(uri_str) {
            Ok(url) => {
                if let Ok(path) = url.to_file_path() {
                    match path.strip_prefix(workspace_root) {
                        Ok(rel) => rel.to_path_buf(),
                        Err(_) => path, // Fallback to absolute if not in workspace
                    }
                } else {
                    continue;
                }
            }
            Err(_) => continue,
        };

        if let Some(doc_items) = doc_report.get("items").and_then(|i| i.as_array()) {
            all_diags.extend(parse_diagnostic_items(doc_items, &file_path));
        }
    }

    Ok(all_diags)
}

/// Parse an array of LSP `Diagnostic` objects.
fn parse_diagnostic_items(items: &[serde_json::Value], file_path: &Path) -> Vec<LspDiagnostic> {
    let mut result = Vec::with_capacity(items.len());
    let file_str = file_path.to_string_lossy().into_owned();

    for item in items {
        let severity = match item["severity"].as_u64().unwrap_or(1) {
            1 => LspDiagnosticSeverity::Error,
            2 => LspDiagnosticSeverity::Warning,
            3 => LspDiagnosticSeverity::Information,
            _ => LspDiagnosticSeverity::Hint,
        };

        let message = item["message"].as_str().unwrap_or("").to_owned();
        if message.is_empty() {
            continue; // Skip diagnostics with no message
        }

        let code = item.get("code").and_then(|c| match c {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        });

        let start_line = item["range"]["start"]["line"]
            .as_u64()
            .map_or(1, |l| u32::try_from(l + 1).unwrap_or(1));
        let end_line = item["range"]["end"]["line"]
            .as_u64()
            .map_or(start_line, |l| u32::try_from(l + 1).unwrap_or(1));

        result.push(LspDiagnostic {
            severity,
            code,
            message,
            file: file_str.clone(),
            start_line,
            end_line,
        });
    }

    result
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

/// Parse the `textDocument/prepareCallHierarchy` response into a `Vec<CallHierarchyItem>`.
fn parse_call_hierarchy_prepare_response(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Result<Vec<CallHierarchyItem>, LspError> {
    if response.is_null() {
        return Ok(Vec::new());
    }

    let items = response
        .as_array()
        .ok_or_else(|| LspError::Protocol("expected array".to_owned()))?;

    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let uri_str = item["uri"].as_str().unwrap_or("");
        let file = Url::parse(uri_str)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .and_then(|p| {
                p.strip_prefix(workspace_root)
                    .map(std::path::Path::to_path_buf)
                    .ok()
            })
            .map_or_else(|| uri_str.to_owned(), |p| p.to_string_lossy().into_owned());

        let line = u32::try_from(
            item["selectionRange"]["start"]["line"]
                .as_u64()
                .unwrap_or(0),
        )
        .unwrap_or(0)
            + 1;
        let column = u32::try_from(
            item["selectionRange"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
        .unwrap_or(0)
            + 1;

        let kind_int = item["kind"].as_u64().unwrap_or(0);
        let kind = match kind_int {
            5 => "class",
            6 => "method",
            11 => "interface",
            12 => "function",
            _ => "symbol",
        }
        .to_owned();

        result.push(CallHierarchyItem {
            name: item["name"].as_str().unwrap_or("").to_owned(),
            kind,
            detail: item
                .get("detail")
                .and_then(|d| d.as_str())
                .map(ToOwned::to_owned),
            file,
            line,
            column,
            data: Some(item.clone()),
        });
    }

    Ok(result)
}

/// Parse the `callHierarchy/incomingCalls` or `outgoingCalls` response.
fn parse_call_hierarchy_calls_response(
    response: &serde_json::Value,
    workspace_root: &Path,
    item_key: &str,
    ranges_key: &str,
) -> Result<Vec<CallHierarchyCall>, LspError> {
    if response.is_null() {
        return Ok(Vec::new());
    }

    let calls = response
        .as_array()
        .ok_or_else(|| LspError::Protocol("expected array".to_owned()))?;

    let mut result = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(item_val) = call.get(item_key) else {
            continue;
        };
        let mut parsed_items = parse_call_hierarchy_prepare_response(
            &serde_json::Value::Array(vec![item_val.clone()]),
            workspace_root,
        )?;
        if parsed_items.is_empty() {
            continue;
        }
        let item = parsed_items.remove(0);

        let mut call_sites = Vec::new();
        if let Some(ranges) = call.get(ranges_key).and_then(|r| r.as_array()) {
            for range in ranges {
                if let Some(line) = range
                    .get("start")
                    .and_then(|s| s.get("line"))
                    .and_then(serde_json::Value::as_u64)
                {
                    call_sites.push(u32::try_from(line).unwrap_or(0) + 1);
                }
            }
        }

        result.push(CallHierarchyCall { item, call_sites });
    }

    Ok(result)
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
                    if state.process.last_used.elapsed() > DEFAULT_IDLE_TIMEOUT {
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
                tracing::info!(
                    language = %lang,
                    restarts = state.restart_count,
                    "LSP: idle timeout — terminating"
                );
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
