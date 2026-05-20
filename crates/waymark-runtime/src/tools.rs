// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use asset_image::{AssetEntry, AssetImage};
use memchr::memmem::Finder;
use nu_protocol::engine::{EngineState, Stack};
use regex::bytes::Regex;
use serde_json::{json, Value as JsonValue};

use crate::{FrontendKind, StoneGuest};

pub struct ShellToolContext<'a> {
    pub engine_state: &'a mut EngineState,
    pub stack: &'a mut Stack,
    pub workspace: WorkspacePolicy,
    pub limits: ToolLimits,
    pub trace: Option<&'a mut ToolTraceSink>,
}

pub trait HostCapabilityRpc {
    fn request_workspace(&mut self, request: &JsonValue) -> Result<JsonValue, String>;

    fn request_linux(&mut self, request: &JsonValue) -> Result<JsonValue, String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolLimits {
    pub max_read_bytes: usize,
    pub max_write_bytes: usize,
    pub max_list_entries: usize,
    pub max_find_entries: usize,
    pub max_search_files: usize,
    pub max_search_file_bytes: u64,
    pub max_search_matches: usize,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_tool_ms: u64,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_read_bytes: 64 * 1024,
            max_write_bytes: 64 * 1024,
            max_list_entries: 200,
            max_find_entries: 4096,
            max_search_files: 1024,
            max_search_file_bytes: 1024 * 1024,
            max_search_matches: 1000,
            max_stdout_bytes: 64 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_tool_ms: 30_000,
        }
    }
}

#[derive(Default)]
pub struct ToolTraceSink {
    records: Vec<JsonValue>,
}

impl ToolTraceSink {
    pub fn push(&mut self, value: JsonValue) {
        self.records.push(value);
    }

    pub fn records(&self) -> &[JsonValue] {
        &self.records
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolTruncation {
    pub stdout: bool,
    pub stderr: bool,
    pub value: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolResult {
    pub ok: bool,
    pub kind: &'static str,
    pub value: JsonValue,
    pub stdout: String,
    pub stderr: String,
    pub truncated: ToolTruncation,
    pub duration_ms: u64,
    pub error: Option<ToolError>,
}

impl ToolResult {
    pub fn success(value: JsonValue, duration_ms: u64) -> Self {
        Self {
            ok: true,
            kind: "success",
            value,
            stdout: String::new(),
            stderr: String::new(),
            truncated: ToolTruncation::default(),
            duration_ms,
            error: None,
        }
    }

    pub fn error(
        kind: &'static str,
        code: impl Into<String>,
        message: impl Into<String>,
        duration_ms: u64,
    ) -> Self {
        Self {
            ok: false,
            kind,
            value: json!({}),
            stdout: String::new(),
            stderr: String::new(),
            truncated: ToolTruncation::default(),
            duration_ms,
            error: Some(ToolError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    pub fn to_json(&self) -> JsonValue {
        let mut root = json!({
            "ok": self.ok,
            "kind": self.kind,
            "value": self.value,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "truncated": {
                "stdout": self.truncated.stdout,
                "stderr": self.truncated.stderr,
                "value": self.truncated.value,
            },
            "duration_ms": self.duration_ms,
        });
        if let (Some(error), Some(fields)) = (&self.error, root.as_object_mut()) {
            fields.insert(
                "error".to_owned(),
                json!({
                    "code": error.code,
                    "message": error.message,
                }),
            );
        }
        root
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolAccess {
    Read,
    Write,
    Edit,
    List,
    Find,
    Search,
    Run,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GuestRoot {
    App,
    Task,
    Work,
    Result,
    Tmp,
}

impl GuestRoot {
    fn guest_path(self) -> &'static str {
        match self {
            Self::App => "/app",
            Self::Task => "/task",
            Self::Work => "/work",
            Self::Result => "/result",
            Self::Tmp => "/tmp",
        }
    }

    fn name(self) -> &'static str {
        self.guest_path().trim_start_matches('/')
    }
}

const ROOTS: [GuestRoot; 5] = [
    GuestRoot::App,
    GuestRoot::Task,
    GuestRoot::Work,
    GuestRoot::Result,
    GuestRoot::Tmp,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePolicy {
    host_root: Option<PathBuf>,
    task_assets: Option<Arc<AssetImage>>,
}

impl WorkspacePolicy {
    pub fn identity() -> Self {
        Self {
            host_root: None,
            task_assets: None,
        }
    }

    pub fn for_host_root(host_root: impl Into<PathBuf>) -> Self {
        Self {
            host_root: Some(host_root.into()),
            task_assets: None,
        }
    }

    pub fn with_task_assets(mut self, task_assets: AssetImage) -> Self {
        self.task_assets = Some(Arc::new(task_assets));
        self
    }

    pub fn prepare_host_workspace(&self) -> io::Result<()> {
        let Some(host_root) = &self.host_root else {
            return Ok(());
        };
        for root in ROOTS {
            if root == GuestRoot::Task && self.task_assets.is_some() {
                continue;
            }
            fs::create_dir_all(host_root.join(root.name()))?;
        }
        Ok(())
    }

    pub fn reset_task_owned_host_dirs(&self) -> io::Result<()> {
        let Some(host_root) = &self.host_root else {
            return Ok(());
        };
        for root in [GuestRoot::Work, GuestRoot::Result, GuestRoot::Tmp] {
            let path = host_root.join(root.name());
            match fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            fs::create_dir_all(path)?;
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        access: ToolAccess,
        raw: impl AsRef<str>,
    ) -> Result<ResolvedPath, ToolResult> {
        let guest_path = normalize_guest_path(raw.as_ref())
            .map_err(|message| ToolResult::error("path_denied", "invalid_path", message, 0))?;
        let root = root_for_path(&guest_path).ok_or_else(|| {
            ToolResult::error(
                "path_denied",
                "path_outside_workspace",
                format!("path is outside the workspace roots: {guest_path}"),
                0,
            )
        })?;
        if !root_allows(access, root) {
            return Err(ToolResult::error(
                "path_denied",
                "access_denied",
                format!(
                    "{} is not allowed under {}",
                    access_name(access),
                    root.guest_path()
                ),
                0,
            ));
        }

        let host_path = match &self.host_root {
            Some(host_root) => host_root
                .join(root.name())
                .join(relative_under_root(&guest_path, root)),
            None => PathBuf::from(&guest_path),
        };

        Ok(ResolvedPath {
            guest_path,
            host_path,
            root,
        })
    }

    fn task_assets(&self) -> Option<&AssetImage> {
        self.task_assets.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPath {
    guest_path: String,
    host_path: PathBuf,
    root: GuestRoot,
}

impl ResolvedPath {
    pub fn guest_path(&self) -> &str {
        &self.guest_path
    }

    pub fn host_path(&self) -> &Path {
        &self.host_path
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadRequest {
    pub path: String,
    pub offset: u64,
    pub max_bytes: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteMode {
    Create,
    Replace,
    Append,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteRequest {
    pub path: String,
    pub content: Vec<u8>,
    pub mode: WriteMode,
    pub create_parent_dirs: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditRequest {
    pub path: String,
    pub old: String,
    pub new: String,
    pub replace_all: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListRequest {
    pub path: String,
    pub max_entries: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindRequest {
    pub path: String,
    pub name_contains: Option<String>,
    pub name_glob: Option<String>,
    pub max_entries: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    pub path: String,
    pub needle: String,
    pub regex: bool,
    pub max_matches: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRequest {
    pub source: String,
    pub frontend: RunFrontend,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunLinuxRequest {
    pub command: String,
    pub cwd: String,
    pub timeout_ms: Option<u64>,
    pub max_stdout_bytes: Option<usize>,
    pub max_stderr_bytes: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunFrontend {
    Stone,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskTools {
    workspace: WorkspacePolicy,
    limits: ToolLimits,
}

impl TaskTools {
    pub fn new(workspace: WorkspacePolicy, limits: ToolLimits) -> Self {
        Self { workspace, limits }
    }

    pub fn identity() -> Self {
        Self::new(WorkspacePolicy::identity(), ToolLimits::default())
    }

    pub fn for_host_root(host_root: impl Into<PathBuf>) -> Self {
        Self::new(
            WorkspacePolicy::for_host_root(host_root),
            ToolLimits::default(),
        )
    }

    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn workspace(&self) -> &WorkspacePolicy {
        &self.workspace
    }

    pub fn limits(&self) -> &ToolLimits {
        &self.limits
    }

    pub fn prepare_workspace(&self) -> io::Result<()> {
        self.workspace.prepare_host_workspace()
    }

    pub fn reset_task_owned_workspace(&self) -> io::Result<()> {
        self.workspace.reset_task_owned_host_dirs()
    }

    pub fn invoke_file_json(&self, request: &JsonValue) -> ToolResult {
        dispatch_file_tool_json(&self.workspace, &self.limits, request)
    }

    pub fn invoke_json(&self, guest: &mut StoneGuest, request: &JsonValue) -> ToolResult {
        dispatch_tool_json(guest, &self.workspace, &self.limits, request)
    }

    pub fn invoke_json_with_host_rpc(
        &self,
        guest: &mut StoneGuest,
        request: &JsonValue,
        host_rpc: Option<&mut dyn HostCapabilityRpc>,
    ) -> ToolResult {
        dispatch_tool_json_with_host_rpc(guest, &self.workspace, &self.limits, request, host_rpc)
    }
}

pub fn dispatch_file_tool_json(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: &JsonValue,
) -> ToolResult {
    let tool = match required_string(request, "tool") {
        Ok(tool) => tool,
        Err(err) => return err,
    };
    let input = request.get("input").unwrap_or(&JsonValue::Null);

    match tool.as_str() {
        "read" => read_tool_json(policy, limits, input),
        "write" => write_tool_json(policy, limits, input),
        "edit" => edit_tool_json(policy, limits, input),
        "list" => list_tool_json(policy, limits, input),
        "find" => find_tool_json(policy, limits, input),
        "search" => search_tool_json(policy, limits, input),
        "run" => ToolResult::error(
            "unsupported",
            "run_requires_guest",
            "run requires a StoneGuest tool dispatcher",
            0,
        ),
        "run_linux" => ToolResult::error(
            "unsupported",
            "linux_rpc_unavailable",
            "run_linux requires a host RPC tool dispatcher",
            0,
        ),
        other => invalid_input(format!("unknown tool {other:?}")),
    }
}

pub fn dispatch_tool_json(
    guest: &mut StoneGuest,
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: &JsonValue,
) -> ToolResult {
    dispatch_tool_json_with_host_rpc(guest, policy, limits, request, None)
}

pub fn dispatch_tool_json_with_host_rpc(
    guest: &mut StoneGuest,
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: &JsonValue,
    host_rpc: Option<&mut dyn HostCapabilityRpc>,
) -> ToolResult {
    let tool = match required_string(request, "tool") {
        Ok(tool) => tool,
        Err(err) => return err,
    };
    let input = request.get("input").unwrap_or(&JsonValue::Null);

    match tool.as_str() {
        "read" | "write" | "edit" | "list" | "find" | "search" => {
            if let Some(host_rpc) = host_rpc {
                if let Some(result) = remote_workspace_tool_json(host_rpc, tool.as_str(), input) {
                    return result;
                }
            }
            dispatch_file_tool_json(policy, limits, request)
        }
        "run" => run_tool_json(guest, limits, input),
        "run_linux" => match host_rpc {
            Some(host_rpc) => run_linux_tool_json(host_rpc, limits, input),
            None => ToolResult::error(
                "linux_rpc_unavailable",
                "linux_rpc_unavailable",
                "run_linux requires a host RPC gateway",
                0,
            ),
        },
        other => invalid_input(format!("unknown tool {other:?}")),
    }
}

fn remote_workspace_tool_json(
    workspace_rpc: &mut dyn HostCapabilityRpc,
    tool: &str,
    input: &JsonValue,
) -> Option<ToolResult> {
    let path = input.get("path").and_then(JsonValue::as_str)?;
    if !path_is_under_app(path) {
        return None;
    }
    if !matches!(tool, "read" | "write" | "list") {
        return Some(ToolResult::error(
            "unsupported",
            "workspace_rpc_tool_unsupported",
            format!("stone-ws /app RPC does not yet support tool {tool:?}"),
            0,
        ));
    }

    let request = json!({
        "tool": tool,
        "input": input,
    });
    let response = match workspace_rpc.request_workspace(&request) {
        Ok(response) => response,
        Err(err) => {
            return Some(ToolResult::error(
                "workspace_rpc_error",
                "workspace_rpc_error",
                err,
                0,
            ))
        }
    };
    Some(tool_result_from_workspace_response(response))
}

pub fn run_linux_tool_json(
    linux_rpc: &mut dyn HostCapabilityRpc,
    limits: &ToolLimits,
    input: &JsonValue,
) -> ToolResult {
    let request = match parse_run_linux_request(input, limits) {
        Ok(request) => request,
        Err(err) => return err,
    };
    run_linux_tool(linux_rpc, limits, request)
}

pub fn run_linux_tool(
    linux_rpc: &mut dyn HostCapabilityRpc,
    limits: &ToolLimits,
    request: RunLinuxRequest,
) -> ToolResult {
    let started = Instant::now();
    if request
        .timeout_ms
        .is_some_and(|timeout_ms| timeout_ms > limits.max_tool_ms)
    {
        return elapsed_error(
            "limit_exceeded",
            "timeout_too_large",
            format!(
                "requested timeout exceeds max_tool_ms {}",
                limits.max_tool_ms
            ),
            started,
        );
    }
    if !path_is_under_app(&request.cwd) {
        return elapsed_error(
            "linux_path_denied",
            "linux_path_denied",
            "run_linux cwd must be /app or below /app",
            started,
        );
    }

    let response = match linux_rpc.request_linux(&json!({
        "op": "exec",
        "command": request.command,
        "cwd": request.cwd,
        "timeout_ms": request.timeout_ms.unwrap_or(limits.max_tool_ms),
        "max_stdout_bytes": request.max_stdout_bytes.unwrap_or(limits.max_stdout_bytes),
        "max_stderr_bytes": request.max_stderr_bytes.unwrap_or(limits.max_stderr_bytes),
    })) {
        Ok(response) => response,
        Err(err) => {
            return elapsed_error(
                "linux_rpc_unavailable",
                "linux_rpc_unavailable",
                err,
                started,
            )
        }
    };
    tool_result_from_linux_response(response, elapsed_ms(started))
}

fn tool_result_from_linux_response(response: JsonValue, fallback_duration_ms: u64) -> ToolResult {
    let duration_ms = response
        .get("duration_ms")
        .and_then(JsonValue::as_u64)
        .unwrap_or(fallback_duration_ms);
    let kind = response
        .get("kind")
        .and_then(JsonValue::as_str)
        .unwrap_or("linux_exec_failed");
    if response.get("ok").and_then(JsonValue::as_bool) == Some(true) {
        let mut result = ToolResult::success(
            response.get("value").cloned().unwrap_or(JsonValue::Null),
            duration_ms,
        );
        if kind == "linux_output_truncated" {
            result.kind = "linux_output_truncated";
        }
        result.stdout = response
            .get("stdout")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        result.stderr = response
            .get("stderr")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        if let Some(truncated) = response.get("truncated") {
            result.truncated.stdout = truncated
                .get("stdout")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            result.truncated.stderr = truncated
                .get("stderr")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            result.truncated.value = truncated
                .get("value")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
        }
        return result;
    }

    let error = response.get("error").unwrap_or(&JsonValue::Null);
    let mut result = ToolResult::error(
        match kind {
            "linux_rpc_unavailable" => "linux_rpc_unavailable",
            "linux_sidecar_unavailable" => "linux_sidecar_unavailable",
            "linux_sidecar_start_failed" => "linux_sidecar_start_failed",
            "linux_exec_timeout" => "linux_exec_timeout",
            "linux_path_denied" => "linux_path_denied",
            "linux_output_truncated" => "linux_output_truncated",
            "workspace_mapping" => "workspace_mapping",
            _ => "linux_exec_failed",
        },
        error
            .get("code")
            .and_then(JsonValue::as_str)
            .unwrap_or(kind),
        error
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or("linux exec failed"),
        duration_ms,
    );
    result.value = response.get("value").cloned().unwrap_or(JsonValue::Null);
    result.stdout = response
        .get("stdout")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_owned();
    result.stderr = response
        .get("stderr")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_owned();
    result
}

fn path_is_under_app(path: &str) -> bool {
    path == "/app" || path.strip_prefix("/app/").is_some()
}

fn tool_result_from_workspace_response(response: JsonValue) -> ToolResult {
    let duration_ms = response
        .get("duration_ms")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    if response.get("ok").and_then(JsonValue::as_bool) == Some(true) {
        let mut result = ToolResult::success(
            response.get("value").cloned().unwrap_or(JsonValue::Null),
            duration_ms,
        );
        if let Some(truncated) = response.get("truncated") {
            result.truncated.value = truncated
                .get("value")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            result.truncated.stdout = truncated
                .get("stdout")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            result.truncated.stderr = truncated
                .get("stderr")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
        }
        return result;
    }

    let error = response.get("error").unwrap_or(&JsonValue::Null);
    ToolResult::error(
        "workspace_rpc_error",
        error
            .get("code")
            .and_then(JsonValue::as_str)
            .unwrap_or("workspace_rpc_error"),
        error
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or("workspace RPC failed"),
        duration_ms,
    )
}

pub fn read_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: ReadRequest,
) -> ToolResult {
    let started = Instant::now();
    let path = match policy.resolve(ToolAccess::Read, &request.path) {
        Ok(path) => path,
        Err(err) => return with_duration(err, started),
    };
    if let Some(result) = read_task_asset_tool(policy, limits, &path, &request, started) {
        return result;
    }
    if path.host_path().is_dir() {
        return elapsed_error(
            "invalid_target",
            "is_directory",
            "read target is a directory",
            started,
        );
    }

    let max_bytes = request
        .max_bytes
        .unwrap_or(limits.max_read_bytes)
        .min(limits.max_read_bytes);
    let mut file = match fs::File::open(path.host_path()) {
        Ok(file) => file,
        Err(err) => return io_tool_error_with_path_suggestions(err, started, policy, &path),
    };
    if let Err(err) = file.seek(SeekFrom::Start(request.offset)) {
        return io_tool_error(err, started);
    }

    let mut bytes = Vec::with_capacity(max_bytes.saturating_add(1));
    let read_limit = max_bytes.saturating_add(1) as u64;
    if let Err(err) = file.take(read_limit).read_to_end(&mut bytes) {
        return io_tool_error(err, started);
    }
    let truncated = bytes.len() > max_bytes;
    bytes.truncate(max_bytes);

    let byte_len = bytes.len();
    let mut value_truncated = truncated;
    let content = match String::from_utf8(bytes) {
        Ok(text) => json!(text),
        Err(err) => {
            value_truncated = true;
            json!({
                "$type": "binary",
                "bytes": err.into_bytes().len(),
                "content": null,
            })
        }
    };
    let mut result = ToolResult::success(
        json!({
            "path": path.guest_path(),
            "bytes": byte_len,
            "content": content,
            "truncated": truncated,
        }),
        elapsed_ms(started),
    );
    result.truncated.value = value_truncated;
    result
}

pub fn read_tool_json(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    input: &JsonValue,
) -> ToolResult {
    let request = match parse_read_request(input) {
        Ok(request) => request,
        Err(err) => return err,
    };
    read_tool(policy, limits, request)
}

pub fn write_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: WriteRequest,
) -> ToolResult {
    let started = Instant::now();
    if request.content.len() > limits.max_write_bytes {
        return elapsed_error(
            "limit_exceeded",
            "write_too_large",
            format!(
                "write content is {} bytes, limit is {} bytes",
                request.content.len(),
                limits.max_write_bytes
            ),
            started,
        );
    }

    let path = match policy.resolve(ToolAccess::Write, &request.path) {
        Ok(path) => path,
        Err(err) => return with_duration(err, started),
    };
    if let Some(parent) = path.host_path().parent() {
        if request.create_parent_dirs {
            if let Err(err) = fs::create_dir_all(parent) {
                return io_tool_error(err, started);
            }
        } else if !parent.exists() {
            return elapsed_error(
                "invalid_target",
                "parent_missing",
                "parent directory does not exist",
                started,
            );
        }
    }

    let mut options = OpenOptions::new();
    options.write(true);
    match request.mode {
        WriteMode::Create => {
            options.create_new(true);
        }
        WriteMode::Replace => {
            options.create(true).truncate(true);
        }
        WriteMode::Append => {
            options.create(true).append(true);
        }
    }

    let mut file = match options.open(path.host_path()) {
        Ok(file) => file,
        Err(err) => return io_tool_error(err, started),
    };
    if let Err(err) = file.write_all(&request.content) {
        return io_tool_error(err, started);
    }

    ToolResult::success(
        json!({
            "path": path.guest_path(),
            "bytes": request.content.len(),
        }),
        elapsed_ms(started),
    )
}

pub fn write_tool_json(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    input: &JsonValue,
) -> ToolResult {
    let request = match parse_write_request(input) {
        Ok(request) => request,
        Err(err) => return err,
    };
    write_tool(policy, limits, request)
}

pub fn edit_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: EditRequest,
) -> ToolResult {
    let started = Instant::now();
    let path = match policy.resolve(ToolAccess::Edit, &request.path) {
        Ok(path) => path,
        Err(err) => return with_duration(err, started),
    };
    let bytes = match fs::read(path.host_path()) {
        Ok(bytes) => bytes,
        Err(err) => return io_tool_error(err, started),
    };
    if bytes.len() > limits.max_read_bytes {
        return elapsed_error(
            "limit_exceeded",
            "file_too_large",
            "file is larger than the edit read limit",
            started,
        );
    }
    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(_) => {
            return elapsed_error(
                "invalid_target",
                "non_utf8_file",
                "edit only supports UTF-8 text files",
                started,
            )
        }
    };
    if request.old.is_empty() {
        return elapsed_error(
            "invalid_input",
            "empty_match",
            "edit old text must not be empty",
            started,
        );
    }
    let matches = content.match_indices(&request.old).count();
    if matches == 0 {
        return elapsed_error(
            "edit_failed",
            "match_not_found",
            "edit old text was not found",
            started,
        );
    }
    if matches > 1 && !request.replace_all {
        return elapsed_error(
            "edit_failed",
            "multiple_matches",
            "edit old text matched more than once",
            started,
        );
    }

    let edited = if request.replace_all {
        content.replace(&request.old, &request.new)
    } else {
        content.replacen(&request.old, &request.new, 1)
    };
    if edited.len() > limits.max_write_bytes {
        return elapsed_error(
            "limit_exceeded",
            "edit_result_too_large",
            "edited file would exceed the write limit",
            started,
        );
    }
    if let Err(err) = fs::write(path.host_path(), edited.as_bytes()) {
        return io_tool_error(err, started);
    }

    ToolResult::success(
        json!({
            "path": path.guest_path(),
            "replacements": if request.replace_all { matches } else { 1 },
            "bytes": edited.len(),
        }),
        elapsed_ms(started),
    )
}

pub fn edit_tool_json(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    input: &JsonValue,
) -> ToolResult {
    let request = match parse_edit_request(input) {
        Ok(request) => request,
        Err(err) => return err,
    };
    edit_tool(policy, limits, request)
}

pub fn list_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: ListRequest,
) -> ToolResult {
    let started = Instant::now();
    let path = match policy.resolve(ToolAccess::List, &request.path) {
        Ok(path) => path,
        Err(err) => return with_duration(err, started),
    };
    if let Some(result) = list_task_asset_tool(policy, limits, &path, &request, started) {
        return result;
    }
    let max_entries = request
        .max_entries
        .unwrap_or(limits.max_list_entries)
        .min(limits.max_list_entries);

    let metadata = match fs::symlink_metadata(path.host_path()) {
        Ok(metadata) => metadata,
        Err(err) => return io_tool_error(err, started),
    };
    if !metadata.is_dir() {
        return elapsed_error(
            "invalid_target",
            "not_directory",
            "list target is not a directory",
            started,
        );
    }

    let mut entries = match fs::read_dir(path.host_path()) {
        Ok(read_dir) => {
            let mut entries = Vec::new();
            for entry in read_dir {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => return io_tool_error(err, started),
                };
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(err) => return io_tool_error(err, started),
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                let guest_child = join_guest_child(path.guest_path(), &name);
                entries.push(json!({
                    "name": name,
                    "path": guest_child,
                    "kind": metadata_kind(&metadata),
                    "bytes": if metadata.is_file() { metadata.len() } else { 0 },
                }));
            }
            entries
        }
        Err(err) => return io_tool_error(err, started),
    };
    entries.sort_by(|left, right| {
        left.get("name")
            .and_then(JsonValue::as_str)
            .cmp(&right.get("name").and_then(JsonValue::as_str))
    });

    let truncated = entries.len() > max_entries;
    entries.truncate(max_entries);
    let mut result = ToolResult::success(
        json!({
            "path": path.guest_path(),
            "entries": entries,
            "truncated": truncated,
        }),
        elapsed_ms(started),
    );
    result.truncated.value = truncated;
    result
}

pub fn list_tool_json(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    input: &JsonValue,
) -> ToolResult {
    let request = match parse_list_request(input) {
        Ok(request) => request,
        Err(err) => return err,
    };
    list_tool(policy, limits, request)
}

pub fn find_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: FindRequest,
) -> ToolResult {
    let started = Instant::now();
    let path = match policy.resolve(ToolAccess::Find, &request.path) {
        Ok(path) => path,
        Err(err) => return with_duration(err, started),
    };
    if let Some(result) = find_task_asset_tool(policy, limits, &path, &request, started) {
        return result;
    }
    let max_entries = request
        .max_entries
        .unwrap_or(limits.max_find_entries)
        .min(limits.max_find_entries);

    let mut entries = Vec::new();
    let mut queue = VecDeque::from([(path.guest_path().to_owned(), path.host_path().to_owned())]);
    let mut truncated = false;

    while let Some((guest_path, host_path)) = queue.pop_front() {
        let metadata = match fs::symlink_metadata(&host_path) {
            Ok(metadata) => metadata,
            Err(err) => return io_tool_error(err, started),
        };
        let name = host_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| guest_path.trim_start_matches('/').to_owned());
        let include = find_name_matches(
            &name,
            request.name_contains.as_deref(),
            request.name_glob.as_deref(),
        );
        if include {
            if entries.len() >= max_entries {
                truncated = true;
                break;
            }
            entries.push(json!({
                "name": name,
                "path": guest_path,
                "kind": metadata_kind(&metadata),
                "bytes": if metadata.is_file() { metadata.len() } else { 0 },
            }));
        }
        if metadata.is_dir() {
            let children = match fs::read_dir(&host_path) {
                Ok(read_dir) => read_dir
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>(),
                Err(err) => return io_tool_error(err, started),
            };
            let mut children = match children {
                Ok(children) => children,
                Err(err) => return io_tool_error(err, started),
            };
            children.sort();
            for child in children {
                let Some(child_name) = child.file_name().map(|name| name.to_string_lossy()) else {
                    continue;
                };
                queue.push_back((join_guest_child(&guest_path, &child_name), child));
            }
        }
    }

    entries.sort_by(|left, right| {
        left.get("path")
            .and_then(JsonValue::as_str)
            .cmp(&right.get("path").and_then(JsonValue::as_str))
    });
    let mut result = ToolResult::success(
        json!({
            "path": path.guest_path(),
            "entries": entries,
            "truncated": truncated,
        }),
        elapsed_ms(started),
    );
    result.truncated.value = truncated;
    result
}

pub fn find_tool_json(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    input: &JsonValue,
) -> ToolResult {
    let request = match parse_find_request(input) {
        Ok(request) => request,
        Err(err) => return err,
    };
    find_tool(policy, limits, request)
}

pub fn search_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    request: SearchRequest,
) -> ToolResult {
    let started = Instant::now();
    if request.needle.is_empty() {
        return elapsed_error(
            "invalid_input",
            "empty_needle",
            "search needle must not be empty",
            started,
        );
    }

    let path = match policy.resolve(ToolAccess::Search, &request.path) {
        Ok(path) => path,
        Err(err) => return with_duration(err, started),
    };
    if let Some(result) = search_task_asset_tool(policy, limits, &path, &request, started) {
        return result;
    }
    let max_matches = request
        .max_matches
        .unwrap_or(limits.max_search_matches)
        .min(limits.max_search_matches);

    let mut files_visited = 0usize;
    let mut matches = Vec::new();
    let mut queue = VecDeque::from([(path.guest_path().to_owned(), path.host_path().to_owned())]);
    let mut truncated = false;
    let matcher = match SearchMatcher::new(&request.needle, request.regex, started) {
        Ok(matcher) => matcher,
        Err(err) => return err,
    };

    while let Some((guest_path, host_path)) = queue.pop_front() {
        if files_visited >= limits.max_search_files || matches.len() >= max_matches {
            truncated = true;
            break;
        }
        let metadata = match fs::symlink_metadata(&host_path) {
            Ok(metadata) => metadata,
            Err(err) => return io_tool_error(err, started),
        };
        if metadata.is_dir() {
            let children = match fs::read_dir(&host_path) {
                Ok(read_dir) => read_dir
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>(),
                Err(err) => return io_tool_error(err, started),
            };
            let mut children = match children {
                Ok(children) => children,
                Err(err) => return io_tool_error(err, started),
            };
            children.sort();
            for child in children {
                let Some(child_name) = child.file_name().map(|name| name.to_string_lossy()) else {
                    continue;
                };
                queue.push_back((join_guest_child(&guest_path, &child_name), child));
            }
            continue;
        }
        if !metadata.is_file() || metadata.len() > limits.max_search_file_bytes {
            continue;
        }

        files_visited += 1;
        let Ok(bytes) = fs::read(&host_path) else {
            continue;
        };
        if bytes_look_binary(&bytes) || !matcher.is_match(&bytes) {
            continue;
        }
        push_search_line_matches_bytes(
            &mut matches,
            &mut truncated,
            &guest_path,
            &bytes,
            &matcher,
            max_matches,
        );
    }

    matches.sort_by(|left, right| {
        left.get("path")
            .and_then(JsonValue::as_str)
            .cmp(&right.get("path").and_then(JsonValue::as_str))
            .then_with(|| {
                left.get("line")
                    .and_then(JsonValue::as_u64)
                    .cmp(&right.get("line").and_then(JsonValue::as_u64))
            })
    });
    let mut result = ToolResult::success(
        json!({
            "path": path.guest_path(),
            "matches": matches,
            "truncated": truncated,
        }),
        elapsed_ms(started),
    );
    result.truncated.value = truncated;
    result
}

pub fn search_tool_json(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    input: &JsonValue,
) -> ToolResult {
    let request = match parse_search_request(input) {
        Ok(request) => request,
        Err(err) => return err,
    };
    search_tool(policy, limits, request)
}

fn read_task_asset_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    path: &ResolvedPath,
    request: &ReadRequest,
    started: Instant,
) -> Option<ToolResult> {
    let assets = task_assets_for_path(policy, path)?;
    if task_asset_is_dir(assets, path.guest_path()) {
        return Some(elapsed_error(
            "invalid_target",
            "is_directory",
            "read target is a directory",
            started,
        ));
    }

    let Some(entry) = task_asset_entry(assets, path.guest_path()) else {
        return Some(elapsed_error(
            "io_error",
            "not_found",
            "asset path was not found",
            started,
        ));
    };
    let max_bytes = request
        .max_bytes
        .unwrap_or(limits.max_read_bytes)
        .min(limits.max_read_bytes);
    let start = match usize::try_from(request.offset) {
        Ok(start) => start.min(entry.content.len()),
        Err(_) => entry.content.len(),
    };
    let end = entry.content.len().min(start.saturating_add(max_bytes));
    let truncated = end < entry.content.len();
    let bytes = entry.content[start..end].to_vec();

    Some(read_bytes_success(
        path.guest_path(),
        bytes,
        truncated,
        started,
    ))
}

fn list_task_asset_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    path: &ResolvedPath,
    request: &ListRequest,
    started: Instant,
) -> Option<ToolResult> {
    let assets = task_assets_for_path(policy, path)?;
    if task_asset_entry(assets, path.guest_path()).is_some() {
        return Some(elapsed_error(
            "invalid_target",
            "not_directory",
            "list target is not a directory",
            started,
        ));
    }
    if !task_asset_is_dir(assets, path.guest_path()) {
        return Some(elapsed_error(
            "io_error",
            "not_found",
            "asset path was not found",
            started,
        ));
    }

    let max_entries = request
        .max_entries
        .unwrap_or(limits.max_list_entries)
        .min(limits.max_list_entries);
    let mut children: BTreeMap<String, JsonValue> = BTreeMap::new();
    for entry in task_asset_descendants(assets, path.guest_path()) {
        let guest_child = task_asset_guest_path(entry);
        let Some(rest) = task_asset_descendant_rest(path.guest_path(), &guest_child) else {
            continue;
        };
        let Some((name, child_kind, bytes)) = task_asset_child_summary(rest, entry) else {
            continue;
        };
        let child_path = join_guest_child(path.guest_path(), name);
        children.entry(name.to_owned()).or_insert_with(|| {
            json!({
                "name": name,
                "path": child_path,
                "kind": child_kind,
                "bytes": bytes,
            })
        });
    }

    let mut entries = children.into_values().collect::<Vec<_>>();
    let truncated = entries.len() > max_entries;
    entries.truncate(max_entries);
    let mut result = ToolResult::success(
        json!({
            "path": path.guest_path(),
            "entries": entries,
            "truncated": truncated,
        }),
        elapsed_ms(started),
    );
    result.truncated.value = truncated;
    Some(result)
}

fn find_task_asset_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    path: &ResolvedPath,
    request: &FindRequest,
    started: Instant,
) -> Option<ToolResult> {
    let assets = task_assets_for_path(policy, path)?;
    let max_entries = request
        .max_entries
        .unwrap_or(limits.max_find_entries)
        .min(limits.max_find_entries);
    let mut entries = Vec::new();
    let mut truncated = false;

    let candidates = if let Some(entry) = task_asset_entry(assets, path.guest_path()) {
        vec![entry]
    } else if task_asset_is_dir(assets, path.guest_path()) {
        task_asset_descendants(assets, path.guest_path()).collect::<Vec<_>>()
    } else {
        return Some(elapsed_error(
            "io_error",
            "not_found",
            "asset path was not found",
            started,
        ));
    };

    for entry in candidates {
        let name = task_asset_name(entry);
        let include = find_name_matches(
            name,
            request.name_contains.as_deref(),
            request.name_glob.as_deref(),
        );
        if !include {
            continue;
        }
        if entries.len() >= max_entries {
            truncated = true;
            break;
        }
        entries.push(json!({
            "name": name,
            "path": task_asset_guest_path(entry),
            "kind": "file",
            "bytes": entry.content.len(),
        }));
    }

    let mut result = ToolResult::success(
        json!({
            "path": path.guest_path(),
            "entries": entries,
            "truncated": truncated,
        }),
        elapsed_ms(started),
    );
    result.truncated.value = truncated;
    Some(result)
}

fn search_task_asset_tool(
    policy: &WorkspacePolicy,
    limits: &ToolLimits,
    path: &ResolvedPath,
    request: &SearchRequest,
    started: Instant,
) -> Option<ToolResult> {
    let assets = task_assets_for_path(policy, path)?;
    let max_matches = request
        .max_matches
        .unwrap_or(limits.max_search_matches)
        .min(limits.max_search_matches);
    let candidates = if let Some(entry) = task_asset_entry(assets, path.guest_path()) {
        vec![entry]
    } else if task_asset_is_dir(assets, path.guest_path()) {
        task_asset_descendants(assets, path.guest_path()).collect::<Vec<_>>()
    } else {
        return Some(elapsed_error(
            "io_error",
            "not_found",
            "asset path was not found",
            started,
        ));
    };

    let mut files_visited = 0usize;
    let mut matches = Vec::new();
    let mut truncated = false;
    let matcher = match SearchMatcher::new(&request.needle, request.regex, started) {
        Ok(matcher) => matcher,
        Err(err) => return Some(err),
    };
    for entry in candidates {
        if files_visited >= limits.max_search_files || matches.len() >= max_matches {
            truncated = true;
            break;
        }
        if entry.content.len() as u64 > limits.max_search_file_bytes {
            continue;
        }
        files_visited += 1;
        if bytes_look_binary(&entry.content) || !matcher.is_match(&entry.content) {
            continue;
        };
        push_search_line_matches_bytes(
            &mut matches,
            &mut truncated,
            task_asset_guest_path(entry).as_str(),
            &entry.content,
            &matcher,
            max_matches,
        );
    }

    let mut result = ToolResult::success(
        json!({
            "path": path.guest_path(),
            "matches": matches,
            "truncated": truncated,
        }),
        elapsed_ms(started),
    );
    result.truncated.value = truncated;
    Some(result)
}

enum SearchMatcher {
    Literal(Vec<u8>),
    Regex(Regex),
}

impl SearchMatcher {
    fn new(needle: &str, regex: bool, started: Instant) -> Result<Self, ToolResult> {
        if regex {
            Regex::new(needle).map(Self::Regex).map_err(|err| {
                elapsed_error("invalid_input", "invalid_regex", err.to_string(), started)
            })
        } else {
            Ok(Self::Literal(needle.as_bytes().to_vec()))
        }
    }

    fn is_match(&self, bytes: &[u8]) -> bool {
        match self {
            Self::Literal(needle) => Finder::new(needle).find(bytes).is_some(),
            Self::Regex(regex) => regex.is_match(bytes),
        }
    }
}

fn push_search_line_matches_bytes(
    matches: &mut Vec<JsonValue>,
    truncated: &mut bool,
    path: &str,
    content: &[u8],
    matcher: &SearchMatcher,
    max_matches: usize,
) {
    let mut line_number = 1usize;
    let mut start = 0usize;
    for end in memchr::memchr_iter(b'\n', content).chain(std::iter::once(content.len())) {
        let line = trim_line_end(&content[start..end]);
        if matcher.is_match(line) {
            if matches.len() >= max_matches {
                *truncated = true;
                break;
            }
            matches.push(json!({
                "path": path,
                "line": line_number,
                "text": String::from_utf8_lossy(line),
            }));
        }
        if end == content.len() {
            break;
        }
        start = end + 1;
        line_number += 1;
    }
}

fn trim_line_end(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn bytes_look_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(1024).any(|byte| *byte == 0)
}

fn task_assets_for_path<'a>(
    policy: &'a WorkspacePolicy,
    path: &ResolvedPath,
) -> Option<&'a AssetImage> {
    if path.root == GuestRoot::Task {
        policy.task_assets()
    } else {
        None
    }
}

fn task_asset_entry<'a>(assets: &'a AssetImage, guest_path: &str) -> Option<&'a AssetEntry> {
    let image_path = task_asset_image_path(guest_path);
    assets
        .entries()
        .binary_search_by(|entry| entry.path.as_str().cmp(image_path.as_str()))
        .ok()
        .map(|index| &assets.entries()[index])
}

fn task_asset_descendants<'a>(
    assets: &'a AssetImage,
    guest_path: &str,
) -> impl Iterator<Item = &'a AssetEntry> {
    let prefix = task_asset_dir_prefix(guest_path);
    assets
        .entries()
        .iter()
        .filter(move |entry| entry.path.starts_with(&prefix))
}

fn task_asset_is_dir(assets: &AssetImage, guest_path: &str) -> bool {
    guest_path == "/task" || task_asset_descendants(assets, guest_path).next().is_some()
}

fn task_asset_image_path(guest_path: &str) -> String {
    let rest = guest_path.strip_prefix("/task").unwrap_or(guest_path);
    if rest.is_empty() {
        "/".to_owned()
    } else {
        rest.to_owned()
    }
}

fn task_asset_dir_prefix(guest_path: &str) -> String {
    if guest_path == "/task" {
        "/".to_owned()
    } else {
        format!(
            "{}/",
            task_asset_image_path(guest_path).trim_end_matches('/')
        )
    }
}

fn task_asset_guest_path(entry: &AssetEntry) -> String {
    format!("/task{}", entry.path)
}

fn task_asset_name(entry: &AssetEntry) -> &str {
    entry
        .path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(entry.path.as_str())
}

fn task_asset_descendant_rest<'a>(base: &str, child: &'a str) -> Option<&'a str> {
    if base == "/task" {
        child.strip_prefix("/task/")
    } else {
        child
            .strip_prefix(base)
            .and_then(|rest| rest.strip_prefix('/'))
    }
}

fn task_asset_child_summary<'a>(
    rest: &'a str,
    entry: &'a AssetEntry,
) -> Option<(&'a str, &'static str, usize)> {
    let (name, nested) = rest.split_once('/').unwrap_or((rest, ""));
    if name.is_empty() {
        return None;
    }
    if nested.is_empty() {
        Some((name, "file", entry.content.len()))
    } else {
        Some((name, "prefix", 0))
    }
}

fn read_bytes_success(
    guest_path: &str,
    bytes: Vec<u8>,
    truncated: bool,
    started: Instant,
) -> ToolResult {
    let byte_len = bytes.len();
    let mut value_truncated = truncated;
    let content = match String::from_utf8(bytes) {
        Ok(text) => json!(text),
        Err(err) => {
            value_truncated = true;
            json!({
                "$type": "binary",
                "bytes": err.into_bytes().len(),
                "content": null,
            })
        }
    };
    let mut result = ToolResult::success(
        json!({
            "path": guest_path,
            "bytes": byte_len,
            "content": content,
            "truncated": truncated,
        }),
        elapsed_ms(started),
    );
    result.truncated.value = value_truncated;
    result
}

fn truncate_utf8(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

pub fn run_external_unsupported() -> ToolResult {
    ToolResult::error(
        "unsupported",
        "external_execution_unsupported",
        "external command execution is unsupported by shell tools",
        0,
    )
}

pub fn run_tool(guest: &mut StoneGuest, limits: &ToolLimits, request: RunRequest) -> ToolResult {
    let started = Instant::now();
    if request
        .timeout_ms
        .is_some_and(|timeout_ms| timeout_ms > limits.max_tool_ms)
    {
        return elapsed_error(
            "limit_exceeded",
            "timeout_too_large",
            format!(
                "requested timeout exceeds max_tool_ms {}",
                limits.max_tool_ms
            ),
            started,
        );
    }

    let frontend = match request.frontend {
        RunFrontend::Stone => FrontendKind::Stone,
    };
    let response = guest.command_response_with_frontend(
        frontend,
        &request.source,
        nu_protocol::PipelineData::empty(),
    );
    if response
        .get("ok")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        let mut result = ToolResult::success(
            response.get("value").cloned().unwrap_or(JsonValue::Null),
            elapsed_ms(started),
        );
        if let Some(output) = response.get("output") {
            if let Some(stdout) = output.get("stdout").and_then(JsonValue::as_str) {
                let (stdout, truncated) = truncate_utf8(stdout, limits.max_stdout_bytes);
                result.stdout = stdout;
                result.truncated.stdout = truncated;
            }
            if let Some(stderr) = output.get("stderr").and_then(JsonValue::as_str) {
                let (stderr, truncated) = truncate_utf8(stderr, limits.max_stderr_bytes);
                result.stderr = stderr;
                result.truncated.stderr = truncated;
            }
        }
        result
    } else {
        let error = response.get("error").unwrap_or(&JsonValue::Null);
        let shell_kind = error
            .get("kind")
            .and_then(JsonValue::as_str)
            .unwrap_or("command_error");
        let kind = if shell_kind == "unsupported" {
            "unsupported"
        } else {
            "command_error"
        };
        let code = error
            .get("code")
            .and_then(JsonValue::as_str)
            .unwrap_or("command_error");
        let message = error
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or("command failed");
        let mut result = elapsed_error(kind, code, message, started);
        result.value = json!({
            "response": response,
        });
        result
    }
}

pub fn run_tool_json(guest: &mut StoneGuest, limits: &ToolLimits, input: &JsonValue) -> ToolResult {
    let request = match parse_run_request(input) {
        Ok(request) => request,
        Err(err) => return err,
    };
    run_tool(guest, limits, request)
}

fn parse_read_request(input: &JsonValue) -> Result<ReadRequest, ToolResult> {
    Ok(ReadRequest {
        path: required_string(input, "path")?,
        offset: optional_u64(input, "offset")?.unwrap_or(0),
        max_bytes: optional_usize(input, "max_bytes")?,
    })
}

fn parse_write_request(input: &JsonValue) -> Result<WriteRequest, ToolResult> {
    let mode = match optional_string(input, "mode")?
        .as_deref()
        .unwrap_or("replace")
    {
        "create" => WriteMode::Create,
        "replace" => WriteMode::Replace,
        "append" => WriteMode::Append,
        other => {
            return Err(invalid_input(format!(
                "unsupported write mode {other:?}; expected create, replace, or append"
            )))
        }
    };
    Ok(WriteRequest {
        path: required_string(input, "path")?,
        content: required_string(input, "content")?.into_bytes(),
        mode,
        create_parent_dirs: optional_bool(input, "create_parent_dirs")?.unwrap_or(false),
    })
}

fn parse_edit_request(input: &JsonValue) -> Result<EditRequest, ToolResult> {
    Ok(EditRequest {
        path: required_string(input, "path")?,
        old: required_string(input, "old")?,
        new: required_string(input, "new")?,
        replace_all: optional_bool(input, "replace_all")?.unwrap_or(false),
    })
}

fn parse_list_request(input: &JsonValue) -> Result<ListRequest, ToolResult> {
    Ok(ListRequest {
        path: required_string(input, "path")?,
        max_entries: optional_usize(input, "max_entries")?,
    })
}

fn parse_find_request(input: &JsonValue) -> Result<FindRequest, ToolResult> {
    Ok(FindRequest {
        path: required_string(input, "path")?,
        name_contains: optional_string(input, "name_contains")?,
        name_glob: optional_string(input, "name_glob")?,
        max_entries: optional_usize(input, "max_entries")?,
    })
}

fn parse_search_request(input: &JsonValue) -> Result<SearchRequest, ToolResult> {
    let regex = optional_bool(input, "regex")?.unwrap_or(false);
    let regex = match optional_string(input, "mode")?.as_deref() {
        None | Some("literal") => regex,
        Some("regex") => true,
        Some(other) => return Err(invalid_input(format!("unsupported search mode {other:?}"))),
    };
    Ok(SearchRequest {
        path: required_string(input, "path")?,
        needle: required_string(input, "needle")?,
        regex,
        max_matches: optional_usize(input, "max_matches")?,
    })
}

fn parse_run_request(input: &JsonValue) -> Result<RunRequest, ToolResult> {
    let frontend = match optional_string(input, "frontend")?
        .as_deref()
        .unwrap_or("stone")
    {
        "stone" => RunFrontend::Stone,
        "nu" => return Err(invalid_input("run frontend `nu` is no longer supported")),
        other => return Err(invalid_input(format!("unsupported run frontend {other:?}"))),
    };
    Ok(RunRequest {
        source: required_string(input, "source")?,
        frontend,
        timeout_ms: optional_u64(input, "timeout_ms")?,
    })
}

fn parse_run_linux_request(
    input: &JsonValue,
    limits: &ToolLimits,
) -> Result<RunLinuxRequest, ToolResult> {
    Ok(RunLinuxRequest {
        command: required_string(input, "command")?,
        cwd: optional_string(input, "cwd")?.unwrap_or_else(|| "/app".to_owned()),
        timeout_ms: optional_u64(input, "timeout_ms")?,
        max_stdout_bytes: optional_usize(input, "max_stdout_bytes")?
            .map(|value| value.min(limits.max_stdout_bytes)),
        max_stderr_bytes: optional_usize(input, "max_stderr_bytes")?
            .map(|value| value.min(limits.max_stderr_bytes)),
    })
}

fn required_string(input: &JsonValue, field: &'static str) -> Result<String, ToolResult> {
    input
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid_input(format!("missing or non-string field {field:?}")))
}

fn optional_string(input: &JsonValue, field: &'static str) -> Result<Option<String>, ToolResult> {
    match input.get(field) {
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or_else(|| invalid_input(format!("field {field:?} must be a string"))),
        None => Ok(None),
    }
}

fn optional_bool(input: &JsonValue, field: &'static str) -> Result<Option<bool>, ToolResult> {
    match input.get(field) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| invalid_input(format!("field {field:?} must be a boolean"))),
        None => Ok(None),
    }
}

fn optional_u64(input: &JsonValue, field: &'static str) -> Result<Option<u64>, ToolResult> {
    match input.get(field) {
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid_input(format!("field {field:?} must be an unsigned integer"))),
        None => Ok(None),
    }
}

fn optional_usize(input: &JsonValue, field: &'static str) -> Result<Option<usize>, ToolResult> {
    optional_u64(input, field)?
        .map(|value| {
            usize::try_from(value)
                .map_err(|_| invalid_input(format!("field {field:?} is too large")))
        })
        .transpose()
}

fn invalid_input(message: impl Into<String>) -> ToolResult {
    ToolResult::error("invalid_input", "invalid_input", message, 0)
}

fn find_name_matches(name: &str, contains: Option<&str>, glob: Option<&str>) -> bool {
    contains.is_none_or(|needle| name.contains(needle))
        && glob.is_none_or(|pattern| wildcard_match(pattern, name))
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut pattern_index, mut text_index) = (0usize, 0usize);
    let mut star_index = None;
    let mut star_text_index = 0usize;

    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_text_index = text_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_text_index += 1;
            text_index = star_text_index;
        } else {
            return false;
        }
    }

    pattern[pattern_index..].iter().all(|byte| *byte == b'*')
}

fn normalize_guest_path(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("path must not be empty".to_owned());
    }
    if !raw.starts_with('/') {
        return Err("tool paths must start with a workspace prefix".to_owned());
    }

    let mut parts = Vec::new();
    for component in Path::new(raw).components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::Prefix(_) => return Err("path prefixes are not supported".to_owned()),
        }
    }

    Ok(format!("/{}", parts.join("/")))
}

fn root_for_path(path: &str) -> Option<GuestRoot> {
    ROOTS.into_iter().find(|root| {
        let root_path = root.guest_path();
        path == root_path
            || path
                .strip_prefix(root_path)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn relative_under_root(path: &str, root: GuestRoot) -> PathBuf {
    path.strip_prefix(root.guest_path())
        .unwrap_or("")
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

fn root_allows(access: ToolAccess, root: GuestRoot) -> bool {
    match access {
        ToolAccess::Read => matches!(
            root,
            GuestRoot::App | GuestRoot::Task | GuestRoot::Work | GuestRoot::Result
        ),
        ToolAccess::Write | ToolAccess::Edit => {
            matches!(root, GuestRoot::Work | GuestRoot::Result | GuestRoot::Tmp)
        }
        ToolAccess::Find => matches!(
            root,
            GuestRoot::App | GuestRoot::Task | GuestRoot::Work | GuestRoot::Result | GuestRoot::Tmp
        ),
        ToolAccess::Search => matches!(
            root,
            GuestRoot::App | GuestRoot::Task | GuestRoot::Work | GuestRoot::Result
        ),
        ToolAccess::List => matches!(
            root,
            GuestRoot::App | GuestRoot::Task | GuestRoot::Work | GuestRoot::Result | GuestRoot::Tmp
        ),
        ToolAccess::Run => matches!(root, GuestRoot::Work),
    }
}

fn access_name(access: ToolAccess) -> &'static str {
    match access {
        ToolAccess::Read => "read",
        ToolAccess::Write => "write",
        ToolAccess::Edit => "edit",
        ToolAccess::List => "list",
        ToolAccess::Find => "find",
        ToolAccess::Search => "search",
        ToolAccess::Run => "run",
    }
}

fn join_guest_child(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

fn metadata_kind(metadata: &fs::Metadata) -> &'static str {
    if metadata.is_dir() {
        "dir"
    } else if metadata.is_file() {
        "file"
    } else {
        "other"
    }
}

fn with_duration(mut result: ToolResult, started: Instant) -> ToolResult {
    result.duration_ms = elapsed_ms(started);
    result
}

fn elapsed_error(
    kind: &'static str,
    code: impl Into<String>,
    message: impl Into<String>,
    started: Instant,
) -> ToolResult {
    ToolResult::error(kind, code, message, elapsed_ms(started))
}

fn io_tool_error(err: std::io::Error, started: Instant) -> ToolResult {
    let code = match err.kind() {
        std::io::ErrorKind::NotFound => "not_found",
        std::io::ErrorKind::AlreadyExists => "already_exists",
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "io_error",
    };
    elapsed_error("io_error", code, err.to_string(), started)
}

fn io_tool_error_with_path_suggestions(
    err: std::io::Error,
    started: Instant,
    policy: &WorkspacePolicy,
    path: &ResolvedPath,
) -> ToolResult {
    let mut result = io_tool_error(err, started);
    let Some(error) = result.error.as_mut() else {
        return result;
    };
    if error.code != "not_found" {
        return result;
    }

    let suggestions = nearby_path_suggestions(policy, path, 5);
    if suggestions.is_empty() {
        return result;
    }

    error.message = format!(
        "{}. Did you mean {}?",
        error.message,
        suggestions.join(" or ")
    );
    result.value = json!({
        "path": path.guest_path(),
        "suggestions": suggestions,
    });
    result
}

fn nearby_path_suggestions(
    policy: &WorkspacePolicy,
    path: &ResolvedPath,
    limit: usize,
) -> Vec<String> {
    let root_dir = match &policy.host_root {
        Some(host_root) => host_root.join(path.root.name()),
        None => PathBuf::from(path.root.guest_path()),
    };
    if !root_dir.exists() {
        return Vec::new();
    }

    let expected_name = path.host_path().file_name().and_then(|name| name.to_str());
    let mut candidates = Vec::new();
    if let Some(expected_name) = expected_name {
        collect_path_suggestions_by_name(
            &root_dir,
            path.root,
            expected_name,
            limit,
            &mut candidates,
        );
    }
    if candidates.len() >= limit {
        candidates.sort();
        candidates.dedup();
        candidates.truncate(limit);
        return candidates;
    }

    let expected_suffix = path
        .host_path()
        .extension()
        .and_then(|suffix| suffix.to_str());
    if let Some(expected_suffix) = expected_suffix {
        collect_path_suggestions_by_suffix(
            &root_dir,
            path.root,
            expected_suffix,
            limit,
            &mut candidates,
        );
    }
    candidates.sort();
    candidates.dedup();
    candidates.truncate(limit);
    candidates
}

fn collect_path_suggestions_by_name(
    root_dir: &Path,
    root: GuestRoot,
    expected_name: &str,
    limit: usize,
    candidates: &mut Vec<String>,
) {
    collect_path_suggestions(root_dir, root_dir, root, limit, candidates, &mut |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == expected_name)
    });
}

fn collect_path_suggestions_by_suffix(
    root_dir: &Path,
    root: GuestRoot,
    expected_suffix: &str,
    limit: usize,
    candidates: &mut Vec<String>,
) {
    collect_path_suggestions(root_dir, root_dir, root, limit, candidates, &mut |path| {
        path.extension()
            .and_then(|suffix| suffix.to_str())
            .is_some_and(|suffix| suffix == expected_suffix)
    });
}

fn collect_path_suggestions(
    root_dir: &Path,
    current: &Path,
    root: GuestRoot,
    limit: usize,
    candidates: &mut Vec<String>,
    matches: &mut dyn FnMut(&Path) -> bool,
) {
    if candidates.len() >= limit {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if candidates.len() >= limit {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_path_suggestions(root_dir, &path, root, limit, candidates, matches);
        } else if path.is_file() && matches(&path) {
            if let Ok(relative) = path.strip_prefix(root_dir) {
                let relative = relative.to_string_lossy();
                candidates.push(format!("{}/{}", root.guest_path(), relative));
            }
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoneGuest;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn path_policy_accepts_allowed_roots() {
        let root = temp_root("policy-allowed");
        let policy = WorkspacePolicy::for_host_root(&root);

        assert_eq!(
            policy
                .resolve(ToolAccess::Read, "/app/task.yaml")
                .unwrap()
                .guest_path(),
            "/app/task.yaml"
        );
        assert_eq!(
            policy
                .resolve(ToolAccess::Read, "/task/input.txt")
                .unwrap()
                .guest_path(),
            "/task/input.txt"
        );
        assert_eq!(
            policy
                .resolve(ToolAccess::Write, "/work/answer.txt")
                .unwrap()
                .guest_path(),
            "/work/answer.txt"
        );
        assert_eq!(
            policy
                .resolve(ToolAccess::List, "/tmp")
                .unwrap()
                .guest_path(),
            "/tmp"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn path_policy_rejects_traversal_and_disallowed_roots() {
        let policy = WorkspacePolicy::identity();

        assert_eq!(
            policy
                .resolve(ToolAccess::Read, "/work/../../etc/passwd")
                .unwrap_err()
                .error
                .unwrap()
                .code,
            "path_outside_workspace"
        );
        assert_eq!(
            policy
                .resolve(ToolAccess::Write, "/task/input.txt")
                .unwrap_err()
                .error
                .unwrap()
                .code,
            "access_denied"
        );
        assert_eq!(
            policy
                .resolve(ToolAccess::Write, "/app/task.yaml")
                .unwrap_err()
                .error
                .unwrap()
                .code,
            "access_denied"
        );
        assert_eq!(
            policy
                .resolve(ToolAccess::Read, "/tmp/scratch.txt")
                .unwrap_err()
                .error
                .unwrap()
                .code,
            "access_denied"
        );
        assert_eq!(
            policy
                .resolve(ToolAccess::Write, "answer.txt")
                .unwrap_err()
                .error
                .unwrap()
                .code,
            "invalid_path"
        );
        assert_eq!(
            policy
                .resolve(ToolAccess::List, "/outside")
                .unwrap_err()
                .error
                .unwrap()
                .code,
            "path_outside_workspace"
        );
    }

    #[test]
    fn workspace_reset_preserves_task_and_clears_task_owned_dirs() {
        let root = temp_root("workspace-reset");
        let tools = TaskTools::for_host_root(&root);
        tools.prepare_workspace().unwrap();
        fs::write(root.join("task/input.txt"), "task").unwrap();
        fs::write(root.join("work/file.txt"), "work").unwrap();
        fs::write(root.join("result/output.txt"), "result").unwrap();
        fs::write(root.join("tmp/scratch.txt"), "tmp").unwrap();

        tools.reset_task_owned_workspace().unwrap();

        assert_eq!(
            fs::read_to_string(root.join("task/input.txt")).unwrap(),
            "task"
        );
        assert!(root.join("work").is_dir());
        assert!(root.join("result").is_dir());
        assert!(root.join("tmp").is_dir());
        assert!(!root.join("work/file.txt").exists());
        assert!(!root.join("result/output.txt").exists());
        assert!(!root.join("tmp/scratch.txt").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_caps_bytes_and_reports_truncation() {
        let root = temp_root("read-cap");
        fs::create_dir_all(root.join("work")).unwrap();
        fs::write(root.join("work/file.txt"), "abcdef").unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits {
            max_read_bytes: 3,
            ..ToolLimits::default()
        };

        let result = read_tool(
            &policy,
            &limits,
            ReadRequest {
                path: "/work/file.txt".to_owned(),
                offset: 0,
                max_bytes: None,
            },
        );

        assert!(result.ok);
        assert_eq!(result.value["content"], json!("abc"));
        assert_eq!(result.value["bytes"], json!(3));
        assert_eq!(result.value["truncated"], json!(true));
        assert!(result.truncated.value);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_enforces_writable_roots_and_byte_limits() {
        let root = temp_root("write-policy");
        fs::create_dir_all(root.join("work")).unwrap();
        fs::create_dir_all(root.join("task")).unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits {
            max_write_bytes: 4,
            ..ToolLimits::default()
        };

        let denied = write_tool(
            &policy,
            &limits,
            WriteRequest {
                path: "/task/input.txt".to_owned(),
                content: b"abc".to_vec(),
                mode: WriteMode::Replace,
                create_parent_dirs: false,
            },
        );
        assert!(!denied.ok);
        assert_eq!(denied.error.unwrap().code, "access_denied");

        let too_large = write_tool(
            &policy,
            &limits,
            WriteRequest {
                path: "/work/file.txt".to_owned(),
                content: b"abcde".to_vec(),
                mode: WriteMode::Replace,
                create_parent_dirs: false,
            },
        );
        assert!(!too_large.ok);
        assert_eq!(too_large.error.unwrap().code, "write_too_large");

        let ok = write_tool(
            &policy,
            &limits,
            WriteRequest {
                path: "/work/file.txt".to_owned(),
                content: b"abcd".to_vec(),
                mode: WriteMode::Replace,
                create_parent_dirs: false,
            },
        );
        assert!(ok.ok);
        assert_eq!(fs::read(root.join("work/file.txt")).unwrap(), b"abcd");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edit_handles_zero_one_and_multiple_matches() {
        let root = temp_root("edit");
        fs::create_dir_all(root.join("work")).unwrap();
        fs::write(root.join("work/file.txt"), "one two one").unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits::default();

        let missing = edit_tool(
            &policy,
            &limits,
            EditRequest {
                path: "/work/file.txt".to_owned(),
                old: "three".to_owned(),
                new: "four".to_owned(),
                replace_all: false,
            },
        );
        assert_eq!(missing.error.unwrap().code, "match_not_found");

        let multiple = edit_tool(
            &policy,
            &limits,
            EditRequest {
                path: "/work/file.txt".to_owned(),
                old: "one".to_owned(),
                new: "ONE".to_owned(),
                replace_all: false,
            },
        );
        assert_eq!(multiple.error.unwrap().code, "multiple_matches");

        let single = edit_tool(
            &policy,
            &limits,
            EditRequest {
                path: "/work/file.txt".to_owned(),
                old: "two".to_owned(),
                new: "TWO".to_owned(),
                replace_all: false,
            },
        );
        assert!(single.ok);
        assert_eq!(
            fs::read_to_string(root.join("work/file.txt")).unwrap(),
            "one TWO one"
        );

        let all = edit_tool(
            &policy,
            &limits,
            EditRequest {
                path: "/work/file.txt".to_owned(),
                old: "one".to_owned(),
                new: "ONE".to_owned(),
                replace_all: true,
            },
        );
        assert!(all.ok);
        assert_eq!(all.value["replacements"], json!(2));
        assert_eq!(
            fs::read_to_string(root.join("work/file.txt")).unwrap(),
            "ONE TWO ONE"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_sorts_and_caps_entries() {
        let root = temp_root("list");
        fs::create_dir_all(root.join("work")).unwrap();
        fs::write(root.join("work/b.txt"), "b").unwrap();
        fs::write(root.join("work/a.txt"), "a").unwrap();
        fs::write(root.join("work/c.txt"), "c").unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits {
            max_list_entries: 2,
            ..ToolLimits::default()
        };

        let result = list_tool(
            &policy,
            &limits,
            ListRequest {
                path: "/work".to_owned(),
                max_entries: None,
            },
        );

        assert!(result.ok);
        assert_eq!(result.value["entries"][0]["name"], json!("a.txt"));
        assert_eq!(result.value["entries"][1]["name"], json!("b.txt"));
        assert_eq!(result.value["truncated"], json!(true));
        assert!(result.truncated.value);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_and_search_json_tools_are_bounded_and_policy_checked() {
        let root = temp_root("find-search-json");
        fs::create_dir_all(root.join("work/nested")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        fs::write(root.join("work/answer.txt"), "needle one\n").unwrap();
        fs::write(root.join("work/nested/answer-notes.txt"), "needle two\n").unwrap();
        fs::write(root.join("tmp/scratch.txt"), "needle tmp\n").unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits {
            max_find_entries: 1,
            max_search_matches: 1,
            ..ToolLimits::default()
        };

        let find = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "find",
                "input": {
                    "path": "/work",
                    "name_contains": "answer",
                    "name_glob": "*.txt"
                }
            }),
        )
        .to_json();
        assert_eq!(find["ok"], json!(true));
        assert_eq!(find["value"]["entries"][0]["name"], json!("answer.txt"));
        assert_eq!(find["value"]["truncated"], json!(true));
        assert_eq!(find["truncated"]["value"], json!(true));

        let search = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "search",
                "input": {
                    "path": "/work",
                    "needle": "needle"
                }
            }),
        )
        .to_json();
        assert_eq!(search["ok"], json!(true));
        assert_eq!(
            search["value"]["matches"][0]["path"],
            json!("/work/answer.txt")
        );
        assert_eq!(search["value"]["matches"][0]["line"], json!(1));
        assert_eq!(search["value"]["truncated"], json!(true));

        let regex_search = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "search",
                "input": {
                    "path": "/work",
                    "needle": "needle\\s+two",
                    "regex": true
                }
            }),
        )
        .to_json();
        assert_eq!(regex_search["ok"], json!(true));
        assert_eq!(
            regex_search["value"]["matches"][0]["path"],
            json!("/work/nested/answer-notes.txt")
        );

        let invalid_regex = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "search",
                "input": {
                    "path": "/work",
                    "needle": "[",
                    "mode": "regex"
                }
            }),
        );
        assert_eq!(invalid_regex.kind, "invalid_input");
        assert_eq!(invalid_regex.error.unwrap().code, "invalid_regex");

        let denied = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "search",
                "input": {
                    "path": "/tmp",
                    "needle": "needle"
                }
            }),
        );
        assert_eq!(denied.kind, "path_denied");
        assert_eq!(denied.error.unwrap().code, "access_denied");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_asset_mount_is_read_only_and_logical() {
        let policy = WorkspacePolicy::identity().with_task_assets(
            AssetImage::new(vec![
                AssetEntry {
                    path: "/README.md".to_owned(),
                    content: b"hello assets\n".to_vec(),
                },
                AssetEntry {
                    path: "/tests/test_outputs.py".to_owned(),
                    content: b"assert 'hello' in output\n".to_vec(),
                },
            ])
            .unwrap(),
        );
        let limits = ToolLimits::default();

        let read = read_tool(
            &policy,
            &limits,
            ReadRequest {
                path: "/task/README.md".to_owned(),
                offset: 0,
                max_bytes: None,
            },
        )
        .to_json();
        assert_eq!(read["ok"], json!(true));
        assert_eq!(read["value"]["content"], json!("hello assets\n"));

        let list = list_tool(
            &policy,
            &limits,
            ListRequest {
                path: "/task".to_owned(),
                max_entries: None,
            },
        )
        .to_json();
        assert_eq!(list["ok"], json!(true));
        assert_eq!(list["value"]["entries"][0]["name"], json!("README.md"));
        assert_eq!(list["value"]["entries"][0]["kind"], json!("file"));
        assert_eq!(list["value"]["entries"][1]["name"], json!("tests"));
        assert_eq!(list["value"]["entries"][1]["kind"], json!("prefix"));

        let find = find_tool(
            &policy,
            &limits,
            FindRequest {
                path: "/task".to_owned(),
                name_contains: Some("outputs".to_owned()),
                name_glob: Some("test_*.py".to_owned()),
                max_entries: None,
            },
        )
        .to_json();
        assert_eq!(find["ok"], json!(true));
        assert_eq!(
            find["value"]["entries"][0]["path"],
            json!("/task/tests/test_outputs.py")
        );

        let search = search_tool(
            &policy,
            &limits,
            SearchRequest {
                path: "/task".to_owned(),
                needle: "hello".to_owned(),
                regex: false,
                max_matches: None,
            },
        )
        .to_json();
        assert_eq!(search["ok"], json!(true));
        assert_eq!(
            search["value"]["matches"][0]["path"],
            json!("/task/README.md")
        );

        let write = write_tool(
            &policy,
            &limits,
            WriteRequest {
                path: "/task/README.md".to_owned(),
                content: b"nope".to_vec(),
                mode: WriteMode::Replace,
                create_parent_dirs: false,
            },
        );
        assert_eq!(write.kind, "path_denied");
        assert_eq!(write.error.unwrap().code, "access_denied");
    }

    #[test]
    fn wildcard_match_supports_simple_file_globs() {
        assert!(wildcard_match("records_*.jsonl", "records_001.jsonl"));
        assert!(wildcard_match("test_?.py", "test_a.py"));
        assert!(!wildcard_match("records_*.jsonl", "records.csv"));
        assert!(!wildcard_match("test_?.py", "test_ab.py"));
    }

    #[test]
    fn json_wrappers_parse_requests_and_return_tool_results() {
        let root = temp_root("json-wrappers");
        fs::create_dir_all(root.join("work")).unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits::default();

        let write = write_tool_json(
            &policy,
            &limits,
            &json!({
                "path": "/work/answer.txt",
                "content": "hello",
                "mode": "replace"
            }),
        );
        assert!(write.ok);

        let read = read_tool_json(
            &policy,
            &limits,
            &json!({
                "path": "/work/answer.txt",
                "max_bytes": 10
            }),
        )
        .to_json();
        assert_eq!(read["ok"], json!(true));
        assert_eq!(read["kind"], json!("success"));
        assert_eq!(read["value"]["content"], json!("hello"));

        let invalid = write_tool_json(
            &policy,
            &limits,
            &json!({
                "path": "/work/answer.txt",
                "content": "hello",
                "mode": "bad"
            }),
        );
        assert_eq!(invalid.kind, "invalid_input");
        assert_eq!(invalid.error.unwrap().code, "invalid_input");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_not_found_suggests_nearby_work_path() {
        let root = temp_root("read-path-suggestion");
        fs::create_dir_all(root.join("work")).unwrap();
        fs::write(root.join("work").join("output.json"), "[]\n").unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits::default();

        let missing = read_tool_json(
            &policy,
            &limits,
            &json!({
                "path": "/work/data/output.json",
            }),
        )
        .to_json();

        assert_eq!(missing["ok"], json!(false));
        assert_eq!(missing["error"]["code"], json!("not_found"));
        assert_eq!(missing["value"]["path"], json!("/work/data/output.json"));
        assert_eq!(
            missing["value"]["suggestions"],
            json!(["/work/output.json"])
        );
        assert!(missing["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Did you mean /work/output.json?"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dispatcher_drives_file_tool_smoke() {
        let root = temp_root("dispatcher-file-smoke");
        fs::create_dir_all(root.join("work")).unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits::default();

        let write = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "write",
                "input": {
                    "path": "/work/answer.txt",
                    "content": "hello",
                    "mode": "replace"
                }
            }),
        );
        assert!(write.ok);

        let read = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "read",
                "input": {
                    "path": "/work/answer.txt"
                }
            }),
        );
        assert_eq!(read.value["content"], json!("hello"));

        let list = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "list",
                "input": {
                    "path": "/work"
                }
            }),
        );
        assert_eq!(list.value["entries"][0]["name"], json!("answer.txt"));

        let edit = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "edit",
                "input": {
                    "path": "/work/answer.txt",
                    "old": "hello",
                    "new": "goodbye"
                }
            }),
        );
        assert!(edit.ok);

        let reread = dispatch_file_tool_json(
            &policy,
            &limits,
            &json!({
                "tool": "read",
                "input": {
                    "path": "/work/answer.txt"
                }
            }),
        )
        .to_json();
        assert_eq!(reread["ok"], json!(true));
        assert_eq!(reread["kind"], json!("success"));
        assert_eq!(reread["value"]["content"], json!("goodbye"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_tool_executes_stone_snippet() {
        let root = temp_root("run-stone");
        fs::create_dir_all(root.join("work")).unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits::default();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();

        let result = dispatch_tool_json(
            &mut guest,
            &policy,
            &limits,
            &json!({
                "tool": "run",
                "input": {
                    "source": "emit({\"ready\": True})"
                }
            }),
        )
        .to_json();

        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["kind"], json!("success"));
        assert_eq!(result["value"], json!({"ready": true}));
        assert_eq!(result["stdout"], json!(""));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_tool_returns_stone_print_stdout() {
        let root = temp_root("run-stone-stdout");
        fs::create_dir_all(root.join("work")).unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits::default();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();

        let result = dispatch_tool_json(
            &mut guest,
            &policy,
            &limits,
            &json!({
                "tool": "run",
                "input": {
                    "source": "for i in range(3):\n    print(i)\nemit(\"done\")"
                }
            }),
        )
        .to_json();

        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["kind"], json!("success"));
        assert_eq!(result["value"], json!("done"));
        assert_eq!(result["stdout"], json!("0\n1\n2\n"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_tool_truncates_stone_print_stdout() {
        let root = temp_root("run-stone-stdout-truncated");
        fs::create_dir_all(root.join("work")).unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits {
            max_stdout_bytes: 4,
            ..ToolLimits::default()
        };
        let mut guest = StoneGuest::new(root.join("work")).unwrap();

        let result = dispatch_tool_json(
            &mut guest,
            &policy,
            &limits,
            &json!({
                "tool": "run",
                "input": {
                    "source": "print(\"abcdef\")\nemit(\"done\")"
                }
            }),
        )
        .to_json();

        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["value"], json!("done"));
        assert_eq!(result["stdout"], json!("abcd"));
        assert_eq!(result["truncated"]["stdout"], json!(true));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_tool_rejects_removed_nu_frontend() {
        let root = temp_root("run-external");
        fs::create_dir_all(root.join("work")).unwrap();
        let policy = WorkspacePolicy::for_host_root(&root);
        let limits = ToolLimits::default();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();

        let disabled = dispatch_tool_json(
            &mut guest,
            &policy,
            &limits,
            &json!({
                "tool": "run",
                "input": {
                    "frontend": "nu",
                    "source": "run-external echo"
                }
            }),
        );
        assert_eq!(disabled.kind, "invalid_input");
        assert_eq!(disabled.error.unwrap().code, "invalid_input");

        let _ = fs::remove_dir_all(root);
    }

    struct FakeHostRpc {
        linux_request: Option<JsonValue>,
        linux_response: JsonValue,
    }

    impl HostCapabilityRpc for FakeHostRpc {
        fn request_workspace(&mut self, _request: &JsonValue) -> Result<JsonValue, String> {
            Err("workspace RPC unavailable in fake".to_owned())
        }

        fn request_linux(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
            self.linux_request = Some(request.clone());
            Ok(self.linux_response.clone())
        }
    }

    #[test]
    fn run_linux_tool_uses_host_rpc_and_maps_result() {
        let policy = WorkspacePolicy::identity();
        let limits = ToolLimits::default();
        let root = temp_root("run-linux-rpc");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut host_rpc = FakeHostRpc {
            linux_request: None,
            linux_response: json!({
                "ok": true,
                "kind": "success",
                "value": {
                    "exit_code": 0,
                    "cwd": "/app",
                    "command": "jq --version"
                },
                "stdout": "jq-1.6\n",
                "stderr": "",
                "truncated": {
                    "stdout": false,
                    "stderr": false,
                    "value": false
                },
                "duration_ms": 7
            }),
        };

        let result = dispatch_tool_json_with_host_rpc(
            &mut guest,
            &policy,
            &limits,
            &json!({
                "tool": "run_linux",
                "input": {
                    "command": "jq --version",
                    "cwd": "/app",
                    "timeout_ms": 30000
                }
            }),
            Some(&mut host_rpc),
        )
        .to_json();

        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["value"]["exit_code"], json!(0));
        assert_eq!(result["stdout"], json!("jq-1.6\n"));
        assert_eq!(host_rpc.linux_request.unwrap()["op"], json!("exec"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_linux_tool_preserves_output_truncation_kind() {
        let policy = WorkspacePolicy::identity();
        let limits = ToolLimits::default();
        let root = temp_root("run-linux-truncated");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut host_rpc = FakeHostRpc {
            linux_request: None,
            linux_response: json!({
                "ok": true,
                "kind": "linux_output_truncated",
                "value": {
                    "exit_code": 0,
                    "cwd": "/app",
                    "command": "printf abcdef"
                },
                "stdout": "abc",
                "stderr": "",
                "truncated": {
                    "stdout": true,
                    "stderr": false,
                    "value": false
                },
                "duration_ms": 3
            }),
        };

        let result = dispatch_tool_json_with_host_rpc(
            &mut guest,
            &policy,
            &limits,
            &json!({
                "tool": "run_linux",
                "input": {
                    "command": "printf abcdef",
                    "cwd": "/app",
                    "max_stdout_bytes": 3
                }
            }),
            Some(&mut host_rpc),
        )
        .to_json();

        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["kind"], json!("linux_output_truncated"));
        assert_eq!(result["stdout"], json!("abc"));
        assert_eq!(result["truncated"]["stdout"], json!(true));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_tools_drives_agent_like_turn_sequence() {
        let root = temp_root("session-turn");
        fs::create_dir_all(root.join("work")).unwrap();
        let session = TaskTools::for_host_root(&root);
        let mut guest = StoneGuest::new(root.join("work")).unwrap();

        let calls = [
            json!({
                "tool": "write",
                "input": {
                    "path": "/work/answer.txt",
                    "content": "hello",
                    "mode": "replace"
                }
            }),
            json!({
                "tool": "read",
                "input": {
                    "path": "/work/answer.txt"
                }
            }),
            json!({
                "tool": "list",
                "input": {
                    "path": "/work"
                }
            }),
            json!({
                "tool": "edit",
                "input": {
                    "path": "/work/answer.txt",
                    "old": "hello",
                    "new": "goodbye"
                }
            }),
            json!({
                "tool": "run",
                "input": {
                    "source": "emit({\"ready\": True})"
                }
            }),
        ];

        let mut responses = Vec::new();
        for call in calls {
            let response = session.invoke_json(&mut guest, &call).to_json();
            assert_eq!(response["ok"], json!(true), "call failed: {response}");
            assert_eq!(response["kind"], json!("success"));
            assert!(response.get("value").is_some());
            assert_eq!(response["stdout"], json!(""));
            assert_eq!(response["stderr"], json!(""));
            assert!(response["duration_ms"].is_number());
            responses.push(response);
        }

        assert_eq!(responses[1]["value"]["content"], json!("hello"));
        assert_eq!(
            responses[2]["value"]["entries"][0]["name"],
            json!("answer.txt")
        );
        assert_eq!(responses[4]["value"], json!({"ready": true}));
        assert_eq!(
            fs::read_to_string(root.join("work/answer.txt")).unwrap(),
            "goodbye"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_tools_work_data_is_cleared_by_guest_work_reset() {
        let root = temp_root("session-reset");
        fs::create_dir_all(root.join("work")).unwrap();
        let session = TaskTools::for_host_root(&root);
        let mut guest = StoneGuest::new(root.join("work")).unwrap();

        let write = session.invoke_file_json(&json!({
            "tool": "write",
            "input": {
                "path": "/work/answer.txt",
                "content": "hello",
                "mode": "replace"
            }
        }));
        assert!(write.ok);
        assert!(root.join("work/answer.txt").exists());

        guest.reset_work_dir().unwrap();
        let list = session
            .invoke_file_json(&json!({
                "tool": "list",
                "input": {
                    "path": "/work"
                }
            }))
            .to_json();

        assert_eq!(list["ok"], json!(true));
        assert_eq!(list["value"]["entries"], json!([]));

        let _ = std::env::set_current_dir(std::env::temp_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tool_result_json_error_shape_is_stable() {
        let result = run_external_unsupported().to_json();

        assert_eq!(result["ok"], json!(false));
        assert_eq!(result["kind"], json!("unsupported"));
        assert_eq!(
            result["error"]["code"],
            json!("external_execution_unsupported")
        );
        assert_eq!(result["truncated"]["stdout"], json!(false));
        assert_eq!(result["truncated"]["stderr"], json!(false));
        assert_eq!(result["truncated"]["value"], json!(false));
    }

    fn temp_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("waymark-tools-{name}-{nanos}"))
    }
}
