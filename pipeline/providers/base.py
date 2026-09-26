from abc import ABC, abstractmethod


class ModelProvider(ABC):

    @abstractmethod
    def generate(self, prompt: str) -> str:
        """Generate a response from the model."""
        pass