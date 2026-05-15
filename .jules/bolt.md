## 2024-05-18 - Avoid String Allocation on HashMap Lookups

**Learning:** When looking up keys in a HashMap using `.entry(owned_key.clone()).or_insert(...)`, a new String is allocated unconditionally on every lookup, even if the key is already in the map. However, because of NLL (Non-Lexical Lifetimes) limitations, the more intuitive approach `if let Some(v) = map.get_mut(&key) { ... } else { map.insert(key.clone(), 1); }` can result in borrow checker issues, and the compiler sometimes rejects this pattern. Also, when checking `contains_key`, it is important to pass a reference (`&key`) if the key is owned. The correct pattern to avoid allocations on cache hits while navigating borrow checker rules is to use `contains_key`, conditionally `insert`, and then use `get_mut().expect(...)`.

**Action:** Whenever possible, avoid using `.entry(key.clone())` on owned keys. Instead, use:

```rust
if !map.contains_key(&key) {
    map.insert(key.clone(), default_value);
}
#[allow(clippy::expect_used)]
let value = map.get_mut(&key).expect("inserted above");
```
This is a standard codebase pattern to circumvent `expect` denials globally and ensures zero allocations on cache hits. Pay close attention to borrow types, specifically that `.contains_key()` and `.get_mut()` require references (`&key`) instead of owned types (`key`).
