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

# 🦀 café 🌍 greeting
def non_ascii_hello() -> str:
    """Non-ASCII comment before declaration."""
    return "hello"
