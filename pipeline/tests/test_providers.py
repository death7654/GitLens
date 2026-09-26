"""
Unit tests for pipeline/providers/.

All tests are fully mocked — no live subprocess is spawned and no network
call is made.  Run with:

    cd pipeline
    python -m pytest tests/test_providers.py -v
"""
import subprocess
import unittest
from unittest.mock import MagicMock, call, patch


# ---------------------------------------------------------------------------
# Helpers shared by multiple test classes
# ---------------------------------------------------------------------------

def _make_completed_process(stdout="hello", stderr="", returncode=0):
    proc = MagicMock(spec=subprocess.CompletedProcess)
    proc.stdout = stdout
    proc.stderr = stderr
    proc.returncode = returncode
    return proc


# ---------------------------------------------------------------------------
# BobProvider tests
# ---------------------------------------------------------------------------

class TestBobProviderInit(unittest.TestCase):
    """Construction and configuration validation."""

    def test_raises_when_no_api_key(self):
        """ValueError if BOB_API_KEY is absent and no explicit key given."""
        with patch.dict("os.environ", {}, clear=True):
            from providers.bob import BobProvider  # noqa: PLC0415
            with self.assertRaises(ValueError, msg="BOB_API_KEY is not configured"):
                BobProvider()

    def test_accepts_explicit_api_key(self):
        """Constructor succeeds when an explicit api_key argument is supplied."""
        with patch("shutil.which", return_value="/usr/bin/bob"):
            from providers.bob import BobProvider
            provider = BobProvider(api_key="explicit-key")
            self.assertEqual(provider.api_key, "explicit-key")

    def test_reads_api_key_from_env(self):
        """Constructor reads BOB_API_KEY from the environment."""
        with patch.dict("os.environ", {"BOB_API_KEY": "env-key"}):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                from providers.bob import BobProvider
                provider = BobProvider()
                self.assertEqual(provider.api_key, "env-key")

    def test_explicit_key_takes_precedence_over_env(self):
        """Explicit api_key argument overrides the environment variable."""
        with patch.dict("os.environ", {"BOB_API_KEY": "env-key"}):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                from providers.bob import BobProvider
                provider = BobProvider(api_key="explicit-key")
                self.assertEqual(provider.api_key, "explicit-key")

    def test_custom_bob_path_stored(self):
        """bob_path override is stored on the instance."""
        with patch("os.path.isfile", return_value=True):
            from providers.bob import BobProvider
            provider = BobProvider(api_key="k", bob_path="/custom/bob")
            self.assertEqual(provider.bob_path, "/custom/bob")


class TestBobProviderGenerate(unittest.TestCase):
    """generate() behaviour — subprocess interactions."""

    def _make_provider(self, bob_path="/usr/bin/bob"):
        with patch("shutil.which", return_value=bob_path):
            from providers.bob import BobProvider
            return BobProvider(api_key="test-key", bob_path=bob_path)

    # --- happy path ---

    def test_returns_stripped_stdout(self):
        """generate() returns stdout with leading/trailing whitespace stripped."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="  response text  \n")
        with patch("subprocess.run", return_value=proc) as mock_run:
            with patch("shutil.which", return_value="/usr/bin/bob"):
                result = provider.generate("hello")
        self.assertEqual(result, "response text")

    def test_correct_command_invocation(self):
        """generate() passes exactly the documented flags to subprocess.run."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="ok")
        with patch("subprocess.run", return_value=proc) as mock_run:
            with patch("shutil.which", return_value="/usr/bin/bob"):
                provider.generate("my prompt")

        args, kwargs = mock_run.call_args
        cmd = args[0]
        self.assertIn("--auth-method", cmd)
        auth_idx = cmd.index("--auth-method")
        self.assertEqual(cmd[auth_idx + 1], "api-key")
        self.assertIn("--hide-intermediary-output", cmd)
        self.assertIn("-p", cmd)
        p_idx = cmd.index("-p")
        self.assertEqual(cmd[p_idx + 1], "my prompt")

    def test_api_key_passed_in_env(self):
        """BOB_API_KEY is injected into the subprocess environment."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="ok")
        with patch("subprocess.run", return_value=proc) as mock_run:
            with patch("shutil.which", return_value="/usr/bin/bob"):
                provider.generate("test")

        _, kwargs = mock_run.call_args
        self.assertIn("BOB_API_KEY", kwargs["env"])
        self.assertEqual(kwargs["env"]["BOB_API_KEY"], "test-key")

    def test_bob_path_used_as_first_cmd_element(self):
        """The explicitly supplied bob_path is the first element of the command."""
        with patch("os.path.isfile", return_value=True):
            from providers.bob import BobProvider
            provider = BobProvider(api_key="k", bob_path="/opt/bob/bin/bob")

        proc = _make_completed_process(stdout="ok")
        with patch("subprocess.run", return_value=proc) as mock_run:
            with patch("shutil.which", return_value=None):
                with patch("os.path.isfile", return_value=True):
                    provider.generate("x")

        args, _ = mock_run.call_args
        self.assertEqual(args[0][0], "/opt/bob/bin/bob")

    def test_timeout_passed_to_subprocess(self):
        """subprocess.run receives a numeric timeout argument."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="ok")
        with patch("subprocess.run", return_value=proc) as mock_run:
            with patch("shutil.which", return_value="/usr/bin/bob"):
                provider.generate("x")

        _, kwargs = mock_run.call_args
        self.assertIn("timeout", kwargs)
        self.assertIsInstance(kwargs["timeout"], (int, float))

    def test_capture_output_and_text_mode(self):
        """subprocess.run is called with capture_output=True and text=True."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="ok")
        with patch("subprocess.run", return_value=proc) as mock_run:
            with patch("shutil.which", return_value="/usr/bin/bob"):
                provider.generate("x")

        _, kwargs = mock_run.call_args
        self.assertTrue(kwargs.get("capture_output"))
        self.assertTrue(kwargs.get("text"))

    # --- error paths ---

    def test_missing_cli_raises_runtime_error(self):
        """RuntimeError with install guidance when the bob binary is not found."""
        from providers.bob import BobProvider
        provider = BobProvider.__new__(BobProvider)
        provider.api_key = "k"
        provider.bob_path = "bob-does-not-exist"

        with patch("shutil.which", return_value=None):
            with patch("os.path.isfile", return_value=False):
                with self.assertRaises(RuntimeError) as ctx:
                    provider.generate("hello")

        self.assertIn("bob-does-not-exist", str(ctx.exception))
        self.assertIn("install", str(ctx.exception).lower())

    def test_nonzero_exit_raises_runtime_error(self):
        """RuntimeError when the bob process exits with non-zero returncode."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="", stderr="auth failed", returncode=1)
        with patch("subprocess.run", return_value=proc):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                with self.assertRaises(RuntimeError) as ctx:
                    provider.generate("x")

        self.assertIn("1", str(ctx.exception))

    def test_nonzero_exit_includes_stderr_in_error(self):
        """RuntimeError message contains the stderr output for diagnostics."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="", stderr="authentication error", returncode=2)
        with patch("subprocess.run", return_value=proc):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                with self.assertRaises(RuntimeError) as ctx:
                    provider.generate("x")

        self.assertIn("authentication error", str(ctx.exception))

    def test_timeout_raises_runtime_error(self):
        """RuntimeError (not TimeoutExpired) when the subprocess times out."""
        provider = self._make_provider()
        with patch("subprocess.run", side_effect=subprocess.TimeoutExpired(cmd="bob", timeout=120)):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                with self.assertRaises(RuntimeError) as ctx:
                    provider.generate("x")

        self.assertIn("timed out", str(ctx.exception).lower())

    def test_empty_stdout_returns_empty_string(self):
        """generate() returns an empty string when stdout is empty (not an error)."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="   ", returncode=0)
        with patch("subprocess.run", return_value=proc):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                result = provider.generate("x")

        self.assertEqual(result, "")

    def test_multiline_response_preserved(self):
        """Multi-line stdout is returned intact (only boundary whitespace stripped)."""
        provider = self._make_provider()
        proc = _make_completed_process(stdout="line1\nline2\nline3", returncode=0)
        with patch("subprocess.run", return_value=proc):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                result = provider.generate("x")

        self.assertEqual(result, "line1\nline2\nline3")


# ---------------------------------------------------------------------------
# get_provider() factory tests
# ---------------------------------------------------------------------------

class TestGetProvider(unittest.TestCase):
    """Factory function selects the correct provider from MODEL_PROVIDER."""

    def test_defaults_to_gemini(self):
        """MODEL_PROVIDER unset → GeminiProvider is returned."""
        with patch.dict("os.environ", {"GEMINI_API_KEY": "gk"}, clear=True):
            from providers import get_provider
            from providers.gemini import GeminiProvider
            with patch.object(GeminiProvider, "__init__", return_value=None):
                provider = get_provider()
            self.assertIsInstance(provider, GeminiProvider)

    def test_gemini_explicit(self):
        """MODEL_PROVIDER=gemini → GeminiProvider is returned."""
        with patch.dict("os.environ", {"MODEL_PROVIDER": "gemini", "GEMINI_API_KEY": "gk"}):
            from providers import get_provider
            from providers.gemini import GeminiProvider
            with patch.object(GeminiProvider, "__init__", return_value=None):
                provider = get_provider()
            self.assertIsInstance(provider, GeminiProvider)

    def test_bob_provider_selected(self):
        """MODEL_PROVIDER=bob → BobProvider is returned."""
        with patch.dict("os.environ", {"MODEL_PROVIDER": "bob", "BOB_API_KEY": "bk"}):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                from providers import get_provider
                from providers.bob import BobProvider
                provider = get_provider()
            self.assertIsInstance(provider, BobProvider)

    def test_unknown_provider_raises(self):
        """Unknown MODEL_PROVIDER value raises ValueError."""
        with patch.dict("os.environ", {"MODEL_PROVIDER": "nonexistent"}):
            from providers import get_provider
            with self.assertRaises(ValueError):
                get_provider()

    def test_case_insensitive_provider_name(self):
        """MODEL_PROVIDER value is lowercased before comparison."""
        with patch.dict("os.environ", {"MODEL_PROVIDER": "BOB", "BOB_API_KEY": "bk"}):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                from providers import get_provider
                from providers.bob import BobProvider
                provider = get_provider()
            self.assertIsInstance(provider, BobProvider)


# ---------------------------------------------------------------------------
# GeminiProvider tests (existing, preserved)
# ---------------------------------------------------------------------------

class TestGeminiProviderInit(unittest.TestCase):
    """Construction and configuration of GeminiProvider."""

    def test_raises_when_no_api_key(self):
        with patch.dict("os.environ", {}, clear=True):
            from providers.gemini import GeminiProvider
            with self.assertRaises(ValueError):
                GeminiProvider()

    def test_accepts_explicit_api_key(self):
        mock_client_cls = MagicMock()
        with patch("google.genai.Client", mock_client_cls):
            from providers.gemini import GeminiProvider
            provider = GeminiProvider(api_key="test-key")
            self.assertEqual(provider.api_key, "test-key")

    def test_reads_api_key_from_env(self):
        mock_client_cls = MagicMock()
        with patch.dict("os.environ", {"GEMINI_API_KEY": "env-key"}):
            with patch("google.genai.Client", mock_client_cls):
                from providers.gemini import GeminiProvider
                provider = GeminiProvider()
                self.assertEqual(provider.api_key, "env-key")


class TestGeminiProviderGenerate(unittest.TestCase):
    """generate() behaviour for GeminiProvider."""

    def _make_provider(self):
        mock_client_cls = MagicMock()
        with patch("google.genai.Client", mock_client_cls):
            from providers.gemini import GeminiProvider
            return GeminiProvider(api_key="k")

    def test_returns_response_text(self):
        provider = self._make_provider()
        mock_response = MagicMock()
        mock_response.text = "gemini answer"
        provider.client.models.generate_content.return_value = mock_response
        result = provider.generate("prompt")
        self.assertEqual(result, "gemini answer")

    def test_raises_on_non_503_error(self):
        provider = self._make_provider()
        provider.client.models.generate_content.side_effect = RuntimeError("quota exceeded")
        with self.assertRaises(RuntimeError):
            provider.generate("prompt")

    def test_retries_on_503(self):
        provider = self._make_provider()
        mock_response = MagicMock()
        mock_response.text = "ok after retry"
        provider.client.models.generate_content.side_effect = [
            RuntimeError("503 UNAVAILABLE"),
            mock_response,
        ]
        with patch("time.sleep"):
            result = provider.generate("prompt")
        self.assertEqual(result, "ok after retry")
        self.assertEqual(provider.client.models.generate_content.call_count, 2)

    def test_raises_after_max_retries_on_503(self):
        provider = self._make_provider()
        provider.client.models.generate_content.side_effect = RuntimeError("503 UNAVAILABLE")
        with patch("time.sleep"):
            with self.assertRaises(RuntimeError):
                provider.generate("prompt")
        self.assertEqual(provider.client.models.generate_content.call_count, 3)


# ---------------------------------------------------------------------------
# ModelProviderAdapter tests
# ---------------------------------------------------------------------------

class TestModelProviderAdapter(unittest.TestCase):
    """Adapter that bridges ModelProvider → narrative_sequencer.ModelInterface."""

    def _make_adapter(self, response: str = "adapter response"):
        """Return an adapter wrapping a simple stub ModelProvider."""
        from providers.model_interface_adapter import ModelProviderAdapter
        from providers.base import ModelProvider

        class _StubProvider(ModelProvider):
            def generate(self, prompt: str) -> str:
                return response

        return ModelProviderAdapter(_StubProvider()), _StubProvider()

    def test_call_model_delegates_to_generate(self):
        """call_model() returns whatever the underlying provider.generate() returns."""
        adapter, _ = self._make_adapter("hello from provider")
        result = adapter.call_model("any prompt")
        self.assertEqual(result, "hello from provider")

    def test_prompt_forwarded_unchanged(self):
        """The exact prompt string is passed through to provider.generate()."""
        from providers.model_interface_adapter import ModelProviderAdapter
        from providers.base import ModelProvider

        received: list[str] = []

        class _CapturingProvider(ModelProvider):
            def generate(self, prompt: str) -> str:
                received.append(prompt)
                return "ok"

        adapter = ModelProviderAdapter(_CapturingProvider())
        adapter.call_model("my exact prompt")
        self.assertEqual(received, ["my exact prompt"])

    def test_max_tokens_accepted_but_not_required(self):
        """max_tokens kwarg is accepted without error and silently ignored."""
        adapter, _ = self._make_adapter("fine")
        result = adapter.call_model("p", max_tokens=100)
        self.assertEqual(result, "fine")

    def test_extra_kwargs_accepted_silently(self):
        """Arbitrary extra kwargs are accepted without error and ignored."""
        adapter, _ = self._make_adapter("fine")
        result = adapter.call_model("p", temperature=0.7, top_p=0.9)
        self.assertEqual(result, "fine")

    def test_satisfies_model_interface_abc(self):
        """ModelProviderAdapter is a concrete subclass of ModelInterface."""
        from providers.model_interface_adapter import ModelProviderAdapter
        import sys, os
        repo_root = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", ".."))
        if repo_root not in sys.path:
            sys.path.insert(0, repo_root)
        from narrative_sequencer import ModelInterface
        adapter, _ = self._make_adapter()
        self.assertIsInstance(adapter, ModelInterface)

    def test_adapter_wraps_bob_provider_stub(self):
        """Adapter correctly wraps a BobProvider stub (no real CLI invoked)."""
        from providers.model_interface_adapter import ModelProviderAdapter
        from providers.bob import BobProvider

        fake_provider = BobProvider.__new__(BobProvider)
        fake_provider.api_key = "k"
        fake_provider.bob_path = "/usr/bin/bob"

        proc = _make_completed_process(stdout="bob answer")
        with patch("subprocess.run", return_value=proc):
            with patch("shutil.which", return_value="/usr/bin/bob"):
                adapter = ModelProviderAdapter(fake_provider)
                result = adapter.call_model("prompt text")

        self.assertEqual(result, "bob answer")


if __name__ == "__main__":
    unittest.main()
