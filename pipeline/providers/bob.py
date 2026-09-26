import os
import shutil
import subprocess

from .base import ModelProvider


class BobProvider(ModelProvider):
    """
    ModelProvider implementation that delegates to Bob Shell (bob CLI) in
    non-interactive mode.

    Authentication uses the BOB_API_KEY environment variable together with
    ``--auth-method api-key``, as documented at:
    https://bob.ibm.com/docs/shell/getting-started/install-and-setup#api-key-authentication

    ``--hide-intermediary-output`` is passed so that only the final answer is
    written to stdout; this avoids the need for any delimiter-based parsing.

    The license must have been accepted once before (``bob --accept-license``)
    on the machine running this code.
    """

    def __init__(self, api_key: str | None = None, bob_path: str | None = None):
        self.api_key = api_key or os.getenv("BOB_API_KEY")

        if not self.api_key:
            raise ValueError("BOB_API_KEY is not configured")

        # Allow explicit override (useful in tests); fall back to PATH lookup.
        self.bob_path = bob_path or shutil.which("bob") or "bob"

    def generate(self, prompt: str) -> str:
        """
        Run ``bob --auth-method api-key --hide-intermediary-output -p <prompt>``
        and return the trimmed stdout.

        Raises
        ------
        RuntimeError
            If the bob binary is not found on PATH, the process exits with a
            non-zero code, or the call times out.
        """
        bob_path = self.bob_path

        # Verify the binary exists before trying to run it so we get a clear
        # error rather than a cryptic FileNotFoundError from subprocess.
        if not shutil.which(bob_path) and not os.path.isfile(bob_path):
            raise RuntimeError(
                f"Bob Shell binary not found: '{bob_path}'. "
                "Install Bob Shell and make sure it is on PATH. "
                "See https://bob.ibm.com/docs/shell/getting-started/install-and-setup"
            )

        cmd = [
            bob_path,
            "--auth-method", "api-key",
            "--hide-intermediary-output",
            "-p", prompt,
        ]

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                env={**os.environ, "BOB_API_KEY": self.api_key},
                timeout=120,
            )
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError("Bob Shell timed out after 120 seconds") from exc

        if result.returncode != 0:
            stderr_snippet = (result.stderr or "").strip()[:300]
            raise RuntimeError(
                f"Bob Shell exited with code {result.returncode}. "
                f"stderr: {stderr_snippet}"
            )

        return result.stdout.strip()
