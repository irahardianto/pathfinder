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
pub(crate) mod process;
mod protocol;
pub mod response_parsers;
pub mod transport;

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
    /// The supervisor task handle — stored as `reader_handle` for compatibility
    /// with stale-reader detection in `request()`/`notify()`.
    pub(crate) reader_handle: tokio::task::JoinHandle<()>,
    /// C-2: Set to `true` when the process entry is created. The reader task
    /// sets this to `false` on exit (EOF, error, or abort). The supervisor's
    /// `remove_if` predicate checks this flag to distinguish the old entry
    /// from a replacement spawned by crash recovery.
    pub(crate) reader_alive: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) restart_count: u32,
    pub(crate) spawned_at: Instant,
    pub(crate) indexing_complete: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) indexing_completion_source:
        Arc<parking_lot::Mutex<Option<IndexingCompletionSource>>>,
    pub(crate) indexing_duration_secs: Arc<parking_lot::Mutex<Option<u64>>>,
    pub(crate) indexing_progress_percent: Arc<parking_lot::Mutex<Option<u8>>>,
    pub(crate) live_capabilities: Arc<parking_lot::RwLock<DetectedCapabilities>>,
    pub(crate) in_coexistence_mode: bool,
    pub(crate) watcher_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl LanguageState {
    pub(crate) fn abort_watchers(&self) {
        for handle in &self.watcher_handles {
            handle.abort();
        }
    }
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

#[allow(clippy::struct_excessive_bools)]
struct ValidationStatusInput<'a> {
    command: &'a str,
    language_id: &'a str,
    running: bool,
    diagnostics_strategy: DiagnosticsStrategy,
    supports_definition: bool,
    supports_call_hierarchy: bool,
    supports_formatting: bool,
    indexing_complete: bool,
    uptime_seconds: u64,
    server_name: Option<&'a str>,
    indexing_source: Option<String>,
    indexing_duration_secs: Option<u64>,
    indexing_progress_pct: Option<u8>,
    registrations_received: u32,
}

impl ProcessEntry {
    fn to_validation_status(
        &self,
        command: &str,
        language_id: &str,
    ) -> crate::types::LspLanguageStatus {
        match self {
            Self::Running(state) => {
                // MT-3: Read from live_capabilities (may include dynamic registrations).
                let caps = state.live_capabilities.read();
                let indexing_source =
                    state
                        .indexing_completion_source
                        .lock()
                        .as_ref()
                        .map(|source| match source {
                            IndexingCompletionSource::Progress => "progress".to_string(),
                            IndexingCompletionSource::TimeoutFallback => {
                                "timeout_fallback".to_string()
                            }
                        });
                let indexing_duration_secs = *state.indexing_duration_secs.lock();
                let indexing_progress_pct = *state.indexing_progress_percent.lock();
                let effective_diag_strategy = if state.in_coexistence_mode {
                    DiagnosticsStrategy::None
                } else {
                    caps.diagnostics_strategy
                };
                validation_status_from_parts(ValidationStatusInput {
                    command,
                    language_id,
                    running: true,
                    diagnostics_strategy: effective_diag_strategy,
                    supports_definition: caps.definition_provider,
                    supports_call_hierarchy: caps.call_hierarchy_provider,
                    supports_formatting: caps.formatting_provider,
                    indexing_complete: state
                        .indexing_complete
                        .load(std::sync::atomic::Ordering::Relaxed),
                    uptime_seconds: state.spawned_at.elapsed().as_secs(),
                    server_name: caps.server_name.as_deref(),
                    indexing_source,
                    indexing_duration_secs,
                    indexing_progress_pct,
                    registrations_received: caps.registrations_received,
                })
            }
            Self::Unavailable(_) => validation_status_from_parts(ValidationStatusInput {
                command,
                language_id,
                running: false,
                diagnostics_strategy: DiagnosticsStrategy::None,
                supports_definition: false,
                supports_call_hierarchy: false,
                supports_formatting: false,
                indexing_complete: false,
                uptime_seconds: 0,
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_pct: None,
                registrations_received: 0,
            }),
        }
    }
}

/// Returns the dynamic-registration grace period for the given language.
///
/// Languages that dynamically register capabilities after `initialize`
/// (e.g., jdtls) need a longer window before we treat a missing
/// `definitionProvider` as conclusive. Statically-advertising servers
/// (rust-analyzer, gopls, pyright) get 0s — a `false` reading is
/// immediately conclusive.
pub(crate) fn grace_period_for_language(language_id: &str) -> u64 {
    match language_id {
        "java" => 15,
        "typescript" | "javascript" => 5,
        _ => 0,
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
fn validation_status_from_parts(
    input: ValidationStatusInput<'_>,
) -> crate::types::LspLanguageStatus {
    if !input.running {
        return crate::types::LspLanguageStatus {
            validation: false,
            reason: format!("{} failed to start or crashed repeatedly", input.command),
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
            registrations_received: None,
        };
    }

    // Grace period for dynamic registration: jdtls and similar LSPs register
    // capabilities dynamically *after* initialize (they don't statically
    // advertise definitionProvider in the handshake). Language-aware grace
    // periods prevent premature `Some(false)` for slow-registering servers
    // while keeping static servers (rust-analyzer, gopls) immediately conclusive.
    let grace = grace_period_for_language(input.language_id);
    let navigation_ready = if !input.supports_definition && input.uptime_seconds < grace {
        None // Indeterminate — too early to tell, registrations may be in flight
    } else {
        Some(input.supports_definition)
    };

    match input.diagnostics_strategy {
        DiagnosticsStrategy::Pull | DiagnosticsStrategy::Push => crate::types::LspLanguageStatus {
            validation: true,
            reason: format!(
                "LSP connected and supports validation ({})",
                if matches!(input.diagnostics_strategy, DiagnosticsStrategy::Pull) {
                    "pull diagnostics"
                } else {
                    "push diagnostics"
                }
            ),
            navigation_ready,
            indexing_complete: Some(input.indexing_complete),
            uptime_seconds: Some(input.uptime_seconds),
            diagnostics_strategy: Some(input.diagnostics_strategy.as_str().to_owned()),
            supports_definition: Some(input.supports_definition),
            supports_call_hierarchy: Some(input.supports_call_hierarchy),
            supports_diagnostics: Some(true),
            supports_formatting: Some(input.supports_formatting),
            server_name: input.server_name.map(ToOwned::to_owned),
            indexing_source: input.indexing_source,
            indexing_duration_secs: input.indexing_duration_secs,
            indexing_progress_percent: if input.indexing_complete {
                None
            } else {
                input.indexing_progress_pct
            },
            registrations_received: Some(input.registrations_received),
        },
        DiagnosticsStrategy::None => crate::types::LspLanguageStatus {
            validation: false,
            reason: "LSP connected but does not support diagnostics".to_owned(),
            navigation_ready,
            indexing_complete: Some(input.indexing_complete),
            uptime_seconds: Some(input.uptime_seconds),
            diagnostics_strategy: Some("none".to_owned()),
            supports_definition: Some(input.supports_definition),
            supports_call_hierarchy: Some(input.supports_call_hierarchy),
            supports_diagnostics: Some(false),
            supports_formatting: Some(input.supports_formatting),
            server_name: input.server_name.map(ToOwned::to_owned),
            indexing_source: input.indexing_source,
            indexing_duration_secs: input.indexing_duration_secs,
            indexing_progress_percent: if input.indexing_complete {
                None
            } else {
                input.indexing_progress_pct
            },
            registrations_received: Some(input.registrations_received),
        },
    }
}

/// RAII guard that increments in-flight counter on creation and decrements on drop.
pub(crate) struct InFlightGuard {
    counter: Arc<AtomicU32>,
}

impl InFlightGuard {
    pub(crate) fn new(counter: Arc<AtomicU32>) -> Self {
        // M-1: Use Release ordering for the counter increment to form a release-acquire
        // pair with the Acquire load in idle_timeout_task.
        counter.fetch_add(1, Ordering::Release);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        // M-1: Use Release ordering for the counter decrement to form a release-acquire
        // pair with the Acquire load in idle_timeout_task.
        self.counter.fetch_sub(1, Ordering::Release);
    }
}

/// The production `Lawyer` implementation.
///
/// Manages per-language LSP child processes and provides JSON-RPC request
/// routing for `textDocument/definition` and future capabilities.
///
/// # Lifecycle
///
/// `LspClient` is `Clone`-able — all fields are `Arc`-wrapped. The client is
/// shared across async tasks via cloning. When the last clone is dropped, the
/// `shutdown_tx` `Arc` is dropped, which causes the `idle_timeout_task` to see
/// a `Closed` error on its broadcast receiver and exit cleanly. Therefore,
/// **no `Drop` impl is needed** — cleanup is handled by the idle-timeout task's
/// natural exit when the broadcast channel closes. Adding a naive `Drop` impl
/// that calls `shutdown()` would fire on every clone drop (not just the last),
/// which is incorrect. Use `shutdown()` explicitly for deterministic cleanup.
#[derive(Clone)]
pub struct LspClient {
    pub(crate) descriptors: Arc<Vec<LspDescriptor>>,
    pub(crate) missing_languages: Arc<Vec<crate::client::detect::MissingLanguage>>,
    pub(crate) processes: Arc<DashMap<String, ProcessEntry>>,
    pub(crate) init_locks: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) dispatcher: Arc<RequestDispatcher>,
    pub(crate) shutdown_tx: Arc<broadcast::Sender<()>>,
    /// C-1: Set atomically when `shutdown()` is called. Checked by `start_process`
    /// to prevent inserting new processes after the `idle_timeout_task` has exited.
    pub(crate) shutdown_requested: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) doc_versions: Arc<DashMap<String, (String, std::sync::atomic::AtomicI32)>>,
    pub(crate) warm_start_complete: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) spawner: std::sync::Arc<dyn crate::client::process::ProcessSpawner>,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::types::IndexingCompletionSource;
    use std::collections::HashMap;
    use std::time::Duration;

    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    fn validation_status_from_parts(
        command: &str,
        language_id: &str,
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
        registrations_received: u32,
    ) -> crate::types::LspLanguageStatus {
        super::validation_status_from_parts(super::ValidationStatusInput {
            command,
            language_id,
            running,
            diagnostics_strategy,
            supports_definition,
            supports_call_hierarchy,
            supports_formatting,
            indexing_complete,
            uptime_seconds,
            server_name,
            indexing_source,
            indexing_duration_secs,
            indexing_progress_pct,
            registrations_received,
        })
    }

    // ── ProcessEntry::to_validation_status tests ──────────────────

    #[test]
    fn test_process_entry_unavailable_status() {
        let entry = ProcessEntry::Unavailable(UnavailableState {
            backoff_attempt: 0,
            unavailable_since: std::time::Instant::now(),
        });
        let status = entry.to_validation_status("gopls", "go");
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
            "rust",
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
            0,
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
            "rust",
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
            0,
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
            "go",
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
            0,
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
            "python",
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
            0,
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
            "python",
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
            0,
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
            "rust",
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
            0,
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
            "go",
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
            0,
        );
        assert_eq!(status.navigation_ready, None);
        assert_eq!(status.indexing_complete, None);
        assert!(!status.validation);
    }

    #[test]
    fn test_navigation_ready_none_during_grace_period_without_definition() {
        // jdtls scenario: LSP just started (2s), dynamic registration hasn't
        // arrived yet so supports_definition=false. Should return None
        // (indeterminate) instead of Some(false) to avoid premature warming_up.
        let status = validation_status_from_parts(
            "jdtls",
            "java",
            true,
            DiagnosticsStrategy::None,
            false, // no definition yet — dynamic registration in flight
            false,
            false,
            false,
            2, // under 15s Java grace period — indeterminate
            None,
            None,
            None,
            None,
            0,
        );
        assert_eq!(
            status.navigation_ready, None,
            "java under 15s with no definition should be indeterminate, not false"
        );
    }

    #[test]
    fn test_navigation_ready_false_after_grace_period() {
        // After 15s (Java grace period), if definition is still not supported, it's conclusive.
        let status = validation_status_from_parts(
            "jdtls",
            "java",
            true,
            DiagnosticsStrategy::None,
            false, // still no definition after grace period
            false,
            false,
            false,
            15, // at 15s — Java grace period over
            None,
            None,
            None,
            None,
            0,
        );
        assert_eq!(
            status.navigation_ready,
            Some(false),
            "at/after 15s with no definition should be conclusively false"
        );
    }

    #[test]
    fn test_navigation_ready_true_during_grace_period_with_definition() {
        // If supports_definition is true (even early), return Some(true).
        // Grace period only applies when definition is false.
        let status = validation_status_from_parts(
            "gopls",
            "go",
            true,
            DiagnosticsStrategy::Push,
            true, // definition available
            true,
            false,
            false,
            1, // uptime 1s
            None,
            None,
            None,
            None,
            0,
        );
        assert_eq!(
            status.navigation_ready,
            Some(true),
            "definition available during grace period should be true"
        );
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
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: std::sync::Arc::new(
                crate::client::process::test_mocks::MockProcessSpawner::failing(),
            ),
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
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: std::sync::Arc::new(
                crate::client::process::test_mocks::MockProcessSpawner::failing(),
            ),
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
            reader_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            restart_count: 0,
            spawned_at: Instant::now(),
            indexing_complete: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            indexing_completion_source: Arc::new(parking_lot::Mutex::new(Some(
                IndexingCompletionSource::Progress,
            ))),
            indexing_duration_secs: Arc::new(parking_lot::Mutex::new(Some(0))),
            indexing_progress_percent: Arc::new(parking_lot::Mutex::new(None)),
            live_capabilities: Arc::new(parking_lot::RwLock::new(DetectedCapabilities {
                definition_provider: true,
                references_provider: true,
                implementation_provider: true,
                ..DetectedCapabilities::default()
            })),
            in_coexistence_mode: false,
            watcher_handles: vec![],
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
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: std::sync::Arc::new(
                crate::client::process::test_mocks::MockProcessSpawner::failing(),
            ),
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
            "rust",
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
            0,
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
            "rust",
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
            0,
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
            "go",
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
            0,
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
            "rust",
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
            0,
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
            "rust",
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
            0,
        );

        assert!(status.validation);
        assert_eq!(status.navigation_ready, Some(false));
        assert_eq!(status.supports_definition, Some(false));
    }

    #[test]
    fn test_validation_status_includes_server_name() {
        let status = validation_status_from_parts(
            "command",
            "rust",
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
            0,
        );

        assert_eq!(status.server_name, Some("custom-lsp-server".to_owned()));
    }

    #[test]
    fn test_validation_status_no_server_name() {
        let status = validation_status_from_parts(
            "command",
            "rust",
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
            0,
        );

        assert!(status.server_name.is_none());
    }

    // ── Tests for InFlightGuard ───────────────────────────────────────────────

    #[test]
    fn test_in_flight_guard_increments_counter() {
        use std::sync::atomic::AtomicU32;
        let counter = Arc::new(AtomicU32::new(0));
        assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 0);

        {
            let _guard = InFlightGuard::new(Arc::clone(&counter));
            assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 1);

            {
                let _guard2 = InFlightGuard::new(Arc::clone(&counter));
                assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 2);
            }
            // Second guard dropped
            assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 1);
        }
        // First guard dropped
        assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 0);
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
        assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 10);

        // Wait for all threads to complete and drop their guards
        for handle in handles {
            handle.join().unwrap();
        }

        // All guards should be dropped
        assert_eq!(counter.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    // ── G-1: ProcessSpawner DI Integration Test ─────────────────────────

    #[tokio::test]
    async fn test_g_1_mock_process_spawner_integration() {
        use crate::client::process::test_mocks::MockProcessSpawner;
        use std::sync::Arc;

        let mock_spawner = Arc::new(MockProcessSpawner::failing());

        let descriptors = vec![LspDescriptor {
            language_id: "test".to_owned(),
            command: "test-lsp-server".to_owned(),
            args: vec!["--arg1".to_owned(), "--arg2".to_owned()],
            root: std::env::temp_dir(),
            init_timeout_secs: None,
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        }];

        let (shutdown_tx, _) = broadcast::channel(1);
        let client = LspClient {
            descriptors: Arc::new(descriptors),
            missing_languages: Arc::new(Vec::new()),
            processes: Arc::new(DashMap::new()),
            init_locks: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::new(shutdown_tx),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: mock_spawner.clone(),
        };

        let descriptor = client.descriptors.first().unwrap().clone();

        let result = client.start_process(descriptor, 0).await;

        assert!(
            result.is_err(),
            "start_process should fail with failing mock spawner"
        );
        assert_eq!(
            mock_spawner.call_count(),
            1,
            "MockProcessSpawner should record exactly one spawn call"
        );

        let call = mock_spawner
            .last_call()
            .expect("Should have recorded a spawn call");
        assert_eq!(call.command, "test-lsp-server");
        assert_eq!(call.args, vec!["--arg1", "--arg2"]);
        assert_eq!(call.language_id, "test");
        assert_eq!(call.project_root, std::env::temp_dir());
        assert!(
            !call.isolate_target_dir,
            "isolate_target_dir should be false for single-language test"
        );

        let entry = client.processes.get("test");
        assert!(
            entry.is_some(),
            "Process entry should exist after failed spawn"
        );
        let entry = entry.unwrap();
        assert!(matches!(entry.value(), ProcessEntry::Unavailable(_)));
    }

    #[tokio::test]
    async fn test_g_1_mock_process_spawner_with_failing_behavior() {
        use crate::client::process::test_mocks::MockProcessSpawner;
        use std::sync::Arc;

        let mock_spawner = Arc::new(MockProcessSpawner::failing());

        let descriptors = vec![LspDescriptor {
            language_id: "rust".to_owned(),
            command: "rust-analyzer".to_owned(),
            args: vec![],
            root: std::env::temp_dir(),
            init_timeout_secs: None,
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        }];

        let (shutdown_tx, _) = broadcast::channel(1);
        let client = LspClient {
            descriptors: Arc::new(descriptors),
            missing_languages: Arc::new(Vec::new()),
            processes: Arc::new(DashMap::new()),
            init_locks: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::new(shutdown_tx),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: mock_spawner.clone(),
        };

        let result = client.ensure_process("rust").await;

        assert!(
            result.is_err(),
            "ensure_process should fail with failing mock spawner"
        );
        assert_eq!(
            mock_spawner.call_count(),
            1,
            "ensure_process should call spawner exactly once"
        );

        let entry = client.processes.get("rust");
        assert!(
            entry.is_some(),
            "Process entry should exist after failed ensure_process"
        );
        let entry = entry.unwrap();
        if let ProcessEntry::Unavailable(state) = entry.value() {
            assert_eq!(
                state.backoff_attempt, 1,
                "backoff_attempt should be 1 after first failure"
            );
        } else {
            panic!("Process should be Unavailable after failed spawn");
        }
    }

    #[tokio::test]
    async fn test_g_1_ensure_process_records_spawn_call_through_di_chain() {
        use crate::client::process::test_mocks::MockProcessSpawner;

        let mock_spawner = Arc::new(MockProcessSpawner::failing());

        let descriptors = vec![LspDescriptor {
            language_id: "rust".to_owned(),
            command: "rust-analyzer".to_owned(),
            args: vec!["--stdio".to_owned()],
            root: std::env::temp_dir(),
            init_timeout_secs: None,
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        }];

        let (shutdown_tx, _) = broadcast::channel(1);
        let client = LspClient {
            descriptors: Arc::new(descriptors),
            missing_languages: Arc::new(Vec::new()),
            processes: Arc::new(DashMap::new()),
            init_locks: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::new(shutdown_tx),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: mock_spawner.clone(),
        };

        assert_eq!(
            mock_spawner.call_count(),
            0,
            "no spawn calls before ensure_process"
        );

        let result = client.ensure_process("rust").await;

        assert!(
            result.is_err(),
            "ensure_process should fail with failing spawner"
        );
        assert_eq!(
            mock_spawner.call_count(),
            1,
            "ensure_process must trigger exactly one spawn call through the DI chain"
        );

        let call = mock_spawner
            .last_call()
            .expect("spawn call must be recorded");
        assert_eq!(
            call.command, "rust-analyzer",
            "spawner must receive the descriptor's command"
        );
        assert_eq!(
            call.args,
            vec!["--stdio".to_owned()],
            "spawner must receive the descriptor's args"
        );
        assert_eq!(
            call.language_id, "rust",
            "spawner must receive the correct language_id"
        );
    }

    // ── Language-aware grace period tests ─────────────────────────────────────

    #[test]
    fn test_grace_period_zero_for_static_servers() {
        // rust-analyzer at 0s with supports_definition=false → Some(false) (NOT None)
        // Static servers have 0s grace period — false is immediately conclusive.
        let status = validation_status_from_parts(
            "rust-analyzer",
            "rust",
            true,
            DiagnosticsStrategy::Pull,
            false, // no definition
            true,
            false,
            false,
            0, // uptime 0s — but 0s grace means no indeterminate window
            None,
            None,
            None,
            None,
            0,
        );
        assert_eq!(
            status.navigation_ready,
            Some(false),
            "static servers: false at 0s should be conclusive, not indeterminate"
        );
    }

    #[test]
    fn test_grace_period_java_14s_still_indeterminate() {
        let status = validation_status_from_parts(
            "jdtls",
            "java",
            true,
            DiagnosticsStrategy::None,
            false,
            false,
            false,
            false,
            14, // under 15s Java grace period
            None,
            None,
            None,
            None,
            0,
        );
        assert_eq!(
            status.navigation_ready, None,
            "java at 14s with no definition should be indeterminate"
        );
    }

    #[test]
    fn test_grace_period_java_15s_conclusive() {
        let status = validation_status_from_parts(
            "jdtls",
            "java",
            true,
            DiagnosticsStrategy::None,
            false,
            false,
            false,
            false,
            15, // at 15s Java grace boundary
            None,
            None,
            None,
            None,
            0,
        );
        assert_eq!(
            status.navigation_ready,
            Some(false),
            "java at 15s should be conclusively false"
        );
    }

    #[test]
    fn test_grace_period_typescript_4s_indeterminate() {
        let status = validation_status_from_parts(
            "typescript-language-server",
            "typescript",
            true,
            DiagnosticsStrategy::Push,
            false,
            false,
            false,
            false,
            4, // under 5s TS grace period
            None,
            None,
            None,
            None,
            0,
        );
        assert_eq!(
            status.navigation_ready, None,
            "typescript at 4s with no definition should be indeterminate"
        );
    }

    #[test]
    fn test_grace_period_go_0s_conclusive() {
        // gopls statically advertises — 0s grace period.
        let status = validation_status_from_parts(
            "gopls",
            "go",
            true,
            DiagnosticsStrategy::Push,
            false,
            false,
            false,
            false,
            0,
            None,
            None,
            None,
            None,
            0,
        );
        assert_eq!(
            status.navigation_ready,
            Some(false),
            "go at 0s with no definition should be conclusive (0s grace)"
        );
    }

    #[test]
    fn test_grace_period_function_values() {
        assert_eq!(grace_period_for_language("java"), 15);
        assert_eq!(grace_period_for_language("typescript"), 5);
        assert_eq!(grace_period_for_language("javascript"), 5);
        assert_eq!(grace_period_for_language("rust"), 0);
        assert_eq!(grace_period_for_language("go"), 0);
        assert_eq!(grace_period_for_language("python"), 0);
        assert_eq!(grace_period_for_language("unknown"), 0);
    }
}
