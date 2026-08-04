#!/usr/bin/env python3
"""Source and admission checks for the visible standard Stone agent control."""

from __future__ import annotations

import json
import re
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "examples/scripts/standard_attempt_agent.stone"
WAYMARK = ROOT / "target/debug/waymark"


class StandardAttemptAgentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = SOURCE.read_text(encoding="utf-8")

    def test_control_and_adapters_remain_visible_stone(self) -> None:
        self.assertIn("def standard_agent_control(", self.source)
        self.assertIn("dispatch_action,", self.source)
        self.assertIn("verify_finish,", self.source)
        self.assertIn("record_progress,", self.source)
        self.assertIn("outcome = dispatch_action(action, options)", self.source)
        self.assertIn(
            "candidate = verify_finish(final, session, state)", self.source
        )
        self.assertIn(
            "critique = standard_completion_critique(", self.source
        )
        self.assertIn(
            "critique = standard_budget_checkpoint(", self.source
        )
        self.assertIn('"completion_critique_repair"', self.source)
        self.assertIn("standard_needs_review(", self.source)
        self.assertNotIn("react_control(", self.source)

    def test_control_bounds_decisions_observations_and_progress(self) -> None:
        self.assertEqual(self.source.count("model_infer("), 2)
        self.assertNotIn("model_call(", self.source)
        self.assertNotIn("json_loads(", self.source)
        self.assertIn('"maxItems": 1', self.source)
        self.assertIn("standard_bounded_messages(", self.source)
        self.assertIn("def standard_prepare_action_context(", self.source)
        self.assertIn("projection = context_project(", self.source)
        self.assertIn('"max_action_context_characters": 32768', self.source)
        self.assertIn('"max_task_context_characters": 6144', self.source)
        self.assertIn('"max_observation_characters": 4096', self.source)
        self.assertIn('"max_observation_field_characters": 1536', self.source)
        self.assertIn('"action_memory_projection_tokens": 512', self.source)
        self.assertIn('"initial_action_memory_required_keys": []', self.source)
        self.assertIn('"action_memory_required_keys": []', self.source)
        self.assertIn("decision_hooks=None", self.source)
        self.assertIn(
            "def standard_initial_action_memory_policy(session, options)",
            self.source,
        )
        self.assertIn('"source": "attempt_admission"', self.source)
        self.assertIn('source = "task_input_compatibility"', self.source)
        self.assertIn('source = "controller_default"', self.source)
        self.assertIn('"initial_action_memory_policy_source":', self.source)
        self.assertIn(
            "def standard_prepare_action_context(",
            self.source,
        )
        self.assertIn("required_keys=required_keys", self.source)
        self.assertIn("initial_projection = state.memory_projections == 0", self.source)
        self.assertIn(
            "else state.action_memory_required_keys",
            self.source,
        )
        self.assertIn('"initial_action_memory_projection_keys":', self.source)
        self.assertIn("messages = prepared.history", self.source)
        self.assertIn(
            "standard_messages_characters(prepared)",
            self.source,
        )
        self.assertIn("def standard_observation_message(", self.source)
        self.assertIn("...<bounded-middle-omitted>...", self.source)
        self.assertIn("...<observation-middle-omitted>...", self.source)
        self.assertIn('"context_messages_dropped":', self.source)
        self.assertIn('"observation_truncations":', self.source)
        self.assertIn('"peak_action_context_characters":', self.source)
        self.assertIn(
            "schema_prompt=standard_schema_prompt_text()",
            self.source,
        )
        self.assertIn("hooks=decision_hooks", self.source)
        self.assertIn(
            '"decision_hooks": "first_class_transition_hooks"',
            self.source,
        )
        prompt = re.search(
            r'^def standard_schema_prompt_text\(\):\n    return "(.*)"$',
            self.source,
            flags=re.MULTILINE,
        )
        self.assertIsNotNone(prompt)
        self.assertLess(len(prompt.group(1)), 1024)
        critique_prompt = re.search(
            r'^def standard_critique_schema_prompt_text\(\):\n    return "(.*)"$',
            self.source,
            flags=re.MULTILINE,
        )
        self.assertIsNotNone(critique_prompt)
        self.assertLess(len(critique_prompt.group(1)), 1024)
        self.assertIn("read_file(args.path, max_bytes=", self.source)
        self.assertIn('"tool": {"const": "edit"}', self.source)
        self.assertIn("result = edit(", self.source)
        self.assertIn('"replacements": result.replacements', self.source)
        self.assertIn("max_stdout_bytes=min(", self.source)
        self.assertIn("max_stderr_bytes=min(", self.source)
        self.assertIn("def standard_terminate_and_reap(", self.source)
        self.assertIn("cleanup = run_terminate(run_id)", self.source)
        self.assertIn("cleanup = run_wait(", self.source)
        self.assertIn("except Exception as error:", self.source)
        self.assertIn(
            "termination_attempts < options.run_cleanup_wait_attempts",
            self.source,
        )
        self.assertIn('"run_cleanup_wait_ms": 5000', self.source)
        self.assertIn('"run_cleanup_wait_attempts": 6', self.source)
        self.assertIn('"run_cleanup_pending"', self.source)
        self.assertIn('"tool": {"const": "run_start"}', self.source)
        self.assertIn('"tool": {"const": "run_complete"}', self.source)
        self.assertIn('"tool": {"const": "run_status"}', self.source)
        self.assertIn('"tool": {"const": "run_wait"}', self.source)
        self.assertIn('"tool": {"const": "run_terminate"}', self.source)
        self.assertIn("background=True", self.source)
        self.assertIn('"run_wait_ms": 30000', self.source)
        self.assertIn('"run_complete_timeout_ms": 900000', self.source)
        self.assertIn('result = run_complete(', self.source)
        self.assertIn('"run_completion_timeout"', self.source)
        self.assertIn('"run_complete_calls":', self.source)
        self.assertIn('"runtime_wait_observations":', self.source)
        self.assertIn('"max_active_runs": 4', self.source)
        self.assertIn('"progress.active_runs"', self.source)
        self.assertIn("def standard_reconcile_active_runs(", self.source)
        self.assertIn("inspection = attempt_inspect(", self.source)
        self.assertIn("for run_id in inspection.active_runs:", self.source)
        self.assertIn(
            "state = standard_reconcile_active_runs(state, options)",
            self.source,
        )
        self.assertIn('"run_reconciliations":', self.source)
        self.assertIn('"recovered_active_runs":', self.source)
        self.assertIn('"run_reconciliation_over_capacity":', self.source)
        self.assertIn('"reason": "active_owned_runs"', self.source)
        self.assertIn("standard_track_owned_run(", self.source)
        self.assertIn("standard_reap_active_runs(state, options)", self.source)
        self.assertIn('"progress.agent_control"', self.source)
        self.assertIn('"requirement.task"', self.source)
        self.assertIn('"requirement.audit"', self.source)
        self.assertIn('"evidence.action." + str(slot)', self.source)
        self.assertIn(
            "slot = (state.tool_calls - 1) % options.max_evidence_items",
            self.source,
        )
        self.assertIn('"completion_critique": True', self.source)
        self.assertIn('"max_rounds": 0', self.source)
        self.assertIn('"max_actions": 0', self.source)
        self.assertIn('"max_model_calls": 0', self.source)
        self.assertIn(
            "while options.max_rounds == 0 or round < options.max_rounds:",
            self.source,
        )
        self.assertIn(
            "if options.max_actions > 0 and state.actions > options.max_actions:",
            self.source,
        )
        self.assertIn('"max_completion_critiques": 2', self.source)
        self.assertIn('"max_stagnant_actions": 3', self.source)
        self.assertIn('"finalization_window": 6', self.source)
        self.assertIn('"proactive_completion_checkpoint": True', self.source)
        self.assertIn('"max_evidence_items": 8', self.source)
        self.assertIn('else "contradicted"', self.source)
        self.assertNotIn('else "rejected"', self.source)
        self.assertIn(
            "state.model_calls + 1 + reserved_calls > options.max_model_calls",
            self.source,
        )
        self.assertIn('"name": "stone.standard_action_v14"', self.source)
        self.assertIn("def standard_current_time_budget():", self.source)
        self.assertIn("def standard_control_frame(", self.source)
        self.assertIn('"kind": "runtime_control"', self.source)
        self.assertIn('"time_budget_finalize"', self.source)
        self.assertIn('"time_budget": get(outcome, "time_budget", None)', self.source)
        self.assertIn('outcome["time_budget"] = standard_current_time_budget()', self.source)
        self.assertIn('"stage": "budget_checkpoint"', self.source)
        self.assertIn('"budget_checkpoint_repair"', self.source)
        self.assertIn(
            "state.model_calls >= checkpoint_at",
            self.source,
        )
        self.assertIn(
            "state.model_calls + 4 <= options.max_model_calls",
            self.source,
        )
        self.assertIn('"stage": "blocked"', self.source)
        self.assertIn('"status": "pending"', self.source)
        self.assertNotIn('"status": "blocked"', self.source)
        self.assertIn('"report_partial_on_limit": False', self.source)
        self.assertIn('"completion": limit_reason', self.source)
        self.assertIn(
            '"result": standard_model_outcome(outcome, options)',
            self.source,
        )
        self.assertIn("standard_action_outcome_signature(", self.source)
        self.assertIn('"repeated_unchanged_action_state"', self.source)
        self.assertIn('"completion": "needs_review"', self.source)
        self.assertEqual(self.source.count('"kind": "runtime_control"'), 1)
        self.assertNotIn('"kind": "time_budget_warning"', self.source)
        self.assertNotIn('"kind": "budget_completion_checkpoint"', self.source)
        self.assertNotIn('"kind": "completion_critique"', self.source)
        self.assertNotIn('"instruction": "Choose exactly one next action.', self.source)

    def test_checked_in_program_is_admitted_before_gateway_context(self) -> None:
        if not WAYMARK.is_file():
            self.skipTest(f"Waymark binary not built: {WAYMARK}")
        completed = subprocess.run(
            [str(WAYMARK), "eval", str(SOURCE)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        response_text = completed.stdout or completed.stderr
        payload = json.loads(response_text)
        self.assertFalse(payload["ok"])
        self.assertEqual(
            payload["error"]["detail"],
            "Gateway task context is not active in this Stone runtime",
        )
        self.assertNotIn("stone_parse_error", response_text)
        self.assertNotIn("stone_admission_error", response_text)

    def test_progress_reducer_executes_as_ordinary_stone(self) -> None:
        if not WAYMARK.is_file():
            self.skipTest(f"Waymark binary not built: {WAYMARK}")
        library, marker, _ = self.source.partition("\nsession = agent_session()")
        self.assertTrue(marker)
        probe = r'''
state = {
    "control_mode": "explore",
    "control_objective": "test objective",
    "control_blocker": None,
    "control_reason": "initial",
    "control_updates": 0,
    "progress_class": "unknown",
    "unchanged_streak": 0,
    "last_action_outcome_signature": "",
}
action = {"tool": "read", "input": {"path": "sample.txt"}}
outcome = {"ok": True, "kind": "read", "path": "sample.txt", "content": "READY\n"}
state = standard_update_progress(action, outcome, state)
first = standard_control_frame(state, None)
state = standard_update_progress(action, outcome, state)
state = standard_update_progress(action, outcome, state)
state = standard_update_progress(action, outcome, state)
emit({
    "first": first,
    "repeated": standard_control_frame(state, None),
})
'''
        with tempfile.TemporaryDirectory() as directory:
            program = Path(directory) / "probe.stone"
            program.write_text(library + probe, encoding="utf-8")
            completed = subprocess.run(
                [str(WAYMARK), "eval", str(program)],
                cwd=directory,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
        payload = json.loads(completed.stdout or completed.stderr)
        self.assertTrue(payload["ok"], payload)
        value = payload["value"]
        self.assertEqual(value["first"]["mode"], "exploit")
        self.assertEqual(value["first"]["progress"], "better")
        self.assertEqual(value["repeated"]["mode"], "explore")
        self.assertEqual(value["repeated"]["progress"], "unchanged")
        self.assertEqual(value["repeated"]["unchanged_streak"], 3)


if __name__ == "__main__":
    unittest.main()
