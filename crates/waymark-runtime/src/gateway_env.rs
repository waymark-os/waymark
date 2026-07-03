// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};
use waymark_gateway_client::proto::{
    AttemptFinishRequest, AttemptFinishResponse, AttemptForkRequest, AttemptRecord,
    AttemptSpawnRequest, AttemptStateResponse, EnvRunCheckpointRequest,
};
use waymark_gateway_client::GatewayRpcClient;

use crate::gateway_runtime::{config, linux_exec_record, GatewayEndpoint, GatewayRuntimeConfig};
use crate::json::json_to_nu_value;

pub(crate) fn enabled() -> bool {
    config().is_some()
}

pub(crate) fn attempt_info(attempt: String) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let record = with_client(&config, |client| client.attempt_info(attempt))?;
    Ok(attempt_record_value(record, Span::unknown()))
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
    Ok(Value::list(
        list.attempts
            .into_iter()
            .map(|attempt| attempt_record_value(attempt, span))
            .collect(),
        span,
    ))
}

pub(crate) fn attempt_state(attempt: String, sample_limit: u32) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = effective_attempt(&config, attempt)?;
    let state = with_client(&config, |client| {
        client.attempt_state(attempt, sample_limit)
    })?;
    attempt_state_value(state, Span::unknown())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn attempt_spawn(
    task: String,
    workspace: String,
    controller: String,
    capability_profile: String,
    container: String,
    workspace_mount: String,
    resource_limits: Vec<(String, String)>,
    metadata: Vec<(String, String)>,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let attempt = with_client(&config, |client| {
        client.attempt_spawn(AttemptSpawnRequest {
            task: task.clone(),
            workspace: workspace.clone(),
            controller: controller.clone(),
            capability_profile: capability_profile.clone(),
            container: container.clone(),
            workspace_mount: workspace_mount.clone(),
            resource_limits: resource_limits.clone().into_iter().collect(),
            metadata: metadata.clone().into_iter().collect(),
        })
    })?;
    Ok(attempt_record_value(attempt, Span::unknown()))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn attempt_fork(
    parent_attempt: String,
    task: String,
    controller: String,
    capability_profile: String,
    container: String,
    workspace_mount: String,
    resource_limits: Vec<(String, String)>,
    metadata: Vec<(String, String)>,
) -> Result<Value, ShellError> {
    let config = required_config()?;
    let parent_attempt = effective_attempt(&config, parent_attempt)?;
    let attempt = with_client(&config, |client| {
        client.attempt_fork(AttemptForkRequest {
            parent_attempt: parent_attempt.clone(),
            task: task.clone(),
            controller: controller.clone(),
            capability_profile: capability_profile.clone(),
            container: container.clone(),
            workspace_mount: workspace_mount.clone(),
            resource_limits: resource_limits.clone().into_iter().collect(),
            metadata: metadata.clone().into_iter().collect(),
        })
    })?;
    Ok(attempt_record_value(attempt, Span::unknown()))
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

fn attempt_record_value(attempt: AttemptRecord, span: Span) -> Value {
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
    Value::record(record, span)
}

fn attempt_state_value(state: AttemptStateResponse, span: Span) -> Result<Value, ShellError> {
    let mut record = Record::new();
    record.push(
        "attempt",
        state
            .attempt
            .map(|attempt| attempt_record_value(attempt, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push("tx_closed", Value::bool(state.tx_closed, span));
    record.push("env_state", Value::string(state.env_state, span));
    if let Some(diff) = state.diff {
        record.push("clean", Value::bool(diff.clean, span));
        record.push("env_diff", Value::string(diff.text, span));
        record.push("json", json_text_value(&diff.json, span)?);
    }
    Ok(Value::record(record, span))
}

fn attempt_finish_value(finish: AttemptFinishResponse, span: Span) -> Result<Value, ShellError> {
    let mut record = Record::new();
    record.push(
        "attempt",
        finish
            .attempt
            .map(|attempt| attempt_record_value(attempt, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
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

fn required_config() -> Result<GatewayRuntimeConfig, ShellError> {
    config().ok_or_else(|| stone_error("env", "Gateway runtime config is not active"))
}

#[cfg(unix)]
fn with_client<T>(
    config: &GatewayRuntimeConfig,
    call: impl FnOnce(
        &mut GatewayRpcClient<std::os::unix::net::UnixStream>,
    ) -> waymark_gateway_client::Result<T>,
) -> Result<T, ShellError> {
    match &config.endpoint {
        GatewayEndpoint::Unix(path) => {
            let mut client = GatewayRpcClient::connect_unix(path)
                .map_err(|err| stone_error("gateway connect", err.to_string()))?;
            call(&mut client).map_err(|err| stone_error("gateway rpc", err.to_string()))
        }
        GatewayEndpoint::Vsock { .. } => Err(stone_error(
            "gateway connect",
            "Gateway vsock transport is not wired in this build",
        )),
    }
}

#[cfg(not(unix))]
fn with_client<T>(
    _config: &GatewayRuntimeConfig,
    _call: impl FnOnce(&mut ()) -> waymark_gateway_client::Result<T>,
) -> Result<T, ShellError> {
    Err(stone_error(
        "gateway connect",
        "Gateway transport is not wired in this build",
    ))
}

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}
