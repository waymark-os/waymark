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
    if kind.starts_with("gateway") {
        return if mentions_timeout(message) {
            "TimeoutError"
        } else {
            "ToolError"
        };
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
    ) || message.starts_with("cannot ")
        || message.contains(" expects ")
        || message.contains(" requires ")
        || message.contains("argument")
    {
        return "TypeError";
    }
    "RuntimeError"
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
