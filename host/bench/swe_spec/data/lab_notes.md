# Experience log for a Stone core calculus

Raw notes from writing and running the Q2 programs (swe_typed, swe_untyped,
swe_wf). Each entry: what happened, then what a core calculus would need.
To be folded into the design doc (11.4 / a new section) once experiment 2
reports.

## Inputs and statuses (H-16)

1. Luna returned a well-formed "done" record whose content was invented
   ("changed X, tests pass"). Shape checking passed; content was a claim.
   -> Model output needs a status separate from its shape: a record of type
   T from the model is a *claimed* T.
2. Haiku's reproduction script was a proposal; running it established "it
   fails before and passes after" (tested), not "it captures the issue"
   (judgment). 11 false "verified" came from treating the second as the first.
   -> At least four statuses appear in practice: observed, verified (claim
   checked), tested (proposal run), judgment (unchecked or cross-checked).
3. In swe_wf the status is a string in a dict. The program can write any
   status; nothing stops it calling a judgment "verified".
   -> Status should be a type qualifier that only host checks can raise,
   like Elba's `Trusted`.

## Stages and data flow

4. Kernel-form workflow stages communicate only through a shared mutable
   dict captured by the handlers (`state["diagnosis"] = found`). Admission
   cannot see that data flow. Only the block form with findings has typed
   handoff (`inputs=` with kinds).
   -> Stages as functions with typed inputs and outputs, so the plan's data
   flow is explicit and checkable.
5. Evidence is a bool, a summary string, and reference strings. Statuses
   were encoded as string prefixes (`verified:files-exist`).
   -> Evidence should carry typed facts, each with its status.
6. The independent-reproduction check backs a judgment. When it cannot be
   established, the run should go on with the judgment unchecked. The kernel
   has only satisfied/unsatisfied, so "unavailable" had to be reported as
   satisfied.
   -> Two kinds of checks: *gates* (the plan cannot proceed without them)
   and *status raisers* (success raises a fact's status; failure leaves it
   lower). The second kind needs a three-way outcome.
7. Evidence is checked before a stage's first action (`evidence_pre`) and
   after each action (`evidence_post_action`). The pre-check is where the
   environment stage probes its assumptions (git readable, tree clean,
   python runs).
   -> Stage preconditions are environment assumptions, probed on entry;
   postconditions are expectations, checked on exit.

## The environment's specification

8. `help('workflow_stage')` lists `inputs=`; admission rejects it.
   -> Tool signatures are the environment's specification in the calculus.
   They must come from one table that help, admission, and the checker share.
9. Four environment assumptions failed silently and cost budget or a run:
   git ownership, sympy test commands, an incomplete repository copy, and
   output truncated at `max_stdout_bytes`.
   -> Every environment fact the plan relies on should be either probed or
   listed as an assumption. A truncated observation is not a complete one,
   and its type should say so.
10. `run()` returns a failed result with `ok=False`; nothing forced the
    program to look.
    -> Results of effects that can fail should be consumed: a "must use"
    rule, or a result type whose failure case must be handled.

## Budgets and stopping

11. Budget exhaustion ends the program by design and cannot be caught. A run
    lost its patch because the loop did not check `model.remaining`.
    -> Outcomes include "unfinished" as a first-class result. The static
    check can see a model loop with no budget test before code that saves
    results.

## Specifications and goals

12. pytest-7205: the program's regression check (existing tests keep
    passing) pushed Haiku away from the behavior the hidden tests expect.
    The issue text does not settle which is right.
    -> The goal type comes from the task. When the task is underspecified,
    "correct" is itself a judgment, and no calculus removes that. It can
    only list it.

## Error quality (not calculus, but feedback)

13. `workflow("p", *stages)` fails with a raw parser dump
    ("unsupported expression: Starred(ExprStarred {...})"), not a fix.

## Experiment 2 (Haiku, 15 runs per arm, 5 tasks x 3)

Results: wf_status 4/15 resolved, 13 claimed verified, 9 wrong.
wf_check 3/15 resolved, 12 claimed verified, 9 wrong, about 8 more calls per run.
In every wf_check verified run the independent reproduction was established
and agreed with the first one; it predicted nothing (3/12 resolved).

14. A cross-check made by the same model from the same issue text is not
    independent. On sympy-12489 both reproductions tested one construction
    path (`p*q` returns the subclass); the hidden test wraps the whole suite
    in a subclass, and `Permutation.random` still returned the base class.
    On matplotlib-23299 the fixes handled the reported case (backend
    sentinel restored), while the hidden test wants rc_context never to
    restore the backend at all. Both reproductions shared the model's
    reading of the issue, so they failed together.
    -> A check raises a fact's status only when its evidence has a different
    origin from the claim. Facts carry an origin (environment, model run k,
    task text); two model outputs from the same input are the same origin.
    Model-made checks can at most move a judgment to "tested". Only checks
    against environment-origin oracles (existing tests, upstream behavior,
    the grader) raise it further. This is an information-flow rule over
    origins, not a rule about how many checks agree.
15. In wf_check, the program reported "the reproduction captures the issue:
    judgment, cross_checked: true" on 9 wrong runs. The status label was
    unsound in practice, because the program, not the runtime, decided
    what counted as a cross-check (entry 3 again, now with data).
16. The failures sit in the gap between the issue text and the hidden
    tests' intent (scope of the fix), in 4 of 5 tasks. A narrow fix passes
    every check the model can write. This is entry 12 generalized: the goal
    is a judgment whose origin is the task text, and no check in the plan
    has a different origin.
17. Regression commands written by the model can be vacuous: one run ended
    its baseline command with `; echo exit=$?`, so it always exits 0.
    -> A test command is a model-made artifact too. "Existing tests still
    pass" is observed only when the command is chosen by the environment
    (e.g. the repository's test runner on the changed module), not when the
    model writes it.

## Model-side assumptions (Qwen, thinking off)

18. Qwen never submitted a diagnosis in wf_status (2/2 runs): 20 exploring
    steps plus two 8-step retries, all reads and searches. In wf_check it ran
    out of call budget during the fix. Nothing was claimed falsely; the
    plan's step and call limits encode assumptions about the model ("can
    diagnose within 20 steps") that were false for this model.
    -> Step and call budgets are assumptions about the LLM input, the twin
    of environment assumptions. They should be declared with the stage and
    reported when they fail ("diagnose: model did not commit within 20
    steps"), so the plan can be revised (more steps, a forcing prompt,
    another model) rather than read as "task too hard".
    Qwen totals (5 tasks, 1 run each): every arm resolved 1/5 (pytest-7205).
    wf arms claimed "verified" 0 times; untyped claimed it twice, once
    wrongly (sympy). Most runs hit the 55-60 call ceiling.

19. Qwen's pytest-7205 fix was correct (the grader resolves it), but the
    program rejected it: "existing tests still pass" failed on
    testing/test_setuponly.py, whose expectations the benchmark's own test
    patch changes. Same conflict as entry 12, from the other side: Haiku
    narrowed a correct fix to satisfy the old tests; Qwen kept it and was
    refused.
    -> An environment-origin oracle is not automatically right about intent.
    Existing tests encode the old behavior, and the issue may ask to change
    it. So origin decides whether a check can raise a status, not whether
    the check is correct. A failed regression check should be evidence
    ("this changes tested behavior X"), routed to whoever owns the goal,
    rather than a gate that silently decides the goal.

## Correction (2026-11-09): the intent was in the environment

Entries 12 and 16 said the pytest and matplotlib issues do not settle what the
hidden tests want. Checking the sources shows they do:
- pytest-7205: the issue ends "Shouldn't that be using `saferepr` or something
  rather than (implicitly) `str()`?" That is the reference fix. The ambiguity
  came from the old tests disagreeing with the issue, and our regression gate
  sided with the old tests.
- matplotlib-23299: the issue gives only the symptom, but the codebase has a
  convention. `rcdefaults`, `rc_file_defaults` and `rc_file` all skip the
  keys in `STYLE_BLACKLIST`, which includes 'backend'. The reference fix makes
  rc_context follow the same rule.
- The other three were recoverable too: Django's `can_fast_delete` already
  treats signal listeners as needing full objects; "subclassing" (sympy)
  implies every constructor; Sphinx has more than one builder.
20. -> The failures come from anchoring on the reported symptom, not from
    missing information. The intent comes from text (the issue's own
    suggestion) and convention (how sibling code handles the same case).
    Executing code shows what the code does, not what is wanted. A workflow
    needs a step where the model states the general rule a fix must satisfy,
    with each part citing an observation (the issue line, the sibling
    function). The program then checks the fix against that rule on cases
    beyond the reproduction. Conflicts between the stated rule and old tests
    are reported, not silently decided by a gate.
