class AppConfig:
    """AppConfig type declaration."""
    timeout_seconds: int = 30
    max_retries: int = 3

def use_config(cfg: AppConfig) -> int:
    """Non-call reference: type annotation."""
    return cfg.timeout_seconds

def zero_caller_symbol() -> bool:
    """Symbol with genuinely zero callers."""
    return True

@staticmethod
def decorated_function() -> int:
    """Annotated/decorated declaration."""
    return 100

# Non-ASCII characters before the symbol name, on its own declaration line
# (TS-A2). Python's grammar has no inline block comment and cannot chain a
# compound statement (`def`) after a `;`, so a `def`-based function can never
# have non-ASCII bytes on the same row before its own name -- the row always
# starts clean with (optionally indented) `def `. A semicolon-joined prior
# simple statement is the only idiomatic way to place same-row content before
# a target identifier in Python, which makes the target a plain module-level
# assignment (a `Constant` symbol) rather than a `def`-based `Function`.
_non_ascii_greeting = "🦀 café 🌍"; non_ascii_hello = "hello"
