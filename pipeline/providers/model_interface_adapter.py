"""
Adapter: pipeline/providers/model_interface_adapter.py
=======================================================

Wraps any ``ModelProvider`` (Person 6's abstraction) so it satisfies
``narrative_sequencer.ModelInterface`` (Person 4's abstraction).

Usage — replace ``EchoStubModel`` in narrative_sequencer at wiring time:

    from pipeline.providers import get_provider
    from pipeline.providers.model_interface_adapter import ModelProviderAdapter

    model = ModelProviderAdapter(get_provider())
    stops = build_tour(commits, model=model)

Neither ``narrative_sequencer.py`` nor any provider file needs to be changed.
The ``max_tokens`` and ``**kwargs`` arguments accepted by ``call_model`` are
silently ignored: the Bob Shell and Gemini providers do not expose a
token-budget parameter through the shared ``generate()`` interface, and adding
one would require coordinated changes across all providers. If token-budget
control is needed in the future, add it to ``ModelProvider.generate()`` first.
"""

from __future__ import annotations

import sys
import os

# ---------------------------------------------------------------------------
# Import ModelInterface from narrative_sequencer.
# narrative_sequencer.py lives at the repository root, one level above the
# pipeline/ package.  We resolve that path at import time so this adapter
# works regardless of the working directory the caller uses.
# ---------------------------------------------------------------------------
_repo_root = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", ".."))
if _repo_root not in sys.path:
    sys.path.insert(0, _repo_root)

from narrative_sequencer import ModelInterface  # noqa: E402

from .base import ModelProvider  # noqa: E402


class ModelProviderAdapter(ModelInterface):
    """
    Bridges ``ModelProvider.generate()`` to ``ModelInterface.call_model()``.

    Parameters
    ----------
    provider:
        Any concrete ``ModelProvider`` (``GeminiProvider``, ``BobProvider``,
        or a test double).
    """

    def __init__(self, provider: ModelProvider) -> None:
        self._provider = provider

    def call_model(self, prompt: str, *, max_tokens: int = 500, **kwargs) -> str:
        """
        Delegate to ``provider.generate()``.

        ``max_tokens`` and any extra ``kwargs`` are accepted but not forwarded
        — see module docstring for rationale.
        """
        return self._provider.generate(prompt)
