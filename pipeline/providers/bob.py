import os

from .base import ModelProvider


class BobProvider(ModelProvider):

    def __init__(self, api_key: str | None = None):
        self.api_key = api_key or os.getenv("BOB_API_KEY")

        if not self.api_key:
            raise ValueError("BOB_API_KEY is not configured")

    def generate(self, prompt: str) -> str:
        # Bob implementation will go here
        raise NotImplementedError