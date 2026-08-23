//! Data types, decorators, non-ASCII declarations, zero-caller symbol.

/// External configuration struct whose impl block is elsewhere.
pub struct ExternalConfig {
    pub timeout_seconds: u32,
    pub max_retries: u32,
}

/// Function with non-call reference (type annotation on parameter).
pub fn take_config(_cfg: &ExternalConfig) -> u32 {
    _cfg.timeout_seconds
}

/// Symbol with genuinely zero callers across the codebase.
pub fn zero_caller_symbol() -> bool {
    true
}

/// Annotated / decorated declaration.
#[inline]
#[must_use]
pub fn decorated_function() -> u32 {
    100
}

/// Non-ASCII characters before the symbol name, on its own declaration line (TS-A2).
pub fn /* 🦀 café 🌍 */ non_ascii_hello() -> &'static str {
    "hello"
}
