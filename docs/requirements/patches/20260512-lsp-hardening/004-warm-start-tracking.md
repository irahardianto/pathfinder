# 004: Track `warm_start` Completion

**Epic**: 2 — LSP Hardening
**Status**: ✅ Complete (2026-05-23)
**Severity**: High
**Risk**: Medium — touches LSP lifecycle state machine

---

## Problem

`LspClient::warm_start_for_languages()` spawns background tasks to initialize LSP processes for detected languages, but nothing tracks whether these tasks have completed. This causes two observable issues:

1. **First-call latency**: The first `get_definition` or `analyze_impact` call after `get_repo_map` hits a 5–30s delay while the LSP initializes on-demand, even though `warm_start` was already triggered.

2. **Misleading health status**: `lsp_health` may show `"warming_up"` indefinitely because it cannot distinguish "still warming" from "warm_start failed silently." There is no signal for "warm_start completed but LSP didn't report readiness."

### Current Flow

```
get_repo_map
  → detects tech_stack: ["rust", "typescript"]
  → tokio::spawn(lawyer.warm_start_for_languages(&languages))
  → returns immediately (fire-and-forget)

// 2 seconds later...
get_definition("src/auth.ts::login")
  → LspClient has no TypeScript LSP yet (warm_start still running)
  → fallback to lazy init → 15s delay while tsserver boots
```

---

## Proposed Solution

### 1. Return `JoinHandle`s from `warm_start`

Replace the fire-and-forget spawn with tracked handles:

```rust
// In LspClient:
pub fn warm_start_for_languages(&self, languages: &[String]) -> Vec<JoinHandle<()>> {
    languages.iter().map(|lang| {
        let client = self.clone();
        let lang = lang.clone();
        tokio::spawn(async move {
            if let Err(e) = client.ensure_started(&lang).await {
                tracing::warn!(language = %lang, error = %e, "warm_start failed");
            }
        })
    }).collect()
}
```

### 2. Add `warm_start_complete` Flag

Add an `Arc<AtomicBool>` to `LspClient` that is set `true` once all warm_start handles join:

```rust
pub struct LspClient {
    // ... existing fields ...
    warm_start_complete: Arc<AtomicBool>,
}
```

In `get_repo_map_impl`, after spawning warm_start:

```rust
let handles = lawyer.warm_start_for_languages(&languages);
let warm_flag = Arc::clone(&lawyer.warm_start_complete);
tokio::spawn(async move {
    for handle in handles {
        let _ = handle.await;
    }
    warm_flag.store(true, Ordering::Release);
    tracing::info!("warm_start: all languages initialized");
});
```

### 3. Expose in `lsp_health`

`lsp_health_impl` checks `warm_start_complete` and includes it in the response:

```rust
pub struct LspHealthResponse {
    // ... existing fields ...
    pub warm_start_complete: bool,
}
```

### Files to Modify

| File | Change |
|------|--------|
| `crates/pathfinder-lsp/src/client/mod.rs` | Return `Vec<JoinHandle>` from `warm_start_for_languages`; add `warm_start_complete` field |
| `crates/pathfinder/src/server/tools/repo_map.rs` | Track warm_start handles; set completion flag |
| `crates/pathfinder/src/server/tools/navigation.rs` | Expose `warm_start_complete` in `lsp_health` response |
| `crates/pathfinder/src/server/types.rs` | Add `warm_start_complete` to `LspHealthResponse` |

---

## Acceptance Criteria

- [x] `warm_start_for_languages()` returns `Vec<JoinHandle<()>>` instead of `()`
- [x] `warm_start_complete` is `false` during initialization, `true` once all handles join
- [x] `lsp_health` response includes `warm_start_complete: bool`
- [x] Failed warm_start attempts are logged at `warn` level with language and error
- [x] If one language fails, others still complete (per-language resilience)
- [x] `warm_start_complete` is `true` even if some languages failed (it means "the process finished")

---

## Test Plan

| Test | Description |
|------|-------------|
| `test_warm_start_sets_complete_flag` | NoOpLawyer warm_start → flag set immediately |
| `test_warm_start_complete_false_during_init` | MockLawyer with delay → flag false during init, true after |
| `test_warm_start_partial_failure_still_completes` | MockLawyer: one language errors → flag still set true |
| `test_lsp_health_reports_warm_start_status` | Integration: warm_start_complete appears in lsp_health response |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- warm_start
cargo test -p pathfinder-mcp-lsp -- warm_start
cargo clippy --workspace -- -D warnings
```
