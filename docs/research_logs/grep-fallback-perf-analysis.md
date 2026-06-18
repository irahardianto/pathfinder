# Performance Analysis: Grep Fallback

## 1. Problem Statement
The LSP grep fallback is very slow. When the LSP server is unavailable or warming up, Pathfinder falls back to a sequence of grep-based searches (up to 4 strategies: file-scoped, impl-scoped, global definition, global broad). Each of these search strategies invokes `Scout::search` (`RipgrepScout`).

## 2. Baseline Profiling Data
We established a baseline benchmark running on the `pathfinder` workspace itself, which is a representative medium-sized Rust project.

- **Workspace File Count**: 56,823 files (including `.git/` and `target/` directories).
- **Non-build/git File Count**: 313 files.
- **Baseline Latency (Worst-case Global Search)**: **211.87 ms**

## 3. Root Cause Analysis
The search engine `walk_files` method performs two separate directory traversals using `ignore::WalkBuilder`:
1. **Pass 1 (No Ignore)**: Traverses the workspace with `git_ignore(false)` and `hidden(false)` to count files that would pass filters if gitignore was disabled.
2. **Pass 2 (With Ignore)**: Traverses the workspace with `git_ignore(true)` and `hidden(false)` to collect files.

### Critical Issues:
1. **Unnecessary Traversals**: Pass 1 is only used to compute the `gitignored_skipped` metric. Walking the entire repository (including `target/` and `.git/`) just to compute a diagnostic count is highly wasteful.
2. **Lack of Directory-Level Pruning**: `WalkBuilder` is configured with `hidden(false)`. Since `.git/` is a hidden directory, the walker recurses into `.git/` completely (over 50,000 files in our workspace). Additionally, since Pass 1 has `git_ignore(false)`, the walker recurses into the build directory `target/` completely.
3. **No Skip at Walk Level**: Excluded directories (like `.git/`, `node_modules/`, `vendor/`, `target/`) are only filtered *after* walking, inside `filter_entry`. This means `WalkBuilder` does all the filesystem recursion and syscalls for 56,000+ files, only for them to be discarded in user-land.

## 4. Proposed Optimization Plan
We will optimize `walk_files` to:
1. **Prune Excluded Directories at Walk Level**: Use `WalkBuilder::filter_entry` to prevent the walker from descending into `.git/`, `node_modules/`, `vendor/`, `target/`, `.idea/`, `.vscode/`, `__pycache__/`, `.qlty/`.
2. **Amortize/Simplify gitignored_skipped calculation**: If we prune these directories, the difference in traversal time will be massive. Let's see if we can do this in a single walk, or keep the two-pass structure but with pruning on both.
