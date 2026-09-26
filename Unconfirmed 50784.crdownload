"""
Quick tests for the pieces that don't need a real model:
- ordering (chronological + subsystem cluster)
- the verbatim-reproduction guard (this is the copyright-safety net, so it
  needs to actually work, not just look plausible)
"""

from narrative_sequencer import (
    SelectedCommit,
    PRDiscussion,
    ModelInterface,
    NarrativeGenerator,
    order_commits,
    longest_shared_ngram,
    violates_verbatim_rule,
)


def make_commit(hash_, ts, subsystem, excerpts=None):
    return SelectedCommit(
        commit_hash=hash_,
        commit_url=f"https://github.com/x/y/commit/{hash_}",
        message="msg",
        author="a",
        timestamp=ts,
        subsystem=subsystem,
        pr=PRDiscussion(pr_number=1, pr_url="https://github.com/x/y/pull/1", excerpts=excerpts or []),
    )


def test_chronological_order():
    commits = [
        make_commit("c2", "2025-02-01T00:00:00", "auth"),
        make_commit("c1", "2025-01-01T00:00:00", "billing"),
        make_commit("c3", "2025-03-01T00:00:00", "auth"),
    ]
    ordered = order_commits(commits, mode="chronological")
    assert [c.commit_hash for c in ordered] == ["c1", "c2", "c3"]


def test_subsystem_cluster_order():
    commits = [
        make_commit("c1", "2025-01-01T00:00:00", "auth"),
        make_commit("c2", "2025-01-02T00:00:00", "billing"),
        make_commit("c3", "2025-01-03T00:00:00", "auth"),
    ]
    ordered = order_commits(commits, mode="subsystem_cluster", subsystem_order=["billing", "auth"])
    assert [c.commit_hash for c in ordered] == ["c2", "c1", "c3"]


def test_longest_shared_ngram_detects_verbatim_copy():
    source = "the quick brown fox jumps over the lazy dog every single morning"
    generated = "As the PR notes, the quick brown fox jumps over the lazy dog."
    assert longest_shared_ngram(generated, source) >= 9


def test_longest_shared_ngram_allows_paraphrase():
    source = "we kept seeing tokens leak through logs because they lived for 30 days"
    generated = "Long-lived tokens were showing up in logs, which was a security concern."
    assert longest_shared_ngram(generated, source) <= 3


def test_violates_verbatim_rule_flags_long_copy():
    source = ["stripe retries the same webhook multiple times and we were double-charging"]
    generated = "The team noticed stripe retries the same webhook multiple times and we were double-charging customers."
    assert violates_verbatim_rule(generated, source) is True


def test_violates_verbatim_rule_allows_clean_paraphrase():
    source = ["stripe retries the same webhook multiple times and we were double-charging"]
    generated = "Duplicate webhook deliveries from the payment provider were causing customers to be billed twice."
    assert violates_verbatim_rule(generated, source) is False


class _AlwaysVerbatimModel(ModelInterface):
    """A model that stubbornly copies the source, to test the retry/sanitize path."""

    def call_model(self, prompt: str, **kwargs) -> str:
        return (
            "WHAT_CHANGED: stripe retries the same webhook multiple times and we were double-charging\n"
            "WHAT_PROMPTED_IT: stripe retries the same webhook multiple times and we were double-charging\n"
            "TAKEAWAY: stripe retries the same webhook multiple times and we were double-charging"
        )


def test_generator_sanitizes_when_model_wont_paraphrase():
    commit = make_commit(
        "c1", "2025-01-01T00:00:00", "billing",
        excerpts=["stripe retries the same webhook multiple times and we were double-charging"],
    )
    gen = NarrativeGenerator(model=_AlwaysVerbatimModel(), max_retries=1)
    sections = gen.generate_stop_narrative(commit)
    for text in sections.values():
        assert "double-charging" not in text  # verbatim fragment must be stripped out


if __name__ == "__main__":
    import sys
    tests = [v for k, v in list(globals().items()) if k.startswith("test_")]
    failures = 0
    for t in tests:
        try:
            t()
            print(f"PASS {t.__name__}")
        except AssertionError as e:
            failures += 1
            print(f"FAIL {t.__name__}: {e}")
    sys.exit(1 if failures else 0)
