// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use crate::gateway_runtime::{config, linux_exec_record, with_client, GatewayRuntimeConfig};
use crate::json::json_to_nu_value;
use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};
use waymark_gateway_client::proto::{
    AttemptAcceptResponse, AttemptContextPromptView, AttemptFinishRequest, AttemptFinishResponse,
    AttemptForkRequest, AttemptInspectRequest, AttemptInspectResponse, AttemptProgram,
    AttemptPublishRequest, AttemptRecord, AttemptReportResultRequest, AttemptReportResultResponse,
    AttemptRunProcessRequest, AttemptRunProcessResponse, AttemptSpawnRequest, AttemptStateResponse,
    CapabilityRequest, ContextSource, EnvRunCheckpointRequest, TaskSpec, WorkspaceSource,
};

pub(crate) fn enabled() -> bool {
    config().is_some()
}

pub(crate) struct WorkflowStageCheckpoint {
    pub(crate) reference: String,
    pub(crate) workspace_revision: u64,
    pub(crate) memory_revision: Option<u64>,
    pub(crate) tool_environment_generation: Option<String>,
    pub(crate) tool_environment_disposition: Option<String>,
    pub(crate) storage_bytes: u64,
    pub(crate) create_duration_ms: u64,
    pub(crate) copy_files: u64,
    pub(crate) copy_bytes: u64,
    pub(crate) reflink_attempts: u64,
    pub(crate) reflink_successes: u64,
}

pub(crate) fn workflow_stage_checkpoint(
    workflow: &str,
    stage: &str,
    policy: &str,
) -> Result<WorkflowStageCheckpoint, ShellError> {
    let config = required_config()?;
    let reason = format!("stone.workflow-stage:{workflow}:{stage}");
    let checkpoint = with_client(&config, |client| {
        client.env_checkpoint_with_lifecycle_and_policy(&config.tx, reason, "attempt", policy)
    })?;
    Ok(WorkflowStageCheckpoint {
        reference: checkpoint.checkpoint,
        workspace_revision: checkpoint.source_tx_revision,
        memory_revision: checkpoint
            .has_memory_revision
            .then_some(checkpoint.memory_revision),
        tool_environment_generation: (!checkpoint.tool_environment_generation.is_empty())
            .then_some(checkpoint.tool_environment_generation),
        tool_environment_disposition: (!checkpoint.tool_environment_disposition.is_empty())
            .then_some(checkpoint.tool_environment_disposition),
        storage_bytes: checkpoint.storage_bytes,
        create_duration_ms: checkpoint.create_duration_ms,
        copy_files: checkpoint.copy_files,
        copy_bytes: checkpoint.copy_bytes,
        reflink_attempts: checkpoint.reflink_attempts,
        reflink_successes: checkpoint.reflink_successes,
    })
}

pub(crate) fn attempt_info(attempt: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let record = with_client(&config, |client| client.attempt_info(attempt))?;
    attempt_record_value(record, Span::unknown())
}

pub(crate) fn attempt_start(attempt: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let record = with_client(&config, |client| client.attempt_start(attempt))?;
    attempt_record_value(record, Span::unknown())
}

pub(crate) fn attempts(
    task: String,
    workspace: String,
    state: String,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let list = with_client(&config, |client| {
        client.attempt_list(task, workspace, state)
    })?;
    let span = Span::unknown();
    let attempts = list
        .attempts
        .into_iter()
        .map(|attempt| attempt_record_value(attempt, span))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::list(attempts, span))
}

pub(crate) fn attempt_state(attempt: String, sample_limit: u32) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let state = with_client(&config, |client| {
        client.attempt_state(attempt, sample_limit)
    })?;
    attempt_state_value(state, Span::unknown())
}

pub(crate) fn attempt_inspect(
    attempt: String,
    include_details: bool,
    trace_limit: u32,
    max_bytes: u32,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let inspection = with_client(&config, |client| {
        client.attempt_inspect(AttemptInspectRequest {
            attempt,
            include_details,
            trace_limit,
            max_bytes,
        })
    })?;
    attempt_inspect_value(inspection, Span::unknown())
}

pub(crate) fn attempt_wait(attempt: String, timeout_ms: Option<u32>) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let record = with_client(&config, |client| {
        client.attempt_wait(attempt.clone(), timeout_ms)
    })?;
    attempt_record_value(record, Span::unknown())
}

pub(crate) struct AttemptWaitSetValue {
    pub(crate) ready: Vec<Value>,
    pub(crate) completed: bool,
    pub(crate) timed_out: bool,
}

pub(crate) fn attempt_wait_set(
    attempts: Vec<String>,
    wait_all: bool,
    timeout_ms: Option<u32>,
) -> Result<AttemptWaitSetValue, ShellError> {
    let config = required_config()?;
    let response = with_client(&config, |client| {
        client.attempt_wait_set(attempts, wait_all, timeout_ms)
    })?;
    let span = Span::unknown();
    let ready = response
        .ready
        .into_iter()
        .map(|attempt| attempt_record_value(attempt, span))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AttemptWaitSetValue {
        ready,
        completed: response.completed,
        timed_out: response.timed_out,
    })
}

pub(crate) fn attempt_terminate(attempt: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let record = with_client(&config, |client| client.attempt_terminate(attempt))?;
    attempt_record_value(record, Span::unknown())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AttemptSpawnV1 {
    pub task_spec: Option<TaskSpec>,
    pub program: Option<AttemptProgram>,
    pub workspace_source: Option<WorkspaceSource>,
    pub context_source: Option<ContextSource>,
    pub capabilities: Option<CapabilityRequest>,
    pub task_input_json: String,
    pub start: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn attempt_spawn(
    task: String,
    workspace: String,
    controller: String,
    capability_profile: String,
    container: String,
    workspace_mount: String,
    parent_attempt: String,
    resource_limits: Vec<(String, String)>,
    metadata: Vec<(String, String)>,
    spawn_v1: AttemptSpawnV1,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let resumes_checkpoint = spawn_v1
        .workspace_source
        .as_ref()
        .is_some_and(|source| !source.checkpoint.is_empty());
    let container = if container.is_empty() && !resumes_checkpoint {
        config.container.clone().unwrap_or_default()
    } else {
        container
    };
    let workspace_mount = if workspace_mount.is_empty() {
        config.workspace_mount.clone()
    } else {
        workspace_mount
    };
    let parent_attempt = if parent_attempt.is_empty() {
        config.attempt.clone()
    } else {
        parent_attempt
    };
    let attempt = with_client(&config, |client| {
        client.attempt_spawn(AttemptSpawnRequest {
            task: task.clone(),
            workspace: workspace.clone(),
            controller: controller.clone(),
            capability_profile: capability_profile.clone(),
            container: container.clone(),
            workspace_mount: workspace_mount.clone(),
            parent_attempt: parent_attempt.clone(),
            resource_limits: resource_limits.clone().into_iter().collect(),
            metadata: metadata.clone().into_iter().collect(),
            task_spec: spawn_v1.task_spec.clone(),
            program: spawn_v1.program.clone(),
            workspace_source: spawn_v1.workspace_source.clone(),
            context_source: spawn_v1.context_source.clone(),
            capabilities: spawn_v1.capabilities.clone(),
            start: spawn_v1.start,
            task_input_json: spawn_v1.task_input_json.clone(),
        })
    })?;
    attempt_record_value(attempt, Span::unknown())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn attempt_fork(
    parent_attempt: String,
    checkpoint: String,
    task: String,
    controller: String,
    capability_profile: String,
    container: String,
    workspace_mount: String,
    resource_limits: Vec<(String, String)>,
    metadata: Vec<(String, String)>,
    context_prompt_required_keys: Option<Vec<String>>,
    task_input_json: String,
    program: Option<AttemptProgram>,
    start: bool,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let parent_attempt = effective_attempt(&config, parent_attempt)?;
    let attempt = with_client(&config, |client| {
        client.attempt_fork(AttemptForkRequest {
            parent_attempt: parent_attempt.clone(),
            checkpoint: checkpoint.clone(),
            task: task.clone(),
            controller: controller.clone(),
            capability_profile: capability_profile.clone(),
            container: container.clone(),
            workspace_mount: workspace_mount.clone(),
            resource_limits: resource_limits.clone().into_iter().collect(),
            metadata: metadata.clone().into_iter().collect(),
            program: program.clone(),
            start,
            task_input_json: task_input_json.clone(),
            context_prompt_view: context_prompt_required_keys
                .clone()
                .map(|required_keys| AttemptContextPromptView { required_keys }),
        })
    })?;
    attempt_record_value(attempt, Span::unknown())
}

pub(crate) fn attempt_finish(
    attempt: String,
    action: String,
    message: String,
    reason: String,
    allow_risky: bool,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let finish = with_client(&config, |client| {
        client.attempt_finish(AttemptFinishRequest {
            attempt: attempt.clone(),
            action: action.clone(),
            message: message.clone(),
            reason: reason.clone(),
            allow_risky,
        })
    })?;
    attempt_finish_value(finish, Span::unknown())
}

pub(crate) fn attempt_report(
    attempt: String,
    status: String,
    result_json: String,
    error_json: String,
    reason: String,
    metadata: Vec<(String, String)>,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let report = with_client(&config, |client| {
        client.attempt_report_result(AttemptReportResultRequest {
            attempt: attempt.clone(),
            status: status.clone(),
            result_json: result_json.clone(),
            error_json: error_json.clone(),
            details_json: String::new(),
            reason: reason.clone(),
            metadata: metadata.clone().into_iter().collect(),
        })
    })?;
    attempt_report_value(report, Span::unknown())
}

pub(crate) fn attempt_accept(parent: String, child: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let parent = effective_attempt(&config, parent)?;
    if child.is_empty() {
        return Err(stone_error(
            "attempt_accept",
            "attempt_accept requires a child attempt",
        ));
    }
    let accepted = with_client(&config, |client| {
        client.attempt_accept(parent.clone(), child.clone())
    })?;
    attempt_accept_value(accepted, Span::unknown())
}

pub(crate) fn attempt_discard(attempt: String, reason: String) -> Result<Value, ShellError> {
    attempt_finish(
        attempt,
        "rollback".to_string(),
        String::new(),
        reason,
        false,
    )
}

pub(crate) fn attempt_publish(
    attempt: String,
    expected_generation: String,
    message: String,
    allow_risky: bool,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    if expected_generation.is_empty() {
        return Err(stone_error(
            "attempt_publish",
            "attempt_publish requires expected_generation",
        ));
    }
    let published = with_client(&config, |client| {
        client.attempt_publish(AttemptPublishRequest {
            attempt: attempt.clone(),
            expected_generation: expected_generation.clone(),
            message: message.clone(),
            allow_risky,
        })
    })?;
    attempt_finish_value(published, Span::unknown())
}

pub(crate) fn attempt_run_process(
    attempt: String,
    argv: Vec<String>,
    env: Vec<(String, String)>,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let run = with_client(&config, |client| {
        client.attempt_run_process(AttemptRunProcessRequest {
            attempt: attempt.clone(),
            argv: argv.clone(),
            env: env.clone().into_iter().collect(),
            image: config.image.clone(),
            container: config.container.clone().unwrap_or_default(),
            workspace_mount: config.workspace_mount.clone(),
            wait_timeout_ms: 1_000,
        })
    })?;
    attempt_process_value(run, Span::unknown())
}

pub(crate) fn env_state(sample_limit: u32) -> Result<Value, ShellError> {
    let config = required_config()?;
    let diff = with_client(&config, |client| client.env_diff(&config.tx, sample_limit))?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(config.tx, span));
    record.push("clean", Value::bool(diff.clean, span));
    record.push("text", Value::string(diff.text, span));
    record.push("json", json_text_value(&diff.json, span)?);
    Ok(Value::record(record, span))
}

pub(crate) fn env_tx_info(tx: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let tx = if tx.is_empty() { config.tx.clone() } else { tx };
    let info = with_client(&config, |client| client.env_tx_info(tx.clone()))?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(info.tx, span));
    record.push("workspace", Value::string(info.workspace, span));
    record.push("base_generation", Value::string(info.base_generation, span));
    record.push("provider", Value::string(info.provider, span));
    record.push(
        "parent_checkpoint",
        Value::string(info.parent_checkpoint, span),
    );
    record.push("purpose", Value::string(info.purpose, span));
    record.push("merged_path", Value::string(info.merged_path, span));
    Ok(Value::record(record, span))
}

pub(crate) fn env_txs(workspace: String, purpose: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let list = with_client(&config, |client| {
        client.env_tx_list(workspace.clone(), purpose.clone())
    })?;
    let span = Span::unknown();
    let transactions = list
        .transactions
        .into_iter()
        .map(|info| {
            let mut record = Record::new();
            record.push("tx", Value::string(info.tx, span));
            record.push("workspace", Value::string(info.workspace, span));
            record.push("base_generation", Value::string(info.base_generation, span));
            record.push("provider", Value::string(info.provider, span));
            record.push(
                "parent_checkpoint",
                Value::string(info.parent_checkpoint, span),
            );
            record.push("purpose", Value::string(info.purpose, span));
            record.push("merged_path", Value::string(info.merged_path, span));
            Value::record(record, span)
        })
        .collect();
    Ok(Value::list(transactions, span))
}

pub(crate) fn env_finish() -> Result<Value, ShellError> {
    let config = required_config()?;
    let finish = with_client(&config, |client| client.env_finish(&config.tx))?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(config.tx, span));
    record.push("ok", Value::bool(finish.clean || finish.tx_closed, span));
    record.push("clean", Value::bool(finish.clean, span));
    record.push("tx_closed", Value::bool(finish.tx_closed, span));
    record.push("stdout", Value::string(finish.stdout, span));
    record.push("stderr", Value::string(finish.stderr, span));
    record.push("env_state", Value::string(finish.env_state, span));
    record.push("env_diff", Value::string(finish.env_diff, span));
    record.push(
        "next_actions",
        Value::list(
            finish
                .next_actions
                .into_iter()
                .map(|action| Value::string(action, span))
                .collect(),
            span,
        ),
    );
    Ok(Value::record(record, span))
}

pub(crate) fn env_restore(paths: Vec<String>) -> Result<Value, ShellError> {
    let config = required_config()?;
    let restore = with_client(&config, |client| {
        client.env_restore(&config.tx, paths.clone())
    })?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(config.tx, span));
    record.push(
        "paths",
        Value::list(
            paths
                .into_iter()
                .map(|path| Value::string(path, span))
                .collect(),
            span,
        ),
    );
    if let Some(diff) = restore.diff {
        record.push("clean", Value::bool(diff.clean, span));
        record.push("env_diff", Value::string(diff.text, span));
        record.push("json", json_text_value(&diff.json, span)?);
    }
    Ok(Value::record(record, span))
}

pub(crate) fn env_checkpoint(reason: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let checkpoint = with_client(&config, |client| {
        client.env_checkpoint(&config.tx, reason.clone())
    })?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(config.tx, span));
    record.push("checkpoint", Value::string(checkpoint.checkpoint, span));
    record.push("workspace", Value::string(checkpoint.workspace, span));
    record.push(
        "base_generation",
        Value::string(checkpoint.base_generation, span),
    );
    record.push("source_tx", Value::string(checkpoint.source_tx, span));
    record.push(
        "parent_checkpoint",
        Value::string(checkpoint.parent_checkpoint, span),
    );
    record.push(
        "child_count",
        Value::int(checkpoint.child_count as i64, span),
    );
    record.push("reason", Value::string(checkpoint.reason, span));
    record.push(
        "created_at_ms",
        Value::int(checkpoint.created_at_ms as i64, span),
    );
    record.push(
        "discarded_at_ms",
        if checkpoint.has_discarded_at_ms {
            Value::int(checkpoint.discarded_at_ms as i64, span)
        } else {
            Value::nothing(span)
        },
    );
    record.push(
        "storage_bytes",
        Value::int(checkpoint.storage_bytes as i64, span),
    );
    record.push(
        "create_duration_ms",
        Value::int(checkpoint.create_duration_ms as i64, span),
    );
    record.push("copy_files", Value::int(checkpoint.copy_files as i64, span));
    record.push("copy_bytes", Value::int(checkpoint.copy_bytes as i64, span));
    record.push(
        "reflink_attempts",
        Value::int(checkpoint.reflink_attempts as i64, span),
    );
    record.push(
        "reflink_successes",
        Value::int(checkpoint.reflink_successes as i64, span),
    );
    record.push("root_path", Value::string(checkpoint.root_path, span));
    Ok(Value::record(record, span))
}

pub(crate) fn env_fork(checkpoint: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let fork = with_client(&config, |client| {
        client.env_fork(checkpoint.clone(), "", "")
    })?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("checkpoint", Value::string(checkpoint, span));
    record.push("tx", Value::string(fork.tx, span));
    record.push("workspace", Value::string(fork.workspace, span));
    record.push("base_generation", Value::string(fork.base_generation, span));
    record.push("provider", Value::string(fork.provider, span));
    record.push("merged_path", Value::string(fork.merged_path, span));
    Ok(Value::record(record, span))
}

pub(crate) fn env_restore_checkpoint(checkpoint: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let restore = with_client(&config, |client| {
        client.env_restore_checkpoint(&config.tx, checkpoint.clone())
    })?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(restore.tx, span));
    record.push("checkpoint", Value::string(restore.checkpoint, span));
    record.push(
        "terminated_runs",
        Value::int(restore.terminated_runs as i64, span),
    );
    record.push(
        "terminated_run_ids",
        Value::list(
            restore
                .terminated_run_ids
                .into_iter()
                .map(|run_id| Value::string(run_id, span))
                .collect(),
            span,
        ),
    );
    record.push("duration_ms", Value::int(restore.duration_ms as i64, span));
    if let Some(diff) = restore.diff {
        record.push("clean", Value::bool(diff.clean, span));
        record.push("env_diff", Value::string(diff.text, span));
        record.push("json", json_text_value(&diff.json, span)?);
    }
    Ok(Value::record(record, span))
}

pub(crate) fn env_checkpoints(
    workspace: String,
    include_discarded: bool,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let list = with_client(&config, |client| {
        client.env_checkpoint_list(workspace.clone(), include_discarded)
    })?;
    let span = Span::unknown();
    let checkpoints = list
        .checkpoints
        .into_iter()
        .map(|checkpoint| {
            let mut record = Record::new();
            record.push("checkpoint", Value::string(checkpoint.checkpoint, span));
            record.push("workspace", Value::string(checkpoint.workspace, span));
            record.push(
                "base_generation",
                Value::string(checkpoint.base_generation, span),
            );
            record.push("source_tx", Value::string(checkpoint.source_tx, span));
            record.push(
                "parent_checkpoint",
                Value::string(checkpoint.parent_checkpoint, span),
            );
            record.push(
                "child_count",
                Value::int(checkpoint.child_count as i64, span),
            );
            record.push("status", Value::string(checkpoint.status, span));
            record.push("retention", Value::string(checkpoint.retention, span));
            record.push("reason", Value::string(checkpoint.reason, span));
            record.push(
                "discarded_at_ms",
                if checkpoint.has_discarded_at_ms {
                    Value::int(checkpoint.discarded_at_ms as i64, span)
                } else {
                    Value::nothing(span)
                },
            );
            record.push(
                "storage_bytes",
                Value::int(checkpoint.storage_bytes as i64, span),
            );
            record.push(
                "create_duration_ms",
                Value::int(checkpoint.create_duration_ms as i64, span),
            );
            record.push("copy_files", Value::int(checkpoint.copy_files as i64, span));
            record.push("copy_bytes", Value::int(checkpoint.copy_bytes as i64, span));
            record.push(
                "reflink_attempts",
                Value::int(checkpoint.reflink_attempts as i64, span),
            );
            record.push(
                "reflink_successes",
                Value::int(checkpoint.reflink_successes as i64, span),
            );
            Value::record(record, span)
        })
        .collect();
    Ok(Value::list(checkpoints, span))
}

pub(crate) fn env_checkpoint_gc(apply: bool) -> Result<Value, ShellError> {
    let config = required_config()?;
    let gc = with_client(&config, |client| client.env_checkpoint_gc(apply))?;
    let span = Span::unknown();
    let entries = gc
        .entries
        .into_iter()
        .map(|entry| {
            let mut record = Record::new();
            record.push("checkpoint", Value::string(entry.checkpoint, span));
            record.push("kind", Value::string(entry.kind, span));
            record.push("path", Value::string(entry.path, span));
            record.push(
                "storage_bytes",
                Value::int(entry.storage_bytes as i64, span),
            );
            record.push("reason", Value::string(entry.reason, span));
            Value::record(record, span)
        })
        .collect();
    let mut record = Record::new();
    record.push("applied", Value::bool(gc.applied, span));
    record.push(
        "checkpoint_count",
        Value::int(gc.checkpoint_count as i64, span),
    );
    record.push("active_count", Value::int(gc.active_count as i64, span));
    record.push(
        "discarded_count",
        Value::int(gc.discarded_count as i64, span),
    );
    record.push("retained_count", Value::int(gc.retained_count as i64, span));
    record.push(
        "active_payload_bytes",
        Value::int(gc.active_payload_bytes as i64, span),
    );
    record.push(
        "reclaimable_bytes",
        Value::int(gc.reclaimable_bytes as i64, span),
    );
    record.push(
        "deleted_entries",
        Value::int(gc.deleted_entries as i64, span),
    );
    record.push("deleted_bytes", Value::int(gc.deleted_bytes as i64, span));
    record.push("entries", Value::list(entries, span));
    Ok(Value::record(record, span))
}

pub(crate) fn env_discard_checkpoint(checkpoint: String, force: bool) -> Result<Value, ShellError> {
    let config = required_config()?;
    let discard = with_client(&config, |client| {
        client.env_checkpoint_discard(checkpoint.clone(), force)
    })?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("checkpoint", Value::string(discard.checkpoint, span));
    record.push("discarded", Value::bool(true, span));
    record.push("force", Value::bool(force, span));
    Ok(Value::record(record, span))
}

pub(crate) fn env_commit(message: String, allow_risky: bool) -> Result<Value, ShellError> {
    let config = required_config()?;
    let effective_allow_risky = allow_risky || blind_agent_surface_enabled();
    let commit = with_client(&config, |client| {
        client.env_commit(&config.tx, message.clone(), effective_allow_risky)
    })?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(config.tx, span));
    record.push("generation", Value::string(commit.generation, span));
    record.push("message", Value::string(message, span));
    record.push("allow_risky", Value::bool(effective_allow_risky, span));
    Ok(Value::record(record, span))
}

fn blind_agent_surface_enabled() -> bool {
    std::env::var("WAYMARK_GATEWAY_AGENT_SURFACE")
        .is_ok_and(|value| value.trim().eq_ignore_ascii_case("blind"))
}

pub(crate) fn env_rollback() -> Result<Value, ShellError> {
    let config = required_config()?;
    with_client(&config, |client| client.env_rollback(&config.tx))?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(config.tx, span));
    record.push("rolled_back", Value::bool(true, span));
    Ok(Value::record(record, span))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn env_run_checkpoint(
    checkpoint: String,
    image: String,
    argv: Vec<String>,
    workspace_mount: String,
    workdir: String,
    env: Vec<(String, String)>,
    user: String,
    stdin: String,
    timeout_ms: u64,
    keep_tx: bool,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let run = with_client(&config, |client| {
        client.env_run_checkpoint(EnvRunCheckpointRequest {
            checkpoint: checkpoint.clone(),
            image: image.clone(),
            argv: argv.clone(),
            workspace_mount: workspace_mount.clone(),
            workdir: workdir.clone(),
            env: env.clone().into_iter().collect::<HashMap<_, _>>(),
            user: user.clone(),
            stdin: stdin.clone(),
            timeout_ms,
            read_only_mounts: Vec::new(),
            keep_tx,
        })
    })?;
    let span = Span::unknown();
    let mut record = if let Some(output) = run.output {
        linux_exec_record(output, span, 1_048_576, 1_048_576)?
    } else {
        Record::new()
    };
    record.push("checkpoint", Value::string(run.checkpoint, span));
    record.push("branch_tx", Value::string(run.tx, span));
    record.push("rolled_back", Value::bool(run.rolled_back, span));
    record.push(
        "fork_duration_ms",
        Value::int(run.fork_duration_ms as i64, span),
    );
    record.push(
        "diff_duration_ms",
        Value::int(run.diff_duration_ms as i64, span),
    );
    record.push(
        "rollback_duration_ms",
        Value::int(run.rollback_duration_ms as i64, span),
    );
    record.push(
        "total_duration_ms",
        Value::int(run.total_duration_ms as i64, span),
    );
    Ok(Value::record(record, span))
}

fn attempt_record_value(attempt: AttemptRecord, span: Span) -> Result<Value, ShellError> {
    let reported_result = optional_json_text_value(
        &attempt.reported_result_json,
        span,
        "attempt reported result",
    )?;
    let reported_error =
        optional_json_text_value(&attempt.reported_error_json, span, "attempt reported error")?;
    let controller_run_count = attempt
        .metadata
        .get("controller_run_count")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    let controller_phase = match controller_run_count {
        0 => "pending",
        1 => "initial",
        _ => "restart",
    };
    let mut record = Record::new();
    record.push("attempt", Value::string(attempt.attempt, span));
    record.push("task", Value::string(attempt.task, span));
    record.push(
        "parent_attempt",
        Value::string(attempt.parent_attempt, span),
    );
    record.push("controller", Value::string(attempt.controller, span));
    record.push("state", Value::string(attempt.state, span));
    record.push("workspace", Value::string(attempt.workspace, span));
    record.push(
        "base_generation",
        Value::string(attempt.base_generation, span),
    );
    record.push("tx", Value::string(attempt.tx, span));
    record.push(
        "source_checkpoint",
        Value::string(attempt.source_checkpoint, span),
    );
    record.push(
        "capability_profile",
        Value::string(attempt.capability_profile, span),
    );
    record.push(
        "controller_run_count",
        Value::int(u64_to_i64(controller_run_count), span),
    );
    record.push(
        "controller_restarted",
        Value::bool(controller_run_count > 1, span),
    );
    record.push("controller_phase", Value::string(controller_phase, span));
    record.push("memory_ref", Value::string(attempt.memory_ref, span));
    record.push(
        "memory_revision",
        Value::int(u64_to_i64(attempt.memory_revision), span),
    );
    record.push("reported_result", reported_result);
    record.push("reported_error", reported_error);
    record.push(
        "fork_origin",
        attempt
            .fork_origin
            .map(|origin| {
                let mut fork = Record::new();
                fork.push("parent_attempt", Value::string(origin.parent_attempt, span));
                fork.push(
                    "workspace_checkpoint",
                    Value::string(origin.workspace_checkpoint, span),
                );
                fork.push(
                    "workspace_revision",
                    Value::int(u64_to_i64(origin.workspace_revision), span),
                );
                fork.push("memory_ref", Value::string(origin.memory_ref, span));
                fork.push(
                    "memory_revision",
                    Value::int(u64_to_i64(origin.memory_revision), span),
                );
                fork.push(
                    "operation_policy",
                    Value::string(origin.operation_policy, span),
                );
                fork.push(
                    "created_at_ms",
                    Value::int(u64_to_i64(origin.created_at_ms), span),
                );
                Value::record(fork, span)
            })
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "context_prompt_view",
        attempt
            .context_prompt_view
            .map(|view| {
                let mut prompt = Record::new();
                prompt.push(
                    "required_keys",
                    Value::list(
                        view.required_keys
                            .into_iter()
                            .map(|key| Value::string(key, span))
                            .collect(),
                        span,
                    ),
                );
                Value::record(prompt, span)
            })
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "resource_limits",
        string_map_value(attempt.resource_limits.into_iter().collect(), span),
    );
    record.push(
        "metadata",
        string_map_value(attempt.metadata.into_iter().collect(), span),
    );
    record.push(
        "created_at_ms",
        Value::int(u64_to_i64(attempt.created_at_ms), span),
    );
    record.push(
        "updated_at_ms",
        Value::int(u64_to_i64(attempt.updated_at_ms), span),
    );
    record.push(
        "completed_at_ms",
        if attempt.has_completed_at_ms {
            Value::int(u64_to_i64(attempt.completed_at_ms), span)
        } else {
            Value::nothing(span)
        },
    );
    record.push("generation", Value::string(attempt.generation, span));
    record.push("close_reason", Value::string(attempt.close_reason, span));
    Ok(Value::record(record, span))
}

fn attempt_state_value(state: AttemptStateResponse, span: Span) -> Result<Value, ShellError> {
    let mut record = Record::new();
    let attempt = match state.attempt {
        Some(attempt) => attempt_record_value(attempt, span)?,
        None => Value::nothing(span),
    };
    record.push("attempt", attempt);
    record.push("tx_closed", Value::bool(state.tx_closed, span));
    record.push("env_state", Value::string(state.env_state, span));
    if let Some(diff) = state.diff {
        record.push("clean", Value::bool(diff.clean, span));
        record.push("env_diff", Value::string(diff.text, span));
        record.push("json", json_text_value(&diff.json, span)?);
    }
    Ok(Value::record(record, span))
}

fn attempt_inspect_value(
    inspection: AttemptInspectResponse,
    span: Span,
) -> Result<Value, ShellError> {
    let mut record = Record::new();
    let attempt = match inspection.attempt {
        Some(attempt) => attempt_record_value(attempt, span)?,
        None => Value::nothing(span),
    };
    record.push("attempt", attempt);
    record.push(
        "summary",
        optional_json_text_value(&inspection.summary_json, span, "attempt summary")?,
    );
    record.push(
        "error",
        optional_json_text_value(&inspection.error_json, span, "attempt error")?,
    );
    record.push(
        "details",
        optional_json_text_value(&inspection.details_json, span, "attempt details")?,
    );
    record.push(
        "details_truncated",
        Value::bool(inspection.details_truncated, span),
    );
    record.push(
        "details_bytes",
        Value::int(u64_to_i64(inspection.details_bytes), span),
    );
    let trace = inspection
        .trace_json
        .iter()
        .map(|event| optional_json_text_value(event, span, "attempt trace event"))
        .collect::<Result<Vec<_>, _>>()?;
    record.push("trace", Value::list(trace, span));
    record.push(
        "trace_truncated",
        Value::bool(inspection.trace_truncated, span),
    );
    record.push(
        "active_operations",
        Value::list(
            inspection
                .active_operations
                .into_iter()
                .map(|operation| Value::string(operation, span))
                .collect(),
            span,
        ),
    );
    record.push(
        "active_runs",
        Value::list(
            inspection
                .active_runs
                .into_iter()
                .map(|run| Value::string(run, span))
                .collect(),
            span,
        ),
    );
    record.push(
        "active_descendants",
        Value::list(
            inspection
                .active_descendants
                .into_iter()
                .map(|attempt| Value::string(attempt, span))
                .collect(),
            span,
        ),
    );
    record.push(
        "resource_state",
        Value::string(inspection.resource_state, span),
    );
    record.push(
        "resources_reclaimed",
        Value::bool(inspection.resources_reclaimed, span),
    );
    record.push(
        "controller_run",
        Value::string(inspection.controller_run, span),
    );
    record.push(
        "controller_active",
        Value::bool(inspection.controller_active, span),
    );
    Ok(Value::record(record, span))
}

fn attempt_finish_value(finish: AttemptFinishResponse, span: Span) -> Result<Value, ShellError> {
    let mut record = Record::new();
    let attempt = match finish.attempt {
        Some(attempt) => attempt_record_value(attempt, span)?,
        None => Value::nothing(span),
    };
    record.push("attempt", attempt);
    record.push("generation", Value::string(finish.generation, span));
    record.push(
        "file_changes",
        Value::int(u64_to_i64(finish.file_changes), span),
    );
    record.push(
        "env_changes",
        Value::int(u64_to_i64(finish.env_changes), span),
    );
    if let Some(diff) = finish.diff {
        record.push("clean", Value::bool(diff.clean, span));
        record.push("env_diff", Value::string(diff.text, span));
        record.push("json", json_text_value(&diff.json, span)?);
    }
    Ok(Value::record(record, span))
}

fn attempt_report_value(
    report: AttemptReportResultResponse,
    span: Span,
) -> Result<Value, ShellError> {
    let mut record = Record::new();
    let attempt = match report.attempt {
        Some(attempt) => attempt_record_value(attempt, span)?,
        None => Value::nothing(span),
    };
    record.push("attempt", attempt);
    record.push("tx_closed", Value::bool(report.tx_closed, span));
    record.push(
        "file_changes",
        Value::int(u64_to_i64(report.file_changes), span),
    );
    record.push(
        "env_changes",
        Value::int(u64_to_i64(report.env_changes), span),
    );
    if let Some(diff) = report.diff {
        record.push("clean", Value::bool(diff.clean, span));
        record.push("env_diff", Value::string(diff.text, span));
        record.push("json", json_text_value(&diff.json, span)?);
    }
    Ok(Value::record(record, span))
}

fn attempt_accept_value(accepted: AttemptAcceptResponse, span: Span) -> Result<Value, ShellError> {
    let mut record = Record::new();
    let parent = match accepted.parent {
        Some(attempt) => attempt_record_value(attempt, span)?,
        None => Value::nothing(span),
    };
    let child = match accepted.child {
        Some(attempt) => attempt_record_value(attempt, span)?,
        None => Value::nothing(span),
    };
    record.push("parent", parent);
    record.push("child", child);
    record.push(
        "file_changes",
        Value::int(u64_to_i64(accepted.file_changes), span),
    );
    record.push(
        "env_changes",
        Value::int(u64_to_i64(accepted.env_changes), span),
    );
    if let Some(diff) = accepted.diff {
        record.push("clean", Value::bool(diff.clean, span));
        record.push("env_diff", Value::string(diff.text, span));
        record.push("json", json_text_value(&diff.json, span)?);
    }
    Ok(Value::record(record, span))
}

fn attempt_process_value(run: AttemptRunProcessResponse, span: Span) -> Result<Value, ShellError> {
    let mut record = Record::new();
    record.push("run", Value::string(run.run, span));
    let attempt = match run.attempt {
        Some(attempt) => attempt_record_value(attempt, span)?,
        None => Value::nothing(span),
    };
    record.push("attempt", attempt);
    record.push("task", Value::string(run.task, span));
    record.push("workspace", Value::string(run.workspace, span));
    record.push("tx", Value::string(run.tx, span));
    record.push("status", Value::int(i64::from(run.status), span));
    record.push("ok", Value::bool(run.status == 0, span));
    record.push("stdout", Value::string(run.stdout, span));
    record.push("stderr", Value::string(run.stderr, span));
    record.push("stdout_path", Value::string(run.stdout_path, span));
    record.push("stderr_path", Value::string(run.stderr_path, span));
    record.push(
        "started_at_ms",
        Value::int(u64_to_i64(run.started_at_ms), span),
    );
    record.push(
        "completed_at_ms",
        Value::int(u64_to_i64(run.completed_at_ms), span),
    );
    record.push("duration_ms", Value::int(u64_to_i64(run.duration_ms), span));
    record.push(
        "argv",
        Value::list(
            run.argv
                .into_iter()
                .map(|arg| Value::string(arg, span))
                .collect(),
            span,
        ),
    );
    Ok(Value::record(record, span))
}

fn string_map_value(map: HashMap<String, String>, span: Span) -> Value {
    let mut record = Record::new();
    let mut entries = map.into_iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (key, value) in entries {
        record.push(key, Value::string(value, span));
    }
    Value::record(record, span)
}

fn effective_attempt(config: &GatewayRuntimeConfig, attempt: String) -> Result<String, ShellError> {
    if !attempt.is_empty() {
        return Ok(attempt);
    }
    if !config.attempt.is_empty() {
        return Ok(config.attempt.clone());
    }
    Err(stone_error(
        "attempt",
        "missing attempt argument and WAYMARK_GATEWAY_ATTEMPT_ID is not set",
    ))
}

fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn json_text_value(text: &str, span: Span) -> Result<Value, ShellError> {
    let json = serde_json::from_str(text)
        .map_err(|err| stone_error("env", format!("Gateway returned invalid diff JSON: {err}")))?;
    Ok(json_to_nu_value(json, span))
}

fn optional_json_text_value(text: &str, span: Span, label: &str) -> Result<Value, ShellError> {
    if text.is_empty() {
        return Ok(Value::nothing(span));
    }
    let json = serde_json::from_str(text).map_err(|err| {
        stone_error(
            "attempt",
            format!("Gateway returned invalid {label} JSON: {err}"),
        )
    })?;
    Ok(json_to_nu_value(json, span))
}

fn required_config() -> Result<GatewayRuntimeConfig, ShellError> {
    config().ok_or_else(|| stone_error("env", "Gateway runtime config is not active"))
}

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_inspection_exposes_typed_archive_and_resource_state() {
        let inspection = AttemptInspectResponse {
            attempt: Some(AttemptRecord {
                attempt: "attempt-child".to_string(),
                ..Default::default()
            }),
            summary_json: r#"{"answer":"hello"}"#.to_string(),
            details_json: r#"{"ok":true,"kind":"task_result"}"#.to_string(),
            trace_json: vec![r#"{"op":"attempt.report_result"}"#.to_string()],
            details_bytes: 32,
            resource_state: "reclaimed".to_string(),
            resources_reclaimed: true,
            controller_run: "process-1".to_string(),
            ..Default::default()
        };

        let value = attempt_inspect_value(inspection, Span::unknown()).unwrap();
        let record = value.as_record().expect("inspection record");
        assert_eq!(
            record
                .get("summary")
                .unwrap()
                .as_record()
                .unwrap()
                .get("answer")
                .unwrap()
                .as_str()
                .unwrap(),
            "hello"
        );
        assert_eq!(
            record.get("resource_state").unwrap().as_str().unwrap(),
            "reclaimed"
        );
        assert!(record
            .get("resources_reclaimed")
            .unwrap()
            .as_bool()
            .unwrap());
        assert_eq!(record.get("trace").unwrap().as_list().unwrap().len(), 1);
    }

    #[test]
    fn attempt_record_exposes_typed_controller_lifecycle_and_memory() {
        let mut attempt = AttemptRecord {
            attempt: "attempt-1".to_string(),
            memory_ref: "attempt-memory:attempt-1".to_string(),
            memory_revision: 5,
            reported_result_json: r#"{"candidate":"cobalt","decision":"select"}"#.to_string(),
            fork_origin: Some(waymark_gateway_client::proto::AttemptForkOrigin {
                parent_attempt: "attempt-parent".to_string(),
                workspace_checkpoint: "cp-1".to_string(),
                workspace_revision: 3,
                operation_policy: "require_idle".to_string(),
                created_at_ms: 10,
                memory_ref: "attempt-memory:attempt-parent".to_string(),
                memory_revision: 4,
            }),
            context_prompt_view: Some(AttemptContextPromptView {
                required_keys: vec!["requirement.target".to_string()],
            }),
            ..Default::default()
        };
        attempt
            .metadata
            .insert("controller_run_count".to_string(), "2".to_string());

        let value = attempt_record_value(attempt, Span::unknown()).unwrap();
        let record = value.as_record().expect("attempt record");
        assert_eq!(
            record
                .get("controller_run_count")
                .expect("run count")
                .as_int()
                .unwrap(),
            2
        );
        assert!(record
            .get("controller_restarted")
            .expect("restart flag")
            .as_bool()
            .unwrap());
        assert_eq!(
            record
                .get("controller_phase")
                .expect("controller phase")
                .as_str()
                .unwrap(),
            "restart"
        );
        assert_eq!(
            record
                .get("memory_revision")
                .expect("memory revision")
                .as_int()
                .unwrap(),
            5
        );
        assert_eq!(
            record
                .get("reported_result")
                .expect("reported result")
                .as_record()
                .unwrap()
                .get("decision")
                .expect("decision")
                .as_str()
                .unwrap(),
            "select"
        );
        assert!(record
            .get("reported_error")
            .expect("reported error")
            .is_nothing());
        let fork = record
            .get("fork_origin")
            .expect("fork origin")
            .as_record()
            .unwrap();
        assert_eq!(
            fork.get("memory_ref")
                .expect("fork memory ref")
                .as_str()
                .unwrap(),
            "attempt-memory:attempt-parent"
        );
        assert_eq!(
            fork.get("memory_revision")
                .expect("fork memory revision")
                .as_int()
                .unwrap(),
            4
        );
        assert_eq!(
            record
                .get("context_prompt_view")
                .expect("context prompt view")
                .as_record()
                .unwrap()
                .get("required_keys")
                .expect("required keys")
                .as_list()
                .unwrap()[0]
                .as_str()
                .unwrap(),
            "requirement.target"
        );
    }
}
