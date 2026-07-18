// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};
use serde_json::{json, Value as JsonValue};
use waymark_gateway_client::proto::{
    attempt_program, AttemptChannelAttachBootRequest, AttemptChannelAttachRequest,
    AttemptChannelAttachResponse, AttemptControlBlock, AttemptReportResultRequest,
    ModelCallRequest, ModelMessage, ModelSampling, TaskSpec as GatewayTaskSpec, WorkspaceEntry,
    WorkspaceEntryKind,
};
use waymark_gateway_client::{GatewayRpcClient, LinuxExecOptions, LinuxProbeOptions};

use crate::agent::{AgentError, AgentModelGateway};
use crate::json::json_to_nu_value;
use crate::json::nu_to_json_value;

const DEFAULT_RUN_SYNC_BUDGET_MS: u64 = 90_000;
const DEFAULT_AGENT_WORKSPACE_READ_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum GatewayEndpoint {
    Unix(PathBuf),
    Vsock { cid: u32, port: u32 },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GatewayRuntimeConfig {
    pub(crate) endpoint: GatewayEndpoint,
    pub(crate) attempt: String,
    pub(crate) controller_run: String,
    pub(crate) boot_token: String,
    pub(crate) tx: String,
    pub(crate) image: String,
    pub(crate) container: Option<String>,
    pub(crate) workspace_mount: String,
    pub(crate) host_workspace_path: Option<PathBuf>,
    pub(crate) capability_profile: String,
    pub(crate) model_class: String,
    pub(crate) control: Option<AttemptControlBlock>,
}

static CONFIG: OnceLock<RwLock<Option<GatewayRuntimeConfig>>> = OnceLock::new();

#[allow(dead_code)]
pub(crate) fn set_config(config: Option<GatewayRuntimeConfig>) {
    let lock = CONFIG.get_or_init(|| RwLock::new(None));
    if let Ok(mut guard) = lock.write() {
        *guard = config;
    }
}

pub(crate) fn config() -> Option<GatewayRuntimeConfig> {
    if let Some(config) = CONFIG
        .get()
        .and_then(|lock| lock.read().ok().and_then(|guard| guard.clone()))
    {
        return Some(config);
    }
    config_from_process_env()
}

pub(crate) fn enabled() -> bool {
    config().is_some()
}

pub(crate) struct GatewayAgentModelGateway {
    config: GatewayRuntimeConfig,
    #[cfg(any(unix, target_os = "hermit"))]
    client: Option<GatewayRpcClient<GatewayClientStream>>,
}

impl GatewayAgentModelGateway {
    pub(crate) fn active() -> Option<Self> {
        config().map(|config| Self {
            config,
            #[cfg(any(unix, target_os = "hermit"))]
            client: None,
        })
    }

    #[cfg(any(unix, target_os = "hermit"))]
    fn ensure_client(&mut self) -> Result<(), ShellError> {
        if self.client.is_none() {
            let mut config = self.config.clone();
            let client = connect_attached_gateway_client(&mut config)?;
            self.config = config;
            self.client = Some(client);
        }
        Ok(())
    }

    #[cfg(any(unix, target_os = "hermit"))]
    fn client_and_config(
        &mut self,
    ) -> Result<
        (
            GatewayRuntimeConfig,
            &mut GatewayRpcClient<GatewayClientStream>,
        ),
        ShellError,
    > {
        self.ensure_client()?;
        let config = self.config.clone();
        Ok((
            config,
            self.client.as_mut().expect("gateway client initialized"),
        ))
    }

    #[cfg(any(unix, target_os = "hermit"))]
    pub(crate) fn attempt_task_value(&mut self) -> Result<JsonValue, ShellError> {
        self.ensure_client()?;
        control_task_value(&self.config)
    }

    #[cfg(any(unix, target_os = "hermit"))]
    pub(crate) fn install_shared_client(&mut self) -> Result<(), ShellError> {
        self.ensure_client()?;
        set_config(Some(self.config.clone()));
        if let Some(client) = self.client.take() {
            shared_gateway_client()
                .lock()
                .map_err(|_| stone_error("gateway client", "shared Gateway client lock poisoned"))?
                .replace(client);
        }
        Ok(())
    }

    #[cfg(any(unix, target_os = "hermit"))]
    pub(crate) fn report_attempt_result(&mut self, response: &JsonValue) -> Result<(), ShellError> {
        let request = attempt_report_result_request(&self.config, response)?;
        if let Some(client) = self.client.as_mut() {
            client
                .attempt_report_result(request)
                .map_err(|err| stone_error("gateway report", err.to_string()))?;
            self.client.take();
            return Ok(());
        }
        let result = with_client(&self.config, |client| client.attempt_report_result(request));
        clear_shared_gateway_client();
        result.map(|_| ())
    }

    #[cfg(not(any(unix, target_os = "hermit")))]
    pub(crate) fn install_shared_client(&mut self) -> Result<(), ShellError> {
        Err(stone_error(
            "gateway connect",
            "Gateway transport is not wired in this build",
        ))
    }

    #[cfg(not(any(unix, target_os = "hermit")))]
    pub(crate) fn attempt_task_value(&mut self) -> Result<JsonValue, ShellError> {
        Err(stone_error(
            "gateway connect",
            "Gateway transport is not wired in this build",
        ))
    }

    #[cfg(not(any(unix, target_os = "hermit")))]
    pub(crate) fn report_attempt_result(
        &mut self,
        _response: &JsonValue,
    ) -> Result<(), ShellError> {
        Err(stone_error(
            "gateway connect",
            "Gateway transport is not wired in this build",
        ))
    }

    #[cfg(not(any(unix, target_os = "hermit")))]
    fn client_and_config(
        &mut self,
    ) -> Result<
        (
            GatewayRuntimeConfig,
            &mut GatewayRpcClient<UnsupportedGatewayStream>,
        ),
        ShellError,
    > {
        Err(stone_error(
            "gateway connect",
            "Gateway transport is not wired in this build",
        ))
    }
}

impl AgentModelGateway for GatewayAgentModelGateway {
    fn request_model(&mut self, request: &JsonValue) -> Result<JsonValue, AgentError> {
        let (config, client) = self.client_and_config().map_err(agent_gateway_error)?;
        gateway_model_request(&config, client, request).map_err(agent_gateway_error)
    }

    fn request_workspace_rpc(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
        match self.client_and_config() {
            Ok((config, client)) => {
                gateway_workspace_request(&config, client, request).map_err(|err| err.to_string())
            }
            Err(err) => Err(err.to_string()),
        }
    }

    fn request_linux_rpc(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
        match self.client_and_config() {
            Ok((config, client)) => {
                gateway_linux_request(&config, client, request).map_err(|err| err.to_string())
            }
            Err(err) => Err(err.to_string()),
        }
    }
}

fn gateway_model_request(
    config: &GatewayRuntimeConfig,
    client: &mut GatewayRpcClient<GatewayClientStream>,
    request: &JsonValue,
) -> Result<JsonValue, ShellError> {
    let rpc_request = model_call_request_from_json(config, request)?;
    let response = client
        .model_call(rpc_request)
        .map_err(|err| stone_error("gateway rpc", err.to_string()))?;
    Ok(json!({
        "ok": true,
        "text": response.content,
        "provider": response.provider,
        "request_id": response.provider_request_id,
        "model": response.resolved_model,
        "finish_reason": response.finish_reason,
        "latency_ms": response.latency_ms,
        "usage": response.usage.map(|usage| json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens,
        })).unwrap_or(JsonValue::Null),
    }))
}

fn attempt_report_result_request(
    config: &GatewayRuntimeConfig,
    response: &JsonValue,
) -> Result<AttemptReportResultRequest, ShellError> {
    let result_json = serde_json::to_string(response).map_err(|err| {
        stone_error(
            "gateway report",
            format!("failed to encode task response JSON: {err}"),
        )
    })?;
    let ok = response
        .get("ok")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let error_json = if ok {
        String::new()
    } else {
        serde_json::to_string(response.get("error").unwrap_or(response)).map_err(|err| {
            stone_error(
                "gateway report",
                format!("failed to encode task error JSON: {err}"),
            )
        })?
    };
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("source".to_string(), "waymark-runtime".to_string());
    if let Some(kind) = response.get("kind").and_then(JsonValue::as_str) {
        metadata.insert("kind".to_string(), kind.to_string());
    }
    Ok(AttemptReportResultRequest {
        attempt: config.attempt.clone(),
        status: if ok { "succeeded" } else { "failed" }.to_string(),
        result_json,
        error_json,
        reason: report_reason(response),
        metadata,
    })
}

fn report_reason(response: &JsonValue) -> String {
    if response
        .get("ok")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return response
            .get("kind")
            .and_then(JsonValue::as_str)
            .unwrap_or("success")
            .to_string();
    }
    response
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(JsonValue::as_str)
        .or_else(|| response.get("kind").and_then(JsonValue::as_str))
        .unwrap_or("failed")
        .to_string()
}

fn model_call_request_from_json(
    config: &GatewayRuntimeConfig,
    request: &JsonValue,
) -> Result<ModelCallRequest, ShellError> {
    let messages = request
        .get("messages")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| stone_error("gateway model", "model request requires messages array"))?
        .iter()
        .map(model_message_from_json)
        .collect::<Result<Vec<_>, _>>()?;
    if messages.is_empty() {
        return Err(stone_error(
            "gateway model",
            "model request messages array must not be empty",
        ));
    }

    let sampling = model_sampling_from_json(request)?;
    let max_output_tokens = match optional_u32_json(request, "max_output_tokens")? {
        Some(value) => value,
        None => optional_u32_json(request, "max_tokens")?.unwrap_or_default(),
    };
    Ok(ModelCallRequest {
        attempt: config.attempt.clone(),
        capability_profile: std::env::var("WAYMARK_GATEWAY_MODEL_CAPABILITY_PROFILE")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| config.capability_profile.clone()),
        model_class: request
            .get("model_class")
            .and_then(JsonValue::as_str)
            .map(str::to_string)
            .or_else(|| std::env::var("WAYMARK_GATEWAY_MODEL_CLASS").ok())
            .or_else(|| (!config.model_class.is_empty()).then(|| config.model_class.clone()))
            .unwrap_or_else(|| "agent".to_string()),
        model_hint: request
            .get("model")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        messages,
        sampling,
        max_output_tokens,
        response_format: response_format_from_json(request.get("response_format"))?,
        metadata: model_metadata_from_json(request),
    })
}

fn model_message_from_json(value: &JsonValue) -> Result<ModelMessage, ShellError> {
    let object = value
        .as_object()
        .ok_or_else(|| stone_error("gateway model", "message must be an object"))?;
    let role = object
        .get("role")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| stone_error("gateway model", "message requires role"))?;
    let content = object
        .get("content")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| stone_error("gateway model", "message requires string content"))?;
    Ok(ModelMessage {
        role: role.to_string(),
        content: content.to_string(),
        name: object
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        metadata: Default::default(),
    })
}

fn model_sampling_from_json(request: &JsonValue) -> Result<Option<ModelSampling>, ShellError> {
    let temperature = optional_f64_json(request, "temperature")?;
    let top_p = optional_f64_json(request, "top_p")?;
    let seed = optional_u32_json(request, "seed")?;
    if temperature.is_none() && top_p.is_none() && seed.is_none() {
        return Ok(None);
    }
    Ok(Some(ModelSampling {
        temperature: temperature.unwrap_or_default(),
        top_p: top_p.unwrap_or_default(),
        seed: seed.unwrap_or_default(),
    }))
}

fn response_format_from_json(value: Option<&JsonValue>) -> Result<String, ShellError> {
    match value {
        Some(JsonValue::String(value)) => Ok(value.clone()),
        Some(value @ JsonValue::Object(_)) => serde_json::to_string(value)
            .map_err(|err| stone_error("gateway model", format!("invalid response_format: {err}"))),
        Some(_) => Err(stone_error(
            "gateway model",
            "response_format must be a string or object",
        )),
        None => Ok(String::new()),
    }
}

fn model_metadata_from_json(request: &JsonValue) -> std::collections::HashMap<String, String> {
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("source".to_string(), "waymark-runtime".to_string());
    if let Some(object) = request.get("metadata").and_then(JsonValue::as_object) {
        for (key, value) in object {
            if let Some(value) = value.as_str() {
                metadata.insert(key.clone(), value.to_string());
            }
        }
    }
    metadata
}

fn optional_u32_json(value: &JsonValue, key: &str) -> Result<Option<u32>, ShellError> {
    let Some(value) = value.get(key) else {
        return Ok(None);
    };
    let Some(number) = value.as_u64() else {
        return Err(stone_error(
            "gateway model",
            format!("{key} must be an unsigned integer"),
        ));
    };
    let number = u32::try_from(number).map_err(|_| {
        stone_error(
            "gateway model",
            format!("{key} exceeds maximum unsigned 32-bit integer"),
        )
    })?;
    Ok(Some(number))
}

fn optional_f64_json(value: &JsonValue, key: &str) -> Result<Option<f64>, ShellError> {
    let Some(value) = value.get(key) else {
        return Ok(None);
    };
    value.as_f64().map(Some).ok_or_else(|| {
        stone_error(
            "gateway model",
            format!("{key} must be a finite JSON number"),
        )
    })
}

fn agent_gateway_error(err: ShellError) -> AgentError {
    AgentError {
        code: "gateway_model_rpc",
        message: err.to_string(),
    }
}

fn gateway_workspace_request(
    config: &GatewayRuntimeConfig,
    client: &mut GatewayRpcClient<GatewayClientStream>,
    request: &JsonValue,
) -> Result<JsonValue, ShellError> {
    let started = std::time::Instant::now();
    let tool = request
        .get("tool")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| stone_error("gateway workspace", "workspace request requires tool"))?;
    let input = request.get("input").unwrap_or(&JsonValue::Null);
    let path = input
        .get("path")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| stone_error("gateway workspace", "workspace request requires input.path"))?;
    let rel = workspace_path(config, Path::new(path))?;
    match tool {
        "read" => {
            let max_bytes = input
                .get("max_bytes")
                .and_then(JsonValue::as_u64)
                .unwrap_or(DEFAULT_AGENT_WORKSPACE_READ_BYTES);
            let response = client
                .workspace_tx_read(&config.tx, rel, max_bytes)
                .map_err(|err| stone_error("gateway rpc", err.to_string()))?;
            let bytes = response.content;
            let byte_len = bytes.len();
            let (content, value_truncated) = match String::from_utf8(bytes) {
                Ok(text) => (json!(text), false),
                Err(err) => (
                    json!({
                        "$type": "binary",
                        "bytes": err.into_bytes().len(),
                        "content": null,
                    }),
                    true,
                ),
            };
            Ok(json!({
                "ok": true,
                "value": {
                    "path": path,
                    "bytes": byte_len,
                    "content": content,
                    "truncated": value_truncated,
                },
                "truncated": {
                    "value": value_truncated,
                    "stdout": false,
                    "stderr": false,
                },
                "duration_ms": elapsed_ms(started),
            }))
        }
        "write" => {
            let content = input
                .get("content")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    stone_error("gateway workspace", "write request requires string content")
                })?;
            let append = input
                .get("mode")
                .and_then(JsonValue::as_str)
                .is_some_and(|mode| mode == "append");
            let response = client
                .workspace_tx_write(&config.tx, rel, content.as_bytes().to_vec(), append)
                .map_err(|err| stone_error("gateway rpc", err.to_string()))?;
            Ok(json!({
                "ok": true,
                "value": {
                    "path": path,
                    "bytes": response.bytes,
                },
                "duration_ms": elapsed_ms(started),
            }))
        }
        "list" => {
            let entries = client
                .workspace_tx_list(&config.tx, rel)
                .map_err(|err| stone_error("gateway rpc", err.to_string()))?;
            let entries = entries
                .into_iter()
                .map(|entry| {
                    json!({
                        "path": app_path(config, &entry.path),
                        "kind": entry_kind(entry.kind),
                        "bytes": entry.size,
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "ok": true,
                "value": {
                    "path": path,
                    "entries": entries,
                    "truncated": false,
                },
                "truncated": {
                    "value": false,
                    "stdout": false,
                    "stderr": false,
                },
                "duration_ms": elapsed_ms(started),
            }))
        }
        other => Err(stone_error(
            "gateway workspace",
            format!("unsupported workspace tool {other:?}"),
        )),
    }
}

fn gateway_linux_request(
    config: &GatewayRuntimeConfig,
    client: &mut GatewayRpcClient<GatewayClientStream>,
    request: &JsonValue,
) -> Result<JsonValue, ShellError> {
    let command = request
        .get("command")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| stone_error("gateway linux", "linux request requires command"))?;
    let cwd = request
        .get("cwd")
        .and_then(JsonValue::as_str)
        .unwrap_or("/app");
    let timeout_ms = request
        .get("timeout_ms")
        .and_then(JsonValue::as_u64)
        .unwrap_or(DEFAULT_RUN_SYNC_BUDGET_MS);
    let max_stdout_bytes = request
        .get("max_stdout_bytes")
        .and_then(JsonValue::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(usize::MAX);
    let max_stderr_bytes = request
        .get("max_stderr_bytes")
        .and_then(JsonValue::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(usize::MAX);
    let workdir = container_path(config, Path::new(cwd))?;
    let mut options = LinuxExecOptions::new(
        config.tx.clone(),
        config.image.clone(),
        vec!["sh".to_string(), "-lc".to_string(), command.to_string()],
    )
    .workspace_mount(config.workspace_mount.clone())
    .workdir(workdir)
    .timeout_ms(timeout_ms);
    if let Some(container) = &config.container {
        options = options.container(container.clone());
    }
    let output = client
        .linux_exec(options)
        .map_err(|err| stone_error("gateway rpc", err.to_string()))?;
    let record = linux_exec_record(output, Span::unknown(), max_stdout_bytes, max_stderr_bytes)?;
    let value = nu_to_json_value(&Value::record(record, Span::unknown()));
    Ok(json!({
        "ok": value.get("ok").and_then(JsonValue::as_bool).unwrap_or(false),
        "kind": value.get("kind").cloned().unwrap_or_else(|| json!("exec_failed")),
        "value": {
            "exit_code": value.get("exit_code").cloned().unwrap_or(JsonValue::Null),
            "cwd": cwd,
            "command": command,
        },
        "stdout": value.get("stdout").cloned().unwrap_or_else(|| json!("")),
        "stderr": value.get("stderr").cloned().unwrap_or_else(|| json!("")),
        "truncated": value.get("truncated").cloned().unwrap_or_else(|| json!({
            "stdout": false,
            "stderr": false,
            "value": false,
        })),
        "duration_ms": value.get("duration_ms").cloned().unwrap_or_else(|| json!(0)),
        "error": {
            "code": value.get("kind").and_then(JsonValue::as_str).unwrap_or("exec_failed"),
            "message": value.get("stderr").and_then(JsonValue::as_str).unwrap_or("linux exec failed"),
        },
    }))
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn read_text(path: &Path, max_bytes: usize) -> Result<String, ShellError> {
    let config = required_config()?;
    let rel = workspace_path(&config, path)?;
    let max_bytes = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    let response = with_client(&config, |client| {
        client.workspace_tx_read(&config.tx, rel, max_bytes)
    })?;
    String::from_utf8(response.content).map_err(|err| {
        stone_error(
            "gateway read_text",
            format!("{}: invalid UTF-8: {err}", path.display()),
        )
    })
}

pub(crate) fn read_bytes(path: &Path, max_bytes: usize) -> Result<Vec<u8>, ShellError> {
    let config = required_config()?;
    let rel = workspace_path(&config, path)?;
    let max_bytes = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    Ok(with_client(&config, |client| {
        client.workspace_tx_read(&config.tx, rel, max_bytes)
    })?
    .content)
}

pub(crate) fn write_text(path: &Path, text: &str, append: bool) -> Result<usize, ShellError> {
    let config = required_config()?;
    let rel = workspace_path(&config, path)?;
    let response = with_client(&config, |client| {
        client.workspace_tx_write(&config.tx, rel, text.as_bytes().to_vec(), append)
    })?;
    Ok(usize::try_from(response.bytes).unwrap_or(usize::MAX))
}

pub(crate) fn mkdir(path: &Path) -> Result<(), ShellError> {
    let config = required_config()?;
    let rel = workspace_path(&config, path)?;
    with_client(&config, |client| client.workspace_tx_mkdir(&config.tx, rel))
}

pub(crate) fn remove(path: &Path) -> Result<(), ShellError> {
    let config = required_config()?;
    let rel = workspace_path(&config, path)?;
    with_client(&config, |client| {
        client.workspace_tx_remove(&config.tx, rel)
    })
}

pub(crate) fn stat_record(path: &Path, span: Span) -> Result<Value, ShellError> {
    let config = required_config()?;
    let rel = workspace_path(&config, path)?;
    let entry = with_client(&config, |client| client.workspace_tx_stat(&config.tx, rel))?;
    Ok(Value::record(
        entry_record(entry, path.to_path_buf(), span),
        span,
    ))
}

pub(crate) fn list_dir_records(path: &Path, span: Span) -> Result<Vec<Value>, ShellError> {
    let config = required_config()?;
    let rel = workspace_path(&config, path)?;
    let entries = with_client(&config, |client| client.workspace_tx_list(&config.tx, rel))?;
    Ok(entries
        .into_iter()
        .map(|entry| {
            let guest_path = path.join(
                Path::new(&entry.path)
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("")),
            );
            Value::record(entry_record(entry, guest_path, span), span)
        })
        .collect())
}

pub(crate) fn run_command(
    argv: &[String],
    cwd: &Path,
    env_overrides: &[(String, String)],
    stdin: Option<&str>,
    timeout: Duration,
    background: bool,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<Record, ShellError> {
    let config = required_config()?;
    let workdir = container_path(&config, cwd)?;
    let mut options = LinuxExecOptions::new(config.tx.clone(), config.image.clone(), argv.to_vec())
        .workspace_mount(config.workspace_mount.clone())
        .workdir(workdir)
        .timeout_ms(if background {
            1
        } else {
            run_sync_timeout_ms(timeout)
        });
    if let Some(container) = &config.container {
        options = options.container(container.clone());
    }
    if let Some(stdin) = stdin {
        options = options.stdin(stdin.to_string());
    }
    for (key, value) in env_overrides {
        options = options.env(key.clone(), value.clone());
    }
    let output = with_client(&config, |client| client.linux_exec(options))?;
    let span = Span::unknown();
    let mut record = linux_exec_record(output, span, max_stdout_bytes, max_stderr_bytes)?;
    record.push("background", Value::bool(background, span));
    record.push("cwd", Value::string(cwd.display().to_string(), span));
    record.push(
        "argv",
        Value::list(
            argv.iter()
                .map(|arg| Value::string(arg.clone(), span))
                .collect(),
            span,
        ),
    );
    Ok(record)
}

fn run_sync_timeout_ms(timeout: Duration) -> u64 {
    let requested_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let budget_ms = std::env::var("WAYMARK_GATEWAY_RUN_SYNC_BUDGET_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUN_SYNC_BUDGET_MS);
    requested_ms.min(budget_ms)
}

pub(crate) fn run_wait(run_id: &str, timeout: Duration) -> Result<Record, ShellError> {
    let config = required_config()?;
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let output = with_client(&config, |client| client.linux_exec_wait(run_id, timeout_ms))?;
    let span = Span::unknown();
    let mut record = linux_exec_record(output, span, usize::MAX, usize::MAX)?;
    append_run_ownership_fields(&mut record, &config, run_id, span);
    Ok(record)
}

pub(crate) fn run_status(run_id: &str) -> Result<Record, ShellError> {
    let config = required_config()?;
    let output = with_client(&config, |client| client.linux_exec_wait(run_id, 1))?;
    let span = Span::unknown();
    let mut record = linux_exec_record(output, span, usize::MAX, usize::MAX)?;
    append_run_ownership_fields(&mut record, &config, run_id, span);
    Ok(record)
}

pub(crate) fn run_terminate(run_id: &str) -> Result<Record, ShellError> {
    let config = required_config()?;
    let output = with_client(&config, |client| client.linux_exec_terminate(run_id))?;
    linux_exec_record(output, Span::unknown(), usize::MAX, usize::MAX)
}

pub(crate) fn start_daemon(
    argv: &[String],
    cwd: &Path,
    env_overrides: &[(String, String)],
) -> Result<Value, ShellError> {
    let span = Span::unknown();
    let output = run_command(
        argv,
        cwd,
        env_overrides,
        None,
        Duration::from_millis(1),
        true,
        usize::MAX,
        usize::MAX,
    )?;
    let still_running = record_bool(&output, "still_running").unwrap_or(false);
    let mut record = Record::new();
    record.push("ok", Value::bool(still_running, span));
    record.push(
        "kind",
        Value::string(if still_running { "started" } else { "exited" }, span),
    );
    record.push("pid", Value::nothing(span));
    if let Some(run_id) = record_string(&output, "run_id") {
        record.push("run_id", Value::string(run_id, span));
    }
    record.push("cwd", Value::string(cwd.display().to_string(), span));
    record.push(
        "argv",
        Value::list(
            argv.iter()
                .map(|arg| Value::string(arg.clone(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "stdout",
        output
            .get("stdout")
            .cloned()
            .unwrap_or_else(|| Value::string("", span)),
    );
    record.push(
        "stderr",
        output
            .get("stderr")
            .cloned()
            .unwrap_or_else(|| Value::string("", span)),
    );
    record.push("stdout_path", Value::nothing(span));
    record.push("stderr_path", Value::nothing(span));
    record.push(
        "explanation",
        Value::string(
            "Gateway started the daemon through linux.exec; use daemon_status() or stop_daemon() with the returned run_id.",
            span,
        ),
    );
    Ok(Value::record(record, span))
}

pub(crate) fn daemon_status(run_id: &str, timeout: Duration) -> Result<Value, ShellError> {
    let span = Span::unknown();
    let output = run_wait(run_id, timeout)?;
    let running = record_bool(&output, "still_running").unwrap_or(false);
    let ok = running || record_bool(&output, "ok").unwrap_or(false);
    let mut record = Record::new();
    record.push("ok", Value::bool(ok, span));
    record.push("running", Value::bool(running, span));
    record.push("run_id", Value::string(run_id.to_string(), span));
    record.push("pid", Value::nothing(span));
    record.push(
        "stdout",
        output
            .get("stdout")
            .cloned()
            .unwrap_or_else(|| Value::string("", span)),
    );
    record.push(
        "stderr",
        output
            .get("stderr")
            .cloned()
            .unwrap_or_else(|| Value::string("", span)),
    );
    record.push(
        "exit_code",
        output
            .get("exit_code")
            .cloned()
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "timed_out",
        output
            .get("timed_out")
            .cloned()
            .unwrap_or_else(|| Value::bool(false, span)),
    );
    push_if_present(&mut record, "processes", &output, span);
    push_if_present(&mut record, "listen_addrs", &output, span);
    push_if_present(&mut record, "open_files", &output, span);
    Ok(Value::record(record, span))
}

pub(crate) fn stop_daemon(run_id: &str) -> Result<Value, ShellError> {
    let span = Span::unknown();
    let output = run_terminate(run_id)?;
    let mut record = Record::new();
    record.push("ok", Value::bool(true, span));
    record.push("kind", Value::string("stopped", span));
    record.push("run_id", Value::string(run_id.to_string(), span));
    record.push("pid", Value::nothing(span));
    record.push(
        "stdout",
        output
            .get("stdout")
            .cloned()
            .unwrap_or_else(|| Value::string("", span)),
    );
    record.push(
        "stderr",
        output
            .get("stderr")
            .cloned()
            .unwrap_or_else(|| Value::string("", span)),
    );
    record.push(
        "exit_code",
        output
            .get("exit_code")
            .cloned()
            .unwrap_or_else(|| Value::nothing(span)),
    );
    Ok(Value::record(record, span))
}

pub(crate) fn ps_record(interval_ms: u64, cwd: &Path) -> Result<Value, ShellError> {
    let _ = cwd;
    let config = required_config()?;
    let target = probe_options(&config);
    let output = with_client(&config, |client| client.linux_ps(target, interval_ms))?;
    let span = Span::unknown();
    Ok(Value::list(
        output
            .processes
            .into_iter()
            .map(|process| {
                let mut record = Record::with_capacity(14);
                record.push("pid", Value::int(process.pid, span));
                record.push("ppid", Value::int(process.ppid, span));
                record.push("name", Value::string(process.name, span));
                record.push("command", Value::string(process.command, span));
                record.push("status", Value::string(process.status, span));
                record.push("cwd", Value::string(process.cwd, span));
                record.push("cpu_percent", Value::float(process.cpu_percent, span));
                record.push("memory_bytes", Value::int(process.memory_bytes, span));
                record.push("virtual_bytes", Value::int(process.virtual_bytes, span));
                record.push("owner_uid", Value::int(process.owner_uid, span));
                record.push("owner_kind", Value::string(process.owner_kind, span));
                record.push("owner_id", Value::string(process.owner_id, span));
                record.push(
                    "listen_addrs",
                    Value::list(
                        process
                            .listen_addrs
                            .into_iter()
                            .map(|addr| Value::string(addr, span))
                            .collect(),
                        span,
                    ),
                );
                record.push(
                    "open_files",
                    Value::list(
                        process
                            .open_files
                            .into_iter()
                            .map(|path| Value::string(path, span))
                            .collect(),
                        span,
                    ),
                );
                Value::record(record, span)
            })
            .collect(),
        span,
    ))
}

pub(crate) fn resolve_command_record(name: &str, cwd: &Path) -> Result<Value, ShellError> {
    let _ = cwd;
    let config = required_config()?;
    let target = probe_options(&config);
    let output = with_client(&config, |client| client.linux_resolve_command(target, name))?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("ok", Value::bool(output.ok, span));
    record.push("name", Value::string(output.name.clone(), span));
    record.push(
        "path",
        if output.path.is_empty() {
            Value::nothing(span)
        } else {
            Value::string(output.path, span)
        },
    );
    record.push(
        "matches",
        Value::list(
            output
                .matches
                .iter()
                .map(|path| Value::string(path.clone(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "searched",
        Value::list(
            output
                .searched
                .iter()
                .map(|path| Value::string(path.clone(), span))
                .collect(),
            span,
        ),
    );
    let mut explanation = Record::new();
    explanation.push(
        "kind",
        Value::string(
            if output.ok {
                "resolved_executable"
            } else {
                "executable_not_found"
            },
            span,
        ),
    );
    explanation.push(
        "summary",
        Value::string(
            if output.ok {
                format!(
                    "Executable `{}` resolves to `{}`.",
                    output.name, output.matches[0]
                )
            } else {
                format!("Executable `{}` was not found in PATH.", output.name)
            },
            span,
        ),
    );
    record.push("explanation", Value::record(explanation, span));
    Ok(Value::record(record, span))
}

pub(crate) fn sysinfo_record(section: Option<&str>, cwd: &Path) -> Result<Value, ShellError> {
    let _ = cwd;
    let config = required_config()?;
    let target = probe_options(&config);
    let output = with_client(&config, |client| {
        client.linux_sysinfo(target, section.unwrap_or("all"))
    })?;
    let parsed: JsonValue = serde_json::from_str(&output.json).map_err(|err| {
        stone_error(
            "sysinfo",
            format!("Gateway returned invalid sysinfo JSON: {err}"),
        )
    })?;
    Ok(json_to_nu_value(parsed, Span::unknown()))
}

pub(crate) fn wait_port_record(
    host: &str,
    port: u16,
    protocol: &str,
    timeout: Duration,
    cwd: &Path,
) -> Result<Value, ShellError> {
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let _ = cwd;
    let config = required_config()?;
    let target = probe_options(&config);
    let output = with_client(&config, |client| {
        client.linux_wait_port(target, host, u32::from(port), protocol, timeout_ms)
    })?;
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("ok", Value::bool(output.ok, span));
    record.push("kind", Value::string(output.kind, span));
    record.push("host", Value::string(output.host, span));
    record.push("port", Value::int(i64::from(output.port), span));
    record.push("protocol", Value::string(output.protocol, span));
    record.push(
        "duration_ms",
        Value::int(i64::try_from(output.duration_ms).unwrap_or(i64::MAX), span),
    );
    if !output.error.is_empty() {
        record.push("error", Value::string(output.error, span));
    }
    Ok(Value::record(record, span))
}

pub(crate) fn linux_exec_record(
    output: waymark_gateway_client::proto::LinuxExecResponse,
    span: Span,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<Record, ShellError> {
    let suggested_actions = linux_exec_suggested_actions(&output);
    let stdout = truncate_text(output.stdout, max_stdout_bytes);
    let stderr = truncate_text(output.stderr, max_stderr_bytes);
    let mut record = Record::new();
    record.push("ok", Value::bool(output.status == 0, span));
    record.push(
        "kind",
        Value::string(
            if output.still_running {
                "still_running"
            } else if output.timed_out {
                "timeout"
            } else if output.status == 0 {
                "success"
            } else {
                "exec_failed"
            },
            span,
        ),
    );
    record.push("exit_code", Value::int(i64::from(output.status), span));
    record.push(
        "duration_ms",
        Value::int(i64::try_from(output.duration_ms).unwrap_or(i64::MAX), span),
    );
    record.push("stdout", Value::string(stdout.text, span));
    record.push("stderr", Value::string(stderr.text, span));
    record.push(
        "stdout_bytes",
        Value::int(i64::try_from(output.stdout_bytes).unwrap_or(i64::MAX), span),
    );
    record.push(
        "stderr_bytes",
        Value::int(i64::try_from(output.stderr_bytes).unwrap_or(i64::MAX), span),
    );
    record.push("stdout_tail", Value::string(output.stdout_tail, span));
    record.push("stderr_tail", Value::string(output.stderr_tail, span));
    record.push("timed_out", Value::bool(output.timed_out, span));
    record.push("still_running", Value::bool(output.still_running, span));
    record.push("done", Value::bool(!output.still_running, span));
    if output.timed_out {
        record.push(
            "partial_output_hint",
            Value::string(
                "The command reached a timeout, but stdout/stderr may contain useful partial results or readiness signals. Inspect stdout_tail, stderr_tail, stdout, and stderr before retrying or terminating.",
                span,
            ),
        );
    }
    record.push(
        "next_action",
        Value::string(
            if output.still_running {
                "run_status_or_run_wait_or_run_terminate"
            } else if output.status == 0 {
                "inspect_result_or_commit"
            } else {
                "inspect_error_or_retry"
            },
            span,
        ),
    );
    record.push(
        "suggested_actions",
        Value::list(
            suggested_actions
                .into_iter()
                .map(|action| Value::string(action, span))
                .collect(),
            span,
        ),
    );
    if !output.run_id.is_empty() {
        record.push("run_id", Value::string(output.run_id, span));
    }
    let mut truncated = Record::new();
    truncated.push("stdout", Value::bool(stdout.truncated, span));
    truncated.push("stderr", Value::bool(stderr.truncated, span));
    record.push("truncated", Value::record(truncated, span));
    if let Some(diff) = output.diff {
        record.push("env_diff", Value::string(diff.text, span));
    }
    Ok(record)
}

fn linux_exec_suggested_actions(
    output: &waymark_gateway_client::proto::LinuxExecResponse,
) -> Vec<&'static str> {
    if output.still_running && output.timed_out {
        return vec![
            "Inspect stdout_tail, stderr_tail, stdout, and stderr for partial success, readiness, prompts, paths, ports, or other progress signals.",
            "If partial output shows the task is ready or nearly complete, validate with a short follow-up command instead of terminating.",
            "If the process should continue, use run_status or run_wait with a bounded timeout.",
            "Terminate only when the partial output indicates the process is stale, wrong, or no longer useful.",
        ];
    }
    if output.timed_out {
        return vec![
            "Inspect stdout_tail, stderr_tail, stdout, and stderr for partial success, readiness, prompts, paths, ports, or other progress signals before retrying.",
            "If partial output is useful, continue from that state or validate it with a short command.",
            "Retry only after using the partial output to adjust the command or plan.",
        ];
    }
    if output.status == 0 {
        vec!["Inspect the result and continue with the next task step."]
    } else {
        vec!["Inspect stdout, stderr, stdout_tail, and stderr_tail before retrying or changing approach."]
    }
}

fn record_bool(record: &Record, key: &str) -> Option<bool> {
    let Value::Bool { val, .. } = record.get(key)? else {
        return None;
    };
    Some(*val)
}

fn record_string(record: &Record, key: &str) -> Option<String> {
    let Value::String { val, .. } = record.get(key)? else {
        return None;
    };
    Some(val.clone())
}

fn push_if_present(target: &mut Record, key: &str, source: &Record, span: Span) {
    target.push(
        key,
        source
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::list(Vec::new(), span)),
    );
}

fn append_run_ownership_fields(
    record: &mut Record,
    config: &GatewayRuntimeConfig,
    run_id: &str,
    span: Span,
) {
    let output = match with_client(config, |client| client.linux_ps(probe_options(config), 0)) {
        Ok(output) => output,
        Err(_) => {
            record.push("processes", Value::list(Vec::new(), span));
            record.push("listen_addrs", Value::list(Vec::new(), span));
            record.push("open_files", Value::list(Vec::new(), span));
            return;
        }
    };
    let mut listen_addrs = Vec::new();
    let mut open_files = Vec::new();
    let mut processes = Vec::new();
    for process in output
        .processes
        .into_iter()
        .filter(|process| process.owner_id == run_id)
    {
        for addr in &process.listen_addrs {
            if !listen_addrs.contains(addr) {
                listen_addrs.push(addr.clone());
            }
        }
        for path in &process.open_files {
            if !open_files.contains(path) {
                open_files.push(path.clone());
            }
        }
        let mut process_record = Record::new();
        process_record.push("pid", Value::int(process.pid, span));
        process_record.push("ppid", Value::int(process.ppid, span));
        process_record.push("name", Value::string(process.name, span));
        process_record.push("command", Value::string(process.command, span));
        process_record.push("status", Value::string(process.status, span));
        process_record.push("cwd", Value::string(process.cwd, span));
        process_record.push("owner_kind", Value::string(process.owner_kind, span));
        process_record.push("owner_id", Value::string(process.owner_id, span));
        process_record.push(
            "listen_addrs",
            Value::list(
                process
                    .listen_addrs
                    .into_iter()
                    .map(|addr| Value::string(addr, span))
                    .collect(),
                span,
            ),
        );
        process_record.push(
            "open_files",
            Value::list(
                process
                    .open_files
                    .into_iter()
                    .map(|path| Value::string(path, span))
                    .collect(),
                span,
            ),
        );
        processes.push(Value::record(process_record, span));
    }
    record.push("processes", Value::list(processes, span));
    record.push(
        "listen_addrs",
        Value::list(
            listen_addrs
                .into_iter()
                .map(|addr| Value::string(addr, span))
                .collect(),
            span,
        ),
    );
    record.push(
        "open_files",
        Value::list(
            open_files
                .into_iter()
                .map(|path| Value::string(path, span))
                .collect(),
            span,
        ),
    );
}

fn probe_options(config: &GatewayRuntimeConfig) -> LinuxProbeOptions {
    let options = LinuxProbeOptions::new(config.tx.clone(), config.image.clone())
        .workspace_mount(config.workspace_mount.clone());
    if let Some(container) = &config.container {
        options.container(container.clone())
    } else {
        options
    }
}

fn control_task_value(config: &GatewayRuntimeConfig) -> Result<JsonValue, ShellError> {
    let control = config
        .control
        .as_ref()
        .ok_or_else(|| stone_error("gateway attempt", "attached attempt has no control block"))?;
    let task_spec = control
        .task_spec
        .as_ref()
        .ok_or_else(|| stone_error("gateway attempt", "attempt control block has no task_spec"))?;
    let id = if task_spec.id.is_empty() {
        config.attempt.as_str()
    } else {
        task_spec.id.as_str()
    };
    let program = control
        .program
        .as_ref()
        .and_then(|program| program.program.as_ref());
    match program {
        Some(attempt_program::Program::Stone(stone)) => Ok(json!({
            "version": 0,
            "id": id,
            "runtime": {"frontend": "stone"},
            "script": {"source": stone.source},
            "artifacts": control_artifacts(task_spec, &config.workspace_mount),
        })),
        Some(attempt_program::Program::Builtin(builtin))
            if builtin.name.is_empty()
                || builtin.name == "agent"
                || builtin.name == "waymark.agent"
                || builtin.name == "react" =>
        {
            let args = control_program_args(&builtin.args_json)?;
            let task = args
                .get("task")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| control_task_prompt(task_spec));
            let mut agent = json!({
                "task": task,
                "max_turns": json_u64(&args, "max_turns").unwrap_or(16),
                "max_rounds": json_u64(&args, "max_rounds").unwrap_or(16),
            });
            if let Some(model) = args.get("model").and_then(JsonValue::as_str) {
                if !model.is_empty() {
                    agent["model"] = JsonValue::String(model.to_string());
                }
            }
            if let Some(completion_path) = args.get("completion_path").and_then(JsonValue::as_str) {
                if !completion_path.is_empty() {
                    agent["completion_path"] = JsonValue::String(completion_path.to_string());
                }
            }
            if let Some(max_tool_ms) = json_u64(&args, "max_tool_ms") {
                agent["max_tool_ms"] = JsonValue::from(max_tool_ms);
            }
            Ok(json!({
                "version": 0,
                "id": id,
                "runtime": {"frontend": "agent"},
                "agent": agent,
                "artifacts": control_artifacts(task_spec, &config.workspace_mount),
            }))
        }
        Some(attempt_program::Program::Builtin(builtin)) => Err(stone_error(
            "gateway attempt",
            format!("unsupported builtin attempt program `{}`", builtin.name),
        )),
        Some(attempt_program::Program::Artifact(_)) => Err(stone_error(
            "gateway attempt",
            "artifact attempt programs are not executable by the LibOS runtime yet",
        )),
        None => Err(stone_error(
            "gateway attempt",
            "attempt control block has no executable program",
        )),
    }
}

fn control_program_args(args_json: &str) -> Result<JsonValue, ShellError> {
    if args_json.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(args_json).map_err(|err| {
        stone_error(
            "gateway attempt",
            format!("invalid program args_json: {err}"),
        )
    })
}

fn json_u64(value: &JsonValue, key: &str) -> Option<u64> {
    value.get(key).and_then(JsonValue::as_u64)
}

fn control_task_prompt(task_spec: &GatewayTaskSpec) -> String {
    let mut lines = Vec::new();
    if !task_spec.objective.is_empty() {
        lines.push(task_spec.objective.clone());
    }
    for input in &task_spec.inputs {
        lines.push(format!(
            "Input: {} kind={} path={} mode={}",
            input.name, input.kind, input.path, input.mode
        ));
    }
    for output in &task_spec.outputs {
        lines.push(format!(
            "Output: {} kind={} path={} mode={} content_type={}",
            output.name, output.kind, output.path, output.mode, output.content_type
        ));
    }
    for success in &task_spec.success {
        lines.push(format!(
            "Success: kind={} input={} output={} path={} content={}",
            success.kind, success.input, success.output, success.path, success.content
        ));
    }
    for constraint in &task_spec.constraints {
        lines.push(format!(
            "Constraint: kind={} path={} enforcement={} args_json={}",
            constraint.kind, constraint.path, constraint.enforcement, constraint.args_json
        ));
    }
    lines.join("\n")
}

fn control_artifacts(task_spec: &GatewayTaskSpec, workspace_mount: &str) -> Vec<JsonValue> {
    task_spec
        .outputs
        .iter()
        .filter(|output| !output.path.is_empty())
        .map(|output| {
            let name = if output.name.is_empty() {
                output.path.as_str()
            } else {
                output.name.as_str()
            };
            json!({
                "name": name,
                "kind": if output.content_type.is_empty() { "text" } else { output.content_type.as_str() },
                "guest_path": control_workspace_path(workspace_mount, &output.path),
            })
        })
        .collect()
}

fn control_workspace_path(workspace_mount: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{}", workspace_mount.trim_end_matches('/'), path)
    }
}

fn config_from_process_env() -> Option<GatewayRuntimeConfig> {
    let endpoint = gateway_endpoint_from_env()?;
    let boot_token = std::env::var("WM_BOOT").unwrap_or_default();
    let tx = if boot_token.is_empty() {
        std::env::var("WAYMARK_GATEWAY_TX").ok()?
    } else {
        std::env::var("WAYMARK_GATEWAY_TX").unwrap_or_default()
    };
    let attempt = std::env::var("WAYMARK_GATEWAY_ATTEMPT_ID").unwrap_or_default();
    let image = std::env::var("WAYMARK_GATEWAY_IMAGE").unwrap_or_default();
    let container = std::env::var("WAYMARK_GATEWAY_CONTAINER")
        .ok()
        .filter(|value| !value.is_empty());
    let workspace_mount =
        std::env::var("WAYMARK_GATEWAY_WORKSPACE_MOUNT").unwrap_or_else(|_| "/app".to_string());
    let host_workspace_path = std::env::var_os("WAYMARK_GATEWAY_WORKSPACE_PATH")
        .or_else(|| std::env::var_os("WAYMARK_ATTEMPT_WORKSPACE_PATH"))
        .map(PathBuf::from);
    Some(GatewayRuntimeConfig {
        endpoint,
        attempt,
        controller_run: std::env::var("WAYMARK_GATEWAY_CONTROLLER_RUN")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("WAYMARK_ATTEMPT_PROCESS_RUN")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_default(),
        boot_token,
        tx,
        image,
        container,
        workspace_mount,
        host_workspace_path,
        capability_profile: std::env::var("WAYMARK_GATEWAY_MODEL_CAPABILITY_PROFILE")
            .unwrap_or_default(),
        model_class: std::env::var("WAYMARK_GATEWAY_MODEL_CLASS").unwrap_or_default(),
        control: None,
    })
}

fn gateway_endpoint_from_env() -> Option<GatewayEndpoint> {
    let endpoint = if let Some(socket) = std::env::var_os("WAYMARK_GATEWAY_SOCKET") {
        GatewayEndpoint::Unix(PathBuf::from(socket))
    } else if let Ok(port) = std::env::var("WAYMARK_GATEWAY_VSOCK_PORT") {
        let port = port.parse::<u32>().ok()?;
        let cid = std::env::var("WAYMARK_GATEWAY_VSOCK_CID")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(2);
        GatewayEndpoint::Vsock { cid, port }
    } else {
        let raw = std::env::var("WM_GW").ok()?;
        let (cid, port) = raw.split_once(':')?;
        let cid = cid.parse::<u32>().ok()?;
        let port = port.parse::<u32>().ok()?;
        GatewayEndpoint::Vsock { cid, port }
    };
    Some(endpoint)
}

fn required_config() -> Result<GatewayRuntimeConfig, ShellError> {
    config().ok_or_else(|| stone_error("gateway", "Gateway runtime config is not active"))
}

#[cfg(any(unix, target_os = "hermit"))]
pub(crate) enum GatewayClientStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(any(target_os = "hermit", target_os = "linux"))]
    Vsock(waymark_gateway_client::VsockStream),
}

#[cfg(any(unix, target_os = "hermit"))]
static SHARED_GATEWAY_CLIENT: OnceLock<Mutex<Option<GatewayRpcClient<GatewayClientStream>>>> =
    OnceLock::new();

#[cfg(any(unix, target_os = "hermit"))]
fn shared_gateway_client() -> &'static Mutex<Option<GatewayRpcClient<GatewayClientStream>>> {
    SHARED_GATEWAY_CLIENT.get_or_init(|| Mutex::new(None))
}

#[cfg(any(unix, target_os = "hermit"))]
fn clear_shared_gateway_client() {
    if let Ok(mut guard) = shared_gateway_client().lock() {
        guard.take();
    }
}

#[cfg(any(unix, target_os = "hermit"))]
impl std::io::Read for GatewayClientStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            GatewayClientStream::Unix(stream) => stream.read(buf),
            #[cfg(any(target_os = "hermit", target_os = "linux"))]
            GatewayClientStream::Vsock(stream) => stream.read(buf),
        }
    }
}

#[cfg(any(unix, target_os = "hermit"))]
impl std::io::Write for GatewayClientStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            GatewayClientStream::Unix(stream) => stream.write(buf),
            #[cfg(any(target_os = "hermit", target_os = "linux"))]
            GatewayClientStream::Vsock(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            GatewayClientStream::Unix(stream) => stream.flush(),
            #[cfg(any(target_os = "hermit", target_os = "linux"))]
            GatewayClientStream::Vsock(stream) => stream.flush(),
        }
    }
}

#[cfg(any(unix, target_os = "hermit"))]
fn connect_gateway_client(
    config: &GatewayRuntimeConfig,
) -> Result<GatewayRpcClient<GatewayClientStream>, ShellError> {
    let stream = match &config.endpoint {
        #[cfg(unix)]
        GatewayEndpoint::Unix(path) => {
            let stream = std::os::unix::net::UnixStream::connect(path)
                .map_err(|err| stone_error("gateway connect", err.to_string()))?;
            GatewayClientStream::Unix(stream)
        }
        #[cfg(not(unix))]
        GatewayEndpoint::Unix(_) => {
            return Err(stone_error(
                "gateway connect",
                "Gateway Unix transport is not wired in this build",
            ))
        }
        #[cfg(any(target_os = "hermit", target_os = "linux"))]
        GatewayEndpoint::Vsock { cid, port } => {
            let stream = waymark_gateway_client::VsockStream::connect(
                waymark_gateway_client::VsockAddr::new(*cid, *port),
            )
            .map_err(|err| stone_error("gateway connect", err.to_string()))?;
            GatewayClientStream::Vsock(stream)
        }
        #[cfg(not(any(target_os = "hermit", target_os = "linux")))]
        GatewayEndpoint::Vsock { .. } => {
            return Err(stone_error(
                "gateway connect",
                "Gateway vsock transport is not wired in this build",
            ))
        }
    };
    Ok(GatewayRpcClient::from_stream(stream))
}

#[cfg(any(unix, target_os = "hermit"))]
fn connect_attached_gateway_client(
    config: &mut GatewayRuntimeConfig,
) -> Result<GatewayRpcClient<GatewayClientStream>, ShellError> {
    let mut client = connect_gateway_client(config)?;
    attach_gateway_client(&mut client, config)?;
    Ok(client)
}

#[cfg(any(unix, target_os = "hermit"))]
fn attach_gateway_client(
    client: &mut GatewayRpcClient<GatewayClientStream>,
    config: &mut GatewayRuntimeConfig,
) -> Result<(), ShellError> {
    let response = if !config.boot_token.is_empty() {
        Some(
            client
                .attempt_channel_attach_boot(AttemptChannelAttachBootRequest {
                    boot_token: config.boot_token.clone(),
                    channel_epoch: String::new(),
                    metadata: [("source".to_string(), "waymark-runtime".to_string())].into(),
                })
                .map_err(|err| stone_error("gateway attach", err.to_string()))?,
        )
    } else if !config.attempt.is_empty() {
        Some(
            client
                .attempt_channel_attach(AttemptChannelAttachRequest {
                    attempt: config.attempt.clone(),
                    controller_run: config.controller_run.clone(),
                    channel_epoch: String::new(),
                    metadata: [("source".to_string(), "waymark-runtime".to_string())].into(),
                })
                .map_err(|err| stone_error("gateway attach", err.to_string()))?,
        )
    } else {
        None
    };
    if let Some(response) = response {
        apply_attach_response(config, response);
    }
    Ok(())
}

fn apply_attach_response(
    config: &mut GatewayRuntimeConfig,
    response: AttemptChannelAttachResponse,
) {
    if let Some(attempt) = response.attempt {
        if !attempt.attempt.is_empty() {
            config.attempt = attempt.attempt;
        }
    }
    config.boot_token.clear();
    if !response.controller_run.is_empty() {
        config.controller_run = response.controller_run;
    }
    if !response.capability_profile.is_empty() {
        config.capability_profile = response.capability_profile;
    }
    if !response.tx.is_empty() {
        config.tx = response.tx;
    }
    if !response.image.is_empty() {
        config.image = response.image;
    }
    config.container = (!response.container.is_empty()).then_some(response.container);
    if !response.workspace_mount.is_empty() {
        config.workspace_mount = response.workspace_mount;
    }
    if !response.model_class.is_empty() {
        config.model_class = response.model_class;
    }
    config.control = response.control;
}

#[cfg(any(unix, target_os = "hermit"))]
pub(crate) fn with_client<T>(
    config: &GatewayRuntimeConfig,
    call: impl FnOnce(&mut GatewayRpcClient<GatewayClientStream>) -> waymark_gateway_client::Result<T>,
) -> Result<T, ShellError> {
    if let Some(client) = shared_gateway_client()
        .lock()
        .map_err(|_| stone_error("gateway client", "shared Gateway client lock poisoned"))?
        .as_mut()
    {
        return call(client).map_err(|err| stone_error("gateway rpc", err.to_string()));
    }
    let mut config = config.clone();
    let mut client = connect_attached_gateway_client(&mut config)?;
    call(&mut client).map_err(|err| stone_error("gateway rpc", err.to_string()))
}

#[cfg(not(any(unix, target_os = "hermit")))]
pub(crate) struct UnsupportedGatewayStream;

#[cfg(not(any(unix, target_os = "hermit")))]
impl std::io::Read for UnsupportedGatewayStream {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Gateway transport is not wired in this build",
        ))
    }
}

#[cfg(not(any(unix, target_os = "hermit")))]
impl std::io::Write for UnsupportedGatewayStream {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Gateway transport is not wired in this build",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(not(any(unix, target_os = "hermit")))]
pub(crate) fn with_client<T>(
    _config: &GatewayRuntimeConfig,
    _call: impl FnOnce(
        &mut GatewayRpcClient<UnsupportedGatewayStream>,
    ) -> waymark_gateway_client::Result<T>,
) -> Result<T, ShellError> {
    Err(stone_error(
        "gateway connect",
        "Gateway transport is not wired in this build",
    ))
}

fn workspace_path(config: &GatewayRuntimeConfig, path: &Path) -> Result<String, ShellError> {
    let mount = Path::new(&config.workspace_mount);
    let rel = if path.is_absolute() {
        if let Some(host_workspace_path) = &config.host_workspace_path {
            if let Ok(rel) = path.strip_prefix(host_workspace_path) {
                return Ok(rel.to_string_lossy().into_owned());
            }
        }
        path.strip_prefix(mount).map_err(|_| {
            stone_error(
                "gateway path",
                format!(
                    "{} is outside Gateway workspace mount {}",
                    path.display(),
                    mount.display()
                ),
            )
        })?
    } else {
        path
    };
    Ok(rel.to_string_lossy().into_owned())
}

fn app_path(config: &GatewayRuntimeConfig, rel: &str) -> String {
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        config.workspace_mount.clone()
    } else {
        Path::new(&config.workspace_mount)
            .join(rel)
            .to_string_lossy()
            .into_owned()
    }
}

fn container_path(config: &GatewayRuntimeConfig, path: &Path) -> Result<String, ShellError> {
    if path.is_absolute() {
        if path.starts_with(&config.workspace_mount) {
            return Ok(path.to_string_lossy().into_owned());
        }
        if let Some(host_workspace_path) = &config.host_workspace_path {
            if let Ok(rel) = path.strip_prefix(host_workspace_path) {
                return Ok(Path::new(&config.workspace_mount)
                    .join(rel)
                    .to_string_lossy()
                    .into_owned());
            }
        }
        return Err(stone_error(
            "gateway path",
            format!(
                "{} is outside Gateway workspace mount {}",
                path.display(),
                config.workspace_mount
            ),
        ));
    }
    Ok(Path::new(&config.workspace_mount)
        .join(path)
        .to_string_lossy()
        .into_owned())
}

fn entry_record(entry: WorkspaceEntry, guest_path: PathBuf, span: Span) -> Record {
    let mut record = Record::with_capacity(10);
    let kind = entry_kind(entry.kind);
    record.push(
        "path",
        Value::string(guest_path.display().to_string(), span),
    );
    record.push("type", Value::string(kind, span));
    record.push("is_file", Value::bool(kind == "file", span));
    record.push("is_dir", Value::bool(kind == "dir", span));
    record.push("is_symlink", Value::bool(kind == "symlink", span));
    record.push("readonly", Value::bool(false, span));
    record.push(
        "size",
        Value::int(i64::try_from(entry.size).unwrap_or(i64::MAX), span),
    );
    record.push("modified_ms", Value::nothing(span));
    record.push("accessed_ms", Value::nothing(span));
    record.push("created_ms", Value::nothing(span));
    record
}

fn entry_kind(kind: i32) -> &'static str {
    match WorkspaceEntryKind::try_from(kind).unwrap_or(WorkspaceEntryKind::Unspecified) {
        WorkspaceEntryKind::File => "file",
        WorkspaceEntryKind::Directory => "dir",
        WorkspaceEntryKind::Symlink => "symlink",
        WorkspaceEntryKind::Other => "other",
        WorkspaceEntryKind::Unspecified => "unknown",
    }
}

struct TruncatedText {
    text: String,
    truncated: bool,
}

fn truncate_text(text: String, max_bytes: usize) -> TruncatedText {
    let mut bytes = text.into_bytes();
    let truncated = bytes.len() > max_bytes;
    if truncated {
        bytes.truncate(max_bytes);
        while std::str::from_utf8(&bytes).is_err() && !bytes.is_empty() {
            bytes.pop();
        }
    }
    TruncatedText {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    }
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
    use waymark_gateway_client::proto::{AttemptProgram, BuiltinWorkflow, TaskOutput};

    #[test]
    fn gateway_model_request_maps_agent_json_to_protobuf() {
        let config = GatewayRuntimeConfig {
            endpoint: GatewayEndpoint::Unix(PathBuf::from("/tmp/gateway.sock")),
            attempt: "attempt-1".to_string(),
            controller_run: "process-1".to_string(),
            boot_token: String::new(),
            tx: "tx-1".to_string(),
            image: "python:3.12".to_string(),
            container: None,
            workspace_mount: "/app".to_string(),
            host_workspace_path: None,
            capability_profile: "local".to_string(),
            model_class: "agent".to_string(),
            control: None,
        };
        let request = model_call_request_from_json(
            &config,
            &json!({
                "model": "served-model",
                "model_class": "reasoning",
                "messages": [
                    {"role": "system", "content": "You are concise."},
                    {"role": "user", "content": "hello"}
                ],
                "temperature": 0,
                "top_p": 1,
                "max_tokens": 64,
                "metadata": {
                    "request_id": "req-1",
                    "ignored_non_string": 7
                }
            }),
        )
        .unwrap();

        assert_eq!(request.attempt, "attempt-1");
        assert_eq!(request.model_hint, "served-model");
        assert_eq!(request.model_class, "reasoning");
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, "hello");
        assert_eq!(request.sampling.as_ref().unwrap().temperature, 0.0);
        assert_eq!(request.sampling.as_ref().unwrap().top_p, 1.0);
        assert_eq!(request.max_output_tokens, 64);
        assert_eq!(
            request.metadata.get("request_id"),
            Some(&"req-1".to_string())
        );
        assert_eq!(
            request.metadata.get("source"),
            Some(&"waymark-runtime".to_string())
        );
        assert!(!request.metadata.contains_key("ignored_non_string"));
    }

    #[test]
    fn attempt_report_result_maps_task_response_to_protobuf() {
        let config = GatewayRuntimeConfig {
            endpoint: GatewayEndpoint::Unix(PathBuf::from("/tmp/gateway.sock")),
            attempt: "attempt-1".to_string(),
            controller_run: "process-1".to_string(),
            boot_token: String::new(),
            tx: "tx-1".to_string(),
            image: "python:3.12".to_string(),
            container: None,
            workspace_mount: "/app".to_string(),
            host_workspace_path: None,
            capability_profile: "local".to_string(),
            model_class: "agent".to_string(),
            control: None,
        };

        let request = attempt_report_result_request(
            &config,
            &json!({
                "id": "task-1",
                "ok": true,
                "kind": "task_result",
                "value": {
                    "answer": "hello"
                }
            }),
        )
        .unwrap();
        let result: JsonValue = serde_json::from_str(&request.result_json).unwrap();

        assert_eq!(request.attempt, "attempt-1");
        assert_eq!(request.status, "succeeded");
        assert_eq!(request.error_json, "");
        assert_eq!(request.reason, "task_result");
        assert_eq!(
            request.metadata.get("source"),
            Some(&"waymark-runtime".to_string())
        );
        assert_eq!(
            request.metadata.get("kind"),
            Some(&"task_result".to_string())
        );
        assert_eq!(result["value"]["answer"], json!("hello"));

        let request = attempt_report_result_request(
            &config,
            &json!({
                "id": "task-1",
                "ok": false,
                "kind": "runner_error",
                "error": {
                    "code": "bad_task",
                    "message": "bad task"
                }
            }),
        )
        .unwrap();
        let error: JsonValue = serde_json::from_str(&request.error_json).unwrap();

        assert_eq!(request.status, "failed");
        assert_eq!(request.reason, "bad task");
        assert_eq!(error["code"], json!("bad_task"));
    }

    #[test]
    fn control_block_maps_to_agent_task_json() {
        let mut config = GatewayRuntimeConfig {
            endpoint: GatewayEndpoint::Unix(PathBuf::from("/tmp/gateway.sock")),
            attempt: "attempt-1".to_string(),
            controller_run: "process-1".to_string(),
            boot_token: String::new(),
            tx: "tx-1".to_string(),
            image: "python:3.12".to_string(),
            container: None,
            workspace_mount: "/app".to_string(),
            host_workspace_path: None,
            capability_profile: "local".to_string(),
            model_class: "agent".to_string(),
            control: None,
        };
        config.control = Some(AttemptControlBlock {
            task_spec: Some(GatewayTaskSpec {
                id: "task-from-gateway".to_string(),
                objective: "write hello.txt".to_string(),
                outputs: vec![TaskOutput {
                    name: "hello".to_string(),
                    path: "hello.txt".to_string(),
                    content_type: "text".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            program: Some(AttemptProgram {
                program: Some(attempt_program::Program::Builtin(BuiltinWorkflow {
                    name: "agent".to_string(),
                    args_json: r#"{"model":"fixture","max_turns":4,"max_rounds":2}"#.to_string(),
                })),
            }),
            ..Default::default()
        });

        let task = control_task_value(&config).unwrap();
        assert_eq!(task["id"], json!("task-from-gateway"));
        assert_eq!(task["runtime"]["frontend"], json!("agent"));
        assert_eq!(task["agent"]["model"], json!("fixture"));
        assert_eq!(
            task["agent"]["task"],
            json!("write hello.txt\nOutput: hello kind= path=hello.txt mode= content_type=text")
        );
        assert_eq!(task["agent"]["max_turns"], json!(4));
        assert_eq!(task["agent"]["max_rounds"], json!(2));
        assert_eq!(task["artifacts"][0]["guest_path"], json!("/app/hello.txt"));
    }
}
