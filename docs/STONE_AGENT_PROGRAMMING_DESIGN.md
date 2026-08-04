<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Agent Programming Design

## Status

Living design, with implemented slices recorded through 2026-07-30. A surface
described as proposed or illustrative is not an implemented Stone builtin.

## Decision

Stone should become the LLM-oriented control language for agent programs by
making model inference an explicit, typed effect. It should not make one ReAct
loop, one graph scheduler, or one multi-agent protocol part of the language
semantics.

In the OS analogy, Waymark provides computing resources, an attempt is the
process abstraction and programming model, and Stone is the language primarily
designed for an LLM to program attempts. A task harness is simply a Stone
program specialized for a task; it is not an additional architectural layer.

Common agent loops should first be available as visible, executable Stone
reference programs. Once a flow is stable and profiling justifies it, an
optimized builtin template may implement the same `AgentControl` interface as
the ordinary Stone function. Either form must remain extensible through Stone
composition and receive the same Shell tools for CPU, memory, files, Linux,
models, context, and other attempts.

The primary author is another language agent. A typical execution is:

```text
Codex or another outer agent
  -> synthesizes an ad-hoc Stone agent for this task
  -> admits and starts that program in an attempt
  -> Stone agent calls models, tools, Linux, and child attempts
  -> reports structured evidence and a result to the outer agent
```

The outer and inner agents may use the same model, but they have different
roles. The outer agent writes or repairs control code. The generated Stone
program performs bounded task-specific control. A human may inspect the source
and trace, but human typing convenience is not the language's primary
optimization target.

In the long term, an explicit formal task contract may lower into checked Stone
interfaces, evidence monitors, and success gates. The current research question
comes first: can existing language agents author and repair effective Stone
attempt programs from task requirements, available APIs, and prior traces?

The division is:

```text
Stone program
  owns prompts, conversation state, control flow, tool dispatch, retry policy,
  verification policy, and when to create child attempts

Waymark LibOS runtime
  evaluates Stone and exposes model, workspace, Linux, attempt, and trace
  capabilities as structured effects

Gateway
  owns credentials, provider/model selection, policy, accounting, durable
  attempt state, transactional resources, and external effect mediation

Attempt
  is one isolated execution of the Stone agent program, not the agent loop
  algorithm itself
```

Inside Waymark LibOS, the controller for an attempt should eventually be a
forkable Stone process running on an ordinary Hermit thread. That runtime
mechanism, its single-address-space trust model, and cleanup rules are defined
in [`STONE_PROCESS_FORK_DESIGN.md`](STONE_PROCESS_FORK_DESIGN.md). It does not
change the Stone agent-control interface or make local process placement
visible to programs.

The first milestone should expose a low-level `model_call(...)` builtin and
prove that an ordinary Stone program can implement and modify a ReAct loop. A
later typed-inference and native-tool protocol should reduce plumbing without
hiding the program's control policy.

## LLM-Oriented Shell And Language Constraints

Agent programming extends Waymark Shell; it does not redefine Stone around one
agent architecture. The original shell constraints come from Stone's role as a
generation target for language models, not from trying to make a conventional
human shell with different syntax.

The following existing goals remain primary:

- **Python-shaped generation.** Preserve syntax that coding models already
  generate reliably: assignments, functions, loops, conditionals,
  comprehensions, `try/except`, lists, and records. This exploits model priors
  and examples already present in training data; it is not a commitment to
  Python compatibility. Do not adopt Pel's Lisp syntax, Turn's actor syntax, or
  a graph DSL merely because their semantics contain useful ideas.
- **Structured I/O.** Model messages, model results, tool observations, usage,
  errors, workspace state, and process results remain typed Stone values. Do
  not regress to JSON embedded in shell strings when a structured boundary is
  available. Text/JSON actions are only a bootstrap over today's text-only
  model RPC.
- **A useful shell without a model.** File, JSON/JSONL/CSV, search, transform,
  process, state, and Gateway operations remain useful in deterministic Stone
  programs. A model backend is optional, not required to parse or execute
  ordinary Stone.
- **Linux ecosystem compatibility.** Stone coordinates existing compilers,
  test runners, databases, media tools, and scripts through structured
  `run`/`must_run`; agent programming does not require those programs to be
  rewritten as model tools.
- **Explicit effects and recoverable diagnostics.** Model support follows the
  same conventions as current file/process operations: structured results,
  stable error codes, bounded output, help, and suggested recovery where
  possible. Errors should give an outer agent enough local information to
  repair the generated program without reconstructing hidden runtime state.
- **Compact self-description.** The grammar, builtin signatures, effect
  requirements, examples, and error shapes must be queryable in bounded form.
  An outer agent should be able to generate a valid small program from task
  context plus Stone help, without loading implementation source or a large
  framework manual.
- **Safe ad-hoc specialization.** Generating a different small Stone program
  for a particular task is a primary use case, not an exceptional fallback.
  Parsing, admission, capabilities, budgets, attempts, and transactions make
  such one-off programs safe enough to run and cheap enough to discard.
- **Lightweight and placement-neutral default.** The normal Waymark build stays
  usable on a host or in a container. Gateway-backed model authority activates
  only when the capability is present; provider SDKs and credentials do not
  enter the unprivileged runtime.
- **One runtime, multiple adapters.** CLI, task server, MCP compatibility, and
  LibOS placement call the same Stone semantics. MCP does not become the agent
  language or the core runtime.
- **No hidden harness semantics.** A standard ReAct helper may be convenient,
  but prompts, loop state, stopping rules, retries, and tool dispatch remain
  inspectable and replaceable by Stone source.

The design test is therefore not just "can Stone run an agent?" It is:

```text
Can an outer language agent synthesize, diagnose, and revise a small
task-specific agent as ordinary Python-shaped Stone, using structured values
and explicit effects, without weakening deterministic shell/data use cases?
```

## Why This Is The Missing Layer

Waymark already has most of the surrounding mechanisms:

- Stone has functions, lambdas, loops, conditionals, structured values,
  structured errors, file operations, and Linux execution.
- Stone has Gateway-backed attempt spawn/fork/start/wait/report/accept/discard
  and publication operations.
- Gateway has a provider-neutral `model.call` RPC and owns model credentials.
- Attached Stone programs have read-only `task_spec()` and `task_input()`
  views backed by the Gateway attempt control block.
- Waymark has a working Rust `AgentSession` with a fixed JSON-action ReAct
  loop.

The current Rust loop proves placement and plumbing, but it is a harness. An
outer agent cannot synthesize a task-specific action grammar, add a critic,
change observation compression, introduce a verifier turn, or choose a
different search policy without changing Rust.

The missing operation is therefore:

```text
Stone control code -> model effect -> structured model result
```

Once that effect exists, ordinary Stone control flow is already expressive
enough for the first agent-programming experiment.

## Research Findings

The emerging agent-language work separates into several useful directions.
Waymark should borrow mechanisms selectively rather than adopt another
language's complete object model. The
[Agent Languages catalogue](https://agentlanguages.dev/) is a useful index,
but many entries are research prototypes or young implementations; their
claims are design evidence and hypotheses, not production validation.

| Work | Useful native idea | Waymark placement |
| --- | --- | --- |
| Turn | typed inference, actors, capabilities | typed model effect; attempts supply process isolation |
| Pel | constrained grammar, restarts | Stone generation schema and structured recovery |
| Quasar | bounded code actions | Stone is the code-action language |
| LBAC | policy-bearing types | Stone program admission plus Gateway enforcement |
| Spell/SPE | model-authored control, replay-safe effects | explicit Stone admission and durable operation ids |
| PDL/GENT | prompt transparency, typed results | low-level model call plus typed inference library |
| Temporal | durable control/activity separation | Stone control over recorded Gateway effects |

### Turn

[Turn](https://arxiv.org/abs/2603.08755) treats typed inference, isolated actor
state, capabilities, and suspension as language/runtime concerns.

Adopt:

- inference output validated against a declared schema before binding;
- opaque capabilities and credential isolation;
- durable, inspectable execution identity;
- explicit model inference rather than a magic natural-language expression.

Map differently:

- a Waymark attempt is already the process-like isolation, lifecycle, budget,
  and supervision object;
- a second Stone actor hierarchy would duplicate attempts and create ambiguous
  ownership for cancellation, workspace state, and accounting;
- lightweight model personas inside one attempt can remain conversation data;
  work needing independent state or authority should use child attempts.

Do not adopt yet:

- a generic `confidence` branch operator. Model-reported confidence is not a
  calibrated probability. Reliability bounds require a task-specific
  calibration method, as in Quasar, rather than special syntax around an
  unverified number.

### Pel

[Pel](https://arxiv.org/abs/2505.13453) argues for a small grammar, constrained
generation, compositional pipelines, static parallelization, and restart-based
self-healing.

Adopt:

- publish a compact Stone grammar and builtin schemas for constrained
  generation;
- keep recoverable errors structured and provide explicit recovery choices;
- keep agent-authored orchestration inspectable as source;
- eventually use effect information to identify safe concurrency.

Map differently:

- grammar restriction is a generation and usability mechanism, not the
  authority boundary. Gateway capabilities remain authoritative even if a
  model emits syntactically valid Stone;
- repair should be an explicit program policy with bounded model calls, not an
  invisible runtime loop;
- automatic parallelization must wait until effects and data dependencies are
  declared accurately. Child attempts and explicit concurrent operations are
  safer initial primitives.

### Quasar And Code Actions

[Quasar](https://arxiv.org/abs/2506.12202) supports the observation that code
actions can express loops, dependencies, and batching better than sequences of
individual tool calls.

This directly supports Stone's role. Stone is the bounded code-action language;
Linux remains a compatibility device. Quasar's approval reduction and
conformal-prediction results are promising research directions, but conformal
guarantees require calibration data and must not be claimed for a generic
`model_call` result.

### Language-Based Agent Control

[Language-Based Agent Control](https://arxiv.org/abs/2605.12863) places agent-
generated and developer-written code under the same type and policy rules.

Adopt in stages:

- attach capability and effect requirements to Stone programs and builtins;
- type-check tool schemas and inference result schemas;
- reject authority amplification before execution where statically visible;
- retain Gateway runtime checks because dynamic values and external resources
  still require mediation.

### Self-Programmed Execution

[Self-Programmed Execution](https://arxiv.org/abs/2605.06898) removes a fixed
turn-to-turn orchestrator and lets model-produced programs determine control.
It also identifies the danger of replaying effects when edited programs are
re-evaluated.

Waymark should support model-authored Stone programs without making every model
completion executable automatically. Model output becomes code only through an
explicit compile/admit operation. Durable resume will require stable operation
ids so replay can reuse recorded model/Linux results rather than duplicate
irreversible effects.

### PDL, GENT, And General Agent SDKs

[PDL](https://ibm.github.io/prompt-declaration-language/) demonstrates the
value of explicit prompts, typed generated data, and ordinary control flow in
one inspectable program. [GENT](https://www.gentlang.org/spec) emphasizes typed
outputs and transparency about what reaches the model. The
[OpenAI Agents SDK](https://openai.github.io/openai-agents-python/) and
[LangGraph](https://langchain-ai.github.io/langgraph/) show the useful standard
services around a loop: tool schemas, sessions, tracing, guardrails,
checkpointing, and human interruption.

Adopt:

- no hidden prompt or context injection in the low-level model operation;
- typed output validation and explicit retry counts;
- common protocol helpers and tracing as libraries/runtime services;
- checkpointable program state and interruption later.

Avoid:

- making the prebuilt loop the only way to use the model;
- making a graph representation mandatory for simple control flow;
- conflating conversation memory with canonical workspace or attempt state.

### Durable Workflow Runtimes

[Temporal's workflow model](https://docs.temporal.io/workflows) separates
deterministic control code from external activities and records activity
results for replay. That distinction is relevant even though model-driven
branching is nondeterministic:

```text
Stone control decision
  <- recorded result of model.call / linux.run / external input
```

Waymark should eventually rebuild controller state from recorded effects or a
checkpoint without issuing the effects again. It should not promise durable
resume merely because the workspace transaction survives.

## Design Principles

### 1. Inference Is An Effect, Not Syntax Magic

Natural-language conditions such as `if "the patch looks safe"` hide a model
call, prompt, budget, and failure mode. Stone should require an explicit
`model_call` or `model_infer` operation whose result is ordinary data.

### 2. Control Remains Ordinary Stone

Loops, branching, functions, error handling, lists, and records should express
ReAct, plan/execute, critic/revise, best-of-N, and verifier loops. New syntax is
justified only when it adds enforceable semantics that ordinary Stone cannot.

### 3. The Low-Level Surface Is Transparent

`model_call` must not silently add system text, tool instructions, memory, or a
stopping rule. The caller supplies the messages. Gateway may enforce policy or
choose a concrete model, but the response must report that resolution.

Higher-level libraries may render prompts, provided their rendered request and
version are inspectable.

### 4. Typed Nondeterminism

The language cannot type-check the semantic truth of a model answer. It can
check that an answer has the declared shape before control flow consumes it.

```text
model_infer<T>(...) -> T | ModelError | ValidationError
```

The effect trace records model policy, schema, usage, and result identity. A
valid structure is still a candidate claim until independently verified.

### 5. Authority Is Not A Prompt Property

Tool lists and prompts help the model choose actions. Gateway capabilities
decide whether an effect is allowed. A hidden or omitted tool description must
not grant or revoke host authority.

### 6. Attempts Are The Composition Boundary

Use an ordinary model call for another perspective inside the same state. Use a
child attempt when work needs independent workspace state, lifecycle,
capability/budget attenuation, cancellation, or candidate acceptance.

The attempt and attempt-tree semantics are Gateway kernel mechanisms. Stone is
the Python-shaped language used to implement the parent and child controllers;
Waymark Shell maps its protected resource operations onto Gateway syscalls.
Neither layer should redefine an attempt around ReAct, critic loops, or any
other policy. The normative process analogy is specified in
[Attempt Process And Process-Tree Model](../../waymark-gateway/docs/ATTEMPT_PROCESS_TREE_MODEL.md).

### 7. Expected Failures Are Values At Policy Boundaries

Transport, rate-limit, malformed-output, tool, timeout, cancellation, and
verification failures need distinct codes. Stone `try/except` can catch them,
but retry, repair, fallback, or abort remains visible program policy.

A rejected candidate is an expected policy outcome, not necessarily a
supervisor exception. It should retain bounded evidence and allow exploration
to continue. Provider loss, authority denial, corrupt state, and other
infrastructure failures remain exceptional and must not be mislabeled as a
bad hypothesis.

### 8. No Automatic Effect Replay

Model calls consume irreversible budget and external calls may be irreversible.
A controller retry must not duplicate an effect unless the program explicitly
requests another sample. Durable execution needs idempotency keys and recorded
results before transparent resume is safe.

### 9. Proposals Are Hypotheses; Evidence Decides

Typed inference proves that model output has an admitted shape. It does not
prove that the selected continuation's assumptions hold or that its result is
correct. A model may propose code, a patch, or candidate ordering; fresh
execution evidence and independent verification retain acceptance authority.

A retry must express semantic progress: try an untried candidate, revise an
assumption, obtain new evidence, or apply a stage-scoped patch. Repeating the
same action against the same state is not a useful control abstraction.

## Proposed Surface

The public surface should be layered. V0 is deliberately small; later layers
must not make V0 unusable.

### Program Entrypoint And Task View

Task-specific source is expected: an outer agent may embed a plan, prompt, tool
set, or verification policy directly in an ad-hoc Stone program. That source is
the exact program identity and must be recorded.

Stone should also support reusable agent programs. Today the Gateway control
block uses the full `TaskSpec` to construct the fixed Rust agent prompt, while
a recorded Stone program receives its source but not an equivalent first-class
task value. Reusable programs therefore need structured task input rather than
runtime-rendered hidden prompts.

The target entrypoint should expose, without prompt rendering:

```text
task_spec() -> record
task_input() -> Any
```

`task_spec()` returns the admitted objective, named inputs/outputs, success
criteria, constraints, and artifact declarations visible to the attempt.
`task_input()` returns the task's typed dynamic input. A normal Stone return or
`emit(value)` remains the program result, and the LibOS controller reports that
result for its own attempt.

The first raw-model smoke and ad-hoc programs may embed task-specific prompts.
`task_spec()` and `task_input()` are implemented for attached attempts and are
required for reusable programs and skills, not for proving that an outer agent
can synthesize a working one-off agent. Dynamic input is carried separately as
canonical JSON in the attempt control protocol; it is not prompt text or an
environment-variable convention.

### Layer 0: Raw Model Device

Proposed Stone signature:

```text
model_call(
  messages: list[record],
  model_class: str = "agent",
  model: str = "",
  temperature: float? = None,
  top_p: float? = None,
  seed: int? = None,
  max_output_tokens: int = 0,
  response_format: record | str | None = None,
  metadata: record = {},
) -> record
```

Input messages in the first slice are:

```json
{"role":"system|user|assistant","content":"text","name":"optional"}
```

Successful result:

```json
{
  "provider": "vllm",
  "model": "nvidia/Qwen3.6-27B-NVFP4",
  "request_id": "...",
  "messages": [{"role":"assistant","content":"..."}],
  "content": "...",
  "finish_reason": "stop",
  "usage": {
    "input_tokens": 100,
    "output_tokens": 20,
    "total_tokens": 120
  },
  "latency_ms": 350,
  "metadata": {}
}
```

Rules:

- `model_class` is the normal policy selector; `model` is only a hint unless
  Gateway policy forces it.
- credentials, provider endpoints, and secret environment-variable names are
  not Stone arguments or results;
- message order and content are preserved by the low-level operation;
- no automatic retry occurs in `model_call`;
- a provider/policy/transport failure raises a structured Stone error;
- Gateway attributes the call and accounting to the attached attempt.

The Gateway protobuf currently carries text messages and a unary response, so
this layer can be implemented without another protocol redesign.

### Layer 1: Typed Inference

Proposed signature:

```text
model_infer(
  messages: list[record],
  schema: record,
  retries: int = 0,
  repair_prompt: str = "",
  ...model options...
) -> record
```

Result:

```json
{
  "value": {"kind":"run","argv":["pytest","-q"]},
  "response": {"model":"...","usage":{}},
  "validation_attempts": 1,
  "errors": []
}
```

Semantics:

1. Convert the declared JSON Schema into the provider's structured-output
   request when supported.
2. Independently validate the returned JSON inside Waymark before binding.
3. If validation fails and `retries > 0`, issue a new, separately traced model
   call with the explicit repair prompt.
4. Return aggregate usage and every validation failure summary.

Provider-side constrained decoding improves efficiency but does not replace
runtime validation. `retries` is bounded and visible because every retry costs
budget.

### Layer 2: Neutral Tool Protocol

The current Gateway model protocol is text-only. A provider-neutral extension
should eventually add:

```text
ModelTool {
  name
  description
  input_schema
}

ModelToolCall {
  id
  name
  arguments
}

ModelToolResult {
  call_id
  content
  is_error
}

ModelOutputItem = Text | ToolCall | Refusal
```

Then `model_call(..., tools=[...])` can return normalized `tool_calls` without
Stone programs parsing provider-specific formats. Stone still chooses and
executes the dispatch policy.

V0 must not wait for this extension. It can request a JSON object, parse it
with `json_loads`, and validate the action fields in Stone, as the existing
Rust loop does informally today.

### Layer 3: Inspectable Library Helpers

After the primitive works, provide helpers rather than new control syntax:

```text
model_tool(name, description, input_schema) -> record
model_tool_result(call, value, is_error=False) -> record
agent_trace_summary(...) -> record
agent_context_size(messages) -> record
```

A standard ReAct program can be shipped as Stone source, a skill, or an
optimized builtin template. An outer agent can synthesize a smaller
task-specific variant. The current Rust `AgentSession` may remain as an
optimized builtin, but it must implement the same public control contract as a
Stone-defined agent rather than define a closed harness mode.

### Layer 4: Composable Agent Control

Agent control is a Waymark user-space abstraction. Its smallest semantic
contract is an ordinary callable:

```stone
interface AgentControl[I, O]:
    def run(session: AgentSession[I]) -> O

AgentSession[I] = {
    task: TaskView[I],
    self: AttemptHandle,
    context: ContextView,
    tools: ResourceTools,
    limits: ResourceLimits,
    events: EventSink,
}
```

This is an ordinary Stone interface/protocol, not a Gateway object or IR. The
exact representation may initially use records and callables. A Stone
function, skill-provided control, or optimized Rust builtin can implement it.
`run_agent` invokes any conforming control, while builtins such as
`react_control`, `tool_agent_control`, `critic_control`, and
`supervisor_control` are simply optimized implementations.

Control state is ordinary local Stone state or explicit context state.
Returning reports a result; raising a structured error reports failure; the
runtime observes cancellation at Shell call/yield boundaries. Waymark may use
an internal poll/resume ABI for suspension or an optimized native control, but
that is an execution detail rather than an agent-program representation.

The protocol separates four things that the current fixed loop conflates:

- control state and stopping policy;
- context construction and observation projection;
- available resource tools;
- the attempt lifecycle that owns the running controller.

Controls must be extensible, stackable, and composable through ordinary Stone
adapters:

```stone
control = react_control(model="default")
control = with_tools(control, shell_tools("file", "linux", "attempt"))
control = with_context(control, context_window(max_tokens=32000))
control = with_budget(control, tokens=100000, wall_time_ms=300000)
control = with_retry(control, transient_only=True, max_attempts=2)
control = guard_actions(control, deny=["publish"])
control = on_event(control, record_progress)
result = run_agent(control, task_spec())
```

Further combinators may include `map_observation`, `with_verifier`,
`with_critic`, `then`, and `fallback`. These are library policies. A task may
also write a direct loop and call the same tools without using `AgentControl`.

### Shell Resource Tool Families

Both builtin and Stone-defined controls receive capability-scoped tools from
Waymark Shell:

| Resource | Representative operations |
| --- | --- |
| CPU and memory | usage, limits, reservations, operation concurrency |
| files/workspace | read, write, list, search, diff, checkpoint |
| Linux/processes | run, run-wait, status, signal, join |
| models | call, stream, structured infer, usage |
| context | task input, read, append, project, summarize |
| attempts | spawn/fork, state/events, wait/join, signal, report, accept/discard |

Some helpers execute locally in Waymark. Protected effects become Gateway
RPCs/syscalls. The wire request is syscall ABI; it is not an agent-program
representation and Gateway does not interpret the surrounding control flow.

## Example: ReAct In Ordinary Stone

This example uses the implementable text/JSON V0. Exact field names may change
after the first prototype, but the control placement should not.

```python
def react(task):
    messages = [
        {
            "role": "system",
            "content": "Return one JSON action with kind run, read, write, or finish.",
        },
        {"role": "user", "content": task},
    ]

    for turn in range(12):
        response = model_call(
            messages,
            model_class="agent",
            response_format={"type": "json_object"},
            max_output_tokens=512,
        )
        action = json_loads(response.content)
        messages.append({
            "role": "assistant",
            "content": response.content,
        })

        if action.kind == "run":
            observation = run(action.argv, timeout_ms=60000)
        elif action.kind == "read":
            observation = {"content": read_text(action.path)}
        elif action.kind == "write":
            write_text(action.path, action.content)
            observation = {"written": action.path}
        elif action.kind == "finish":
            return {"answer": action.answer, "turns": turn + 1}
        else:
            observation = {"error": "unsupported action"}

        messages.append({
            "role": "user",
            "content": json_dumps({"observation": observation}),
        })

    fail("agent exhausted its turn budget")

task = "Create answer.txt containing the result of the requested computation."
emit(react(task))
```

After Layer 1, the same control structure can replace manual parsing with
schema-checked inference. It can also express a critic loop without runtime
changes:

```python
candidate = model_infer(candidate_messages, schema=CANDIDATE_SCHEMA)
critique = model_infer(
    critique_messages + [{"role": "user", "content": json_dumps(candidate.value)}],
    schema=CRITIQUE_SCHEMA,
)
if not critique.value.accept:
    candidate = model_infer(revision_messages, schema=CANDIDATE_SCHEMA)
```

This is the key test of agent programming rather than agent configuration.

## Relationship To Attempt Programs

A task admitted with a Stone program becomes an agent program when that Stone
source invokes the model device. No new `frontend: "agent"` is required in the
long-term object model.

```text
AttemptProgram(kind=stone, source=...)
  + capability model.call
  + capability linux.exec
  + workspace transaction
  = one executable agent-program instance
```

The program source, referenced skills, tool schemas, Stone/runtime version, and
model-class requirement contribute to program identity. The concrete model and
provider are execution metadata selected by Gateway policy. Gateway may record
that identity without parsing the program into another IR.

Child attempts remain appropriate for:

- alternative workspace candidates;
- independent failure/cancellation;
- capability or budget attenuation;
- concurrent work needing separate state;
- evidence-producing specialists whose results a parent may accept or reject.

Do not create child attempts merely to represent every prompt persona or every
model call. That would make the process abstraction too expensive and obscure
the actual state boundary.

### Process-Tree Mapping

Stone needs to distinguish three levels:

```text
Stone function/loop       local deterministic or model-driven control
operation handle          asynchronous Linux/model/browser/device effect owned
                          by the current attempt
attempt handle            independently isolated and supervised computation
```

The supervision tree owns lifetime, signals, budget reservations, and reaping.
Workspace, context, artifact, and evidence lineage may form DAGs and must be
specified separately. A child exit is not a merge, and a parent wait is not an
acceptance decision.

### Fork Versus Spawn

At the Stone surface, `attempt_fork` means “continue from exactly here in an
isolated child.” Waymark asks Gateway to create one coherent current-parent
frontier, then starts a named child entrypoint against that state. The parent
continues after the call. The child receives the bounded parent memory snapshot
at the typed `child.fork_origin.memory_revision`, then parent and child ledgers
diverge under separate `memory_ref` values. Accept imports workspace state but
does not merge child memory; parent Stone policy promotes selected facts with
an explicit `context_write`.

`attempt_spawn` instead constructs a child from explicit resource sources. A
spawned child can have a lifecycle parent without being a state continuation of
that parent. Choosing a parent workspace checkpoint and a context summary
independently remains spawn because those sources need not describe one atomic
parent revision.

The target LLM-friendly form is:

```stone
child = attempt_fork(
    entrypoint="worker",
    input={"strategy": "alternate"},
    context_prompt_view={"required_keys": ["requirement.target"]},
    budget={"model_calls": 4, "wall_time_ms": 120000},
    scope=scope,
)
```

The attached parent and current admitted module are implicit. Fork starts the
child by default and registers it with the current structured-concurrency
scope. The raw syscall retains create-without-start for supervisors. Arbitrary
workspace/context source selection, host provider identities, and credentials
are not fork arguments.

The required-key subset of `context_prompt_view` is implemented now. It is
persisted as child admission state and exposed through
`agent_session().context_prompt_view`; it does not carry rendered prompt bytes.

Fork does not clone the live Stone stack, a model invocation, or an in-flight
Linux RPC. V1 requires the parent's provider operations to be idle and starts
the child at `entrypoint(input)`. A future resumable controller mode must be
explicit and must fail when no compatible controller checkpoint exists.

The Gateway-side state frontier, conservation, accept/discard, and failure
contract is specified in the gateway repository's
`docs/ATTEMPT_FORK_DESIGN.md`.

The current inline form:

```stone
attempt_fork(program={"kind": "stone", "source": "..."}, start=True)
```

is a bootstrap executable form. It forces an outer model to generate and
escape nested programs. The target is one loaded Stone module with multiple
named entrypoints:

```stone
def worker(input):
    return candidate(value=solve(input), evidence=[])


def main(input):
    scope = attempt_scope(exit_policy="cancel_then_join")
    child = attempt_spawn(
        program=current_program(),
        entrypoint="worker",
        task_input={"strategy": "alternate"},
        workspace_source=workspace_fork(),
        context_source=context_summary(),
        capabilities=attenuate(["workspace", "linux.exec", "model.call"]),
        limits={"wall_time_ms": 120000, "model_calls": 4},
        scope=scope,
    )
    outcome = attempt_join(child)
    if outcome.evaluation.status == "passed":
        attempt_accept(attempt_info().attempt, child, import="workspace_all_allowed")
    else:
        attempt_discard(child, reason="candidate did not pass")
    attempt_scope_close(scope)
    return inspect_parent_result()
```

This remains ordinary Python-shaped Stone with structured values. Waymark
resolves `current_program()`, checks the named entrypoint and serializable
input, and invokes the Gateway spawn syscall. The syscall arguments describe
the executable, resource views, authority, limits, and lifecycle; they are not
a control-flow IR. Gateway enforces authority and lifecycle constraints at
admission and runtime.

The first process-tree surface should include:

- current/named module entrypoint launch and typed `AttemptHandle`;
- `attempt_scope`, child sets, `attempt_wait_any`, `attempt_wait_all`, and
  `attempt_join`;
- cancel/terminate/kill signals and mandatory cancel-then-join scope cleanup;
- typed `AttemptOutcome` separating execution, result, evaluation, selection,
  and cleanup;
- aggregate root/child budget views and attempt-owned operation ledgers;
- bounded progress events before general peer messaging.

Portfolio selection, semantic duplicate detection, critic placement, retry
strategy, and scoring remain Stone library/program policy. Scope cleanup,
budget conservation, authority attenuation, and immutable outcome delivery are
attempt-runtime semantics.

### Bounded Candidate Exploration

The Caffe V35-V37 sequence supports this standard control pattern:

```text
shared proved prefix -> semantic checkpoint
  -> model proposes an admitted candidate
  -> child attempt restores the checkpoint
  -> fresh stage and final evidence
  -> accept, or reject and try an untried candidate
```

The model call is optional: a deterministic policy may order candidates. The
important property is that proposal, execution, evaluation, and acceptance are
separate transitions.

The first interface is now the inspectable
[`bounded_attempt_explore.stone`](../examples/scripts/bounded_attempt_explore.stone)
control library. Its current ordinary-source shape is:

```stone
frontier = semantic_frontier(build_checkpoint)
result = explore_frontier(
    frontier=frontier,
    candidates=[
        candidate(
            name="path_guard",
            input={"replacement": "bundle_runtime_path_guard"},
            assumptions=["source and destination strings are equal"],
            ensures=["runtime artifact is independently loadable"],
        ),
        candidate(
            name="inode_guard",
            input={"replacement": "bundle_runtime"},
            assumptions=["source and destination may name the same inode"],
            ensures=["runtime artifact is independently loadable"],
        ),
    ],
    worker_entrypoint="repair_worker",
    evaluate=evaluate_candidate,
    propose=propose_candidate,
    max_candidates=2,
    context_prompt_view={
        "required_keys": ["requirement.target"],
    },
)
```

The library implements this semantic contract:

1. validate candidate identity, input, assumptions, and evidence obligations;
2. let the proposal choose only among admitted candidates;
3. branch every tried candidate from the same semantic frontier in a supervised
   scope, independent of whether the runtime forks a current parent or restores
   a retained repair checkpoint;
4. return a bounded rejected-candidate record when evidence is unsatisfied;
5. eliminate tried candidates and continue within the declared bound;
6. accept only a candidate satisfying final evidence; and
7. discard and reap every rejected or unresolved child; and
8. release the semantic frontier after acceptance or exhaustion.

`explore(checkpoint=...)` remains a compatibility entrypoint for a raw
parent-owned checkpoint. `explore_frontier(...)` is the policy-bearing
interface: it also preserves the same `context_prompt_view` admission contract
across both runtime lowerings. The typed entrypoint consumes the frontier for
that bounded search. `semantic_frontier_release(frontier)` is the explicit
lower-level lifetime operation when another control library owns the policy.
As a safety net, evaluation teardown closes child scopes first and then
releases every still-live frontier. This is resource cleanup, not exploration
policy: exceptions retain their original error identity, with any release
failure attached as related cleanup evidence.

Stone user functions now support keyword arguments so the policy-bearing call
is self-describing. In a frozen three-pair authorship comparison, the library
passed 3/3 first responses with no repair and reduced passing authored source
from 2,523 to 390 mean bytes versus explicit lifecycle control. See
[Stone Bounded Exploration Authorship Experiment](STONE_BOUNDED_EXPLORATION_AUTHORSHIP_EXPERIMENT.md).

Only after this ordinary-source form works across several task shapes should
Stone gain dedicated `explore` syntax. Any syntax must lower to the same
attempt, checkpoint, evidence, and cleanup operations rather than introduce a
hidden workflow engine.

See the Gateway repository's
[Caffe Bounded Stage Exploration Experiment](../../waymark-gateway/docs/CAFFE_BOUNDED_STAGE_EXPLORATION_EXPERIMENT.md)
for the exact claim boundary.

## Errors, Cancellation, And Resume

### Error Taxonomy

Model errors should eventually expose:

```text
model_policy_denied
model_unavailable
model_rate_limited
model_timeout
model_transport_failed
model_malformed_response
model_schema_validation_failed
model_cancelled
model_budget_exhausted
```

The record should include `retryable`, bounded provider detail, and
`retry_after_ms` when known, but never a credential.

### Cancellation

V0 model RPC is unary and occupies the control stream. The target device needs
Gateway operation handles so a long model call can be observed and cancelled
without blocking attempt control, matching the Linux provider operation model.

### Durable Resume

Durable agent control requires more than persistent messages:

```text
operation id
request digest
admission/policy decision
provider execution state
normalized result or terminal error
accounting record
```

Stone should eventually checkpoint serializable locals plus a program counter,
or replay deterministic control against recorded effect results. Until then,
restart means restart from an explicit program checkpoint, not transparent
continuation.

## Capabilities And Effects

Initial enforcement remains dynamic:

```text
model.call          irreversible budget effect
workspace.read      read-only
workspace.write     transactional
linux.exec          effect depends on delegated provider policy
attempt.fork        allocates isolated child state and budget
attempt.publish     canonical mutation, explicitly authorized
```

Later Stone program admission should declare required effects and budgets:

```text
requires model.call(class="agent")
requires linux.exec
modifies workspace("src/**", "tests/**")
budget model_calls <= 12
budget output_tokens <= 20000
budget child_attempts <= 3
```

Static declarations improve review and rejection, while Gateway checks actual
calls and attenuates child authority. Neither layer replaces the other.

## Alternatives Considered

### Keep The Rust ReAct Loop And Add Options

Rejected as the semantic center. It makes common behavior easy but preserves a
fixed orchestrator. Every new strategy becomes another Rust option or callback
surface, which is another harness rather than an agent program model.

### Add `agent { ... }` Syntax Immediately

Deferred. Declarative agent blocks can be convenient for prompts, tools, and
schemas, but they do not add necessary semantics to the first experiment.
Ordinary Stone plus typed inference will reveal which declarations recur
enough to deserve syntax.

### Adopt An Actor Runtime Inside Stone

Rejected for the first design. It duplicates attempt identity, supervision,
mailboxes, cancellation, budgets, and durable state. If lightweight in-attempt
actors later prove useful, they must have a precise relationship to the owning
attempt and must not receive independent authority implicitly.

### Require Native Provider Tool Calling First

Rejected as a sequencing dependency. Native tool calls are worth adding, but a
schema-validated action record can test Stone-controlled agents against the
current Gateway protocol now.

### Treat Any Model-Generated Stone As Trusted

Rejected. Stone parsing, typing, and constrained decoding reduce mistakes.
Gateway capabilities and attempt isolation remain the security boundary.

## Implementation Roadmap

### M1: Raw Model Effect

1. Add Gateway-backed `model_call` to Stone.
2. Validate message and option records with structured Stone errors.
3. Add complete `help("model_call")` documentation and executable examples.
4. Add fixture tests, local fake-provider tests, and a real local-vLLM smoke.
5. Attribute usage and latency to the attached attempt trace.

Exit criterion: a checked-in Stone script performs two model turns through
Gateway in Waymark LibOS without receiving provider credentials.

### M2: Ad-Hoc Agent Authorship And ReAct Parity

1. Have GPT-5.5 synthesize an ad-hoc JSON-action ReAct program from compact
   Stone help, then repeat with `gpt-5.6-terra` and `gpt-5.6-luna`; record
   each admitted source and author-model identity.
2. Implement the current JSON-action ReAct behavior as checked-in Stone source
   for a reusable baseline.
3. Expose structured `task_spec()` and `task_input()` to reusable Stone
   programs; do not inject a hidden task prompt.
4. Run the same deterministic model-response fixtures through the Rust and
   Stone loops.
5. Match tool observations, turn/round limits, completion, and failure cases.
6. Keep the Rust frontend as compatibility code during comparison.

Exit criterion: the synthesized and checked-in Stone agents match the fixed
loop on deterministic conformance, and synthesized programs from at least two
outer-agent models pass a task end to end without a runtime code change.

The first GPT-5.5 authorship observation and its deliberately limited
interpretation are recorded in `STONE_AGENT_AUTHORSHIP_M2_PILOT.md`. That pilot
does not yet meet this exit criterion.

### M2.5: Composable Agent And Attempt Control Surface

1. Define the common `AgentControl` protocol and drive both Rust/builtin ReAct
   and Stone-defined controls through it.
2. Add ordinary Stone adapters for tools, context, budgets, retry, event hooks,
   verification, critic, sequencing, and fallback.
3. Launch multiple named entrypoints from the current or another Stone module
   using the Gateway spawn syscall, without embedded child source strings.
4. Add typed attempt handles/outcomes and structured child scopes with
   wait-any/wait-all, join, and automatic cancel-then-join cleanup.
5. Expose aggregate budget/usage, attempt-owned operation history, and bounded
   child progress events as structured values.

Exit criterion: an outer model writes one Stone module containing a parent and
multiple child entrypoints; the parent launches a bounded portfolio, observes
and joins every child, accepts one result, discards the others, and exits with
no manual source-string construction or leaked lifecycle state.

The M2.5 implementation now establishes the control and supervision seams:

- `waymark-runtime` exposes a public Rust `AgentControl` trait and
  `AgentSession::run_control` driver;
- the optimized JSON-action ReAct loop and deterministic scripted controller
  both implement that trait;
- controls can wrap/delegate to another control over the same session, with a
  deterministic trace conformance test;
- Stone exposes `agent_session()` plus `help("agent_control")`, so an ordinary
  `def control(session)` receives structured task, input, current attempt,
  typed context-prompt admission view, limits, and discoverable resource-tool
  families.
- `react_control(...)` and `scripted_control(...)` construct opaque,
  task-owned `agent_control` values inside Stone. They are ordinary callables,
  survive warm evaluations, and can be captured and delegated to by Stone
  functions and lambdas.
- invoking an optimized control delegates to the same public Rust
  `AgentControl` contract over a nested Stone guest and the current resource
  capabilities. The value is an implementation handle, not an agent-program
  IR, and the Gateway still sees only resource syscalls.
- `examples/scripts/native_react_control.stone` is the boundary canary: its
  fixture-backed LibOS run exercises admitted Stone -> callable control ->
  Gateway `model.call` and returns the model-selected structured final value.
- `attempt_scope(...)` now creates an opaque task-owned supervision value.
  Passing `scope=scope` to `attempt_spawn` or `attempt_fork` registers the
  child; `attempt_join` distinguishes controller completion from the still
  active candidate-selection state; accept/discard resolves scope ownership.
- `attempt_scope_close` performs bounded cancel-then-join cleanup and rolls
  back every unresolved child. The evaluator closes all remaining scopes on
  both successful and exceptional exit, and reports incomplete cleanup as a
  primary or related structured error rather than silently leaking it.
- `examples/scripts/attempt_scope_cleanup.stone` and
  `attempt_scope_error_cleanup.stone` are LibOS/Gateway boundary canaries. The
  former proves explicit join and rollback; the latter intentionally fails
  after spawn and proves automatic child rollback while preserving the
  original declared failure.
- `current_program()` returns the exact currently loaded Stone module as the
  existing structured Gateway program argument. `attempt_spawn` and
  `attempt_fork` accept `entrypoint=...`, and the LibOS task adapter now
  preserves `StoneProgram.entrypoint` plus structured task input.
- named-entrypoint modules currently allow only top-level `def` and `pass`;
  this makes child startup unambiguous and prevents parent bootstrap effects
  from running in a worker. Entrypoints accept zero arguments or one structured
  task input.
- `examples/scripts/attempt_module_entrypoints.stone` is the process-tree
  canary: root `main(input)` forks `worker` from the same admitted source,
  joins it, accepts its workspace, and exits with both attempts cleanly closed.
- `attempt_spawn` and `attempt_fork` now return nominal `attempt_handle`
  runtime values. Direct attributes remain record-shaped for compatibility,
  while `type(child)` distinguishes a handle and untyped Stone functions
  preserve it without stringifying or flattening it.
- `attempt_join` and `attempt_wait_any` return immutable `attempt_outcome`
  snapshots. The outcome keeps compatibility fields and separates execution,
  reported result, evaluation, selection, and cleanup views; unavailable phases
  are explicit (`not_evaluated`, `pending`) rather than inferred as success.
  Terminal wait/join snapshots parse the child's compact returned value into
  `outcome.result.value` or `outcome.result.error`. Ordinary info/list records
  omit report bodies, so comparison is explicit without making every lifecycle
  observation carry child payloads.
- `attempt_best(scope, objective="max" | "min")` is a task-owned selection
  capability layered over those outcomes. `attempt_best_consider` admits only
  a joined successful child owned by the same scope, retains at most one full
  outcome, immediately rolls back a rejected candidate or replaced incumbent,
  and keeps evidence/artifact references separate from its numeric selection
  score. `attempt_best_accept` transfers the retained workspace through the
  existing parent/child accept operation; finalization drops the full outcome
  and leaves only bounded identity/score diagnostics. This makes the runtime,
  rather than model-written bookkeeping, own the common “best observed so
  far” invariant and cleanup obligation.
- `attempt_accept` returns nominal `attempt_acceptance`, not an untyped import
  record. It preserves the structured parent/child diff report, exposes the
  selected attempt id directly, and keeps `accepted.selected` as a nominal
  `attempt_handle`. User functions may annotate `attempt_handle`,
  `attempt_outcome`, `attempt_scope`, `semantic_frontier`, and
  `attempt_acceptance`; mismatching acceptance and handle values fails at the
  function boundary with lifecycle-specific repair guidance. The compact
  runtime tags are intentionally suitable for later JIT specialization.
- `attempt_inspect(child, include_details=False, trace_limit=20,
  max_bytes=32768)` is the bounded drill-down path. It returns the compact
  summary, optional archived controller envelope, a relevant trace tail, and
  live controller/operation/run state. Accept/discard close the child
  transaction and reclaim execution resources; report sidecars and trace
  remain inspectable afterward.
- `attempt_wait_any` and `attempt_wait_all` use the Gateway
  `AttemptWaitSet` syscall. The Gateway observes all controller processes as
  one wait set, so Stone does not implement biased sequential polling.
- `examples/scripts/attempt_wait_set.stone` is the composition canary: one
  admitted module forks two named workers, passes the winning typed outcome
  through an ordinary Stone function, accepts it, waits for and discards the
  remaining child, and closes the supervision scope.
- named Stone functions are now first-class callable values, so adapter
  arguments can use `dispatch_action` directly rather than requiring a lambda
  wrapper;
- `examples/scripts/standard_attempt_agent.stone` is the first ordinary-source
  control library. Its `standard_agent_control(...)` loop composes typed
  inference with replaceable dispatch, verification, and progress adapters,
  bounded observations/history/resources, recoverable tool errors, same-key
  memory, and compact result provenance.
- `examples/references/standard_attempt_fork_portfolio.stone` composes that
  library into a complete parent/child portfolio: negative spawn baseline,
  two typed-view forks, retained child inspection, deterministic selection,
  accept/discard, and post-cleanup reclamation checks.
- `examples/references/attempt_best_canary.stone` recreates the late-search
  failure shape observed in the FastText experiment: the winner is the middle
  of three candidates and a worse candidate arrives last. The runtime replaces
  and reclaims the baseline, rejects and reclaims the late candidate, accepts
  only the retained winner, releases its full outcome, and closes with no live
  transactions. A Terra authorship probe selected every new lifecycle builtin
  from its compact help, without seeing the completed canary. Its first source
  used `raise "..."`, exposing a language rather than ownership failure;
  preliminary structured `raise` support let that unchanged source complete
  selection, import, and cleanup correctly. A fresh authorship run then used
  Python's mistaken `except error:` shape while trying to inspect a released
  selector. Stone still does not add divergent binding semantics for that
  spelling. Admission now returns an `exception_binding` correction with the
  two intent-preserving repairs (`except Exception as error:` or `except:`),
  marks execution `not_started`, and requires the author to choose rather than
  silently rewriting ambiguous control flow.

This closes the native-to-Stone invocation seam. Forked named entrypoints now
also receive structured per-fork input through ordinary `input=...` syntax.
The first visible agent-control library covers the common single-attempt
decision loop, but Stone does not yet provide compositional wrappers such as
`with_tools`, `with_context`, aggregate budgets, critic/fallback policy, or
event hooks. Verifier-populated evaluation outcomes and aggregate
budget/event views also remain. Those are the remaining M2.5 composition and
attempt-tree ergonomics, rather than another agent execution representation.

### M3: Typed Inference

Implemented:

1. Waymark validates a strict, documented JSON Schema subset before and after
   the model effect.
2. `model_infer` provides explicit repair with at most four retries; each
   attempt is an ordinary traced `model_call` transition.
3. Successful inference returns aggregate usage and bounded validation
   failures; exhausted inference includes the same bounded summary in its
   structured error while per-call accounting remains in Gateway trace.

Remaining: map the canonical schema to provider-native structured output where
supported. The portable path currently injects the schema into visible
messages, requests JSON-object output, and relies on independent runtime
validation for authority.

Exit criterion: malformed outputs never enter Stone control as typed values;
repair behavior and cost are fully visible.

### M4: Neutral Native Tool Calls

1. Extend the Gateway protobuf with typed tool definitions, calls, and results.
2. Normalize OpenAI Responses and compatible chat provider formats.
3. Add Stone helpers for tool messages without adding a mandatory dispatcher.
4. Preserve text/JSON action programs as a compatible strategy.

Exit criterion: one Stone program runs unchanged against OpenAI and vLLM native
tool calling, subject to provider capability support.

### M5: Durable Control And Effect Types

1. Give model operations durable ids and nonblocking handles.
2. Add attempt-scoped model budgets and concurrency admission.
3. Checkpoint or replay Stone control state without repeating effects.
4. Add program effect/capability declarations and typed task/result contracts.
5. Publish a constrained Stone grammar/schema for model-generated programs.

Exit criterion: a controller crash during a model/tool loop can resume without
duplicating completed model or Linux effects, and authority amplification is
rejected at admission or call time.

## Validation Plan

### A. Language Mechanism Conformance

Use a deterministic fake model sequence to test:

- message ordering and exact prompt transparency;
- structured output parsing and schema rejection;
- tool success, tool failure, and observation feedback;
- turn exhaustion and explicit finish;
- model timeout/rate-limit classification;
- credential absence from Stone values and traces.

This establishes semantics, not model usability.

### B. Outer-Agent Authorship And Repair Test

Give GPT-5.5, `gpt-5.6-terra`, and `gpt-5.6-luna` identical task context,
`help("model_call")`, and a compact Stone language reference. In isolated runs,
ask each model to write:

1. a one-tool ReAct loop;
2. a critic/revision loop;
3. a loop with a verifier before finish.

Measure parse success, valid model calls, invalid actions, repair iterations,
and whether the program completes under a fixed budget.

After each deliberately injected syntax, type, and effect error, return only
the normal bounded Stone diagnostic and permit one repair. Compare first-pass
validity, repair success, tokens spent on language guidance, and task success
with Python and the fixed Rust loop where meaningful.

This tests Stone's principal usability claim: another agent can author and
repair an ad-hoc task agent without runtime changes. Report results by author
model; do not pool them in a way that hides a model-specific interface failure.

### C. Programmability Versus Configuration

Freeze the fixed Rust loop and require a behavior change that is not an option:

- batch all read-only probes before mutation;
- invoke a critic only after a failed verifier;
- fork two candidate attempts and accept one based on evidence.

The treatment succeeds only if the outer agent expresses the behavior entirely
in generated or edited Stone without changing the runtime. This is the
decisive test that the result is a programming layer rather than another
configurable harness.

### D. Local Model End-To-End

Run the same small agent program with the local vLLM model at
`http://127.0.0.1:8001`. Use deterministic task fixtures first, then a few real
repository tasks. Record structured-output failure rate separately from task
failure; a smaller local model is useful as a stress test after the hosted
outer-agent cohort proves the interface is learnable.

### E. Attempt Composition

Run a Stone parent program that:

1. uses a model call to propose strategies;
2. forks child Stone programs for selected strategies;
3. waits for reported candidate evidence;
4. accepts one child and discards the rest;
5. verifies in the parent and returns without leaking lifecycle state.

Constructed tasks are acceptable for mechanism conformance. Claims about
spontaneous usefulness require untouched, historically unresolved Terminal-
Bench tasks where uncertainty arises naturally.

### F. Comparative Outcome Experiment

After A-E pass, compare on a frozen unresolved Terminal-Bench cohort:

```text
B: existing agent + blind shell
C: existing agent + explicit transaction controls
E: Stone-authored agent program + implicit attempt support
```

Keep model, task instructions, time, token budget, Linux image, and verifier
fixed. Gate E on actual `model_call` use, a recorded digest and source for the
admitted Stone program, attempt conformance, official verifier completion, and
clean lifecycle state. Do not require one fixed program identity when the
treatment intentionally synthesizes a task-specific program.

Primary outcome remains official task success. Secondary measures include
model/tool calls, invalid operations, repeated work, recovery after failure,
wall time, tokens, retained evidence, and cleanup.

This experiment addresses the larger hypothesis that attempt-first OS support
helps agent computer use. A successful Stone ReAct demo alone does not.

## Immediate Next Step

M1 and the M2 mechanism gate are complete. The evidence and remaining claim
boundary are recorded in `STONE_AGENT_AUTHORSHIP_M2_PILOT.md`.

The compact standard visible control now passes the three deterministic and
real-model action baselines with the exact ten-call trajectory, including
failed-tool recovery. Its result/progress/trace split is sound, and separating
the compact model-facing contract from the complete runtime validation schema
reduced real-model input below the smaller typed loop. Terra and Luna also
authored and executed named verifier specializations from the compact adapter
contract. The first Terra probe exposed one learnability gap—record mutation
requires item rather than attribute assignment—which the bounded language
guide now states explicitly.

A subsequent simple repository task provides the first causal task-policy
evidence: a Terra-authored verifier plus a real Terra inner loop produced exact
externally checked workspace bytes, while the same admitted verifier rejected
a plausible wrong-output fixture with its declared verification error. Both
paths preserved the expected trace, bounded same-key progress, and rollback.

The executable-verifier gate now passes too. Terra authored a bounded visible
test adapter without access to the repository, answer, or hidden tests. A real
Terra inner attempt passed that test and an independent host-owned verifier in
a rolled-back checkpoint branch. A wrong implementation was rejected by both
layers. This establishes the intended feedback/authority boundary on one
constructed task, not broad task competence.

The first generic-control TBv2.1 engineering cell now passes as well. The
unchanged standard Stone source solved untouched `portfolio-optimization` in
eight decisions and earned official Harbor reward `1.0`. The source was not
task-specialized and used no forks. Exact source/admission hashes, guarded
binary publication, official-verifier handoff, bounded progress memory, and
clean lifecycle closure all passed. Earlier same-task cells informed generic
harness fixes, so this is mechanism evidence rather than an unbiased causal
comparison.

An exact-source cross-task replication on untouched `mcmc-sampling-stan` was
conformant but earned reward `0.0`. It passed the two numerical-result checks
but failed RStan installation and end-to-end sampling. The controller retained
only one same-key progress/counter item and accepted a non-empty finish claim;
it had no structured public-requirement ledger, per-requirement evidence, or
independent completion critique. This separates clean mechanism delivery from
task-policy adequacy.

Standard control V1 now implements that next slice. It retains one bounded
public-task requirement, a fixed evidence ring, a same-key semantic audit, and
a fresh completion critique that may veto finish and return a repair
objective. It receives only public requirements and recorded visible evidence,
never hidden verifier content, and the total model-call budget reserves room
for it.

Three deterministic Gateway/LibOS cells provide causal mechanism evidence. A
premature finish caused one missing read in the no-critic ablation; treatment
rejected it, induced the read, and then approved. A claimed approval containing
an unsupported requirement was also normalized to rejection. Every cell
rolled back and treatment retained five latest-state memory items. See
`STONE_STANDARD_AGENT_COMPLETION_CRITIQUE_EXPERIMENT.md`.

The small real-model comparison now passes as well. Terra rejected missing
read-back evidence, approved complete write/read evidence, and rejected a
failed RStan execution with a concrete dependency/run repair objective. Each
case used one model call and at most four retained memory items. The first
failed-execution probe also found a protocol integration bug—failed evidence
must use status `contradicted`, not `rejected`—which is fixed and source-tested.
See `STONE_STANDARD_COMPLETION_CRITIC_QUALITY_EXPERIMENT.md`.

The Caffe V35-V37 sequence adds evidence about repair control. A typed
single-shot model choice failed because its projected observation omitted an
applicability fact. Bounded exploration then rejected that candidate from a
retained post-build frontier and tried the remaining candidate without
replaying compilation. Independent evaluation of the saved artifact fixed a
second evidence-boundary error, after which the official verifier passed all
six checks. This supports the control shape; it does not establish broad
task-level reliability.

The visible bounded-exploration library now passes its deterministic mechanism
gate, a three-pair authorship comparison, and an unchanged-library transfer to
the official Caffe task. In the Caffe cell it rejected the model's first
candidate, reused the post-build frontier, and passed all six checks. The next
mechanism cell generalized that argument into a nominal `semantic_frontier`
and one `attempt_branch` operation. The same source call passed against both a
parent-owned fork checkpoint and a retained failed-owner repair checkpoint;
the runtime selected the kernel operation, exposed deterministic seal-cost
guidance, counted frontier use, and left no open transaction or checkpoint.

A matched three-pair authorship cell then reached 3/3 eventual success with
both the raw and typed interfaces and emitted no redundant seals. Typed
programs averaged 42.7 rather than 53.7 lines, but required one repair while
raw programs required none; the frozen aggregate hypothesis was therefore not
supported. The repair was caused by rebinding a child handle to the
`attempt_accept` report, not by frontier selection or lowering. Keep the
frontier abstraction as a source/kernel boundary; next improve nominal
lifecycle-result ergonomics before claiming an authorship advantage or moving
to a task-shaped transfer.

The lexical-resource experiment adds that ergonomics without a new Gateway
ABI. `with` owns `attempt_scope` and `semantic_frontier` values through normal,
return, and exceptional exit; methods lower to the existing lifecycle kernel.
The first authorship run exposed a Stone bug rather than an ownership failure:
`with` hid body bindings unlike Python. After separating name visibility from
resource authority, corrected scoped syntax passed 3/3 first responses with no
repairs and was slightly smaller than typed-functional control. Leaving a block
now revokes the resource but preserves the handle/evidence for finalization.

A narrow async follow-up admits `async def`, attempt-resource `async with`, and
`await` on joins, wait sets, and acceptance. It also passed 3/3 first responses,
but was slightly larger than synchronous scoped syntax. The interpreter
currently lowers these markers to blocking Gateway effects; this is useful
effect notation, not a general coroutine system. Keep synchronous scoped
control as the block-shaped default, the functional forms as the explicit
kernel surface, and async experimental until actual operation overlap requires
it. See [Scoped Attempt Resources Experiment](STONE_SCOPED_ATTEMPT_RESOURCES_EXPERIMENT.md)
and [Async Attempt Control Experiment](STONE_ASYNC_ATTEMPT_CONTROL_EXPERIMENT.md).

A later run-control slice gives the marker concrete value without adding a
general scheduler: `await run(...)` lowers to the existing runtime-owned
`run_complete` lifecycle. The standard V12 controller exposes the same policy
as one `run_complete` action, reserving manual run handles for intentional
overlap or services. This removes model decisions whose only purpose was to
repeat a bounded status/wait poll. In a matched `mcmc-sampling-stan` pair, V11
spent 14 decisions observing runs and exhausted its 32-round budget before a
required dependency-repair rerun. V12 made zero model-driven run observations,
used three runtime wait events, preserved enough budget to repair and verify
the task, and earned official reward `1.0`. One pair is not a broad estimate,
but it validates the intended mechanism on a previously failing task.

The unchanged V12 controller then failed `make-doom-for-mips` without making a
single polling decision. It spent 29 actions inspecting and deferred its first
build until the last action, where two repairable linker errors appeared. This
sharpens the architecture boundary: runtime-owned effects make execution
cheaper, but do not define useful progress. A task harness needs typed stage
obligations, per-stage action budgets, and early evidence gates so exploration
must produce an executable build attempt while repair budget remains. Retained
semantic frontiers become useful after such a stage succeeds; they cannot
compensate for never attempting it.

An authorship follow-up compared `@stage`, `workflow`/`stage` blocks with header
evidence, and blocks with body-level `ensure` contracts. All three surfaces
were authored correctly in 3/3 first responses and independently inferred a
bounded early build stage, so constructing a graph is not required. The
contract form was shortest and was the only arm to represent the public stdout
obligation consistently. The remaining shared error was an incorrect workspace
path caused by omitting generic path-resolution context from the authoring
call. The preferred near-term spec is therefore executable Stone code with
ordered stage blocks, optional stage-scoped `agent_loop()`, and re-checkable
`ensure` contracts; graphs remain a derived runtime view. See
[Stone Staged Harness Syntax Experiment](STONE_STAGED_HARNESS_SYNTAX_EXPERIMENT.md).

The first staged Doom rollout sharpens what “typed” must mean. Stage names,
budgets, and artifact gates successfully forced real execution, while bounded
diagnostic-tail memory enabled a model to repair a missing-header build and
reach the VM stage. But a prose-only inspection decision could still advance
without discovering the toolchain or backend. Stone now permits
`decision_recorded(fields=[...])`: the spec names downstream-required findings,
the visible controller must produce them, and the runtime gates transition on
their presence. A stage contract should constrain the semantic interface to
the next stage, not merely certify that an action occurred.

The same rollout argues for an explicit action-state controller in the standard
library: explore, execute/repair, and a bounded diagnose-after-gate-failure
transition. Tool names continue to describe execution mechanics. They should
not silently double as policy types; doing so rejected valid package-install
commands and consumed repair budget.

The typed-decision surface also passed a fresh 3/3 blind-authorship canary at
low reasoning. All drafts named concrete Doom inspection findings and passed
the tightened semantic gate without repair. This supports interface
learnability; it does not show that the inner agent's findings are correct or
that the task completes.

The first task execution with that surface made the distinction concrete: a
required, non-empty `toolchain` finding could still report that the answer was
unknown. Typed shape is useful, but a stage boundary also needs explicit
resolution state and a bounded observation basis. The run later reached a
repairable linker failure after exhausting its normal build actions. Treat
repair reserve as a separate control resource; merely raising an
undifferentiated stage budget weakens the pressure to execute early.

Stone is experimenting with that distinction as
`decision_recorded(resolved=[...])`. Findings are bounded records with
`state`, `value`, and `basis`. An `unknown` record is valid information but not
sufficient transition evidence, so uncertainty remains visible without
allowing the next stage to consume it as a resolved interface. The basis is
semantic provenance for control and debugging, not a replacement for fresh
artifact or verifier evidence.

The first Doom cell with this interface stopped at inspection with two resolved
and two unknown findings, zero Linux effects, and clean resource state. This is
the desired failure direction: an incomplete semantic interface no longer
silently advances. The trace also shows that partial state publication and
observation compete for one flat action budget. A subsequent control primitive
should make incremental finding state cheap to retain, or account for it
separately, while preserving the model-call cost in observability.

The follow-up makes findings runtime-owned stage state. Controllers publish
small semantic deltas after observations; the kernel validates and merges them,
adds action-derived provenance, projects the accumulated map into the next
decision, and gates transition on the whole map. Final decisions no longer
restate state the runtime already owns. This is the same ownership rule used
elsewhere in the system: the model proposes meaning, while the runtime owns
lifecycle, typing, provenance, and bounded retention.

Typed state must also constrain control. Offering `decide` while required
fields were unresolved produced fluent but evidence-free decisions. The
standard controller therefore projects action availability from the current
finding type state: unresolved fields admit probes, resolved fields admit a
decision, and the final bounded action admits an honest partial report. The
model used this interface to retain one field on the Doom task, but failed to
choose probes for the other three. The next useful PL construct is therefore a
field-directed probe/evidence-acquisition interface, not another finding record
shape.

That interface now has a minimal executable form. A resolved decision contract
may describe each field with `{kind, question}`. The controller tags each
inspection action with `for_field`, while the kernel retains a bounded probe
record and exposes it to subsequent decisions. `resolves={field: value}` is
admitted only after an observation for that field and receives runtime-derived
provenance. This keeps the model responsible for semantic interpretation while
preventing it from inventing the observation source.

The first comparison changed control behavior as intended: the model probed
all four Doom obligations instead of one. It resolved only `source_layout` and
honestly left the other three open. The remaining failure clarifies the next
layer. `for_field` answers *what to investigate*, but generic bounded `read` and
`run_linux` do not ensure that the observation answers the field's question.
Typed acquisition needs a refinement state, focused operations such as
search/tail, and retry accounting. The runtime should distinguish `observed`
from `sufficient`, rather than treating one action per field as a complete
acquisition policy.

The focused-acquisition slice implements that mechanical layer. The visible
controller now exposes recursive `find`, bounded literal/regex `search`, and
offset, tail, and streamed line-range reads. Per-field probes carry bounded
revision/provenance history; fields without observations are covered before
refinement, and empty output is retained as `insufficient`. A local Doom-tree
probe (95 C files, symbol search, and a 4 KiB tail) took about 40 ms, so native
Rust acquisition is cheap relative to model calls.

The updated Doom cell completed inspection and produced a valid MIPS ELF before
failing at runtime. That advance validates the control hypothesis, while the
failure sharpens the remaining distinction: `observed` can be checked
mechanically, but `sufficient` is relative to the declared question. A filename
and a compiler path do not prove that the program satisfies the JavaScript VM's
freestanding ABI, entry, and frame-output contract. Future finding descriptors
should therefore admit task-authored evidence predicates or typed acquisition
recipes, and multi-contract gates should direct repair at the still-missing
contract without discarding already-satisfied evidence.

The first task-authored predicate is deliberately small:
`evidence={tool?, contains?, success?}` on a resolved finding descriptor. The
kernel checks it against the full acquiring outcome, but retains only bounded
summaries and the exact required strings that matched. Failed checks produce a
structured insufficiency reason and cannot be bypassed through legacy finding
updates. This gives the task harness a mechanical lower bound on evidence
quality while leaving semantic interpretation with the model and final task
truth with artifact, command, and verifier contracts.

Both authorship and execution refined the design. Models learned the construct
once the prompt showed its complete syntactic parent and enumerated valid
finding kinds. Full-outcome checking avoided false negatives caused by compact
memory, while a toolchain predicate showed that `success=True` is not
universally desirable: command failure can prove absence and motivate a
specific install strategy. On Doom, the corrected predicates resolved source
layout, toolchain, frame backend, and VM runtime contract, and inspection
advanced honestly.

That success exposed a missing programming-model boundary. The build context
received only the name of the completed inspection stage, not its verified
typed findings. It consequently rediscovered facts and exhausted its budget
on a dependency inspection had already named. Completed-stage findings should
be explicit, bounded stage outputs projected into later workflow contexts.
This is ordinary typed dataflow between process phases, not an incidental
memory-search optimization; the harness should be able to name and constrain
the interface while the runtime owns retention and provenance.

Stone now expresses that boundary with `inputs=["inspect"]` on a dependent
stage. Inputs must name earlier stages and are capped at eight. The workflow
kernel projects only their declared resolved findings into
`step.stage_outputs`, preserving `kind`, `state`, `value`, and runtime-owned
`basis`. The standard controller injects that typed record directly; general
semantic memory is no longer the correctness mechanism for a declared stage
interface. An ordered workflow therefore remains code-like while gaining
explicit dataflow edges where they matter.

The construct was authored correctly in the first blind canary. On Doom it
moved the frontier from a 12-action build failure to a nine-action successful
build and the known VM run failure. It did not answer facts the prior stage had
failed to declare precisely: a finding that says `my_stdlib.h` is required
does not say where it lives or how it should be implemented. This separates
three obligations cleanly: acquisition predicates establish a lower bound on
evidence, typed stage outputs preserve the interpreted result, and later
artifact/verifier contracts test its consequences.

Consumers can now strengthen `inputs=["inspect"]` to a selective interface
such as `inputs={"inspect": {"source_layout": "path"}}`. Workflow admission
checks that the producer declares the field with the same kind, and runtime
projection omits unrequested findings. The all-fields list remains the default
for small boundaries. A blind low-reasoning author used the stricter form
correctly on its first response and narrowed the run stage to one of four
inspection fields. This is evidence for an optional typed module boundary,
not yet evidence of improved end-task success; its immediate value is earlier
diagnosis and bounded context, with a useful static shape for a future JIT.

The first selective-input task follow-up did not reach the stage where
selection differed: three conformant Doom cells completed inspection and
failed build, whose four-field projection is equivalent to import-all. The
useful evidence came from repair behavior. The controller lacked Stone's
exact-match edit operation, and its compact memory accidentally kept the front
of an already-bounded process tail. Standard control now exposes typed
`edit(path, old, new, all?)`, treats failed `run_linux` like failed
`run_complete` for diagnostic reopening, and preserves the terminal N
characters of process diagnostics. A live v34 cell used exact edit and then
reacted to successive concrete compiler/linker errors instead of manually
recovering a hidden tail.

That cell still stopped at a `__start` collision. A follow-up tested one flat
`startup_strategy` obligation over backend `__start`, VM-selected `main`, and
CRT policy. A fresh low-reasoning model authored and typed the field correctly
on its first response, but execution exposed the boundary: arbitrary
`run_linux` output can satisfy a literal predicate by echoing its markers. The
decision step correctly rejected that weak basis and kept the field unknown.

The concrete missing type is therefore a derived finding, not unconstrained
nested data. Its runtime-owned basis should name earlier acquired fields, and
its value should be synthesized from their retained provenance rather than a
new arbitrary probe. The same run spent five probe actions and one decision on
a six-action stage, showing that budget inference or diagnostics should account
for obligation count and refinement slack.

A stage-aware shell experiment tested the complementary acquisition problem.
The runtime now returns contract-relative warnings and recommendations for
truncated output, empty probes, self-generated marker text, likely literal
regex mistakes, and repeated insufficient revisions. The controller exposes
budget pressure and a suggested least-revised field. In the advisory v38 cell,
the same harness resolved three of five fields with provenance, compared with
zero in v35.

Recommendations must remain below the authority boundary. V37 placed the
suggested field into the action schema and rejected four reasonable deepening
actions. V38 allowed every unresolved field while retaining advisory focus and
avoided that failure. Schemas should enforce safety and type validity; dynamic
acquisition order is harness policy. Neither form resolved an absent toolchain
or the derived startup decision, which still require explicit environment or
contract transitions rather than more prompt context.

The next slice makes the environment path explicit. `ensure_command` appears
only when a finding contract selects it as the evidence tool. It checks for the
command, may run one harness/model-supplied provisioning argv, and verifies
availability before returning a structured outcome. This collapses
check/install/recheck polling into one action without adding authority:
Gateway still controls the underlying command effect. A failed or denied
transition remains an insufficient finding with a concrete recovery path; it
cannot be mistaken for acquired capability evidence.

The matched Doom rollout supports this surface. On its first opportunity the
model chose `ensure_command`, mapped the missing command to
`gcc-mips-linux-gnu`, and the runtime applied and verified the install in one
action. The toolchain finding resolved with runtime provenance. The attempt
still failed inspection because the transition occurred at action 11 and two
other obligations were untouched. This separates two concerns: typed effects
make one environment transition reliable, while obligation scheduling and
derived-finding semantics determine whether the attempt spends its budget on
the right transitions.

Runtime representation matters to viability. Storing another `Vec` inline in
the recursive runtime-value enum caused the large standard controller to cross
the test-thread stack limit. Stage goal/input metadata is now boxed as one
owned unit, shrinking the enum rather than making every value pay for the new
feature. At experiment termination, the parent runner also discards retained
repair checkpoints after recording their trace, preserving inspectability
without leaking resources.

Runtime diagnostics are part of that interface. A generic JSON `oneOf` error
did not help the model repair an illegal action. Validation now reports allowed
discriminators and the nearest branch's missing or extra fields, and the
stage-specific schema hint states the actions currently admitted. This is a
general design rule for Waymark: rejected effects should explain the available
recovery path, not merely expose an error code.

Verifier-populated outcomes and aggregate budget/event views remain necessary.
See
[Caffe Bounded-Explore Library Experiment](../../waymark-gateway/docs/CAFFE_BOUNDED_EXPLORE_LIBRARY_EXPERIMENT.md).
Compatibility ids remain accepted at syscall boundaries, but new Stone control
should retain handles. Do not treat the mechanism, specialization, or
single-task cells as validation of the broader attempt-first OS hypothesis.
