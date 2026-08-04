#!/usr/bin/env python3

from __future__ import annotations

import unittest
from pathlib import Path

import smoke_attempt_best as canary


ROOT = Path(__file__).resolve().parents[2]
PROGRAM = ROOT / "examples/references/attempt_best_canary.stone"


class AttemptBestCanaryTests(unittest.TestCase):
    def test_program_leaves_candidate_lifecycle_to_selector(self) -> None:
        source = PROGRAM.read_text(encoding="utf-8")
        self.assertIn('attempt_best(scope, objective="max")', source)
        self.assertIn("attempt_best_consider(", source)
        self.assertIn("attempt_best_accept(", source)
        self.assertIn("attempt_scope_close(", source)
        self.assertNotIn("attempt_discard(", source)

    def test_result_requires_middle_winner_and_bounded_cleanup(self) -> None:
        children = ["attempt-first", "attempt-winner", "attempt-last"]
        payload = {
            "ok": True,
            "value": {
                "answer": "beta",
                "winner": children[1],
                "score": 0.9,
                "status": "accepted",
                "considered": 3,
                "replacements": 1,
                "released_outcome": True,
                "children": children,
                "decisions": [
                    {
                        "selected": True,
                        "best_attempt": children[0],
                        "discarded_attempt": None,
                    },
                    {
                        "selected": True,
                        "best_attempt": children[1],
                        "discarded_attempt": children[0],
                    },
                    {
                        "selected": False,
                        "best_attempt": children[1],
                        "discarded_attempt": children[2],
                    },
                ],
                "clean": True,
            },
        }
        self.assertEqual(canary.assert_result(payload), (children[1], children))


if __name__ == "__main__":
    unittest.main()
