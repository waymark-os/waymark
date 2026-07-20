// SPDX-License-Identifier: MIT OR Apache-2.0

use serde_json::{json, Value as JsonValue};
use std::fs;

use crate::{
    tools::{HostCapabilityRpc, TaskTools, ToolResult},
    StoneGuest,
};

const DEFAULT_SYSTEM_PROMPT: &str = r#"You are controlling a Waymark in-VM agent.
Return JSON only. Do not use markdown.
Use this shape:
{"actions":[{"tool":"write","input":{"path":"/work/answer.txt","content":"...","mode":"replace"}},{"tool":"read","input":{"path":"/work/answer.txt"}},{"final":{...}}]}
Allowed tools: run, run_linux, read, write, edit, list, find, search, finish.
The run tool executes a persistent Stone language session, not a Linux shell. Its input is {"source":"..."}.
Stone is the control language for first-class attempts. Before branching, call run with source emit(help("attempt_workflow")); use emit(help("attempt_fork")) for exact signatures.
Use attempts when work is uncertain, risky, expensive to repeat, or has multiple plausible strategies. Fork candidates from a stable parent, wait for their reported results, inspect them, accept one, and discard every rejected child.
Use run_linux only for a Linux operation in the current attempt. To execute child work, fork with a recorded Stone program and start it; each child receives its own scoped LibOS channel.
Write generated files under /work unless the task explicitly names another writable output path.
run_linux sees the current attempt workspace at /app.
If you write a file, read it back before final.
Before final, ensure no child attempts remain active. Return finish for the current root; the outer controller reports and publishes it.
Use {"tool":"finish","input":{...}} or {"final":{...}} when the task is complete.
You may return one or more tool actions without a final value. After tool observations, return the next tool actions or finish."#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentAction {
    Tool(JsonValue),
    Final(JsonValue),
}

impl AgentAction {
    pub fn from_json(value: &JsonValue) -> Result<Self, AgentError> {
        let object = value.as_object().ok_or_else(|| AgentError {
            code: "invalid_action",
            message: "agent action must be an object".to_owned(),
        })?;

        let tool = object.get("tool").and_then(JsonValue::as_str);
        if tool == Some("finish") {
            return Ok(Self::Final(
                object
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| json!({"ok": true})),
            ));
        }

        let has_tool = object.contains_key("tool");
        let final_value = object.get("final");
        match (has_tool, final_value) {
            (true, None) => Ok(Self::Tool(value.clone())),
            (false, Some(value)) => Ok(Self::Final(value.clone())),
            (true, Some(_)) => Err(AgentError {
                code: "invalid_action",
                message: "agent action must not contain both tool and final".to_owned(),
            }),
            (false, None) => Err(AgentError {
                code: "invalid_action",
                message: "agent action requires tool or final".to_owned(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentRunResult {
    pub ok: bool,
    pub final_value: Option<JsonValue>,
    pub rounds: usize,
    pub turns: usize,
    pub trace: Vec<JsonValue>,
    pub error: Option<AgentError>,
}

pub struct AgentSession {
    tools: TaskTools,
    max_rounds: usize,
    max_turns: usize,
    trace: Vec<JsonValue>,
    next_seq: u64,
    completion_path: Option<String>,
}

/// User-space control policy executed inside one attempt.
///
/// Implementations may be optimized Rust builtins or adapters around another
/// control. They receive the same [`AgentSession`] resource surface; Gateway
/// authority remains in the tools and model gateway rather than in the
/// control implementation.
pub trait AgentControl {
    fn name(&self) -> &'static str;

    fn run(
        &mut self,
        session: &mut AgentSession,
        guest: &mut StoneGuest,
        gateway: Option<&mut dyn AgentModelGateway>,
    ) -> AgentRunResult;
}

/// The optimized builtin JSON-action ReAct control.
pub struct ReactAgentControl {
    task: String,
    model: Option<String>,
}

impl ReactAgentControl {
    pub fn new(task: impl Into<String>, model: Option<&str>) -> Self {
        Self {
            task: task.into(),
            model: model.map(str::to_owned),
        }
    }
}

/// A deterministic control useful for fixtures and pre-authored action lists.
pub struct ScriptedAgentControl {
    actions: Vec<AgentAction>,
}

impl ScriptedAgentControl {
    pub fn new(actions: impl IntoIterator<Item = AgentAction>) -> Self {
        Self {
            actions: actions.into_iter().collect(),
        }
    }
}

pub trait AgentModelGateway {
    fn request_model(&mut self, request: &JsonValue) -> Result<JsonValue, AgentError>;

    fn request_workspace_rpc(&mut self, _request: &JsonValue) -> Result<JsonValue, String> {
        Err("workspace RPC is not available for this model gateway".to_owned())
    }

    fn request_linux_rpc(&mut self, _request: &JsonValue) -> Result<JsonValue, String> {
        Err("linux RPC is not available for this model gateway".to_owned())
    }
}

struct AgentHostCapabilityRpc<'a>(&'a mut dyn AgentModelGateway);

impl HostCapabilityRpc for AgentHostCapabilityRpc<'_> {
    fn request_workspace(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
        self.0.request_workspace_rpc(request)
    }

    fn request_linux(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
        self.0.request_linux_rpc(request)
    }
}

impl AgentSession {
    pub fn new(tools: TaskTools) -> Self {
        Self {
            tools,
            max_rounds: 16,
            max_turns: 16,
            trace: Vec::new(),
            next_seq: 0,
            completion_path: None,
        }
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self.max_rounds = max_turns;
        self
    }

    pub fn with_max_rounds(mut self, max_rounds: usize) -> Self {
        self.max_rounds = max_rounds;
        self
    }

    pub fn with_completion_path(mut self, completion_path: Option<String>) -> Self {
        self.completion_path = completion_path;
        self
    }

    pub fn tools(&self) -> &TaskTools {
        &self.tools
    }

    pub fn trace(&self) -> &[JsonValue] {
        &self.trace
    }

    pub fn max_rounds(&self) -> usize {
        self.max_rounds
    }

    pub fn max_turns(&self) -> usize {
        self.max_turns
    }

    pub fn record_event(&mut self, event: &str, value: JsonValue) {
        self.push_event(event, value);
    }

    pub fn run_control(
        &mut self,
        guest: &mut StoneGuest,
        control: &mut dyn AgentControl,
        gateway: Option<&mut dyn AgentModelGateway>,
    ) -> AgentRunResult {
        self.trace.clear();
        self.next_seq = 0;
        self.push_event(
            "episode_start",
            json!({
                "control": control.name(),
            }),
        );
        control.run(self, guest, gateway)
    }

    pub fn run_model_task(
        &mut self,
        guest: &mut StoneGuest,
        task: &str,
        model: Option<&str>,
        gateway: &mut dyn AgentModelGateway,
    ) -> AgentRunResult {
        let mut control = ReactAgentControl::new(task, model);
        self.run_control(guest, &mut control, Some(gateway))
    }

    fn run_react_after_episode_start(
        &mut self,
        guest: &mut StoneGuest,
        task: &str,
        model: Option<&str>,
        gateway: &mut dyn AgentModelGateway,
    ) -> AgentRunResult {
        let mut messages = initial_model_messages(task);
        let mut rounds = 0;
        let mut turns = 0;
        loop {
            if rounds >= self.max_rounds {
                return self.round_limit_result(rounds, turns);
            }
            rounds += 1;
            let request = model_request_from_messages(messages.clone(), model);
            self.push_event("model_request", redacted_model_request(&request));
            let response = match gateway.request_model(&request) {
                Ok(response) => response,
                Err(error) => return self.finish_with_error(rounds, turns, error),
            };
            self.push_event("model_response", model_response_trace(&response));

            let text = match model_text(&response) {
                Ok(text) => text.to_owned(),
                Err(error) => return self.finish_with_error(rounds, turns, error),
            };
            messages.push(json!({
                "role": "assistant",
                "content": text.clone(),
            }));

            let actions = match parse_model_actions(&text) {
                Ok(actions) => actions,
                Err(error) => return self.finish_with_error(rounds, turns, error),
            };
            if actions.is_empty() {
                return self.finish_with_error(
                    rounds,
                    turns,
                    AgentError {
                        code: "invalid_model_response",
                        message: "model actions array must not be empty".to_owned(),
                    },
                );
            }

            let mut saw_tool = false;
            for action in actions {
                if turns >= self.max_turns {
                    return self.turn_limit_result(rounds, turns);
                }

                turns += 1;
                match action {
                    AgentAction::Tool(call) => {
                        saw_tool = true;
                        self.push_event("tool_call", call.clone());
                        let result = {
                            let mut host_rpc = AgentHostCapabilityRpc(gateway);
                            self.tools
                                .invoke_json_with_host_rpc(guest, &call, Some(&mut host_rpc))
                        };
                        let mut result_json = result.to_json();
                        terminate_active_linux_run(&mut result_json, gateway);
                        self.push_event("tool_result", result_json.clone());
                        messages.push(tool_observation_message(&call, &result_json));
                        if result_json.get("ok") == Some(&JsonValue::Bool(true)) {
                            if let Some(path) = self.completed_path() {
                                let value = json!({
                                    "answer_path": path,
                                    "auto_final": true,
                                });
                                self.push_event("agent_final", value.clone());
                                self.push_event(
                                    "episode_end",
                                    json!({
                                        "ok": true,
                                        "auto_final": true,
                                    }),
                                );
                                return AgentRunResult {
                                    ok: true,
                                    final_value: Some(value),
                                    rounds,
                                    turns,
                                    trace: self.trace.clone(),
                                    error: None,
                                };
                            }
                        }
                    }
                    AgentAction::Final(value) => {
                        self.push_event("agent_final", value.clone());
                        self.push_event(
                            "episode_end",
                            json!({
                                "ok": true,
                            }),
                        );
                        return AgentRunResult {
                            ok: true,
                            final_value: Some(value),
                            rounds,
                            turns,
                            trace: self.trace.clone(),
                            error: None,
                        };
                    }
                }
            }

            if !saw_tool {
                return self.finish_with_error(
                    rounds,
                    turns,
                    AgentError {
                        code: "final_missing",
                        message: "model action stream ended without a final value or tool call"
                            .to_owned(),
                    },
                );
            }
        }
    }

    pub fn run_scripted(
        &mut self,
        guest: &mut StoneGuest,
        actions: impl IntoIterator<Item = AgentAction>,
    ) -> AgentRunResult {
        let mut control = ScriptedAgentControl::new(actions);
        self.run_control(guest, &mut control, None)
    }

    fn run_actions_after_episode_start(
        &mut self,
        guest: &mut StoneGuest,
        actions: impl IntoIterator<Item = AgentAction>,
    ) -> AgentRunResult {
        let mut turns = 0;
        for action in actions {
            if turns >= self.max_turns {
                return self.turn_limit_result(0, turns);
            }

            turns += 1;
            match action {
                AgentAction::Tool(call) => {
                    self.push_event("tool_call", call.clone());
                    let result = self.tools.invoke_json(guest, &call);
                    let result_json = result.to_json();
                    self.push_event("tool_result", result_json.clone());
                    if !result.ok {
                        return self.finish_with_error(0, turns, agent_error_from_tool(&result));
                    }
                }
                AgentAction::Final(value) => {
                    self.push_event("agent_final", value.clone());
                    self.push_event(
                        "episode_end",
                        json!({
                            "ok": true,
                        }),
                    );
                    return AgentRunResult {
                        ok: true,
                        final_value: Some(value),
                        rounds: 0,
                        turns,
                        trace: self.trace.clone(),
                        error: None,
                    };
                }
            }
        }

        let error = AgentError {
            code: "final_missing",
            message: "agent action stream ended without a final value".to_owned(),
        };
        self.push_event(
            "episode_end",
            json!({
                "ok": false,
                "error": {
                    "code": error.code,
                    "message": error.message,
                },
            }),
        );
        AgentRunResult {
            ok: false,
            final_value: None,
            rounds: 0,
            turns,
            trace: self.trace.clone(),
            error: Some(error),
        }
    }

    fn round_limit_result(&mut self, rounds: usize, turns: usize) -> AgentRunResult {
        self.finish_with_error(
            rounds,
            turns,
            AgentError {
                code: "round_limit_exceeded",
                message: format!("agent exceeded max_rounds {}", self.max_rounds),
            },
        )
    }

    fn turn_limit_result(&mut self, rounds: usize, turns: usize) -> AgentRunResult {
        self.finish_with_error(
            rounds,
            turns,
            AgentError {
                code: "turn_limit_exceeded",
                message: format!("agent exceeded max_turns {}", self.max_turns),
            },
        )
    }

    fn completed_path(&self) -> Option<&str> {
        let path = self.completion_path.as_deref()?;
        fs::metadata(path).is_ok().then_some(path)
    }

    fn finish_with_error(
        &mut self,
        rounds: usize,
        turns: usize,
        error: AgentError,
    ) -> AgentRunResult {
        self.push_event(
            "episode_end",
            json!({
                "ok": false,
                "error": {
                    "code": error.code,
                    "message": error.message,
                },
            }),
        );
        AgentRunResult {
            ok: false,
            final_value: None,
            rounds,
            turns,
            trace: self.trace.clone(),
            error: Some(error),
        }
    }

    pub fn run_scripted_json(
        &mut self,
        guest: &mut StoneGuest,
        actions: &JsonValue,
    ) -> AgentRunResult {
        let Some(actions) = actions.as_array() else {
            let error = AgentError {
                code: "invalid_actions",
                message: "agent.actions must be an array".to_owned(),
            };
            return AgentRunResult {
                ok: false,
                final_value: None,
                rounds: 0,
                turns: 0,
                trace: vec![json!({
                    "seq": 0,
                    "event": "episode_end",
                    "value": {
                        "ok": false,
                        "error": {
                            "code": error.code,
                            "message": error.message,
                        },
                    },
                })],
                error: Some(error),
            };
        };

        let parsed = actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                AgentAction::from_json(action).map_err(|mut error| {
                    error.message = format!("agent.actions[{index}]: {}", error.message);
                    error
                })
            })
            .collect::<Result<Vec<_>, _>>();

        match parsed {
            Ok(actions) => self.run_scripted(guest, actions),
            Err(error) => AgentRunResult {
                ok: false,
                final_value: None,
                rounds: 0,
                turns: 0,
                trace: vec![json!({
                    "seq": 0,
                    "event": "episode_end",
                    "value": {
                        "ok": false,
                        "error": {
                            "code": error.code,
                            "message": error.message,
                        },
                    },
                })],
                error: Some(error),
            },
        }
    }

    fn push_event(&mut self, event: &str, value: JsonValue) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.trace.push(json!({
            "seq": seq,
            "event": event,
            "value": value,
        }));
    }
}

impl AgentControl for ReactAgentControl {
    fn name(&self) -> &'static str {
        "react_json_v0"
    }

    fn run(
        &mut self,
        session: &mut AgentSession,
        guest: &mut StoneGuest,
        gateway: Option<&mut dyn AgentModelGateway>,
    ) -> AgentRunResult {
        let Some(gateway) = gateway else {
            return session.finish_with_error(
                0,
                0,
                AgentError {
                    code: "model_gateway_unavailable",
                    message: "react control requires a model gateway".to_owned(),
                },
            );
        };
        session.run_react_after_episode_start(guest, &self.task, self.model.as_deref(), gateway)
    }
}

impl AgentControl for ScriptedAgentControl {
    fn name(&self) -> &'static str {
        "scripted_v0"
    }

    fn run(
        &mut self,
        session: &mut AgentSession,
        guest: &mut StoneGuest,
        _gateway: Option<&mut dyn AgentModelGateway>,
    ) -> AgentRunResult {
        session.run_actions_after_episode_start(guest, self.actions.drain(..))
    }
}

pub fn model_request(task: &str, model: Option<&str>) -> JsonValue {
    model_request_from_messages(initial_model_messages(task), model)
}

fn initial_model_messages(task: &str) -> Vec<JsonValue> {
    vec![
        json!({
            "role": "system",
            "content": DEFAULT_SYSTEM_PROMPT,
        }),
        json!({
            "role": "user",
            "content": task,
        }),
    ]
}

fn model_request_from_messages(messages: Vec<JsonValue>, model: Option<&str>) -> JsonValue {
    let mut request = json!({
        "messages": messages,
        "temperature": 0,
    });
    if let Some(model) = model {
        if !model.is_empty() {
            request["model"] = json!(model);
        }
    }
    request
}

fn tool_observation_message(call: &JsonValue, result: &JsonValue) -> JsonValue {
    let observation = json!({
        "tool_call": call,
        "tool_result": result,
    });
    let content = serde_json::to_string(&json!({
        "observation": observation,
        "instruction": "Use this observation to choose the next tool action or final. If tool_result.ok is false, correct the error if possible.",
    }))
    .unwrap_or_else(|_| "Observation unavailable. Continue.".to_owned());
    json!({
        "role": "user",
        "content": content,
    })
}

fn terminate_active_linux_run(result: &mut JsonValue, gateway: &mut dyn AgentModelGateway) {
    let value = result.get("value").unwrap_or(&JsonValue::Null);
    if value.get("still_running").and_then(JsonValue::as_bool) != Some(true) {
        return;
    }
    let Some(run_id) = value.get("run_id").and_then(JsonValue::as_str) else {
        return;
    };
    let cleanup = gateway
        .request_linux_rpc(&json!({"op": "terminate", "run_id": run_id}))
        .unwrap_or_else(|error| {
            json!({
                "ok": false,
                "kind": "linux_terminate_failed",
                "error": {"message": error},
            })
        });
    if let Some(fields) = result.as_object_mut() {
        fields.insert("active_run_cleanup".to_owned(), cleanup);
    }
}

pub fn parse_model_actions(content: &str) -> Result<Vec<AgentAction>, AgentError> {
    let decoded = parse_model_json(content)?;
    if decoded.get("tool").and_then(JsonValue::as_str) == Some("finish")
        && decoded.get("actions").is_none()
    {
        return AgentAction::from_json(&decoded).map(|action| vec![action]);
    }
    if decoded.get("final").is_some() && decoded.get("actions").is_none() {
        return AgentAction::from_json(&decoded).map(|action| vec![action]);
    }
    let actions = decoded.get("actions").ok_or_else(|| AgentError {
        code: "invalid_model_response",
        message: "model JSON requires actions array".to_owned(),
    })?;
    let Some(actions) = actions.as_array() else {
        return Err(AgentError {
            code: "invalid_model_response",
            message: "model JSON actions must be an array".to_owned(),
        });
    };

    actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            AgentAction::from_json(action).map_err(|mut error| {
                error.message = format!("model actions[{index}]: {}", error.message);
                error
            })
        })
        .collect()
}

fn parse_model_json(content: &str) -> Result<JsonValue, AgentError> {
    let mut stripped = content.trim();
    if stripped.starts_with("```") {
        let mut lines = stripped.lines().collect::<Vec<_>>();
        if lines.first().is_some_and(|line| line.starts_with("```")) {
            lines.remove(0);
        }
        if lines.last().is_some_and(|line| line.starts_with("```")) {
            lines.pop();
        }
        let joined = lines.join("\n");
        return parse_model_json(&joined);
    }
    if let Some(start) = stripped.find('{') {
        stripped = &stripped[start..];
    }
    if let Some(end) = stripped.rfind('}') {
        stripped = &stripped[..=end];
    }
    serde_json::from_str(stripped).map_err(|err| AgentError {
        code: "invalid_model_response",
        message: format!("model did not return valid JSON: {err}"),
    })
}

fn model_text(response: &JsonValue) -> Result<&str, AgentError> {
    response
        .get("text")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| AgentError {
            code: "invalid_model_response",
            message: "model response requires text".to_owned(),
        })
}

fn redacted_model_request(request: &JsonValue) -> JsonValue {
    json!({
        "message_count": request
            .get("messages")
            .and_then(JsonValue::as_array)
            .map_or(0, Vec::len),
        "model": request.get("model").cloned().unwrap_or(JsonValue::Null),
    })
}

fn model_response_trace(response: &JsonValue) -> JsonValue {
    let text_len = response
        .get("text")
        .and_then(JsonValue::as_str)
        .map_or(0, str::len);
    json!({
        "ok": response.get("ok").cloned().unwrap_or(JsonValue::Null),
        "text_len": text_len,
    })
}

fn agent_error_from_tool(result: &ToolResult) -> AgentError {
    match &result.error {
        Some(error) => AgentError {
            code: "tool_failed",
            message: format!("tool failed with {}: {}", error.code, error.message),
        },
        None => AgentError {
            code: "tool_failed",
            message: "tool failed without an error payload".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::TaskTools;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn scripted_agent_uses_stone_and_file_tools() {
        let root = temp_root("scripted-agent");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut agent = AgentSession::new(TaskTools::for_host_root(&root));

        let result = agent.run_scripted(
            &mut guest,
            [
                AgentAction::Tool(json!({
                    "tool": "run",
                    "input": {
                        "source": "emit({\"path\": \"/work/answer.txt\", \"content\": \"hello\", \"ok\": True})"
                    }
                })),
                AgentAction::Tool(json!({
                    "tool": "write",
                    "input": {
                        "path": "/work/answer.txt",
                        "content": "hello",
                        "mode": "replace"
                    }
                })),
                AgentAction::Tool(json!({
                    "tool": "read",
                    "input": {
                        "path": "/work/answer.txt"
                    }
                })),
                AgentAction::Final(json!({
                    "answer": "hello"
                })),
            ],
        );

        assert!(result.ok);
        assert_eq!(result.final_value, Some(json!({"answer": "hello"})));
        assert_eq!(
            fs::read_to_string(root.join("work/answer.txt")).unwrap(),
            "hello"
        );
        assert!(result.trace.iter().any(|entry| {
            entry["event"] == json!("tool_result")
                && entry["value"]["value"]
                    == json!({"path": "/work/answer.txt", "content": "hello", "ok": true})
        }));
        assert!(result.trace.iter().any(|entry| {
            entry["event"] == json!("tool_result")
                && entry["value"]["value"]["content"] == json!("hello")
        }));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scripted_agent_stops_on_tool_error() {
        let root = temp_root("scripted-agent-error");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut agent = AgentSession::new(TaskTools::for_host_root(&root));

        let result = agent.run_scripted(
            &mut guest,
            [
                AgentAction::Tool(json!({
                    "tool": "read",
                    "input": {
                        "path": "/tmp/scratch.txt"
                    }
                })),
                AgentAction::Final(json!({"unreachable": true})),
            ],
        );

        assert!(!result.ok);
        assert_eq!(result.error.unwrap().code, "tool_failed");
        assert_eq!(result.final_value, None);
        assert!(result
            .trace
            .iter()
            .any(|entry| entry["event"] == json!("episode_end")
                && entry["value"]["ok"] == json!(false)));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn agent_controls_are_stackable_over_one_session_contract() {
        struct EventWrapper<C> {
            inner: C,
        }

        impl<C: AgentControl> AgentControl for EventWrapper<C> {
            fn name(&self) -> &'static str {
                "event_wrapper_v0"
            }

            fn run(
                &mut self,
                session: &mut AgentSession,
                guest: &mut StoneGuest,
                gateway: Option<&mut dyn AgentModelGateway>,
            ) -> AgentRunResult {
                session.record_event("control_enter", json!({"inner": self.inner.name()}));
                let mut result = self.inner.run(session, guest, gateway);
                session.record_event("control_exit", json!({"ok": result.ok}));
                result.trace = session.trace().to_vec();
                result
            }
        }

        let root = temp_root("stacked-agent-control");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut session = AgentSession::new(TaskTools::for_host_root(&root));
        let inner = ScriptedAgentControl::new([AgentAction::Final(json!({"answer": "ok"}))]);
        let mut control = EventWrapper { inner };

        let result = session.run_control(&mut guest, &mut control, None);

        assert!(result.ok);
        assert_eq!(result.final_value, Some(json!({"answer": "ok"})));
        assert_eq!(result.trace[0]["event"], json!("episode_start"));
        assert_eq!(
            result.trace[0]["value"]["control"],
            json!("event_wrapper_v0")
        );
        assert!(result.trace.iter().any(|entry| {
            entry["event"] == json!("control_enter")
                && entry["value"]["inner"] == json!("scripted_v0")
        }));
        assert_eq!(result.trace.last().unwrap()["event"], json!("control_exit"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_model_actions_from_json_object() {
        let actions = parse_model_actions(
            r#"{"actions":[{"tool":"read","input":{"path":"/work/a.txt"}},{"final":{"ok":true}}]}"#,
        )
        .expect("actions");

        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], AgentAction::Tool(_)));
        assert!(matches!(actions[1], AgentAction::Final(_)));
    }

    #[test]
    fn parses_model_actions_from_fenced_json() {
        let actions =
            parse_model_actions("```json\n{\"actions\":[{\"final\":{\"answer\":\"done\"}}]}\n```")
                .expect("actions");

        assert_eq!(actions, vec![AgentAction::Final(json!({"answer": "done"}))]);
    }

    #[test]
    fn parses_top_level_model_final() {
        let actions = parse_model_actions(r#"{"final":{"answer":"done"}}"#).expect("actions");

        assert_eq!(actions, vec![AgentAction::Final(json!({"answer": "done"}))]);
    }

    #[test]
    fn parses_finish_tool_as_final_action() {
        let actions = parse_model_actions(
            r#"{"actions":[{"tool":"finish","input":{"answer":"done","answer_path":"/work/out.txt"}}]}"#,
        )
        .expect("actions");

        assert_eq!(
            actions,
            vec![AgentAction::Final(json!({
                "answer": "done",
                "answer_path": "/work/out.txt",
            }))]
        );
    }

    #[test]
    fn builtin_agent_terminates_active_linux_run_handles() {
        struct CleanupGateway {
            requests: Vec<JsonValue>,
        }

        impl AgentModelGateway for CleanupGateway {
            fn request_model(&mut self, _request: &JsonValue) -> Result<JsonValue, AgentError> {
                unreachable!("cleanup test does not call the model")
            }

            fn request_linux_rpc(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
                self.requests.push(request.clone());
                Ok(json!({
                    "ok": true,
                    "kind": "terminated",
                    "value": {"run_id": "run-7", "still_running": false},
                }))
            }
        }

        let mut gateway = CleanupGateway { requests: vec![] };
        let mut result = json!({
            "ok": false,
            "kind": "linux_exec_timeout",
            "value": {"run_id": "run-7", "still_running": true},
        });
        terminate_active_linux_run(&mut result, &mut gateway);

        assert_eq!(
            gateway.requests,
            vec![json!({"op": "terminate", "run_id": "run-7"})]
        );
        assert_eq!(result["active_run_cleanup"]["ok"], json!(true));
        assert_eq!(
            result["active_run_cleanup"]["value"]["still_running"],
            json!(false)
        );
    }

    #[test]
    fn parses_top_level_finish_tool_as_final_action() {
        let actions =
            parse_model_actions(r#"{"tool":"finish","input":{"answer":"done"}}"#).expect("actions");

        assert_eq!(actions, vec![AgentAction::Final(json!({"answer": "done"}))]);
    }

    #[test]
    fn builds_model_request_in_guest_agent_shape() {
        let request = model_request("write hello", Some("local-model"));

        assert_eq!(request["model"], json!("local-model"));
        let system = request["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("persistent Stone language session"));
        assert!(system.contains("help(\"attempt_workflow\")"));
        assert!(system.contains("discard every rejected child"));
        assert_eq!(request["messages"][1]["role"], json!("user"));
        assert_eq!(request["messages"][1]["content"], json!("write hello"));
    }

    #[test]
    fn model_agent_loops_with_tool_observation_feedback() {
        let root = temp_root("react-agent");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut agent = AgentSession::new(TaskTools::for_host_root(&root)).with_max_turns(4);
        let mut gateway = SequenceGateway {
            responses: vec![
                r#"{"actions":[{"tool":"write","input":{"path":"/work/answer.txt","content":"hello react","mode":"replace"}}]}"#.to_owned(),
                r#"{"actions":[{"tool":"read","input":{"path":"/work/answer.txt"}}]}"#.to_owned(),
                r#"{"actions":[{"final":{"answer":"hello react","answer_path":"/work/answer.txt"}}]}"#.to_owned(),
            ],
            requests: Vec::new(),
        };

        let result = agent.run_model_task(
            &mut guest,
            "Create /work/answer.txt containing hello react.",
            Some("fixture-model"),
            &mut gateway,
        );

        assert!(result.ok);
        assert_eq!(
            result.final_value,
            Some(json!({
                "answer": "hello react",
                "answer_path": "/work/answer.txt",
            }))
        );
        assert_eq!(
            fs::read_to_string(root.join("work/answer.txt")).unwrap(),
            "hello react"
        );
        assert_eq!(gateway.requests.len(), 3);
        assert_eq!(result.rounds, 3);
        assert_eq!(result.turns, 3);
        assert_eq!(gateway.requests[0]["messages"].as_array().unwrap().len(), 2);
        assert!(gateway.requests[1]["messages"][3]["content"]
            .as_str()
            .unwrap()
            .contains("\"tool_result\""));
        assert!(gateway.requests[2]["messages"][5]["content"]
            .as_str()
            .unwrap()
            .contains("hello react"));
        assert_eq!(
            result
                .trace
                .iter()
                .filter(|entry| entry["event"] == json!("model_request"))
                .count(),
            3
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn model_agent_counts_multi_action_response_as_one_round() {
        let root = temp_root("react-agent-one-round");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut agent = AgentSession::new(TaskTools::for_host_root(&root))
            .with_max_turns(4)
            .with_max_rounds(1);
        let mut gateway = SequenceGateway {
            responses: vec![
                r#"{"actions":[{"tool":"write","input":{"path":"/work/answer.txt","content":"same round","mode":"replace"}},{"tool":"read","input":{"path":"/work/answer.txt"}},{"final":{"answer":"same round","answer_path":"/work/answer.txt"}}]}"#.to_owned(),
            ],
            requests: Vec::new(),
        };

        let result = agent.run_model_task(
            &mut guest,
            "Create /work/answer.txt containing same round.",
            Some("fixture-model"),
            &mut gateway,
        );

        assert!(result.ok);
        assert_eq!(result.rounds, 1);
        assert_eq!(result.turns, 3);
        assert_eq!(gateway.requests.len(), 1);
        assert_eq!(
            fs::read_to_string(root.join("work/answer.txt")).unwrap(),
            "same round"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn model_agent_stops_at_round_limit() {
        let root = temp_root("react-agent-round-limit");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut agent = AgentSession::new(TaskTools::for_host_root(&root))
            .with_max_turns(4)
            .with_max_rounds(1);
        let mut gateway = SequenceGateway {
            responses: vec![
                r#"{"actions":[{"tool":"write","input":{"path":"/work/answer.txt","content":"needs another round","mode":"replace"}}]}"#.to_owned(),
                r#"{"actions":[{"final":{"answer":"unreachable"}}]}"#.to_owned(),
            ],
            requests: Vec::new(),
        };

        let result = agent.run_model_task(
            &mut guest,
            "Create /work/answer.txt and then report final.",
            Some("fixture-model"),
            &mut gateway,
        );

        assert!(!result.ok);
        assert_eq!(result.error.unwrap().code, "round_limit_exceeded");
        assert_eq!(result.rounds, 1);
        assert_eq!(result.turns, 1);
        assert_eq!(gateway.requests.len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn model_agent_can_recover_from_tool_error_observation() {
        let root = temp_root("react-agent-recover");
        fs::create_dir_all(root.join("work")).unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut agent = AgentSession::new(TaskTools::for_host_root(&root)).with_max_turns(4);
        let mut gateway = SequenceGateway {
            responses: vec![
                r#"{"actions":[{"tool":"read","input":{"path":"/work/missing.txt"}}]}"#.to_owned(),
                r#"{"actions":[{"tool":"write","input":{"path":"/work/answer.txt","content":"recovered","mode":"replace"}}]}"#.to_owned(),
                r#"{"actions":[{"final":{"answer":"recovered","answer_path":"/work/answer.txt"}}]}"#.to_owned(),
            ],
            requests: Vec::new(),
        };

        let result = agent.run_model_task(
            &mut guest,
            "Create /work/answer.txt containing recovered.",
            Some("fixture-model"),
            &mut gateway,
        );

        assert!(result.ok);
        assert_eq!(
            result.final_value,
            Some(json!({
                "answer": "recovered",
                "answer_path": "/work/answer.txt",
            }))
        );
        assert_eq!(
            fs::read_to_string(root.join("work/answer.txt")).unwrap(),
            "recovered"
        );
        assert_eq!(gateway.requests.len(), 3);
        let first_observation = gateway.requests[1]["messages"][3]["content"]
            .as_str()
            .unwrap();
        assert!(first_observation.contains("\"ok\":false"));
        assert!(first_observation.contains("correct the error"));
        assert!(result.trace.iter().any(|entry| {
            entry["event"] == json!("tool_result")
                && entry["value"]["ok"] == json!(false)
                && entry["value"]["error"]["code"] == json!("not_found")
        }));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn model_agent_can_use_stone_sum_over_open_lines() {
        let root = temp_root("react-sum-agent");
        fs::create_dir_all(root.join("work")).unwrap();
        fs::write(root.join("work/numbers.txt"), "10\n20\n-3\n").unwrap();
        let mut guest = StoneGuest::new(root.join("work")).unwrap();
        let mut agent = AgentSession::new(TaskTools::for_host_root(&root)).with_max_turns(6);
        let mut gateway = SequenceGateway {
            responses: vec![
                r#"{"actions":[{"tool":"run","input":{"source":"emit({\"sum\": sum(int(line) for line in open(\"numbers.txt\"))})"}}]}"#.to_owned(),
                r#"{"actions":[{"tool":"write","input":{"path":"/work/answer.txt","content":"27","mode":"replace"}}]}"#.to_owned(),
                r#"{"actions":[{"tool":"read","input":{"path":"/work/answer.txt"}}]}"#.to_owned(),
                r#"{"actions":[{"final":{"answer":27,"answer_path":"/work/answer.txt"}}]}"#.to_owned(),
            ],
            requests: Vec::new(),
        };

        let result = agent.run_model_task(
            &mut guest,
            "Calculate the sum of numbers in /work/numbers.txt and write it to /work/answer.txt.",
            Some("fixture-model"),
            &mut gateway,
        );

        assert!(result.ok);
        assert_eq!(
            result.final_value,
            Some(json!({
                "answer": 27,
                "answer_path": "/work/answer.txt",
            }))
        );
        assert_eq!(
            fs::read_to_string(root.join("work/answer.txt")).unwrap(),
            "27"
        );
        assert!(result.trace.iter().any(|entry| {
            entry["event"] == json!("tool_result") && entry["value"]["value"] == json!({"sum": 27})
        }));
        assert_eq!(gateway.requests.len(), 4);

        let _ = fs::remove_dir_all(root);
    }

    struct SequenceGateway {
        responses: Vec<String>,
        requests: Vec<JsonValue>,
    }

    impl AgentModelGateway for SequenceGateway {
        fn request_model(&mut self, request: &JsonValue) -> Result<JsonValue, AgentError> {
            self.requests.push(request.clone());
            if self.responses.is_empty() {
                return Err(AgentError {
                    code: "fixture_exhausted",
                    message: "fixture gateway has no response left".to_owned(),
                });
            }
            Ok(json!({
                "ok": true,
                "text": self.responses.remove(0),
            }))
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("waymark-agent-{name}-{nanos}"))
    }
}
