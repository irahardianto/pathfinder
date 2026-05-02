
## 2026-05-02 - Unnecessary Mutex overhead in Sequential Loops
**Learning:** `RipgrepScout::search` processed files sequentially, yet used `std::sync::Mutex` to wrap the `match_buf` and `total_count`, resulting in unnecessary locking and unlocking for every match and file hashed.
**Action:** Always verify if a scope is actually accessed concurrently before using synchronization primitives. If data is mutated sequentially within a single thread, use mutable references (`&mut T`) instead of a `Mutex<T>`.
