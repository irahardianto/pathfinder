from service import Greeter

class GreeterService(Greeter):
    """GreeterService implements Greeter in a different file."""

    def __init__(self, greeting_prefix: str):
        self.greeting_prefix = greeting_prefix

    def greet(self, name: str) -> str:
        return f"{self.greeting_prefix}, {name}!"
