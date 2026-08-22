//! Caller chain >= 2 deep: chain_root -> chain_middle -> chain_leaf.

/// Leaf function with callers.
pub fn chain_leaf() -> i32 {
    42
}

/// Middle function calling leaf.
pub fn chain_middle() -> i32 {
    chain_leaf() + 1
}

/// Root function calling middle.
pub fn chain_root() -> i32 {
    chain_middle() * 2
}
