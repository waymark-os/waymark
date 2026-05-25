// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};
use serde_json::Value as JsonValue;
use waymark_gateway_client::proto::{WorkspaceEntry, WorkspaceEntryKind};
use waymark_gateway_client::{GatewayRpcClient, LinuxExecOptions, LinuxProbeOptions};

use crate::json::json_to_nu_value;

const DEFAULT_RUN_SYNC_BUDGET_MS: u64 = 90_000;

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum GatewayEndpoint {
    Unix(PathBuf),
    Vsock { cid: u32, port: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GatewayRuntimeConfig {
    pub(crate) endpoint: GatewayEndpoint,
    pub(crate) tx: String,
    pub(crate) image: String,
    pub(crate) container: Option<String>,
    pub(crate) workspace_mount: String,
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
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<Record, ShellError> {
    let config = required_config()?;
    let workdir = container_path(&config, cwd)?;
    let mut options = LinuxExecOptions::new(config.tx.clone(), config.image.clone(), argv.to_vec())
        .workspace_mount(config.workspace_mount.clone())
        .workdir(workdir)
        .timeout_ms(run_sync_timeout_ms(timeout));
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
    linux_exec_record(output, Span::unknown(), usize::MAX, usize::MAX)
}

pub(crate) fn run_status(run_id: &str) -> Result<Record, ShellError> {
    let config = required_config()?;
    let output = with_client(&config, |client| client.linux_exec_wait(run_id, 1))?;
    linux_exec_record(output, Span::unknown(), usize::MAX, usize::MAX)
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
                let mut record = Record::with_capacity(10);
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

fn linux_exec_record(
    output: waymark_gateway_client::proto::LinuxExecResponse,
    span: Span,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<Record, ShellError> {
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
    record.push("timed_out", Value::bool(output.timed_out, span));
    record.push("still_running", Value::bool(output.still_running, span));
    record.push("done", Value::bool(!output.still_running, span));
    record.push(
        "next_action",
        Value::string(
            if output.still_running {
                "run_wait_or_run_terminate"
            } else if output.status == 0 {
                "inspect_result_or_commit"
            } else {
                "inspect_error_or_retry"
            },
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

fn probe_options(config: &GatewayRuntimeConfig) -> LinuxProbeOptions {
    let options = LinuxProbeOptions::new(config.tx.clone(), config.image.clone())
        .workspace_mount(config.workspace_mount.clone());
    if let Some(container) = &config.container {
        options.container(container.clone())
    } else {
        options
    }
}

fn config_from_process_env() -> Option<GatewayRuntimeConfig> {
    let socket = std::env::var_os("WAYMARK_GATEWAY_SOCKET")?;
    let tx = std::env::var("WAYMARK_GATEWAY_TX").ok()?;
    let image = std::env::var("WAYMARK_GATEWAY_IMAGE").unwrap_or_default();
    let container = std::env::var("WAYMARK_GATEWAY_CONTAINER")
        .ok()
        .filter(|value| !value.is_empty());
    let workspace_mount =
        std::env::var("WAYMARK_GATEWAY_WORKSPACE_MOUNT").unwrap_or_else(|_| "/app".to_string());
    Some(GatewayRuntimeConfig {
        endpoint: GatewayEndpoint::Unix(PathBuf::from(socket)),
        tx,
        image,
        container,
        workspace_mount,
    })
}

fn required_config() -> Result<GatewayRuntimeConfig, ShellError> {
    config().ok_or_else(|| stone_error("gateway", "Gateway runtime config is not active"))
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

fn workspace_path(config: &GatewayRuntimeConfig, path: &Path) -> Result<String, ShellError> {
    let mount = Path::new(&config.workspace_mount);
    let rel = if path.is_absolute() {
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

fn container_path(config: &GatewayRuntimeConfig, path: &Path) -> Result<String, ShellError> {
    if path.is_absolute() {
        if path.starts_with(&config.workspace_mount) {
            return Ok(path.to_string_lossy().into_owned());
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
