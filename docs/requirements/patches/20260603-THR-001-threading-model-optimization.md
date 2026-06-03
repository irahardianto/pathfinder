# THR-001: Threading Model Optimization — Executor Thread Starvation Fix

**Status:** Planned
**Priority:** P0 — Critical (causes MCP timeouts under concurrent load from 5+ parallel agents)
**Estimated Effort:** 18–24 hours across 5 patches
**Prerequisite:** None
**PR Strategy:** 5 progressive PRs, each independently mergeable and testable

---

## Problem Statement

Pathfinder runs on a tokio async runtime. When multiple agents send concurrent MCP tool requests,
CPU-bound tree-sitter operations execute on the async worker threads instead of the dedicated
blocking thread pool. This starves the runtime of workers that could be serving other requests,
causing MCP client timeouts (`-32001: Request timed out`).

### Root Cause

Tree-sitter parsing and symbol extraction are synchronous, CPU-bound operations called from async
contexts without `tokio::task::spawn_blocking`. A single file parse can block an async worker for
up to 500ms (the tree-sitter timeout set in `parser.rs:38`). Under 5 parallel agents, this
repeatedly blocks all available workers.

### Symptom

```
MCP error -32001: Request timed out
```

The MCP client's `-32001` timeout code collides with Pathfinder's `-32001` ACCESS_DENIED code,
making diagnosis harder. But the underlying cause is executor thread starvation.

### Scope of Impact

- **Every tool that triggers tree-sitter parsing** under concurrent load:
  `read_source_file`, `read_files`, `read_symbol_scope`, `read_with_deep_context`,
  `symbol_overview`, `get_repo_map`, `search_codebase` (enrichment), `find_symbol`
- **5 parallel agents** sending 6+ concurrent requests reliably trigger timeouts
- **Single agent** usage is mostly unaffected (one file at a time, cache hits after first parse)

---

## Architecture Overview

### Current State

```
Agent Request → MCP Handler (async) → AstCache::get_or_parse (async)
  → tokio::fs::read (async, good)
  → AstParser::parse_source (SYNC, 0-500ms, BLOCKS WORKER)
  → extract_symbols_from_tree (SYNC, 0-50ms, BLOCKS WORKER)
  → VersionHash::compute (SYNC, 0-5ms, BLOCKS WORKER)
  → Response sent
```

All synchronous work runs on the tokio async worker thread. Under concurrency, workers are
exhausted.

### Target State

```
Agent Request → MCP Handler (async) → AstCache::get_or_parse (async)
  → tokio::fs::read (async, good)
  → spawn_blocking(move || {
      AstParser::parse_source (on blocking pool)
      VersionHash::compute (on blocking pool)
    })
  → Response sent (worker was free the whole time)
```

After cache population, subsequent calls hit the LRU cache (no parse needed).

### Key Constraint: OnceCell Compatibility

`tokio::sync::OnceCell::get_or_init` takes an `async` closure. We cannot call `spawn_blocking`
directly inside it because `spawn_blocking` returns a `JoinHandle` that must be `.await`ed.

**Solution:** Use a two-phase approach:
1. `OnceCell::get_or_init` with an async closure that calls `spawn_blocking`
2. The `spawn_blocking` closure performs all synchronous work (read, hash, parse)
3. After `spawn_blocking` completes, re-acquire the cache lock to store the result

This preserves the singleflight deduplication (only one parse per file) while moving
CPU work off async workers.

---

## Patch Sequence

Each patch is independently mergeable, testable, and leaves the codebase in a working state.

### THR-001-A: Offload Tree-Sitter Parse to Blocking Pool
**Effort:** 3–4 hours | **PR:** Standalone

The foundational fix. Move `parse_source` and `VersionHash::compute` into `spawn_blocking`
inside `AstCache::get_or_parse`.

### THR-001-B: Offload Symbol Extraction to Blocking Pool
**Effort:** 2–3 hours | **PR:** Standalone, depends on THR-001-A being merged

Move `extract_symbols_from_tree` into `spawn_blocking` inside `TreeSurgeon::cached_parse`.
Amplifies the benefit of THR-001-A because symbol extraction runs on every surgeon method.

### THR-001-C: Parallelize `repo_map` File Processing
**Effort:** 4–6 hours | **PR:** Standalone, benefits from THR-001-A

Convert the sequential file loop in `repo_map` generation to concurrent batch processing.
Largest user-visible speedup (5-10x for projects with 50+ files).

### THR-001-D: Parallelize `find_symbol` and `read_files`
**Effort:** 3–4 hours | **PR:** Standalone, benefits from THR-001-A

Convert sequential loops to concurrent using `tokio::JoinSet` or `buffer_unordered`.

### THR-001-E: Runtime Configuration and Low-Priority Polish
**Effort:** 3–4 hours | **PR:** Standalone

Explicit tokio runtime builder, regex caching, `parking_lot::Mutex` evaluation.

---

## THR-001-A: Offload Tree-Sitter Parse to Blocking Pool

### Files Modified

| File | Change |
|---|---|
| `crates/pathfinder-treesitter/src/cache.rs` | Wrap parse + hash in `spawn_blocking` |
| `crates/pathfinder-treesitter/src/cache.rs` (tests) | Update any tests affected by blocking pool |

### Detailed Changes

#### 1. `crates/pathfinder-treesitter/src/cache.rs` — Non-Vue parse path

**Current code (lines 175–210):**

```rust
let result = cell
    .get_or_init(|| async {
        let content = tokio::fs::read(path).await.map_err(|e| io_err(e, path))?;
        let current_hash = VersionHash::compute(&content);
        let content_arc: Arc<[u8]> = Arc::from(content);
        let parse_input = lang.preprocess_source(&content_arc);
        let tree = AstParser::parse_source(path, lang, &parse_input)?;

        self.entries
            .lock()
            .map_err(|_| SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: "Lock poisoned".into(),
            })?
            .put(
                path.to_path_buf(),
                CacheEntry {
                    tree: tree.clone(),
                    source: content_arc.clone(),
                    content_hash: current_hash,
                    lang,
                    mtime: current_mtime,
                },
            );

        Ok::<_, SurgeonError>((tree, content_arc.clone()))
    })
    .await;
```

**Target code:**

```rust
let result = cell
    .get_or_init(|| {
        let path = path.to_path_buf();
        let entries = self.entries.clone();
        let current_mtime = current_mtime;

        async move {
            let content = tokio::fs::read(&path).await.map_err(|e| io_err(e, &path))?;

            // Move CPU-bound work (hash + parse) to the blocking pool.
            // `preprocess_source` is also synchronous and CPU-bound for Vue SFCs.
            let (tree, current_hash, content_arc) =
                tokio::task::spawn_blocking(move || {
                    let current_hash = VersionHash::compute(&content);
                    let content_arc: Arc<[u8]> = Arc::from(content);
                    let parse_input = lang.preprocess_source(&content_arc);
                    let tree = AstParser::parse_source(&path, lang, &parse_input)?;
                    Ok::<_, SurgeonError>((tree, current_hash, content_arc))
                })
                .await
                .map_err(|_| SurgeonError::ParseError {
                    path: path.clone(),
                    reason: "spawn_blocking task panicked".into(),
                })??;

            entries
                .lock()
                .map_err(|_| SurgeonError::ParseError {
                    path: path.clone(),
                    reason: "Lock poisoned".into(),
                })?
                .put(
                    path.clone(),
                    CacheEntry {
                        tree: tree.clone(),
                        source: content_arc.clone(),
                        content_hash: current_hash,
                        lang,
                        mtime: current_mtime,
                    },
                );

            Ok::<_, SurgeonError>((tree, content_arc.clone()))
        }
    })
    .await;
```

**Wait — the above has a problem.** `self.entries` is a `Mutex` (not `Arc<Mutex>`), so it can't
be cloned. And `AstCache` is shared via `Arc<AstCache>` already (confirmed below).

The actual approach: since `AstCache` is behind `Arc` in all usage sites, we can capture `self`
as `Arc<Self>` inside the async closure. But `OnceCell::get_or_init` takes `&self`, and the
closure captures by reference.

**Revised approach — restructure to avoid Arc<Self capture:**

The simplest correct approach is to extract the CPU-bound portion into a helper method that
accepts owned data and returns owned results, then call it via `spawn_blocking`:

```rust
impl AstCache {
    /// Perform CPU-bound parse work on the blocking pool.
    /// Input: owned file content. Output: parsed tree + hash.
    fn do_parse(
        path: PathBuf,
        lang: SupportedLanguage,
        content: Vec<u8>,
    ) -> Result<(Tree, VersionHash, Arc<[u8]>), SurgeonError> {
        let current_hash = VersionHash::compute(&content);
        let content_arc: Arc<[u8]> = Arc::from(content);
        let parse_input = lang.preprocess_source(&content_arc);
        let tree = AstParser::parse_source(&path, lang, &parse_input)?;
        Ok((tree, current_hash, content_arc))
    }
}
```

Then in `get_or_parse`:

```rust
let result = cell
    .get_or_init(|| async {
        let content = tokio::fs::read(path).await.map_err(|e| io_err(e, path))?;

        let path_owned = path.to_path_buf();
        let (tree, current_hash, content_arc) =
            tokio::task::spawn_blocking(move || {
                Self::do_parse(path_owned, lang, content)
            })
            .await
            .map_err(|_| SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: "spawn_blocking task panicked".into(),
            })??;

        self.entries
            .lock()
            .map_err(|_| SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: "Lock poisoned".into(),
            })?
            .put(
                path.to_path_buf(),
                CacheEntry {
                    tree: tree.clone(),
                    source: content_arc.clone(),
                    content_hash: current_hash,
                    lang,
                    mtime: current_mtime,
                },
            );

        Ok::<_, SurgeonError>((tree, content_arc.clone()))
    })
    .await;
```

**Problem:** `self` is borrowed by the async closure via `&self`. The closure captures `&self.entries`
but `spawn_blocking` requires `'static`. We need to separate the self reference from the blocking work.

**Final correct approach:** The async closure captures `&self` for the cache insertion step
(which happens after `spawn_blocking` returns, still on the async worker, and is fast).
The `spawn_blocking` closure only captures owned data:

```rust
let result = cell
    .get_or_init(|| {
        // Capture owned copies for the blocking closure.
        let path_owned = path.to_path_buf();
        let lang = lang; // Copy (it's a small enum)

        async move {
            let content = tokio::fs::read(&path_owned).await
                .map_err(|e| io_err(e, &path_owned))?;

            let (tree, current_hash, content_arc) =
                tokio::task::spawn_blocking(move || {
                    let current_hash = VersionHash::compute(&content);
                    let content_arc: Arc<[u8]> = Arc::from(content);
                    let parse_input = lang.preprocess_source(&content_arc);
                    let tree = AstParser::parse_source(&path_owned, lang, &parse_input)?;
                    Ok::<_, SurgeonError>((tree, current_hash, content_arc))
                })
                .await
                .map_err(|_| SurgeonError::ParseError {
                    path: path_owned.clone(),
                    reason: "spawn_blocking task panicked".into(),
                })??;

            // Fast cache insertion back on async worker (nanoseconds)
            self.entries
                .lock()
                .map_err(|_| SurgeonError::ParseError {
                    path: path_owned.clone(),
                    reason: "Lock poisoned".into(),
                })?
                .put(
                    path_owned,
                    CacheEntry {
                        tree: tree.clone(),
                        source: content_arc.clone(),
                        content_hash: current_hash,
                        lang,
                        mtime: current_mtime,
                    },
                );

            Ok::<_, SurgeonError>((tree, content_arc))
        }
    })
    .await;
```

**Wait — `self` is still referenced from the async closure that `get_or_init` takes.**
Looking at `tokio::sync::OnceCell::get_or_init`:

```rust
pub async fn get_or_init<F, Fut>(&self, f: F) -> &T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
```

The closure `f` is `FnOnce() -> Fut`. It's called immediately. The returned `Fut` borrows
`self` (the `AstCache`). Since `AstCache` lives behind `Arc` and the `get_or_parse` method
takes `&self`, the async block can borrow `self.entries` because `self` outlives the
`get_or_init` call. This is fine — the async block borrows `&self` (the `AstCache`), and
`spawn_blocking` only captures owned data. The borrow of `self.entries` happens AFTER
`spawn_blocking` returns, on the async worker.

This works. The key insight: `spawn_blocking` closure captures only owned data. The `self.entries`
access (for cache insertion) happens outside `spawn_blocking`, on the async worker, and is fast
(LRU put = nanoseconds).

#### 2. `crates/pathfinder-treesitter/src/cache.rs` — Vue multi-zone parse path

Same pattern applied to the Vue path (lines ~280-340). The `parse_vue_multizone` call is also
synchronous CPU-bound work. Wrap in `spawn_blocking` with the same structure.

#### 3. Test considerations

- Existing tests call `get_or_parse` via the `TreeSurgeon` which wraps `AstCache`.
- `spawn_blocking` works in tokio tests (annotated with `#[tokio::test]`).
- No test changes should be needed — the behavior is identical, only the execution context changes.
- Add one new test: concurrent parse stress test (10 concurrent `get_or_parse` calls for the same
  file) to verify singleflight deduplication still works with `spawn_blocking`.

### Acceptance Criteria

- [ ] `AstParser::parse_source` and `VersionHash::compute` execute on the blocking pool
- [ ] All existing tests pass (`cargo test --workspace`)
- [ ] No `clippy` warnings (`cargo clippy --workspace -- -D warnings`)
- [ ] Concurrent stress test added and passing
- [ ] Manual test: 5 parallel agents sending `read_source_file` requests — no timeouts

### Rollback

Revert is safe — removing `spawn_blocking` returns to current behavior (blocking async workers).
No data or API changes.

---

## THR-001-B: Offload Symbol Extraction to Blocking Pool

### Files Modified

| File | Change |
|---|---|
| `crates/pathfinder-treesitter/src/treesitter_surgeon.rs` | Wrap `extract_symbols_from_tree` in `spawn_blocking` |

### Detailed Changes

#### 1. `crates/pathfinder-treesitter/src/treesitter_surgeon.rs` — `cached_parse` method

**Current code (lines ~46-68):**

```rust
async fn cached_parse(&self, workspace_root: &Path, file_path: &Path)
    -> Result<(SupportedLanguage, Tree, Arc<[u8]>, Vec<ExtractedSymbol>), SurgeonError>
{
    // ... resolve lang, get tree from cache ...
    let (tree, source) = self.cache.get_or_parse(&abs_path, lang).await?;
    let symbols = extract_symbols_from_tree(&tree, &source, lang); // CPU-bound AST walk
    Ok((lang, tree, source, symbols))
}
```

**Target code:**

```rust
async fn cached_parse(&self, workspace_root: &Path, file_path: &Path)
    -> Result<(SupportedLanguage, Tree, Arc<[u8]>, Vec<ExtractedSymbol>), SurgeonError>
{
    // ... resolve lang, get tree from cache ...
    let (tree, source) = self.cache.get_or_parse(&abs_path, lang).await?;

    let tree_clone = tree.clone();
    let source_clone = source.clone();
    let symbols = tokio::task::spawn_blocking(move || {
        extract_symbols_from_tree(&tree_clone, &source_clone, lang)
    })
    .await
    .map_err(|_| SurgeonError::ParseError {
        path: abs_path,
        reason: "spawn_blocking task panicked during symbol extraction".into(),
    })?;

    Ok((lang, tree, source, symbols))
}
```

**Note:** `Tree` and `Arc<[u8]>` are cheap to clone (`Tree` is tree-sitter's ref-counted tree,
`Arc<[u8]>` is already ref-counted). The clone overhead is negligible compared to the
symbol extraction cost.

**Alternative (better):** Combine parse + extract into a single `spawn_blocking` call in
`AstCache::get_or_parse`. This avoids the clone entirely. However, it changes the cache
structure (caching symbols alongside the tree). This is a bigger refactor and can be deferred.

For THR-001-B, the simpler approach (clone + separate `spawn_blocking`) is sufficient. The
symbol extraction is typically 10-50ms, so moving it off the async worker is the win.

### Acceptance Criteria

- [ ] `extract_symbols_from_tree` executes on the blocking pool
- [ ] All existing tests pass
- [ ] No `clippy` warnings
- [ ] Manual test: `symbol_overview` on a large file — no regression

---

## THR-001-C: Parallelize `repo_map` File Processing

### Files Modified

| File | Change |
|---|---|
| `crates/pathfinder-treesitter/src/repo_map.rs` | Convert sequential loop to concurrent batch |

### Current Behavior

```rust
for result in walker {
    // Per file:
    let source = tokio::fs::read(path).await;           // I/O: ~1ms
    let hash = VersionHash::compute(&source);            // CPU: ~1ms
    let symbols = surgeon.extract_symbols(...).await;    // CPU: 10-100ms (parse + extract)
    let filtered = filter_by_visibility(symbols, ...);   // CPU: ~1ms
    let skeleton = render_file_skeleton(&filtered, ...);  // CPU: ~1ms
}
// Total: O(n) * (10-100ms) = 1-10s for 100 files
```

### Target Design

Two-phase concurrent approach:

```
Phase 1: Discovery + Dispatch (async, fast)
  - Walk directory tree (tokio::fs::read_dir) — collect all file paths
  - Sort by estimated relevance (source files first, config last)
  - Create processing batches

Phase 2: Concurrent Parse + Extract (spawn_blocking pool)
  - Use futures::stream::iter(batches)
    .map(|batch| async { spawn_blocking(parse_batch) })
    .buffer_unordered(BATCH_CONCURRENCY)  // cap at 8-16
  - Each batch: read file + parse + extract symbols
  - Collect results into a Vec

Phase 3: Sequential Render (single-threaded, token-budget-aware)
  - Iterate collected results
  - Render skeletons sequentially
  - Stop when token budget exhausted
```

**Concurrency cap:** `BATCH_CONCURRENCY = min(num_cpus, 16)`. This prevents overwhelming the
blocking pool while maximizing throughput.

### Token Budget Interaction

`get_repo_map` has a `max_tokens` parameter that limits output size. The current sequential
approach stops rendering when the budget is exceeded but still parses all files.

Optimization: In Phase 2, we don't know which files will make the cut. Options:
1. Parse all files, render until budget full (current behavior, now faster)
2. Heuristic: parse only until estimated output exceeds 2x budget (aggressive, risky)

Go with option 1 for safety. The parallelism gain alone is 5-10x.

### Acceptance Criteria

- [ ] `get_repo_map` processes files concurrently via `buffer_unordered`
- [ ] Token budget logic preserved (stop rendering when exceeded)
- [ ] Output identical to current implementation (same file order for same inputs)
- [ ] All existing tests pass
- [ ] Performance benchmark: repo_map on a 100-file project — under 2s (currently 5-10s)

---

## THR-001-D: Parallelize `find_symbol` and `read_files`

### Files Modified

| File | Change |
|---|---|
| `crates/pathfinder/src/server/tools/find_symbol.rs` | Concurrent pattern search |
| `crates/pathfinder/src/server/tools/read_files.rs` | Concurrent file reading |

### THR-001-D.1: `find_symbol` — Concurrent Pattern Search

**Current:** Sequential loop over up to 32 patterns (8 extensions x ~4 patterns each).

```rust
for (pattern, glob) in patterns {
    match self.scout.search(&search_params).await { ... }
}
```

**Target:**

```rust
use futures::stream::{self, StreamExt};

const FIND_SYMBOL_CONCURRENCY: usize = 8;

let results: Vec<(String, SearchResult)> = stream::iter(patterns)
    .map(|(pattern, glob)| {
        let scout = self.scout.clone();
        async move {
            let search_params = SearchParams { query: pattern.clone(), is_regex: true, ... };
            let result = scout.search(&search_params).await;
            (pattern, result)
        }
    })
    .buffer_unordered(FIND_SYMBOL_CONCURRENCY)
    .collect()
    .await;
```

Each `scout.search` already uses `spawn_blocking` internally (ripgrep.rs). The `buffer_unordered`
dispatches multiple searches to the blocking pool concurrently instead of waiting for each to
complete before starting the next.

### THR-001-D.2: `read_files` — Concurrent File Processing

**Current:** Sequential loop over up to 10 files.

```rust
for file_path in &params.paths {
    let result = self.read_single_file(file_path, &params).await;
    file_results.push(result);
}
```

**Target:**

```rust
use tokio::task::JoinSet;

const READ_FILES_CONCURRENCY: usize = 5;

let mut set = JoinSet::new();
for file_path in &params.paths {
    set.spawn(self.read_single_file(file_path.clone(), params.clone()));
}

let mut file_results = Vec::with_capacity(params.paths.len());
while let Some(res) = set.join_next().await {
    file_results.push(res.unwrap());
}
```

The existing comment about "file descriptor exhaustion" is valid for unbounded concurrency.
Capping at 5 (well under the 10-file limit) is safe and provides parallelism.

**Note:** `read_single_file` must be `Sync` + `Send` safe. Verify this when implementing.

### Acceptance Criteria

- [ ] `find_symbol` searches patterns concurrently with `buffer_unordered(8)`
- [ ] `read_files` processes files concurrently with `JoinSet` capped at 5
- [ ] All existing tests pass
- [ ] No `clippy` warnings
- [ ] Manual test: `find_symbol` for a common name — faster than before, same results

---

## THR-001-E: Runtime Configuration and Low-Priority Polish

### Files Modified

| File | Change |
|---|---|
| `crates/pathfinder/src/main.rs` | Replace `#[tokio::main]` with explicit runtime builder |
| `crates/pathfinder/src/server/tools/find_symbol.rs` | Cache compiled regexes |
| `crates/pathfinder/src/server/tools/navigation/mod.rs` | Cache compiled regexes |

### THR-001-E.1: Explicit Runtime Builder

**Current:**

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> { ... }
```

**Target:**

```rust
fn main() -> anyhow::Result<()> {
    let worker_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(64)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let cli = Cli::parse();
        run(cli.workspace_path, cli.lsp_trace).await
    })
}
```

Rationale:
- `max_blocking_threads(64)` — after THR-001-A/B, tree-sitter parse + symbol extraction
  land on this pool. 64 threads is generous for a machine with 8-16 cores, allowing multiple
  concurrent parses without pool exhaustion.
- Explicit `worker_threads` — makes the thread count visible and tunable.
- `#[tokio::main]` defaults to 512 blocking threads, which is wasteful. 64 is more appropriate
  for CPU-bound work (one thread per core would be ideal, but some headroom for I/O mixing is good).

### THR-001-E.2: Regex Caching

**`find_symbol.rs`** — compile regex patterns once per tool invocation (not per pattern).

```rust
use std::sync::LazyLock; // or once_cell::sync::Lazy

// Cache common regex patterns at module level
static SYMBOL_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
    // ... compile all patterns ...
});
```

**`navigation/mod.rs::extract_call_candidates`** — cache the regex.

### THR-001-E.3: Evaluate `parking_lot::Mutex` for `AstCache`

Replace `std::sync::Mutex` with `parking_lot::Mutex` in `AstCache`:
- Smaller memory footprint (no poisoning overhead)
- Slightly faster uncontended lock acquisition
- Consistent with LSP client code which already uses `parking_lot`

Low priority — the lock hold times are nanoseconds, so the gain is marginal. But it
improves code consistency.

### Acceptance Criteria

- [ ] Explicit runtime builder with documented thread counts
- [ ] Regex compilation cached in `find_symbol` and `extract_call_candidates`
- [ ] All existing tests pass
- [ ] No `clippy` warnings
- [ ] Runtime thread counts logged at startup (info level)

---

## Testing Strategy

### Per-Patch

Each patch includes:

1. **Existing tests** — all must pass (`cargo test --workspace`)
2. **Clippy** — no warnings (`cargo clippy --workspace -- -D warnings`)
3. **New concurrent stress test** — only for THR-001-A (spawn_blocking correctness)

### Integration Test (After All Patches)

Create a stress test scenario:

```
1. Start Pathfinder MCP server
2. Send 20 concurrent tool requests (mix of read_source_file, search, repo_map)
3. Verify: zero timeouts, all responses received within 30s
4. Verify: CPU utilization across multiple cores (not single-threaded)
```

### Performance Benchmarks

| Metric | Before | Target |
|---|---|---|
| `get_repo_map` (100 files) | 5-10s | Under 2s |
| `find_symbol` (8 patterns) | Sequential sum | max(individual) + 10% |
| `read_files` (10 files) | Sequential sum | max(individual) + 20% |
| 5 concurrent agents, 6 requests each | Timeouts | Zero timeouts |

---

## Risk Assessment

### THR-001-A (spawn_blocking in AstCache)

| Risk | Likelihood | Mitigation |
|---|---|---|
| `spawn_blocking` JoinHandle panic | Low | Map `JoinError` to `SurgeonError::ParseError` |
| Blocking pool exhaustion | Low | `max_blocking_threads(64)` in THR-001-E |
| Cache lock contention increase | Low | Locks held for nanoseconds, unchanged |
| OnceCell closure borrow conflict | Medium | Careful closure design (owned captures only) |

### THR-001-C (repo_map parallelism)

| Risk | Likelihood | Mitigation |
|---|---|---|
| Output ordering changes | Medium | Sort results before rendering (same as current) |
| Token budget overspend | Low | Parse all, render sequentially (same as current) |
| File descriptor exhaustion | Low | Cap concurrency at 16 |

### THR-001-D (find_symbol, read_files parallelism)

| Risk | Likelihood | Mitigation |
|---|---|---|
| Result ordering changes | Low | Sort results by relevance (same scoring) |
| FD exhaustion in read_files | Low | Cap at 5 concurrent |

---

## Dependency Graph

```
THR-001-A (spawn_blocking parse)
    |
    +---> THR-001-B (spawn_blocking extract) [depends on A]
    |
    +---> THR-001-C (repo_map parallel) [benefits from A, not strictly dependent]
    |
    +---> THR-001-D (find_symbol, read_files parallel) [benefits from A]
    |
    +---> THR-001-E (runtime config, polish) [independent, can ship anytime]
```

Recommended merge order: A → B → E → C → D

Rationale:
- A first: foundational, unblocks all others
- B second: compounds A's benefit
- E third: tunes the runtime for the new blocking workload, safe standalone
- C fourth: largest refactor, needs A+B+E stable
- D fifth: smallest impact, simplest changes

---

## Out of Scope

The following were identified but are deferred:

1. **Adding `rayon` dependency** — for CPU-bound batch rendering (repo_map). The `spawn_blocking`
   approach with `buffer_unordered` is simpler and sufficient. Rayon can be added later if
   profiling shows the blocking pool as a bottleneck.

2. **Error code collision (`-32001`)** — Pathfinder's ACCESS_DENIED and the MCP client's timeout
   both use `-32001`. This is a cosmetic issue that doesn't cause failures. Fix would require
   coordinating with the opencode MCP client codebase. Low priority.

3. **LSP stdin serialization** — All LSP requests for a language serialize through a single stdin.
   This is inherent to JSON-RPC-over-stdio. Not a Pathfinder defect. Would require a fundamental
   protocol change (e.g., LSP over TCP) to address.

4. **Agent behavior (skipping `get_repo_map`)** — Agents using grep instead of `get_repo_map`
   generates more concurrent parse requests. This is a usage pattern fix, not a code fix. Address
   via agent skill documentation updates.
