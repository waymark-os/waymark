// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};
use waymark_gateway_client::proto::{WorkspaceEntry, WorkspaceEntryKind};
use waymark_gateway_client::{GatewayRpcClient, LinuxExecOptions};

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
    _timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<Record, ShellError> {
    if stdin.is_some() {
        return Err(stone_error(
            "gateway run",
            "Gateway linux.exec does not support stdin yet",
        ));
    }
    let config = required_config()?;
    let workdir = container_path(&config, cwd)?;
    let mut options = LinuxExecOptions::new(config.tx.clone(), config.image.clone(), argv.to_vec())
        .workspace_mount(config.workspace_mount.clone())
        .workdir(workdir);
    options.container = config.container.clone();
    for (key, value) in env_overrides {
        options = options.env(key.clone(), value.clone());
    }
    let output = with_client(&config, |client| client.linux_exec(options))?;
    let span = Span::unknown();
    let stdout = truncate_text(output.stdout, max_stdout_bytes);
    let stderr = truncate_text(output.stderr, max_stderr_bytes);
    let mut record = Record::new();
    record.push("ok", Value::bool(output.status == 0, span));
    record.push(
        "kind",
        Value::string(
            if output.status == 0 {
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
    record.push("stdout", Value::string(stdout.text, span));
    record.push("stderr", Value::string(stderr.text, span));
    record.push("timed_out", Value::bool(false, span));
    let mut truncated = Record::new();
    truncated.push("stdout", Value::bool(stdout.truncated, span));
    truncated.push("stderr", Value::bool(stderr.truncated, span));
    record.push("truncated", Value::record(truncated, span));
    if let Some(diff) = output.diff {
        record.push("env_diff", Value::string(diff.text, span));
    }
    Ok(record)
}

fn config_from_process_env() -> Option<GatewayRuntimeConfig> {
    let socket = std::env::var_os("WAYMARK_GATEWAY_SOCKET")?;
    let tx = std::env::var("WAYMARK_GATEWAY_TX").ok()?;
    let image = std::env::var("WAYMARK_GATEWAY_IMAGE").unwrap_or_default();
    let container = std::env::var("WAYMARK_GATEWAY_CONTAINER")
        .ok()
        .filter(|value| !value.is_empty());
    if image.is_empty() && container.is_none() {
        return None;
    }
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
