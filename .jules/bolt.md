## 2026-05-08 - Optimized search codebase normalize_path memory allocations
**Learning:** `normalize_path` was returning an owned `String` unconditionally and being called heavily in the `.map` closures and `group_by_file` loop within `search_codebase` (especially for O(N) matches processing). This caused unnecessary heap allocations (2 allocs per map/lookup).
**Action:** Changed `normalize_path` to return a `&str`, updated closures, and used `&str` lookup in the HashSet and HashMap instead of owned Strings to significantly reduce allocation pressure in large `search_codebase` workloads.
