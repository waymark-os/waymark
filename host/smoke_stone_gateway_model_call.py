#!/usr/bin/env python3
"""Smoke test Stone model_call through a fixture-backed Waymark Gateway."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SANDBOX = ROOT.parent


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        default=ROOT / "target" / "debug" / "waymark",
    )
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway" / "target" / "debug" / "waymark-gateway",
    )
    parser.add_argument(
        "--script",
        type=Path,
        default=ROOT / "examples" / "scripts" / "model_two_turn.stone",
    )
    return parser.parse_args()


def run(command: list[str], *, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        command,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"command failed: {command}\nstdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return completed


def wait_for_socket(path: Path, process: subprocess.Popen[str]) -> None:
    deadline = time.time() + 10
    while time.time() < deadline:
        if process.poll() is not None:
            stderr = process.stderr.read() if process.stderr else ""
            raise RuntimeError(f"Gateway exited with {process.returncode}: {stderr}")
        if path.exists():
            return
        time.sleep(0.05)
    raise TimeoutError(f"Gateway socket did not appear: {path}")


def main() -> int:
    args = parse_args()
    waymark_bin = args.waymark_bin.resolve()
    gateway_bin = args.gateway_bin.resolve()
    script = args.script.resolve()
    for path in (waymark_bin, gateway_bin, script):
        if not path.exists():
            raise SystemExit(f"missing required path: {path}")

    fixture_text = "structured observations reduce parsing ambiguity"
    with tempfile.TemporaryDirectory(prefix="waymark-stone-model-call-") as temp:
        root = Path(temp)
        data_root = root / "gateway-data"
        source = root / "source"
        work = root / "work"
        socket_path = root / "gateway.sock"
        source.mkdir()
        work.mkdir()
        (source / "README.md").write_text("fixture workspace\n", encoding="utf-8")

        run(
            [
                str(gateway_bin),
                "--data-root",
                str(data_root),
                "repo",
                "snapshot",
                "--name",
                "repo",
                "--path",
                str(source),
            ]
        )
        tx = run(
            [
                str(gateway_bin),
                "--data-root",
                str(data_root),
                "env",
                "snapshot",
                "--workspace",
                "repo",
            ]
        ).stdout.strip()

        gateway_env = dict(os.environ)
        gateway_env.update(
            {
                "WAYMARK_MODEL_PROVIDER": "fixture",
                "WAYMARK_MODEL_FIXTURE_TEXT": fixture_text,
            }
        )
        server = subprocess.Popen(
            [
                str(gateway_bin),
                "--data-root",
                str(data_root),
                "rpc",
                "serve",
                "--socket",
                str(socket_path),
            ],
            env=gateway_env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            wait_for_socket(socket_path, server)
            waymark_env = dict(os.environ)
            waymark_env.update(
                {
                    "WAYMARK_START_DIR": str(work),
                    "WAYMARK_GATEWAY_SOCKET": str(socket_path),
                    "WAYMARK_GATEWAY_TX": tx,
                    "WAYMARK_GATEWAY_IMAGE": "python:3.12",
                    "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
                    "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
                }
            )
            completed = run([str(waymark_bin), "eval", str(script)], env=waymark_env)
            response = json.loads(completed.stdout)
            if response.get("ok") is not True:
                raise AssertionError(f"Stone model_call failed: {completed.stdout}")
            value = response.get("value", {})
            if value.get("first") != fixture_text or value.get("second") != fixture_text:
                raise AssertionError(f"unexpected two-turn result: {completed.stdout}")
            if value.get("model") != "agent":
                raise AssertionError(f"Gateway did not resolve the requested model class: {completed.stdout}")
            usage = value.get("usage")
            if not isinstance(usage, dict) or usage.get("total_tokens", 0) <= 0:
                raise AssertionError(f"missing model usage: {completed.stdout}")
            if "api_key" in completed.stdout.lower():
                raise AssertionError("model result exposed credential-shaped data")
            print("Stone Gateway model_call two-turn smoke passed")
        finally:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
