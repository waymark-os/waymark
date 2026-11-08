// SPDX-License-Identifier: MIT OR Apache-2.0

//! Typed Stone errors.
//!
//! A caught error is still a plain record (data only). Its `type` field names
//! an error type, and handler matching walks a fixed single-inheritance chain:
//! the builtin table below, then model-defined error classes whose base is
//! recorded on their constructor `FunctionDef`. Matching is a short string
//! walk on the handler path only, so it costs nothing on the normal path.

use nu_protocol::{shell_error::io::ErrorKind, Record, Span, Value};

/// Builtin error types and their parents. The tree follows Python where
/// Python helps, with one deliberate addition: `ToolError` groups every
/// failure that comes from outside the program (files, commands, tools,
/// model calls), so one handler covers recoverable effect failures.
pub(crate) const BUILTIN_ERROR_TYPES: &[(&str, Option<&str>)] = &[
    ("Exception", None),
    ("ToolError", Some("Exception")),
    ("OSError", Some("ToolError")),
    ("FileNotFoundError", Some("OSError")),
    ("FileExistsError", Some("OSError")),
    ("PermissionError", Some("OSError")),
    ("IsADirectoryError", Some("OSError")),
    ("NotADirectoryError", Some("OSError")),
    ("TimeoutError", Some("OSError")),
    ("ExpectationFailed", Some("ToolError")),
    ("UndeclaredEffect", Some("ToolError")),
    ("ModelOutputError", Some("ToolError")),
    ("ValueError", Some("Exception")),
    ("TypeError", Some("Exception")),
    ("LookupError", Some("Exception")),
    ("KeyError", Some("LookupError")),
    ("IndexError", Some("LookupError")),
    ("ArithmeticError", Some("Exception")),
    ("ZeroDivisionError", Some("ArithmeticError")),
    ("NameError", Some("Exception")),
    ("AttributeError", Some("Exception")),
    ("RuntimeError", Some("Exception")),
    ("NotImplementedError", Some("RuntimeError")),
    ("AssertionError", Some("Exception")),
];

/// Python names that do not exist in Stone, with the replacement. Handlers
/// naming them are admission errors rather than handlers that never match.
pub(crate) const REJECTED_ERROR_TYPES: &[(&str, &str)] = &[
    (
        "BaseException",
        "`BaseException` does not exist in Stone: budget exhaustion and policy stops end the attempt and are not catchable. Use `except Exception as error:`",
    ),
    (
        "KeyboardInterrupt",
        "`KeyboardInterrupt` does not exist in Stone: interrupts end the attempt and are not catchable. Use `except Exception as error:` for program errors",
    ),
    (
        "SystemExit",
        "`SystemExit` does not exist in Stone: there is no sys.exit(); return or emit a result instead",
    ),
    (
        "GeneratorExit",
        "`GeneratorExit` does not exist in Stone: there are no generators to close",
    ),
    (
        "IOError",
        "`IOError` is spelled `OSError` in Stone (as in Python 3). Use `except OSError as error:`, or `except ToolError as error:` for any tool, file, command, or model failure",
    ),
    (
        "EnvironmentError",
        "`EnvironmentError` is spelled `OSError` in Stone (as in Python 3). Use `except OSError as error:`, or `except ToolError as error:` for any tool, file, command, or model failure",
    ),
];

/// Error types only the host creates. A program can catch them, test them
/// with `isinstance`, and re-raise one it caught, but it cannot construct,
/// raise, or subclass them. A parent deciding what to do after a child's
/// failure can therefore trust that an `ExpectationFailed` really came from a
/// checked call and not from the child's own code.
pub(crate) const HOST_ONLY_ERROR_TYPES: &[&str] =
    &["ExpectationFailed", "UndeclaredEffect", "ModelOutputError"];

pub(crate) fn is_host_only_error_type(name: &str) -> bool {
    HOST_ONLY_ERROR_TYPES.contains(&name)
}

pub(crate) fn host_only_error_message(name: &str) -> String {
    let made_by = match name {
        "ExpectationFailed" => "a call whose `expect=` check failed (or must_run/edit)",
        "ModelOutputError" => "model.ask() when the model's answers never matched `output=`",
        _ => "the host when a call attempts an effect it did not declare",
    };
    format!(
        "`{name}` is raised only by {made_by}, so programs cannot construct, raise, or subclass it. Re-raise one you caught with `raise`, or define your own type: `class CheckFailed(ToolError):` with fields"
    )
}

/// The tool errors each builtin syscall can raise, for `help()` and for the
/// unreachable-handler warning. `None` from [`builtin_call_raises`] means the
/// call is not described here and the analysis must assume anything.
/// Builtins that never raise a tool error are in [`PURE_BUILTINS`].
const FILE_READ_RAISES: &[&str] = &[
    "FileNotFoundError",
    "PermissionError",
    "IsADirectoryError",
    "NotADirectoryError",
];
const FILE_WRITE_RAISES: &[&str] = &[
    "FileNotFoundError",
    "FileExistsError",
    "PermissionError",
    "IsADirectoryError",
    "NotADirectoryError",
];
const EDIT_RAISES: &[&str] = &[
    "FileNotFoundError",
    "PermissionError",
    "IsADirectoryError",
    "NotADirectoryError",
    "ExpectationFailed",
];
const RUN_RAISES: &[&str] = &["FileNotFoundError", "NotADirectoryError"];
const RUN_EXPECT_RAISES: &[&str] = &[
    "FileNotFoundError",
    "NotADirectoryError",
    "ExpectationFailed",
];
const SERVICE_RAISES: &[&str] = &["ToolError"];

pub(crate) const PURE_BUILTINS: &[&str] = &[
    "abs",
    "all",
    "any",
    "bool",
    "dict",
    "echo",
    "emit",
    "enumerate",
    "fail",
    "first",
    "float",
    "format",
    "from_json",
    "get",
    "help",
    "int",
    "isinstance",
    "items",
    "join",
    "json_dumps",
    "json_loads",
    "keys",
    "last",
    "len",
    "list",
    "max",
    "min",
    "parse_float",
    "parse_int",
    "print",
    "pwd",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "slice",
    "split",
    "starts_with",
    "startswith",
    "str",
    "sum",
    "to_json",
    "to_jsonl",
    "tuple",
    "type",
    "unique",
    "values",
    "zip",
    "Expect",
    "Effects",
];

/// Builtins that take callables; a call is only analyzable when no argument
/// is a bare name (which could be a function doing I/O).
pub(crate) const HIGHER_ORDER_BUILTINS: &[&str] = &[
    "map", "filter", "where", "sort", "sorted", "max", "min", "any", "all",
];

pub(crate) fn builtin_call_raises(name: &str, has_expect: bool) -> Option<&'static [&'static str]> {
    let raises: &'static [&'static str] = match name {
        "cat" | "open" | "read_text" | "read_file" | "read_json" | "read_csv" | "read_jsonl"
        | "stat" | "ls" | "list_dir" | "cd" => FILE_READ_RAISES,
        "write_text" | "write_file" | "write_json" | "write_csv" | "write_jsonl" | "save"
        | "mkdir" | "rm" => FILE_WRITE_RAISES,
        "edit" | "edit_file" => EDIT_RAISES,
        "run" | "run_complete" if has_expect => RUN_EXPECT_RAISES,
        "run" | "run_complete" => RUN_RAISES,
        "must_run" => RUN_EXPECT_RAISES,
        "model_call" | "model_infer" | "start_daemon" | "stop_daemon" | "daemon_status"
        | "wait_port" | "wait_for" => SERVICE_RAISES,
        _ if name.starts_with("attempt")
            || name.starts_with("env_")
            || name.starts_with("semantic_frontier")
            || name.starts_with("context_") =>
        {
            SERVICE_RAISES
        }
        _ if PURE_BUILTINS.contains(&name) || is_builtin_error_type(name) => &[],
        _ => return None,
    };
    Some(raises)
}

/// The `expect=` vocabulary, per kind of call. It is closed: an unknown key is
/// an error that lists these.
pub(crate) const PROCESS_EXPECT_KEYS: &[&str] = &[
    "exit",
    "stdout_contains",
    "stdout_matches",
    "stderr_empty",
    "max_seconds",
];
pub(crate) const EDIT_EXPECT_KEYS: &[&str] = &["occurrences"];

/// Declared effects (tier 3): what a command may do that cannot be undone.
/// Everything else is observed, not declared.
pub(crate) const EFFECT_KEYS: &[&str] = &["network", "irreversible", "background"];
pub(crate) const IRREVERSIBLE_KINDS: &[&str] =
    &["push", "publish", "send", "deploy", "external_write"];
pub(crate) const EXPECT_KEYS: &[&str] = &[
    "exit",
    "stdout_contains",
    "stdout_matches",
    "stderr_empty",
    "max_seconds",
    "occurrences",
];

pub(crate) fn builtin_error_parent(name: &str) -> Option<Option<&'static str>> {
    BUILTIN_ERROR_TYPES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, parent)| *parent)
}

pub(crate) fn is_builtin_error_type(name: &str) -> bool {
    builtin_error_parent(name).is_some()
}

pub(crate) fn rejected_error_type_message(name: &str) -> Option<&'static str> {
    REJECTED_ERROR_TYPES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, message)| *message)
}

pub(crate) fn builtin_error_type_list() -> String {
    BUILTIN_ERROR_TYPES
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether an error of type `actual` is caught by a handler for `handler`.
/// `user_parent` resolves model-defined error classes to their base. An
/// unknown type (for example a class defined in a child attempt that this
/// session never saw) is treated as a direct subclass of `Exception`.
pub(crate) fn error_type_matches(
    actual: &str,
    handler: &str,
    user_parent: impl Fn(&str) -> Option<String>,
) -> bool {
    let mut current = actual.to_string();
    // The chain is acyclic by construction (a class base must already exist
    // when the class is defined), but bound the walk anyway.
    for _ in 0..64 {
        if current == handler {
            return true;
        }
        let parent = match builtin_error_parent(&current) {
            Some(parent) => parent.map(str::to_string),
            None => Some(user_parent(&current).unwrap_or_else(|| "Exception".to_string())),
        };
        match parent {
            Some(parent) => current = parent,
            None => return false,
        }
    }
    false
}

/// Whether a type is `ToolError` or one of its subclasses.
pub(crate) fn is_tool_error_type(
    actual: &str,
    user_parent: impl Fn(&str) -> Option<String>,
) -> bool {
    error_type_matches(actual, "ToolError", user_parent)
}

/// Construct the record for a builtin error type: `ValueError("bad")`.
pub(crate) fn builtin_error_record(type_name: &str, message: String) -> Value {
    let span = Span::unknown();
    let mut record = Record::with_capacity(2);
    record.push("type", Value::string(type_name, span));
    record.push("message", Value::string(message, span));
    Value::record(record, span)
}

/// The record a handler sees for a failed `expect=` check. `message` is the
/// model-facing rendering with the fix-relevant facts first; `expected`,
/// `observed`, and `diff` are the structured form; `result` is the call's full
/// result, so a handler never re-runs the call to see its output.
pub(crate) fn expectation_failed_record(
    call: &str,
    message: String,
    expected: Value,
    observed: Value,
    diff: Value,
    result: Value,
) -> Value {
    let span = Span::unknown();
    let mut record = Record::with_capacity(7);
    record.push("type", Value::string("ExpectationFailed", span));
    record.push("message", Value::string(message, span));
    record.push("call", Value::string(call, span));
    record.push("expected", expected);
    record.push("observed", observed);
    record.push("diff", diff);
    record.push("result", result);
    Value::record(record, span)
}

/// Classify a runtime error record that has no explicit `type` field. The
/// record comes from `shell_error_record`, so only `kind`, `code`, and
/// `message` are inspected. Runtime errors from the evaluator are all
/// `Stone {kind} error` with code `stone_script_error`; the message refines
/// the type for the handful of cases Python distinguishes.
pub(crate) fn classify_error(kind: &str, code: &str, message: &str) -> &'static str {
    let lower_kind = kind.to_ascii_lowercase();
    match code {
        "stone_io_error" | "io_error" => {
            return if lower_kind.contains("not found") {
                "FileNotFoundError"
            } else if lower_kind.contains("permission denied") {
                "PermissionError"
            } else if lower_kind.contains("already exists") {
                "FileExistsError"
            } else if lower_kind.contains("is a directory") {
                "IsADirectoryError"
            } else if lower_kind.contains("not a directory") {
                "NotADirectoryError"
            } else if lower_kind.contains("timed out") {
                "TimeoutError"
            } else {
                "OSError"
            };
        }
        "stone_must_run_failed" => return "ToolError",
        "task_failure" => return "Exception",
        _ if code.starts_with("model_") || code.starts_with("context_") => {
            return if mentions_timeout(message) {
                "TimeoutError"
            } else {
                "ToolError"
            };
        }
        "stone_script_error" => {}
        _ => return "RuntimeError",
    }

    let kind = lower_kind
        .strip_prefix("stone ")
        .and_then(|rest| rest.strip_suffix(" error"))
        .unwrap_or(lower_kind.as_str());
    if kind.starts_with("gateway") || message.starts_with("Gateway runtime") {
        return if mentions_timeout(message) {
            "TimeoutError"
        } else {
            "ToolError"
        };
    }
    // Working-directory checks before a process starts.
    if message.starts_with("cwd `") {
        if message.contains("does not exist") {
            return "FileNotFoundError";
        }
        if message.contains("is not a directory") {
            return "NotADirectoryError";
        }
    }
    if message.starts_with("permission denied while accessing cwd") {
        return "PermissionError";
    }
    if message.contains("by zero") {
        return "ZeroDivisionError";
    }
    if message.starts_with("unknown name") || kind == "name" {
        return "NameError";
    }
    if kind == "attribute" || message.contains("has no attribute") {
        return "AttributeError";
    }
    if message.contains("has no key") || message.contains("missing key") {
        return "KeyError";
    }
    if message.contains("out of range") {
        return "IndexError";
    }
    if message.starts_with("failed to parse") || message.contains("invalid literal") {
        return "ValueError";
    }
    if matches!(
        kind,
        "addition"
            | "subtraction"
            | "multiplication"
            | "division"
            | "floor division"
            | "modulo"
            | "comparison"
            | "unary minus"
            | "membership"
            | "iteration"
            | "len"
            | "augmented assignment"
            | "function call"
            | "record"
    ) || message.starts_with("cannot ")
        || message.contains(" expects ")
        || message.contains(" requires ")
        || message.contains("argument")
    {
        return "TypeError";
    }
    // JSON parse failures (read_json, json_loads, from_json) are ValueError,
    // as Python's json.JSONDecodeError is.
    if kind.contains("json") || is_json_parse_message(message) {
        return "ValueError";
    }
    "RuntimeError"
}

fn is_json_parse_message(message: &str) -> bool {
    [
        "expected value at line",
        "EOF while parsing",
        "trailing characters at line",
        "key must be a string at line",
        "invalid escape at line",
        "expected `,` or",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// Type for a Nu I/O error, from its kind rather than its display text
/// (the display text collapses most kinds to "I/O error").
pub(crate) fn io_error_type(kind: &ErrorKind) -> &'static str {
    use std::io::ErrorKind as Std;
    match kind {
        ErrorKind::FileNotFound | ErrorKind::DirectoryNotFound => "FileNotFoundError",
        ErrorKind::NotAFile => "IsADirectoryError",
        ErrorKind::Std(kind, ..) => match kind {
            Std::NotFound => "FileNotFoundError",
            Std::PermissionDenied | Std::ReadOnlyFilesystem => "PermissionError",
            Std::AlreadyExists => "FileExistsError",
            Std::IsADirectory => "IsADirectoryError",
            Std::NotADirectory => "NotADirectoryError",
            Std::TimedOut => "TimeoutError",
            _ => "OSError",
        },
        _ => "OSError",
    }
}

fn mentions_timeout(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("timed out") || message.contains("timeout")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_user(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn builtin_chain_matches_python_shape() {
        assert!(error_type_matches("FileNotFoundError", "OSError", no_user));
        assert!(error_type_matches(
            "FileNotFoundError",
            "ToolError",
            no_user
        ));
        assert!(error_type_matches(
            "FileNotFoundError",
            "Exception",
            no_user
        ));
        assert!(error_type_matches("KeyError", "LookupError", no_user));
        assert!(error_type_matches(
            "NotImplementedError",
            "RuntimeError",
            no_user
        ));
        assert!(!error_type_matches("KeyError", "IndexError", no_user));
        assert!(!error_type_matches("ValueError", "ToolError", no_user));
        assert!(!error_type_matches("Exception", "ValueError", no_user));
    }

    #[test]
    fn user_classes_walk_through_their_base() {
        let parent = |name: &str| match name {
            "ConfigError" => Some("ValueError".to_string()),
            "MissingConfig" => Some("ConfigError".to_string()),
            _ => None,
        };
        assert!(error_type_matches("MissingConfig", "ValueError", parent));
        assert!(error_type_matches("MissingConfig", "Exception", parent));
        assert!(!error_type_matches("MissingConfig", "ToolError", parent));
        // Unknown classes behave as direct Exception subclasses.
        assert!(error_type_matches("Unseen", "Exception", no_user));
        assert!(!error_type_matches("Unseen", "ValueError", no_user));
    }

    #[test]
    fn runtime_errors_classify_like_python() {
        let script = "stone_script_error";
        let cases = [
            (
                "Stone subscript error",
                script,
                "record has no key `k`",
                "KeyError",
            ),
            (
                "Stone subscript error",
                script,
                "index 5 is out of range",
                "IndexError",
            ),
            (
                "Stone addition error",
                script,
                "cannot add int and string",
                "TypeError",
            ),
            (
                "Stone int error",
                script,
                "failed to parse integer: invalid digit",
                "ValueError",
            ),
            ("Stone name error", script, "unknown name `x`", "NameError"),
            (
                "Stone division error",
                script,
                "division by zero",
                "ZeroDivisionError",
            ),
            (
                "Stone attribute error",
                script,
                "record has no attribute `b`",
                "AttributeError",
            ),
            (
                "Stone gateway rpc error",
                script,
                "connection refused",
                "ToolError",
            ),
            (
                "Stone gateway rpc error",
                script,
                "request timed out",
                "TimeoutError",
            ),
            (
                "Not found",
                "stone_io_error",
                "Not found: /x",
                "FileNotFoundError",
            ),
            (
                "Permission denied",
                "stone_io_error",
                "Permission denied: /x",
                "PermissionError",
            ),
            (
                "Checked run failed",
                "stone_must_run_failed",
                "must_run failed",
                "ToolError",
            ),
            ("Task failure", "task_failure", "boom", "Exception"),
        ];
        for (kind, code, message, expected) in cases {
            assert_eq!(
                classify_error(kind, code, message),
                expected,
                "{kind} / {message}"
            );
        }
    }
}
