# GAP-002: Re-Probe "Ready" Languages on lsp_health Calls

## Group: A (Critical) — LSP Timeout Resilience
## Depends on: GAP-001 (for the timeout fallback to be meaningful)

## Objective

Both reports identified that `lsp_health` reports "ready" even when the LSP is
non-responsive. The probe mechanism (`probe_language_readiness`) exists and works
correctly, but it ONLY fires for languages with status "warming_up". Once a language
reaches "ready" status (via `navigation_ready: true` from the LSP initialize handshake),
the probe is never run again.

This means: if the LSP process becomes non-responsive AFTER initial readiness
(e.g., stuck indexing, memory pressure, internal deadlock), `lsp_health` continues
to report "ready" indefinitely. Agents trust this and proceed to use LSP-dependent
tools, which then timeout.

The fix: add a lightweight "liveness probe" for "ready" languages that checks
whether the LSP can still respond to queries within a reasonable time.

## Scope

| File | Function | Change |
|------|----------|--------|
| `crates/pathfinder/src/server/tools/navigation.rs` | `lsp_health_impl` | Add liveness probe for "ready" languages |
| `crates/pathfinder/src/server.rs` | `ProbeCacheEntry` | Extend with liveness timestamp |
| `crates/pathfinder/src/server/tools/navigation.rs` | tests | Add test for liveness probe |

## Current Code

In `lsp_health_impl`, the probe only runs inside this block:

```rust
// Lines ~1330-1390 in navigation.rs
for lang_health in &mut languages {
    if lang_health.status == "warming_up" {  // ← ONLY for warming_up!
        // ... probe logic with cache ...
        let uptime_secs = parse_uptime_to_seconds(lang_health.uptime.as_deref());
        if let Some(secs) = uptime_secs {
            if secs > 10 {
                let probe_result = self.probe_language_readiness(&lang_health.language).await;
                // ...
            }
        }
    }
}
```

Languages with `status == "ready"` skip this entire block.

## Target Code

Add a second loop after the existing warming_up probe loop:

```rust
// After the warming_up probe loop, add:

// LIVENESS PROBE: Verify "ready" languages can still respond.
// Runs at most once per LIVENESS_PROBE_INTERVAL_SECS per language.
// Uses the same ProbeCacheEntry mechanism for caching.
const LIVENESS_PROBE_INTERVAL_SECS: u64 = 120; // Re-probe every 2 minutes

for lang_health in &mut languages {
    if lang_health.status != "ready" {
        continue;
    }

    // Check liveness cache
    let cache_action = {
        let cache = self
            .probe_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match cache.get(&lang_health.language) {
            Some(entry) if entry.is_valid() && entry.success => {
                // Positive entry — check if it's time for a re-probe
                if entry.age_secs() < LIVENESS_PROBE_INTERVAL_SECS {
                    ProbeAction::UseCachedReady
                } else {
                    ProbeAction::Probe // Stale — re-probe
                }
            }
            Some(entry) if entry.is_valid() && !entry.success => {
                ProbeAction::SkipProbe
            }
            Some(_) => {
                ProbeAction::Probe // Expired
            }
            None => ProbeAction::Probe, // Never probed (shouldn't happen for "ready")
        }
    };

    match cache_action {
        ProbeAction::UseCachedReady => {
            lang_health.probe_verified = true;
            continue;
        }
        ProbeAction::SkipProbe => continue,
        ProbeAction::Probe => {}
    }

    // Run the same probe as warming_up
    let probe_result = self.probe_language_readiness(&lang_health.language).await;

    if probe_result {
        // Still alive — cache positive result
        lang_health.probe_verified = true;
        self.probe_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(lang_health.language.clone(), ProbeCacheEntry::new(true));
    } else {
        // LSP is dead! Downgrade from "ready" to "degraded"
        lang_health.status = "degraded".to_owned();
        lang_health.probe_verified = false;
        // Cache negative result
        self.probe_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(lang_health.language.clone(), ProbeCacheEntry::new(false));

        // Downgrade overall status if all ready languages are now degraded
        if languages.iter().all(|l| l.status != "ready") {
            overall_status = "degraded";
        }
    }
}
```

Also extend `ProbeCacheEntry` with a timestamp for age-based re-probe:

```rust
// In server.rs, extend ProbeCacheEntry:
pub(crate) struct ProbeCacheEntry {
    success: bool,
    created_at: std::time::Instant,  // NEW
    ttl: Option<std::time::Duration>, // NEW: negative entries expire
}

impl ProbeCacheEntry {
    pub(crate) fn new(success: bool) -> Self {
        Self {
            success,
            created_at: std::time::Instant::now(),
            ttl: if !success {
                Some(std::time::Duration::from_secs(PROBE_NEGATIVE_TTL_SECS))
            } else {
                None // Positive entries: use age-based re-probe instead of expiry
            },
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        match self.ttl {
            Some(ttl) => self.created_at.elapsed() < ttl,
            None => true, // Positive entries never expire (liveness re-probe handles staleness)
        }
    }

    // NEW: How old is this cache entry?
    pub(crate) fn age_secs(&self) -> u64 {
        self.created_at.elapsed().as_secs()
    }
}
```

## Important Design Decision: "degraded" vs "unavailable"

The new status "degraded" is used instead of "warming_up" because:
1. The LSP process IS running (unlike "unavailable")
2. It WAS ready but became non-responsive (unlike "warming_up" which implies never-ready)
3. Agents need to know they should avoid LSP-dependent tools (like "degraded" in analyze_impact)

The status values become:
- `unavailable` — no LSP process at all
- `starting` — process exists, not yet initialized
- `warming_up` — initialized but not yet confirmed responsive
- `ready` — confirmed responsive (probe_verified = true)
- `degraded` — was ready, now non-responsive (liveness probe failed)

## Exclusions

- Do NOT downgrade "ready" to "warming_up" — that would trigger the startup probe
  path which has different caching semantics.
- Do NOT add the liveness probe to every lsp_health call — use cache with
  LIVENESS_PROBE_INTERVAL_SECS to avoid LSP hammering.
- Do NOT change probe_language_readiness itself — it's correct as-is.

## Verification

```bash
cargo test -p pathfinder --lib -- test_lsp_health_liveness_probe_downgrades_dead_lsp
cargo test -p pathfinder --lib -- test_lsp_health_liveness_probe_caches_positive
cargo test -p pathfinder --lib -- test_liveness_probe_interval_skips_recent
```

## Tests

### Test 1: test_lsp_health_liveness_probe_downgrades_dead_lsp
```rust
// Setup: server with MockLawyer that was "ready" but now returns Err for goto_definition
// Call lsp_health
// Verify: status = "degraded", probe_verified = false
```

### Test 2: test_lsp_health_liveness_probe_caches_positive
```rust
// Setup: server with MockLawyer that returns Ok for goto_definition
// Call lsp_health twice
// Verify: second call uses cached result (probe_verified = true, no second probe call)
```

### Test 3: test_liveness_probe_interval_skips_recent
```rust
// Setup: server with recently-cached positive entry (age < LIVENESS_PROBE_INTERVAL_SECS)
// Call lsp_health
// Verify: no probe is fired (UseCachedReady action)
```
