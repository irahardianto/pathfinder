from types import AppConfig

class ConfigWrapper:
    """Wraps AppConfig declared in types.py."""

    def __init__(self, config: AppConfig):
        self.config = config

    def get_timeout(self) -> int:
        return self.config.timeout_seconds
