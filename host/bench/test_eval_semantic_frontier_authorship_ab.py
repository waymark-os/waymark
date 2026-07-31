#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = Path(__file__).with_name(
    "eval_semantic_frontier_authorship_ab.py"
)
SPEC = importlib.util.spec_from_file_location("semantic_frontier_ab", MODULE_PATH)
assert SPEC and SPEC.loader
EXPERIMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPERIMENT)


def help_topics() -> dict[str, dict[str, object]]:
    names = (
        "semantic_frontier",
        "attempt_branch",
        "attempt_fork",
        "attempt_spawn",
        "attempt_scope",
        "attempt_info",
        "attempt_join",
        "attempt_accept",
        "attempt_discard",
        "attempt_finish",
        "attempt_scope_close",
    )
    return {
        name: {
            "signature": f"{name}(...)",
            "use_when": "test",
            "avoid": [],
            "examples": [],
        }
        for name in names
    }


class SemanticFrontierAuthorshipTests(unittest.TestCase):
    def test_prompts_name_checkpoint_and_attempt_roles(self) -> None:
        raw = EXPERIMENT.arm_prompt("raw", help_topics())
        typed = EXPERIMENT.arm_prompt("typed", help_topics())
        for prompt in (raw, typed):
            self.assertIn("checkpoint = fixture_prepare_checkpoint()", prompt)
            self.assertIn("root = attempt_info()", prompt)
            self.assertIn("root is the current attempt record", prompt)
            self.assertIn('"attempt_info"', prompt)
        self.assertIn("workspace_source", raw)
        self.assertIn("Do not call attempt_fork", typed)

    def test_reference_sources_pass_only_their_own_structural_gate(self) -> None:
        raw_source = (
            ROOT
            / "examples/references/semantic_frontier_authorship_raw_reference.stone"
        ).read_text(encoding="utf-8")
        typed_source = (
            ROOT
            / "examples/references/semantic_frontier_authorship_typed_reference.stone"
        ).read_text(encoding="utf-8")
        raw = EXPERIMENT.source_features(raw_source)
        typed = EXPERIMENT.source_features(typed_source)
        self.assertTrue(EXPERIMENT.structural_gate("raw", raw)[0])
        self.assertTrue(EXPERIMENT.structural_gate("typed", typed)[0])
        self.assertFalse(EXPERIMENT.structural_gate("raw", typed)[0])
        self.assertFalse(EXPERIMENT.structural_gate("typed", raw)[0])
        self.assertEqual(raw["redundant_seal_requests"], 0)
        self.assertEqual(typed["redundant_seal_requests"], 0)

    def test_composition_dispatches_all_three_entrypoints(self) -> None:
        composed = EXPERIMENT.compose_source(
            "def main(input):\n    return input\n",
            "def worker(input):\n    return input\n"
            "def setup_owner(input):\n    return input\n",
        )
        self.assertIn('entrypoint == "worker"', composed)
        self.assertIn('entrypoint == "setup_owner"', composed)
        self.assertIn("result = main(task_input())", composed)

    def test_fixture_requires_handles_at_the_lifecycle_boundary(self) -> None:
        fixture = (
            ROOT
            / "examples/references/semantic_frontier_authorship_fixture.stone"
        ).read_text(encoding="utf-8")
        self.assertIn("parent_outcome: attempt_outcome", fixture)
        self.assertIn("repaired_outcome: attempt_outcome", fixture)
        self.assertIn("parent_child: attempt_handle", fixture)
        self.assertIn("owner: attempt_handle", fixture)
        self.assertIn("repaired: attempt_handle", fixture)


if __name__ == "__main__":
    unittest.main()
