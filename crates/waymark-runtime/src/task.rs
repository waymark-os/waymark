// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fs;
use std::path::{Path, PathBuf};

use nu_protocol::PipelineData;
use serde_json::{json, Value as JsonValue};

use crate::agent::{AgentModelGateway, AgentRunResult, AgentSession};
use crate::tools::{TaskTools, ToolLimits};
use crate::{pipeline_input_from_bytes, FrontendKind, StoneGuest};

const DEFAULT_TASK_PATH: &str = "/work/task/task.json";

impl StoneGuest {
    pub fn task_response_from_default_path(&mut self) -> JsonValue {
        self.task_response_from_path(DEFAULT_TASK_PATH)
    }

    pub fn task_response_from_path(&mut self, path: impl AsRef<Path>) -> JsonValue {
        match TaskSpec::load(path.as_ref()) {
            Ok(spec) => self.task_response(spec),
            Err(err) => task_error(None, "runner_error", err),
        }
    }

    pub fn task_response_from_value(&mut self, root: JsonValue) -> JsonValue {
        match TaskSpec::from_value(root) {
            Ok(spec) => self.task_response(spec),
            Err(err) => task_error(None, "runner_error", err),
        }
    }

    pub fn task_response_from_value_with_model_gateway(
        &mut self,
        root: JsonValue,
        gateway: &mut dyn AgentModelGateway,
    ) -> JsonValue {
        match TaskSpec::from_value(root) {
            Ok(spec) => self.task_response_with_model_gateway(spec, Some(gateway)),
            Err(err) => task_error(None, "runner_error", err),
        }
    }

    pub fn task_response_from_gateway_attempt(&mut self) -> JsonValue {
        let Some(mut gateway) = crate::gateway_runtime::GatewayAgentModelGateway::active() else {
            return task_error(
                None,
                "gateway_attempt_unavailable",
                "Gateway runtime config is not active",
            );
        };
        match gateway.attempt_task_value() {
            Ok(task) => {
                if task
                    .get("runtime")
                    .and_then(JsonValue::as_object)
                    .and_then(|runtime| runtime.get("frontend"))
                    .and_then(JsonValue::as_str)
                    == Some("stone")
                {
                    if let Err(err) = gateway.install_shared_client() {
                        return task_error(
                            None,
                            "gateway_attempt_unavailable",
                            shell_error_message(&err),
                        );
                    }
                }
                let response = self.task_response_from_value_with_model_gateway(task, &mut gateway);
                if let Err(err) = gateway.report_attempt_result(&response) {
                    return task_error(
                        response.get("id").and_then(JsonValue::as_str),
                        "gateway_report_failed",
                        shell_error_message(&err),
                    );
                }
                response
            }
            Err(err) => task_error(
                None,
                "gateway_attempt_unavailable",
                shell_error_message(&err),
            ),
        }
    }

    fn task_response(&mut self, spec: TaskSpec) -> JsonValue {
        self.task_response_with_model_gateway(spec, None)
    }

    fn task_response_with_model_gateway(
        &mut self,
        spec: TaskSpec,
        mut gateway: Option<&mut dyn AgentModelGateway>,
    ) -> JsonValue {
        let id = spec.id.clone();
        match spec.resolve() {
            Ok(resolved) => {
                let response = match resolved.frontend {
                    TaskFrontend::Stone => self.command_response_with_frontend(
                        FrontendKind::Stone,
                        &resolved.script,
                        resolved.input,
                    ),
                    TaskFrontend::Agent => match resolved.agent.task.as_deref() {
                        Some(_) => match gateway.as_deref_mut() {
                            Some(gateway) => {
                                self.agent_model_task_response(&resolved.agent, gateway)
                            }
                            None => {
                                let mut gateway_model =
                                    crate::gateway_runtime::GatewayAgentModelGateway::active();
                                match gateway_model.as_mut() {
                                    Some(gateway) => {
                                        self.agent_model_task_response(&resolved.agent, gateway)
                                    }
                                    None => agent_response(
                                        self.current_cwd(),
                                        AgentRunResult {
                                            ok: false,
                                            final_value: None,
                                            rounds: 0,
                                            turns: 0,
                                            trace: Vec::new(),
                                            error: Some(crate::agent::AgentError {
                                                code: "model_gateway_unavailable",
                                                message: "agent task requires a model gateway"
                                                    .to_owned(),
                                            }),
                                        },
                                    ),
                                }
                            }
                        },
                        None => self.agent_task_response(&resolved.agent),
                    },
                };
                task_result(
                    id,
                    response,
                    collect_artifacts(&resolved.artifacts, &self.work_dir),
                )
            }
            Err(err) => task_error(Some(&id), "runner_error", err),
        }
    }

    fn agent_task_response(&mut self, spec: &AgentSpec) -> JsonValue {
        let mut session = AgentSession::new(agent_task_tools(&self.work_dir, spec))
            .with_max_turns(spec.max_turns)
            .with_max_rounds(spec.max_rounds)
            .with_completion_path(spec.completion_path.clone());
        agent_response(
            self.current_cwd(),
            match &spec.actions {
                Some(actions) => session.run_scripted_json(self, actions),
                None => AgentRunResult {
                    ok: false,
                    final_value: None,
                    rounds: 0,
                    turns: 0,
                    trace: Vec::new(),
                    error: Some(crate::agent::AgentError {
                        code: "agent_actions_missing",
                        message: "scripted agent task requires agent.actions".to_owned(),
                    }),
                },
            },
        )
    }

    fn agent_model_task_response(
        &mut self,
        spec: &AgentSpec,
        gateway: &mut dyn AgentModelGateway,
    ) -> JsonValue {
        let mut session = AgentSession::new(agent_task_tools(&self.work_dir, spec))
            .with_max_turns(spec.max_turns)
            .with_max_rounds(spec.max_rounds)
            .with_completion_path(spec.completion_path.clone());
        let result = match spec.task.as_deref() {
            Some(task) => session.run_model_task(self, task, spec.model.as_deref(), gateway),
            None => AgentRunResult {
                ok: false,
                final_value: None,
                rounds: 0,
                turns: 0,
                trace: Vec::new(),
                error: Some(crate::agent::AgentError {
                    code: "agent_task_missing",
                    message: "model-backed agent task requires agent.task".to_owned(),
                }),
            },
        };
        agent_response(self.current_cwd(), result)
    }
}

fn agent_response(cwd: String, result: AgentRunResult) -> JsonValue {
    let diagnostics = json!({
        "agent": {
            "rounds": result.rounds,
            "turns": result.turns,
            "trace": result.trace,
        },
    });

    if result.ok {
        json!({
            "ok": true,
            "cwd": cwd,
            "value": result.final_value.unwrap_or(JsonValue::Null),
            "diagnostics": diagnostics,
        })
    } else {
        let error = result.error.unwrap_or(crate::agent::AgentError {
            code: "agent_failed",
            message: "agent failed without an error payload".to_owned(),
        });
        json!({
            "ok": false,
            "cwd": cwd,
            "error": {
                "kind": "agent",
                "code": error.code,
                "message": error.message,
            },
            "diagnostics": diagnostics,
        })
    }
}

#[cfg(target_os = "hermit")]
fn task_tools_for_current_runtime(_work_dir: &Path) -> TaskTools {
    TaskTools::identity()
}

#[cfg(not(target_os = "hermit"))]
fn task_tools_for_current_runtime(work_dir: &Path) -> TaskTools {
    match work_dir.file_name().and_then(|name| name.to_str()) {
        Some("work") => work_dir
            .parent()
            .map(TaskTools::for_host_root)
            .unwrap_or_else(TaskTools::identity),
        _ => TaskTools::identity(),
    }
}

fn agent_task_tools(work_dir: &Path, spec: &AgentSpec) -> TaskTools {
    let tools = task_tools_for_current_runtime(work_dir);
    let Some(max_tool_ms) = spec.max_tool_ms else {
        return tools;
    };
    let mut limits: ToolLimits = tools.limits().clone();
    limits.max_tool_ms = max_tool_ms;
    tools.with_limits(limits)
}

struct TaskSpec {
    id: String,
    root: JsonValue,
}

struct ResolvedTask {
    frontend: TaskFrontend,
    script: String,
    input: PipelineData,
    agent: AgentSpec,
    artifacts: Vec<ArtifactSpec>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskFrontend {
    Stone,
    Agent,
}

#[derive(Clone, Debug)]
struct AgentSpec {
    actions: Option<JsonValue>,
    task: Option<String>,
    model: Option<String>,
    max_rounds: usize,
    max_turns: usize,
    max_tool_ms: Option<u64>,
    completion_path: Option<String>,
}

impl Default for AgentSpec {
    fn default() -> Self {
        Self {
            actions: Some(json!([])),
            task: None,
            model: None,
            max_rounds: 16,
            max_turns: 16,
            max_tool_ms: None,
            completion_path: None,
        }
    }
}

struct TaskInput {
    pipeline: PipelineData,
}

#[derive(Clone, Debug)]
struct ArtifactSpec {
    name: String,
    guest_path: String,
    kind: String,
}

impl TaskSpec {
    fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|err| format!("failed to read task spec: {err}"))?;
        let root = serde_json::from_slice::<JsonValue>(&bytes)
            .map_err(|err| format!("failed to parse task spec JSON: {err}"))?;
        let id = required_string(&root, &["id"])?.to_string();
        Ok(Self { id, root })
    }

    fn from_value(root: JsonValue) -> Result<Self, String> {
        let id = required_string(&root, &["id"])?.to_string();
        Ok(Self { id, root })
    }

    fn resolve(&self) -> Result<ResolvedTask, String> {
        check_version(&self.root)?;
        let frontend = frontend_kind(&self.root)?;
        let script = self.script_source()?;
        let input = self.input()?;
        let agent = if frontend == TaskFrontend::Agent {
            self.agent_spec()?
        } else {
            AgentSpec::default()
        };
        let artifacts = self.artifacts()?;

        Ok(ResolvedTask {
            frontend,
            script,
            input: input.pipeline,
            agent,
            artifacts,
        })
    }

    fn script_source(&self) -> Result<String, String> {
        if matches!(frontend_kind(&self.root)?, TaskFrontend::Agent)
            && self.root.get("script").is_none()
        {
            return Ok(String::new());
        }

        let script = required_object(&self.root, &["script"])?;
        let source = script.get("source").and_then(JsonValue::as_str);
        let guest_path = script.get("guest_path").and_then(JsonValue::as_str);

        match (source, guest_path) {
            (Some(source), None) => Ok(source.to_string()),
            (None, Some(path)) => fs::read_to_string(path)
                .map_err(|err| format!("failed to read script guest_path {path}: {err}")),
            (Some(_), Some(_)) => {
                Err("task script must not contain both source and guest_path".into())
            }
            (None, None) => Err("task script requires source or guest_path".into()),
        }
    }

    fn input(&self) -> Result<TaskInput, String> {
        let input = self.root.get("input");
        let input_path = self.root.get("input_path").and_then(JsonValue::as_str);

        match (input, input_path) {
            (Some(value), None) => {
                let bytes = serde_json::to_vec(value)
                    .map_err(|err| format!("failed to encode task input: {err}"))?;
                Ok(TaskInput {
                    pipeline: pipeline_input_from_bytes(bytes),
                })
            }
            (None, Some(path)) => {
                let bytes = fs::read(path)
                    .map_err(|err| format!("failed to read task input_path {path}: {err}"))?;
                Ok(TaskInput {
                    pipeline: pipeline_input_from_bytes(bytes),
                })
            }
            (None, None) => Ok(TaskInput {
                pipeline: PipelineData::empty(),
            }),
            (Some(_), Some(_)) => Err("task must not contain both input and input_path".into()),
        }
    }

    fn artifacts(&self) -> Result<Vec<ArtifactSpec>, String> {
        let Some(value) = self.root.get("artifacts") else {
            return Ok(Vec::new());
        };
        let artifacts = value
            .as_array()
            .ok_or_else(|| "task artifacts must be an array".to_string())?;

        artifacts
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let artifact = value
                    .as_object()
                    .ok_or_else(|| format!("task artifacts[{index}] must be an object"))?;
                let name = artifact
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| format!("task artifacts[{index}] requires name"))?;
                let guest_path = artifact
                    .get("guest_path")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| format!("task artifacts[{index}] requires guest_path"))?;
                let kind = artifact
                    .get("kind")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("text");

                Ok(ArtifactSpec {
                    name: name.to_string(),
                    guest_path: guest_path.to_string(),
                    kind: kind.to_string(),
                })
            })
            .collect()
    }

    fn agent_spec(&self) -> Result<AgentSpec, String> {
        let agent = required_object(&self.root, &["agent"])?;
        let actions = agent.get("actions").cloned();
        let task = agent
            .get("task")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        let model = agent
            .get("model")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        let completion_path = agent
            .get("completion_path")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        if actions.is_none() && task.is_none() {
            return Err("agent frontend requires agent.actions or agent.task".to_owned());
        }
        let max_turns = match agent.get("max_turns").and_then(JsonValue::as_u64) {
            Some(value) => usize::try_from(value)
                .map_err(|_| format!("agent.max_turns is too large: {value}"))?,
            None => 16,
        };
        let max_rounds = match agent.get("max_rounds").and_then(JsonValue::as_u64) {
            Some(value) => usize::try_from(value)
                .map_err(|_| format!("agent.max_rounds is too large: {value}"))?,
            None => max_turns,
        };
        let max_tool_ms = agent.get("max_tool_ms").and_then(JsonValue::as_u64);

        Ok(AgentSpec {
            actions,
            task,
            model,
            max_rounds,
            max_turns,
            max_tool_ms,
            completion_path,
        })
    }
}

fn check_version(root: &JsonValue) -> Result<(), String> {
    match root.get("version").and_then(JsonValue::as_u64) {
        Some(0) => Ok(()),
        Some(version) => Err(format!("unsupported task contract version {version}")),
        None => Err("task requires version=0".into()),
    }
}

fn frontend_kind(root: &JsonValue) -> Result<TaskFrontend, String> {
    match optional_string(root, &["runtime", "frontend"]).unwrap_or("stone") {
        "stone" => Ok(TaskFrontend::Stone),
        "nu" => Err("runtime.frontend `nu` is no longer supported; use `stone`".into()),
        "agent" => Ok(TaskFrontend::Agent),
        other => Err(format!("unsupported runtime.frontend `{other}`")),
    }
}

fn task_result(id: String, response: JsonValue, artifacts: Vec<JsonValue>) -> JsonValue {
    let mut result = if response
        .get("ok")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        json!({
            "version": 0,
            "id": id,
            "ok": true,
            "kind": "success",
            "value": response.get("value").cloned().unwrap_or(JsonValue::Null),
            "guest": {
                "package": "waymark",
                "cwd": response.get("cwd").cloned().unwrap_or(JsonValue::Null),
            },
            "output": response.get("output").cloned().unwrap_or_else(empty_output),
            "artifacts": artifacts,
        })
    } else {
        json!({
            "version": 0,
            "id": id,
            "ok": false,
            "kind": classify_guest_error(&response),
            "error": response.get("error").cloned().unwrap_or(JsonValue::Null),
            "guest": {
                "package": "waymark",
                "cwd": response.get("cwd").cloned().unwrap_or(JsonValue::Null),
            },
            "output": response.get("output").cloned().unwrap_or_else(empty_output),
            "artifacts": artifacts,
        })
    };

    if let Some(runtime) = response.get("runtime") {
        result["runtime"] = runtime.clone();
    }
    if let Some(diagnostics) = response.get("diagnostics") {
        result["diagnostics"] = diagnostics.clone();
    }

    result
}

fn empty_output() -> JsonValue {
    json!({
        "stdout": "",
        "stderr": "",
    })
}

fn collect_artifacts(specs: &[ArtifactSpec], work_dir: &Path) -> Vec<JsonValue> {
    specs
        .iter()
        .map(|spec| collect_artifact(spec, work_dir))
        .collect()
}

fn collect_artifact(spec: &ArtifactSpec, work_dir: &Path) -> JsonValue {
    let host_path = resolve_artifact_host_path(&spec.guest_path, work_dir);
    let bytes = match fs::read(&host_path) {
        Ok(bytes) => bytes,
        Err(err) => return artifact_error(spec, format!("failed to read artifact: {err}")),
    };

    let value = match spec.kind.as_str() {
        "json" => match serde_json::from_slice::<JsonValue>(&bytes) {
            Ok(value) => value,
            Err(err) => {
                return artifact_error(spec, format!("failed to parse JSON artifact: {err}"))
            }
        },
        "text" => match String::from_utf8(bytes) {
            Ok(text) => JsonValue::String(text),
            Err(err) => {
                return artifact_error(spec, format!("failed to decode text artifact: {err}"));
            }
        },
        other => return artifact_error(spec, format!("unsupported artifact kind `{other}`")),
    };

    json!({
        "name": spec.name,
        "guest_path": spec.guest_path,
        "kind": spec.kind,
        "ok": true,
        "value": value,
    })
}

fn resolve_artifact_host_path(guest_path: &str, work_dir: &Path) -> PathBuf {
    let Some((root, rest)) = logical_workspace_path(guest_path) else {
        return PathBuf::from(guest_path);
    };
    let Some(workspace_root) = host_workspace_root_for_work_dir(work_dir) else {
        return PathBuf::from(guest_path);
    };
    workspace_root.join(root).join(rest)
}

fn logical_workspace_path(path: &str) -> Option<(&'static str, PathBuf)> {
    for root in ["task", "work", "result", "tmp"] {
        let prefix = format!("/{root}");
        if path == prefix {
            return Some((root, PathBuf::new()));
        }
        if let Some(rest) = path.strip_prefix(&format!("{prefix}/")) {
            return Some((root, rest.split('/').collect()));
        }
    }
    None
}

fn host_workspace_root_for_work_dir(work_dir: &Path) -> Option<&Path> {
    if work_dir.file_name().and_then(|name| name.to_str()) == Some("work") {
        work_dir.parent()
    } else {
        None
    }
}

fn artifact_error(spec: &ArtifactSpec, message: impl Into<String>) -> JsonValue {
    json!({
        "name": spec.name,
        "guest_path": spec.guest_path,
        "kind": spec.kind,
        "ok": false,
        "error": {
            "message": message.into(),
        },
    })
}

fn task_error(id: Option<&str>, kind: &str, message: impl Into<String>) -> JsonValue {
    json!({
        "version": 0,
        "id": id.unwrap_or("unknown"),
        "ok": false,
        "kind": kind,
        "error": {
            "message": message.into(),
        },
        "guest": {
            "package": "waymark",
        },
        "artifacts": [],
    })
}

fn shell_error_message(err: &nu_protocol::ShellError) -> String {
    match err {
        nu_protocol::ShellError::Generic(generic) if !generic.msg.is_empty() => {
            format!("{}: {}", generic.error, generic.msg)
        }
        _ => err.to_string(),
    }
}

fn classify_guest_error(response: &JsonValue) -> &'static str {
    let code = response
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(JsonValue::as_str);

    match code {
        Some("task_failure") => "task_failure",
        Some("stone_parse_error") => "stone_parse_error",
        Some("stone_script_unsupported") => "stone_lower_error",
        Some("stone_script_error") => "stone_eval_error",
        Some("parse_error") => "stone_parse_error",
        Some("io_error") => "command_error",
        Some("type_mismatch") => "command_error",
        _ => "command_error",
    }
}

fn required_string<'a>(root: &'a JsonValue, path: &[&str]) -> Result<&'a str, String> {
    optional_string(root, path).ok_or_else(|| format!("task requires {}", path.join(".")))
}

fn optional_string<'a>(root: &'a JsonValue, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(root, |value, key| value.get(*key))
        .and_then(JsonValue::as_str)
}

fn required_object<'a>(
    root: &'a JsonValue,
    path: &[&str],
) -> Result<&'a serde_json::Map<String, JsonValue>, String> {
    path.iter()
        .try_fold(root, |value, key| value.get(*key))
        .and_then(JsonValue::as_object)
        .ok_or_else(|| format!("task requires object {}", path.join(".")))
}

#[cfg(test)]
mod tests {
    use super::TaskSpec;
    use crate::agent::{AgentError, AgentModelGateway};
    use crate::tools::TaskTools;
    use crate::StoneGuest;
    use serde_json::{json, Value as JsonValue};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn gateway_task_errors_include_generic_error_detail() {
        let err = nu_protocol::ShellError::Generic(
            nu_protocol::shell_error::generic::GenericError::new_internal(
                "Stone gateway attach error",
                "connection refused",
            ),
        );

        assert_eq!(
            super::shell_error_message(&err),
            "Stone gateway attach error: connection refused"
        );
    }

    #[test]
    fn runs_inline_task_spec() {
        let root = temp_dir("inline-task");
        let task_path = root.join("task.json");
        fs::write(
            &task_path,
            r#"{
              "version": 0,
              "id": "inline-task",
              "runtime": { "frontend": "stone" },
              "script": { "source": "items = get(\"items\")\nnames = []\nfor item in items:\n    if item[\"kind\"] == \"file\":\n        names.append(item[\"name\"])\nemit(sorted(names))" },
              "input": {
                "items": [
                  { "name": "b.txt", "kind": "file" },
                  { "name": "dir", "kind": "dir" },
                  { "name": "a.txt", "kind": "file" }
                ]
              }
            }"#,
        )
        .expect("write task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["kind"], json!("success"));
        assert_eq!(response["value"], json!(["a.txt", "b.txt"]));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_guest_path_task_spec() {
        let root = temp_dir("guest-path-task");
        let task_path = root.join("task.json");
        let script_path = root.join("task.stone");
        let input_path = root.join("input.json");
        fs::write(&script_path, "get(\"message\")").expect("write script");
        fs::write(&input_path, r#"{"message":"hello"}"#).expect("write input");
        fs::write(
            &task_path,
            format!(
                r#"{{
                  "version": 0,
                  "id": "guest-path-task",
                  "runtime": {{ "frontend": "stone" }},
                  "script": {{ "guest_path": "{}" }},
                  "input_path": "{}"
                }}"#,
                script_path.display(),
                input_path.display(),
            ),
        )
        .expect("write task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!("hello"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_checked_in_stone_control_expressions_smoke_task() {
        let root = temp_dir("stone-control-expressions-smoke");
        let task = serde_json::from_str::<JsonValue>(include_str!(
            "../../../examples/tasks/stone-control-expressions-smoke.json"
        ))
        .expect("parse checked-in task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_value(task);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["kind"], json!("success"));
        assert_eq!(
            response["value"],
            json!({
                "name": "sample-17",
                "bucket": "pass",
                "primary_tag": "logic",
                "tail_tag": "safe"
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_checked_in_stone_record_selection_smoke_task() {
        let root = temp_dir("stone-record-selection-smoke");
        let task = serde_json::from_str::<JsonValue>(include_str!(
            "../../../examples/tasks/stone-record-selection-smoke.json"
        ))
        .expect("parse checked-in task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_value(task);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["kind"], json!("success"));
        assert_eq!(
            response["value"],
            json!({
                "selected": "beta",
                "score": 91,
                "margin": "positive"
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_checked_in_stone_shell_edit_smoke_task() {
        let root = temp_dir("stone-shell-edit-smoke");
        let task = serde_json::from_str::<JsonValue>(include_str!(
            "../../../examples/tasks/stone-shell-edit-smoke.json"
        ))
        .expect("parse checked-in task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_value(task);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["kind"], json!("success"));
        assert_eq!(
            response["value"],
            json!({
                "ok": true,
                "content": "goodbye world",
                "replacements": 1
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_checked_in_stone_find_search_jsonl_smoke_task() {
        let root = temp_dir("stone-find-search-jsonl-smoke");
        let task = serde_json::from_str::<JsonValue>(include_str!(
            "../../../examples/tasks/stone-find-search-jsonl-smoke.json"
        ))
        .expect("parse checked-in task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_value(task);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["kind"], json!("success"));
        assert_eq!(response["value"]["files"][0]["name"], json!("answer.txt"));
        let record = &response["value"]["matches"][0];
        assert_eq!(record["line"], json!(2));
        assert_eq!(record["text"], json!("needle here"));
        assert!(record["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("answer.txt")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_invalid_task_spec_shape() {
        let root = temp_dir("bad-task");
        let task_path = root.join("task.json");
        fs::write(&task_path, r#"{"version":0,"id":"bad","script":{}}"#).expect("write task");

        let spec = TaskSpec::load(&task_path).expect("load task");
        assert!(spec.resolve().is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classifies_invalid_task_shape_as_runner_error() {
        let root = temp_dir("runner-error");
        let task_path = root.join("task.json");
        fs::write(&task_path, r#"{"version":0,"id":"bad","script":{}}"#).expect("write task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["kind"], json!("runner_error"));
        assert_eq!(response["id"], json!("bad"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn captures_declared_text_artifact() {
        let root = temp_dir("text-artifact");
        let task_path = root.join("task.json");
        let artifact_path = root.join("message.txt");
        fs::write(
            &task_path,
            format!(
                r#"{{
                  "version": 0,
                  "id": "text-artifact",
                  "runtime": {{ "frontend": "stone" }},
                  "script": {{ "source": "message = open(\"{}\", \"w\")\nmessage.write(\"hello\")\ncat(\"{}\")" }},
                  "input": {{}},
                  "artifacts": [
                    {{
                      "name": "message",
                      "guest_path": "{}",
                      "kind": "text"
                    }}
                  ]
                }}"#,
                artifact_path.display(),
                artifact_path.display(),
                artifact_path.display(),
            ),
        )
        .expect("write task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["artifacts"][0]["ok"], json!(true));
        assert_eq!(response["artifacts"][0]["name"], json!("message"));
        assert_eq!(response["artifacts"][0]["value"], json!("hello"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn captures_work_text_and_json_artifacts() {
        let root = temp_dir("work-artifacts");
        let task_path = root.join("task.json");
        let text_path = root.join("message.txt");
        let json_path = root.join("summary.json");
        fs::write(&text_path, "hello artifact").expect("write text artifact");
        fs::write(&json_path, r#"{"ok":true,"count":2}"#).expect("write json artifact");
        fs::write(
            &task_path,
            format!(
                r#"{{
                  "version": 0,
                  "id": "work-artifacts",
                  "runtime": {{ "frontend": "stone" }},
                  "script": {{ "source": "echo(\"done\")" }},
                  "input": {{}},
                  "artifacts": [
                    {{
                      "name": "message",
                      "guest_path": "{}",
                      "kind": "text"
                    }},
                    {{
                      "name": "summary",
                      "guest_path": "{}",
                      "kind": "json"
                    }}
                  ]
                }}"#,
                text_path.display(),
                json_path.display(),
            ),
        )
        .expect("write task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["artifacts"][0]["ok"], json!(true));
        assert_eq!(response["artifacts"][0]["value"], json!("hello artifact"));
        assert_eq!(response["artifacts"][1]["ok"], json!(true));
        assert_eq!(
            response["artifacts"][1]["value"],
            json!({"ok": true, "count": 2})
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn captures_logical_result_artifact_from_workspace_root() {
        let root = temp_dir("logical-result-artifact");
        fs::create_dir_all(root.join("work")).expect("create work");
        fs::create_dir_all(root.join("result")).expect("create result");
        fs::write(root.join("result/summary.json"), r#"{"answer":42}"#)
            .expect("write result artifact");
        let task = json!({
            "version": 0,
            "id": "logical-result-artifact",
            "runtime": { "frontend": "stone" },
            "script": { "source": "emit({\"done\": True})" },
            "input": {},
            "artifacts": [
                {
                    "name": "summary",
                    "guest_path": "/result/summary.json",
                    "kind": "json"
                }
            ]
        });

        let mut guest = StoneGuest::new(root.join("work")).expect("guest");
        let response = guest.task_response_from_value(task);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["artifacts"][0]["ok"], json!(true));
        assert_eq!(
            response["artifacts"][0]["guest_path"],
            json!("/result/summary.json")
        );
        assert_eq!(response["artifacts"][0]["value"], json!({"answer": 42}));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn captures_tool_written_logical_result_artifact() {
        let root = temp_dir("tool-result-artifact");
        let tools = TaskTools::for_host_root(&root);
        tools.prepare_workspace().expect("prepare workspace");
        let write = tools.invoke_file_json(&json!({
            "tool": "write",
            "input": {
                "path": "/result/summary.json",
                "content": "{\"answer\":42}",
                "mode": "replace"
            }
        }));
        assert!(write.ok);
        let task = json!({
            "version": 0,
            "id": "tool-result-artifact",
            "runtime": { "frontend": "stone" },
            "script": { "source": "emit({\"ready\": True})" },
            "input": {},
            "artifacts": [
                {
                    "name": "summary",
                    "guest_path": "/result/summary.json",
                    "kind": "json"
                }
            ]
        });

        let mut guest = StoneGuest::new(root.join("work")).expect("guest");
        let response = guest.task_response_from_value(task);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!({"ready": true}));
        assert_eq!(response["artifacts"][0]["ok"], json!(true));
        assert_eq!(response["artifacts"][0]["value"], json!({"answer": 42}));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_scripted_agent_task_spec() {
        let root = temp_dir("scripted-agent-task");
        fs::create_dir_all(root.join("work")).expect("create work");
        let task = json!({
            "version": 0,
            "id": "scripted-agent-task",
            "runtime": { "frontend": "agent" },
            "agent": {
                "max_turns": 4,
                "actions": [
                    {
                        "tool": "run",
                        "input": {
                            "source": "emit({\"plan\": \"write answer\"})"
                        }
                    },
                    {
                        "tool": "write",
                        "input": {
                            "path": "/work/answer.txt",
                            "content": "hello from agent",
                            "mode": "replace"
                        }
                    },
                    {
                        "tool": "read",
                        "input": {
                            "path": "/work/answer.txt"
                        }
                    },
                    {
                        "final": {
                            "answer": "hello from agent",
                            "answer_path": "/work/answer.txt"
                        }
                    }
                ]
            },
            "artifacts": [
                {
                    "name": "answer",
                    "guest_path": "/work/answer.txt",
                    "kind": "text"
                }
            ]
        });

        let mut guest = StoneGuest::new(root.join("work")).expect("guest");
        let response = guest.task_response_from_value(task);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(
            response["value"],
            json!({
                "answer": "hello from agent",
                "answer_path": "/work/answer.txt"
            })
        );
        assert_eq!(response["artifacts"][0]["value"], json!("hello from agent"));
        assert_eq!(
            response["diagnostics"]["agent"]["trace"][0]["event"],
            json!("episode_start")
        );
        assert!(response["diagnostics"]["agent"]["trace"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event"] == json!("agent_final")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_model_backed_agent_task_spec_with_guest_parser() {
        let root = temp_dir("model-backed-agent-task");
        fs::create_dir_all(root.join("work")).expect("create work");
        let task = json!({
            "version": 0,
            "id": "model-backed-agent-task",
            "runtime": { "frontend": "agent" },
            "agent": {
                "max_turns": 4,
                "model": "fixture-model",
                "task": "Create /work/answer.txt containing hello from model."
            },
            "artifacts": [
                {
                    "name": "answer",
                    "guest_path": "/work/answer.txt",
                    "kind": "text"
                }
            ]
        });
        let mut gateway = FixtureGateway {
            text: r#"{"actions":[{"tool":"write","input":{"path":"/work/answer.txt","content":"hello from model","mode":"replace"}},{"tool":"read","input":{"path":"/work/answer.txt"}},{"final":{"answer":"hello from model","answer_path":"/work/answer.txt"}}]}"#.to_owned(),
            requests: Vec::new(),
        };

        let mut guest = StoneGuest::new(root.join("work")).expect("guest");
        let response = guest.task_response_from_value_with_model_gateway(task, &mut gateway);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"]["answer"], json!("hello from model"));
        assert_eq!(response["artifacts"][0]["value"], json!("hello from model"));
        assert_eq!(gateway.requests.len(), 1);
        assert_eq!(response["diagnostics"]["agent"]["rounds"], json!(1));
        assert_eq!(response["diagnostics"]["agent"]["turns"], json!(3));
        assert_eq!(gateway.requests[0]["model"], json!("fixture-model"));
        assert_eq!(
            gateway.requests[0]["messages"][1]["content"],
            json!("Create /work/answer.txt containing hello from model.")
        );
        assert!(response["diagnostics"]["agent"]["trace"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event"] == json!("model_request")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_checked_in_agent_model_smoke_task_with_fixture_gateway() {
        let root = temp_dir("agent-model-smoke");
        fs::create_dir_all(root.join("work")).expect("create work");
        let task = include_str!("../../../examples/tasks/agent-model-smoke.json");
        let task = serde_json::from_str(task).expect("parse task");
        let mut gateway = FixtureGateway {
            text: r#"{"actions":[{"tool":"write","input":{"path":"/work/answer.txt","content":"Hello, world!","mode":"replace"}},{"tool":"read","input":{"path":"/work/answer.txt"}},{"final":{"answer":"Hello, world!","answer_path":"/work/answer.txt"}}]}"#.to_owned(),
            requests: Vec::new(),
        };

        let mut guest = StoneGuest::new(root.join("work")).expect("guest");
        let response = guest.task_response_from_value_with_model_gateway(task, &mut gateway);

        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"]["answer"], json!("Hello, world!"));
        assert_eq!(response["artifacts"][0]["value"], json!("Hello, world!"));

        let _ = fs::remove_dir_all(root);
    }

    struct FixtureGateway {
        text: String,
        requests: Vec<JsonValue>,
    }

    impl AgentModelGateway for FixtureGateway {
        fn request_model(&mut self, request: &JsonValue) -> Result<JsonValue, AgentError> {
            self.requests.push(request.clone());
            Ok(json!({
                "ok": true,
                "text": self.text,
            }))
        }
    }

    #[test]
    fn classifies_fail_command_as_task_failure() {
        let root = temp_dir("task-failure");
        let task_path = root.join("task.json");
        fs::write(
            &task_path,
            r#"{
              "version": 0,
              "id": "task-failure",
              "runtime": { "frontend": "stone" },
              "script": { "source": "fail(\"bad input\", code=\"bad_input\")" },
              "input": {}
            }"#,
        )
        .expect("write task");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["kind"], json!("task_failure"));
        assert_eq!(response["error"]["code"], json!("task_failure"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classifies_stone_parse_error() {
        let root = temp_dir("parse-failure");
        let task_path = task_with_source(&root, "parse-failure", "if true print('oops')");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["kind"], json!("stone_parse_error"));
        assert_eq!(response["error"]["code"], json!("stone_parse_error"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classifies_stone_lower_error() {
        let root = temp_dir("lower-failure");
        let task_path = task_with_source(&root, "lower-failure", "f(*args)");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["kind"], json!("stone_lower_error"));
        assert_eq!(response["error"]["code"], json!("stone_script_unsupported"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classifies_stone_eval_error() {
        let root = temp_dir("eval-failure");
        let task_path = task_with_source(&root, "eval-failure", "missing()");

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["kind"], json!("stone_eval_error"));
        assert_eq!(response["error"]["code"], json!("stone_script_error"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classifies_command_runtime_error() {
        let root = temp_dir("command-failure");
        let task_path = task_with_source(
            &root,
            "command-failure",
            r#"edit("missing.json", "before", "after")"#,
        );

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let response = guest.task_response_from_path(&task_path);

        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["kind"], json!("command_error"));
        assert_eq!(response["error"]["code"], json!("io_error"));

        let _ = fs::remove_dir_all(root);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = Path::new("/tmp").join(format!("stone-task-{label}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn task_with_source(root: &Path, id: &str, source: &str) -> PathBuf {
        let task_path = root.join("task.json");
        fs::write(
            &task_path,
            json!({
                "version": 0,
                "id": id,
                "runtime": { "frontend": "stone" },
                "script": { "source": source },
                "input": {}
            })
            .to_string(),
        )
        .expect("write task");
        task_path
    }
}
