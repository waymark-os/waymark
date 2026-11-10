// SPDX-License-Identifier: MIT OR Apache-2.0

//! Optional per-call expectations (`expect=`).
//!
//! An expectation is a closed record of checks on one call's observable
//! result. A met expectation adds nothing to the result. An unmet one raises
//! the host-only `ExpectationFailed`, which carries what was expected, what was
//! observed, the unmet checks, and the call's full result, so a handler never
//! re-runs the call (and doubles its side effects) to see what happened.

use nu_protocol::{Record, ShellError, Span, Value};

use super::stone_error;
use crate::json::nu_to_json_value;
use crate::stone_errors::{
    expectation_failed_record, EDIT_EXPECT_KEYS, EFFECT_KEYS, EXPECT_KEYS, IRREVERSIBLE_KINDS,
    PROCESS_EXPECT_KEYS,
};

const TAIL_CHARS: usize = 300;

/// Validate one `Expect(...)` field. The vocabulary is closed; an unknown key
/// uses the keyword-error shape the correction engine already repairs.
pub(super) fn validate_expect_field(key: &str, value: &Value) -> Result<(), ShellError> {
    let fail = |message: String| Err(stone_error("Expect", message));
    match key {
        "exit" => match value {
            Value::Int { .. } => Ok(()),
            Value::List { vals, .. }
                if !vals.is_empty() && vals.iter().all(|v| matches!(v, Value::Int { .. })) =>
            {
                Ok(())
            }
            other => fail(format!(
                "Expect exit must be an int or a non-empty list of ints, got {}; for example Expect(exit=0) or Expect(exit=[0, 1])",
                crate::stone_builtins::value_type_name(&other)
            )),
        },
        "stdout_contains" => match value {
            Value::String { .. } => Ok(()),
            other => fail(format!(
                "Expect stdout_contains must be a string, got {}",
                crate::stone_builtins::value_type_name(&other)
            )),
        },
        "stdout_matches" => match value {
            Value::String { val, .. } => regex::Regex::new(val).map(|_| ()).map_err(|err| {
                stone_error(
                    "Expect",
                    format!("Expect stdout_matches is not a valid regular expression: {err}"),
                )
            }),
            other => fail(format!(
                "Expect stdout_matches must be a regular-expression string, got {}",
                crate::stone_builtins::value_type_name(&other)
            )),
        },
        "stderr_empty" => match value {
            Value::Bool { .. } => Ok(()),
            other => fail(format!(
                "Expect stderr_empty must be True or False, got {}",
                crate::stone_builtins::value_type_name(&other)
            )),
        },
        "max_seconds" => match value {
            Value::Int { val, .. } if *val > 0 => Ok(()),
            Value::Float { val, .. } if *val > 0.0 => Ok(()),
            other => fail(format!(
                "Expect max_seconds must be a positive number, got {}",
                nu_to_json_value(other)
            )),
        },
        "occurrences" => match value {
            Value::Int { val, .. } if *val >= 1 => Ok(()),
            other => fail(format!(
                "Expect occurrences must be an int of at least 1, got {}",
                nu_to_json_value(other)
            )),
        },
        other => fail(format!(
            "unexpected Expect keyword argument `{other}`; expected {}",
            EXPECT_KEYS.join(", ")
        )),
    }
}

/// Validate one `Effects(...)` field (declared, tier-3 effects).
pub(super) fn validate_effect_field(key: &str, value: &Value) -> Result<(), ShellError> {
    let fail = |message: String| Err(stone_error("Effects", message));
    match key {
        "network" => match value {
            Value::Bool { val: false, .. } => Ok(()),
            Value::Bool { val: true, .. } => fail(
                "Effects network=True is too broad; list the hosts the command may reach, for example network=[\"pypi.org\", \"files.pythonhosted.org\"]".to_string(),
            ),
            Value::List { vals, .. } => {
                for host in vals {
                    match host {
                        Value::String { val, .. }
                            if !val.is_empty()
                                && !val.contains("://")
                                && !val.contains('/')
                                && !val.chars().any(char::is_whitespace) => {}
                        other => {
                            return fail(format!(
                                "Effects network hosts must be host names such as \"pypi.org\" or \"github.com:443\" (no scheme or path), got {}",
                                nu_to_json_value(other)
                            ));
                        }
                    }
                }
                Ok(())
            }
            other => fail(format!(
                "Effects network must be False or a list of host names, got {}",
                crate::stone_builtins::value_type_name(&other)
            )),
        },
        "irreversible" => match value {
            Value::List { vals, .. } => {
                for kind in vals {
                    match kind {
                        Value::String { val, .. } if IRREVERSIBLE_KINDS.contains(&val.as_str()) => {}
                        other => {
                            return fail(format!(
                                "Effects irreversible kinds are {}, got {}",
                                IRREVERSIBLE_KINDS.join(", "),
                                nu_to_json_value(other)
                            ));
                        }
                    }
                }
                Ok(())
            }
            other => fail(format!(
                "Effects irreversible must be a list such as [\"push\"], got {}",
                crate::stone_builtins::value_type_name(&other)
            )),
        },
        "background" => match value {
            Value::Bool { .. } => Ok(()),
            other => fail(format!(
                "Effects background must be True or False, got {}",
                crate::stone_builtins::value_type_name(&other)
            )),
        },
        other => fail(format!(
            "unexpected Effects keyword argument `{other}`; expected {}",
            EFFECT_KEYS.join(", ")
        )),
    }
}

/// Read an `effects=` argument: the record `Effects(...)` returns or a dict
/// with the same keys.
pub(super) fn effects_from_value(
    value: &Value,
    context: &str,
) -> Result<Option<Record>, ShellError> {
    let record = match value {
        Value::Nothing { .. } => return Ok(None),
        Value::Record { val, .. } => val.as_ref().clone(),
        other => {
            return Err(stone_error(
                context,
                format!(
                    "{context}() effects= must be Effects(...) or a dict, got {}; for example effects=Effects(network=[\"pypi.org\"])",
                    crate::stone_builtins::value_type_name(&other)
                ),
            ));
        }
    };
    for (key, value) in record.iter() {
        validate_effect_field(key, value)?;
    }
    Ok((!record.is_empty()).then_some(record))
}

/// Read an `expect=` argument for a call that supports the checks in
/// `allowed`. Accepts the record `Expect(...)` returns or a dict literal with
/// the same keys. `None` clears it.
pub(super) fn expect_from_value(
    value: &Value,
    context: &str,
    allowed: &[&str],
) -> Result<Option<Record>, ShellError> {
    let record = match value {
        Value::Nothing { .. } => return Ok(None),
        Value::Record { val, .. } => val.as_ref().clone(),
        other => {
            return Err(stone_error(
                context,
                format!(
                    "{context}() expect= must be Expect(...) or a dict of checks, got {}; for example expect=Expect({})",
                    crate::stone_builtins::value_type_name(&other),
                    if allowed.contains(&"exit") {
                        "exit=0"
                    } else {
                        "occurrences=1"
                    }
                ),
            ));
        }
    };
    if record.is_empty() {
        return Ok(None);
    }
    for (key, value) in record.iter() {
        validate_expect_field(key, value)?;
        if !allowed.contains(&key.as_str()) {
            return Err(stone_error(
                context,
                format!(
                    "{context}() does not support the expectation `{key}`; {context}() checks: {}",
                    allowed.join(", ")
                ),
            ));
        }
    }
    Ok(Some(record))
}

pub(super) fn process_expect_keys() -> &'static [&'static str] {
    PROCESS_EXPECT_KEYS
}

pub(super) fn edit_expect_keys() -> &'static [&'static str] {
    EDIT_EXPECT_KEYS
}

/// `max_seconds` doubles as the timeout when no explicit timeout is given.
pub(super) fn expect_timeout_ms(expect: &Record) -> Option<i64> {
    match expect.get("max_seconds")? {
        Value::Int { val, .. } => Some(val.saturating_mul(1000)),
        Value::Float { val, .. } => Some((val * 1000.0).ceil() as i64),
        _ => None,
    }
}

/// Check a process result (`run`, `run_complete`, `must_run`). Returns the
/// `ExpectationFailed` record when a check is not met.
pub(super) fn check_process_expectation(
    call: &str,
    expect: &Record,
    result: &Record,
    hint: Option<&str>,
) -> Option<Value> {
    let span = Span::unknown();
    let text = |key: &str| match result.get(key) {
        Some(Value::String { val, .. }) => val.as_str(),
        _ => "",
    };
    let stdout = text("stdout");
    let stderr = text("stderr");
    let exit_code = match result.get("exit_code") {
        Some(Value::Int { val, .. }) => Some(*val),
        _ => None,
    };
    let timed_out = matches!(result.get("timed_out"), Some(Value::Bool { val: true, .. }));
    let duration_seconds = match result.get("duration_ms") {
        Some(Value::Int { val, .. }) => Some(*val as f64 / 1000.0),
        _ => None,
    };

    let mut observed = Record::with_capacity(expect.len());
    let mut diff = Vec::new();
    let mut lines = Vec::new();
    let mut show_stdout_tail = false;
    let mut show_stderr_tail = false;
    for (key, expected) in expect.iter() {
        let (observed_value, met, line) = match key.as_str() {
            "exit" => {
                let allowed = match expected {
                    Value::Int { val, .. } => vec![*val],
                    Value::List { vals, .. } => vals
                        .iter()
                        .filter_map(|v| match v {
                            Value::Int { val, .. } => Some(*val),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let met = exit_code.is_some_and(|code| allowed.contains(&code));
                let got = match exit_code {
                    Some(code) => code.to_string(),
                    None if timed_out => "no exit status (timed out and killed)".to_string(),
                    None => "no exit status (the process did not run)".to_string(),
                };
                show_stderr_tail |= !met;
                (
                    exit_code.map_or_else(|| Value::nothing(span), |code| Value::int(code, span)),
                    met,
                    format!("exit: expected {}, got {got}", nu_to_json_value(expected)),
                )
            }
            "stdout_contains" => {
                let needle = string_value(expected);
                let met = stdout.contains(needle);
                show_stdout_tail |= !met;
                (
                    Value::bool(met, span),
                    met,
                    format!("stdout_contains {}: not found", nu_to_json_value(expected)),
                )
            }
            "stdout_matches" => {
                let pattern = string_value(expected);
                let met = regex::Regex::new(pattern)
                    .map(|re| re.is_match(stdout))
                    .unwrap_or(false);
                show_stdout_tail |= !met;
                (
                    Value::bool(met, span),
                    met,
                    format!("stdout_matches {}: no match", nu_to_json_value(expected)),
                )
            }
            "stderr_empty" => {
                let want = matches!(expected, Value::Bool { val: true, .. });
                let is_empty = stderr.trim().is_empty();
                let met = is_empty == want;
                show_stderr_tail |= !met && !is_empty;
                (
                    Value::bool(is_empty, span),
                    met,
                    format!(
                        "stderr_empty: expected {}, stderr is {}",
                        if want { "True" } else { "False" },
                        if is_empty { "empty" } else { "not empty" }
                    ),
                )
            }
            "max_seconds" => {
                let limit = match expected {
                    Value::Int { val, .. } => *val as f64,
                    Value::Float { val, .. } => *val,
                    _ => f64::INFINITY,
                };
                let met = !timed_out && duration_seconds.is_some_and(|seconds| seconds <= limit);
                let got = match duration_seconds {
                    Some(seconds) if timed_out => format!("{seconds:.1}s (timed out and killed)"),
                    Some(seconds) => format!("{seconds:.1}s"),
                    None => "unknown".to_string(),
                };
                (
                    duration_seconds
                        .map_or_else(|| Value::nothing(span), |s| Value::float(s, span)),
                    met,
                    format!("max_seconds: expected at most {limit}s, took {got}"),
                )
            }
            _ => continue,
        };
        if !met {
            let mut entry = Record::with_capacity(3);
            entry.push("check", Value::string(key.clone(), span));
            entry.push("expected", expected.clone());
            entry.push("observed", observed_value.clone());
            diff.push(Value::record(entry, span));
            lines.push(line);
        }
        observed.push(key.clone(), observed_value);
    }
    if diff.is_empty() {
        return None;
    }
    if show_stdout_tail {
        lines.push(format!("stdout tail: {}", quoted_tail(stdout)));
    }
    if show_stderr_tail && !stderr.trim().is_empty() {
        lines.push(format!("stderr tail: {}", quoted_tail(stderr)));
    }
    if exit_code.is_none() && !timed_out {
        if let Some(reason) = spawn_failure(result) {
            lines.push(format!("error: {reason}"));
        }
    }
    if let Some(hint) = hint {
        lines.push(hint.to_string());
    }
    let message = format!(
        "{call} {}: expectation NOT met\n  {}",
        command_label(result),
        lines.join("\n  ")
    );
    Some(expectation_failed_record(
        call,
        message,
        Value::record(expect.clone(), span),
        Value::record(observed, span),
        Value::list(diff, span),
        Value::record(result.clone(), span),
    ))
}

/// Build the `ExpectationFailed` record for an edit whose old text matched a
/// different number of times than expected.
pub(super) fn edit_expectation_failed(
    path: &str,
    expected: i64,
    matched: usize,
    match_lines: &[usize],
    first_line_hits: &[usize],
    replace_all: bool,
) -> Value {
    let span = Span::unknown();
    let mut lines = vec![format!(
        "occurrences: expected {}, found {matched}",
        if replace_all && expected <= 0 {
            "at least 1".to_string()
        } else {
            expected.to_string()
        }
    )];
    if matched == 0 {
        if first_line_hits.is_empty() {
            lines.push(
                "the old text does not appear in the file; read the file and copy the exact text, including indentation".to_string(),
            );
        } else {
            lines.push(format!(
                "the first line of the old text appears at line {}, so a later line or the whitespace differs; read those lines and copy them exactly",
                join_numbers(first_line_hits)
            ));
        }
    } else {
        lines.push(format!(
            "matches at line {}; add surrounding lines to the old text so it matches once, pass expect=Expect(occurrences={matched}) to replace all {matched}, or all=True",
            join_numbers(match_lines)
        ));
    }
    let mut expected_record = Record::with_capacity(1);
    let mut observed = Record::with_capacity(1);
    let mut diff_entry = Record::with_capacity(3);
    let expected_value = if replace_all && expected <= 0 {
        Value::string("at least 1", span)
    } else {
        Value::int(expected, span)
    };
    expected_record.push("occurrences", expected_value.clone());
    observed.push("occurrences", Value::int(matched as i64, span));
    diff_entry.push("check", Value::string("occurrences", span));
    diff_entry.push("expected", expected_value);
    diff_entry.push("observed", Value::int(matched as i64, span));
    let mut result = Record::with_capacity(4);
    result.push("path", Value::string(path, span));
    result.push("matched", Value::int(matched as i64, span));
    result.push(
        "match_lines",
        Value::list(
            match_lines
                .iter()
                .map(|line| Value::int(*line as i64, span))
                .collect(),
            span,
        ),
    );
    result.push("changed", Value::bool(false, span));
    expectation_failed_record(
        "edit",
        format!(
            "edit {path}: expectation NOT met, file unchanged\n  {}",
            lines.join("\n  ")
        ),
        Value::record(expected_record, span),
        Value::record(observed, span),
        Value::list(vec![Value::record(diff_entry, span)], span),
        Value::record(result, span),
    )
}

fn string_value(value: &Value) -> &str {
    match value {
        Value::String { val, .. } => val.as_str(),
        _ => "",
    }
}

fn quoted_tail(text: &str) -> String {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return "(empty)".to_string();
    }
    let start = trimmed
        .char_indices()
        .rev()
        .nth(TAIL_CHARS - 1)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let tail = &trimmed[start..];
    let prefix = if start > 0 { "..." } else { "" };
    serde_json::Value::String(format!("{prefix}{tail}")).to_string()
}

fn command_label(result: &Record) -> String {
    let argv = match result.get("argv") {
        Some(Value::List { vals, .. }) => vals
            .iter()
            .map(|value| match value {
                Value::String { val, .. } if val.contains(' ') || val.is_empty() => {
                    serde_json::Value::String(val.clone()).to_string()
                }
                Value::String { val, .. } => val.clone(),
                other => nu_to_json_value(other).to_string(),
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    if argv.chars().count() > 120 {
        let cut: String = argv.chars().take(117).collect();
        format!("{cut}...")
    } else {
        argv
    }
}

fn spawn_failure(result: &Record) -> Option<String> {
    match result.get("error")? {
        Value::Record { val, .. } => match val.get("detail").or_else(|| val.get("message"))? {
            Value::String { val, .. } => Some(val.chars().take(300).collect()),
            _ => None,
        },
        Value::String { val, .. } => Some(val.chars().take(300).collect()),
        _ => None,
    }
}

fn join_numbers(numbers: &[usize]) -> String {
    numbers
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
