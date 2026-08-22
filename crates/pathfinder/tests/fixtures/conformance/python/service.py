from abc import ABC, abstractmethod

class Greeter(ABC):
    """Greeter abstract interface."""

    @abstractmethod
    def greet(self, name: str) -> str:
        pass
