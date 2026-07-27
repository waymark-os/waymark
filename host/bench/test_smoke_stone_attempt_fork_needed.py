#!/usr/bin/env python3

from __future__ import annotations

import unittest
from pathlib import Path

import smoke_stone_attempt_fork_needed as canary


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "examples/scripts/attempt_fork_needed_canary.stone"


class ForkNeededCanaryTests(unittest.TestCase):
    def test_example_requires_the_parent_frontier_and_isolated_branches(self) -> None:
        source = SOURCE.read_text(encoding="utf-8")

        self.assertIn('write_file("problem.txt", input.problem + "\\n")', source)
        self.assertIn('"requirement.fork_target"', source)
        self.assertEqual(source.count("attempt_fork("), 2)
        self.assertIn("attempt_wait_all(scope", source)
        self.assertIn("attempt_accept(root.attempt, winner.attempt)", source)
        self.assertIn("attempt_discard(loser.attempt", source)
        self.assertNotIn("attempt_spawn(", source)

    def test_result_gate_accepts_one_winner_and_one_loser(self) -> None:
        payload = {
            "ok": True,
            "value": {
                "answer": canary.EXPECTED,
                "winner": "attempt-winner",
                "loser": "attempt-loser",
                "accepted": "attempt-winner",
                "clean": True,
                "parent_keys": ["requirement.fork_target"],
                "results": [
                    {
                        "fork_memory_revision": 1,
                        "result": {
                            "candidate": canary.CANDIDATES[0],
                            "passed": False,
                            "problem": canary.PROBLEM,
                            "inherited_target": canary.EXPECTED,
                            "memory_keys": [
                                "candidate.result",
                                "requirement.fork_target",
                            ],
                        },
                    },
                    {
                        "fork_memory_revision": 1,
                        "result": {
                            "candidate": canary.EXPECTED,
                            "passed": True,
                            "problem": canary.PROBLEM,
                            "inherited_target": canary.EXPECTED,
                            "memory_keys": [
                                "candidate.result",
                                "requirement.fork_target",
                            ],
                        },
                    },
                ],
            },
        }

        self.assertEqual(
            canary.assert_result(payload),
            ("attempt-winner", "attempt-loser"),
        )


if __name__ == "__main__":
    unittest.main()
