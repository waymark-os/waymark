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

pub(crate) fn env_commit(message: String, allow_risky: bool) -> Result<Value, ShellError> {
    let config = required_config()?;
    let commit = with_client(&config, |client| {
        client.env_commit(&config.tx, message.clone(), allow_risky)
    })?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("tx", Value::string(config.tx, span));
    record.push("generation", Value::string(commit.generation, span));
    record.push("message", Value::string(message, span));
    record.push("allow_risky", Value::bool(allow_risky, span));
    Ok(Value::record(record, span))
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
