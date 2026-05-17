// SPDX-License-Identifier: MIT OR Apache-2.0

#![cfg(not(target_os = "hermit"))]

use std::env;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Seek, Write};
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Once;
use std::thread;
use std::time::{Duration, Instant};

use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};
use serde_json::Value as JsonValue;

use crate::gateway_runtime;
use crate::stone_helpers::attach_service_helper_observation;

#[cfg(unix)]
extern "C" {
    fn setsid() -> i32;
}

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}

fn io_stone_error(kind: &str, err: std::io::Error, path: &Path) -> ShellError {
    let path = path.to_path_buf();
    ShellError::Io(
        nu_protocol::shell_error::io::IoError::new_internal_with_path(
            err,
            format!("Stone {kind} I/O error at {}", path.display()),
            path,
        ),
    )
}

fn value_to_i64(value: &Value, context: &str) -> Result<i64, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(*val),
        Value::Float { val, .. } => Ok(*val as i64),
        Value::String { val, .. } | Value::Glob { val, .. } => val
            .trim()
            .parse::<i64>()
            .map_err(|err| stone_error(context, format!("failed to parse integer: {err}"))),
        other => Err(stone_error(
            context,
            format!("expected integer, got {}", other.get_type()),
        )),
    }
}

fn value_to_string(value: &Value, context: &str) -> Result<String, ShellError> {
    match value {
        Value::String { val, .. } | Value::Glob { val, .. } => Ok(val.clone()),
        other => Err(stone_error(
            context,
            format!("expected string, got {}", other.get_type()),
        )),
    }
}

fn value_to_limit(value: &Value, context: &str) -> Result<usize, ShellError> {
    let limit = value_to_i64(value, context)?;
    if limit < 0 {
        return Err(stone_error(context, "limit must be non-negative"));
    }
    usize::try_from(limit).map_err(|_| stone_error(context, "limit is too large"))
}

fn value_to_string_list(value: &Value, context: &str) -> Result<Vec<String>, ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            context,
            format!(
                "expected list of strings, got {}; use {context}([\"cmd\", \"arg\"]) instead of {context}(\"cmd\")",
                value.get_type()
            ),
        ));
    };
    vals.iter()
        .map(|value| value_to_string(value, context))
        .collect()
}

fn value_to_string_pairs(
    value: &Value,
    context: &str,
) -> Result<Vec<(String, String)>, ShellError> {
    let Value::Record { val, .. } = value else {
        return Err(stone_error(
            context,
            format!("expected record of string values, got {}", value.get_type()),
        ));
    };
    val.iter()
        .map(|(key, value)| value_to_string(value, context).map(|value| (key.clone(), value)))
        .collect()
}

pub(crate) struct StoneRunInvocation {
    pub(crate) record: Record,
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env_overrides: Vec<(String, String)>,
}

pub(crate) fn run_call_values(
    positional: &[Value],
    named: &[(String, Value)],
    default_cwd: PathBuf,
    mut resolve_path: impl FnMut(&str) -> Result<PathBuf, ShellError>,
) -> Result<StoneRunInvocation, ShellError> {
    if positional.is_empty() || positional.len() > 4 {
        return Err(stone_error(
            "run",
            "run() expects run(argv, cwd? = None, stdin? = None, timeout_ms? = None)",
        ));
    }
    let argv = value_to_string_list(&positional[0], "run")?;
    if argv.is_empty() {
        return Err(stone_error("run", "run() argv list cannot be empty"));
    }

    let mut cwd: Option<String> = None;
    let mut stdin: Option<String> = None;
    let mut env_overrides: Vec<(String, String)> = Vec::new();
    let mut timeout_ms: i64 = 300_000;
    let mut max_stdout_bytes: usize = 1_048_576;
    let mut max_stderr_bytes: usize = 1_048_576;
    let mut stdout_target = RunOutputTarget::Capture;
    let mut stderr_target = RunOutputTarget::Capture;

    for (index, value) in positional.iter().enumerate().skip(1) {
        match index {
            1 => match value {
                Value::Int { .. } | Value::Float { .. } => {
                    timeout_ms = value_to_i64(value, "run timeout_ms")?;
                    if timeout_ms <= 0 {
                        return Err(stone_error("run", "timeout_ms must be positive"));
                    }
                }
                _ => cwd = Some(value_to_string(value, "run cwd")?),
            },
            2 => match value {
                Value::Int { .. } | Value::Float { .. } => {
                    timeout_ms = value_to_i64(value, "run timeout_ms")?;
                    if timeout_ms <= 0 {
                        return Err(stone_error("run", "timeout_ms must be positive"));
                    }
                }
                _ => stdin = Some(value_to_string(value, "run stdin")?),
            },
            3 => {
                timeout_ms = value_to_i64(value, "run timeout_ms")?;
                if timeout_ms <= 0 {
                    return Err(stone_error("run", "timeout_ms must be positive"));
                }
            }
            _ => unreachable!("run positional arity checked above"),
        }
    }

    for (name, value) in named {
        match name.as_str() {
            "cwd" => cwd = Some(value_to_string(value, "run cwd")?),
            "stdin" => stdin = Some(value_to_string(value, "run stdin")?),
            "env" => env_overrides = value_to_string_pairs(value, "run env")?,
            "timeout_ms" => {
                timeout_ms = value_to_i64(value, "run timeout_ms")?;
                if timeout_ms <= 0 {
                    return Err(stone_error("run", "timeout_ms must be positive"));
                }
            }
            "max_stdout_bytes" => {
                max_stdout_bytes = value_to_limit(value, "run max_stdout_bytes")?;
            }
            "max_stderr_bytes" => {
                max_stderr_bytes = value_to_limit(value, "run max_stderr_bytes")?;
            }
            "stdout" => {
                stdout_target = value_to_run_stdout_target(value, "run stdout")?;
            }
            "stderr" => {
                stderr_target = value_to_run_stderr_target(value, "run stderr")?;
            }
            other => {
                return Err(stone_error(
                    "run",
                    format!(
                        "unsupported keyword `{other}`; expected cwd, env, stdin, timeout_ms, max_stdout_bytes, max_stderr_bytes, stdout, or stderr"
                    ),
                ));
            }
        }
    }

    let cwd = match cwd {
        Some(path) => resolve_path(&path)?,
        None => default_cwd,
    };
    let record = run_posix_command(
        &argv,
        &cwd,
        &env_overrides,
        stdin.as_deref(),
        Duration::from_millis(timeout_ms as u64),
        stdout_target,
        stderr_target,
        max_stdout_bytes,
        max_stderr_bytes,
    )?;
    Ok(StoneRunInvocation {
        record,
        argv,
        cwd,
        env_overrides,
    })
}

pub(crate) fn resolve_command_call_values(
    positional: &[Value],
    named: &[(String, Value)],
) -> Result<Value, ShellError> {
    let [name] = positional else {
        return Err(stone_error(
            "resolve_command",
            "resolve_command() requires exactly one command name",
        ));
    };
    if !named.is_empty() {
        return Err(stone_error(
            "resolve_command",
            "resolve_command() keyword arguments are not supported",
        ));
    }
    Ok(resolve_command_record(&value_to_string(
        name,
        "resolve_command",
    )?))
}

pub(crate) fn start_daemon_call_values(
    positional: &[Value],
    named: &[(String, Value)],
    default_cwd: PathBuf,
    mut resolve_path: impl FnMut(&str) -> Result<PathBuf, ShellError>,
) -> Result<Value, ShellError> {
    let [argv_value] = positional else {
        return Err(stone_error(
            "start_daemon",
            "start_daemon() requires exactly one argv list",
        ));
    };
    let argv = value_to_string_list(argv_value, "start_daemon")?;
    if argv.is_empty() {
        return Err(stone_error("start_daemon", "argv list cannot be empty"));
    }

    let mut cwd: Option<String> = None;
    let mut env_overrides: Vec<(String, String)> = Vec::new();
    let mut stdout: Option<String> = None;
    let mut stderr: Option<String> = None;

    for (name, value) in named {
        match name.as_str() {
            "cwd" => cwd = Some(value_to_string(value, "start_daemon cwd")?),
            "env" => env_overrides = value_to_string_pairs(value, "start_daemon env")?,
            "stdout" => stdout = Some(value_to_string(value, "start_daemon stdout")?),
            "stderr" => stderr = Some(value_to_string(value, "start_daemon stderr")?),
            other => {
                return Err(stone_error(
                    "start_daemon",
                    format!("unsupported keyword `{other}`; expected cwd, env, stdout, or stderr"),
                ));
            }
        }
    }

    let cwd_path = match cwd {
        Some(path) => resolve_path(&path)?,
        None => default_cwd,
    };
    let stdout_path = match stdout {
        Some(path) => resolve_path(&path)?,
        None => daemon_temp_path("stdout"),
    };
    let stderr_path = match stderr {
        Some(path) => resolve_path(&path)?,
        None => daemon_temp_path("stderr"),
    };
    start_posix_daemon(&argv, &cwd_path, &env_overrides, &stdout_path, &stderr_path)
}

pub(crate) fn daemon_status_call_values(
    positional: &[Value],
    named: &[(String, Value)],
    mut resolve_path: impl FnMut(&str) -> Result<PathBuf, ShellError>,
) -> Result<Value, ShellError> {
    let [daemon] = positional else {
        return Err(stone_error(
            "daemon_status",
            "daemon_status() requires exactly one daemon handle or pid",
        ));
    };
    let pid = value_to_daemon_pid(daemon, "daemon_status")?;
    let mut host = "127.0.0.1".to_owned();
    let mut port: Option<u16> = None;
    let mut log: Option<String> = None;
    let mut max_log_bytes: usize = 4000;

    for (name, value) in named {
        match name.as_str() {
            "host" => host = value_to_string(value, "daemon_status host")?,
            "port" => port = Some(value_to_port(value, "daemon_status port")?),
            "log" => log = Some(value_to_string(value, "daemon_status log")?),
            "max_log_bytes" => {
                max_log_bytes = value_to_limit(value, "daemon_status max_log_bytes")?
            }
            other => {
                return Err(stone_error(
                    "daemon_status",
                    format!(
                        "unsupported keyword `{other}`; expected host, port, log, or max_log_bytes"
                    ),
                ));
            }
        }
    }

    let log_path = match log {
        Some(path) => Some(resolve_path(&path)?),
        None => daemon_log_path(daemon),
    };
    Ok(daemon_status_record(
        pid,
        port,
        &host,
        log_path.as_deref(),
        max_log_bytes,
    ))
}

pub(crate) fn stop_daemon_call_values(
    positional: &[Value],
    named: &[(String, Value)],
) -> Result<Value, ShellError> {
    let [daemon] = positional else {
        return Err(stone_error(
            "stop_daemon",
            "stop_daemon() requires exactly one daemon handle or pid",
        ));
    };
    let pid = value_to_daemon_pid(daemon, "stop_daemon")?;
    let mut timeout_ms: i64 = 5000;
    for (name, value) in named {
        match name.as_str() {
            "timeout_ms" => {
                timeout_ms = value_to_i64(value, "stop_daemon timeout_ms")?;
                if timeout_ms <= 0 {
                    return Err(stone_error("stop_daemon", "timeout_ms must be positive"));
                }
            }
            other => {
                return Err(stone_error(
                    "stop_daemon",
                    format!("unsupported keyword `{other}`; expected timeout_ms"),
                ));
            }
        }
    }
    Ok(stop_daemon_record(
        pid,
        Duration::from_millis(timeout_ms as u64),
    ))
}

pub(crate) fn wait_port_call_values(
    positional: &[Value],
    named: &[(String, Value)],
) -> Result<Value, ShellError> {
    let [port_value] = positional else {
        return Err(stone_error(
            "wait_port",
            "wait_port() requires exactly one port",
        ));
    };
    let port = value_to_port(port_value, "wait_port port")?;
    let mut host = "127.0.0.1".to_owned();
    let mut timeout_ms: i64 = 30_000;
    for (name, value) in named {
        match name.as_str() {
            "host" => host = value_to_string(value, "wait_port host")?,
            "timeout_ms" => {
                timeout_ms = value_to_i64(value, "wait_port timeout_ms")?;
                if timeout_ms <= 0 {
                    return Err(stone_error("wait_port", "timeout_ms must be positive"));
                }
            }
            other => {
                return Err(stone_error(
                    "wait_port",
                    format!("unsupported keyword `{other}`; expected host or timeout_ms"),
                ));
            }
        }
    }
    Ok(wait_port_record(
        &host,
        port,
        Duration::from_millis(timeout_ms as u64),
    ))
}

#[cfg(not(target_os = "hermit"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunOutputTarget {
    Capture,
    Suppress,
    Stdout,
}

#[cfg(not(target_os = "hermit"))]
fn value_to_run_stdout_target(value: &Value, context: &str) -> Result<RunOutputTarget, ShellError> {
    let target = value_to_string(value, context)?;
    match target.as_str() {
        "capture" | "pipe" => Ok(RunOutputTarget::Capture),
        "suppress" | "discard" | "null" | "none" | "devnull" => Ok(RunOutputTarget::Suppress),
        "stdout" => Err(stone_error(
            context,
            "stdout cannot be redirected to itself; use stdout=\"capture\" or stdout=\"suppress\"",
        )),
        other => Err(stone_error(
            context,
            format!(
                "unsupported stdout target `{other}`; expected capture, suppress, discard, null, none, or devnull"
            ),
        )),
    }
}

#[cfg(not(target_os = "hermit"))]
fn value_to_run_stderr_target(value: &Value, context: &str) -> Result<RunOutputTarget, ShellError> {
    let target = value_to_string(value, context)?;
    match target.as_str() {
        "capture" | "pipe" => Ok(RunOutputTarget::Capture),
        "suppress" | "discard" | "null" | "none" | "devnull" => Ok(RunOutputTarget::Suppress),
        "stdout" => Ok(RunOutputTarget::Stdout),
        other => Err(stone_error(
            context,
            format!(
                "unsupported stderr target `{other}`; expected capture, stdout, suppress, discard, null, none, or devnull"
            ),
        )),
    }
}

#[cfg(not(target_os = "hermit"))]
fn value_to_port(value: &Value, context: &str) -> Result<u16, ShellError> {
    let port = value_to_i64(value, context)?;
    if !(1..=65_535).contains(&port) {
        return Err(stone_error(context, "port must be between 1 and 65535"));
    }
    Ok(port as u16)
}

#[cfg(not(target_os = "hermit"))]
fn value_to_daemon_pid(value: &Value, context: &str) -> Result<u32, ShellError> {
    let raw_pid = match value {
        Value::Record { val, .. } => {
            let Some(pid) = val.get("pid") else {
                return Err(stone_error(
                    context,
                    "daemon record is missing `pid`; pass the start_daemon() result or a pid",
                ));
            };
            value_to_i64(pid, context)?
        }
        _ => value_to_i64(value, context)?,
    };
    if raw_pid <= 0 {
        return Err(stone_error(context, "pid must be positive"));
    }
    u32::try_from(raw_pid).map_err(|_| stone_error(context, "pid is too large"))
}

#[cfg(not(target_os = "hermit"))]
fn daemon_log_path(value: &Value) -> Option<PathBuf> {
    let Value::Record { val, .. } = value else {
        return None;
    };
    val.get("stderr_path")
        .or_else(|| val.get("stdout_path"))
        .and_then(|value| value_to_string(value, "daemon log path").ok())
        .map(PathBuf::from)
}

#[cfg(not(target_os = "hermit"))]
fn daemon_temp_path(suffix: &str) -> PathBuf {
    static DAEMON_ID: AtomicU64 = AtomicU64::new(0);
    let temp_prefix = format!(
        "stone-daemon-{}-{}",
        std::process::id(),
        DAEMON_ID.fetch_add(1, AtomicOrdering::Relaxed)
    );
    env::temp_dir().join(format!("{temp_prefix}.{suffix}"))
}

#[cfg(not(target_os = "hermit"))]
fn resolve_command_record(name: &str) -> Value {
    let span = Span::unknown();
    let resolution = resolve_command(name);
    let ok = !resolution.matches.is_empty();
    let mut record = Record::new();
    record.push("ok", Value::bool(ok, span));
    record.push("name", Value::string(name.to_owned(), span));
    record.push(
        "path",
        resolution
            .matches
            .first()
            .map(|path| Value::string(path.display().to_string(), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "matches",
        Value::list(
            resolution
                .matches
                .iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "searched",
        Value::list(
            resolution
                .searched
                .iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "explanation",
        command_resolution_explanation(name, &resolution, span),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
pub(crate) struct CommandResolution {
    pub(crate) matches: Vec<PathBuf>,
    pub(crate) searched: Vec<PathBuf>,
}

#[cfg(not(target_os = "hermit"))]
pub(crate) fn resolve_command(name: &str) -> CommandResolution {
    if name.contains('/') {
        let path = PathBuf::from(name);
        return CommandResolution {
            matches: if is_executable_file(&path) {
                vec![path.clone()]
            } else {
                Vec::new()
            },
            searched: path.parent().map(Path::to_path_buf).into_iter().collect(),
        };
    }

    let searched: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    let mut matches = Vec::new();
    for dir in &searched {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            matches.push(candidate);
        }
    }
    CommandResolution { matches, searched }
}

#[cfg(not(target_os = "hermit"))]
fn resolve_command_with_env(name: &str, env_overrides: &[(String, String)]) -> CommandResolution {
    if name.contains('/') {
        return resolve_command(name);
    }

    let path_override = env_overrides
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.as_str());
    let Some(path) = path_override else {
        return resolve_command(name);
    };

    let searched: Vec<PathBuf> = env::split_paths(path).collect();
    let mut matches = Vec::new();
    for dir in &searched {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            matches.push(candidate);
        }
    }
    CommandResolution { matches, searched }
}

#[cfg(all(not(target_os = "hermit"), unix))]
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(all(not(target_os = "hermit"), not(unix)))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(not(target_os = "hermit"))]
fn maybe_python_runtime_context(
    argv: &[String],
    cwd: &Path,
    env_overrides: &[(String, String)],
) -> Option<Value> {
    let command = argv.first()?;
    let command_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command.as_str());
    let module_name =
        if command_name.starts_with("python") && argv.get(1).map(String::as_str) == Some("-m") {
            argv.get(2).map(String::as_str)
        } else {
            None
        };
    let is_python_adjacent = command_name.starts_with("python")
        || matches!(command_name, "pip" | "pip3" | "uv" | "pytest")
        || matches!(module_name, Some("pip" | "pytest" | "build" | "twine"));
    if !is_python_adjacent {
        return None;
    }

    let span = Span::unknown();
    let resolution = resolve_command_with_env(command, env_overrides);
    let env_virtual_env = env_value_with_overrides("VIRTUAL_ENV", env_overrides);
    let env_pythonpath = env_value_with_overrides("PYTHONPATH", env_overrides);
    let uv_project_environment = env_value_with_overrides("UV_PROJECT_ENVIRONMENT", env_overrides);

    let mut record = Record::new();
    record.push("kind", Value::string("python", span));
    record.push("command_name", Value::string(command_name.to_owned(), span));
    record.push(
        "resolved_executable",
        resolution
            .matches
            .first()
            .map(|path| Value::string(path.display().to_string(), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "matches",
        Value::list(
            resolution
                .matches
                .iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "env_virtual_env",
        env_virtual_env
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "env_pythonpath",
        env_pythonpath
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "uv_project_environment",
        uv_project_environment
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "cwd_project_markers",
        Value::list(
            python_project_markers(cwd)
                .into_iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );

    if command_name.starts_with("python") {
        match python_self_probe(command, cwd, env_overrides) {
            Ok(probe) => {
                record.push(
                    "python_executable",
                    string_or_null(probe.get("executable"), span),
                );
                record.push("python_version", string_or_null(probe.get("version"), span));
                record.push("sys_prefix", string_or_null(probe.get("prefix"), span));
                record.push(
                    "sys_base_prefix",
                    string_or_null(probe.get("base_prefix"), span),
                );
                record.push(
                    "pip_available",
                    Value::bool(
                        probe
                            .get("pip_available")
                            .and_then(JsonValue::as_bool)
                            .unwrap_or(false),
                        span,
                    ),
                );
            }
            Err(err) => record.push("python_probe_error", Value::string(err, span)),
        }
    }

    if command_name == "uv" {
        record.push(
            "note",
            Value::string(
                "uv may select a project environment when cwd contains Python project markers; compare env_virtual_env with the interpreter used by uv run.",
                span,
            ),
        );
    }

    Some(Value::record(record, span))
}

#[cfg(not(target_os = "hermit"))]
fn env_value_with_overrides(name: &str, env_overrides: &[(String, String)]) -> Option<String> {
    env_overrides
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .or_else(|| env::var(name).ok())
}

#[cfg(not(target_os = "hermit"))]
fn python_project_markers(cwd: &Path) -> Vec<PathBuf> {
    let marker_names = ["pyproject.toml", "setup.py", "setup.cfg", ".venv"];
    let mut markers = Vec::new();
    let mut dir = cwd;
    for _ in 0..=3 {
        for name in marker_names {
            let path = dir.join(name);
            if path.exists() {
                markers.push(path);
            }
        }
        let Some(parent) = dir.parent() else {
            break;
        };
        dir = parent;
    }
    markers
}

#[cfg(not(target_os = "hermit"))]
fn python_self_probe(
    python: &str,
    cwd: &Path,
    env_overrides: &[(String, String)],
) -> Result<JsonValue, String> {
    let source = r#"import importlib.util, json, sys
print(json.dumps({
    "executable": sys.executable,
    "version": sys.version.split()[0],
    "prefix": sys.prefix,
    "base_prefix": sys.base_prefix,
    "pip_available": importlib.util.find_spec("pip") is not None,
}))
"#;
    let mut child = Command::new(python)
        .arg("-c")
        .arg(source)
        .current_dir(cwd)
        .envs(env_overrides.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to probe Python runtime: {err}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= Duration::from_secs(2) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Python runtime probe timed out after 2 seconds".to_owned());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(format!("failed to wait for Python runtime probe: {err}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("failed to read Python runtime probe output: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Python runtime probe exited with code {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("failed to parse Python runtime probe output: {err}"))
}

#[cfg(not(target_os = "hermit"))]
fn string_or_null(value: Option<&JsonValue>, span: Span) -> Value {
    value
        .and_then(JsonValue::as_str)
        .map(|text| Value::string(text.to_owned(), span))
        .unwrap_or_else(|| Value::nothing(span))
}

#[cfg(not(target_os = "hermit"))]
pub(crate) fn bounded_command_stdout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    bounded_command_output(program, args, cwd, timeout).filter(|text| !text.is_empty())
}

#[cfg(not(target_os = "hermit"))]
pub(crate) fn bounded_command_output(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Some(if stdout.trim().is_empty() {
        stderr.into_owned()
    } else {
        stdout.into_owned()
    })
}

#[cfg(not(target_os = "hermit"))]
fn command_resolution_explanation(name: &str, resolution: &CommandResolution, span: Span) -> Value {
    if let Some(first) = resolution.matches.first() {
        let summary = format!(
            "Executable `{name}` resolves to `{}`; {} executable match(es) were found in PATH.",
            first.display(),
            resolution.matches.len()
        );
        command_explanation(
            "command_found",
            summary,
            "external command resolution; no process was started",
            &[
                "Use the first path when PATH ordering is intended.",
                "Use an absolute path if a later match is required.",
            ],
            span,
        )
    } else {
        command_explanation(
            "command_not_found",
            format!("Executable `{name}` was not found in PATH."),
            "external command resolution; no process was started",
            &[
                "Check the executable name.",
                "Inspect the searched PATH locations reported in `searched`.",
                "Install or provide the executable if the task requires it.",
            ],
            span,
        )
    }
}

#[cfg(not(target_os = "hermit"))]
fn command_spawn_error(context: &str, name: &str, err: &std::io::Error) -> ShellError {
    match err.kind() {
        ErrorKind::NotFound => {
            let resolution = resolve_command(name);
            let searched = if resolution.searched.is_empty() {
                "<empty PATH>".to_owned()
            } else {
                resolution
                    .searched
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            stone_error(
                context,
                format!(
                    "executable `{name}` was not found in PATH; searched: {searched}. Use resolve_command(\"{name}\") for structured command-resolution details."
                ),
            )
        }
        ErrorKind::PermissionDenied => stone_error(
            context,
            format!(
                "permission denied while spawning executable `{name}`. Use resolve_command(\"{name}\") and stat() to inspect the resolved file and permissions."
            ),
        ),
        ErrorKind::InvalidInput => stone_error(
            context,
            format!(
                "invalid process arguments or environment while spawning `{name}`; check argv and env for unsupported values such as interior NUL bytes."
            ),
        ),
        ErrorKind::Interrupted => stone_error(
            context,
            format!("process spawn for `{name}` was interrupted before exec; retry if the task state is otherwise unchanged."),
        ),
        ErrorKind::OutOfMemory => stone_error(
            context,
            format!("not enough memory to spawn executable `{name}`."),
        ),
        _ => {
            if let Some(raw) = err.raw_os_error() {
                return command_spawn_raw_os_error(context, name, raw, err);
            }
            stone_error(context, format!("failed to spawn `{name}`: {err}"))
        }
    }
}

#[cfg(not(target_os = "hermit"))]
fn command_spawn_raw_os_error(
    context: &str,
    name: &str,
    raw: i32,
    err: &std::io::Error,
) -> ShellError {
    let (kind, detail, next) = match raw {
        7 => (
            "argument_list_too_large",
            "the argv or environment block is too large",
            "Reduce argv/env size or pass large input through files/stdin.",
        ),
        8 => (
            "exec_format_error",
            "the file exists but is not executable for this OS/architecture, or a script is missing a valid shebang",
            "Inspect the executable with stat() and read_file(); use a valid interpreter or binary.",
        ),
        11 => (
            "spawn_resource_limit",
            "the OS temporarily could not allocate process resources",
            "Retry after other processes exit, or reduce concurrent process starts.",
        ),
        12 => (
            "spawn_resource_limit",
            "the OS reported insufficient memory",
            "Reduce memory pressure before spawning another process.",
        ),
        13 => (
            "permission_denied",
            "permission was denied by executable, directory, or mount permissions",
            "Use resolve_command() and stat() to inspect the executable and containing directories.",
        ),
        20 => (
            "path_component_not_directory",
            "a component in the executable or cwd path is not a directory",
            "Use stat() on the path components to find the non-directory entry.",
        ),
        23 | 24 => (
            "spawn_resource_limit",
            "the process or system file descriptor limit was reached",
            "Close unneeded files/processes before retrying.",
        ),
        26 => (
            "text_file_busy",
            "the executable is currently busy, often because it is being written",
            "Wait for file writes to finish or use a stable executable path.",
        ),
        _ => (
            "spawn_failed",
            "the OS rejected process spawn",
            "Inspect cwd, argv, environment, executable path, and permissions.",
        ),
    };
    stone_error(
        context,
        format!("{kind}: failed to spawn `{name}`: {err}. Cause: {detail}. Next step: {next}"),
    )
}

#[cfg(not(target_os = "hermit"))]
fn validate_command_cwd(context: &str, cwd: &Path) -> Result<(), ShellError> {
    let metadata = fs::metadata(cwd).map_err(|err| match err.kind() {
        ErrorKind::NotFound => stone_error(
            context,
            format!(
                "cwd `{}` does not exist; use stat() to inspect the intended working directory.",
                cwd.display()
            ),
        ),
        ErrorKind::PermissionDenied => stone_error(
            context,
            format!(
                "permission denied while accessing cwd `{}`; use stat() to inspect directory permissions.",
                cwd.display()
            ),
        ),
        _ => io_stone_error(context, err, cwd),
    })?;
    if !metadata.is_dir() {
        return Err(stone_error(
            context,
            format!(
                "cwd `{}` exists but is not a directory; choose a directory cwd.",
                cwd.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "hermit"))]
fn create_command_output_file(context: &str, label: &str, path: &Path) -> Result<File, ShellError> {
    File::create(path).map_err(|err| match err.kind() {
        ErrorKind::NotFound => stone_error(
            context,
            format!(
                "{label} path `{}` could not be created because a parent directory is missing; create the parent directory or choose another log path.",
                path.display()
            ),
        ),
        ErrorKind::PermissionDenied => stone_error(
            context,
            format!(
                "permission denied while creating {label} path `{}`; use stat() to inspect parent directory permissions.",
                path.display()
            ),
        ),
        ErrorKind::InvalidInput => stone_error(
            context,
            format!(
                "{label} path `{}` is invalid for file creation.",
                path.display()
            ),
        ),
        ErrorKind::OutOfMemory => stone_error(
            context,
            format!("not enough memory while creating {label} path `{}`.", path.display()),
        ),
        _ => {
            if let Some(raw) = err.raw_os_error() {
                if matches!(raw, 23 | 24 | 28) {
                    return stone_error(
                        context,
                        format!(
                            "resource limit while creating {label} path `{}`: {err}. Check file descriptor limits or available disk space.",
                            path.display()
                        ),
                    );
                }
            }
            io_stone_error(context, err, path)
        }
    })
}

#[cfg(not(target_os = "hermit"))]
struct CommandCaptureFile {
    file: File,
    path: Option<PathBuf>,
}

#[cfg(not(target_os = "hermit"))]
impl CommandCaptureFile {
    fn try_clone(&self, context: &str, label: &str) -> Result<File, ShellError> {
        self.file
            .try_clone()
            .map_err(|err| stone_error(context, format!("failed to clone {label} file: {err}")))
    }

    fn read_bytes(&mut self, context: &str, label: &str) -> Result<Vec<u8>, ShellError> {
        if let Some(path) = &self.path {
            fs::read(path).map_err(|err| io_stone_error(context, err, path))
        } else {
            self.file
                .rewind()
                .map_err(|err| stone_error(context, format!("failed to rewind {label}: {err}")))?;
            let mut bytes = Vec::new();
            self.file
                .read_to_end(&mut bytes)
                .map_err(|err| stone_error(context, format!("failed to read {label}: {err}")))?;
            Ok(bytes)
        }
    }

    fn cleanup(&self) {
        if let Some(path) = &self.path {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(not(target_os = "hermit"))]
fn create_command_capture_file(
    context: &str,
    label: &str,
    temp_prefix: &str,
    suffix: &str,
) -> Result<CommandCaptureFile, ShellError> {
    #[cfg(target_os = "linux")]
    if let Ok(file) = create_anonymous_command_output_file(&env::temp_dir()) {
        return Ok(CommandCaptureFile { file, path: None });
    }

    let path = env::temp_dir().join(format!("{temp_prefix}.{suffix}"));
    let file = create_command_output_file(context, label, &path)?;
    Ok(CommandCaptureFile {
        file,
        path: Some(path),
    })
}

#[cfg(target_os = "linux")]
fn create_anonymous_command_output_file(dir: &Path) -> Result<File, std::io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(ErrorKind::InvalidInput))?;
    let fd = unsafe {
        libc::open(
            dir.as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(not(target_os = "hermit"))]
fn command_explanation(
    kind: &str,
    summary: String,
    scope: &str,
    next_steps: &[&str],
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push("summary", Value::string(summary, span));
    record.push("scope", Value::string(scope.to_owned(), span));
    record.push(
        "next_steps",
        Value::list(
            next_steps
                .iter()
                .map(|step| Value::string((*step).to_owned(), span))
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn cleanup_stale_run_temp_files_once() {
    static CLEANUP: Once = Once::new();
    CLEANUP.call_once(|| {
        cleanup_stale_run_temp_files(&env::temp_dir(), Duration::from_secs(6 * 60 * 60));
    });
}

#[cfg(not(target_os = "hermit"))]
pub(crate) fn cleanup_stale_run_temp_files(dir: &Path, stale_after: Duration) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("stone-run-")
            || !(name.ends_with(".stdout") || name.ends_with(".stderr"))
        {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let is_stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= stale_after);
        if is_stale {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(not(target_os = "hermit"))]
fn run_posix_command(
    argv: &[String],
    cwd: &Path,
    env_overrides: &[(String, String)],
    stdin: Option<&str>,
    timeout: Duration,
    stdout_target: RunOutputTarget,
    stderr_target: RunOutputTarget,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<Record, ShellError> {
    if gateway_runtime::enabled() {
        if stdout_target == RunOutputTarget::Stdout || stderr_target == RunOutputTarget::Stdout {
            return Err(stone_error(
                "run",
                "Gateway linux.exec only supports captured or suppressed output",
            ));
        }
        let mut record = gateway_runtime::run_command(
            argv,
            cwd,
            env_overrides,
            stdin,
            timeout,
            max_stdout_bytes,
            max_stderr_bytes,
        )?;
        let span = Span::unknown();
        let mut suppressed = Record::new();
        suppressed.push(
            "stdout",
            Value::bool(stdout_target == RunOutputTarget::Suppress, span),
        );
        suppressed.push(
            "stderr",
            Value::bool(stderr_target == RunOutputTarget::Suppress, span),
        );
        if stdout_target == RunOutputTarget::Suppress {
            record.push("stdout", Value::string("", span));
        }
        if stderr_target == RunOutputTarget::Suppress {
            record.push("stderr", Value::string("", span));
        }
        record.push("suppressed", Value::record(suppressed, span));
        record.push("stderr_to_stdout", Value::bool(false, span));
        return Ok(record);
    }
    static RUN_ID: AtomicU64 = AtomicU64::new(0);

    let span = Span::unknown();
    let started = Instant::now();
    cleanup_stale_run_temp_files_once();
    validate_command_cwd("run", cwd)?;
    let temp_prefix = format!(
        "stone-run-{}-{}",
        std::process::id(),
        RUN_ID.fetch_add(1, AtomicOrdering::Relaxed)
    );
    let mut stdout_file = (stdout_target == RunOutputTarget::Capture)
        .then(|| create_command_capture_file("run", "stdout", &temp_prefix, "stdout"))
        .transpose()?;
    let mut stderr_file = (stderr_target == RunOutputTarget::Capture)
        .then(|| create_command_capture_file("run", "stderr", &temp_prefix, "stderr"))
        .transpose()?;

    let mut command = Command::new(&argv[0]);
    let stdout_stdio = match stdout_target {
        RunOutputTarget::Capture => Stdio::from(
            stdout_file
                .as_ref()
                .expect("stdout capture file should exist")
                .try_clone("run", "stdout")?,
        ),
        RunOutputTarget::Suppress => Stdio::null(),
        RunOutputTarget::Stdout => unreachable!("stdout cannot target stdout"),
    };
    let stderr_stdio = match stderr_target {
        RunOutputTarget::Capture => Stdio::from(
            stderr_file
                .as_ref()
                .expect("stderr capture file should exist")
                .try_clone("run", "stderr")?,
        ),
        RunOutputTarget::Suppress => Stdio::null(),
        RunOutputTarget::Stdout => match stdout_target {
            RunOutputTarget::Capture => Stdio::from(
                stdout_file
                    .as_ref()
                    .expect("stdout capture file should exist")
                    .try_clone("run", "stdout file for stderr")?,
            ),
            RunOutputTarget::Suppress => Stdio::null(),
            RunOutputTarget::Stdout => unreachable!("stdout cannot target stdout"),
        },
    };
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdout(stdout_stdio)
        .stderr(stderr_stdio);
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    for (key, value) in env_overrides {
        command.env(key, value);
    }

    let mut child = command.spawn().map_err(|err| {
        if let Some(file) = &stdout_file {
            file.cleanup();
        }
        if let Some(file) = &stderr_file {
            file.cleanup();
        }
        command_spawn_error("run", &argv[0], &err)
    })?;
    if let Some(stdin) = stdin {
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin
                .write_all(stdin.as_bytes())
                .map_err(|err| stone_error("run", format!("failed to write stdin: {err}")))?;
        }
    }

    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| stone_error("run", format!("failed to wait for child: {err}")))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait().map_err(|err| {
                stone_error("run", format!("failed to reap timed-out child: {err}"))
            })?;
        }
        thread::sleep(Duration::from_millis(10));
    };

    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let stdout_bytes = stdout_file
        .as_mut()
        .map(|file| file.read_bytes("run", "stdout"))
        .transpose()?
        .unwrap_or_default();
    let stderr_bytes = stderr_file
        .as_mut()
        .map(|file| file.read_bytes("run", "stderr"))
        .transpose()?
        .unwrap_or_default();
    if let Some(file) = &stdout_file {
        file.cleanup();
    }
    if let Some(file) = &stderr_file {
        file.cleanup();
    }

    let (stdout_text, stdout_truncated) = lossy_limited_text(&stdout_bytes, max_stdout_bytes);
    let (stderr_text, stderr_truncated) = lossy_limited_text(&stderr_bytes, max_stderr_bytes);

    let mut truncated = Record::new();
    truncated.push("stdout", Value::bool(stdout_truncated, span));
    truncated.push("stderr", Value::bool(stderr_truncated, span));
    let mut suppressed = Record::new();
    suppressed.push(
        "stdout",
        Value::bool(stdout_target == RunOutputTarget::Suppress, span),
    );
    suppressed.push(
        "stderr",
        Value::bool(stderr_target == RunOutputTarget::Suppress, span),
    );

    let explanation = if timed_out {
        Some(run_timeout_explanation(argv, timeout, duration_ms, span))
    } else if status.success() {
        None
    } else {
        Some(external_process_failure_explanation(
            &argv,
            status,
            &stdout_text,
            &stderr_text,
            span,
        ))
    };

    let mut record = Record::new();
    record.push("ok", Value::bool(status.success() && !timed_out, span));
    record.push(
        "kind",
        Value::string(
            if timed_out {
                "timeout"
            } else if status.success() {
                "success"
            } else {
                "exec_failed"
            },
            span,
        ),
    );
    record.push(
        "exit_code",
        status
            .code()
            .map(|code| Value::int(i64::from(code), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push("duration_ms", Value::int(duration_ms, span));
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
    record.push("stdout", Value::string(stdout_text, span));
    record.push("stderr", Value::string(stderr_text, span));
    record.push("timed_out", Value::bool(timed_out, span));
    record.push("truncated", Value::record(truncated, span));
    record.push("suppressed", Value::record(suppressed, span));
    record.push(
        "stderr_to_stdout",
        Value::bool(stderr_target == RunOutputTarget::Stdout, span),
    );
    if let Some(runtime) = maybe_python_runtime_context(argv, cwd, env_overrides) {
        record.push("runtime", runtime);
    }
    if let Some(explanation) = explanation {
        record.push("explanation", explanation);
    }
    Ok(record)
}

#[cfg(not(target_os = "hermit"))]
fn start_posix_daemon(
    argv: &[String],
    cwd: &Path,
    env_overrides: &[(String, String)],
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<Value, ShellError> {
    let span = Span::unknown();
    validate_command_cwd("start_daemon", cwd)?;
    let stdout_file = create_command_output_file("start_daemon", "stdout", stdout_path)?;
    let stderr_file = create_command_output_file("start_daemon", "stderr", stderr_path)?;

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    for (key, value) in env_overrides {
        command.env(key, value);
    }
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = command
        .spawn()
        .map_err(|err| command_spawn_error("start_daemon", &argv[0], &err))?;
    let pid = child.id();
    std::mem::forget(child);

    let mut record = Record::new();
    record.push("ok", Value::bool(true, span));
    record.push("kind", Value::string("started", span));
    record.push("pid", Value::int(i64::from(pid), span));
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
        "stdout_path",
        Value::string(stdout_path.display().to_string(), span),
    );
    record.push(
        "stderr_path",
        Value::string(stderr_path.display().to_string(), span),
    );
    record.push(
        "explanation",
        daemon_explanation(
            "daemon_started",
            "Stone spawned the process without waiting for it; use wait_port() and daemon_status() before final validation.",
            &[
                "Use run() for commands that should finish.",
                "Use start_daemon() for servers or services that tests must find still running later.",
                "Inspect stdout_path and stderr_path if the daemon exits early.",
            ],
            span,
        ),
    );
    attach_service_helper_observation(
        &mut record,
        "run.after_success",
        "service.start_daemon.after_success",
        "Stone started a long-lived daemon; validate it from a fresh client before finalizing.",
        &[
            "Use wait_port() for the expected service port.",
            "Use daemon_status() with the daemon handle to confirm the process survived.",
            "Inspect stdout_path and stderr_path if validation fails.",
        ],
        span,
    );
    Ok(Value::record(record, span))
}

#[cfg(not(target_os = "hermit"))]
fn daemon_status_record(
    pid: u32,
    port: Option<u16>,
    host: &str,
    log_path: Option<&Path>,
    max_log_bytes: usize,
) -> Value {
    let span = Span::unknown();
    let running = process_alive(pid);
    let port_open = port.map(|port| tcp_port_open(host, port, Duration::from_millis(200)));
    let ok = running && port_open.unwrap_or(true);
    let mut record = Record::new();
    record.push("ok", Value::bool(ok, span));
    record.push(
        "kind",
        Value::string(if ok { "running" } else { "not_ready" }, span),
    );
    record.push("pid", Value::int(i64::from(pid), span));
    record.push("running", Value::bool(running, span));
    if let Some(port) = port {
        record.push("host", Value::string(host.to_owned(), span));
        record.push("port", Value::int(i64::from(port), span));
        record.push("port_open", Value::bool(port_open.unwrap_or(false), span));
    }
    if let Some(log_path) = log_path {
        record.push(
            "log_path",
            Value::string(log_path.display().to_string(), span),
        );
        match read_log_tail(log_path, max_log_bytes) {
            Ok((tail, truncated)) => {
                record.push("log_tail", Value::string(tail, span));
                record.push("log_truncated", Value::bool(truncated, span));
            }
            Err(err) => {
                record.push("log_error", Value::string(err.to_string(), span));
            }
        }
    }
    if !ok {
        record.push(
            "explanation",
            daemon_explanation(
                "daemon_not_ready",
                "The daemon is not currently ready for validation.",
                &[
                    "If running is false, inspect the daemon log and restart it after fixing the command.",
                    "If port_open is false, wait for the service port or check that the daemon binds the expected host and port.",
                    "Use start_daemon() instead of shell backgrounding through run() for long-lived services.",
                ],
                span,
            ),
        );
    }
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn run_timeout_explanation(
    argv: &[String],
    timeout: Duration,
    duration_ms: i64,
    span: Span,
) -> Value {
    let timeout_ms = i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX);
    let command = argv.join(" ");
    let mut record = Record::new();
    record.push("kind", Value::string("timeout", span));
    record.push(
        "summary",
        Value::string(
            format!(
                "Stone stopped waiting for the external process after {timeout_ms} ms and killed it."
            ),
            span,
        ),
    );
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push("timeout_ms", Value::int(timeout_ms, span));
    record.push("duration_ms", Value::int(duration_ms, span));
    record.push(
        "argv",
        Value::list(
            argv.iter()
                .map(|arg| Value::string(arg.clone(), span))
                .collect(),
            span,
        ),
    );
    record.push("command", Value::string(command, span));
    record.push(
        "next_steps",
        Value::list(
            [
                "Inspect stdout and stderr for partial progress before retrying; a timed-out command may have left files or a partial checkout behind.",
                "If the command is expected to take longer, rerun it with a larger timeout_ms, for example run(argv, timeout_ms=600000).",
                "If the command should be quick, narrow the command or fix the reported stall before retrying.",
                "For services that should keep running, use start_daemon() instead of run().",
            ]
            .into_iter()
            .map(|step| Value::string(step, span))
            .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn stop_daemon_record(pid: u32, timeout: Duration) -> Value {
    let span = Span::unknown();
    let existed_before = process_alive(pid);
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
    let started = Instant::now();
    while process_alive(pid) && started.elapsed() < timeout {
        thread::sleep(Duration::from_millis(50));
    }
    let killed = if process_alive(pid) {
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status();
        thread::sleep(Duration::from_millis(50));
        true
    } else {
        false
    };
    let stopped = !process_alive(pid);
    let mut record = Record::new();
    record.push("ok", Value::bool(stopped, span));
    record.push(
        "kind",
        Value::string(if stopped { "stopped" } else { "still_running" }, span),
    );
    record.push("pid", Value::int(i64::from(pid), span));
    record.push("existed_before", Value::bool(existed_before, span));
    record.push("sent_kill", Value::bool(killed, span));
    if !stopped {
        record.push(
            "explanation",
            daemon_explanation(
                "daemon_stop_failed",
                "Stone sent termination signals but the process still appears to be running.",
                &[
                    "Inspect the process tree to see whether the service re-spawned or ignored signals.",
                    "Stop child processes explicitly if the daemon manager forked additional workers.",
                ],
                span,
            ),
        );
    }
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn wait_port_record(host: &str, port: u16, timeout: Duration) -> Value {
    let span = Span::unknown();
    let started = Instant::now();
    let mut open = false;
    while started.elapsed() < timeout {
        if tcp_port_open(host, port, Duration::from_millis(200)) {
            open = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let mut record = Record::new();
    record.push("ok", Value::bool(open, span));
    record.push(
        "kind",
        Value::string(if open { "open" } else { "timeout" }, span),
    );
    record.push("host", Value::string(host.to_owned(), span));
    record.push("port", Value::int(i64::from(port), span));
    record.push("duration_ms", Value::int(duration_ms, span));
    if !open {
        record.push(
            "explanation",
            daemon_explanation(
                "port_wait_timeout",
                format!("Port {host}:{port} did not accept TCP connections before the timeout."),
                &[
                    "Confirm the daemon is still running with daemon_status().",
                    "Check that the service binds the expected host and port.",
                    "Inspect daemon logs for startup errors.",
                ],
                span,
            ),
        );
    }
    attach_service_helper_observation(
        &mut record,
        if open {
            "run.after_success"
        } else {
            "run.after_timeout"
        },
        "service.wait_port.after_result",
        if open {
            "The TCP port accepted a connection; validate protocol behavior with a fresh client next."
        } else {
            "The TCP port did not become ready before the wait timeout."
        },
        &[
            "Confirm the daemon is still running with daemon_status().",
            "Run a fresh client process against the service, not only in-process checks.",
            "For gRPC tasks, verify the generated client can complete a real RPC handshake.",
        ],
        span,
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn process_alive(pid: u32) -> bool {
    let stat_path = Path::new("/proc").join(pid.to_string()).join("stat");
    let Ok(stat) = fs::read_to_string(stat_path) else {
        return false;
    };
    let Some(end_comm) = stat.rfind(") ") else {
        return true;
    };
    !stat[end_comm + 2..].starts_with("Z ")
}

#[cfg(not(target_os = "hermit"))]
fn tcp_port_open(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(mut addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, port)) else {
        return false;
    };
    addrs.any(|addr| std::net::TcpStream::connect_timeout(&addr, timeout).is_ok())
}

#[cfg(not(target_os = "hermit"))]
fn read_log_tail(path: &Path, max_bytes: usize) -> Result<(String, bool), std::io::Error> {
    let bytes = fs::read(path)?;
    let truncated = bytes.len() > max_bytes;
    let start = if truncated {
        bytes.len() - max_bytes
    } else {
        0
    };
    Ok((
        String::from_utf8_lossy(&bytes[start..]).into_owned(),
        truncated,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn daemon_explanation(
    kind: &str,
    summary: impl Into<String>,
    next_steps: &[&str],
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push("summary", Value::string(summary.into(), span));
    record.push(
        "scope",
        Value::string("external daemon; Stone transport succeeded", span),
    );
    record.push(
        "next_steps",
        Value::list(
            next_steps
                .iter()
                .map(|step| Value::string((*step).to_owned(), span))
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn run_failure_explanation(kind: &str, summary: String, next_steps: &[&str], span: Span) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push("summary", Value::string(summary, span));
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push(
        "next_steps",
        Value::list(
            next_steps
                .iter()
                .map(|step| Value::string((*step).to_owned(), span))
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn external_process_failure_explanation(
    argv: &[String],
    status: ExitStatus,
    stdout: &str,
    stderr: &str,
    span: Span,
) -> Value {
    let exit_text = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        let mut next_steps = vec![
            "The command produced no stdout or stderr, so Stone cannot infer the program-internal cause from captured output.".to_owned(),
            "Rerun the same command with that program's verbose/debug/log option, or change the invoked script to write diagnostics to a file.".to_owned(),
            "If you do not know the option, inspect the program help with --help, -h, or man before choosing a retry.".to_owned(),
        ];
        if let Some(hint) = verbose_hint_for_command(argv.first().map(String::as_str).unwrap_or(""))
        {
            next_steps.push(hint.to_owned());
        }
        return run_failure_explanation_owned(
            "external_process_no_clear_error",
            format!(
                "Stone successfully ran the external process, but it exited with code {exit_text} and produced no clear error message."
            ),
            &next_steps,
            span,
        );
    }
    run_failure_explanation(
        "external_process_exit",
        format!(
            "Stone successfully ran the external process, but it exited with code {exit_text}."
        ),
        &[
            "Treat stdout and stderr as feedback from the process, test runner, or tool.",
            "Fix the reported issue and rerun the command if this was validation.",
            "If the nonzero exit is expected, include that reason in the final summary.",
        ],
        span,
    )
}

#[cfg(not(target_os = "hermit"))]
fn run_failure_explanation_owned(
    kind: &str,
    summary: String,
    next_steps: &[String],
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push("summary", Value::string(summary, span));
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push(
        "next_steps",
        Value::list(
            next_steps
                .iter()
                .map(|step| Value::string(step.clone(), span))
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn verbose_hint_for_command(command: &str) -> Option<&'static str> {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    match name {
        "vim" | "vi" => Some(
            "For Vim, a useful probe is to add verbose logging such as -V1/tmp/vim.log, then read that log file.",
        ),
        "pytest" | "py.test" => Some(
            "For pytest, useful probes include -vv, -s, and --tb=long to expose test output and full tracebacks.",
        ),
        "curl" => Some("For curl, -v shows request/response connection details."),
        "make" | "gmake" => Some(
            "For make, try VERBOSE=1 or --debug when the build hides the failing command.",
        ),
        "cmake" => Some(
            "For cmake, try --debug-output, --trace, or build with --verbose depending on the failing phase.",
        ),
        "npm" => Some("For npm, try --loglevel verbose or inspect the npm debug log path it prints."),
        "python" | "python3" => Some(
            "For Python, make sure tracebacks are not swallowed; -X dev can expose additional runtime warnings.",
        ),
        "sh" | "bash" => Some(
            "For shell scripts, add set -x or explicit echo/log lines around the failing step.",
        ),
        "gcc" | "g++" | "clang" | "clang++" => {
            Some("For C/C++ compilers, -v shows toolchain, include, and linker details.")
        }
        "cargo" => Some("For cargo, -vv shows the underlying rustc commands and build-script output."),
        _ => None,
    }
}

#[cfg(not(target_os = "hermit"))]
fn lossy_limited_text(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    let truncated = bytes.len() > max_bytes;
    let end = if truncated { max_bytes } else { bytes.len() };
    (
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
        truncated,
    )
}
