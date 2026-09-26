import os
import time

from google import genai

from .base import ModelProvider


class GeminiProvider(ModelProvider):

    def __init__(self, api_key: str | None = None):
        self.api_key = api_key or os.getenv("GEMINI_API_KEY")

        if not self.api_key:
            raise ValueError("GEMINI_API_KEY is not configured")

        self.client = genai.Client(api_key=self.api_key)

    def generate(self, prompt: str) -> str:
        max_retries = 3

        for attempt in range(max_retries):
            try:
                response = self.client.models.generate_content(
                    model="gemini-3.8-flash",
                    contents=prompt,
                )

                return response.text

            except Exception as e:
                error_message = str(e)

                if "503" not in error_message and "UNAVAILABLE" not in error_message:
                    raise

                if attempt == max_retries - 1:
                    raise

                wait_time = 2 ** attempt

                print(
                    f"Gemini temporarily unavailable. "
                    f"Retrying in {wait_time} seconds..."
                )

                time.sleep(wait_time)

        raise RuntimeError("Gemini request failed after retries")