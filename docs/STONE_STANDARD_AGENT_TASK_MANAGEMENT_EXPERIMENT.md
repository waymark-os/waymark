# Standard Agent Owned-Task Management

## Question

Can the standard Stone controller expose the Gateway's existing long-run task
lifecycle in a form that an action model can use without losing owned process
handles during context compaction?

## V5 Interface

V5 keeps `run_linux` for short synchronous actions and adds four model-facing
actions:

- `run_start`: start a command as an attempt-owned task and return `run_id`;
- `run_status`: make a nonblocking observation of that handle;
- `run_wait`: wait for it under a bounded controller timeout;
- `run_terminate`: terminate and reap it under the existing bounded cleanup
  policy.

The controller retains active handles in one same-key
`progress.active_runs` memory item. The list is capped at four by default and
eight at the policy maximum. Terminal handles are removed. A finish action is
rejected while the list is nonempty, and budget exhaustion invokes bounded
reaping before the controller reports failure.

This is an interface over the existing Gateway lifecycle, not a new scheduler.
The harness remains authoritative for operation ownership, fencing, recovery,
and final reclamation.

## Results

Three focused checks passed:

1. A deterministic Gateway/Firecracker canary started a real background
   process, observed it by `run_id`, waited to terminal success, verified its
   workspace output, changed active-run memory from active/one to
   verified/empty, reported success, and rolled back.
2. A deterministic controller fixture attempted an early finish and a second
   start at a one-run cap. V5 rejected both, retained the first handle, accepted
   a terminal wait, and finished with zero active handles. Its result recorded
   `finish_blocked_active_runs=1`, `peak_active_runs=1`,
   `run_completions=1`, and one contradicted capped-start outcome.
3. `gpt-5.6-terra` selected `run_start`, copied the dynamically returned
   `run_id` into `run_wait`, observed `done=true`, read the exact output, and
   finished in four actions plus one completion-critic call.

The first real-model probe failed schema validation because the compact prompt
described actions as pseudo-calls rather than exact JSON. Making every
tool/input wrapper explicit fixed the next probe without weakening the strict
runtime schema. The passing probe then exposed two accounting issues: Stone
record arguments are value-like, so updated lifecycle state must be returned
to the loop explicitly; and a still-running background start is a successful
start even though the low-level initial probe has not observed process exit.
Both are corrected in V5.

## Artifacts

- `target/runs/stone-owned-run-lifecycle-v5/summary.json`
- `target/runs/stone-standard-task-management-v5/aggregate.json`
- `target/runs/stone-standard-owned-task-model-v5-terra-r4/summary.json`

The deterministic V5 source hash is recorded in the aggregate. All successful
attempts rolled back cleanly.

## Interpretation

The narrow hypothesis passes: an LLM can learn and use an explicit owned-task
lifecycle, while bounded same-key memory prevents the handle itself from
decaying out of short-term context.

This does not yet establish good policy for when to launch several tasks,
interleave useful work, interpret partial progress, or recover a controller
after restart. Those are later control-policy experiments; the lifecycle and
memory interface are now present.
