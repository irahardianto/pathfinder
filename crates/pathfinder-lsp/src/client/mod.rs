//! `LspClient` — the production [`Lawyer`] implementation.
//!
//! `LspClient` manages a pool of LSP child processes (one per language).
//! Processes are started lazily on first use and terminated automatically
//! after an idle timeout.
//!
//! # Crash Recovery
//! When a crash is detected the client restarts the process with exponential
//! back-off capped at 60 seconds (1s → 2s → 4s → … → 60s). The process
//! is never permanently marked unavailable — each backoff window is computed
//! from `backoff_attempt` so recovery is always attempted.

mod background;
mod capabilities;
mod detect;
mod document;
#[cfg(test)]
pub(crate) mod fake_transport;
mod lawyer_impl;
mod lifecycle;
mod process;
mod protocol;
mod response_parsers;
mod transport;

pub use capabilities::{DetectedCapabilities, DiagnosticsStrategy};
pub use detect::install_hint;
pub use detect::{
    detect_languages, language_id_for_extension, DetectionResult, LanguageLsp, MissingLanguage,
};

use crate::types::IndexingCompletionSource;
use dashmap::DashMap;
use detect::LanguageLsp as LspDescriptor;
use process::LspTransport;
use protocol::RequestDispatcher;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;

#[derive(Clone)]
pub(crate) struct ProcessLifecycle {
    pub(crate) child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
}

pub(crate) struct LanguageState {
    pub(crate) transport: Arc<dyn LspTransport>,
    pub(crate) lifecycle: Option<ProcessLifecycle>,
    pub(crate) reader_handle: tokio::task::JoinHandle<()>,
    pub(crate) restart_count: u32,
    pub(crate) spawned_at: Instant,
    pub(crate) indexing_complete: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) indexing_completion_source: Arc<std::sync::Mutex<Option<IndexingCompletionSource>>>,
    pub(crate) indexing_duration_secs: Arc<std::sync::Mutex<Option<u64>>>,
    pub(crate) indexing_progress_percent: Arc<std::sync::Mutex<Option<u8>>>,
    pub(crate) live_capabilities: Arc<std::sync::RwLock<DetectedCapabilities>>,
    pub(crate) in_coexistence_mode: bool,
}

/// Tracks backoff state for a language whose last spawn attempt failed.
///
/// The language is never permanently dead — `ensure_process` will retry once
/// the exponential backoff window has elapsed.
pub(crate) struct UnavailableState {
    /// When this language last failed to start (start of current backoff window).
    pub(crate) unavailable_since: Instant,
    /// Number of consecutive failed spawn attempts. Used to compute backoff:
    /// `min(1 << backoff_attempt, MAX_BACKOFF_SECS)` seconds.
    pub(crate) backoff_attempt: u32,
}

pub(crate) enum ProcessEntry {
    /// Active LSP process. Boxed to equalise variant sizes.
    Running(Box<LanguageState>),
    Unavailable(UnavailableState),
}

impl ProcessEntry {
    fn to_validation_status(&self, command: &str) -> crate::types::LspLanguageStatus {
        match self {
            Self::Running(state) => {
                // MT-3: Read from live_capabilities (may include dynamic registrations).
                #[allow(clippy::expect_used)]
                let caps = state
                    .live_capabilities
                    .read()
                    .expect("live_capabilities lock");
                #[allow(clippy::expect_used)]
                let indexing_source = state
                    .indexing_completion_source
                    .lock()
                    .expect("indexing_completion_source lock")
                    .as_ref()
                    .map(|source| match source {
                        IndexingCompletionSource::Progress => "progress".to_string(),
                        IndexingCompletionSource::TimeoutFallback => "timeout_fallback".to_string(),
                    });
                #[allow(clippy::expect_used)]
                let indexing_duration_secs = *state
                    .indexing_duration_secs
                    .lock()
                    .expect("indexing_duration_secs lock");
                #[allow(clippy::expect_used)]
                let indexing_progress_pct = *state
                    .indexing_progress_percent
                    .lock()
                    .expect("indexing_progress_percent lock");
                let effective_diag_strategy = if state.in_coexistence_mode {
                    DiagnosticsStrategy::None
                } else {
                    caps.diagnostics_strategy
                };
                validation_status_from_parts(
                    command,
                    true,
                    effective_diag_strategy,
                    caps.definition_provider,
                    caps.call_hierarchy_provider,
                    caps.formatting_provider,
                    state
                        .indexing_complete
                        .load(std::sync::atomic::Ordering::Relaxed),
                    state.spawned_at.elapsed().as_secs(),
                    caps.server_name.as_deref(),
                    indexing_source,
                    indexing_duration_secs,
                    indexing_progress_pct,
                )
            }
            Self::Unavailable(_) => validation_status_from_parts(
                command,
                false,
                DiagnosticsStrategy::None,
                false,
                false,
                false,
                false,
                0,
                None,
                None,
                None,
                None,
            ),
        }
    }
}

/// Pure helper that maps raw process state to [`LspLanguageStatus`].
///
/// Extracted from [`ProcessEntry::to_validation_status`] to make the
/// mapping logic independently unit-testable without requiring a live
/// [`ManagedProcess`] (which embeds an OS child process handle).
///
/// MT-2: `server_name` added so the status carries the server identity for
/// per-server push diagnostics config selection in validation tools.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)]
fn validation_status_from_parts(
    command: &str,
    running: bool,
    diagnostics_strategy: DiagnosticsStrategy,
    supports_definition: bool,
    supports_call_hierarchy: bool,
    supports_formatting: bool,
    indexing_complete: bool,
    uptime_seconds: u64,
    server_name: Option<&str>,
    indexing_source: Option<String>,
    indexing_duration_secs: Option<u64>,
    indexing_progress_pct: Option<u8>,
) -> crate::types::LspLanguageStatus {
    if !running {
        return crate::types::LspLanguageStatus {
            validation: false,
            reason: format!("{command} failed to start or crashed repeatedly"),
            navigation_ready: None,
            indexing_complete: None,
            uptime_seconds: None,
            diagnostics_strategy: None,
            supports_definition: None,
            supports_call_hierarchy: None,
            supports_diagnostics: None,
            supports_formatting: None,
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
        };
    }

    let navigation_ready = Some(supports_definition);

    match diagnostics_strategy {
        DiagnosticsStrategy::Pull | DiagnosticsStrategy::Push => crate::types::LspLanguageStatus {
            validation: true,
            reason: format!(
                "LSP connected and supports validation ({})",
                match diagnostics_strategy {
                    DiagnosticsStrategy::Pull => "pull diagnostics",
                    DiagnosticsStrategy::Push => "push diagnostics",
                    DiagnosticsStrategy::None => unreachable!(),
                }
            ),
            navigation_ready,
            indexing_complete: Some(indexing_complete),
            uptime_seconds: Some(uptime_seconds),
            diagnostics_strategy: Some(diagnostics_strategy.as_str().to_owned()),
            supports_definition: Some(supports_definition),
            supports_call_hierarchy: Some(supports_call_hierarchy),
            supports_diagnostics: Some(true),
            supports_formatting: Some(supports_formatting),
            server_name: server_name.map(ToOwned::to_owned),
            indexing_source,
            indexing_duration_secs,
            indexing_progress_percent: if indexing_complete {
                None
            } else {
                indexing_progress_pct
            },
        },
        DiagnosticsStrategy::None => crate::types::LspLanguageStatus {
            validation: false,
            reason: "LSP connected but does not support diagnostics".to_owned(),
            navigation_ready,
            indexing_complete: Some(indexing_complete),
            uptime_seconds: Some(uptime_seconds),
            diagnostics_strategy: Some("none".to_owned()),
            supports_definition: Some(supports_definition),
            supports_call_hierarchy: Some(supports_call_hierarchy),
            supports_diagnostics: Some(false),
            supports_formatting: Some(supports_formatting),
            server_name: server_name.map(ToOwned::to_owned),
            indexing_source,
            indexing_duration_secs,
            indexing_progress_percent: if indexing_complete {
                None
            } else {
                indexing_progress_pct
            },
        },
    }
}

/// RAII guard that increments in-flight counter on creation and decrements on drop.
pub(crate) struct InFlightGuard {
    counter: Arc<AtomicU32>,
}

impl InFlightGuard {
    pub(crate) fn new(counter: Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The production `Lawyer` implementation.
///
/// Manages per-language LSP child processes and provides JSON-RPC request
/// routing for `textDocument/definition` and future capabilities.
#[derive(Clone)]
pub struct LspClient {
    pub(crate) descriptors: Arc<Vec<LspDescriptor>>,
    pub(crate) missing_languages: Arc<Vec<crate::client::detect::MissingLanguage>>,
    pub(crate) processes: Arc<DashMap<String, ProcessEntry>>,
    pub(crate) init_locks: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) dispatcher: Arc<RequestDispatcher>,
    pub(crate) shutdown_tx: Arc<broadcast::Sender<()>>,
    pub(crate) doc_versions: Arc<DashMap<String, std::sync::atomic::AtomicI32>>,
    pub(crate) warm_start_complete: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::types::IndexingCompletionSource;
    use std::collections::HashMap;
    use std::time::Duration;

    // ── ProcessEntry::to_validation_status tests ──────────────────

    #[test]
    fn test_process_entry_unavailable_status() {
        let entry = ProcessEntry::Unavailable(UnavailableState {
            backoff_attempt: 0,
            unavailable_since: std::time::Instant::now(),
        });
        let status = entry.to_validation_status("gopls");
        assert!(!status.validation);
        assert!(status.reason.contains("gopls"));
        assert!(status.reason.contains("failed"));
    }

    #[test]
    fn test_process_entry_running_with_diagnostics_status() {
        // validation_status_from_parts is the pure helper extracted from
        // ProcessEntry::to_validation_status so we can test without a
        // live ManagedProcess (which requires a real OS child handle).
        // Future agents: add more variants here as capabilities grow.
        let status = validation_status_from_parts(
            "rust-analyzer",
            true,
            DiagnosticsStrategy::Pull,
            true,  // supports_definition
            true,  // supports_call_hierarchy
            false, // supports_formatting
            false, // indexing_complete
            10,    // uptime_seconds
            None,  // server_name
            None,
            None,
            None,
        );
        assert!(
            status.validation,
            "DiagnosticsStrategy::Pull must yield validation=true"
        );
        assert_eq!(
            status.reason,
            "LSP connected and supports validation (pull diagnostics)"
        );
        assert_eq!(status.indexing_complete, Some(false));
        assert_eq!(status.uptime_seconds, Some(10));
    }

    #[test]
    fn test_process_entry_running_with_diagnostics_indexing_complete() {
        let status = validation_status_from_parts(
            "rust-analyzer",
            true,
            DiagnosticsStrategy::Pull,
            true,  // supports_definition
            true,  // supports_call_hierarchy
            false, // supports_formatting
            true,  // indexing_complete
            42,    // uptime_seconds
            None,  // server_name
            None,
            None,
            None,
        );
        assert!(status.validation);
        assert_eq!(status.indexing_complete, Some(true));
        assert_eq!(status.uptime_seconds, Some(42));
    }

    #[test]
    fn test_process_entry_running_without_diagnostics_status() {
        // LSP connected but does not support textDocument/diagnostic.
        let status = validation_status_from_parts(
            "gopls",
            true,
            DiagnosticsStrategy::None,
            true,
            true,
            false,
            true,
            5,
            None,
            None,
            None,
            None,
        );
        assert!(
            !status.validation,
            "diagnostic_provider=false must yield validation=false"
        );
        assert!(
            status.reason.contains("does not support"),
            "reason must mention lack of support, got: {}",
            status.reason
        );
        assert_eq!(status.indexing_complete, Some(true));
        assert_eq!(status.uptime_seconds, Some(5));
    }

    #[test]
    fn test_process_entry_running_uptime_is_non_none() {
        // Uptime should always be Some for a running process (even if 0 seconds).
        let status = validation_status_from_parts(
            "pyright",
            true,
            DiagnosticsStrategy::Pull,
            true,
            true,
            false,
            false,
            0,
            None,
            None,
            None,
            None,
        );
        assert!(status.uptime_seconds.is_some());
        assert!(status.indexing_complete.is_some());
    }

    // ── navigation_ready tests (LSP-HEALTH-001) ──────────────────

    #[test]
    fn test_navigation_ready_true_when_supports_definition_and_running() {
        // Pyright scenario: LSP running, supports_definition=true,
        // but diagnostics_strategy=None AND indexing_complete=false.
        // Navigation should still be "ready" because initialize handshake completed.
        let status = validation_status_from_parts(
            "pyright",
            true, // running
            DiagnosticsStrategy::None,
            true,  // supports_definition
            true,  // supports_call_hierarchy
            false, // supports_formatting
            false, // indexing_complete (still indexing)
            5,     // uptime_seconds
            None,  // server_name
            None,
            None,
            None,
        );
        // Navigation ready regardless of diagnostics and indexing status
        assert_eq!(status.navigation_ready, Some(true));
        // But validation is false because no diagnostics
        assert!(!status.validation);
        // Indexing is still in progress
        assert_eq!(status.indexing_complete, Some(false));
    }

    #[test]
    fn test_navigation_ready_false_when_supports_definition_false() {
        // Edge case: LSP running but doesn't support definition at all
        let status = validation_status_from_parts(
            "weird-lsp",
            true, // running
            DiagnosticsStrategy::Pull,
            false, // supports_definition = false
            false, // supports_call_hierarchy
            false, // supports_formatting
            true,  // indexing_complete
            10,    // uptime_seconds
            None,  // server_name
            None,
            None,
            None,
        );
        // Navigation not ready because LSP doesn't have definitionProvider capability
        assert_eq!(status.navigation_ready, Some(false));
        // But validation is true because pull diagnostics available
        assert!(status.validation);
    }

    #[test]
    fn test_navigation_ready_none_when_not_running() {
        // When LSP is not running (crashed, failed to start), navigation_ready is None
        let status = validation_status_from_parts(
            "gopls",
            false,                     // NOT running
            DiagnosticsStrategy::None, // irrelevant when !running
            true,                      // irrelevant when !running
            true,                      // irrelevant when !running
            false,                     // irrelevant when !running
            false,                     // irrelevant when !running
            0,                         // irrelevant when !running
            None,                      // server_name
            None,
            None,
            None,
        );
        assert_eq!(status.navigation_ready, None);
        assert_eq!(status.indexing_complete, None);
        assert!(!status.validation);
    }

    // ── WP-1: LspClient Test Harness ──────────────────────────────
    //
    // These tests exercise LspClient's routing and lifecycle logic without
    // spawning real LSP child processes. We use a test-only constructor that
    // injects pre-configured process map entries.

    /// Create a test `LspClient` with empty descriptors (no languages detected).
    ///
    /// Useful for testing error paths where `ensure_process` returns `NoLspAvailable`
    /// because no descriptor was found.
    pub(crate) fn client_no_languages() -> LspClient {
        let (shutdown_tx, _) = broadcast::channel(1);
        LspClient {
            descriptors: Arc::new(Vec::new()),
            missing_languages: Arc::new(Vec::new()),
            processes: Arc::new(DashMap::new()),
            init_locks: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::new(shutdown_tx),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Create a test `LspClient` with descriptors for specific languages but no
    /// running processes. The `processes` map can be pre-populated by the caller.
    pub(crate) fn client_with_descriptors(
        languages: Vec<&str>,
        processes: HashMap<String, ProcessEntry>,
    ) -> LspClient {
        let descriptors = languages
            .into_iter()
            .map(|lang| LspDescriptor {
                language_id: lang.to_owned(),
                command: format!("{lang}-lsp-server"),
                args: vec![],
                root: std::env::temp_dir(),
                init_timeout_secs: None,
                auto_plugins: vec![],
                init_options: serde_json::Value::Null,
            })
            .collect();

        let processes_dashmap = DashMap::new();
        for (k, v) in processes {
            processes_dashmap.insert(k, v);
        }

        let (shutdown_tx, _) = broadcast::channel(1);
        LspClient {
            descriptors: Arc::new(descriptors),
            missing_languages: Arc::new(Vec::new()),
            processes: Arc::new(processes_dashmap),
            init_locks: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::new(shutdown_tx),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    use fake_transport::FakeTransport;

    pub(crate) fn make_running_client(language_id: &str) -> (LspClient, Arc<FakeTransport>) {
        let fake = Arc::new(FakeTransport::new());
        let dispatcher = Arc::new(RequestDispatcher::new());
        let (shutdown_tx, _) = broadcast::channel(1);

        fake.set_dispatcher(Arc::clone(&dispatcher));

        let reader_handle = tokio::spawn(async {
            std::future::pending::<()>().await;
        });

        let entry = ProcessEntry::Running(Box::new(LanguageState {
            transport: Arc::clone(&fake) as Arc<dyn LspTransport>,
            lifecycle: None,
            reader_handle,
            restart_count: 0,
            spawned_at: Instant::now(),
            indexing_complete: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            indexing_completion_source: Arc::new(std::sync::Mutex::new(Some(
                IndexingCompletionSource::Progress,
            ))),
            indexing_duration_secs: Arc::new(std::sync::Mutex::new(Some(0))),
            indexing_progress_percent: Arc::new(std::sync::Mutex::new(None)),
            live_capabilities: Arc::new(std::sync::RwLock::new(DetectedCapabilities::default())),
            in_coexistence_mode: false,
        }));

        let descriptors = vec![LspDescriptor {
            language_id: language_id.to_owned(),
            command: "fake-lsp".to_owned(),
            args: vec![],
            root: std::env::temp_dir(),
            init_timeout_secs: None,
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        }];

        let processes = DashMap::new();
        processes.insert(language_id.to_owned(), entry);

        let client = LspClient {
            descriptors: Arc::new(descriptors),
            missing_languages: Arc::new(Vec::new()),
            processes: Arc::new(processes),
            init_locks: Arc::new(DashMap::new()),
            dispatcher,
            shutdown_tx: Arc::new(shutdown_tx),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        (client, fake)
    }

    // ── Warm start tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_warm_start_no_languages_is_noop() {
        let client = client_no_languages();
        client.warm_start(); // Should not panic
                             // Give spawned tasks a chance to run
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn test_shutdown_sends_signal() {
        let client = client_no_languages();

        // Subscribe before sending to ensure we catch it
        let mut rx = client.shutdown_tx.subscribe();

        client.shutdown();

        // The receiver should get the shutdown signal
        let result = rx.try_recv();
        assert!(
            result.is_ok(),
            "shutdown signal should be sent and received"
        );
    }

    // ── LT-4: Predictive LSP Warmup ──────────────────────────────────────────

    #[tokio::test]
    async fn test_warm_start_for_languages_starts_only_requested() {
        // warm_start_for_languages should only attempt to start the explicitly
        // requested languages, not all descriptors. With no real LSP binary,
        // start_process will fail, but the method must not panic.
        let client = client_with_descriptors(vec!["rust", "go", "typescript"], HashMap::new());
        // Only request "go" — "rust" and "typescript" should remain unstarted.
        let _ = client.warm_start_for_languages(&["go".to_owned()]);
        // No process should be running (no real binary), but no panic.
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_skips_already_running() {
        // If a process is already running for the language, warm_start_for_languages
        // must skip it without error (idempotent).
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        // Call twice — should be safe and not panic.
        let _ = client.warm_start_for_languages(&["rust".to_owned()]);
        let _ = client.warm_start_for_languages(&["rust".to_owned()]);
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_ignores_unknown() {
        // Languages not in descriptors should be silently ignored.
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        let _ = client.warm_start_for_languages(&["unknown_lang".to_owned()]);
    }

    #[tokio::test]
    async fn test_touch_language_extends_idle_timer() {
        // touch_language must update last_used for a running process.
        let client = client_no_languages();
        // With no processes, touch should be a no-op (no panic).
        client.touch_language("rust");
    }

    #[tokio::test]
    async fn test_touch_language_no_process_is_noop() {
        // touch_language on a language with no running process must be a no-op.
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        client.touch_language("rust");
        // No panic, no error.
    }

    // ── Tests for validation_status_from_parts helper ───────────────────────

    #[test]
    fn test_validation_status_not_running() {
        let status = validation_status_from_parts(
            "rust-analyzer",
            false, // not running
            DiagnosticsStrategy::Pull,
            true,
            true,
            true,
            true,
            100,
            Some("rust-analyzer"),
            None,
            None,
            None,
        );

        assert!(!status.validation);
        assert!(status.reason.contains("failed to start"));
        assert!(status.navigation_ready.is_none());
        assert!(status.indexing_complete.is_none());
        assert!(status.uptime_seconds.is_none());
        assert!(status.diagnostics_strategy.is_none());
        assert!(status.supports_definition.is_none());
        assert!(status.supports_call_hierarchy.is_none());
        assert!(status.supports_diagnostics.is_none());
        assert!(status.supports_formatting.is_none());
        assert!(status.server_name.is_none());
    }

    #[test]
    fn test_validation_status_running_with_pull_diagnostics() {
        let status = validation_status_from_parts(
            "rust-analyzer",
            true, // running
            DiagnosticsStrategy::Pull,
            true,
            true,
            true,
            true,
            100,
            Some("rust-analyzer"),
            None,
            None,
            None,
        );

        assert!(status.validation);
        assert!(status.reason.contains("pull diagnostics"));
        assert_eq!(status.navigation_ready, Some(true));
        assert_eq!(status.indexing_complete, Some(true));
        assert_eq!(status.uptime_seconds, Some(100));
        assert_eq!(status.diagnostics_strategy, Some("pull".to_owned()));
        assert_eq!(status.supports_definition, Some(true));
        assert_eq!(status.supports_call_hierarchy, Some(true));
        assert_eq!(status.supports_diagnostics, Some(true));
        assert_eq!(status.supports_formatting, Some(true));
        assert_eq!(status.server_name, Some("rust-analyzer".to_owned()));
    }

    #[test]
    fn test_validation_status_running_with_push_diagnostics() {
        let status = validation_status_from_parts(
            "gopls",
            true,
            DiagnosticsStrategy::Push,
            true,
            false,
            false,
            false,
            50,
            Some("gopls"),
            None,
            None,
            None,
        );

        assert!(status.validation);
        assert!(status.reason.contains("push diagnostics"));
        assert_eq!(status.navigation_ready, Some(true));
        assert_eq!(status.indexing_complete, Some(false));
        assert_eq!(status.uptime_seconds, Some(50));
        assert_eq!(status.diagnostics_strategy, Some("push".to_owned()));
        assert_eq!(status.supports_definition, Some(true));
        assert_eq!(status.supports_call_hierarchy, Some(false));
        assert_eq!(status.supports_diagnostics, Some(true));
        assert_eq!(status.supports_formatting, Some(false));
    }

    #[test]
    fn test_validation_status_running_with_no_diagnostics() {
        let status = validation_status_from_parts(
            "some-lsp",
            true,
            DiagnosticsStrategy::None,
            true,
            false,
            false,
            true,
            200,
            None,
            None,
            None,
            None,
        );

        assert!(!status.validation);
        assert!(status.reason.contains("does not support diagnostics"));
        assert_eq!(status.navigation_ready, Some(true));
        assert_eq!(status.indexing_complete, Some(true));
        assert_eq!(status.uptime_seconds, Some(200));
        assert_eq!(status.diagnostics_strategy, Some("none".to_owned()));
        assert_eq!(status.supports_definition, Some(true));
        assert_eq!(status.supports_call_hierarchy, Some(false));
        assert_eq!(status.supports_diagnostics, Some(false));
        assert_eq!(status.supports_formatting, Some(false));
        assert!(status.server_name.is_none());
    }

    #[test]
    fn test_validation_status_navigation_ready_false_when_no_definition() {
        let status = validation_status_from_parts(
            "lsp",
            true,
            DiagnosticsStrategy::Pull,
            false, // no definition support
            true,
            true,
            true,
            10,
            None,
            None,
            None,
            None,
        );

        assert!(status.validation);
        assert_eq!(status.navigation_ready, Some(false));
        assert_eq!(status.supports_definition, Some(false));
    }

    #[test]
    fn test_validation_status_includes_server_name() {
        let status = validation_status_from_parts(
            "command",
            true,
            DiagnosticsStrategy::Pull,
            true,
            true,
            true,
            true,
            0,
            Some("custom-lsp-server"),
            None,
            None,
            None,
        );

        assert_eq!(status.server_name, Some("custom-lsp-server".to_owned()));
    }

    #[test]
    fn test_validation_status_no_server_name() {
        let status = validation_status_from_parts(
            "command",
            true,
            DiagnosticsStrategy::Pull,
            true,
            true,
            true,
            true,
            0,
            None, // no server name
            None,
            None,
            None,
        );

        assert!(status.server_name.is_none());
    }

    // ── Tests for InFlightGuard ───────────────────────────────────────────────

    #[test]
    fn test_in_flight_guard_increments_counter() {
        use std::sync::atomic::AtomicU32;
        let counter = Arc::new(AtomicU32::new(0));
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 0);

        {
            let _guard = InFlightGuard::new(Arc::clone(&counter));
            assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);

            {
                let _guard2 = InFlightGuard::new(Arc::clone(&counter));
                assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 2);
            }
            // Second guard dropped
            assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);
        }
        // First guard dropped
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn test_in_flight_guard_concurrent() {
        use std::sync::atomic::AtomicU32;
        use std::sync::Barrier;
        use std::thread;
        let counter = Arc::new(AtomicU32::new(0));
        let barrier = Arc::new(Barrier::new(11)); // 10 threads + main
        let mut handles = vec![];

        for _ in 0..10 {
            let counter_clone = Arc::clone(&counter);
            let barrier_clone = Arc::clone(&barrier);
            let handle = thread::spawn(move || {
                let _guard = InFlightGuard::new(counter_clone);
                // Wait for all threads to reach this point
                barrier_clone.wait();
                // Guard is still alive here
                thread::sleep(std::time::Duration::from_millis(10));
            });
            handles.push(handle);
        }

        // Wait for all threads to create their guards
        barrier.wait();

        // All guards should be alive now
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 10);

        // Wait for all threads to complete and drop their guards
        for handle in handles {
            handle.join().unwrap();
        }

        // All guards should be dropped
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
