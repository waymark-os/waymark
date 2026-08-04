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

    def test_result_diagnoses_child_handle_records(self) -> None:
        payload = {
            "ok": True,
            "value": {"children": [{"attempt": "attempt-child"}]},
        }
        with self.assertRaisesRegex(
            AssertionError,
            r"children must be list\[str\].*append child\.attempt directly",
        ):
            canary.assert_result(payload)

    def test_process_result_requires_measured_middle_winner(self) -> None:
        fixture = canary.compression_fixture()
        lzma_size = len(canary.lzma.compress(fixture, preset=9))
        gzip_size = len(
            canary.gzip.compress(fixture, compresslevel=9, mtime=0)
        )
        children = ["attempt-missing", "attempt-lzma", "attempt-gzip"]
        payload = {
            "ok": True,
            "value": {
                "answer": "lzma",
                "winner": children[1],
                "score": lzma_size,
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
                        "score": 1_000_000_000,
                    },
                    {
                        "selected": True,
                        "best_attempt": children[1],
                        "discarded_attempt": children[0],
                        "score": lzma_size,
                    },
                    {
                        "selected": False,
                        "best_attempt": children[1],
                        "discarded_attempt": children[2],
                        "score": gzip_size,
                    },
                ],
                "clean": True,
            },
        }
        self.assertEqual(
            canary.assert_result(
                payload,
                case="process-compression",
                fixture=fixture,
            ),
            (children[1], children),
        )


if __name__ == "__main__":
    unittest.main()
