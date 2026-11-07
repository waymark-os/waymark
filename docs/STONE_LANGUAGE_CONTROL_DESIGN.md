# Stone As An Agent Control Language (Draft)

Status: draft v3 (2026-11-06). Decisions S1-S3 are settled. Draft v3 adds the
decisions from the literature review (Section 3). Each one is marked **from
review** or **deviates from review because ...**. One review decision
conflicted with S2; it is resolved provisionally by D-1 (Section 3.1),
pending LLM trials. Nothing is
implemented. Items marked **H-n** are hypotheses to test, **L-n** are language
decisions (Section 7), and **R-n** are review decisions (Section 3).

Review: "Literature review: language-level control for the Waymark agent
shell" (2026-11-06). Its questions are cited here as **RQ1-RQ6**, to keep them
apart from the brief's Q1-Q5 (Section 8). Its quotes were checked against the
primary sources (Section 12.1).

## 1. Goal

Stone is the language Waymark agent loops are written in. Earlier work made
Stone easy for LLMs to write without retraining and gave it a strongly typed
runtime. This design makes Stone good at agent control:

- **Authority**: what a loop and its subagents may do.
- **Outcome**: what the loop must establish before it may claim success.
- **Resources**: how much model, tool, and wall-clock budget each part gets.
- **Lifecycle**: which resources must be released before a scope ends.
- **Recovery**: which failures a loop may handle, and which it must not
  swallow.
- **Expectations and side effects** (per tool call): what each invocation may
  change, and optionally what it is expected to achieve. The runtime confines
  the call to its declared writes and reports whether the observed result met
  the expectation.

Each contract is part of a function's signature or a value's type, written in
Python-shaped syntax. Admission checks it before running where it can, and the
runtime enforces it where it cannot. The aim is not only safety. The runtime
should raise the chance that a loop finishes the task and lower the chance it
derails: claiming success without evidence, editing tests instead of code,
spending the budget before verifying, looping without progress, or swallowing
errors.

The language must also stay easy to optimize (see
`docs/STONE_VM_PERFORMANCE_PLAN.md`).

### Settled Constraints

- Target models: Claude Haiku 5.5, GPT-6 Luna, Qwen3.6-27B. Python-shaped
  surface only. No Haskell or Lean.
- No retraining or fine-tuning: few-shot prompting plus a checker in the loop.
- Contracts use syntax these models already write reliably: annotations and
  pydantic/FastAPI-shaped declarations.
- Deviations from Python are absent or loud, never quietly different.
- Errors contain the fix.

### Settled Decisions (2026-11-06)

- **S1. Python semantics fixes are adopted** (L1-L4): lexical scoping and
  closures, reference semantics for lists and dicts, `or`/`and` return
  operands, and Python `str()`/`round()` behavior. Compatibility impact is
  still measured first, to plan the migration, not to revisit the decision.
- **S2. Effect defaults follow reversibility.** Whether an effect can be
  undone is a property of the execution substrate, reported by the host, not
  of the operation. On Waymark, file operations run on an overlayfs-backed,
  sandboxed workspace and are reversible, so undeclared file effects are
  allowed and reported. Effects that cannot be reversed (network, external
  services, anything outside the overlay) stay off. They are prohibited unless
  the host grants a capability for them and the call declares them.
  **For model-issued commands in the controlled profile, D-1 provisionally
  replaces the first half of S2 with enforcement, pending LLM trials.**
- **S3. Priority: task completion first, then drift, then security.** The
  roadmap (Section 13) orders features by that priority. Security properties
  are designed in from the start so later phases do not change the language,
  but strict enforcement ships last.

## 2. What We Borrow From Language-Based Agent Control

Reference: Zhou, D'Antoni, Polikarpova, "Language-Based Agent Control"
(arXiv 2605.12863v2), system Elba. The paper was read in full, along with the
Elba source (`core/LBAC.hs`, `core/SystemPrompt.md`, `examples/files/lib/RIO.hs`,
`examples/ifc/agentdojo/IfcGuidance.md`).

Core ideas we adopt:

1. **Control is expressed inside the language, not around it.** Agent-written
   code and scaffolding share one language and one set of rules. Elba's
   soundness argument (App. B) needs the whole loop, including retry logic,
   to be in the host language.
2. **Policies are types of values and functions.** What a computation may do
   follows from what it is given and what it must return.
3. **Values the agent cannot forge.** Elba's `Trusted Bib` has a hidden
   constructor, so only the DBLP API can produce one. That gave 0/925
   hallucinated citations versus 19/1002 for a pydantic-ai baseline (Table 4).
   This is the most transferable result.
4. **Subagents inherit a ceiling.** A child can be stricter than its parent,
   never looser.
5. **Check, then run; feed errors back.** Generated code that fails the check
   is rejected before any effect, and the error returns to the model.

Where Elba's examples mislead for our setting:

- Its static guarantee covers which operations exist and which values reach
  them. Path containment (`//`, `scoped`), information flow (LIO), and
  recursion depth are **runtime** checks in Elba too (App. A.3: "policies are
  not checked statically"). LLM calls are ambient (`unsafePerformIO`) and
  unbudgeted.
- The 0.1 average retries (Table 3) is for Opus 4.7/4.8, GPT 5.4, and
  Sonnet 5. It is unmeasured for our target models.
- IFC policies roughly halve utility even for frontier models (Table 1: 8-10/21
  tasks with policy versus 14-18/21 without), and need a six-rule prompt guide
  on structuring scopes.
- The tasks are single-shot tool compositions. Real Waymark loops run tens of
  model calls with verification, critique, owned processes, and context
  management (`examples/scripts/standard_attempt_agent.stone`, about 2,200
  lines).

## 3. Review Decisions

The review's two cross-cutting conclusions, both adopted **from review**:

- Confinement in a dynamically typed language is a property of the reference
  graph, not of the type system (E, Joe-E, Hardened JS). Stone can have it if
  it closes a short list of leak channels.
- Almost every answer routes through one mechanism: a walk over the values
  that cross a boundary. Build it once (R24).

| ID | Decision | Marking | Section |
|---|---|---|---|
| R1 | Confinement comes from the reference graph. Close the leak channels in R2-R8. | From review (RQ1) | 5.1 |
| R2 | Exceptions carry data only. A raise whose payload reaches a capability is an error at the raise site. Host errors never embed a handle. | From review (RQ1, Joe-E). Stricter than Agoric, whose errors may carry other passables (12.1). | 5.5 |
| R3 | No frame, traceback, locals, or globals object is ever reified. Tracebacks are strings. | From review (RQ1). Already true today. | 5.5 |
| R4 | The builtin table is frozen and shared read-only. No attempt, helper, or session binding can change what a builtin name means for anyone else. | From review (RQ1) | 5.1 |
| R4a | Shadowing a builtin name in your own lexical scope stays legal (`id = ...`, `type = ...`). | **Deviates from review** ("no attempt can rebind `read_text`") **because** the threat is cross-boundary sharing, which R4 plus R7 and per-attempt namespaces remove. Rejecting local shadowing would reject common Python. | 5.1 |
| R5 | No mutable module-level state crosses a trust boundary. Values defined at the top level of an L9 module are deep-frozen at import. | From review (RQ1, Joe-E) | 7 (L9) |
| R6 | The session is a vat with a ceiling the host fixes at session start. Bindings, `last_result()`, and `state()` stay within it. A child gets session bindings only if passed, never by lookup. | From review (RQ1). Replaces draft v2's "capabilities are not session-persistable". | 8 (Q4) |
| R7 | Host callbacks (helpers, hooks) sit behind a membrane. Helpers run in their own evaluator, receive data or attenuated handles, never raw capabilities, and return data that enters only through the ceiling. | From review (RQ1) | 9.3 |
| R7a | In the controlled profile, helpers are not loaded from directories inside the agent's write grant. | Not in the review; my concretization of R7 after finding that cwd helpers are agent-writable (9.3) | 9.3 |
| R8 | A child's initial reference graph is exactly its arguments (loader isolation). | From review (RQ1) | 5.7 |
| R9 | Capabilities may live in lists, dicts, and records. Draft v2's H-4 ("ban capabilities in `Any` positions") is dropped. | From review (RQ2) | 5.1 |
| R10 | Child start, `delegate`, and `Verified` sealing are pass-style boundaries: data and containers are deep-copied, capability handles are shared by reference, errors cross as copy data. `freeze()` is an explicit opt-in instead of a copy. | From review (RQ2, Agoric `passStyleOf`) | 5.7 |
| R10a | A closure may cross a boundary only if its captures are deeply immutable or capability handles. Otherwise it is a loud error that says to pass the data explicitly. | Not covered by the review. Needed because a captured mutable list is a post-spawn loosening channel. | 5.7 |
| R11 | `def f(x, acc=[])` is rejected with a prescriptive error. | From review (RQ2). Already rejected at definition today; only the message changes. | 9.4 |
| R12 | `ExpectationFailed` carries `.expected`, `.observed`, `.diff`, and the tool's full `.result`. | From review (RQ3) | 5.8 |
| R13 | The runtime counts handlers that catch a `ToolError` subclass and neither re-raise nor bind it, and reports the count in the attempt trace and `last_result()`. | From review (RQ3) | 5.5 |
| R14 | A handler for `Exception` or `ToolError` (or a subclass) whose body is only `pass` is an admission error with a prescriptive message. | From review (RQ3), **narrowed because** admission cannot always tell whether a try body reaches a tool call. `except KeyError: pass` stays legal. | 5.5 |
| R15 | Bare `except:` is a parse error. `BaseException` does not exist in Stone, so no spelling catches a control error. | From review (RQ3, RQ4) | 5.5 |
| R16 | `PolicyViolation` has Joe-E `Error` semantics. Agent code cannot catch it, no agent code runs after it, the host cleans up, the attempt is dead, and the parent sees the violation as data. | From review (RQ4) | 5.5 |
| R16a | `ContractViolation` (for example, returning a bare `Report` where `Verified[Report]` is required) gets the same semantics as R16. | Not covered by the review. My choice: it is an attempt to claim an outcome the attempt does not hold. | 5.5 |
| R17 | `BudgetExhausted` has Trio cancel-scope semantics. It is raised inside a `with budget(...) as b:` scope, cannot be caught by inner code, is absorbed by the scope that owns the exhausted budget, and is exposed as `b.exhausted`. | From review (RQ4) | 5.3 |
| R17a | `model.reserve(...)` is dropped. Room to verify comes from running the loop in a budget scope that holds some budget back (`grace=`). `model.limit(...)` stays, for passing a narrowed `Model` to a child. | Simplification of R17; not in the review | 5.3 |
| R18 | `finally` stays absent. Code after the budget scope plays the role of the review's grace-budget `finally`. | **Deviates from review** (whose table lets `finally` run under a grace budget) **because** Stone has no `finally` today, Joe-E bans it, host compensation (R19) removes the resource-cleanup reason for it, and code after the scope gives the same best-effort result without code running during unwinding. | 5.3 |
| R19 | The host is the compensation stack. It releases every resource an attempt opened on any exit. Agent code never cleans up resources. | From review (RQ4, Weimer & Necula). **Only partly true of the host today** (9.1). | 5.4, 9.1 |
| R20 | `Verified[T]` is a sealed box holding a frozen copy of the value, the verifier id, the claim checked, and the attempt nonce. | From review (RQ5, Morris 1973) | 5.2 |
| R21 | Verifiers are host-minted only, run in their own attempt on a read-only snapshot, and take no evidence argument. An LLM judge, if ever used, says so in its claim. | From review (RQ5). Drops draft v2's `critic=` argument to `verify`. | 5.2 |
| R22 | A `Verified` value is accepted only by the parent of the attempt that minted it (nonce check). | From review (RQ5) | 5.2 |
| R23 | A 0-CFA reachability check at admission catches bad returns and spawn arguments before any budget is spent. The runtime check stays the guarantee. | From review (RQ1, RQ5), **scoped because** L12 requires per-command admission to stay a linear pass. It runs on delegated bodies and library loops, not on every one-line command, and ships in P3. | 7 (L12) |
| R24 | One reachability-walk component, used at child start, return, and host callbacks. | From review. Two more call sites are mine: `raise` (R2) and session injection or restore (R6). | 5.1 |
| R25 | Declared writes are mandatory and enforced at the capability layer. | From review (RQ6, the brief's Q6 directive). Adopted provisionally by D-1; the enforcement mechanism (per-call overlay layer) is ours, since the review does not cover the OS layer. | 5.8 |
| R26 | Outcome prediction (`expect=`) is optional, typed, and host-diffed. The diff is reported only on a mismatch, and mismatches are kept in history. | From review (RQ6) | 5.8 |
| R26a | Expected exit status belongs to the optional prediction, not the mandatory declaration. | **Deviates from review** (which lists exit status with the mandatory declaration) **because** exit status is an outcome prediction, and keeping it out of the mandatory part keeps ablation arms 2 and 3 distinct. | 5.8 |
| R27 | Set up, but do not run, the three-arm ablation. | From review (RQ6). GPT-6 Luna added as an optional third model if we have access. | 11.1 |
| R27a | Add a fourth arm: declared writes, reported but not enforced. Measure write-prediction accuracy in every arm that declares. | **Deviates from review** (three arms) **because** predicting side effects may be hard. Without this arm, a drop in arm 2 could come from bad predictions or from enforcement, and we could not tell which. | 11.1 |

### 3.1 Decisions

**D-1. Undeclared reversible writes: refuse (R25) or allow and report (S2)?**
**Decided provisionally (2026-11-06): start with mandatory, enforced
declarations, and let LLM trials decide what stays.** Predicting side effects
may be hard for the target models, so this is a starting point, not a
settled rule. The review does not cover the OS layer. The enforcement
mechanism below is ours.

S2 says undeclared file writes on the overlay are allowed and reported. R25
says declared writes are mandatory and undeclared writes are refused. These
conflict for model-issued commands.

The starting design:

- **Controlled profile, model-issued `Exec` and host-tool calls: enforce
  (R25).** Each call runs in its own overlay layer. If it wrote anything
  outside its declared set, the whole layer is discarded (all or nothing, so
  no half-applied build is left behind), and the call raises
  `UndeclaredEffect`. The error names each undeclared path and the fix ("add
  `.pytest_cache` to `writes`, or run with `--no-cacheprovider`"). Where the
  Gateway can mount everything except the declared paths read-only, the write
  is prevented outright instead of discarded.
- **Default profile (today's scripts): S2 unchanged.** Undeclared reversible
  writes are kept and reported.
- **Irreversible effects: unchanged.** They are prohibited unless granted and
  declared. Reversibility (S2) is what makes enforcement cheap: discarding a
  layer is the refusal.
- **Revisit by measurement.** The trials (11.1) separate two questions: can
  the models predict writes (arm 2r, report-only), and does enforcement cost
  task success (arm 2 versus 2r and arm 1)? If enforcement lowers task
  success, S3 says fall back to S2 behavior.

The completion risk is incidental writes (`__pycache__`, `.pytest_cache`,
build outputs, tool caches under `$HOME`) that the model did not predict.
Declarations can name directories (`src / "build"`), and the error is
prescriptive. Whether that brings the cost down to about one repair per new
command family is unmeasured (H-9, H-12).

If the trials show prediction is hard, the fallbacks, in order of how much
they keep:

1. Coarser declarations: directory-level `writes` only, or a per-command
   write profile the runtime learns from earlier observed runs of the same
   command family and offers to the model as a default.
2. Enforce only where the declaration matters for drift (for example, writes
   into read-only-intended paths such as `tests/`), and report elsewhere.
3. S2 as the default: report only.

Note what discarding does and does not do. The command still saw its own
writes while it ran, so a refusal by discard is equivalent to prevention only
in its effect on the workspace afterward.

## 4. The Problem In The Current Stone Loop

`standard_agent_control` (V14) is the standard visible controller. Its main
loop shows what is missing from the language:

- **Untyped state**: about 50 keys in one `state` dict (`"model_calls"`,
  `"finish_blocked_active_runs"`, `"budget_checkpoint_done"`, ...), updated
  by hand. A typo creates a new key silently.
- **Budget arithmetic by hand**:
  `state["model_calls"] += inference.validation_attempts`, a manual reserve of
  calls for critique, and a manual proactive checkpoint window. A missed
  increment is silent.
- **Success is a convention**: `finish` is checked by an adapter
  (`verify_finish`) plus a model critique. Nothing in the language stops a
  modified loop from returning a result without either.
- **Lifecycle is a convention**: finish is rejected while
  `state.active_runs` is non-empty, maintained by hand.
- **Authority is invisible**: every action can read, write, or run anything
  the Gateway allows. A loop cannot say "edit `src/`, never `tests/`".
- **No modules**: the controller ships as a pasted source bundle because Stone
  has no import.

Each item is a place where a contract would replace about 20-200 lines of
library convention with something the runtime guarantees. Whether that
improves task success is H-1.

## 5. Contract Model

### 5.1 Authority: Capabilities As Values

There is no ambient authority in the **controlled** profile. Effects go
through capability values the host grants: `Dir`, `File`, `Exec`, `Model`,
`Verifier`, and host-defined ones. A function's authority is what is reachable
from its parameters.

```python
def solve(task: str, src: Dir[Read, Write], tests: Dir[Read],
          shell: Exec, model: Model, check: Verifier) -> Verified[Report]:
    ...
```

- Capabilities are created only by the host. Stone code cannot construct them,
  and no API accepts a string as authority. `str(src)` is display text.
- Narrowing only removes rights: `src / "pkg"`, `src.read_only()`,
  `shell.within(src / "pkg")`, `model.limit(calls=30)`.
- Capabilities may sit in containers (R9). A list of `File` handles is
  ordinary. The boundary walk (R24) descends into containers and closures.
- "Child no looser than parent" holds by construction plus the boundary rules
  in 5.7. A parent can only pass capability objects it holds, or narrowings
  of them (H-2).
- `Exec` is the residual hole: Stone cannot see what argv touches. Every
  `Exec` must be bound by the Gateway or OS sandbox to the directory
  capabilities in the same grant (`shell.within(...)` maps to a sandbox mount).
  Mandatory declared writes (5.8) narrow this further, to what one command
  says it will touch.

The leak channels the review lists, and where each is closed:

| Channel | Rule | Today |
|---|---|---|
| Capability inside an exception | R2: walk at `raise` | Exceptions bind JSON strings; no handles can leak yet |
| Frame or traceback objects | R3 | None are exposed |
| Mutable builtins | R4: frozen table | `def read_text(...)` shadows the builtin and persists in the session (9.4) |
| Mutable module state | R5 | No modules yet |
| Session namespace | R6: vat with ceiling | Locals and functions persist with no ceiling (9.2) |
| Host callbacks | R7: membrane | Helpers share the agent's evaluator (9.3) |
| Loader grants at spawn | R8 | No function-plus-arguments spawn exists yet |

This prevents a known derailing mode directly. A loop granted
`tests: Dir[Read]` cannot make tests pass by editing them.

**The boundary walk (R24).** One runtime component,
`collect_capabilities(values)`, walks a value graph (lists, dicts, records,
closure captures, `Verified` boxes, exceptions) and returns the capabilities
it reaches. It has five call sites:

| Call site | Check |
|---|---|
| Child start, `delegate` | Every capability reached is within the parent's ceiling and matches the child's declared parameter types; data is copied (R10) |
| Attempt return, `Verified` sealing | The value is copied or frozen; a sealed value holds no capabilities |
| Host callback (helper in or out) | Only data crosses (R7) |
| `raise` | The payload reaches no capability (R2) |
| Session injection or restore | Every capability is within the session ceiling (R6) |

The static 0-CFA version (R23) answers the same question before running, for
delegated bodies.

### 5.2 Outcome: Unforgeable Success

`Verified[T]` is a sealed box (R20), in the sense of Morris's trademark: only
the holder of the sealer can make one, and anything else applied to it is an
error. The host holds the sealer. A `Verifier` capability asks the host to
seal. The box holds four host-written things:

- a frozen deep copy of the value, taken at seal time, so "verify report A,
  then mutate it or return report B" is not expressible;
- the verifier identity;
- the claim that was checked, as predicate plus inputs (for example,
  `tests_pass(pytest, ["tests/test_parser.py"], snapshot="abc123")`);
- the attempt nonce.

```python
outcome = check.verify(Report(summary=action.summary, files_changed=changed))
# -> Verified[Report] if the host's checks pass; a Rejected record with host evidence otherwise
```

Verifier rules (R21, R22):

- Verifiers are host-minted. Stone code cannot write one.
- The verifier runs in its own attempt, against a read-only snapshot of the
  agent's workspace. The review cites evidence that read-only tests block
  test edits while keeping legitimate success.
- `verify` takes the value to seal and nothing else. It takes no evidence or
  explanation argument; it observes the world through host code.
- If an LLM judge is ever a verifier, its claim says so, and consumers can
  refuse it. Draft v2's `check.verify(claim, critic=critic)` is dropped: a
  model critique, if kept, is a separate verifier whose claim names it.
- A `Verified` value is accepted only by the parent of the attempt that
  minted it. Replaying one from an earlier run or a sibling fails the nonce
  check.

A loop whose signature returns `Verified[Report]` cannot return a bare
`Report`. At runtime that is a `ContractViolation` (R16a). Admission catches
it early when the return expression is visibly a record (L12), and the 0-CFA
check (R23) catches the rest before the run.

**Consumer policy.** `Verified[Report]` promises only "some host verifier
stamped this report in this attempt." What a parent accepts belongs to the
parent. My proposal, not in the review: by default, the runtime accepts a
child's `Verified` only if its verifier is one the parent passed to that child
(`check.only(tests)` in 6.2). A signature form for richer policies
(`Verified[Report, by=...]`) is deferred until a use case needs it.

`Evidence` values come only from effects (tool output, file reads, process
results) and carry provenance. A model can quote evidence but cannot create
it.

### 5.3 Resources: Metered Capabilities And Budget Scopes

Two mechanisms, with different jobs:

- **`model.limit(calls=40, output_tokens=60_000)`** returns a narrowed
  `Model` whose usage the runtime meters. It is how a parent gives a child a
  budget (6.2).
- **`with budget(...) as b:`** (R17) bounds everything metered in its dynamic
  extent: model calls, tokens, tool calls, and wall clock. A budget only
  restricts, so its dynamic extent grants no authority.

```python
with budget(calls=20, seconds=600, grace=Usage(calls=2)) as b:
    for f in files:
        results.append(summarize(f))
if b.exhausted:
    emit({"partial": results, "stopped_at": b.stopped_at})
```

Semantics (from review, following Trio's cancel scopes):

- When a limit is hit, the runtime raises `BudgetExhausted` at the current
  metered call. Inner code cannot catch it. `except BudgetExhausted` is an
  admission error that shows this pattern.
- The scope whose budget ran out absorbs it. Execution continues after the
  `with` block with `b.exhausted == True`, `b.usage`, and `b.stopped_at`.
  Nested scopes behave like Trio: the innermost exhausted scope absorbs.
- With no budget scope, exhaustion of the attempt's host-given budget ends the
  attempt, and the parent sees `Exhausted` as data.
- `grace=` holds that amount back from the enclosing budget for the code
  after the scope. That code is where a loop writes its partial result, and
  the grace amount keeps it from being starved. `grace` is the review's
  reserved grace budget, moved from `finally` to the code after the scope
  (R18).

This replaces draft v2's `model.reserve(calls=4)` (R17a). Running the loop in
a scope with `grace=Usage(calls=4)` leaves those calls for verification and
summary, which is the same guarantee with one mechanism instead of two.

`model.usage` and `model.remaining` are runtime-owned and read-only.

### 5.4 Lifecycle: Scoped Resources And Host Compensation

Owned processes, child attempts, and temporary workspaces live in `with`
scopes (existing `attempt_scope` semantics: cancel, then join on exit).

The host is the compensation stack (R19). It records every obligation it
issues (open file, daemon, temp dir, child attempt) and discharges them on
every exit path, including `PolicyViolation` and `BudgetExhausted`. Agent code
never cleans up resources. That removes most reasons to want `finally`.

A function may not return while a resource it opened outside a scope is live
(runtime check at return, with an error naming the open handles). This
replaces "finish blocked while `state.active_runs` is non-empty".

The host does not track every handle today. Daemon handles are forgeable pids
with no registry in local mode (9.1). Closing that gap is a prototype item.

### 5.5 Recovery: Typed Failures

There are three tiers.

| Tier | Types | Catchable by agent code | What runs afterward |
|---|---|---|---|
| Recoverable | `ToolError` and subclasses (`ExpectationFailed`, `UndeclaredEffect`), ordinary errors | Yes | The handler |
| Budget | `BudgetExhausted` | No; absorbed by the owning `budget` scope (5.3) | Code after the scope |
| Fatal | `PolicyViolation`, `ContractViolation` | Never | No agent code. The host cleans up; the parent sees the violation as data |

Rules:

- Errors are typed: `except ToolError as e:` with fields (`e.kind`,
  `e.detail`). Today `except Exception as e` binds a JSON string.
- Exceptions carry data only (R2). `raise ToolError(detail=src)` where `src`
  is a capability is an error at the raise site that says to return the handle
  instead.
- Bare `except:` is a parse error (R15). `BaseException` does not exist.
  `except PolicyViolation`, `except ContractViolation`, and
  `except BudgetExhausted` are admission errors with the fix in the message.
- A handler for `Exception` or `ToolError` whose body is only `pass` is an
  admission error (R14): "this handler discards a ToolError such as
  ExpectationFailed; bind it with `as e` and use it, or re-raise".
- Every handler that catches a `ToolError` subclass and neither re-raises nor
  binds it is counted (R13). The count appears in the attempt trace and in
  `last_result()`.
- `PolicyViolation` follows Joe-E `Error` (R16): an attempt that just tried to
  exceed its authority does not get to run code of its own choosing
  afterward.

The Python precedent is PEP 352 and bpo-32528, which moved
`KeyboardInterrupt` and then asyncio's `CancelledError` out of `Exception`.
The known pitfall from that change is code that caught `Exception` to unwind
and then re-raise, which stopped seeing the cancellation. In Stone, resource
unwinding belongs to the host (R19), so only agent logic is affected, and the
budget-scope pattern gives that logic a place to run.

### 5.6 Output: Typed Model Calls

```python
action = model.ask(view, output=Action)    # Action is a union of records
```

The JSON Schema is derived from the record type. Use constrained decoding
where the provider supports it; otherwise validate and repair a bounded
number of times. All attempts are charged to `model`. This replaces
hand-written schema dicts (35+ lines in `typed_react_agent.stone`, about 250
lines of schema in the standard controller).

### 5.7 Delegation And The Pass-Style Boundary

- **Run a written function in isolation:** `scope.start(f, **args)` starts a
  top-level function as a child attempt. Its initial reference graph is
  exactly `args` (R8). Today attempts are started by id through the Gateway,
  not as a function with arguments, so this is new surface, not a change to
  existing semantics.
- **Generate a program (Elba's `subagent`):**
  `model.delegate(task, data=..., grants={...}, output=T)`. The model writes
  a function whose parameters are the grants and whose return type is `T`.
  Admission checks it under the controlled profile, then the runtime runs it.

Both boundaries are pass-style (R10). Each argument is classified:

| Value | Crosses as |
|---|---|
| Primitives, strings, records, lists, dicts | Deep copy |
| Capability handles | Shared reference (the same live handle) |
| Exceptions | Copy data, with no handles (R2) |
| Closures | Shared, only if captures are deeply immutable or handles; otherwise a loud error (R10a) |
| `freeze(x)` values | Shared, immutable; no copy |

Why copy: with reference semantics (L2), a parent that passes list `L` and
later runs `L.append(secrets_dir)` would widen the child's authority after the
check. Agoric's documentation gives the same example for vats. Copying makes
the walk at the boundary exact and final.

The copy is a deviation from in-process Python, where the child would share
`L`. It matches Python's `multiprocessing` and `subprocess` priors, where
arguments are copied, so I expect models to handle it. The child's results
come back through its return value, which crosses the same boundary. Whether
this surprises target models is untested (added to H-7).

### 5.8 Declared Writes And Optional Outcome Prediction

Capabilities bound what a function *may* do over its whole run. A single tool
call usually means to do much less. RQ6 splits per-call contracts into two
parts with different status:

| Part | Status | Why |
|---|---|---|
| Declared effects (`effects=`, the action's `writes`) | Mandatory, enforced at the capability layer (R25; provisional, D-1) | A policy, not a prediction. The review expects small models to produce it reliably as structured output; whether they can predict a command's writes is H-12. |
| Outcome prediction (`expect=`) | Optional per harness, typed, host-diffed (R26) | Positive evidence at GPT-3.5/4 scale (PreAct), but per-step prediction costs tokens and can hurt smaller models |

```python
r = shell.run(
    ["pytest", "tests/test_parser.py", "-x", "-q"],
    cwd=src,
    effects=Effects(writes=[src / ".pytest_cache"]),            # mandatory
    expect=Expect(exit=0, stdout_contains="passed"),           # optional
)
```

These are runtime-checked observations, not proof obligations. There is no
`requires`/`ensures` logic and nothing to prove. That keeps the settled rule
that contracts are types, capabilities, and runtime checks.

#### Two authors, one vocabulary

- **Harness code** writes `effects=` and `expect=` as record literals.
  Literal declarations are validated at admission: fields exist, and paths
  are capability expressions within the function's grant.
- **The model** writes them as data inside its action, through `output=`
  structured output. `writes` is a required field with no default, so a model
  cannot omit it; an empty list declares no writes. The vocabulary is a
  closed, JSON-Schema-expressible record set, emitted with structured or
  constrained decoding.

#### Effect vocabulary (closed)

| Field | Meaning | When omitted |
|---|---|---|
| `reads` | Dirs or files the call may read | The capability's read grant |
| `writes` | Dirs or files the call may create or modify | Empty. Writes outside the set: see D-1 |
| `deletes` | Paths the call may remove | Empty, same enforcement as `writes` |
| `network` | `False`, or a list of hosts | `False` (irreversible) |
| `background` | May leave a process running after return | `False` |
| `irreversible` | Effects outside any transaction (send, push, deploy, external API write) | `False`; allowed only with a host-granted capability **and** a declaration |

Each effect has a reversibility class: read-only, reversible (workspace
changes on the overlay that an attempt can roll back), or irreversible. This
extends the classes in `STONE_AGENT_PROGRAMMING_DESIGN.md` (`workspace.read`,
`workspace.write` transactional, `model.call` irreversible budget effect). The
host reports which class each substrate provides. Local mode without an
overlay has no cheap rollback, so enforcement there needs prevention
(read-only mounts) or is unavailable; see Section 12.

For capability methods whose effects are evident from the method
(`src.file(p).replace(...)` writes `p`), the runtime knows the effect set, and
declaring it is unnecessary. Declarations matter for `Exec` and host-defined
tools, whose effects Stone cannot see.

#### Outcome vocabulary (closed, plus one escape; optional)

| Target | Checks |
|---|---|
| Process | `exit` (int or list), `stdout_contains`, `stdout_matches` (regex), `stderr_empty`, `stdout_json` (record type), `max_seconds` |
| File | `exists`, `contains`, `unchanged` |
| Edit | `occurrences` (default exactly 1) |
| Custom | `check=fn`: a Stone function that admission proves pure (no capability parameters or captures) |

Draft v2's `Tests: passes=[...]` row is removed. Test results that count
toward success go through a `Verifier` (5.2), not a per-call expectation.

Expected exit status is part of this optional record, not the mandatory
declaration (R26a).

#### Safeguards

| When | What the runtime does |
|---|---|
| Admission | Literal `effects` must be within the function's capabilities; otherwise `PolicyViolation` before anything runs |
| Before the call | Model-supplied `effects` must be within the capability grant; otherwise `PolicyViolation`. Declared `irreversible` effects need a host-granted capability and go through the approval handler (L13). |
| During the call | The call runs in its own overlay layer, bounded by the capability grant. Irreversible effects are prevented: network off unless declared and granted, no writes outside the overlay. Where the Gateway supports it, paths outside the declared write set are mounted read-only. Timeout comes from `max_seconds`. |
| After the call | The observed file diff and surviving processes are compared with the declaration. Under D-1's recommendation, an undeclared write discards the whole layer and raises `UndeclaredEffect`. If `expect=` was given, the checks run on the observation. |
| Retry | Only effects declared idempotent may be retried automatically |

Failure tiers:

- **Outside the capability grant** (authority): `PolicyViolation`, fatal
  (5.5).
- **Outside the call's own declaration**: `UndeclaredEffect`, a `ToolError`.
- **Prediction not met**: `ExpectationFailed`, a `ToolError`. It carries
  `.expected`, `.observed`, `.diff`, and `.result` (R12), so a handler never
  needs to re-run the tool to get the output. Re-running would double the
  side effects.

Raising is the default because the most common failure in LLM-written code is
a missing check. A returned `met=False` that nobody tests fails silently. A
raise fails loudly when the model writes nothing, and R13-R15 make the
remaining failure (over-broad handlers) visible.

#### Feedback shape

The model-facing rendering states the fix-relevant facts first. When there is
no mismatch, nothing extra is added to the context (R26).

```text
run pytest tests/test_parser.py -x -q: expectation NOT met
  exit: expected 0, got 1
  stdout_contains "passed": not found (tail: "FAILED tests/test_parser.py::test_empty - KeyError: 'name'")
```

```text
run make: UndeclaredEffect, changes discarded
  declared writes: [src/build]
  undeclared: src/.deps/parser.d, src/config.h
  fix: add the paths (or src/.deps) to writes, or point the build at src/build
```

The structured form (`r.checks[i].expected/actual/met`,
`r.effects.declared/observed/undeclared/discarded`) is what the harness reads.
The runtime also counts mismatches per action signature, which gives
`log.stuck(...)` a real signal instead of string comparison. Mismatches are
kept in history; matched predictions are not (R26, following PreAct's finding
that retained predictions help).

## 6. Realistic Examples

All syntax is proposed and illustrative.

### 6.1 The Standard Coding Loop With Contracts

```python
from typing import Literal

class Read(Record):
    kind: Literal["read"]
    path: str

class Edit(Record):
    kind: Literal["edit"]
    path: str
    old: str
    new: str

class Run(Record):
    kind: Literal["run"]
    argv: list[str]
    writes: list[str]                 # mandatory: paths under src it may modify ([] = none)
    expect: Expect | None = None      # optional outcome prediction

class Finish(Record):
    kind: Literal["finish"]
    summary: str

Action = Read | Edit | Run | Finish

class Report(Record):
    summary: str
    files_changed: list[str]

class Unfinished(Record):
    report: Report
    reason: str


def solve(task: str, repo: Dir[Read], src: Dir[Read, Write], shell: Exec,
          model: Model, check: Verifier) -> Verified[Report] | Unfinished:
    log = Transcript(max_chars=40_000)          # std library: bounded history
    log.add("user", task)
    changed = []
    with budget(grace=Usage(calls=2)) as b:
        while True:
            action = model.ask(log.view(), output=Action)
            try:
                if isinstance(action, Read):
                    obs = repo.file(action.path).read_text(max_chars=8_000)
                elif isinstance(action, Edit):
                    obs = src.file(action.path).replace(
                        action.old, action.new, expect=Expect(occurrences=1))
                    changed.append(action.path)
                elif isinstance(action, Run):
                    obs = shell.run(
                        action.argv,
                        cwd=src,
                        effects=Effects(writes=[src / p for p in action.writes]),
                        expect=action.expect,
                    )
                else:
                    outcome = check.verify(
                        Report(summary=action.summary, files_changed=changed))
                    if isinstance(outcome, Verified):
                        return outcome
                    obs = outcome                # Rejected: host evidence of what failed
            except ToolError as e:               # ExpectationFailed, UndeclaredEffect, ...
                obs = e
            log.add_observation(action, obs)
            if log.stuck(window=4):              # same action, same mismatch
                log.add_control("Stop repeating; inspect the failing evidence.")
    # Reached only when the scope absorbed BudgetExhausted; runs on the grace calls.
    summary = model.ask(log.view(), output=Report)
    return Unfinished(report=summary, reason=f"budget exhausted at {b.stopped_at}")
```

A model action (structured output, validated against `Run`):

```json
{"kind": "run", "argv": ["pytest", "tests/test_parser.py", "-x", "-q"],
 "writes": [".pytest_cache"],
 "expect": {"exit": 0, "stdout_contains": "passed"}}
```

If the command fails, the next observation says which predicted check failed.
If it writes outside `.pytest_cache`, its changes are discarded and the
observation names the undeclared paths (D-1). If an action names a path
outside `src`, it is rejected before running. If it tries to reach the
network, the sandbox blocks it.

What the runtime guarantees here, where today's controller needs convention:

| Concern | Contract |
|---|---|
| Edits only in `src` | `Edit` goes through `src: Dir[Read, Write]`; `src.file("../tests/x")` fails containment |
| Tests cannot be "fixed" | `repo` is read-only, and the verifier runs on a read-only snapshot |
| Success needs evidence | Return type `Verified[Report]`; the box is sealed and frozen |
| Room to finish | `grace` holds calls back from the loop's budget scope |
| Budget accounting | Metered; `BudgetExhausted` cannot be caught inside the scope |
| Malformed actions | `output=Action`, repair charged to the budget |
| Commands touch only what they claim | Mandatory `writes`; undeclared changes discarded |
| The loop notices a failed step | Optional `expect`: `ExpectationFailed` with expected, observed, and diff |
| Errors are not swallowed | Bare `except:` rejected; `pass` handlers rejected; swallow count in the trace |
| Stuck detection | Repeated mismatch per action signature, counted by the runtime |

### 6.2 Delegation With Narrowed Authority

```python
def fix_packages(repo: Dir[Read], packages: Dir[Read, Write], shell: Exec,
                 model: Model, check: Verifier) -> list[Verified[Report] | Unfinished | Failed]:
    failing = check.failures()                   # Evidence-backed list of failing tests
    by_pkg = group_by(failing, key=lambda t: t.package)
    with attempts(max_parallel=4) as scope:
        for pkg, tests in by_pkg.items():
            pkg_dir = packages / pkg
            scope.start(
                solve,
                task=f"Make these tests pass: {[t.name for t in tests]}",
                repo=repo,
                src=pkg_dir,
                shell=shell.within(pkg_dir),
                model=model.limit(calls=30),
                check=check.only(tests),
            )
        return scope.wait_all()
```

Each child can write only its own package, spends at most 30 calls, and
succeeds only against its own tests. `task` is copied into the child; the
handles are shared (5.7). A child's `Verified` is accepted only if
`check.only(tests)` for that child minted it (5.2). A child that hits a
`PolicyViolation` shows up in the list as `Failed` with the violation as
data.

### 6.3 Generated Subprograms (Level B)

```python
triage = model.delegate(
    "Find the root cause of the crash in these logs. Read only what you need.",
    data={"crash_id": crash.id},
    grants={"logs": logs.read_only(), "model": model.limit(calls=8)},
    output=Triage,
)
```

The model is asked to emit:

```python
def main(logs: Dir[Read], model: Model) -> Triage:
    ...
```

Admission rejects any body that names `shell`, a path string API, or anything
not reachable from `logs` and `model`. The prompt shows the signature, the
record definitions, and the capability method signatures. The error for a
rejected body says what to use instead.

## 7. Language Decisions

L1-L4 are settled (S1). The rest are recommendations. L1 and L2 also settle
D1 and D2 in `docs/STONE_VM_PERFORMANCE_PLAN.md` (on the `perf/interpreter`
branch): lexical scoping, and Python reference semantics instead of
copy-on-write. That plan must be updated to match. The review (RQ2) agrees on
keeping reference semantics.

| ID | Decision | Recommendation | Why | Perf effect |
|---|---|---|---|---|
| L1 | Lexical scoping and real closures | Adopt | Dynamic scope lets a callee use its caller's capabilities, and lets helpers read agent locals (9.3). It is also a silent Python divergence. | Enables register allocation (perf D1) |
| L2 | List/dict semantics | **Python reference semantics**, with copies at pass-style boundaries (R10) | Copy semantics silently break `def add(items, x): items.append(x)` and `for row in rows: row["k"] = v` | Shared mutable objects, like Lua tables; copying only at boundaries |
| L3 | `or`/`and` return operands | Adopt Python semantics | `name = x or "default"` currently yields `True` | None |
| L4 | `str(None)`, `str(True)`, `str(1.0)`, `round()` | Python behavior | Silent divergences today | None |
| L5 | Records: field-only `class X(Record)`, `Literal`, `A \| B` unions, `T \| None`, defaults, real `list[T]` element types | Adopt | Needed for `output=`, typed state, and contracts | Fixed shapes give slot offsets for the VM |
| L6 | Typed exceptions; `except Name as e`; three tiers (5.5); exceptions carry data only (R2) | Adopt | Recovery contracts | None |
| L6a | Bare `except:` is a parse error; `pass`-only handlers for `Exception`/`ToolError` are admission errors; swallow counter | Adopt (R13-R15) | Makes the remaining exception failure mode visible | Counter is on the handler path only |
| L6b | `finally` stays absent, with a prescriptive error | Adopt (R18) | Host compensation and budget scopes cover its uses | None |
| L7 | Controlled profile and capability types | Adopt as opt-in | Authority contracts (5.1) | Effects are explicit host calls, so pure code is freely optimizable |
| L8 | Host-defined opaque types (`Verified[T]` as a sealed box, `Evidence`) | Adopt | Outcome contracts (5.2) | Deep copy at seal time, once per verify |
| L9 | Modules: `from lib import name` for pure Stone modules (top-level `def`, records, and constants only; no effects). Module-level values are deep-frozen (R5). | Adopt | Agent libraries without pasted bundles; pure modules hold no authority | Compiled prototypes and frozen constants can be cached per module |
| L10 | `from typing import Literal`, `from pydantic import BaseModel` | Accept known names as no-ops; reject unknown names loudly | Absorbs strong Python priors (H-6) | None |
| L11 | `match` statement | Defer | `isinstance` chains suffice | — |
| L12 | Static checking at admission | Linear pass per command: ambient authority, unknown record fields, unknown capability methods, visibly wrong return types, bare `except`, `pass` handlers. 0-CFA reachability (R23) only for delegated bodies and library loops, in P3. | Agents run many small commands | Per-command cost stays linear |
| L13 | Effects as internal handlers (record, replay, mock, approval); hooks become one handler kind | Adopt, no user syntax | Replay and durable loops without new constructs | Effect calls are already off the hot path |
| L14 | Per-call `effects=` (mandatory for `Exec`/host tools) and `expect=` (optional) | Adopt (R25, R26; D-1 pending) | Safeguards each tool call and gives checked feedback | Checks run on effect calls only; per-call overlay layer cost is unknown (Section 12) |
| L15 | `ExpectationFailed` and `UndeclaredEffect` are `ToolError` subclasses carrying the full result | Adopt (R12) | Loud by default, recoverable, one handler in loops | None |
| L16 | Custom `check=fn` must be pure | Adopt | Expectation checks cannot have effects | Pure functions are VM-compilable |
| L17 | `with budget(...) as b:` with Trio scope semantics; `model.limit` for children; no `reserve` | Adopt (R17, R17a) | Budget exhaustion cannot be swallowed inside, and is handled at one place | Metering is on effect calls only |
| L18 | Builtin table frozen; local shadowing follows Python | Adopt (R4, R4a) | Confinement without rejecting common code | Builtins resolve at compile time unless lexically shadowed (with L1) |

Compatibility: L1-L4, L6a, and L18 change existing behavior. Measure their
impact on examples, help examples, task specs, and the conformance corpus
(perf Phase 0) to plan the migration.

## 8. Open Questions From The Brief: Current Answers

**Q1. Two-level or one-level?**
One language, two authoring levels. Level A: a human or outer harness writes
the signature, and the model writes or runs the body. Level B: the model
writes harnesses and delegates with explicit grants. B is A applied
recursively (`delegate` generates a level-A body), so there is one checker.
Ship order follows S3; see Section 13.

**Q2. Minimal novel constructs.**
Capability-typed parameters, field-only record classes, `output=`,
host-defined opaque types, and `with budget(...)`. `spawn` becomes
`scope.start` with pass-style arguments. Taint and declassify are deferred.
Algebraic effects stay internal. Narrowing uses `/` (pathlib prior), not `//`
(H-5). Per-call effects and expectations add no syntax: they are two record
types passed as keyword arguments, and model-authored ones arrive as
structured-output data.

**Q3. How much of Elba's static guarantee survives?**

| Policy | Elba | Stone (proposed) | Lost |
|---|---|---|---|
| Operation restriction | Static | Static: no ambient authority, method allowlist, no reflection | No |
| Pure helpers | Static | Static: no capability parameters or captures (with L1) | No |
| Provenance (`Trusted`, `Verified`) | Static | Runtime seal check at the sink (sound); 0-CFA at admission for delegated bodies (R23) | Partly: may fail after earlier effects outside delegated bodies |
| Containment | Runtime | Runtime | No |
| Information flow | Runtime | Deferred | Deferred |
| Child ceiling | Static | Runtime walk at a pass-style boundary (exact, because data is copied); 0-CFA for delegated bodies | No |
| Budgets | Not modeled | Runtime, metered, scoped | Gain |

**Q4. Sessions, cwd, and `cd`.**
Separate the grant from the position. The host grants a session root. `cwd`
is a position inside it, and `cd` navigates anywhere under the root, so it is
not narrowing. Leaving the root is a loud error. This matches Elba's shipped
RIO.

The session is a vat (R6, from review; replaces draft v2's non-persistence
rule). The host fixes the session ceiling when the session starts.
Capabilities may persist in session bindings, stored as references into the
host's handle table, never as serialized data. The ceiling is checked at
three points:

- **Injection**: grants for a command must be within the ceiling.
- **Restore**: when a session is reloaded, each persisted capability is
  re-resolved by the host and dropped (with a diagnostic) if it is outside
  the ceiling or no longer exists.
- **Child boundary**: a child attempt gets session bindings only through its
  arguments (R8).

Draft v2 worried that a later command admitted under a narrower grant could
pick up a capability bound earlier. Under the vat model, a narrower command is
a child attempt, not a command in the same session.

`last_result()` and `state()` return data today (JSON and a runtime state
record) and must keep doing so.

**Q5. Silent Python divergences.**

| Idiom | Stone today | Fix |
|---|---|---|
| `x or "default"` | `True` | L3 |
| Mutating a list parameter or loop item | No effect on the caller | L2 |
| `a = xs; xs.append(1)` | `a` unchanged | L2 |
| Helper reads an unpassed name | Sees caller locals | L1 |
| `f = lambda: x; x = 2; f()` | Old value | L1 |
| `str(None)`, `str(True)`, `str(1.0)`, `round(2.5)` | `""`, `"true"`, `"1"`, `3.0` | L4 |
| Bare `except:` | Accepted | Parse error (R15) |
| `except Exception: pass` around effects | Swallows silently | Admission error (R14); swallow counter (R13) |
| `try: ... finally: ...` | "finally is not supported yet" | Prescriptive error naming budget scopes and host cleanup (R18) |
| `def f(x, acc=[])` | Rejected, generic message | Prescriptive message (R11) |
| Argument mutated by a child attempt | — (no such spawn yet) | Copy at boundary, like `multiprocessing` (R10) |
| `open("x")`, `os.path.join`, `Path("x")` | Ambient file access | L7: absent, error shows the capability form |
| `root + "/x"`, `f"{root}/x"` passed to file APIs | — | `Dir + str` is a loud error; strings are never authority |
| Pydantic `Field(...)`, validators, `.model_dump()` | — | Absent and loud, naming the supported subset |
| `run = 5` | Misleading "Stone workflow syntax error: run target name `= 5`" from textual `run name` preprocessing | Prescriptive error, or treat as assignment (not from review) |

## 9. Codebase Collisions With The Review

Checked against `crates/waymark-runtime/src` at `c68abb2`.

### 9.1 "The host already knows every handle" (RQ4): partly true

- **Tracked**: open file handles (`insert_file`, per scope) and attempt
  scopes (`EvalState.attempt_scopes`, closed by `close_open_attempt_scopes`
  at the end of evaluation). Gateway-owned runs are tracked by the Gateway.
- **Not tracked**: daemons. `start_daemon` and `stop_daemon` exchange plain
  records, and `stop_daemon_call_values` (`linux_tools/process.rs:619`)
  accepts "a daemon handle or pid" through `value_to_daemon_pid`. A forged
  record or a bare integer stops any process the user can signal. Local mode
  keeps no daemon registry, so nothing is stopped when an attempt ends.
- **Consequence**: R19 needs a host-side obligation registry for daemons,
  with unforgeable handles. In the controlled profile, `stop_daemon(pid)` is
  absent. The default profile keeps it for compatibility.

### 9.2 "Session as a ceiling-bounded vat" (RQ1): structure exists, ceiling does not

- `StoneSession` (`stone_functions.rs`) persists `locals`, `functions`,
  `context`, and `next_transition_id`. `update_from_root_scope` keeps locals
  that pass `is_session_persistable`, which admits callables whose captures
  are persistable. Function definitions always persist.
- `AttemptScope` values are not persistable today. There are no capability
  values yet, so nothing authority-bearing persists, but nothing would stop
  one once capabilities exist.
- `last_result()` reads `WAYMARK_LAST_RESULT_JSON`: data only. `state()`
  returns a runtime-state record plus Gateway environment state: data only.
- **Consequence**: add a session ceiling set by the host at session start,
  and checks at injection, restore, and child boundaries (Q4). Persist
  capabilities as host-table references.

### 9.3 "Helpers receive raw handles" (RQ1): they do not, but they share the evaluator

- Helpers receive one `event` record (data) and return observation records
  (data). No capability crosses today.
- They run in the **same evaluator** as the agent program.
  `invoke_stone_helper_function` pushes a scope on the agent's scope stack
  and merges the helper's functions into the agent's function table
  (`stone_eval.rs`, about lines 6320-6410). Under today's dynamic scoping, a
  helper can read the agent program's locals, and an agent `def` with a
  helper function's name can collide with it.
- Helpers are loaded first from `<cwd>/.stone/helpers`, which is inside the
  agent's writable workspace. An agent can plant a helper that runs on later
  commands. In the default profile this is no escalation, because helpers and
  the agent have the same ambient authority. In the controlled profile,
  where helpers run with host authority, it would be a confused deputy.
- **Consequence**: helpers run in an isolated evaluator state (R7). In the
  controlled profile, helper directories inside the agent's write grant are
  skipped with a diagnostic (R7a). That changes the documented search path
  for the controlled profile only, so `docs/STONE_HELPERS.md`, `README.md`,
  `AGENTS.md`, and tests change with it.

### 9.4 Other findings

- Bare `except:` is accepted today.
- `except Exception: pass` silently swallows today.
- `finally` is rejected: "finally is not supported yet".
- `def f(x, acc=[])` is already rejected at definition
  (`stone_ast.rs`, "mutable default parameter values are not supported").
- `def read_text(p)` shadows the builtin, and the definition persists in the
  session.
- Exceptions bound by `except Exception as e` are JSON strings, so R2 holds
  trivially today. Typed exceptions (L6) must keep it.
- No frame or traceback objects are exposed (R3 holds).
- Attempts are started by id (`attempt_start(attempt=...)`), not as
  function-plus-arguments. `scope.start(f, ...)` and `delegate` are new
  surface, so R8 and R10 change no existing semantics.

## 10. Performance Requirements On The Design

- L1 and L5 give the VM what it needs: locals in registers and records with
  fixed slots.
- L2 removes copy-on-write bookkeeping and matches Lua's table model. Copies
  happen only at pass-style boundaries (child start, `delegate`, sealing),
  which are rare and already expensive.
- L18 (frozen builtins) lets the compiler resolve builtin calls statically
  unless a name is shadowed in lexical scope, which L1 makes visible at
  compile time.
- Explicit effects (L7, L13) mean pure Stone code has no hidden host
  interaction and can be compiled aggressively. Effects are host calls.
- Capability checks are a rights test plus path canonicalization, on
  I/O-bound operations only. The boundary walk (R24) costs time proportional
  to the argument graph, at boundaries only.
- Budget metering is a counter update on effect calls; budget scopes add a
  scope-stack check on those calls only.
- Per-command admission stays a linear pass with no solver (L12). 0-CFA runs
  only on delegated bodies.
- `output=` schemas compile once per record type, and validation runs in
  Rust.

## 11. Hypotheses And How To Test Them

| ID | Hypothesis | Test |
|---|---|---|
| H-1 | Contracts raise task success and reduce derailing compared with V14 conventions | Port `standard_agent_control` to the proposed surface. Compare success, derail modes (unsupported finish, test tampering, budget overrun, stuck loops), and source size on the existing task suites. |
| H-2 | Capabilities as values plus pass-style boundaries give the child ceiling without inference | Write-up of the ocap argument; adversarial delegation tests, including post-spawn mutation of a passed container |
| H-3 | Sealed `Verified` cuts unsupported-finish failures like `Trusted` cut citation hallucination | Count finish attempts rejected by the verifier, and false successes, versus V14 |
| H-4 | Dropped (R9). Capabilities are allowed in containers. | — |
| H-5 | `/` beats `//` and `.sub()` for narrowing on the target models | Authorship eval (`host/bench/` harness) |
| H-6 | Accepting `BaseModel` as an alias raises the first-pass rate without importing other pydantic habits | Authorship eval |
| H-7 | Target models write level-A bodies under capability signatures with about 1 repair or fewer, including budget scopes and copy-at-boundary | Authorship eval on 6-10 realistic tasks |
| H-8 | Optional outcome prediction, host-diffed, reduces derailing beyond declared writes alone | Ablation arm 3 versus arm 2 (11.1) |
| H-9 | Enforced declared writes do not lower task success, or lower it less than they reduce derailing | Ablation arm 2 versus arms 2r and 1 (11.1); decides D-1 by measurement |
| H-12 | Target models can predict a command's writes well enough to declare them: high recall against observed writes, with misses concentrated in a few incidental paths that one repair fixes | Arm 2r: per-call precision and recall of declared versus observed writes, by command family |
| H-10 | Target models produce well-formed `writes`/`expect` with near-zero errors when they are structured-output fields | Measured within the ablation |
| H-11 | Rejecting `pass` handlers and bare `except` costs about one repair and does not push models toward worse patterns (for example `except Exception as e: e`) | Authorship eval; inspect handler bodies after repair |

### 11.1 The Declared-Effects Ablation (Set Up, Not Run)

From review (RQ6). The setup is a prototype deliverable; running it waits for
your go-ahead and model access.

- **Models**: Qwen3.6-27B and Claude Haiku 5.5 (from review). GPT-6 Luna as
  an optional third if we have access.
- **Tasks**: the existing standard-agent task suites behind
  `host/bench/eval_standard_agent_*.py`, so results compare with V14.
- **Arms**, one controller with four action schemas and runtime flags:
  1. No declarations: `Run` has no `writes` and no `expect`; S2 allow-and-report.
  2r. Declared writes, report only: `writes` required; undeclared writes kept
     and reported (R27a, not from review).
  2. Declared writes, enforced: `writes` required; undeclared writes discarded
     with `UndeclaredEffect` (D-1).
  3. Declared writes plus outcome prediction: arm 2, plus `expect` required
     in the schema, mismatches reported and kept in history.
- **Metrics**: task success; tokens per task; loop repetition (same action
  signature after a failure); repairs per action (H-10); undeclared-write
  refusals; false "done" claims rejected by the verifier.
- **Write-prediction accuracy** (H-12), in arms 2r, 2, and 3: per call,
  precision and recall of declared versus observed writes (observed before
  any discard), grouped by command family, plus the most common missed
  paths. Arm 1 also records observed writes, as the baseline for what
  commands actually touch.
- **Off-canonical-call rate**: the review's metric, from Toolathlon, needs a
  canonical tool path per task. Our suites do not have one. I propose
  steps-to-recovery after the first failed call as a proxy. This is a
  substitution, not the review's metric.
- **Prediction from the review**: arm 2 wins on cost-adjusted success, arm 3
  on raw success for 27B+ models.
- **Script**: `host/bench/eval_stone_declared_effects_ablation.py`, following
  the existing `eval_*` pattern, with a `--dry-run` that validates arms and
  schemas without model calls.

## 12. Uncertainties

- No experiment has been run for any proposal here.
- The Gateway repo (`../waymark-gateway`) was not reviewed. How `Exec.within`,
  `Dir` grants, `Verifier` attempts on read-only snapshots, and per-call
  overlay layers map onto the Gateway is unresolved. D-1's mechanism depends
  on the per-call layer being cheap.
- Local mode without an overlay cannot discard a call's writes cheaply.
  Enforcement there needs read-only mounts or is limited to capability
  methods.
- `Verified` is only as strong as its verifier. The claim field makes weak
  verifiers visible; it does not make them strong.
- The utility cost of capability confinement is unmeasured. Elba reports
  policy-versus-no-policy only for IFC.
- The compatibility cost of L1-L4, L6a, and L18 is unmeasured.
- Whether target models handle copy-at-boundary without confusion (H-7).
- Whether dynamic-extent budget scopes surprise models that expect budgets
  to be values (H-7).
- Model-predicted expectations can be wrong in either direction. A too-weak
  expectation (`exit` in `[0, 1]`) gives false comfort. The verifier
  remains the authority on task success.

### 12.1 Source Check Of The Review

A separate pass checked the review's key quotes against the primary sources
(downloads under the session scratchpad). Most hold. Corrections that matter
here:

- **Joe-E** (NDSS 2010, Sections 4.1 and 4.3): "no code is able to execute
  once an error is thrown" rests on the bans on catching `Error` and on
  `finally`. The finalizer ban has a different reason (encapsulation,
  Fig. 5). R16 and R18 cite only the first two.
- **Agoric `passStyleOf`**: errors may carry other passables, so Agoric's
  errors are not capability-free. R2 follows Joe-E's immutable exceptions,
  which are stricter. The page also does not state "transitively frozen" or
  "send throws" as rules; they appear in examples. R10 rests on the
  post-send mutation example, which the page does state.
- **bpo-32528**: the quoted comment is from a commenter (bmerry), not the
  reporter, and the same comment reports real breakage: code that used
  `except Exception` to unwind and re-raise. Section 5.5 addresses that
  pitfall.
- **Scope ownership of cancellation** is Trio's documented semantics, not
  AnyIO's page. Trio's shield applies only to outside cancellations. R17
  cites Trio.
- **Weimer & Necula** (OOPSLA 2004): compensation stacks are first-class and
  still run when a heap-allocated stack is finalized. The paper makes
  finalizer-like cleanup more useful; it does not discard it. The 800-mistake
  figure comes from an unsound analysis with filtering and manual checks.
  R19 does not depend on either point.
- **PreAct** (2024): predicts observations "at a higher level", not exactly
  the next one. Gains are about 20% on household tasks and 12% and 6% on OS
  and database tasks, no gain on lateral-thinking puzzles, all on GPT-3.5
  and GPT-4. Evidence for our target models is indirect, which is why R26
  keeps prediction optional and R27 measures it.
- **Not checked by that pass**: the 2025-2026 empirical papers (Ren et al.,
  Seeker, Li et al., Guo et al., ImpossibleBench, EvilGenie, Lee,
  Near-Miss, AgentTether, the CoT scaling studies) and Pony. Decisions resting
  mainly on them (R13-R15, R21, R25-R26) are marked as hypotheses in
  Section 11.

Before implementing any item that changes existing semantics, I will read the
cited sections myself, not only this summary (Section 14 lists them per
item).

## 13. Roadmap (Ordered By S3)

Each phase is additive. Later phases do not change the language that earlier
phases ship. Every phase ends with the conformance corpus and authorship
evals updated.

### P1. Task Completion

- L1-L4 semantics fixes.
- L5 records and `model.ask(..., output=T)`.
- L6 typed exceptions with the three tiers; L6a (bare `except`, `pass`
  handlers, swallow counter); L6b prescriptive `finally` error.
- `ExpectationFailed` with full result (R12); optional `expect=` (R26).
- Mandatory declared writes for `Exec` (R25), behind D-1.
- L17 budget scopes and `model.limit`.
- L9 modules with frozen module values.
- Ablation harness set up (R27).

Exit: the standard controller ported to P1 features, with task success equal
or better on the existing suites and a smaller source.

### P2. Drift

- L8 sealed `Verified[T]` and `Evidence`; verifier in its own attempt on a
  read-only snapshot (R20-R22).
- Capability values for files and processes (`Dir`, `File`, `Exec`) with
  narrowing, alongside the existing builtins.
- Boundary walk (R24), pass-style `scope.start` (R8, R10).
- Host obligation registry, including daemons (R19).
- Helpers in an isolated evaluator (R7).
- Runtime stuck signal from repeated mismatches.

Exit: measured drop in false "done" claims, test tampering, and stuck loops
(H-1, H-3).

### P3. Security

- L7 controlled profile: ambient effect builtins absent.
- PolicyViolation and ContractViolation as fatal (R16, R16a) once there is
  authority to violate.
- Session vat with ceiling (R6); helpers not loaded from agent-writable
  directories (R7a).
- Irreversible effects only through host-granted capabilities, with approval
  handlers (L13).
- `model.delegate` with explicit grants (level B); 0-CFA admission check for
  delegated bodies (R23).
- Information-flow control (taint and declassify) reconsidered, with
  measurements.

Exit: adversarial delegation and injection tests, plus Gateway mapping for
`Exec.within`, grants, and per-call layers.

## 14. What Changes In The Prototype

Compared with the draft v2 plan. Nothing here has started. "Existing" marks
items that change current Stone behavior; those wait for the source reading
in the last column and for the compatibility measurement.

| # | Item | Change from draft v2 | Existing | Read before implementing | Phase |
|---|---|---|---|---|---|
| 1 | Bare `except:` is a parse error | New | **Yes**: accepted today | Joe-E §4.3; PEP 352; Ren et al. 2023 | P1 |
| 2 | `pass`-only handler for `Exception`/`ToolError` is an admission error | New | **Yes**: legal today | Ren et al. 2023; Seeker 2024 | P1 |
| 3 | Swallowed-`ToolError` counter in trace and `last_result()` | New | Additive field | — | P1 |
| 4 | `except BaseException`, `except PolicyViolation`, `except BudgetExhausted` rejected | New | No (names do not exist) | Trio cancellation docs | P1 |
| 5 | `ExpectationFailed` fields `.expected/.observed/.diff/.result` | Was "carries the full observation" | No | — | P1 |
| 6 | `Run.writes` mandatory; undeclared writes discard the call's layer and raise `UndeclaredEffect`; a report-only flag for arm 2r | Was allow-and-report (S2) | No (new API) | D-1 (provisional); PreAct; Near-Miss | P1 |
| 7 | `expect=` optional; exit status moved into it; `Tests` row removed | Was part of every action | No | PreAct | P1 |
| 8 | `with budget(...) as b:` with Trio semantics; `grace=`; `model.reserve` removed | Was `limit` + `reserve`, exhaustion propagating | No (new API) | Trio "Cancellation semantics"; bpo-32528 thread | P1 |
| 9 | `finally` stays absent; error becomes prescriptive | Was unaddressed | Message only | Joe-E §4.3 | P1 |
| 10 | Mutable default argument error becomes prescriptive | Was unaddressed | Message only | — | P1 |
| 11 | L9 module values deep-frozen at import | New | No (no modules yet) | Joe-E §4.2 | P1 |
| 12 | Ablation harness `host/bench/eval_stone_declared_effects_ablation.py`, four arms, write-prediction accuracy, dry-run only | New | No | PreAct; Lee 2026 | P1 |
| 13 | `Verified` as a sealed box with value, verifier id, claim, nonce; verifier in its own read-only attempt; no `critic=` | Was an opaque tag minted with an optional critic | No (new type) | Morris 1973 §6-7 | P2 |
| 14 | Boundary walk component with five call sites | New | No | Taly et al. 2011 §II.B, §V | P2 |
| 15 | Pass-style `scope.start(f, **args)`: copy data, share handles, closure rule | Was "capability arguments", no copy rule | No (attempts start by id today) | Agoric "Far, Remotable, and Marshaling" | P2 |
| 16 | Daemon obligation registry and unforgeable handles; host stops daemons on attempt exit | New | **Yes**: `stop_daemon(pid)` behavior in controlled profile | Weimer & Necula §5-7 | P2 |
| 17 | Helpers in an isolated evaluator state | New | **Yes**: helpers share scope and function table today | `docs/STONE_HELPERS.md`; Taly et al. §VI.A | P2 |
| 18 | Builtin table frozen; helper and agent function tables separate; local shadowing allowed | New | **Yes**: agent `def` of a builtin name persists in the session | Taly et al. §II.B | P2 |
| 19 | Exceptions carry data only (walk at `raise`) | New, with L6 | No (exceptions are strings today) | Joe-E §4.3, Fig. 6 | P2 |
| 20 | Session ceiling; capabilities persisted as host-table references; checks at injection, restore, child start | Was "capabilities not session-persistable" | **Yes**: `StoneSession` persistence | Miller 2006 (vats); Agoric docs | P3 |
| 21 | Helper directories inside the agent's write grant skipped in controlled profile | New | **Yes**: documented search path (controlled profile only) | `docs/STONE_HELPERS.md` | P3 |
| 22 | `PolicyViolation`/`ContractViolation` fatal, no agent code afterward | Was "outside `Exception`, catchable explicitly" | No (no violations exist yet) | Joe-E §4.3 | P3 |
| 23 | 0-CFA admission check for delegated bodies | New | No | Taly et al. §V | P3 |
| 24 | H-4 test dropped; capabilities allowed in containers | Removed | No | — | — |

Items 1, 2, 16, 17, 18, 20, and 21 change existing behavior. For each, the
plan is: read the cited sections, measure the impact on examples, help
examples, task specs, and the conformance corpus, then ask before changing the
interpreter.
