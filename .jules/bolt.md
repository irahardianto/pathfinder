## 2025-01-20 - Avoid Unconditional String Clones with HashMap::entry

**Learning:** When grouping data in HashMaps where the key is an owned String and the lookup key is also passed as an owned String, using `HashMap::entry(key.clone())` causes an unconditional string allocation on every cache hit. This happens because `.entry()` consumes the key, requiring a clone even if the key already exists in the map. This creates a hot path performance bottleneck due to excessive heap allocations.

**Action:** Avoid `HashMap::entry` when dealing with owned String keys unless the string is being constructed efficiently or an allocation is strictly required. Prefer checking the map first with `.get_mut()` (which allows borrowing the key as `&str`) followed by a conditional `.insert()` to save allocations on cache hits.
