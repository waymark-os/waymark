# Stone Python-Lookalike Audit

Status: first deliverable of the "first guess is correct" guideline,
2026-11-09. Findings and proposals only; no semantics have changed.

## Rule Being Audited

For any Python-shaped feature:

1. Accept it if it means the same as in Python.
2. Reject it before execution, with a correction, if the meaning would differ.
3. Never accept the syntax and change the behaviour. A lookalike is allowed
   only if every operation where it differs from Python raises at that point.

## Method

`host/audit_stone_python_lookalikes.py` runs 136 small snippets in Stone
(`target/debug/waymark eval`) and in Python 3, and compares values, errors
and stdout. For every Stone rejection, a second run starts with
`write_text('marker.txt', 'x')` and checks whether the marker exists. That
shows whether the rejection happened before any statement ran or mid-run,
after earlier effects.

Verdicts:

| Verdict | Meaning |
|---|---|
| same | Same value or same error as Python |
| fix | Rejected with a message that names the correction |
| no fix | Rejected, but the message is terse or a raw parser dump |
| **SILENT** | Accepted with a different meaning: a bug under rule 3 |

"Mid-run" marks rejections that happen after earlier statements have already
run.

## Summary

| Area | Cases | same | fix | no fix | SILENT |
|---|---:|---:|---:|---:|---:|
| tuple (stored as list) | 20 | 4 | 1 | 4 | 11 |
| `set()` (unique list) | 31 | 4 | 1 | 15 | 10 (+1 order) |
| truthiness and equality | 22 | 6 | 1 | 7 | 8 |
| `with` | 10 | 6 | 0 | 2 | 2 |
| `except` / `try` | 20 | 4 | 8 | 4 | 4 |
| other lookalikes | 33 | 10 | 7 | 12 | 4 |
| **total** | **136** | **34** | **18** | **44** | **39** |

The two `except` probes whose Python baseline is not applicable (hook
denial, write outside the roots) are counted under "same" for `except`.

39 silent differences were found. The guarantee the guideline singles out
holds: no `except` form can catch a budget stop (see "Budget And Policy
Stops").

## Tuples

Tuple literals become lists.

| Id | Operation | Python | Stone | Verdict |
|---|---|---|---|---|
| T1 | `t[0] = 5` | TypeError | mutates `t` | **SILENT** |
| T2 | `t.append(3)` | AttributeError | appends | **SILENT** |
| T4 | `t.sort()` | AttributeError | sorts in place | **SILENT** |
| T13 | `u = t; t += (3,)`, then read `u` | `(1, 2)` | `[1, 2, 3]`: the alias changes | **SILENT** |
| T17 | `(1,) + [2]` | TypeError | `[1, 2]` | **SILENT** |
| T7 | `(1, 2) == [1, 2]` | False | True | **SILENT** |
| T8 | `isinstance((1, 2), list)` | False | True | **SILENT** |
| T10, T11, T19 | `str`, f-string, `repr` of a tuple | `(1, 2)`, `(1,)` | `[1, 2]`, `[1]` | **SILENT** |
| T12 | `print((1, 2))` | `(1, 2)` | `[1,2]` (JSON) | **SILENT** |
| T9 | `isinstance(x, tuple)` | True | error: "tuple values are lists, use list" | fix, mid-run |
| T5 | `d[(1, 2)] = 3` | works | "cannot assign record item with list<int> index" | no fix, mid-run |
| T6 | `{(1, 2): "a"}` | works | raw parser dump | no fix |
| T3 | `del t[0]` | TypeError | raw parser dump (`del` unsupported) | no fix |
| T20 | `hash(...)` | works | unknown function | no fix |
| T14, T15, T16, T18 | unpacking, membership, `(1,)`, `tuple(x)` copies | | | same |

## `set()` (a deterministic unique list)

| Id | Operation | Python | Stone | Verdict |
|---|---|---|---|---|
| S4 | `set([1, 2]) == set([2, 1])` | True | False (compared as ordered lists) | **SILENT** |
| S17 | `s[0]` | TypeError | first element | **SILENT** |
| S18 | `s.append(1)` | AttributeError | appends a duplicate (`len` becomes 2) | **SILENT** |
| S19 | `s + [1]` | TypeError | concatenates, duplicate kept | **SILENT** |
| S30 | `s.sort()` | AttributeError | sorts | **SILENT** |
| S21 | `isinstance(s, list)` | False | True | **SILENT** |
| S22, S23 | `set` of dicts or lists | TypeError (unhashable) | deduplicates | **SILENT** |
| S25 | `set([1, 1.0])` | one element | two elements | **SILENT** |
| S24 | `str(set([1]))` | `{1}` | `[1]` | **SILENT** |
| S3 | `list(set([3, 1, 2]))` | `[1, 2, 3]` (order unspecified) | `[3, 1, 2]` (first seen) | order: Python promises none |
| S1, S2 | `{1, 2}` literal, set comprehension | works | raw parser dump | no fix |
| S5–S8 | `\|`, `&`, `^` | set operations | "expected integer, got list<int>", mid-run | no fix |
| S7 | `-` | set difference | "cannot subtract list<int> and list<int>", mid-run | no fix |
| S9–S13, S15, S26 | `union`, `intersection`, `difference`, `update`, `remove`, `issubset`, `pop` | work | "unsupported method" | no fix |
| S14 | `s.discard(5)` | works | "expected attempt id or record": routed to `attempt_discard` | no fix, **misleading**, mid-run |
| S20 | `isinstance(s, set)` | True | "set values are lists, use list" | fix, mid-run |
| S27 | `frozenset` | works | unknown function | no fix |
| S16, S28, S29, S31 | `add` deduplicates, `<=`, `sorted`, aliasing | | | same |

## Truthiness And Equality

Truthiness itself matches Python (B2, B12, B13). The differences come from
booleans being a separate type that never equals an int, and from `is`.

| Id | Operation | Python | Stone | Verdict |
|---|---|---|---|---|
| B3 | `True == 1` | True | False | **SILENT** (documented) |
| B8 | `0 == False` | True | False | **SILENT** |
| B6, B7 | `True in [1]`, `1 in [True]` | True | False | **SILENT** |
| B10 | `[0, 1].count(False)` | 1 | 0 | **SILENT** |
| B19 | `set([True, 1])` | one element | two elements | **SILENT** |
| B16 | `[] is []` | False | True | **SILENT** (`is` compares values) |
| B18 | `1 is 1.0` | False | True | **SILENT** |
| B4, B5, B11, B20, B22 | `True + 1`, `sum([True, True])`, `max(True, 2)`, `sorted([True, 0])`, `int(True)` | work | typed errors, mid-run | no fix |
| B9 | `{1: "a"}[True]` | `"a"` | "cannot index record<1: string> with bool" | no fix, mid-run |
| B21 | `[1].index(True)` | 0 | "value is not in list" | no fix, mid-run |
| B1 | `bool(x)` | works | unknown function | fix (generic "use help()") |
| B14 | truthiness of a failed `run()` result | truthy | truthy | same (a trap in both) |
| B15, B17 | `x is None`, `a is b` for distinct lists | | | same |

## `with`

| Id | Operation | Python | Stone | Verdict |
|---|---|---|---|---|
| W5 | `with 5 as x:` | TypeError | runs, binds `x = 5` | **SILENT** |
| W10 | `for line in f:` | lines keep `"\n"` | lines lose `"\n"` | **SILENT** |
| W4 | `with a as f, b as g:` | works | "only a single with item is supported yet" | no fix |
| W9 | `f.closed` | True | "file objects are task-owned runtime values…" | no fix, mid-run |
| W1, W3, W6, W7 | names visible after the block, flush at exit, error propagation, no `as` | | | same |
| W2, W8 | read or write after close | ValueError | Stone error | same |

## `except` And `try`

| Id | Operation | Python | Stone | Verdict |
|---|---|---|---|---|
| E7, E12 | `str(e)`, `f"{e}"` | the message | the whole error record as text | **SILENT** |
| E20 | `repr(e)` | `ValueError('bad')` | record text | **SILENT** |
| E15 | `ValueError == "ValueError"` | False | True | **SILENT** (documented) |
| E1 | bare `except:` | catches all | rejected, names the fix | fix |
| E2 | `except BaseException` | catches all | rejected, explains budget stops | fix |
| E3 | `except Exception: pass` | allowed | rejected, explains the trace loss | fix |
| E4 | `except error:` | NameError | rejected, names the fix | fix |
| E5 | `try/finally` | works | rejected, explains host cleanup | fix |
| E9 | `raise X from e` | works | rejected, names the fix | fix |
| E10 | `except KeyboardInterrupt` | works | rejected, explains | fix |
| E11 | `except json.JSONDecodeError` | works | `import json` rejected, lists replacements | fix |
| E6 | `try/except/else` | works | "try/else is not supported yet" | no fix |
| E14 | `assert` | works | raw parser dump | no fix |
| E8 | `e.args[0]` | `"bad"` | lists the available fields | no fix, mid-run |
| E16 | `type(e).__name__` | `"ValueError"` | "string has no attribute `__name__`" | no fix, mid-run |
| E13, E19 | `isinstance(e, ValueError)`, bare `raise` | | | same |

### Budget And Policy Stops

- **Budget:** a `tool_calls=1` budget inside
  `try: … except Exception as e:` ends the program with
  `status: "unfinished"`. The handler never runs. In the runtime, only budget
  stops bypass handlers (`stone_eval.rs`, `eval_try_stmt`).
- **Policy:** the runtime has no separate "policy stop".
  - The `BaseException` correction and `docs/STONE_LANGUAGE.md` both say
    that "policy stops end the attempt and are not catchable".
  - By code reading (not tested against a live Gateway), a path or Gateway
    denial arrives as an ordinary error, so `except Exception` can catch it.
    The denied effect still doesn't happen, so authority is not bypassed.
  - A `run(...)` pre-hook denial returns `ok=False` instead of raising.
  - **Decision needed:** add a real uncatchable policy stop, or correct the
    message and the docs.

## Other Lookalikes Found

| Id | Operation | Python | Stone | Verdict |
|---|---|---|---|---|
| X31 | `{1: "a"}` | int key | key silently becomes `"1"`; `d[1]` then fails | **SILENT** |
| X32 | `{True: "a"}` | bool key | key silently becomes `"true"` | **SILENT** |
| X7 | `print([1, "x"])` | `[1, 'x']` | `[1,"x"]` (JSON) | **SILENT** (documented) |
| X16 | `d.keys() == ["a"]` | False | True | **SILENT** (minor) |
| X5, X6 | `d[1]`, `d[1] = …` | work | "cannot index/assign record … with int" | no fix, mid-run |
| X15, X33 | `2 ** 70`, `2 ** 3` | work | raw parser dump (`**` unsupported) | no fix |
| X10, X20, X25 | `f(*xs)`, walrus, slice step | work | raw dump or "not supported yet" | no fix |
| X12, X19, X22, X23 | `**kwargs`, `for/else`, `%` formatting, `removeprefix` | work | terse errors | no fix |
| X28 | `key=len` | works | "unknown name `len`": builtins are not values | no fix, mid-run |
| X8, X13, X17, X18, X21, X24 | `nonlocal`, `def f(x=[])`, `import os`, `global`, `list(gen)`, class with methods | | | fix |
| X27 | `dict(d)` | works | unknown function | fix (generic) |
| X1–X4, X9, X11, X26, X29, X30 | aliasing, argument mutation, `+=`, closures, keyword calls, chained comparison, float text, `get` | | | same |

## Proposed Fixes For The Silent Cases

These are proposals, in order of expected harm. Nothing is changed yet.

1. **`str(e)`, `f"{e}"`, `repr(e)` of a caught error.** Agent code writes
   `f"failed: {e}"` constantly, and today it embeds the whole record. Rule 1
   says to match Python: `str(e)` is the message and `repr(e)` is
   `ValueError('bad')`. Structured fields stay available as `e.type`,
   `e.code`, `e.message`.
2. **Literal dict keys that silently become strings** (X31, X32). Reject
   non-string literal keys before execution, with a correction: "record keys
   are strings; write `\"1\"` or use `str(k)`". The alternative is real int
   keys.
3. **Tuple mutation and aliasing** (T1, T2, T4, T13, T17). Give list values a
   one-bit "from a tuple" mark. Mutating methods, item assignment, `+=`, and
   `+` with a plain list raise with a correction. The cost is one flag check
   on mutation paths, not on reads.
4. **`set()` results** (S4, S17, S18, S19, S21, S22, S23, S25, S30). Use the
   same mark for "from `set()`":
   - `==` compares as sets;
   - indexing, `append`, `+` and `sort` raise with a correction, pointing to
     `sorted(s)`;
   - unhashable members raise;
   - `1` and `1.0` collapse.
   The missing methods and operators (`union`, `|`, …) are a growth question
   for the observed-guess log, but each should raise with a correction now,
   not "expected integer".
5. **Bool and int equality** (B3, B6–B8, B10, B19). Two options:
   - match Python (`True == 1`);
   - keep strict booleans, but raise when `==`, `in` or `count` mixes a bool
     with an int, with a correction.

   Rule 3 rules out the current silent `False`. This needs a decision.
6. **`is` between non-singletons** (B16, B18). Allow `is` and `is not` only
   when one side is `None`, `True` or `False`. Otherwise reject before
   execution: "use `==`".
7. **Reading file lines** (W10). Keep the `"\n"`, as Python does.
8. **`with` on a value that isn't a context** (W5). Raise.
9. **Documented deviations** (E15 `ValueError == "ValueError"`, X7 `print`
   of a list as JSON, X16 `keys()` equality). Each one was a deliberate
   choice, and each breaks rule 3. Each needs a keep-or-change decision.

### Error Quality

- **Raw parser dumps:** 9 cases print parser internals instead of a
  correction (T3, T6, S1, S2, E14, X10, X15, X20, X33). Replacing each
  with one sentence that names the fix is cheap and has no semantic risk.
- **Mid-run rejections that could happen before execution:** some can't move
  earlier, because the type depends on runtime values (B4). But
  `isinstance(x, tuple)`, `list(gen)`, `x % (...)` with a literal format, and
  `e.args` are all visible in the source.
- **The `s.discard` collision (S14):** a set method name reaches attempt
  control. Method dispatch on a list should never reach `attempt_*`.

## Proposed Core And Authoring Split

Today `help()` is about 54,500 characters (about 14,000 tokens). Its 158
builtin entries take 46,300 of those characters.

**Core, shown by default (88 entries, 20,500 characters, 45% of the builtin
text):**

- **Data:** `len list tuple set range enumerate sort map filter where first
  last get keys values items min max sum any all round int float str repr
  type isinstance parse_int parse_float split join slice starts_with format
  json_loads json_dumps print emit fail help`
- **Files:** `open read_file write_file ls find search stat diff edit mkdir rm
  pwd cd read_json write_json read_jsonl write_jsonl read_csv write_csv
  to_jsonl md5 sha1 sha256`
- **Commands:** `run must_run Expect Effects run_status run_wait
  run_terminate start_daemon daemon_status stop_daemon wait_port wait_for ps
  resolve_command sysinfo`
- **Transactions:** `env_state env_commit env_rollback env_restore
  env_checkpoint env_restore_checkpoint`
- **Typed model calls:** `model.ask model.limit Record`. **Decision needed:**
  the guideline's core list does not name model calls. I'd keep them,
  because typed model calls are what ordinary Stone programs use.

**Authoring, shown by `help("authoring")` or a topic (70 entries):**
`attempt_*` (29), `semantic_frontier*`, `workflow*`, `stage`, evidence specs
(`artifact`, `command_succeeded`, `file_nonempty`, `file_valid`,
`stdout_nonempty`, `decision_recorded`, `all_evidence`), `context_*`,
`agent_session`, `react_control`, `scripted_control`, `transition_hooks`,
`correction_apply`, `current_program`, `task_spec`, `task_input`,
`last_result`, `state`, `model_call`, `model_infer`, the remaining `env_*`
checkpoint and transaction calls, and the aliases `cat`, `echo`, `save`.

## Proposed One Spelling Per Concept

Each row keeps one spelling in model-facing help. The others stay accepted
but move to internal or kernel help. Nothing is removed.

| Concept | Spellings today | Proposed model-facing | Basis |
|---|---|---|---|
| Ask a model | `model.ask`, `model_call`, `model_infer` | `model.ask` | typed output, used by every Q2 program |
| Explore candidates | `explore()` library, functional `attempt_*`, scoped methods, `async` | `explore(...)` for the common case; scoped methods otherwise | library 3/3 first-response vs explicit 1/3 (bounded exploration experiment) |
| Create a child | `attempt_spawn`, `attempt_fork`, `attempt_branch`, `scope.branch`, `env_fork`, `attempt_start` | `scope.branch(frontier, …)` | scoped methods passed 3/3 and are the documented preferred spelling |
| Wait for children | `attempt_wait`, `attempt_join`, `child.wait`, `await child.wait`, `attempt_wait_any/all`, `scope.wait_any/all` | `child.wait`, `scope.wait_any`, `scope.wait_all` | same |
| Keep or drop a child | `attempt_accept`, `root.accept`, `attempt_best_accept`, `attempt_discard`, `child.discard`, `attempt_best_discard`, `attempt_finish(action=…)` | `root.accept(child)`, `child.discard(reason=…)` | same |
| Async attempt control | `async def`, `async with`, `await` | internal | did not beat synchronous scoped syntax (27 lines each) |
| Staged workflow | `@stage`, `workflow_stage(...)` + `workflow(...)` + `workflow_run`/`workflow_main`, block `workflow`/`stage`/`ensure` | block form with `ensure` | the `ensure` arm wrote complete contracts 3/3, against 0/3 for both header forms, and was 28–36% smaller |
| Read a file | `open().read()`, `read_file`, `cat` | `open` and `read_file` (ranges); `cat` internal | Python prior plus ranged reads |
| Write a file | `open(…, "w")`, `write_file`, `save` | `open` and `write_file`; `save` internal | same |

Our analogy guidance:
- `child.wait` matches futures;
- `branch` and checkpoints match git;
- `accept`/`discard` have no close Python prior and keep their own names.

Evidence gates should not borrow assert/pytest naming for checks the model
wrote. In the SWE pilots, model-written checks passed 9 of 12 wrong fixes,
so they do not have pytest's meaning.

## Measurement Plan Before Any Change

Per the guideline:
- **Validity metrics:** first-attempt validity and retries-to-valid, with at
  least 20 samples per arm.
- **Models:** at least one mid-tier model (local Qwen with thinking off, plus
  Haiku).
- **A wrong-guess log:** every rejected construct, with the spelling the
  model used.

The first runs should target the silent cases above. A fix is worth making
when models actually write the construct. `str(e)` and `f"{e}"` are almost
certainly common; set operators may not be.

## Decisions (2026-11-09)

1. **Booleans stay strict.** `True == 1` stays False in meaning, but by rule 3
   the current silent results must become errors. A `==`, `in`, `count`,
   `index`, or set membership test that mixes a bool with an int raises, with
   a correction such as "compare with `x == 1` or `bool(x)`".
2. **Documented deviations:** open; see the options below.
3. **Stops:** split by who decided (Fable, relayed 2026-11-09).
   - **Per-call denial** (Gateway refuses an effect, or a hook vetoes a
     command) stays catchable. Nothing happened, and the program can route
     around it. Hook vetoes already return `ok=false`,
     `kind="policy_rejected"`. Fix the `BaseException` message and the docs
     to say "denied, not executed, recoverable". The host counts denials in
     the trace and the result envelope, whether or not they were caught.
   - **Attempt-level stop** (budget or deadline exhausted, cancellation by
     the parent or host, revoked authority, denials over a threshold) becomes
     a real uncatchable class. `eval_try_stmt` re-raises any error for which
     `is_control_stop(err)` holds, before any handler runs. Runtime-owned
     cleanup, such as scope cancel-then-join and frontier release, still
     runs. Today only the budget stop takes this path; `wall_time_ms` is
     part of the budget. Gateway stays the enforcer: once an attempt is
     stopped, it refuses further effects.
   - **The test for each error:** did the host say "this call is not
     allowed" or "this attempt is over"? Cancellation and timeouts get
     audited against that question.
4. **Model calls stay in the core tier** (`model.ask`, `model.limit`,
   `Record`).
5. **Stronger typing, not hidden flags.** Tuples and sets get real runtime
   types, with Python's meaning:
   - `isinstance(t, tuple)` is True;
   - mutation raises TypeError;
   - set equality ignores order;
   - unhashable members raise;
   - `str`/`repr` give Python's text.

   Operations a type does not support raise, as in Python. This follows
   Python's strong typing, not JavaScript-style coercion, and it gives the
   checker and the JIT a known type at every literal site. Set operations
   that are not built yet raise with a correction until the wrong-guess log
   shows they are needed.

## After Fixes 1–8 (2026-11-09)

The probe (`host/audit_stone_python_lookalikes.py`) was rerun on the changed
build. Counted automatically: a case is *silent* when both Stone and Python
run and give different values.

| Outcome | Before | After |
|---|---|---|
| Same value as Python | 31 | 53 |
| Silent difference | 27 | 5 |
| Stone raises, Python runs | 59 | 62 |
| Python raises, Stone runs | 11 | 0 |
| Both raise | 5 | 16 |
| Error text is a raw parser dump | 8 | 0 |

The five silent cases left:

- **S3** set iteration order: Stone keeps first-insertion order, Python uses
  hash order. Programs should not rely on either; this is documented.
- **E7** the wording of an `int("x")` error message differs; both raise.
- **E17, E18** Stone-only behavior (hook denials, write roots) with no Python
  counterpart.
- **X16** `d.keys() == [...]` is true: the accepted difference (Q2.3).

What changed:

1. **Control stops and denials.** A host stop (budget, deadline, cancelled
   or finished attempt) is never caught by `except`; a denied call is an
   ordinary recoverable error, counted in `diagnostics.denials`.
2. **Caught errors print as Python.** `str(e)`, `f"{e}"`, `format`, and
   `print(e)` give the message; `repr(e)` gives `ValueError('message')`.
   `e.type` and `type(e)` are the type itself; `type(plan)` is the record
   class. A type compared with a string raises with an `isinstance`
   correction. `print` writes Python text for every value; `emit` stays JSON.
3. **Booleans are not numbers.** `True == 1`, `1 in [True]`,
   `[1, True].count(1)`, and nested forms raise.
4. **`is`.** With a literal other than None/True/False it is rejected before
   running; between numbers or strings it raises; lists, dicts, records,
   tuples, sets, types, and bools keep Python's identity meaning.
5. **Dict literal keys** must be strings; `{1: ...}`, `{True: ...}`, and
   computed keys are rejected before running with a correction.
6. **Tuples and sets are real types** (`stone_collections.rs`), with
   Python's meaning. They become JSON arrays where values leave the program.
   A tuple or set compared with a list raises. Lists lost `.add()`.
7. **Unsupported Python forms** (`del`, `assert`, `match`, `:=`, `**`, `@`,
   `*` unpacking, `yield`, `...`, bytes, top-level annotations, set
   literals and comprehensions) get a one-sentence correction instead of a
   parser dump.
8. **File lines keep `"\n"`** (`for line in f`, `f.readlines()`), as in
   Python; `f.splitlines()` still drops it. `with` on a value that releases
   nothing raises.

Found during the work, not changed (candidates for a decision):

- Computed keys in dict literals (`{name: 1}`) are not supported at all.
- There is no `**` operator and no `pow()`.
- Lists and dicts have no `pop()`, and there is no `del`.
- Iterating a string yields its lines, not its characters.
- `type(x)` returns a type for records and errors but a name string for
  builtin values (`type(1) == "int"`), and builtin types such as `int` and
  `len` are not values (`sorted(xs, key=len)` fails).
- `zip` and `dict()` do not exist.
- A value assigned inside an `except` block was dropped at the end of the
  block if it was a record, type, or model; this is fixed, and tuples and
  sets also survive.
