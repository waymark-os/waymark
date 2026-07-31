#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name(
    "eval_bounded_attempt_explore_authorship_ab.py"
)
SPEC = importlib.util.spec_from_file_location("bounded_explore_ab", MODULE_PATH)
assert SPEC and SPEC.loader
EXPERIMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPERIMENT)


LIBRARY_SOURCE = r'''
def main(input):
    checkpoint = fixture_prepare_checkpoint()
    result = explore(
        checkpoint=checkpoint,
        candidates=fixture_candidates(),
        worker_entrypoint="worker",
        evaluate=evaluate_candidate,
        propose=propose_lexical_first,
        max_candidates=2,
    )
    return fixture_finalize(result)
'''

EXPLICIT_SOURCE = r'''
def main(input):
    checkpoint = fixture_prepare_checkpoint()
    root = attempt_info()
    scope = attempt_scope()
    child = attempt_fork(checkpoint=checkpoint)
    outcome = attempt_join(child)
    attempt_discard(outcome)
    child = attempt_fork(checkpoint=checkpoint)
    outcome = attempt_join(child)
    attempt_accept(root.attempt, outcome)
    cleanup = attempt_scope_close(scope)
    return fixture_finalize({"clean": cleanup.clean})
'''


class BoundedExploreAuthorshipTests(unittest.TestCase):
    def test_prompts_hold_task_fixed_and_separate_control_surfaces(self) -> None:
        explicit = EXPERIMENT.arm_prompt("explicit")
        library = EXPERIMENT.arm_prompt("library")
        self.assertIn("fixture_prepare_checkpoint", explicit)
        self.assertIn("fixture_prepare_checkpoint", library)
        self.assertIn("attempt_fork", explicit)
        self.assertIn("Do not directly call", library)
        self.assertIn("explore(", library)

    def test_structural_gate_distinguishes_explicit_and_library_arms(self) -> None:
        library = EXPERIMENT.source_features(LIBRARY_SOURCE)
        explicit = EXPERIMENT.source_features(EXPLICIT_SOURCE)
        self.assertTrue(EXPERIMENT.structural_gate("library", library)[0])
        self.assertTrue(EXPERIMENT.structural_gate("explicit", explicit)[0])
        self.assertFalse(EXPERIMENT.structural_gate("explicit", library)[0])
        self.assertFalse(EXPERIMENT.structural_gate("library", explicit)[0])
        self.assertLess(library["bytes"], explicit["bytes"])

    def test_composition_places_library_only_in_treatment(self) -> None:
        library = EXPERIMENT.compose_source(
            "library",
            LIBRARY_SOURCE,
            "def explore():\n    pass\n",
            "def worker(input):\n    pass\n",
        )
        explicit = EXPERIMENT.compose_source(
            "explicit",
            EXPLICIT_SOURCE,
            "def explore():\n    pass\n",
            "def worker(input):\n    pass\n",
        )
        self.assertIn("def explore()", library)
        self.assertNotIn("def explore()", explicit)
        self.assertLess(
            library.index("def worker"),
            library.index("def main"),
        )
        self.assertIn('entrypoint == "worker"', library)


if __name__ == "__main__":
    unittest.main()
