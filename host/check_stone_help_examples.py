#!/usr/bin/env python3
"""Check Stone help coverage and execute help examples."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_WAYMARK_BIN = ROOT / "target" / "debug" / "waymark"
STONE_EVAL = ROOT / "crates" / "waymark-runtime" / "src" / "stone_eval.rs"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Verify every Stone builtin has help and every help example runs."
    )
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        default=DEFAULT_WAYMARK_BIN,
        help="waymark binary to run. Default: %(default)s",
    )
    parser.add_argument(
        "--keep-going",
        action="store_true",
        help="Report all failures instead of stopping after collecting them.",
    )
    return parser.parse_args()


def builtin_names() -> set[str]:
    source = STONE_EVAL.read_text()
    match = re.search(
        r"const STONE_BUILTIN_NAMES: &\[&str\] = &\[(.*?)\];",
        source,
        flags=re.DOTALL,
    )
    if match is None:
        raise RuntimeError(f"could not find STONE_BUILTIN_NAMES in {STONE_EVAL}")
    names = set(re.findall(r'"([A-Za-z_][A-Za-z0-9_]*)"', match.group(1)))
    names.update({"get", "keys", "values", "items"})
    return names


def run_eval(waymark_bin: Path, source: str, cwd: Path) -> tuple[int, dict[str, Any] | None, str, str]:
    proc = subprocess.run(
        [str(waymark_bin), "eval", "-c", source],
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    payload = parse_waymark_payload(proc.stdout) or parse_waymark_payload(proc.stderr)
    return proc.returncode, payload, proc.stdout, proc.stderr


def parse_waymark_payload(text: str) -> dict[str, Any] | None:
    for line in reversed(text.splitlines()):
        line = line.strip()
        if not line:
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and "ok" in value:
            return value
    return None


def help_value(waymark_bin: Path, source: str, cwd: Path) -> Any:
    code, payload, stdout, stderr = run_eval(waymark_bin, source, cwd)
    if code != 0 or not payload or payload.get("ok") is not True:
        raise RuntimeError(
            f"help query failed\nsource:\n{source}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        )
    return payload["value"]


def prepare_fixture(root: Path) -> Path:
    app = root / "app"
    app.mkdir()
    (app / "subdir").mkdir()
    (app / "out").mkdir()
    (app / "input.txt").write_text("alpha\nERROR beta\n", encoding="utf-8")
    (app / "report.txt").write_text("ok\n", encoding="utf-8")
    (app / "config.txt").write_text("debug=false\n", encoding="utf-8")
    (app / "expected.txt").write_text("same\n", encoding="utf-8")
    (app / "actual.txt").write_text("same\n", encoding="utf-8")
    (app / "tmp.txt").write_text("remove me\n", encoding="utf-8")
    (app / "input.csv").write_text(
        "name,qty,amount,region,status,score,size,count\n"
        "ada,2,3.5,west,404,10,2048,2\n"
        "grace,1,1.5,east,200,7,512,1\n",
        encoding="utf-8",
    )
    (app / "events.jsonl").write_text(
        '{"name":"ada","qty":"2","amount":3.5,"region":"west","status":404,"score":10,"size":2048,"count":2}\n'
        '{"name":"grace","qty":"1","amount":1.5,"region":"east","status":200,"score":7,"size":512,"count":1}\n',
        encoding="utf-8",
    )
    (app / "config.json").write_text('{"ok":true,"name":"ada"}\n', encoding="utf-8")
    return app


def rewrite_logical_paths(source: str, app: Path) -> str:
    app_text = str(app)
    rewrites = {
        '"/app/': f'"{app_text}/',
        "'/app/": f"'{app_text}/",
        '"/app"': f'"{app_text}"',
        "'/app'": f"'{app_text}'",
    }
    for before, after in rewrites.items():
        source = source.replace(before, after)
    return source


def example_prelude(app: Path, example: str) -> str:
    app_literal = json.dumps(str(app))
    lines = [
        f"cd({app_literal})",
        'rows = [{"name": "ada", "qty": 2, "amount": 3.5, "region": "west", "status": 404, "score": 10, "size": 2048, "count": 2}, {"name": "grace", "qty": 1, "amount": 1.5, "region": "east", "status": 200, "score": 7, "size": 512, "count": 1}]',
        "row = rows[0]",
        f'files = find({app_literal}, "*.jsonl")',
        f'lines = read_file({app_literal} + "/input.txt").splitlines()',
        'line = "ERROR alpha,7"',
        'name = "ada"',
        "count = 2",
        "total = 7",
        'fields = ["ada", "2"]',
        'counts = {"ada": 2, "grace": 1}',
        'text = "{\\"ok\\": true}"',
        'seen = set()',
        'user = "ada"',
        "a = 1",
        "b = 2",
        "c = 3",
        'names = ["grace", "ada"]',
        'result = {"ok": True, "exit_code": 0, "stderr": "", "explanation": {}}',
    ]
    if "daemon_status(daemon" in example or "stop_daemon(daemon" in example:
        lines.append('daemon = start_daemon(["sh", "-c", "sleep 2"], stderr="server.err")')
    return "\n".join(lines) + "\n"


def check_help_coverage(waymark_bin: Path, cwd: Path) -> tuple[list[str], list[dict[str, Any]]]:
    overview = help_value(waymark_bin, "emit(help())", cwd)
    entries = overview.get("builtins", [])
    documented: set[str] = set()
    detailed_entries: list[dict[str, Any]] = []
    failures: list[str] = []

    for entry in entries:
        name = entry.get("name")
        if not isinstance(name, str):
            continue
        detail = help_value(waymark_bin, f"emit(help({json.dumps(name)}))", cwd)
        detailed_entries.append(detail)
        if detail.get("found") is not True:
            failures.append(f"help({name!r}) did not return found=true")
            continue
        documented.add(name)
        for alias in detail.get("aliases", []):
            if isinstance(alias, str):
                documented.add(alias)
        examples = detail.get("examples")
        if not isinstance(examples, list) or not examples:
            failures.append(f"help({name!r}) has no examples")

    missing = sorted(builtin_names() - documented)
    if missing:
        failures.append(f"builtins missing from help entries or aliases: {', '.join(missing)}")

    return failures, detailed_entries


def check_examples(waymark_bin: Path, entries: list[dict[str, Any]]) -> list[str]:
    failures: list[str] = []
    for entry in entries:
        name = entry["name"]
        use_when = entry.get("use_when")
        if isinstance(use_when, str) and "Gateway mode" in use_when:
            # Gateway transaction helpers require runtime config that is not
            # active in ordinary `waymark eval`; coverage is checked above.
            continue
        for index, example in enumerate(entry.get("examples", [])):
            if not isinstance(example, str) or not example.strip():
                failures.append(f"help({name!r}) example {index} is empty or non-string")
                continue
            with tempfile.TemporaryDirectory(prefix=f"stone-help-{name}-") as temp:
                root = Path(temp)
                app = prepare_fixture(root)
                source = example_prelude(app, example)
                source += rewrite_logical_paths(example, app)
                source += "\n"
                code, payload, stdout, stderr = run_eval(waymark_bin, source, root)
                ok = payload is not None and payload.get("ok") is True
                if name == "fail":
                    ok = payload is not None and payload.get("ok") is False
                if code != 0 and name != "fail":
                    ok = False
                if not ok:
                    failures.append(
                        "\n".join(
                            [
                                f"help({name!r}) example {index} failed",
                                "example:",
                                example,
                                "expanded source:",
                                source,
                                f"exit: {code}",
                                "stdout:",
                                stdout,
                                "stderr:",
                                stderr,
                            ]
                        )
                    )
    return failures


def main() -> int:
    args = parse_args()
    if not args.waymark_bin.exists():
        print(f"waymark binary not found: {args.waymark_bin}", file=sys.stderr)
        print("build it first with: cargo build -p waymark", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory(prefix="stone-help-check-") as temp:
        root = Path(temp)
        prepare_fixture(root)
        coverage_failures, entries = check_help_coverage(args.waymark_bin, root)
        example_failures = check_examples(args.waymark_bin, entries)

    failures = coverage_failures + example_failures
    if failures:
        for failure in failures if args.keep_going else failures[:1]:
            print(failure, file=sys.stderr)
            print("", file=sys.stderr)
        if not args.keep_going and len(failures) > 1:
            print(f"{len(failures) - 1} more failure(s); rerun with --keep-going", file=sys.stderr)
        return 1

    print(f"checked {len(entries)} help entries and their examples")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
