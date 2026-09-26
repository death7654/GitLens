import os

from .base import ModelProvider
from .gemini import GeminiProvider
from .bob import BobProvider


def get_provider() -> ModelProvider:
    provider = os.getenv("MODEL_PROVIDER", "gemini").lower()

    if provider == "gemini":
        return GeminiProvider()

    if provider == "bob":
        return BobProvider()

    raise ValueError(f"Unsupported model provider: {provider}")