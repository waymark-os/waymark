#!/usr/bin/env python3

import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("eval_stone_stage_syntax_ab.py")
SPEC = importlib.util.spec_from_file_location("stage_syntax_ab", MODULE_PATH)
assert SPEC and SPEC.loader
EXPERIMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPERIMENT)


CORE_SOURCE = r'''
def ready(step):
    probe = run(["test", "-s", "artifact.txt"])
    return workflow_evidence(probe.ok, "artifact is non-empty", ["stat:artifact.txt"] if probe.ok else [])

def action(step):
    return run(["sh", "-c", "exit 7"])

def repair(step):
    return run(["sh", "-c", "printf ready > artifact.txt"])

artifact = workflow_stage("artifact", evidence=ready, action=action, repair=repair, max_attempts=1)
emit(workflow_run(workflow("build-artifact", artifact)))
'''

SYNTAX_SOURCE = r'''
def repair(step):
    return run(["sh", "-c", "printf ready > artifact.txt"])

@stage(evidence=file_nonempty("artifact.txt"), repair=repair, max_attempts=1)
def artifact(step):
    return run(["sh", "-c", "exit 7"])

emit(workflow_run(workflow("build-artifact", artifact)))
'''


class StageSyntaxExperimentTests(unittest.TestCase):
    def test_prompts_keep_task_fixed_and_separate_surfaces(self):
        topics = {
            name: {"signature": name, "use_when": "", "avoid": [], "examples": []}
            for name in (
                "workflow_evidence",
                "workflow_stage",
                "stage",
                "file_nonempty",
                "workflow",
                "workflow_run",
            )
        }
        core = EXPERIMENT.arm_prompt("core", topics)
        syntax = EXPERIMENT.arm_prompt("syntax", topics)
        self.assertIn("exit 7", core)
        self.assertIn("exit 7", syntax)
        self.assertIn("workflow_stage", core)
        self.assertNotIn("@stage or file_nonempty", syntax)
        self.assertIn("@stage(evidence=file_nonempty", syntax)

    def test_structural_gates_distinguish_the_arms(self):
        core = EXPERIMENT.source_features(CORE_SOURCE)
        syntax = EXPERIMENT.source_features(SYNTAX_SOURCE)
        self.assertTrue(EXPERIMENT.structural_gate("core", core)[0])
        self.assertTrue(EXPERIMENT.structural_gate("syntax", syntax)[0])
        self.assertFalse(EXPERIMENT.structural_gate("core", syntax)[0])
        self.assertFalse(EXPERIMENT.structural_gate("syntax", core)[0])
        self.assertLess(syntax["bytes"], core["bytes"])
        self.assertLess(syntax["function_defs"], core["function_defs"])

    def test_behavior_gate_accepts_both_equivalent_programs(self):
        waymark = EXPERIMENT.ROOT / "target/debug/waymark"
        if not waymark.is_file():
            self.skipTest("waymark binary is not built")
        with tempfile.TemporaryDirectory(prefix="stone-stage-ab-test-") as temp:
            root = Path(temp)
            for arm, source in (("core", CORE_SOURCE), ("syntax", SYNTAX_SOURCE)):
                run_dir = root / arm
                run_dir.mkdir()
                source_path = run_dir / "agent.stone"
                source_path.write_text(source, encoding="utf-8")
                result = EXPERIMENT.execute_source(waymark, source_path, run_dir)
                self.assertTrue(result["ok"], result)


if __name__ == "__main__":
    unittest.main()
