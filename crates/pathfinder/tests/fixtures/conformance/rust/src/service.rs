//! Trait declaration in a standalone file.

/// Greeter trait interface.
pub trait Greeter {
    /// Greet by name.
    fn greet(&self, name: &str) -> String;
}
