<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Helpers

Helpers are dynamic Stone extension registrations. They let projects add
diagnostic behavior without rebuilding Waymark Shell.

## Search Path

By default helpers are loaded from:

1. `<current Stone cwd>/.stone/helpers`
2. `~/.stone/helpers`
3. `/usr/share/waymark/stone/helpers`

Set `WAYMARK_STONE_HELPER_DIRS` to an OS path list to replace that order.

## Registration

A helper file registers hooks with `hook(...)` lines:

```python
def python_after_failure(event):
    return {
        "helper": "python.after_failure",
        "event": event["event"],
        "family": event["family"],
        "kind": "python_failure",
        "summary": event["stderr"],
        "evidence": {},
        "next_checks": [["python3", "-m", "pip", "check"]],
    }

hook("run.after_failure", family="python", argv0_prefix=["python"], handler="python.after_failure", priority=100)
```

Supported fields:

- `event`: lifecycle event, currently centered on `run.after_success`,
  `run.after_failure`, and `run.after_timeout`.
- `family`: helper family name such as `python`, `native`, `media`, `ml`,
  `llvm`, `service`, or `build`.
- `handler`: internal handler name.
- `priority`: higher values run first.
- `argv0`: optional exact command-name filter. Helper registration builds the
  command-family lookup table from these filters.
- `argv0_prefix`: optional command-name prefix filter.

`handler` is resolved during registration. If the helper file defines a Stone
function with the same name, or with `.` replaced by `_`, Waymark invokes that
function with one `event` record when the hook matches. The callback may return
one observation record, a list of observation records, or `None`. Built-in
handler names remain available for checked-in helpers.

The checked-in examples live under [.stone/helpers](../.stone/helpers).

## Testing A Helper

Create `.stone/helpers/example.stone` in a workspace:

```python
def example_after_failure(event):
    return {
        "helper": "example.after_failure",
        "event": event["event"],
        "family": event["family"],
        "kind": "example_failure",
        "summary": event["stderr"],
        "evidence": {"argv": event["argv"]},
        "next_checks": [],
    }

hook("run.after_failure", family="python", argv0_prefix=["python"], handler="example.after_failure", priority=100)
```

Then run a matching command:

```sh
WAYMARK_START_DIR="$PWD" waymark eval -c 'result = run(["python3", "-c", "import missing_module"]); emit(result)'
```

If the helper matched, the emitted `run` record contains a `helpers` list.
