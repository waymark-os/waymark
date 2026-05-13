// SPDX-License-Identifier: MIT OR Apache-2.0

#![cfg(not(target_os = "hermit"))]

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};

use crate::stone_ast::{FunctionDef, Stmt};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StoneHelperHook {
    pub(crate) event: String,
    pub(crate) family: String,
    pub(crate) argv0: Vec<String>,
    pub(crate) argv0_prefix: Vec<String>,
    pub(crate) handler: StoneHelperHandler,
    pub(crate) priority: i64,
    pub(crate) source: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StoneHelperHandler {
    pub(crate) name: String,
    pub(crate) kind: StoneHelperHandlerKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StoneHelperHandlerKind {
    StoneFunction {
        function: FunctionDef,
        functions: HashMap<String, FunctionDef>,
    },
    Registered,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StoneHelperRegistry {
    pub(crate) hooks: Vec<StoneHelperHook>,
    family_by_argv0: HashMap<String, String>,
    family_prefix_matchers: Vec<StoneHelperFamilyMatcher>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoneHelperFamilyMatcher {
    family: String,
    argv0: Vec<String>,
    argv0_prefix: Vec<String>,
    priority: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct StoneRunEvent<'a> {
    pub(crate) event: &'static str,
    pub(crate) family: String,
    pub(crate) argv: &'a [String],
    pub(crate) cwd: &'a Path,
    pub(crate) env_overrides: &'a [(String, String)],
    pub(crate) ok: bool,
    pub(crate) exit_code: Option<i64>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
    pub(crate) duration_ms: i64,
    pub(crate) explanation_kind: Option<String>,
    pub(crate) explanation: Option<Value>,
}

pub(crate) fn stone_run_event_from_record<'a>(
    record: &Record,
    argv: &'a [String],
    cwd: &'a Path,
    env_overrides: &'a [(String, String)],
    registry: &StoneHelperRegistry,
) -> StoneRunEvent<'a> {
    let ok = record_bool(record, "ok").unwrap_or(false);
    let timed_out = record_bool(record, "timed_out").unwrap_or(false);
    let event = if timed_out {
        "run.after_timeout"
    } else if ok {
        "run.after_success"
    } else {
        "run.after_failure"
    };
    StoneRunEvent {
        event,
        family: registry.command_family(argv),
        argv,
        cwd,
        env_overrides,
        ok,
        exit_code: record_i64(record, "exit_code"),
        stdout: record_string(record, "stdout").unwrap_or_default(),
        stderr: record_string(record, "stderr").unwrap_or_default(),
        timed_out,
        duration_ms: record_i64(record, "duration_ms").unwrap_or_default(),
        explanation_kind: record_explanation_kind(record),
        explanation: record.get("explanation").cloned(),
    }
}

impl StoneHelperRegistry {
    fn new(hooks: Vec<StoneHelperHook>) -> Self {
        let mut exact_matchers: Vec<StoneHelperFamilyMatcher> = hooks
            .iter()
            .filter(|hook| hook.family != "generic" && !hook.argv0.is_empty())
            .map(|hook| StoneHelperFamilyMatcher {
                family: hook.family.clone(),
                argv0: hook.argv0.clone(),
                argv0_prefix: Vec::new(),
                priority: hook.priority,
            })
            .collect();
        exact_matchers.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.family.cmp(&right.family))
        });
        let mut family_by_argv0 = HashMap::new();
        for matcher in exact_matchers {
            for argv0 in matcher.argv0 {
                family_by_argv0
                    .entry(argv0)
                    .or_insert(matcher.family.clone());
            }
        }

        let mut family_prefix_matchers: Vec<StoneHelperFamilyMatcher> = hooks
            .iter()
            .filter(|hook| hook.family != "generic" && !hook.argv0_prefix.is_empty())
            .map(|hook| StoneHelperFamilyMatcher {
                family: hook.family.clone(),
                argv0: Vec::new(),
                argv0_prefix: hook.argv0_prefix.clone(),
                priority: hook.priority,
            })
            .collect();
        family_prefix_matchers.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| right.max_prefix_len().cmp(&left.max_prefix_len()))
                .then_with(|| left.family.cmp(&right.family))
        });
        family_prefix_matchers.dedup();
        Self {
            hooks,
            family_by_argv0,
            family_prefix_matchers,
        }
    }

    pub(crate) fn command_family(&self, argv: &[String]) -> String {
        let argv0 = command_argv0(argv);
        if let Some(family) = self.family_by_argv0.get(&argv0) {
            return family.clone();
        }
        self.family_prefix_matchers
            .iter()
            .find(|matcher| matcher.matches_argv0(&argv0))
            .map(|matcher| matcher.family.clone())
            .unwrap_or_else(|| "generic".to_owned())
    }

    pub(crate) fn matching_hooks<'a>(
        &'a self,
        event: &StoneRunEvent<'_>,
    ) -> Vec<&'a StoneHelperHook> {
        let argv0 = command_argv0(event.argv);
        let mut hooks: Vec<&StoneHelperHook> = self
            .hooks
            .iter()
            .filter(|hook| {
                let family_matches = hook.family == event.family
                    || event.family.starts_with(&format!("{}/", hook.family))
                    || hook.family == "generic";
                let argv0_matches = (hook.argv0.is_empty() && hook.argv0_prefix.is_empty())
                    || hook.argv0.iter().any(|expected| expected == &argv0)
                    || hook
                        .argv0_prefix
                        .iter()
                        .any(|prefix| argv0.starts_with(prefix));
                hook.event == event.event && family_matches && argv0_matches
            })
            .collect();
        hooks.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.handler.name.cmp(&right.handler.name))
        });
        hooks
    }
}

impl StoneHelperFamilyMatcher {
    fn matches_argv0(&self, argv0: &str) -> bool {
        self.argv0.iter().any(|expected| expected == argv0)
            || self
                .argv0_prefix
                .iter()
                .any(|prefix| argv0.starts_with(prefix))
    }

    fn max_prefix_len(&self) -> usize {
        self.argv0_prefix.iter().map(String::len).max().unwrap_or(0)
    }
}

impl StoneHelperHandler {
    fn resolve(name: String, functions: &HashMap<String, FunctionDef>) -> Self {
        let kind = resolve_stone_helper_function(&name, functions)
            .map(|function| StoneHelperHandlerKind::StoneFunction {
                function,
                functions: functions.clone(),
            })
            .unwrap_or(StoneHelperHandlerKind::Registered);
        Self { name, kind }
    }
}

fn resolve_stone_helper_function(
    handler: &str,
    functions: &HashMap<String, FunctionDef>,
) -> Option<FunctionDef> {
    functions
        .get(handler)
        .cloned()
        .or_else(|| functions.get(&handler.replace('.', "_")).cloned())
}

pub(crate) fn stone_helper_registry(cwd: &Path) -> StoneHelperRegistry {
    let mut hooks = Vec::new();
    for dir in stone_helper_dirs(cwd) {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| matches!(path.extension().and_then(|ext| ext.to_str()), Some("stone")))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(source) = fs::read_to_string(&path) else {
                continue;
            };
            let functions = stone_helper_functions(&source);
            hooks.extend(parse_stone_helper_hooks(&source, &path, &functions));
        }
    }
    StoneHelperRegistry::new(hooks)
}

fn stone_helper_functions(source: &str) -> HashMap<String, FunctionDef> {
    let Ok(program) = crate::stone_ast::lower_source(source) else {
        return HashMap::new();
    };
    program
        .statements
        .into_iter()
        .filter_map(|statement| match statement {
            Stmt::FunctionDef(function) => Some((function.name.clone(), function)),
            _ => None,
        })
        .collect()
}

fn stone_helper_dirs(cwd: &Path) -> Vec<PathBuf> {
    if let Some(raw) = env::var_os("WAYMARK_STONE_HELPER_DIRS") {
        return env::split_paths(&raw).collect();
    }
    let mut dirs = Vec::new();
    dirs.push(cwd.join(".stone").join("helpers"));
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".stone").join("helpers"));
    }
    dirs.push(PathBuf::from("/usr/share/waymark/stone/helpers"));
    dirs
}

fn parse_stone_helper_hooks(
    source: &str,
    path: &Path,
    functions: &HashMap<String, FunctionDef>,
) -> Vec<StoneHelperHook> {
    let mut hooks = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("hook(") {
            continue;
        }
        let Some(event) = parse_hook_event(trimmed) else {
            continue;
        };
        let Some(family) = parse_named_string_arg(trimmed, "family") else {
            continue;
        };
        let Some(handler) = parse_named_string_arg(trimmed, "handler") else {
            continue;
        };
        let priority = parse_named_i64_arg(trimmed, "priority").unwrap_or(100);
        hooks.push(StoneHelperHook {
            event,
            family,
            argv0: parse_named_string_list_arg(trimmed, "argv0"),
            argv0_prefix: parse_named_string_list_arg(trimmed, "argv0_prefix"),
            handler: StoneHelperHandler::resolve(handler, functions),
            priority,
            source: path.to_path_buf(),
        });
    }
    hooks
}

fn parse_hook_event(line: &str) -> Option<String> {
    let rest = line.trim_start_matches("hook(").trim_start();
    parse_quoted_prefix(rest).map(|(value, _)| value)
}

fn parse_named_string_arg(line: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=");
    let index = line.find(&marker)?;
    parse_quoted_prefix(line[index + marker.len()..].trim_start()).map(|(value, _)| value)
}

fn parse_named_string_list_arg(line: &str, name: &str) -> Vec<String> {
    let marker = format!("{name}=");
    let Some(index) = line.find(&marker) else {
        return Vec::new();
    };
    let mut rest = line[index + marker.len()..].trim_start();
    if !rest.starts_with('[') {
        return Vec::new();
    }
    rest = &rest[1..];
    let mut values = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with(']') || rest.is_empty() {
            break;
        }
        let Some((value, next)) = parse_quoted_prefix(rest) else {
            break;
        };
        values.push(value);
        rest = next.trim_start();
        if rest.starts_with(',') {
            rest = &rest[1..];
        }
    }
    values
}

fn parse_named_i64_arg(line: &str, name: &str) -> Option<i64> {
    let marker = format!("{name}=");
    let index = line.find(&marker)?;
    let rest = line[index + marker.len()..].trim_start();
    let end = rest
        .find(|ch: char| !ch.is_ascii_digit() && ch != '-')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn parse_quoted_prefix(rest: &str) -> Option<(String, &str)> {
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let stripped = &rest[quote.len_utf8()..];
    let end = stripped.find(quote)?;
    Some((
        stripped[..end].to_owned(),
        &stripped[end + quote.len_utf8()..],
    ))
}

fn command_argv0(argv: &[String]) -> String {
    let Some(first) = argv.first() else {
        return String::new();
    };
    Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first)
        .to_owned()
}

fn helper_observation(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    kind: impl Into<String>,
    summary: impl Into<String>,
    evidence: Record,
    next_checks: Vec<Vec<String>>,
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("helper", Value::string(hook.handler.name.clone(), span));
    record.push("event", Value::string(event.event.to_owned(), span));
    record.push("family", Value::string(event.family.clone(), span));
    record.push("kind", Value::string(kind.into(), span));
    record.push("summary", Value::string(summary.into(), span));
    if let Some(exit_code) = event.exit_code {
        record.push("exit_code", Value::int(exit_code, span));
    }
    record.push(
        "source",
        Value::string(hook.source.display().to_string(), span),
    );
    record.push("evidence", Value::record(evidence, span));
    record.push(
        "next_checks",
        Value::list(
            next_checks
                .into_iter()
                .map(|argv| {
                    Value::list(
                        argv.into_iter()
                            .map(|arg| Value::string(arg, span))
                            .collect(),
                        span,
                    )
                })
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

pub(crate) fn helper_error_observation(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    err: ShellError,
    span: Span,
) -> Value {
    let mut evidence = Record::new();
    evidence.push("error", Value::string(err.to_string(), span));
    evidence.push(
        "source",
        Value::string(hook.source.display().to_string(), span),
    );
    helper_observation(
        hook,
        event,
        "helper_error",
        format!(
            "Helper `{}` failed while handling {}.",
            hook.handler.name, hook.event
        ),
        evidence,
        Vec::<Vec<String>>::new(),
        span,
    )
}

pub(crate) fn stone_run_event_value(event: &StoneRunEvent<'_>, span: Span) -> Value {
    let mut record = Record::new();
    record.push("event", Value::string(event.event.to_owned(), span));
    record.push("family", Value::string(event.family.clone(), span));
    record.push(
        "argv",
        Value::list(
            event
                .argv
                .iter()
                .map(|arg| Value::string(arg.clone(), span))
                .collect(),
            span,
        ),
    );
    record.push("cwd", Value::string(event.cwd.display().to_string(), span));
    record.push(
        "env",
        Value::record(
            event
                .env_overrides
                .iter()
                .map(|(key, value)| (key.clone(), Value::string(value.clone(), span)))
                .collect(),
            span,
        ),
    );
    record.push("ok", Value::bool(event.ok, span));
    record.push(
        "exit_code",
        event
            .exit_code
            .map(|code| Value::int(code, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push("stdout", Value::string(event.stdout.clone(), span));
    record.push("stderr", Value::string(event.stderr.clone(), span));
    record.push("timed_out", Value::bool(event.timed_out, span));
    record.push("duration_ms", Value::int(event.duration_ms, span));
    record.push(
        "explanation_kind",
        event
            .explanation_kind
            .as_ref()
            .map(|kind| Value::string(kind.clone(), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "explanation",
        event
            .explanation
            .clone()
            .unwrap_or_else(|| Value::nothing(span)),
    );
    Value::record(record, span)
}

pub(crate) fn stone_helper_observations_from_value(value: Value) -> Result<Vec<Value>, ShellError> {
    match value {
        Value::Nothing { .. } => Ok(Vec::new()),
        Value::List { vals, .. } => Ok(vals),
        Value::Record { .. } => Ok(vec![value]),
        other => Err(stone_helper_error(
            "helper",
            format!(
                "helper callback must return a record, list of records, or None; got {}",
                other.get_type()
            ),
        )),
    }
}

pub(crate) fn attach_service_helper_observation(
    record: &mut Record,
    event: &'static str,
    handler: &str,
    summary: &str,
    next_checks: &[&str],
    span: Span,
) {
    let mut evidence = Record::new();
    evidence.push(
        "ok",
        Value::bool(record_bool(record, "ok").unwrap_or(false), span),
    );
    if let Some(pid) = record_i64(record, "pid") {
        evidence.push("pid", Value::int(pid, span));
    }
    if let Some(port) = record_i64(record, "port") {
        evidence.push("port", Value::int(port, span));
    }
    let hook = StoneHelperHook {
        event: event.to_owned(),
        family: "service".to_owned(),
        argv0: Vec::new(),
        argv0_prefix: Vec::new(),
        handler: StoneHelperHandler::resolve(handler.to_owned(), &HashMap::new()),
        priority: 100,
        source: PathBuf::from("<builtin-service-lifecycle>"),
    };
    let synthetic_event = StoneRunEvent {
        event,
        family: "service".to_owned(),
        argv: &[],
        cwd: Path::new("."),
        env_overrides: &[],
        ok: record_bool(record, "ok").unwrap_or(false),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: event == "run.after_timeout",
        duration_ms: record_i64(record, "duration_ms").unwrap_or_default(),
        explanation_kind: None,
        explanation: None,
    };
    let observation = helper_observation(
        &hook,
        &synthetic_event,
        "service_lifecycle_probe",
        summary,
        evidence,
        next_checks
            .iter()
            .map(|check| vec![(*check).to_owned()])
            .collect(),
        span,
    );
    record.push("helpers", Value::list(vec![observation], span));
}

fn record_bool(record: &Record, field: &str) -> Option<bool> {
    match record.get(field) {
        Some(Value::Bool { val, .. }) => Some(*val),
        _ => None,
    }
}

fn record_i64(record: &Record, field: &str) -> Option<i64> {
    match record.get(field) {
        Some(Value::Int { val, .. }) => Some(*val),
        _ => None,
    }
}

fn record_string(record: &Record, field: &str) -> Option<String> {
    match record.get(field) {
        Some(Value::String { val, .. }) | Some(Value::Glob { val, .. }) => Some(val.clone()),
        _ => None,
    }
}

fn record_explanation_kind(record: &Record) -> Option<String> {
    let Some(Value::Record { val, .. }) = record.get("explanation") else {
        return None;
    };
    match val.get("kind") {
        Some(Value::String { val, .. }) | Some(Value::Glob { val, .. }) => Some(val.clone()),
        _ => None,
    }
}

fn stone_helper_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}
