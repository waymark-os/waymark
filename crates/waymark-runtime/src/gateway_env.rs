// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};
use waymark_gateway_client::GatewayRpcClient;

use crate::gateway_runtime::{config, GatewayEndpoint, GatewayRuntimeConfig};
use crate::json::json_to_nu_value;

pub(crate) fn enabled() -> bool {
    config().is_some()
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
    record.push("reason", Value::string(checkpoint.reason, span));
    record.push(
        "created_at_ms",
        Value::int(checkpoint.created_at_ms as i64, span),
    );
    record.push(
        "storage_bytes",
        Value::int(checkpoint.storage_bytes as i64, span),
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
            record.push("status", Value::string(checkpoint.status, span));
            record.push("retention", Value::string(checkpoint.retention, span));
            record.push("reason", Value::string(checkpoint.reason, span));
            record.push(
                "storage_bytes",
                Value::int(checkpoint.storage_bytes as i64, span),
            );
            Value::record(record, span)
        })
        .collect();
    Ok(Value::list(checkpoints, span))
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
