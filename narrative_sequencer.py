"""
Person 4 — Narrative Sequencer & Generation
=============================================

Responsibilities implemented here:
  1. Order selected commits/PRs into a tour (chronological default, or
     clustered by subsystem).
  2. Generate a per-stop narrative: what changed / what prompted it / what a
     new hire should take away.
  3. Enforce a hard no-verbatim-reproduction rule on any PR discussion text
     that gets paraphrased into a narrative.
  4. Call the model ONLY through the shared abstraction interface (owned by
     Person 6) — never directly.
  5. Guarantee every stop links back to its source commit/PR.
  6. Emit the finished tour as JSON for Person 5 to consume.

This file has zero knowledge of how commits were selected (Person 1/2) or
how the tour is rendered (Person 5) — it only knows the input/output
contracts below, so it can be developed and tested in isolation.
"""

from __future__ import annotations

import json
import re
from abc import ABC, abstractmethod
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from typing import Iterable, Literal, Optional


# ---------------------------------------------------------------------------
# 1. Model abstraction interface (contract with Person 6)
# ---------------------------------------------------------------------------
# Person 6 owns the real implementation (rate limiting, retries, provider
# choice, prompt logging, etc). Person 4 and Person 3 both code against this
# interface only, so the underlying model call is swappable without touching
# sequencing/generation logic.

class ModelInterface(ABC):
    """Abstraction boundary. Do not call any model SDK directly outside this."""

    @abstractmethod
    def call_model(self, prompt: str, *, max_tokens: int = 500, **kwargs) -> str:
        """Send `prompt` to the underlying model and return the text response."""
        raise NotImplementedError


class EchoStubModel(ModelInterface):
    """
    A trivial stand-in so this module runs/tests before Person 6's real
    interface lands. Replace with the real implementation at wiring time —
    nothing else in this file changes.
    """

    def call_model(self, prompt: str, *, max_tokens: int = 500, **kwargs) -> str:
        return (
            "[STUB OUTPUT — replace EchoStubModel with Person 6's real "
            "ModelInterface implementation]\n"
            f"(prompt was {len(prompt)} chars)"
        )


# ---------------------------------------------------------------------------
# 2. Input data contracts
# ---------------------------------------------------------------------------

@dataclass
class PRDiscussion:
    """One PR's discussion thread, as handed off by the upstream stage."""
    pr_number: int
    pr_url: str
    excerpts: list[str] = field(default_factory=list)  # raw discussion text, source material only — never emitted verbatim


@dataclass
class SelectedCommit:
    """A single commit selected upstream for inclusion in the tour."""
    commit_hash: str
    commit_url: str
    message: str
    author: str
    timestamp: str  # ISO 8601
    subsystem: str  # e.g. "auth", "billing", "infra" — set by upstream tagging
    files_changed: list[str] = field(default_factory=list)
    diff_summary: str = ""  # short structured summary of the diff, NOT the raw diff
    pr: Optional[PRDiscussion] = None


# ---------------------------------------------------------------------------
# 3. Output data contract (what Person 5 consumes)
# ---------------------------------------------------------------------------

@dataclass
class TourStop:
    order: int
    commit_hash: str
    commit_url: str
    pr_number: Optional[int]
    pr_url: Optional[str]
    subsystem: str
    timestamp: str
    what_changed: str
    what_prompted_it: str
    new_hire_takeaway: str

    def to_dict(self) -> dict:
        return asdict(self)


# ---------------------------------------------------------------------------
# 4. Ordering
# ---------------------------------------------------------------------------

OrderMode = Literal["chronological", "subsystem_cluster"]


def order_commits(
    commits: Iterable[SelectedCommit],
    mode: OrderMode = "chronological",
    subsystem_order: Optional[list[str]] = None,
) -> list[SelectedCommit]:
    """
    Order the selected commits into tour sequence.

    - "chronological" (default): earliest first.
    - "subsystem_cluster": group by subsystem (in `subsystem_order` if given,
      else first-seen order), chronological within each cluster.
    """
    commits = list(commits)

    if mode == "chronological":
        return sorted(commits, key=lambda c: c.timestamp)

    if mode == "subsystem_cluster":
        if subsystem_order is None:
            seen = []
            for c in commits:
                if c.subsystem not in seen:
                    seen.append(c.subsystem)
            subsystem_order = seen

        rank = {name: i for i, name in enumerate(subsystem_order)}
        return sorted(
            commits,
            key=lambda c: (rank.get(c.subsystem, len(rank)), c.timestamp),
        )

    raise ValueError(f"Unknown order mode: {mode}")


# ---------------------------------------------------------------------------
# 5. Verbatim-reproduction guard (copyright requirement)
# ---------------------------------------------------------------------------
# PR discussion excerpts are source material for paraphrasing only. This
# guard catches cases where a generated narrative accidentally reproduces a
# long run of the original text, so a violation is caught deterministically
# rather than trusted to the model's instruction-following alone.

def _normalize(text: str) -> list[str]:
    return re.findall(r"[a-z0-9']+", text.lower())


def longest_shared_ngram(generated: str, source: str) -> int:
    """Return the length (in words) of the longest run of words shared
    verbatim between `generated` and `source`."""
    g = _normalize(generated)
    s = _normalize(source)
    if not g or not s:
        return 0

    s_index: dict[str, list[int]] = {}
    for i, w in enumerate(s):
        s_index.setdefault(w, []).append(i)

    best = 0
    for i in range(len(g)):
        for j in s_index.get(g[i], []):
            k = 0
            while i + k < len(g) and j + k < len(s) and g[i + k] == s[j + k]:
                k += 1
            best = max(best, k)
    return best


MAX_ALLOWED_SHARED_RUN = 6  # words — beyond this, treat as verbatim reproduction


def violates_verbatim_rule(generated_text: str, source_excerpts: list[str]) -> bool:
    return any(
        longest_shared_ngram(generated_text, src) > MAX_ALLOWED_SHARED_RUN
        for src in source_excerpts
    )


# ---------------------------------------------------------------------------
# 6. Narrative generation
# ---------------------------------------------------------------------------

_NARRATIVE_PROMPT_TEMPLATE = """You are writing one stop of a codebase "narrative tour" for new engineering hires.

Given the commit and PR context below, produce THREE short sections:
1. WHAT CHANGED — a plain-language summary of the change itself.
2. WHAT PROMPTED IT — why this change happened, based on the PR discussion.
3. TAKEAWAY — one or two sentences a new hire should remember from this.

CRITICAL RULES:
- Paraphrase everything. Never quote PR discussion text verbatim, not even short phrases.
- Do not copy more than a few consecutive words from the source material.
- Keep each section to 2-4 sentences.
- Output exactly in this format, with no extra commentary:
WHAT_CHANGED: ...
WHAT_PROMPTED_IT: ...
TAKEAWAY: ...

--- COMMIT ---
Message: {message}
Files changed: {files}
Diff summary: {diff_summary}

--- PR DISCUSSION (paraphrase only, do not quote) ---
{discussion}
"""


def _build_prompt(commit: SelectedCommit) -> str:
    discussion = "\n".join(commit.pr.excerpts) if commit.pr else "(no linked PR discussion)"
    return _NARRATIVE_PROMPT_TEMPLATE.format(
        message=commit.message,
        files=", ".join(commit.files_changed) or "(not specified)",
        diff_summary=commit.diff_summary or "(not specified)",
        discussion=discussion,
    )


def _parse_sections(raw: str) -> dict[str, str]:
    sections = {"WHAT_CHANGED": "", "WHAT_PROMPTED_IT": "", "TAKEAWAY": ""}
    pattern = re.compile(
        r"WHAT_CHANGED:\s*(?P<changed>.*?)\s*"
        r"WHAT_PROMPTED_IT:\s*(?P<prompted>.*?)\s*"
        r"TAKEAWAY:\s*(?P<takeaway>.*)",
        re.DOTALL,
    )
    m = pattern.search(raw)
    if m:
        sections["WHAT_CHANGED"] = m.group("changed").strip()
        sections["WHAT_PROMPTED_IT"] = m.group("prompted").strip()
        sections["TAKEAWAY"] = m.group("takeaway").strip()
    else:
        # Model didn't follow format — fall back to putting everything in
        # WHAT_CHANGED so nothing silently disappears; caller can flag for review.
        sections["WHAT_CHANGED"] = raw.strip()
    return sections


class NarrativeGenerator:
    def __init__(self, model: ModelInterface, max_retries: int = 2):
        self.model = model
        self.max_retries = max_retries

    def generate_stop_narrative(self, commit: SelectedCommit) -> dict[str, str]:
        source_excerpts = commit.pr.excerpts if commit.pr else []
        prompt = _build_prompt(commit)

        last_sections = None
        for attempt in range(self.max_retries + 1):
            raw = self.model.call_model(prompt)
            sections = _parse_sections(raw)
            combined = " ".join(sections.values())

            if not violates_verbatim_rule(combined, source_excerpts):
                return sections

            last_sections = sections
            # Escalate the instruction on retry
            prompt = (
                "Your previous answer copied wording too closely from the source. "
                "Rewrite fully in your own words, changing sentence structure, "
                "with no run of more than a few words matching the source text.\n\n"
                + prompt
            )

        # Exhausted retries: strip anything that still overlaps rather than
        # ship a verbatim fragment.
        return self._sanitize(last_sections, source_excerpts)

    @staticmethod
    def _sanitize(sections: dict[str, str], source_excerpts: list[str]) -> dict[str, str]:
        safe = {}
        for key, text in sections.items():
            if violates_verbatim_rule(text, source_excerpts):
                safe[key] = "[content withheld: could not paraphrase below the verbatim-overlap threshold — flag for manual review]"
            else:
                safe[key] = text
        return safe


# ---------------------------------------------------------------------------
# 7. Tour assembly
# ---------------------------------------------------------------------------

def build_tour(
    commits: Iterable[SelectedCommit],
    model: ModelInterface,
    mode: OrderMode = "chronological",
    subsystem_order: Optional[list[str]] = None,
) -> list[TourStop]:
    ordered = order_commits(commits, mode=mode, subsystem_order=subsystem_order)
    generator = NarrativeGenerator(model)

    stops: list[TourStop] = []
    for i, commit in enumerate(ordered, start=1):
        sections = generator.generate_stop_narrative(commit)
        stops.append(
            TourStop(
                order=i,
                commit_hash=commit.commit_hash,
                commit_url=commit.commit_url,
                pr_number=commit.pr.pr_number if commit.pr else None,
                pr_url=commit.pr.pr_url if commit.pr else None,
                subsystem=commit.subsystem,
                timestamp=commit.timestamp,
                what_changed=sections["WHAT_CHANGED"],
                what_prompted_it=sections["WHAT_PROMPTED_IT"],
                new_hire_takeaway=sections["TAKEAWAY"],
            )
        )
    return stops


def tour_to_json(stops: list[TourStop], mode: OrderMode) -> str:
    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "order_mode": mode,
        "stop_count": len(stops),
        "stops": [s.to_dict() for s in stops],
    }
    return json.dumps(payload, indent=2)


# ---------------------------------------------------------------------------
# 8. Example / smoke test (uses the stub model — swap for the real one)
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    sample_commits = [
        SelectedCommit(
            commit_hash="a1b2c3d",
            commit_url="https://github.com/death7654/GitLens/commit/a1b2c3d",
            message="Switch session tokens to rotating refresh tokens",
            author="alice",
            timestamp="2025-03-01T10:00:00",
            subsystem="auth",
            files_changed=["auth/session.py", "auth/tokens.py"],
            diff_summary="Replaced long-lived JWTs with short-lived access tokens plus a refresh flow.",
            pr=PRDiscussion(
                pr_number=142,
                pr_url="https://github.com/death7654/GitLens/pull/142",
                excerpts=[
                    "we kept seeing tokens leak through logs because they lived for 30 days",
                    "rotating refresh tokens cut our exposure window down to about 15 minutes",
                ],
            ),
        ),
        SelectedCommit(
            commit_hash="e4f5g6h",
            commit_url="https://github.com/death7654/GitLens/commit/e4f5g6h",
            message="Add idempotency keys to billing webhook handler",
            author="bob",
            timestamp="2025-02-15T09:00:00",
            subsystem="billing",
            files_changed=["billing/webhooks.py"],
            diff_summary="Webhook handler now dedupes on a client-supplied idempotency key.",
            pr=PRDiscussion(
                pr_number=98,
                pr_url="https://github.com/death7654/GitLens/pull/98",
                excerpts=["stripe retries the same webhook multiple times and we were double-charging"],
            ),
        ),
    ]

    stops = build_tour(sample_commits, model=EchoStubModel(), mode="chronological")
    print(tour_to_json(stops, mode="chronological"))
