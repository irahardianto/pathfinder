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

#[derive(Clone, Copy)]
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
    indexing_source: Option<&'a str>,
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
                            IndexingCompletionSource::Progress => "progress",
                            IndexingCompletionSource::TimeoutFallback => "timeout_fallback",
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
fn build_language_status(
    input: &ValidationStatusInput<'_>,
    validation: bool,
    reason: String,
    diagnostics_strategy: String,
    supports_diagnostics: bool,
    navigation_ready: Option<bool>,
) -> crate::types::LspLanguageStatus {
    crate::types::LspLanguageStatus {
        validation,
        reason,
        navigation_ready,
        indexing_complete: Some(input.indexing_complete),
        uptime_seconds: Some(input.uptime_seconds),
        diagnostics_strategy: Some(diagnostics_strategy),
        supports_definition: Some(input.supports_definition),
        supports_call_hierarchy: Some(input.supports_call_hierarchy),
        supports_diagnostics: Some(supports_diagnostics),
        supports_formatting: Some(input.supports_formatting),
        server_name: input.server_name.map(ToOwned::to_owned),
        indexing_source: input.indexing_source.map(ToOwned::to_owned),
        indexing_duration_secs: input.indexing_duration_secs,
        indexing_progress_percent: if input.indexing_complete {
            None
        } else {
            input.indexing_progress_pct
        },
        registrations_received: Some(input.registrations_received),
    }
}

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
        DiagnosticsStrategy::Pull | DiagnosticsStrategy::Push => {
            let reason = format!(
                "LSP connected and supports validation ({})",
                if matches!(input.diagnostics_strategy, DiagnosticsStrategy::Pull) {
                    "pull diagnostics"
                } else {
                    "push diagnostics"
                }
            );
            build_language_status(
                &input,
                true,
                reason,
                input.diagnostics_strategy.as_str().to_owned(),
                true,
                navigation_ready,
            )
        }
        DiagnosticsStrategy::None => build_language_status(
            &input,
            false,
            "LSP connected but does not support diagnostics".to_owned(),
            "none".to_owned(),
            false,
            navigation_ready,
        ),
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
#[path = "mod_test.rs"]
mod tests;
