//! Concrete implementation in a DIFFERENT file (mod.rs vs lawyer_impl.rs shape).

use crate::service::Greeter;

/// Concrete implementation of Greeter.
pub struct GreeterService {
    pub greeting_prefix: String,
}

impl Greeter for GreeterService {
    fn greet(&self, name: &str) -> String {
        format!("{}, {}!", self.greeting_prefix, name)
    }
}
