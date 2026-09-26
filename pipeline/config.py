import os

from dotenv import load_dotenv

load_dotenv()

MODEL_PROVIDER = os.getenv("MODEL_PROVIDER", "gemini")
GEMINI_API_KEY = os.getenv("GEMINI_API_KEY")
BOB_API_KEY = os.getenv("BOB_API_KEY")