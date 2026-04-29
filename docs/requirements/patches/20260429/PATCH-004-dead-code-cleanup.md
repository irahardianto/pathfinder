# PATCH-004: Dead Code Removal

## Status: COMPLETED (2026-04-29)

## Objective

Remove two genuinely unused fields that are hidden behind `#[allow(dead_code)]` suppressors. Prefer outright **deletion** over annotation upgrades — no dead code should live in the codebase with a suppressor as an excuse. Also remove the erroneous `#[allow(dead_code)]` from `sandbox` (which is actively used in 18+ call sites).

## Severity: LOW — Housekeeping, eliminates dead weight

---

## Scope

| # | File | Line(s) | Action |
|---|------|---------|--------|
| 1 | `crates/pathfinder/src/server.rs` | ~49-52, ~140 | REMOVE `config` field + annotation; REMOVE `#[allow]` from `sandbox` |
| 2 | `crates/pathfinder-lsp/src/client/process.rs` | ~59-60, ~142 | REMOVE `language_id` field + annotation + initializer |

> **Note:** Documentation comments for false positives (`sandbox.rs`, `vue_zones.rs`, `types.rs`) are handled in **PATCH-006** to keep concerns separated.

---

## Task 4.1: Remove unused `config` field from `PathfinderServer`

**File:** `crates/pathfinder/src/server.rs`

`self.config` is **never read** anywhere in the codebase (confirmed: `grep -r "self\.config"` returns zero results). Remove the field and its `#[allow]` annotation. Also remove the erroneous `#[allow(dead_code)]` from `sandbox` — that field IS used.

### Struct definition (lines ~46–57)

**Find:**
```rust
#[derive(Clone)]
pub struct PathfinderServer {
    workspace_root: Arc<WorkspaceRoot>,
    #[allow(dead_code)]
    config: Arc<PathfinderConfig>,
    #[allow(dead_code)]
    sandbox: Arc<Sandbox>,
    scout: Arc<dyn Scout>,
    surgeon: Arc<dyn Surgeon>,
    lawyer: Arc<dyn Lawyer>,
    tool_router: ToolRouter<Self>,
}
```

**Replace with:**
```rust
#[derive(Clone)]
pub struct PathfinderServer {
    workspace_root: Arc<WorkspaceRoot>,
    sandbox: Arc<Sandbox>,
    scout: Arc<dyn Scout>,
    surgeon: Arc<dyn Surgeon>,
    lawyer: Arc<dyn Lawyer>,
    tool_router: ToolRouter<Self>,
}
```

### `with_all_engines` body (lines ~138–147)

**Find:**
```rust
        Self {
            workspace_root: Arc::new(workspace_root),
            config: Arc::new(config),
            sandbox: Arc::new(sandbox),
            scout,
            surgeon,
            lawyer,
            tool_router: Self::tool_router(),
        }
```

**Replace with:**
```rust
        Self {
            workspace_root: Arc::new(workspace_root),
            sandbox: Arc::new(sandbox),
            scout,
            surgeon,
            lawyer,
            tool_router: Self::tool_router(),
        }
```

> [!IMPORTANT]
> Do **NOT** remove `config` from the `with_all_engines` **parameter list** — there are 20+ call sites in tests. Simply stop assigning it to a struct field. If the compiler warns about an unused parameter, add `let _ = config;` at the top of the function body.

---

## Task 4.2: Remove unused `language_id` field from `ManagedProcess`

**File:** `crates/pathfinder-lsp/src/client/process.rs`

The `language_id: String` field is stored in the struct but **never read back via `process.language_id`**. The tracing log uses the local `language_id: &str` parameter, not the stored field. Remove the field entirely — no annotation needed.

### Struct definition (lines ~52–67)

**Find:**
```rust
pub(super) struct ManagedProcess {
    /// The child process handle — kept alive until explicitly dropped.
    pub(super) child: Child,
    /// Exclusive write handle to the LSP's stdin.
    pub(super) stdin: Mutex<tokio::io::BufWriter<ChildStdin>>,
    /// The language this process serves.
    #[allow(dead_code)] // Kept for debugging/logging; not yet used in dispatch
    pub(super) language_id: String,
    /// Capabilities negotiated during `initialize`.
    pub(super) capabilities: DetectedCapabilities,
    /// Last time this process was used (for idle-timeout tracking).
    pub(super) last_used: Instant,
    /// Number of in-flight requests (prevents idle timeout during active ops)
    pub(super) in_flight: Arc<AtomicU32>,
}
```

**Replace with:**
```rust
pub(super) struct ManagedProcess {
    /// The child process handle — kept alive until explicitly dropped.
    pub(super) child: Child,
    /// Exclusive write handle to the LSP's stdin.
    pub(super) stdin: Mutex<tokio::io::BufWriter<ChildStdin>>,
    /// Capabilities negotiated during `initialize`.
    pub(super) capabilities: DetectedCapabilities,
    /// Last time this process was used (for idle-timeout tracking).
    pub(super) last_used: Instant,
    /// Number of in-flight requests (prevents idle timeout during active ops).
    pub(super) in_flight: Arc<AtomicU32>,
}
```

### Struct initializer in `spawn_and_initialize` (lines ~139–146)

**Find:**
```rust
    let process = ManagedProcess {
        child,
        stdin: Mutex::new(writer),
        language_id: language_id.to_owned(),
        capabilities,
        last_used: Instant::now(),
        in_flight: Arc::new(AtomicU32::new(0)),
    };
```

**Replace with:**
```rust
    let process = ManagedProcess {
        child,
        stdin: Mutex::new(writer),
        capabilities,
        last_used: Instant::now(),
        in_flight: Arc::new(AtomicU32::new(0)),
    };
```

---

## Verification

```bash
# 1. Confirm no dead_code allows remain on targeted items
grep -n 'allow(dead_code)' crates/pathfinder/src/server.rs
# Expected: 1 result — line ~110 (cfg_attr for with_engines test method). NOT on config/sandbox.

grep -n 'allow(dead_code)' crates/pathfinder-lsp/src/client/process.rs
# Expected: ZERO results

# 2. Confirm config struct field is gone
grep -n 'config: Arc' crates/pathfinder/src/server.rs
# Expected: ZERO results (no struct field line)

# 3. Confirm language_id struct field is gone
grep -n 'language_id: String' crates/pathfinder-lsp/src/client/process.rs
# Expected: ZERO results

# 4. Full build and test
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Completion Criteria

- [ ] `config` field removed from `PathfinderServer` struct definition
- [ ] `config: Arc::new(config)` assignment removed from `with_all_engines` body
- [ ] `sandbox` field `#[allow(dead_code)]` annotation removed (field kept — it is used)
- [ ] `language_id` field removed from `ManagedProcess` struct definition
- [ ] `language_id: language_id.to_owned()` removed from struct initializer in `spawn_and_initialize`
- [ ] No `#[allow(dead_code)]` remains on any of the above items
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --all` passes
