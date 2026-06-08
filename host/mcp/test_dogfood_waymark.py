#!/usr/bin/env python3

from __future__ import annotations

import argparse
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import dogfood_waymark as dogfood


class DogfoodWaymarkTests(unittest.TestCase):
    def test_prompt_mentions_stone_first_and_persistent_bindings(self) -> None:
        prompt = dogfood.prompt_for_task("inspect docs")

        self.assertIn("Use the Stone MCP tools as the primary shell surface", prompt)
        self.assertIn("bindings persist across stone_eval calls", prompt)
        self.assertIn("allow_large_output=true", prompt)
        self.assertIn("Task:\ninspect docs", prompt)

    def test_runner_args_use_persistent_dogfood_paths(self) -> None:
        args = argparse.Namespace(
            workspace=dogfood.ROOT,
            codex="codex",
            codex_home=dogfood.DOGFOOD_ROOT / "codex-home",
            trace=dogfood.DOGFOOD_ROOT / "trace.jsonl",
            waymark_bin=dogfood.ROOT / "target" / "debug" / "waymark",
            timeout_seconds=5.0,
            model="gpt-5.5",
            reasoning_effort="medium",
            check=True,
            exec=False,
            json_events=True,
            codex_sandbox="workspace-write",
            task="improve waymark",
        )

        runner_args = dogfood.runner_args(args)

        self.assertEqual(runner_args.workspace, dogfood.ROOT.resolve())
        self.assertEqual(runner_args.codex_home, (dogfood.DOGFOOD_ROOT / "codex-home").resolve())
        self.assertEqual(runner_args.stone_cwd, str(dogfood.ROOT.resolve()))
        self.assertEqual(runner_args.codex_sandbox, "workspace-write")
        self.assertIn("improve waymark", runner_args.prompt)

    def test_seed_codex_auth_copies_only_auth_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            target = root / "target"
            source.mkdir()
            (source / "auth.json").write_text("{}")
            (source / "installation_id").write_text("id")
            (source / "config.toml").write_text("do not copy")

            copied = dogfood.seed_codex_auth(source, target)

            self.assertEqual(copied, ["auth.json", "installation_id"])
            self.assertEqual((target / "auth.json").read_text(), "{}")
            self.assertEqual((target / "installation_id").read_text(), "id")
            self.assertFalse((target / "config.toml").exists())


if __name__ == "__main__":
    unittest.main()
