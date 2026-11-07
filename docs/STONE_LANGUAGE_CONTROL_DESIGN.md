# Stone As An Agent Control Language (Draft)

Status: draft v3.1 (2026-11-07). Decisions S1-S3 are settled. Draft v3 adds
the decisions from the literature review (Section 3). Each one is marked
**from review** or **deviates from review because ...**. One review decision
(mandatory declared writes) conflicted with S2. D-1 (Section 3.1) resolves it
after an LLM pilot (11.2): effects fall into three tiers, and models declare
only irreversible effects. Nothing is
implemented. Items marked **H-n** are hypotheses to test, **L-n** are language
decisions (Section 7), and **R-n** are review decisions (Section 3).

Review: "Literature review: language-level control for the Waymark agent
shell" (2026-11-06). Its questions are cited here as **RQ1-RQ6**, to keep them
apart from the brief's Q1-Q5 (Section 8). Its quotes were checked against the
primary sources (Section 12.1).

## 1. Goal

**Core principle: make computers easy for LLMs to use.** Syntax sugar,
formal contracts, the strongly typed runtime, and performance all serve this
goal. Where a Python convention helps LLMs, Stone keeps it. Where it hurts,
Stone deviates. Each decision below is judged by that test, and contested
cases are settled by measuring models (authorship evals, pilots), not by
fidelity to Python.

Stone is the language Waymark agent loops are written in. Earlier work made
Stone easy for LLMs to write without retraining and gave it a strongly typed
runtime. This design makes Stone good at agent control:

- **Authority**: what a loop and its subagents may do.
- **Outcome**: what the loop must establish before it may claim success.
- **Resources**: how much model, tool, and wall-clock budget each part gets.
- **Lifecycle**: which resources must be released before a scope ends.
- **Recovery**: which failures a loop may handle, and which it must not
  swallow.
- **Syscall contracts** (per tool call). The Gateway is the kernel, and
  every call that crosses it is a syscall: `Exec`, host tools, RPCs, and MCP
  tool calls. Each syscall has the following, all checked at runtime:
  - an effect signature;
  - a precondition (`requires`: the grant covers it);
  - a postcondition (`ensures`: the observed effects fit the signature, and
    any optional expectation holds).
  
  Recoverable effects are reported, not declared. Models declare only what
  cannot be undone (5.8). MCP is a good interface but has no formal
  contracts. Adding those contracts is what lets language-based agent
  control (Section 2) govern tool calls.

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
- Python is a means, not a goal (see the core principle above). Keep Python
  where it helps LLMs, and deviate where it hurts. Every deviation is loud:
  - Stone may reject a Python construct, such as bare `except:`,
    `pass`-only handlers, or calling a call's result, with an error that
    names the fix.
  - A program Stone accepts never quietly behaves differently from what a
    model trained on Python would expect.
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
  **D-1 (3.1) extends S2 into three effect tiers and applies them to every
  syscall, not only file operations.**
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
| R25 | ~~Declared writes are mandatory and enforced at the capability layer.~~ Replaced by R25a-R25c. | **Deviates from review** (RQ6, the brief's Q6 directive) **because** the pilot (11.2) found that per-path write declarations caught no drift. Every rollback was a toolchain by-product or a legitimate edit, and the capable models over-declared (`/app`), which defeats enforcement. Each declaration is also an escalation with a cost (D-1). | 5.8 |
| R25a | Effects fall into three tiers. (1) Recoverable effects are not declared; they are observed and reported on every result. (2) Critical recoverable paths are protected by the grant and enforced by the Gateway. (3) Irreversible effects are denied unless granted and declared. | Mine (D-1), decided with you after the pilot. Keeps the review's point that declarations are policy, applied only where a declaration buys safety. | 3.1, 5.8 |
| R25b | Every Gateway call (`Exec`, host tool, RPC, MCP tool) is a syscall with a Gateway-owned effect signature, `requires`, and `ensures`. For a typed syscall, the call itself is the declaration. A model declares effects only for opaque syscalls (`Exec`), and only in tier 3. | Mine. Generalizes RQ6's per-call contracts from shell commands to all RPC. | 5.8 |
| R25c | MCP tool annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`) may seed a tool's effect signature but never decide it. | Mine. The MCP spec itself calls these untrusted hints unless the server is trusted. | 5.8 |
| R26 | Outcome prediction (`expect=`) is optional, typed, and host-diffed, on any syscall. The diff is reported only on a mismatch, and mismatches are kept in history. | From review (RQ6), extended to every syscall (R25b) | 5.8 |
| R26a | Expected exit status belongs to the optional prediction, not to any declaration. | **Deviates from review** (which lists exit status with the mandatory declaration) **because** exit status is an outcome prediction. | 5.8 |
| R27 | Set up, but do not run, the ablation. Reduced to two arms: tiers only, and tiers plus `expect`. | **Deviates from review** (three arms) **because** D-1 removed declared writes. The write arms ran as the pilot instead (11.2). | 11.1 |
| R27a | ~~Add a fourth arm: writes reported but not enforced.~~ Done as the pilot's report arm (11.2). | **Deviates from review** (three arms); the pilot has run it. | 11.2 |

### 3.1 Decisions

**D-1. Which effects must a model declare?**
**Decided (2026-11-07), after the pilot (11.2): three tiers. A model declares
only irreversible effects of opaque syscalls.**

Draft v3 started from R25: declared writes are mandatory, and undeclared
writes are refused. That conflicted with S2, under which undeclared file
writes on the overlay are allowed and reported. The pilot ran four models
(Haiku 5.5, GPT-6 Luna, Gemini 3.8 Flash, Qwen3.8-27B), and per-path
declarations bought almost nothing:

- Every rollback was a toolchain by-product (`__pycache__`, `.o`) or an edit
  the model meant to make. None was drift.
- The capable models declared `/app` or `/work` before running tests or
  builds. A declaration that broad turns enforcement into a rubber stamp.
- The cost was low (no measurable drop in task success), but it was not
  zero: an extra field per action, rollbacks, and repair turns.

Like a mobile permission prompt or an OS context switch, every declaration
is an escalation with a cost. The design therefore keeps declarations to a
minimum. A model declares an effect only if the Gateway cannot infer it from
the syscall's signature **and** cannot undo it.

| Tier | Effects | Who decides | Enforcement | On violation |
|---|---|---|---|---|
| 1. Recoverable | File changes inside the attempt's transaction; reads; processes that end with the call | Nobody declares | Transaction (overlay, copy, or worktree). The observed diff is reported on every result. Rollback to a checkpoint is a Gateway operation the harness can request. | None; observed and reported |
| 2. Critical, recoverable | Writes to protected paths: `tests/`, `.git`, CI config, credentials, verifier inputs | The grant (harness or host), never the model | The Gateway mounts the paths read-only for `Exec`; capability methods fail containment | `Exec` sees `EROFS`, and the result names the protected path; a capability method raises `PolicyViolation` (5.5) |
| 3. Irreversible | Network egress, push, publish, send, deploy, external API writes, processes that outlive the call, signals to processes outside the transaction | Typed syscalls: the effect signature, so the call is the declaration. `Exec`: the model declares (`network=[...]`, `irreversible=[...]`). | Denied unless the grant allows it and the call declares it. Approval handler (L13) where the host requires one. | Blocked before it happens; `UndeclaredEffect` (`ToolError`) names the attempt and the fix |

Notes:

- **Tier 2 is where drift protection lives.** "Do not edit tests" is a
  property of the task, so the harness states it once, in the grant. It
  should not depend on a model scoping each call correctly.
- **Tier 3 is not Linux-specific.** It applies to every syscall. For typed
  RPC and MCP calls, the Gateway checks the call against the tool's effect
  signature (R25b). Only `Exec` is opaque, and only there does the model
  name the effects. On Linux, the Gateway can enforce `Exec` declarations
  per call with eBPF (cgroup `connect`/`sendmsg` hooks, BPF LSM). Other
  substrates use a network namespace with a Gateway proxy, or deny by
  default.
- **Default profile (today's scripts): S2 unchanged.** Tier 1 is S2. Tiers 2
  and 3 need a grant, so they apply in the controlled profile.
- **Deferred.** How predictable effects are for models was discussed but not
  settled. The pilot is weak evidence: the tasks were short, and recall was
  89-100%. Under D-1, the question matters only for tier-3 declarations on
  `Exec`.

**What the Gateway provides today** (read-only review of
`../waymark-gateway` at `60e9d6f`, 2026-11-06):

- Isolation is one overlay (or full-copy, or git-worktree) transaction per
  **attempt**, not per command. Every `run(...)` is a `docker exec` into one
  long-lived container per transaction.
- A per-operation private overlay is designed in the Gateway docs but not
  built.
- A file diff exists, but it is cumulative for the transaction (base versus
  merged tree, full-tree hash). Stone receives only its text form
  (`env_diff`).
- Checkpoints are full copies, about 300 ms per 50 MB of workspace.
- There is no per-command read-only mounting (no Landlock, seccomp, or bwrap)
  and no network control.
- Writes outside the workspace mount (`$HOME` caches, `/tmp`) are not
  observed.
- Background jobs from earlier commands write into the same tree, which
  breaks attributing writes to one command.

Gateway work for D-1, ranked by effort (Gateway changes live outside this
repository):

1. Pass the structured diff to Stone, and diff before and after each
   command. This is the tier-1 report. Small.
2. Checkpoint and restore of changed paths, so the harness can request a
   rollback. Small to medium; the copy machinery exists.
3. Tier 2: read-only bind mounts for the grant's protected paths inside the
   transaction's container. Small to medium.
4. Tier 3 for `Exec`: network off by default, with per-call egress
   allowances enforced by eBPF on Linux. Medium.
5. Effect signatures for host tools, RPC, and MCP tools in a Gateway-owned
   manifest (R25b, R25c). Medium.
6. A policy for processes that outlive the call (tier 3). Medium.

A per-command overlay is no longer needed. Tier 1 does not discard per call.
The pilot (11.2) emulated items 1 and 2 at harness level with bubblewrap.

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
  Tier-2 protected paths and tier-3 egress rules (D-1, 5.8) are enforced by
  the Gateway on each `Exec`, whatever argv does.

The leak channels the review lists, and where each is closed:

| Channel | Rule | Today |
|---|---|---|
| Capability inside an exception | R2: walk at `raise` | Caught exceptions are plain dicts; no handles can leak yet |
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
  `e.detail`). Today `except Exception as e` binds a plain dict
  (`{"code", "kind", "message", ...}`), and `str(e)` renders it as JSON.
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

### 5.8 Syscall Contracts: Effect Tiers And Optional Expectations

MCP gives tools a good interface: a name, a typed input schema, and a
result. It has no formal contract. Nothing binding says what a call may
change, whether the change can be undone, what must hold before the call, or
what the caller can rely on afterward. Its tool annotations are explicitly
untrusted hints. Language-based agent control (Section 2) needs those
contracts in order to reason about tool calls at all. This section adds them
uniformly to every call that crosses the Gateway.

The Gateway is the kernel. Every call that crosses it is a syscall:

- `Exec`;
- host tools;
- RPCs;
- MCP tool calls;
- model calls, which are metered by budget scopes (5.3).

Capabilities bound what a function *may* do over its whole run. A syscall
contract states what one call does. It has three parts, all checked at
runtime and none proved. There is no `requires`/`ensures` logic and nothing
to prove; the contract is types, capabilities, and runtime checks.

| Part | Holder | Checked |
|---|---|---|
| Effect signature: which effects the syscall can have, by tier (D-1) | The Gateway, in a manifest it owns (R25b). MCP annotations may seed it but never decide it (R25c). | Statically at admission for literal calls, and before dispatch |
| `requires`: the capability is held, the arguments are within the grant, and any tier-3 effect is granted and declared | Grant plus signature; `Exec` adds the model's tier-3 declaration | Before dispatch; failure means nothing runs |
| `ensures`: the observed effects fit the signature and declaration, plus the optional expectation (`expect=`) | Signature, declaration, and caller | After the call; tier 1 is reported, a tier-3 attempt was blocked, and `expect` raises `ExpectationFailed` |

```python
r = shell.run(["pytest", "tests/test_parser.py", "-x", "-q"], cwd=src,
              expect=Expect(exit=0, stdout_contains="passed"))  # optional
r.effects.observed            # tier 1: what the command changed, always reported

r = shell.run(["pip", "download", "requests"], cwd=src,
              effects=Effects(network=["pypi.org", "files.pythonhosted.org"]))  # tier 3, Exec only

gh.create_pull_request(repo, title=t, body=b)   # typed syscall: the signature says
                                                 # "irreversible: publish"; the grant must allow it
```

#### Minimize what models declare

The model declares an effect only when both are true:

1. The Gateway cannot infer the effect from the syscall's signature. In
   practice that means only `Exec`, whose effects depend on argv.
2. The effect cannot be undone (tier 3).

Everything else is inferred, granted, or observed. A capability method whose
effect is evident (`src.file(p).replace(...)` writes `p`) needs no
declaration. A typed RPC or MCP tool carries its effects in its signature.
Recoverable file changes from `Exec` are reported after the fact.

#### Two authors, one vocabulary

- **Harness code** writes grants (including tier-2 protected paths),
  `effects=`, and `expect=` as record literals. Literals are validated at
  admission: fields exist, and paths and hosts are within the function's
  grant.
- **The model** writes tier-3 `effects` and an optional `expect` as data
  inside its action, through `output=` structured output. Both fields are
  optional and default to "none". The vocabulary is a closed,
  JSON-Schema-expressible record set, emitted with structured or constrained
  decoding.

#### Effect vocabulary (closed)

Declared, tier 3 only:

| Field | Meaning | When omitted |
|---|---|---|
| `network` | `False`, or a list of hosts the call may reach | `False` |
| `irreversible` | Named effect kinds outside any transaction (`push`, `publish`, `send`, `deploy`, `external_write`) | Empty; allowed only with a host-granted capability **and** a declaration |
| `background` | May leave a process running after return | `False` |

Observed, tier 1, reported on every result as `r.effects`: `writes`,
`deletes`, and processes started. These are never declared.

Tier 2 lives in the grant (`Dir(..., protect=["tests", ".git"])` or a
read-only sub-capability), not in the call.

Each effect has a reversibility class: read-only, reversible (workspace
changes in the transaction), or irreversible. This extends the classes in
`STONE_AGENT_PROGRAMMING_DESIGN.md` (`workspace.read`, `workspace.write`
transactional, `model.call` irreversible budget effect). The host reports
which class each substrate provides. Local mode without an overlay has no
rollback, so tier 1 there is report-only (Section 12).

#### Outcome vocabulary (closed, plus one escape; optional)

| Target | Checks |
|---|---|
| Process | `exit` (int or list), `stdout_contains`, `stdout_matches` (regex), `stderr_empty`, `stdout_json` (record type), `max_seconds` |
| File | `exists`, `contains`, `unchanged` |
| Edit | `occurrences` (default exactly 1) |
| RPC / MCP tool | `result` (record type the result must validate against), `status` (success or a named error), `max_seconds` |
| Custom | `check=fn`: a Stone function that admission proves pure (no capability parameters or captures) |

Draft v2's `Tests: passes=[...]` row is removed. Test results that count
toward success go through a `Verifier` (5.2), not a per-call expectation.

Expected exit status is part of this optional record, not of any
declaration (R26a). `expect=` is accepted on every syscall (R26).

#### Safeguards

| When | What the runtime and Gateway do |
|---|---|
| Admission | A literal call is checked against the syscall's effect signature and the function's grant. Literal `effects` must be within the grant. Otherwise `PolicyViolation` before anything runs. |
| Before dispatch (`requires`) | A model-supplied tier-3 declaration must be within the grant; otherwise `PolicyViolation`. A typed syscall whose signature has a tier-3 effect needs a granting capability. Irreversible effects go through the approval handler (L13). |
| During the call | Tier 1: the call runs inside the attempt's transaction. Tier 2: protected paths are read-only. Tier 3: everything not declared and granted is denied (`Exec` network off; eBPF per call on Linux). Timeout comes from `max_seconds`. |
| After the call (`ensures`) | The observed diff and surviving processes are attached as `r.effects`. A blocked tier-3 attempt raises `UndeclaredEffect`. If `expect=` was given, the checks run on the observation. |
| Retry | Only syscalls whose signature says idempotent may be retried automatically |

Failure tiers:

- **Outside the capability grant** (authority): `PolicyViolation`, fatal
  (5.5).
- **Tier-3 effect not declared**: `UndeclaredEffect`, a `ToolError`. The
  effect was blocked, so nothing irreversible happened.
- **Prediction not met**: `ExpectationFailed`, a `ToolError`. It carries
  `.expected`, `.observed`, `.diff`, and `.result` (R12), so a handler never
  needs to re-run the tool to get the output. Re-running would double the
  side effects.

Raising is the default because the most common failure in LLM-written code is
a missing check. A returned `met=False` that nobody tests fails silently. A
raise fails loudly when the model writes nothing, and R13-R15 make the
remaining failure (over-broad handlers) visible.

#### Feedback shape

The model-facing rendering states the fix-relevant facts first. The tier-1
effect report is one compact line, with directories collapsed. The pilot's
report arm showed this on every result without a measurable cost to task
success. A met expectation adds nothing to the context (R26).

```text
run pytest tests/test_parser.py -x -q: expectation NOT met
  exit: expected 0, got 1
  stdout_contains "passed": not found (tail: "FAILED tests/test_parser.py::test_empty - KeyError: 'name'")
```

```text
run make: ok (exit 0)
  changed: src/build/ (14 files), src/.deps/ (6 files), src/config.h
```

```text
run pip install requests: UndeclaredEffect, blocked
  attempted: connect pypi.org:443
  declared network: none; grant allows: pypi.org, files.pythonhosted.org
  fix: add effects network=["pypi.org", "files.pythonhosted.org"], or use the vendored wheel in deps/
```

The structured form (`r.checks[i].expected/actual/met`,
`r.effects.observed`, `r.effects.blocked`) is what the harness reads.
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
    expect: Expect | None = None      # optional outcome prediction
    # No effect fields: file changes are reported (tier 1), tests/ is protected
    # by the grant (tier 2), and this grant allows no network (tier 3).

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
                    obs = shell.run(action.argv, cwd=src, expect=action.expect)
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
 "expect": {"exit": 0, "stdout_contains": "passed"}}
```

What happens next:

- If the command fails, the next observation says which predicted check
  failed.
- Every observation lists what the command changed (tier 1).
- `shell` is bound to `src` with `tests/` protected, so if the command
  writes there, the write fails and the observation names the protected
  path (tier 2).
- If an action names a path outside `src`, it is rejected before running.
- If the command tries to reach the network, the Gateway blocks it and
  raises `UndeclaredEffect` (tier 3).

What the runtime guarantees here, where today's controller needs convention:

| Concern | Contract |
|---|---|
| Edits only in `src` | `Edit` goes through `src: Dir[Read, Write]`; `src.file("../tests/x")` fails containment |
| Tests cannot be "fixed" | `repo` is read-only, and the verifier runs on a read-only snapshot |
| Success needs evidence | Return type `Verified[Report]`; the box is sealed and frozen |
| Room to finish | `grace` holds calls back from the loop's budget scope |
| Budget accounting | Metered; `BudgetExhausted` cannot be caught inside the scope |
| Malformed actions | `output=Action`, repair charged to the budget |
| Commands cannot touch tests or the network | Tier 2 protected paths and tier 3 egress denial, enforced by the Gateway on every `Exec`; tier 1 changes reported on every result |
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
| L14 | Syscall contracts: Gateway-owned effect signatures for every syscall; models declare only tier-3 effects of `Exec` (`effects=`); `expect=` optional on any syscall | Adopt (R25a-R25c, R26; D-1) | Safeguards each syscall and gives checked feedback with the fewest declarations | Checks run on syscalls only; tier-1 diff cost per command is one diff (Section 12) |
| L15 | `ExpectationFailed` and `UndeclaredEffect` are `ToolError` subclasses carrying the full result | Adopt (R12) | Loud by default, recoverable, one handler in loops | None |
| L16 | Custom `check=fn` must be pure | Adopt | Expectation checks cannot have effects | Pure functions are VM-compilable |
| L17 | `with budget(...) as b:` with Trio scope semantics; `model.limit` for children; no `reserve` | Adopt (R17, R17a) | Budget exhaustion cannot be swallowed inside, and is handled at one place | Metering is on effect calls only |
| L18 | Builtin table frozen; one Python namespace for callables and variables; local shadowing follows Python; calling a non-callable that shadows a builtin is a prescriptive error | Adopt (R4, R4a; namespace finding in 9.5) | Confinement without rejecting common code; the shadowing error keeps unification loud | Builtins resolve at compile time unless lexically shadowed (with L1) |

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
- Exceptions bound by `except Exception as e` are plain dicts (data), so R2 holds
  trivially today. Typed exceptions (L6) must keep it.
- No frame or traceback objects are exposed (R3 holds).
- Attempts are started by id (`attempt_start(attempt=...)`), not as
  function-plus-arguments. `scope.start(f, ...)` and `delegate` are new
  surface, so R8 and R10 change no existing semantics.
- Callables and variables live in **separate namespaces** today:
  `round = 2; round(2.4)` returns `2.0`. Python has one namespace.

### 9.5 Compatibility Scan (L1-L4, L6a, L18)

The scan covered 482 parsed units: 65 `.stone` files (about 10.1k lines,
including the standard controller), 227 `help()` examples, 4 task-spec
sources, 58 doc blocks, and 128 Stone strings embedded in `host/bench` and
`host/mcp`. It used static analysis with scope resolution and a call graph,
after checking today's behavior on the binary. Scripts and results are in the
session scratchpad (`compat/`).

| Change | Sites that change behavior | Notes |
|---|---|---|
| L1 lexical scoping | 0 in executed code (424 functions) | No function reads a caller's local; no closure depends on late binding. State carried between session commands is not visible to a static scan. |
| L2 reference semantics | 0-2 | All 20 call sites that pass a mutated parameter use `state = f(state, ...)`. About 10 alias-then-mutate lines, for example `final = action.input` in the standard controller; harmless today because that path returns. Host-returned values must be fresh objects under L2. |
| L3 operand `or`/`and` | 0 | No `x or default` idiom anywhere; all value-position BoolOps have bool operands |
| L4 scalar `str()`/`round()` | 2 | Two `round(x, 2)` calls that can differ on half-way cases (`round(2.675, 2)` is 2.68 today, 2.67 in Python) |
| L4 `str()` of records | 11 | See decision D-2 |
| L6a bare `except`, `pass` handlers | 0 of 15 handlers | But the tooling suggests bare `except:` (below) |
| L18 builtin shadowing | 0 calls today, 71 latent bindings | See namespace unification below |

Findings that change the plan:

- **The correction engine recommends bare `except:`.**
  `stone_correction.rs:195-197, 918` suggests it as a repair. So do
  `stone_ast.rs:1069,1076`, `lib.rs:1512`, `commands.rs:1805`, and
  `docs/STONE_LANGUAGE.md:343-350`, with a test at `stone_eval.rs:20919`.
  L6a must change all of these in the same change. The replacement
  suggestion is `except Exception as e:`.
- **Namespace unification is the largest silent risk.** Under Python's single
  namespace, a variable named like a builtin hides the builtin. The corpus
  binds builtin names 71 times (`state`, `round`, `head`, `tail`, `first`,
  `items`, `stage`). No scope that binds one also calls that builtin, so the
  risk is latent in files. It is real in sessions: the MCP tool examples
  (`host/mcp/stone_mcp_server.py:283,338,386`) bind `state`, `first`, and
  `run` at top level. Those persist, so a later `run([...])` or `state()` in
  the same session would break. To stay "absent or loud": calling a
  non-callable value whose name shadows a builtin is an error that names the
  binding site and suggests renaming it. The MCP examples are renamed in the
  same change.
- **`str()` is internally inconsistent today.** `str(1.0)` is `"1"`, but
  `str([1.0])` contains `1.0` and renders `None` as `null`. `format()` and
  `print` share the scalar rules.

**D-2. Does L4 cover `str()` of dicts and records? Open.** Containers render
as JSON today by design: structured text was judged better for LLMs than plain
strings. Today's scalars follow neither convention: `str(None)` is `""` and
`str(True)` is `"true"`.

The proposal separates two uses:

- **What the model reads**: `emit`, returned values, observations, error
  envelopes, and `print` of a container. These stay JSON. This is where the
  structured-I/O argument applies, and no program logic depends on the
  rendering.
- **What `str()`/`repr()` return inside a program**: these follow Python
  (`"None"`, `"True"`, `"{'a': None}"`). Their results feed comparisons,
  concatenation, and parsing in model-written code, where a silent divergence
  causes bugs (`if str(flag) == "True":`).

The 11 controller sites that build model-facing text with `str(error)` switch
to `json_dumps(error)`, so the model still reads JSON there.

The alternative is JSON as `str()`'s convention for containers, with scalars
made consistent (`null`, `true`). It is coherent, but a quiet divergence in
program logic.

Unmeasured: whether target models read JSON better than Python repr in
observations. That is a cheap authorship-eval A/B if it matters.

### 9.6 M0: Executable Probes And Source Reading (2026-11-07)

`host/check_stone_semantics_compat.py` turns the 9.5 scan into measurements
that run on the binary.

- **`probes`** runs 44 small programs in `waymark eval` and in CPython. The
  target is CPython for L1-L4, and an explicit rejection for L6a, item 4,
  and the L18 shadowing error. A change is done when all of its probes
  match.
- **`snapshot` and `diff`** record and compare the results of the
  executable corpus: 52 example and reference scripts and 159 help
  examples, run in a scratch copy. The baseline is 211 units (169 ok, 42
  deterministic errors), with 0 unstable across two runs. After each change,
  `diff` lists exactly which units changed.

Corpus coverage is partial:

- 21 scripts need a live Gateway.
- About 13 are library fragments that cannot run alone.
- The standard controller and the libraries are covered only by the static
  scan (9.5) and the 475 runtime unit tests.

Probe results today (matching target / total):

| Change | Match | What diverges |
|---|---|---|
| L1 | 3/8 | **Stone scopes dynamically.** A function reads its caller's locals (`g` sees `f`'s `y`). A returned inner function does not capture its environment: it fails with "unknown name", or picks up a later global of the same name (`add(3)` gives 103, not 5). Plain globals and in-function rebinding already match. |
| L2 | 1/5 | Lists and dicts are copied on assignment and on argument passing. Explicit copies already match. |
| L3 | 1/6 | `or`/`and` return booleans. Test contexts already match. |
| L4 | 2/11 | `str(None)` is `""`, `str(True)` is `"true"`, `str(1.0)` is `"1"`, `print(None)` prints nothing, `round` rounds half up on the decimal text (2.675 becomes 2.68; 0.5+1.5+2.5 rounds to 6, not 4). Float repr already matches. |
| D-2 | 0/4 | Containers render as compact JSON (`[1.0,null,true]`). |
| L6a | 1/3 | Bare `except:` and `pass`-only handlers are accepted. |
| Item 4 | 1/1 | `except BaseException:` is **already rejected** ("unsupported exception handler"). |
| L18 | 3/4 | `len = 5; len([1])` still calls the builtin. `def len` and local shadowing already behave like Python. |

Two limits found that are outside the plan:

- **A call's result cannot be called** (`make(2)(3)`: "unsupported call
  target").
- **Callables cannot be stored in lists or dicts** ("task-owned runtime
  value cannot cross this boundary", `stone_runtime_value.rs:184`).
  Containers are Nu `Value`s, and runtime values (callables, files,
  attempts) cannot live inside them.

This data-model split is also the root of L2's copy semantics. It
constrains R9 (capabilities in containers) and R8 and R10 (pass-style
boundaries), so L2 and R9 are one piece of M4 work, not two.

**Source reading for items 1, 2, and 18.**

- **Item 1 (bare `except:`).** Three sources bear on it:
  - Joe-E §4.3 rejects any `catch` whose *type* is `Throwable` or `Error`,
    so no code runs after an error. The rule is on the caught type, not on
    syntax.
  - PEP 352 discourages bare `except:` and `except BaseException:` for the
    same reason: terminating exceptions must propagate.
  - PEP 760 (2024) proposed making bare `except:` a SyntaxError and was
    withdrawn. The PEP gives no reason; Python has a large installed base
    and Stone does not. Its one rejected counter-example, a catch-all that
    aborts a transaction and re-raises, is the host's job in Stone (R19).
    Its suggested rewrite (`except BaseException:`) is banned here, so the
    fix Stone suggests stays `except Exception as e:`.
  
  Consequence: items 1 and 4 are one rule, about which exception types a
  handler may catch, and the tooling fix (9.5) ships with it.
- **Item 2 (`pass`-only handlers).** The cited empirical papers support it
  only weakly.
  - Ren et al. 2023 (ASE, Java, ChatGPT) found that missing handling is the
    dominant failure (81 of 92 tasks), with wrong exception types in 4 and
    try-catch abuse in 5.
  - Seeker (arXiv 2410.06949, Java) reports detection and type-precision
    problems.
  - Neither measures handlers that swallow an error with `pass`.
  
  They support raising by default (5.8) more than banning `pass`. Item 2
  stays, but its justification is visibility (5.5), and H-11 must measure
  its cost.
- **Item 18 (frozen builtins).** Taly et al. §II.B (SESlight) makes builtin
  objects transitively immutable and the builtin properties of the global
  object immutable. Top-level rebinding of a builtin fails; function-local
  shadowing is allowed.
  - Applied to Stone sessions, that would reject the corpus's 19 top-level
    bindings of builtin names (`state`, `first`, `stage`, `items`) and 6
    top-level `def artifact`.
  - The current plan (Python shadowing, plus an error when a shadowing
    non-callable is called) rejects none of them.
  - Confinement does not need the stricter rule once helpers run in their
    own evaluator (item 17): a session's rebinding then only affects that
    session's own code.
  
  Recommendation: keep the current plan.
- **Item 25 (`str()`).** It cites no sources. It waits on D-2.

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
| H-8 | Optional outcome prediction, host-diffed, reduces derailing beyond the effect tiers alone | Ablation arm B versus arm A (11.1) |
| H-9 | Tier-2 protection and tier-3 denial do not lower task success, and tier 2 measurably cuts test tampering | Ablation: attempted writes to protected paths and blocked egress per task, against V14 tampering rates |
| H-12 | Deferred. Whether models can predict a command's effects. The pilot (11.2) is weak evidence (recall 89-100% on short tasks). Under D-1 this matters only for tier-3 `Exec` declarations. | Revisit if tier-3 `UndeclaredEffect` rates are high |
| H-10 | Target models produce well-formed `expect` and tier-3 `effects` with near-zero errors as structured-output fields, provided the field sits flat in the schema | Measured within the ablation; the pilot (11.2) found nesting mattered for Qwen |
| H-11 | Rejecting `pass` handlers and bare `except` costs about one repair and does not push models toward worse patterns (for example `except Exception as e: e`) | Authorship eval; inspect handler bodies after repair |

### 11.1 The Outcome-Prediction Ablation (Set Up, Not Run)

From review (RQ6). The setup is a prototype deliverable; running it waits for
your go-ahead and model access.

- **Models**: Qwen3.6-27B and Claude Haiku 5.5 (from review). GPT-6 Luna as
  an optional third if we have access.
- **Tasks**: the existing standard-agent task suites behind
  `host/bench/eval_standard_agent_*.py`, so results compare with V14.
- **Arms**, one controller with two action schemas. Both run with D-1's
  tiers: tier-1 reports, protected `tests/`, and no network.
  - A. Tiers only: `Run` has no `expect`.
  - B. Tiers plus outcome prediction: `expect` is required in the schema;
    mismatches are reported and kept in history.
  
  The review's write-declaration arms were run as the pilot (11.2) and are
  not repeated (R27).
- **Metrics**: task success; tokens per task; loop repetition (same action
  signature after a failure); repairs per action (H-10); attempted writes to
  protected paths and blocked egress (H-9); false "done" claims rejected by
  the verifier.
- **Off-canonical-call rate**: the review's metric, from Toolathlon, needs a
  canonical tool path per task. Our suites do not have one. I propose
  steps-to-recovery after the first failed call as a proxy. This is a
  substitution, not the review's metric.
- **Prediction from the review** (restated for the two arms): B wins on raw
  success for 27B+ models; A wins on cost-adjusted success for smaller ones.
- **Script**: `host/bench/eval_stone_outcome_prediction_ablation.py`, following
  the existing `eval_*` pattern, with a `--dry-run` that validates arms and
  schemas without model calls.

### 11.2 Write-Prediction Pilot

`host/bench/pilot_declared_writes.py` runs arms 1, 2r, and 2 at harness
level, before any language change. It reuses the `run_stone_tbench4.py`
ReAct loop (JSON actions: `stone_eval`, `escape_linux`, `stone_help`,
`finish`).

- **Isolation.** Each action runs in bubblewrap. The root is read-only; only
  `/app`, `/work`, `/tmp`, and a sandbox `$HOME` are writable. The network is
  off and the environment is cleared. Every write therefore lands where the
  harness can see it.
- **Observed writes.** A snapshot diff before and after each action, with
  file hashes.
- **Enforcement (arm 2).** Restores a copy of the writable directories taken
  before the action, and returns `UndeclaredEffect` with the undeclared paths
  and a fix.
- **Tasks.** 12 Terminal-Bench data tasks the existing harness can verify
  locally, plus 2 synthetic coding tasks with incidental writes: a Python fix
  run under `unittest` (`__pycache__`) and a C build with `make` (`.o`
  files).
- **Models.** Local Qwen3.8-27B (Q4_K_XL; the brief named Qwen3.6-27B, but
  this is what the local server runs), Claude Haiku 5.5, and optionally
  GPT-6 Luna and Gemini 3.8 Flash.
- **Metrics.** Per (model, arm): tasks resolved; the fraction of writing
  actions fully declared; path recall; misses by class (pycache, build
  artifact, `$HOME` cache, `/tmp`, task path); overly broad declarations
  (`/app`, `/work`); rollbacks; format errors; tokens.

Limits: outcome prediction is not in the pilot, and the prompt
re-sends a long Stone guide every turn, so token counts are high in absolute
terms.

**Pilot v1 (2026-11-06, one repeat, 14 tasks): format confound.** v1 placed
`writes` beside the nested `input` object.

- **Haiku 5.5 was unaffected.**
  - It resolved 13, 13, and 14 of 14 tasks in arms 1, 2r, and 2.
  - It fully declared every writing action (path recall 100%), including
    `__pycache__` and `/tmp` scratch files.
  - Enforcement never rolled back an action.
  - It made three overly broad declarations (`/app`, `/app/src`) before
    running tests or `make`.
- **Qwen3.8-27B dropped from 11 resolved tasks to 8 in both declaring arms.**
  The cause was mostly format, not prediction:
  - It produced 34 and 53 JSON parse errors (mostly an extra closing brace),
    against none without `writes`.
  - In 30 actions it put `writes` inside `input`, which v1 counted as
    missing.
  - Its actual write prediction was 87% path recall. Every miss was
    `__pycache__`, and it never declared broadly.

Lessons:

- For the smaller model, *where* the field sits in the schema matters as
  much as whether the model can predict its contents (H-10). This supports a
  flat record field plus constrained decoding over free-form JSON.
- Broad declarations defeat per-call enforcement. Drift-critical rules such
  as "never write `tests/`" belong in the capability grant, not the
  declaration.

v2 moves `writes` into `input`, accepts the first complete JSON object
(counting such "lenient parses"), and runs three repeats.

**Pilot v2 (2026-11-06, three repeats, 14 tasks, 42 episodes per arm).**

| Model | Resolved: none / report / enforce | Path recall | Missed writes | Broad declarations | Rollbacks | Parse errors |
|---|---|---|---|---|---|---|
| Haiku 5.5 | 39 / 40 / 41 | 99.1% | 1 `main.o` per arm | 6 / 8 | 1 | 2 / 1 / 3 |
| Qwen3.8-27B (T=0.7) | 35 / 34 / 36 | 89% | 14 `__pycache__` per arm; 2 task paths (enforce) | 0 | 8 | 1 / 0 / 1 |
| GPT-6 Luna | 41 / 42 / 42 | 97.5% / 100% | 3 `__pycache__` (report) | 4 / 5 | 0 | 4 / 1 / 0 |
| Gemini 3.8 Flash (partial: run stopped at 106 of 126) | 29/42 / 25/36 / 18/28 | about 95% | 3 `__pycache__`, 2 task paths | 4 / 8 | 3 | 59 / 49 / 31 |

GPT-6 Luna and Gemini were added on 2026-11-07.

- **GPT-6 Luna** behaves like Haiku but stronger. It over-declares `/app`
  on the Python-fix task. One enforce episode was cut short by a host DNS
  failure after the correct output was already written, so it still counts
  as resolved.
- **Gemini 3.8 Flash** solves about two thirds of tasks in every arm, about
  65–70%. Its per-task results match across arms. Its many JSON parse errors
  appear in the no-declaration arm too, so they come from its general output
  format, not from `writes`. Its declarations show both failure directions:
  broad declarations and a few missed paths.

- **Mandatory declarations cost neither model task success.** The arms are
  within one task of each other, which is within per-task repeat variance.
  Almost every Haiku failure is `count-call-stack`, which no arm solves
  reliably. One Haiku report episode was lost to a harness bug: a
  whitespace-only reply was echoed into the history and rejected by the API.
  The bug is now fixed.
- **With the format confound removed, Qwen recovers fully.** Its v1 drop of
  3 tasks was the schema, not the prediction.
- **Enforcement rollbacks were cheap.** Qwen's 8 rollbacks were all fixed by
  the next action:
  - 6 were `__pycache__`;
  - 1 was a source file it edited without declaring it;
  - 1 was a `count.c` it created.
  
  Its mean turns per episode (6.6 enforce against 7.1 none) did not rise.
  Haiku's one rollback (`make` writing `main.o`) was also fixed on the next
  action.
- **The two models fail in opposite directions.**
  - Haiku *over-declares*: it lists `/app` or `/work` before running tests
    or a build. Enforcement then cannot catch drift inside those
    directories.
  - Qwen *under-declares toolchain side effects*: it never predicts
    `__pycache__`. Excluding that class, its recall is about 99%.

Lessons (provisional; the tasks are small and easy, and three repeats give
no statistical power):

- Across all four models, mandatory, enforced write declarations did not
  measurably hurt completion, provided the field sits flat in the schema.
  Gemini's run is partial.
- They also bought almost nothing. Every rollback was a toolchain by-product
  or an intended edit; none was drift.
- Capable models over-declared, which defeats per-call enforcement.
  Drift-critical rules such as "never write `tests/`" belong in the grant.
- Incidental toolchain writes (`__pycache__`, object files) should never be
  a prediction burden.

**Outcome:** D-1 was decided as three tiers (3.1). Write declarations were
dropped, and models declare only tier-3 effects of `Exec`.

Not yet tested:

- outcome prediction (11.1);
- longer-horizon tasks where drift actually happens;
- tier-2 and tier-3 enforcement on the Gateway;
- the Gateway's real checkpoint cost (about 300 ms per 50 MB) in place of the
  harness's copy-restore.

## 12. Uncertainties

- No experiment has been run for any proposal here.
- The Gateway was reviewed only for D-1 (3.1): protected-path mounts,
  network control, eBPF egress enforcement, and a syscall effect manifest
  do not exist yet. How `Exec.within`, `Dir`
  grants, and `Verifier` attempts on read-only snapshots map onto it is still
  unresolved.
- Local mode without an overlay has no rollback, so tier 1 there is
  report-only. Tiers 2 and 3 need prevention (read-only mounts, network
  denial), or are limited to capability methods.
- Effect predictability for models (H-12) is deferred. It affects only
  tier-3 `Exec` declarations.
- Effect signatures for third-party MCP tools come from a manifest we
  maintain. A wrong signature (an irreversible tool marked recoverable) is
  a hole the runtime cannot detect.
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
- Tier-1 effect reports on every `Exec` result; tier-2 protected paths in
  grants (D-1, R25a).
- Tier-3 `effects=` vocabulary for `Exec`, with the network off unless
  declared (already S2).
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
  handlers (L13). Syscall effect manifest for host tools, RPC, and MCP
  (R25b, R25c); per-call eBPF egress enforcement for `Exec` on Linux.
- `model.delegate` with explicit grants (level B); 0-CFA admission check for
  delegated bodies (R23).
- Information-flow control (taint and declassify) reconsidered, with
  measurements.

Exit: adversarial delegation and injection tests, plus Gateway mapping for
`Exec.within`, grants, protected paths, and egress control.

## 14. What Changes In The Prototype

Compared with the draft v2 plan. Nothing here has started. "Existing" marks
items that change current Stone behavior; those wait for the source reading
in the last column and for the compatibility measurement.

| # | Item | Change from draft v2 | Existing | Read before implementing | Phase |
|---|---|---|---|---|---|
| 1 | Bare `except:` is a parse error; the correction engine, AST hints, CLI help, and `STONE_LANGUAGE.md` stop suggesting it (9.5) | New | **Yes**: accepted and recommended today | Joe-E §4.3; PEP 352; Ren et al. 2023 | P1 |
| 2 | `pass`-only handler for `Exception`/`ToolError` is an admission error | New | **Yes**: legal today | Ren et al. 2023; Seeker 2024 | P1 |
| 3 | Swallowed-`ToolError` counter in trace and `last_result()` | New | Additive field | — | P1 |
| 4 | `except BaseException`, `except PolicyViolation`, `except BudgetExhausted` rejected | New | No (names do not exist) | Trio cancellation docs | P1 |
| 5 | `ExpectationFailed` fields `.expected/.observed/.diff/.result` | Was "carries the full observation" | No | — | P1 |
| 6 | Three effect tiers per syscall: observed effects on every result as `r.effects` (tier 1); protected paths in the grant, read-only for `Exec` (tier 2); `Exec` declares tier-3 effects (`network`, `irreversible`, `background`), denied otherwise with `UndeclaredEffect` | Was mandatory `Run.writes` with discard (draft v3, D-1 provisional); replaced after the pilot | No (new API) | D-1; pilot 11.2 | P1 (tiers 1-2, tier-3 vocabulary) / P3 (eBPF enforcement) |
| 7 | `expect=` optional on every syscall; exit status in it; RPC/MCP `result` and `status` checks; `Tests` row removed | Was part of every action, and for `Exec` only | No | PreAct | P1 |
| 8 | `with budget(...) as b:` with Trio semantics; `grace=`; `model.reserve` removed | Was `limit` + `reserve`, exhaustion propagating | No (new API) | Trio "Cancellation semantics"; bpo-32528 thread | P1 |
| 9 | `finally` stays absent; error becomes prescriptive | Was unaddressed | Message only | Joe-E §4.3 | P1 |
| 10 | Mutable default argument error becomes prescriptive | Was unaddressed | Message only | — | P1 |
| 11 | L9 module values deep-frozen at import | New | No (no modules yet) | Joe-E §4.2 | P1 |
| 12 | Ablation harness `host/bench/eval_stone_outcome_prediction_ablation.py`, two arms (tiers, tiers plus `expect`), dry-run only | Was four write-declaration arms | No | PreAct; Lee 2026 | P1 |
| 13 | `Verified` as a sealed box with value, verifier id, claim, nonce; verifier in its own read-only attempt; no `critic=` | Was an opaque tag minted with an optional critic | No (new type) | Morris 1973 §6-7 | P2 |
| 14 | Boundary walk component with five call sites | New | No | Taly et al. 2011 §II.B, §V | P2 |
| 15 | Pass-style `scope.start(f, **args)`: copy data, share handles, closure rule | Was "capability arguments", no copy rule | No (attempts start by id today) | Agoric "Far, Remotable, and Marshaling" | P2 |
| 16 | Daemon obligation registry and unforgeable handles; host stops daemons on attempt exit | New | **Yes**: `stop_daemon(pid)` behavior in controlled profile | Weimer & Necula §5-7 | P2 |
| 17 | Helpers in an isolated evaluator state | New | **Yes**: helpers share scope and function table today | `docs/STONE_HELPERS.md`; Taly et al. §VI.A | P2 |
| 18 | Builtin table frozen; helper and agent function tables separate; one Python namespace with local shadowing; calling a non-callable that shadows a builtin is a prescriptive error; rename MCP examples (9.5) | New | **Yes**: separate callable namespace today; agent `def` of a builtin name persists in the session | Taly et al. §II.B | P1 (namespace, with L1) / P2 (freeze, helper tables) |
| 19 | Exceptions carry data only (walk at `raise`) | New, with L6 | No (exceptions are plain dicts today) | Joe-E §4.3, Fig. 6 | P2 |
| 20 | Session ceiling; capabilities persisted as host-table references; checks at injection, restore, child start | Was "capabilities not session-persistable" | **Yes**: `StoneSession` persistence | Miller 2006 (vats); Agoric docs | P3 |
| 21 | Helper directories inside the agent's write grant skipped in controlled profile | New | **Yes**: documented search path (controlled profile only) | `docs/STONE_HELPERS.md` | P3 |
| 22 | `PolicyViolation`/`ContractViolation` fatal, no agent code afterward | Was "outside `Exception`, catchable explicitly" | No (no violations exist yet) | Joe-E §4.3 | P3 |
| 23 | 0-CFA admission check for delegated bodies | New | No | Taly et al. §V | P3 |
| 24 | H-4 test dropped; capabilities allowed in containers | Removed | No | — | — |
| 25 | L4 extended to `str()` of dicts and records (D-2); 11 call sites switched to `json_dumps` | New | **Yes**: `str(dict)` is JSON today | — | P1 |
| 26 | Write-prediction pilot harness (11.2) | New | No | — | P1 (done) |
| 27 | Syscall effect manifest: Gateway-owned effect signatures for host tools, RPC, and MCP tools, seeded from MCP annotations (R25b, R25c) | New | No | MCP spec, tool annotations | P2 (manifest) / P3 (enforcement) |

Items 1, 2, 16, 17, 18, 20, 21, and 25 change existing behavior. The
compatibility scan (9.5) found almost no executed code that depends on
L1-L4 or L6a; the cost is concentrated in items 1, 18, and 25. For each, the
plan is: read the cited sections, measure the impact on examples, help
examples, task specs, and the conformance corpus, then ask before changing the
interpreter.
