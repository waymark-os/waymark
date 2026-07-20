// SPDX-License-Identifier: MIT OR Apache-2.0

#[cfg(target_os = "hermit")]
use std::ffi::CString;
#[cfg(not(target_os = "hermit"))]
use std::fs;
use std::path::{Path, PathBuf};
use std::{env, io};

use nu_protocol::{
    engine::{EngineState, Stack},
    shell_error::generic::GenericError,
    IntoPipelineData, PipelineData, ShellError, Span, Value,
};
use serde_json::Value as JsonValue;

pub mod agent;
mod commands;
mod frontend;
mod gateway_env;
mod gateway_runtime;
mod json;
mod linux_tools;
mod server;
mod stone_agent_control;
mod stone_ast;
mod stone_attempt_scope;
mod stone_builtins;
mod stone_eval;
mod stone_file_ops;
mod stone_frontend;
mod stone_hash;
mod stone_helpers;
mod stone_vm;
#[cfg(test)]
mod stone_vm_tests;
mod task;
pub mod tools;
mod vsock;

pub use frontend::{Frontend, StoneFrontend};
pub use server::{run_task_server, run_task_server_stream};
pub use stone_ast::{
    lower_source as lower_stone_source, Call as StoneCall, Expr as StoneExpr,
    Program as StoneProgram, Stmt as StoneStmt,
};
pub use stone_frontend::parse_stone_source;
pub use vsock::run_vsock_task_server;

pub struct StoneGuest {
    engine_state: EngineState,
    stack: Stack,
    work_dir: PathBuf,
    task_scope: TaskScope,
    stone_session: stone_eval::StoneSession,
}

const LAST_RESULT_ENV: &str = "WAYMARK_LAST_RESULT_JSON";
const MAX_LAST_RESULT_JSON_BYTES: usize = 1024 * 1024;

impl StoneGuest {
    pub fn new(start_dir: PathBuf) -> Result<Self, ShellError> {
        let engine_state = EngineState::new();

        let mut stack = Stack::new();
        seed_environment(&engine_state, &mut stack, &start_dir)?;

        Ok(Self {
            engine_state,
            stack,
            work_dir: start_dir,
            task_scope: TaskScope::default(),
            stone_session: stone_eval::StoneSession::default(),
        })
    }

    pub fn evaluate(&mut self, source: &str) -> Result<PipelineData, ShellError> {
        self.evaluate_with_input(source, PipelineData::empty())
    }

    pub fn evaluate_with_input(
        &mut self,
        source: &str,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        self.evaluate_stone_with_input(source, input)
    }

    pub fn evaluate_stone(&mut self, source: &str) -> Result<PipelineData, ShellError> {
        self.evaluate_stone_with_input(source, PipelineData::empty())
    }

    pub fn evaluate_stone_with_input(
        &mut self,
        source: &str,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        self.evaluate_with_frontend(FrontendKind::Stone, source, input)
    }

    pub fn run_command(&mut self, source: &str) -> i32 {
        self.run_command_with_input(source, PipelineData::empty())
    }

    pub fn run_command_with_input(&mut self, source: &str, input: PipelineData) -> i32 {
        self.run_stone_command_with_input(source, input)
    }

    pub fn run_stone_command_with_input(&mut self, source: &str, input: PipelineData) -> i32 {
        self.run_command_with_frontend(FrontendKind::Stone, source, input)
    }

    pub fn run_command_with_frontend(
        &mut self,
        frontend: FrontendKind,
        source: &str,
        input: PipelineData,
    ) -> i32 {
        let response = self.command_response_with_frontend(frontend, source, input);
        let ok = response
            .get("ok")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let encoded = serde_json::to_string(&response).unwrap_or_else(|err| {
            format!(
                "{{\"ok\":false,\"error\":{{\"message\":\"failed to encode response\",\"debug\":{:?}}}}}",
                err.to_string()
            )
        });

        if ok {
            println!("{encoded}");
            0
        } else {
            eprintln!("{encoded}");
            1
        }
    }

    pub fn command_response(&mut self, source: &str) -> JsonValue {
        self.command_response_with_input(source, PipelineData::empty())
    }

    pub fn command_response_with_input(&mut self, source: &str, input: PipelineData) -> JsonValue {
        self.stone_command_response_with_input(source, input)
    }

    pub fn stone_command_response(&mut self, source: &str) -> JsonValue {
        self.stone_command_response_with_input(source, PipelineData::empty())
    }

    pub fn stone_command_response_with_input(
        &mut self,
        source: &str,
        input: PipelineData,
    ) -> JsonValue {
        self.command_response_with_frontend(FrontendKind::Stone, source, input)
    }

    pub fn command_response_with_frontend(
        &mut self,
        frontend: FrontendKind,
        source: &str,
        input: PipelineData,
    ) -> JsonValue {
        let response = if frontend == FrontendKind::Stone {
            match stone_frontend::eval_stone_source_with_output_and_session(
                &self.engine_state,
                &mut self.stack,
                source,
                input,
                &mut self.stone_session,
            ) {
                Ok(output) => {
                    match json::pipeline_to_json_value(output.pipeline, Span::unknown()) {
                        Ok(value) => {
                            let mut response = json::success_response_with_output(
                                value,
                                self.current_cwd(),
                                output.stdout,
                                String::new(),
                            );
                            let has_diagnostics = output
                                .diagnostics
                                .as_object()
                                .map(|fields| !fields.is_empty())
                                .unwrap_or(true);
                            if has_diagnostics {
                                response["diagnostics"] = output.diagnostics;
                            }
                            response
                        }
                        Err(err) => json::error_response(&err, Some(self.current_cwd())),
                    }
                }
                Err(err) => json::error_response(&err, Some(self.current_cwd())),
            }
        } else {
            match self.execute_json(frontend, source, input) {
                Ok(value) => json::success_response(value, self.current_cwd()),
                Err(err) => json::error_response(&err, Some(self.current_cwd())),
            }
        };
        self.remember_last_response(&response);
        response
    }

    pub fn reset_work_dir(&mut self) -> io::Result<()> {
        if self.work_dir.as_os_str().is_empty() || self.work_dir == Path::new("/") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "refusing to reset invalid work dir {}",
                    self.work_dir.display()
                ),
            ));
        }

        #[cfg(target_os = "hermit")]
        {
            let path = CString::new(self.work_dir.to_string_lossy().as_bytes())
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()))?;
            // SAFETY: `path` is a live, NUL-terminated CString for the duration of the syscall.
            // Hermit copies/reads the path synchronously and returns before `path` is dropped.
            let status = unsafe { hermit_abi::reset_mount(path.as_ptr()) };
            if status < 0 {
                return Err(io::Error::from_raw_os_error(-status));
            }
            self.stack
                .set_cwd(&self.work_dir)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
            self.stone_session = stone_eval::StoneSession::default();
            return Ok(());
        }

        #[cfg(not(target_os = "hermit"))]
        {
            match fs::remove_dir_all(&self.work_dir) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            fs::create_dir_all(&self.work_dir)?;
            env::set_current_dir(&self.work_dir)?;
            self.stack
                .set_cwd(&self.work_dir)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
            self.stone_session = stone_eval::StoneSession::default();
            Ok(())
        }
    }

    pub fn reset_task_state(&mut self) -> io::Result<TaskScopeSnapshot> {
        self.task_scope.reset()
    }

    pub fn task_scope_snapshot(&self) -> TaskScopeSnapshot {
        self.task_scope.snapshot()
    }

    pub fn debug_register_live_task_resource(&mut self, name: impl Into<String>) {
        self.task_scope.register_live(name);
    }

    pub fn debug_register_completed_task_resource(&mut self, name: impl Into<String>) {
        self.task_scope.register_completed(name);
    }

    pub fn debug_clear_task_resources(&mut self) {
        self.task_scope.force_clear();
    }

    fn remember_last_response(&mut self, response: &JsonValue) {
        let mut encoded = serde_json::to_string(response).unwrap_or_else(|err| {
            serde_json::json!({
                "ok": false,
                "error": {
                    "kind": "runtime_error",
                    "code": "last_result_encode_failed",
                    "message": err.to_string(),
                }
            })
            .to_string()
        });
        if encoded.len() > MAX_LAST_RESULT_JSON_BYTES {
            encoded = serde_json::json!({
                "ok": false,
                "error": {
                    "kind": "resource_limit",
                    "code": "last_result_too_large",
                    "message": "previous result exceeded the last_result storage limit",
                    "max_bytes": MAX_LAST_RESULT_JSON_BYTES,
                    "actual_bytes": encoded.len(),
                }
            })
            .to_string();
        }
        self.stack.add_env_var(
            LAST_RESULT_ENV.into(),
            Value::string(encoded, Span::unknown()),
        );
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskScopeSnapshot {
    pub live: Vec<String>,
    pub completed: Vec<String>,
}

#[derive(Debug, Default)]
struct TaskScope {
    live: Vec<String>,
    completed: Vec<String>,
}

impl TaskScope {
    fn register_live(&mut self, name: impl Into<String>) {
        self.live.push(name.into());
    }

    fn register_completed(&mut self, name: impl Into<String>) {
        self.completed.push(name.into());
    }

    fn reset(&mut self) -> io::Result<TaskScopeSnapshot> {
        let before = self.snapshot();
        if !before.live.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "task-owned resources still live at reset: {}",
                    before.live.join(", ")
                ),
            ));
        }

        self.completed.clear();
        Ok(before)
    }

    fn force_clear(&mut self) {
        self.live.clear();
        self.completed.clear();
    }

    fn snapshot(&self) -> TaskScopeSnapshot {
        TaskScopeSnapshot {
            live: self.live.clone(),
            completed: self.completed.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontendKind {
    Stone,
    Python,
}

fn seed_environment(
    engine_state: &EngineState,
    stack: &mut Stack,
    start_dir: &Path,
) -> Result<(), ShellError> {
    stack.set_cwd(start_dir)?;
    add_env_var(
        stack,
        "HOME",
        env_var_string("HOME").unwrap_or_else(|| start_dir.display().to_string()),
    );
    add_env_var(
        stack,
        "TMPDIR",
        env_var_string("TMPDIR").unwrap_or_else(|| "/tmp".to_string()),
    );
    add_env_var(
        stack,
        "XDG_RUNTIME_DIR",
        env_var_string("XDG_RUNTIME_DIR").unwrap_or_else(|| "/run".to_string()),
    );
    if let Some(path) = env_var_string("PATH") {
        add_env_var(stack, "PATH", path);
    }
    if let Some(oldpwd) = env_var_string("OLDPWD") {
        add_env_var(stack, "OLDPWD", oldpwd);
    } else {
        add_env_var(stack, "OLDPWD", engine_state.cwd_as_string(Some(stack))?);
    }

    Ok(())
}

fn add_env_var(stack: &mut Stack, name: &str, value: String) {
    stack.add_env_var(name.into(), Value::string(value, Span::unknown()));
}

fn env_var_string(name: &str) -> Option<String> {
    env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

impl StoneGuest {
    fn evaluate_with_frontend(
        &mut self,
        frontend: FrontendKind,
        source: &str,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        match frontend {
            FrontendKind::Stone => stone_frontend::eval_stone_source_with_output_and_session(
                &self.engine_state,
                &mut self.stack,
                source,
                input,
                &mut self.stone_session,
            )
            .map(|output| output.pipeline),
            FrontendKind::Python => Err(ShellError::Generic(
                GenericError::new_internal(
                    "unsupported command frontend",
                    "python is only supported by the task runner",
                )
                .with_code("stone_frontend_unsupported"),
            )),
        }
    }

    fn execute_json(
        &mut self,
        frontend: FrontendKind,
        source: &str,
        input: PipelineData,
    ) -> Result<JsonValue, ShellError> {
        let pipeline = self.evaluate_with_frontend(frontend, source, input)?;

        json::pipeline_to_json_value(pipeline, Span::unknown())
    }

    pub(crate) fn current_cwd(&self) -> String {
        self.engine_state
            .cwd_as_string(Some(&self.stack))
            .unwrap_or_else(|_| "/".to_string())
    }
}

pub fn pipeline_input_from_bytes(bytes: Vec<u8>) -> PipelineData {
    let span = Span::unknown();
    if bytes.is_empty() {
        return PipelineData::empty();
    }

    if let Ok(value) = json::parse_json_bytes(&bytes, span) {
        return value.into_pipeline_data();
    }

    match String::from_utf8(bytes) {
        Ok(text) => Value::string(text, span).into_pipeline_data(),
        Err(err) => Value::binary(err.into_bytes(), span).into_pipeline_data(),
    }
}

#[cfg(test)]
mod tests {
    use super::{pipeline_input_from_bytes, run_vsock_task_server, StoneGuest};
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use nu_protocol::{ListStream, PipelineData, Record, ShellError, Signals, Span, Value};

    #[test]
    fn vsock_task_server_reports_unsupported_in_default_build() -> Result<(), ShellError> {
        let start_dir = test_root("vsock-unsupported");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let err = run_vsock_task_server(&mut guest, 9975).expect_err("vsock is not built in");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(err
            .to_string()
            .contains("vsock task server is not included"));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn pwd_uses_configured_start_dir() -> Result<(), ShellError> {
        let start_dir = test_root("pwd");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let output = pipeline_to_string(guest.evaluate("pwd()")?)?;
        assert_eq!(output.trim(), start_dir.display().to_string());

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn echo_and_save_roundtrip() -> Result<(), ShellError> {
        let start_dir = test_root("roundtrip");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        assert_eq!(guest.run_command(r#"save("hello", "note.txt")"#), 0);

        let output = pipeline_to_string(guest.evaluate(r#"cat("note.txt")"#)?)?;
        assert_eq!(output, "hello");

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn edit_replaces_exact_file_text() -> Result<(), ShellError> {
        let start_dir = test_root("edit-command");
        fs::create_dir_all(&start_dir).expect("create start dir");
        fs::write(start_dir.join("answer.txt"), "hello world").expect("write file");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.stone_command_response(
            r#"result = edit("answer.txt", "hello", "goodbye")
emit({"content": cat("answer.txt"), "replacements": result["replacements"]})"#,
        );
        assert_eq!(response["ok"], json!(true));
        assert_eq!(
            response["value"],
            json!({
                "content": "goodbye world",
                "replacements": 1
            })
        );

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn find_and_search_are_stone_native() -> Result<(), ShellError> {
        let start_dir = test_root("find-search-jsonl");
        fs::create_dir_all(start_dir.join("nested")).expect("create nested dir");
        fs::write(start_dir.join("answer.txt"), "alpha\nneedle here\n").expect("write answer");
        fs::write(start_dir.join("nested/notes.txt"), "no match\n").expect("write notes");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let find_response =
            guest.stone_command_response(r#"find(".", name_contains="answer", name_glob="*.txt")"#);
        assert_eq!(find_response["ok"], json!(true));
        assert_eq!(find_response["value"][0]["name"], json!("answer.txt"));
        assert_eq!(find_response["value"][0]["type"], json!("file"));

        let search_response = guest.stone_command_response(r#"search(".", "needle")"#);
        assert_eq!(search_response["ok"], json!(true));
        let record = &search_response["value"][0];
        assert_eq!(record["line"], json!(2));
        assert_eq!(record["text"], json!("needle here"));
        assert!(record["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("answer.txt")));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn command_response_is_json_by_default() -> Result<(), ShellError> {
        let start_dir = test_root("response");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.command_response("pwd()");
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["cwd"], json!(start_dir.display().to_string()));
        assert_eq!(response["value"], json!(start_dir.display().to_string()));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_command_response_uses_python_syntax_frontend() -> Result<(), ShellError> {
        let start_dir = test_root("stone-response");
        fs::create_dir_all(&start_dir).expect("create start dir");
        fs::write(start_dir.join("a.txt"), "a").expect("write file");
        fs::create_dir(start_dir.join("dir")).expect("create dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.stone_command_response(
            r#"names = []
for item in ls("."):
    if item["type"] == "file":
        names.append(item["name"])
emit(names)"#,
        );
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!(["a.txt"]));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn emit_returns_explicit_structured_value() -> Result<(), ShellError> {
        let start_dir = test_root("emit");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.stone_command_response(r#"emit({"status": "ok", "count": 2})"#);
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!({"status": "ok", "count": 2}));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn help_returns_topic_records_and_unknown_topic_feedback() -> Result<(), ShellError> {
        let start_dir = test_root("help-topics");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let overview = guest.command_response("help()");
        assert_eq!(overview["ok"], json!(true));
        assert!(overview["value"]["builtins"].is_array());
        assert!(overview["value"]["syntax"].is_array());

        let topic = guest.command_response(r#"help("emit")"#);
        assert_eq!(topic["ok"], json!(true));
        assert_eq!(topic["value"]["found"], json!(true));
        assert_eq!(topic["value"]["name"], json!("emit"));

        let missing = guest.command_response(r#"help("no_such_topic")"#);
        assert_eq!(missing["ok"], json!(true));
        assert_eq!(missing["value"]["found"], json!(false));
        assert!(missing["value"]["available"].is_array());

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn mkdir_and_rm_handle_files_and_directories() -> Result<(), ShellError> {
        let start_dir = test_root("mkdir-rm");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let mkdir = guest.command_response(r#"mkdir("nested", "nested/deeper")"#);
        assert_eq!(mkdir["ok"], json!(true));
        assert!(start_dir.join("nested/deeper").is_dir());

        fs::write(start_dir.join("nested/file.txt"), "remove me").expect("write file");
        let rm_file = guest.command_response(r#"rm("nested/file.txt")"#);
        assert_eq!(rm_file["ok"], json!(true));
        assert!(!start_dir.join("nested/file.txt").exists());

        let rm_dir = guest.command_response(r#"rm("nested")"#);
        assert_eq!(rm_dir["ok"], json!(true));
        assert!(!start_dir.join("nested").exists());

        let missing_arg = guest.command_response("mkdir()");
        assert_eq!(missing_arg["ok"], json!(false));
        assert_eq!(missing_arg["error"]["code"], json!("stone_script_error"));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn to_json_and_to_jsonl_serialize_structured_values() -> Result<(), ShellError> {
        let start_dir = test_root("json-serializers");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let json_text =
            guest.command_response(r#"to_json(json_loads('{"name":"Ada","count":2}'))"#);
        assert_eq!(json_text["ok"], json!(true));
        assert_eq!(json_text["value"], json!(r#"{"count":2,"name":"Ada"}"#));

        let jsonl_text =
            guest.command_response(r#"to_jsonl(json_loads('[{"name":"Ada"},{"name":"Grace"}]'))"#);
        assert_eq!(jsonl_text["ok"], json!(true));
        assert_eq!(
            jsonl_text["value"],
            json!("{\"name\":\"Ada\"}\n{\"name\":\"Grace\"}\n")
        );

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn state_returns_agent_runtime_snapshot() -> Result<(), ShellError> {
        let start_dir = test_root("state");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.stone_command_response(r#"emit(state())"#);
        assert_eq!(response["ok"], json!(true));
        assert_eq!(
            response["value"]["cwd"],
            json!(start_dir.display().to_string())
        );
        assert!(response["value"]["git"].is_object());
        assert!(response["value"]["tools"]["available"].is_array());

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn last_result_returns_previous_command_response() -> Result<(), ShellError> {
        let start_dir = test_root("last-result");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let first = guest.stone_command_response(r#"emit({"answer": 42})"#);
        assert_eq!(first["ok"], json!(true));

        let second = guest.stone_command_response(r#"emit(last_result())"#);
        assert_eq!(second["ok"], json!(true));
        assert_eq!(second["value"]["ok"], json!(true));
        assert_eq!(second["value"]["value"], json!({"answer": 42}));
        assert_eq!(
            second["value"]["cwd"],
            json!(start_dir.display().to_string())
        );

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_top_level_bindings_persist_across_guest_calls() -> Result<(), ShellError> {
        let start_dir = test_root("stone-session-bindings");
        fs::create_dir_all(&start_dir).expect("create start dir");
        fs::write(start_dir.join("items.csv"), "name,count\na,1\nb,2\n").expect("write csv");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let first = guest.stone_command_response(
            r#"rows = read_csv("items.csv")
total = 0
for row in rows:
    total += int(row["count"])
emit({"loaded": len(rows), "total": total})"#,
        );
        assert_eq!(first["ok"], json!(true));
        assert_eq!(first["value"], json!({"loaded": 2, "total": 3}));
        assert_eq!(
            first["diagnostics"]["session"]["bound"],
            json!(["rows", "total"])
        );

        let second = guest.stone_command_response(
            r#"rows.append({"name": "c", "count": "4"})
emit({"names": map(lambda row: row["name"], rows), "total": total})"#,
        );
        assert_eq!(second["ok"], json!(true));
        assert_eq!(second["value"]["names"], json!(["a", "b", "c"]));
        assert_eq!(second["value"]["total"], json!(3));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_assignment_only_response_reports_bound_names_without_value() -> Result<(), ShellError>
    {
        let start_dir = test_root("stone-session-binding-ack");
        fs::create_dir_all(&start_dir).expect("create start dir");
        fs::write(start_dir.join("input.txt"), "large value").expect("write input");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.stone_command_response(r#"a = read_text("input.txt")"#);
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!(null));
        assert_eq!(response["output"]["stdout"], json!(""));
        assert_eq!(response["diagnostics"]["session"]["bound"], json!(["a"]));

        let reuse = guest.stone_command_response(r#"emit(len(a))"#);
        assert_eq!(reuse["ok"], json!(true));
        assert_eq!(reuse["value"], json!(11));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_session_binding_ack_omits_loop_scratch_assignments() -> Result<(), ShellError> {
        let start_dir = test_root("stone-session-binding-loop-scratch");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.stone_command_response(
            r#"rows = [{"name": "alpha"}, {"name": "beta"}]
matches = []
for row in rows:
    name = row["name"]
    if name.startswith("a"):
        matches.append(row)
emit(len(matches))"#,
        );
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!(1));
        assert_eq!(
            response["diagnostics"]["session"]["bound"],
            json!(["matches", "rows"])
        );

        let reuse = guest.stone_command_response(r#"emit(name)"#);
        assert_eq!(reuse["ok"], json!(true));
        assert_eq!(reuse["value"], json!("beta"));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_file_handles_do_not_persist_across_guest_calls() -> Result<(), ShellError> {
        let start_dir = test_root("stone-session-no-files");
        fs::create_dir_all(&start_dir).expect("create start dir");
        fs::write(start_dir.join("input.txt"), "hello").expect("write input");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let first = guest.stone_command_response(r#"handle = open("input.txt")"#);
        assert_eq!(first["ok"], json!(true));

        let second = guest.stone_command_response(r#"emit(handle)"#);
        assert_eq!(second["ok"], json!(false));
        assert_eq!(second["error"]["code"], json!("stone_script_error"));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_functions_persist_across_guest_calls() -> Result<(), ShellError> {
        let start_dir = test_root("stone-session-functions");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let first = guest.stone_command_response(
            r#"def double(value: int) -> int:
    return value * 2
emit(double(3))"#,
        );
        assert_eq!(first["ok"], json!(true));
        assert_eq!(first["value"], json!(6));

        let second = guest.stone_command_response(r#"emit(double(5))"#);
        assert_eq!(second["ok"], json!(true));
        assert_eq!(second["value"], json!(10));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_agent_controls_are_first_class_persistable_and_wrappable() -> Result<(), ShellError> {
        let start_dir = test_root("stone-session-agent-controls");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let defined = guest.stone_command_response(
            r#"base = scripted_control([{"final": {"answer": "wrapped"}}])
wrapped = lambda session: base(session)"#,
        );
        assert_eq!(defined["ok"], json!(true));
        assert_eq!(
            defined["diagnostics"]["session"]["bound"],
            json!(["base", "wrapped"])
        );

        let result = guest.stone_command_response(
            r#"session = {"task": {"objective": "fixture"}, "input": None}
outcome = wrapped(session)
emit({
    "control_type": type(base),
    "answer": outcome.value.answer,
    "control": outcome.trace[0].value.control,
})"#,
        );
        assert_eq!(result["ok"], json!(true));
        assert_eq!(
            result["value"],
            json!({
                "control_type": "agent_control",
                "answer": "wrapped",
                "control": "scripted_v0",
            })
        );

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_react_control_uses_shared_runtime_contract_without_gateway() -> Result<(), ShellError>
    {
        let start_dir = test_root("stone-react-control-contract");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let result = guest.stone_command_response(
            r#"control = react_control(max_rounds=2, max_turns=3)
outcome = control({"task": {"objective": "fixture"}, "input": None})
emit({
    "ok": outcome.ok,
    "error": outcome.error.code,
    "control": outcome.trace[0].value.control,
})"#,
        );
        assert_eq!(result["ok"], json!(true));
        assert_eq!(
            result["value"],
            json!({
                "ok": false,
                "error": "model_gateway_unavailable",
                "control": "react_json_v0",
            })
        );

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_attempt_scope_is_structured_and_closes_cleanly() -> Result<(), ShellError> {
        let start_dir = test_root("stone-attempt-scope-empty");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let result = guest.stone_command_response(
            r#"scope = attempt_scope(join_timeout_ms=1234)
before = {"type": type(scope), "policy": scope.exit_policy, "children": scope.children}
cleanup = attempt_scope_close(scope)
emit({"before": before, "clean": cleanup.clean, "closed": scope.closed})"#,
        );
        assert_eq!(result["ok"], json!(true));
        assert_eq!(
            result["value"],
            json!({
                "before": {
                    "type": "attempt_scope",
                    "policy": "cancel_then_join",
                    "children": [],
                },
                "clean": true,
                "closed": true,
            })
        );

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_attempt_scope_auto_closes_at_evaluation_boundary() -> Result<(), ShellError> {
        let start_dir = test_root("stone-attempt-scope-auto-close");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let result = guest.stone_command_response(
            r#"scope = attempt_scope()
emit({"scope": scope.id, "closed_during_body": scope.closed})"#,
        );
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["value"]["closed_during_body"], json!(false));
        assert_eq!(result["diagnostics"]["session"]["bound"], json!(null));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_attempt_scope_cleanup_failure_is_related_to_primary_error() -> Result<(), ShellError> {
        let start_dir = test_root("stone-attempt-scope-related-cleanup-error");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let result = guest.stone_command_response(
            r#"scope = attempt_scope()
registered = attempt_scope_add(scope, "missing-child")
fail("primary failure", code="primary_failure")"#,
        );
        assert_eq!(result["ok"], json!(false));
        assert_eq!(
            result["error"]["declared_code"],
            json!("primary_failure"),
            "{result}"
        );
        assert!(result["error"]["related"]
            .as_array()
            .is_some_and(|errors| errors.iter().any(|error| error["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("automatic cancel-then-join cleanup")))));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn fail_returns_intentional_task_failure_error() -> Result<(), ShellError> {
        let start_dir = test_root("fail");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.stone_command_response(
            r#"fail("bad input", code="bad_input", detail={"field": "items"})"#,
        );
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"]["kind"], json!("generic"));
        assert_eq!(response["error"]["code"], json!("task_failure"));
        assert_eq!(response["error"]["declared_code"], json!("bad_input"));
        assert_eq!(response["error"]["detail"], json!("bad input"));
        assert_eq!(response["error"]["help"], json!("code=bad_input"));
        assert!(response["error"]["related"].is_array());

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn open_json_file_returns_structured_value() -> Result<(), ShellError> {
        let start_dir = test_root("open-json");
        fs::create_dir_all(&start_dir).expect("create start dir");
        fs::write(
            start_dir.join("input.json"),
            br#"{"name":"stone","items":[1,true,null]}"#,
        )
        .expect("write json");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.command_response(r#"read_json("input.json")"#);
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"]["name"], json!("stone"));
        assert_eq!(response["value"]["items"], json!([1, true, null]));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn structured_filters_work_on_record_lists() -> Result<(), ShellError> {
        let start_dir = test_root("filters");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.command_response(
            r#"rows = json_loads('[{"name":"b","size":1},{"name":"a","size":2}]')
emit(map(lambda row: row["name"], sort(rows, key="name")))"#,
        );
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!(["a", "b"]));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn where_first_and_last_work_on_structured_values() -> Result<(), ShellError> {
        let start_dir = test_root("where-first-last");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let filtered = guest.command_response(
            r#"rows = json_loads('[{"name":"a","kind":"file"},{"name":"b","kind":"dir"},{"name":"c","kind":"dir"}]')
dirs = where(rows, "kind", "dir")
emit(first(map(lambda row: row["name"], dirs)))"#,
        );
        assert_eq!(filtered["ok"], json!(true));
        assert_eq!(filtered["value"], json!("b"));

        let tail = guest.command_response("last(sort([3, 1, 2]), 2)");
        assert_eq!(tail["ok"], json!(true));
        assert_eq!(tail["value"], json!([2, 3]));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stdin_json_becomes_structured_pipeline_input() -> Result<(), ShellError> {
        let start_dir = test_root("stdin-json");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.command_response_with_input(
            r#"get("name")"#,
            pipeline_input_from_bytes(br#"{"name":"stone","kind":"shell"}"#.to_vec()),
        );
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!("stone"));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stdin_text_falls_back_to_string_input() -> Result<(), ShellError> {
        let start_dir = test_root("stdin-text");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.command_response_with_input(
            r#"text = emit()
save(text, "note.txt")
cat("note.txt")"#,
            pipeline_input_from_bytes(b"hello from stdin".to_vec()),
        );
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!("hello from stdin"));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn get_streams_record_fields_from_list_stream_input() -> Result<(), ShellError> {
        let start_dir = test_root("stream-get");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let span = Span::unknown();
        let input = PipelineData::list_stream(
            ListStream::new(
                vec![
                    record_value([("name", Value::string("a", span))], span),
                    record_value([("name", Value::string("b", span))], span),
                ]
                .into_iter(),
                span,
                Signals::empty(),
            ),
            None,
        );
        let response = guest.command_response_with_input(
            r#"rows = emit()
emit(map(lambda row: row["name"], rows))"#,
            input,
        );
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!(["a", "b"]));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn where_filters_list_stream_input() -> Result<(), ShellError> {
        let start_dir = test_root("stream-where");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let span = Span::unknown();
        let input = PipelineData::list_stream(
            ListStream::new(
                vec![
                    record_value(
                        [
                            ("name", Value::string("a", span)),
                            ("kind", Value::string("file", span)),
                        ],
                        span,
                    ),
                    record_value(
                        [
                            ("name", Value::string("b", span)),
                            ("kind", Value::string("dir", span)),
                        ],
                        span,
                    ),
                ]
                .into_iter(),
                span,
                Signals::empty(),
            ),
            None,
        );
        let response = guest.command_response_with_input(
            r#"rows = emit()
dirs = where(rows, "kind", "dir")
emit(map(lambda row: row["name"], dirs))"#,
            input,
        );
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!(["b"]));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn first_and_last_work_with_list_stream_input() -> Result<(), ShellError> {
        let start_dir = test_root("stream-first-last");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let span = Span::unknown();

        let first_input = PipelineData::list_stream(
            ListStream::new(
                vec![
                    Value::int(1, span),
                    Value::int(2, span),
                    Value::int(3, span),
                ]
                .into_iter(),
                span,
                Signals::empty(),
            ),
            None,
        );
        let first = guest.command_response_with_input(
            r#"values = emit()
first(values)"#,
            first_input,
        );
        assert_eq!(first["ok"], json!(true));
        assert_eq!(first["value"], json!(1));

        let last_input = PipelineData::list_stream(
            ListStream::new(
                vec![
                    Value::int(1, span),
                    Value::int(2, span),
                    Value::int(3, span),
                ]
                .into_iter(),
                span,
                Signals::empty(),
            ),
            None,
        );
        let last = guest.command_response_with_input(
            r#"values = emit()
last(values, 2)"#,
            last_input,
        );
        assert_eq!(last["ok"], json!(true));
        assert_eq!(last["value"], json!([2, 3]));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn get_with_index_still_uses_whole_input_shape() -> Result<(), ShellError> {
        let start_dir = test_root("get-index");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response =
            guest.command_response(r#"json_loads('[{"name":"a"},{"name":"b"}]')[1]["name"]"#);
        assert_eq!(response["ok"], json!(true));
        assert_eq!(response["value"], json!("b"));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn type_errors_have_stable_json_schema() -> Result<(), ShellError> {
        let start_dir = test_root("error-type");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.command_response("from_json(1)");
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"]["kind"], json!("generic"));
        assert_eq!(response["error"]["code"], json!("stone_script_error"));
        assert!(response["error"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("expected string")));

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn parse_errors_have_stable_json_schema() -> Result<(), ShellError> {
        let start_dir = test_root("error-parse");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.command_response("'oops' | first");
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"]["kind"], json!("generic"));
        assert_eq!(response["error"]["code"], json!("stone_script_error"));
        assert!(response["error"]["location"].is_string());

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn io_errors_include_path_and_kind() -> Result<(), ShellError> {
        let start_dir = test_root("error-io");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.command_response(r#"read_file("missing.txt")"#);
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"]["kind"], json!("io"));
        assert_eq!(response["error"]["code"], json!("io_error"));
        assert_eq!(response["error"]["io_kind"], json!("not_found"));
        assert_eq!(
            response["error"]["path"],
            json!(start_dir.join("missing.txt").display().to_string())
        );

        cleanup_dir(&start_dir);
        Ok(())
    }

    #[test]
    fn stone_io_errors_with_unknown_spans_do_not_panic() -> Result<(), ShellError> {
        let start_dir = test_root("error-io-unknown-span");
        fs::create_dir_all(&start_dir).expect("create start dir");

        let mut guest = StoneGuest::new(start_dir.clone())?;
        let response = guest.stone_command_response(r#"ls("/definitely-missing-stone-path")"#);
        assert_eq!(response["ok"], json!(false));
        assert_eq!(response["error"]["kind"], json!("io"));
        assert_eq!(response["error"]["code"], json!("io_error"));
        assert_eq!(response["error"]["io_kind"], json!("not_found"));
        assert_eq!(
            response["error"]["path"],
            json!("/definitely-missing-stone-path")
        );

        cleanup_dir(&start_dir);
        Ok(())
    }

    fn pipeline_to_string(data: PipelineData) -> Result<String, ShellError> {
        Ok(data.into_value(Span::unknown())?.coerce_into_string()?)
    }

    fn record_value<const N: usize>(entries: [(&str, Value); N], span: Span) -> Value {
        let mut record = Record::with_capacity(N);
        for (key, value) in entries {
            record.push(key, value);
        }
        Value::record(record, span)
    }

    fn test_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        Path::new("/tmp").join(format!("waymark-{label}-{nanos}"))
    }

    fn cleanup_dir(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }
}
