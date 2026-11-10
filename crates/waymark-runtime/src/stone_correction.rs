// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeSet;

use nu_protocol::ShellError;
use serde_json::{json, Map, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::stone_eval::stone_builtin_names;

const MAX_CANDIDATES: usize = 3;
const MAX_EXPECTED: usize = 16;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const KEYWORD_SCHEMAS: &[(&str, &[&str])] = &[
    ("Expect", crate::stone_errors::EXPECT_KEYS),
    ("Effects", crate::stone_errors::EFFECT_KEYS),
    (
        "model_call",
        &[
            "model_class",
            "model",
            "temperature",
            "top_p",
            "seed",
            "max_output_tokens",
            "response_format",
            "metadata",
            "hooks",
        ],
    ),
    (
        "model_infer",
        &[
            "retries",
            "repair_prompt",
            "schema_prompt",
            "model_class",
            "model",
            "temperature",
            "top_p",
            "seed",
            "max_output_tokens",
            "metadata",
            "hooks",
        ],
    ),
    (
        "context_write",
        &["key", "kind", "content", "status", "evidence"],
    ),
    ("context_read", &["query", "keys", "kinds", "limit"]),
    ("context_project", &["focus", "max_tokens", "required_keys"]),
    ("correction_apply", &["source", "correction", "candidate"]),
    ("workflow_evidence", &[]),
    ("file_nonempty", &[]),
    (
        "stage",
        &["evidence", "repair", "max_attempts", "checkpoint"],
    ),
    (
        "workflow_stage",
        &["evidence", "action", "repair", "max_attempts", "checkpoint"],
    ),
    ("workflow", &[]),
    ("workflow_run", &[]),
    ("semantic_frontier", &["checkpoint", "owner"]),
    ("semantic_frontier_release", &["frontier"]),
    ("attempt_best", &["scope", "objective"]),
    (
        "attempt_best_consider",
        &[
            "best",
            "outcome",
            "score",
            "summary",
            "evidence",
            "artifacts",
        ],
    ),
    ("attempt_best_accept", &["best", "parent"]),
    ("attempt_best_discard", &["best", "reason"]),
    ("attempt_retain_failure", &["scope", "outcome"]),
    (
        "attempt_branch",
        &[
            "frontier",
            "task",
            "input",
            "task_input",
            "context_prompt_view",
            "program",
            "entrypoint",
            "start",
            "scope",
            "controller",
            "capability_profile",
            "container",
            "workspace_mount",
            "time_budget_ms",
            "resource_limits",
            "limits",
            "metadata",
            "meta",
        ],
    ),
];

#[derive(Clone, Copy)]
enum SourceUse {
    Call,
    Keyword,
    Attribute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RankedCandidate {
    replacement: String,
    distance: usize,
}

pub(crate) fn correction_for_error(err: &ShellError, source: &str) -> Option<JsonValue> {
    let ShellError::Generic(error) = err else {
        return None;
    };
    let code = error.code.as_ref();
    let detail = error.msg.to_string();
    let phase = if matches!(
        code,
        "stone_parse_error" | "stone_script_unsupported" | "stone_admission_error"
    ) {
        "admission"
    } else {
        "evaluation"
    };

    if let Some(received) = unknown_call_name(&detail) {
        let mut expected = stone_builtin_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        expected.extend(["get", "items", "keys", "values"].map(str::to_string));
        expected.extend(source_function_names(source));
        let candidates = rank_candidates(&received, &expected);
        if candidates.is_empty() {
            return None;
        }
        return Some(suggestion_envelope(
            source,
            phase,
            "name",
            &received,
            candidates
                .iter()
                .map(|item| item.replacement.clone())
                .collect(),
            candidate_json(source, &received, SourceUse::Call, &candidates),
            "Replace the unknown callable, then explicitly evaluate the corrected source again.",
        ));
    }

    if let Some((received, expected)) = keyword_error(&detail) {
        let candidates = rank_candidates(&received, &expected);
        return Some(suggestion_envelope(
            source,
            phase,
            "keyword",
            &received,
            expected,
            candidate_json(source, &received, SourceUse::Keyword, &candidates),
            "Correct the structured keyword, then explicitly evaluate the source again.",
        ));
    }

    if let Some((received, expected)) = attribute_error(&detail) {
        let candidates = rank_candidates(&received, &expected);
        if candidates.is_empty() {
            return None;
        }
        return Some(suggestion_envelope(
            source,
            phase,
            "field",
            &received,
            expected,
            candidate_json(source, &received, SourceUse::Attribute, &candidates),
            "Use a field exposed by the structured record, then explicitly evaluate the source again.",
        ));
    }

    if code == "stone_script_unsupported" {
        if let Some(name) = exception_handler_binding_error(&detail) {
            return Some(repair_envelope_owned(
                source,
                phase,
                "exception_binding",
                &format!("except {name}:"),
                vec![format!("except Exception as {name}:")],
                &format!(
                    "`except {name}:` names an exception type in Python; it does not bind the caught error. Use `except Exception as {name}:`, or a narrower type such as `except FileNotFoundError as {name}:` when only that error is expected. Then explicitly evaluate the repaired source again."
                ),
            ));
        }
        if detail.starts_with("bare `except:` is not supported") {
            return Some(repair_envelope(
                source,
                phase,
                "bare_except",
                "except:",
                vec!["except Exception as error:"],
                "Stone requires every handler to name an error type. Use `except Exception as error:` to handle any error, or a narrower type such as `except FileNotFoundError as error:` when only that error is expected. Then explicitly evaluate the repaired source again.",
            ));
        }
    }

    if code == "stone_admission_error" {
        if let Some(received) = swallowing_handler_error(&detail) {
            let bound = received.trim_end_matches(": pass").to_string();
            return Some(repair_envelope_owned(
                source,
                phase,
                "swallowed_error",
                &received,
                vec![
                    format!("{bound} as error: <use error.message or raise>"),
                    "except FileNotFoundError: pass".to_string(),
                ],
                "A handler for every error whose body is only `pass` hides failed commands, missing files, and failed model calls. Bind the error and act on it (record error.message, return a fallback value, or `raise`), or catch only the specific error you expect. Then explicitly evaluate the repaired source again.",
            ));
        }
        if let Some(received) = backticked_after(&detail, "unknown error type `") {
            let builtin = crate::stone_errors::BUILTIN_ERROR_TYPES
                .iter()
                .map(|(name, _)| (*name).to_string())
                .collect::<Vec<_>>();
            let mut expected = rank_candidates(&received, &builtin)
                .into_iter()
                .map(|candidate| candidate.replacement)
                .collect::<Vec<_>>();
            expected.push(format!("class {received}(Exception):"));
            return Some(repair_envelope_owned(
                source,
                phase,
                "error_type",
                &received,
                expected,
                &format!(
                    "`{received}` is not a builtin error type and no error class with that name is defined. Use a builtin type, or define it first with `class {received}(Exception):` and optional typed fields. Then explicitly evaluate the repaired source again."
                ),
            ));
        }
    }

    if code == "stone_script_unsupported" {
        if let Some((name, literal)) = mutable_default_error(&detail) {
            return Some(repair_envelope_owned(
                source,
                phase,
                "mutable_default",
                &format!("{name}={literal}"),
                vec![format!("{name}=None")],
                &format!(
                    "Python creates a default value once and shares it across calls, so a list or dict default leaks state between calls. Use `{name}=None` and add `if {name} is None: {name} = {literal}` at the top of the function body, keeping the original initial value. Then explicitly evaluate the repaired source again."
                ),
            ));
        }
        if detail.contains("`finally` is not part of Stone") {
            return Some(repair_envelope(
                source,
                phase,
                "finally",
                "finally:",
                vec![
                    "except Exception as error: <cleanup>; raise error",
                    "<cleanup> after the try statement",
                ],
                "Stone has no `finally`. The host releases runtime resources (attempts, daemons, open files) when an attempt ends, so cleanup is only needed for your own state. Move the cleanup into `except Exception as error:` followed by `raise error`, and repeat it after the try statement for the success path. Then explicitly evaluate the repaired source again.",
            ));
        }
    }

    if code == "stone_script_unsupported"
        && detail.starts_with("Stone has no `global`")
        && contains_identifier(source, "global")
    {
        return Some(repair_envelope(
            source,
            phase,
            "unsupported_construct",
            "global",
            vec!["return the new value", "state held in a dict"],
            "Stone has no `global`: functions read module variables but do not rebind them. Return the new value and assign it at module level (`total = add(total, x)`), or keep shared state in a dict (`state[\"total\"] += x`). Then explicitly evaluate the repaired source again.",
        ));
    }

    if code == "stone_script_unsupported"
        && detail.starts_with("Stone has no `nonlocal`")
        && contains_identifier(source, "nonlocal")
    {
        return Some(repair_envelope(
            source,
            phase,
            "unsupported_construct",
            "nonlocal",
            vec!["return the new value", "state held in a list or dict"],
            "Stone has no `nonlocal`: closures read enclosing variables but do not rebind them. Keep the state in a list or dict (`count = [0]`, then `count[0] += 1`), or return the new value. Then explicitly evaluate the repaired source again.",
        ));
    }

    if let Some((received, expected)) = message_role_error(&detail) {
        return Some(repair_envelope(
            source,
            phase,
            "message_role",
            &received,
            expected,
            "Do not relabel a tool result mechanically. Serialize the structured observation into an allowed model message role and preserve its meaning.",
        ));
    }

    if detail.contains("expected attempt_handle, got attempt_acceptance") {
        return Some(repair_envelope(
            source,
            phase,
            "lifecycle_type",
            "attempt_acceptance",
            vec!["attempt_handle"],
            "attempt_accept() returns a typed lifecycle result. Preserve the original child handle, or pass accepted.selected when the callee requires attempt_handle.",
        ));
    }

    None
}

fn suggestion_envelope(
    source: &str,
    phase: &str,
    class: &str,
    received: &str,
    expected: Vec<String>,
    candidates: Vec<JsonValue>,
    guidance: &str,
) -> JsonValue {
    json!({
        "version": 1,
        "mode": "suggest",
        "phase": phase,
        "execution_state": execution_state(phase),
        "class": class,
        "safety": "suggest_only",
        "auto_apply": false,
        "retry": "explicit_only",
        "source_sha256": source_sha256(source),
        "received": received,
        "expected": bounded_unique(expected),
        "candidates": candidates,
        "guidance": guidance,
        "choices": ["apply", "edit", "reject", "abort"],
    })
}

fn repair_envelope(
    source: &str,
    phase: &str,
    class: &str,
    received: &str,
    expected: Vec<&str>,
    guidance: &str,
) -> JsonValue {
    repair_envelope_owned(
        source,
        phase,
        class,
        received,
        expected.into_iter().map(str::to_string).collect(),
        guidance,
    )
}

fn repair_envelope_owned(
    source: &str,
    phase: &str,
    class: &str,
    received: &str,
    expected: Vec<String>,
    guidance: &str,
) -> JsonValue {
    json!({
        "version": 1,
        "mode": "suggest",
        "phase": phase,
        "execution_state": execution_state(phase),
        "class": class,
        "safety": "requires_repair",
        "auto_apply": false,
        "retry": "explicit_only",
        "source_sha256": source_sha256(source),
        "received": received,
        "expected": expected,
        "candidates": [],
        "guidance": guidance,
        "choices": ["edit", "reject", "abort"],
    })
}

pub(crate) fn apply_correction(
    source: &str,
    correction: &JsonValue,
    candidate_index: usize,
) -> Result<JsonValue, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source exceeds the correction limit of {MAX_SOURCE_BYTES} bytes"
        ));
    }
    let fields = correction
        .as_object()
        .ok_or_else(|| "correction must be a record".to_string())?;
    if fields.get("version").and_then(JsonValue::as_u64) != Some(1) {
        return Err("correction version must be 1".to_string());
    }
    if fields.get("mode").and_then(JsonValue::as_str) != Some("suggest")
        || fields.get("safety").and_then(JsonValue::as_str) != Some("suggest_only")
        || fields.get("auto_apply").and_then(JsonValue::as_bool) != Some(false)
        || fields.get("retry").and_then(JsonValue::as_str) != Some("explicit_only")
    {
        return Err("correction is not an explicit, suggest-only edit".to_string());
    }
    let expected_digest = fields
        .get("source_sha256")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "correction is missing source_sha256".to_string())?;
    let actual_digest = source_sha256(source);
    if expected_digest != actual_digest {
        return Err("correction does not match the supplied source".to_string());
    }
    let received = fields
        .get("received")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "correction is missing received".to_string())?;
    let candidates = fields
        .get("candidates")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "correction candidates must be a list".to_string())?;
    if candidates.is_empty() || candidates.len() > MAX_CANDIDATES {
        return Err(format!(
            "correction must contain between 1 and {MAX_CANDIDATES} candidates"
        ));
    }
    let candidate = candidates
        .get(candidate_index)
        .and_then(JsonValue::as_object)
        .ok_or_else(|| format!("correction candidate {candidate_index} does not exist"))?;
    let replacement = candidate
        .get("replacement")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "correction candidate is missing replacement".to_string())?;
    if !is_identifier(replacement) || replacement.len() > 128 {
        return Err("correction replacement must be a bounded identifier".to_string());
    }
    let edit = candidate
        .get("edit")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| "correction candidate has no unambiguous source edit".to_string())?;
    let start = edit
        .get("start")
        .and_then(JsonValue::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| "correction edit start must be a non-negative byte offset".to_string())?;
    let end = edit
        .get("end")
        .and_then(JsonValue::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| "correction edit end must be a non-negative byte offset".to_string())?;
    let edit_replacement = edit
        .get("replacement")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "correction edit is missing replacement".to_string())?;
    if edit_replacement != replacement {
        return Err("correction candidate and edit replacements differ".to_string());
    }
    if start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
    {
        return Err("correction edit is outside the supplied source".to_string());
    }
    if &source[start..end] != received {
        return Err("correction edit no longer identifies the received text".to_string());
    }

    let mut corrected = String::with_capacity(source.len() - (end - start) + replacement.len());
    corrected.push_str(&source[..start]);
    corrected.push_str(replacement);
    corrected.push_str(&source[end..]);
    Ok(json!({
        "applied": true,
        "executed": false,
        "candidate": candidate_index,
        "source": corrected,
        "source_sha256": source_sha256(&corrected),
        "previous_source_sha256": actual_digest,
        "edit": {
            "start": start,
            "end": end,
            "received": received,
            "replacement": replacement,
        },
        "next_action": "Evaluate the returned source explicitly in the appropriate transaction.",
        "failed_execution_state": fields.get("execution_state").cloned().unwrap_or(JsonValue::Null),
    }))
}

fn execution_state(phase: &str) -> &'static str {
    if phase == "admission" {
        "not_started"
    } else {
        "started_or_unknown"
    }
}

fn source_sha256(source: &str) -> String {
    let digest = Sha256::digest(source.as_bytes());
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("write to string");
    }
    output
}

fn candidate_json(
    source: &str,
    received: &str,
    source_use: SourceUse,
    candidates: &[RankedCandidate],
) -> Vec<JsonValue> {
    let edit_span = unique_identifier_span(source, received, source_use);
    let best_distance = candidates.first().map(|item| item.distance);
    let best_count = best_distance.map_or(0, |best| {
        candidates
            .iter()
            .take_while(|item| item.distance == best)
            .count()
    });
    candidates
        .iter()
        .map(|candidate| {
            let confidence = if candidate.distance <= 1 && best_count == 1 {
                "high"
            } else if candidate.distance <= 2 {
                "medium"
            } else {
                "low"
            };
            let mut value = Map::new();
            value.insert(
                "replacement".to_string(),
                JsonValue::String(candidate.replacement.clone()),
            );
            value.insert(
                "confidence".to_string(),
                JsonValue::String(confidence.into()),
            );
            value.insert("distance".to_string(), json!(candidate.distance));
            if let Some((start, end)) = edit_span {
                value.insert(
                    "edit".to_string(),
                    json!({
                        "start": start,
                        "end": end,
                        "replacement": candidate.replacement.clone(),
                    }),
                );
            }
            JsonValue::Object(value)
        })
        .collect()
}

fn unknown_call_name(detail: &str) -> Option<String> {
    [
        "unknown Stone function `",
        "unknown builtin `",
        "unknown function `",
    ]
    .into_iter()
    .find_map(|prefix| backticked_after(detail, prefix))
}

fn keyword_error(detail: &str) -> Option<(String, Vec<String>)> {
    for (call_name, expected) in KEYWORD_SCHEMAS {
        let prefix = format!("unexpected {call_name} keyword argument `");
        if let Some(received) = backticked_after(detail, &prefix) {
            return Some((
                received,
                expected.iter().map(|value| (*value).to_string()).collect(),
            ));
        }
    }

    let received = backticked_after(detail, "unexpected keyword argument `")?;
    let expected = detail
        .split_once("; expected ")
        .map(|(_, values)| parse_expected_words(values))
        .unwrap_or_default();
    (!expected.is_empty()).then_some((received, expected))
}

pub(crate) fn expected_keywords(call_name: &str) -> Option<&'static [&'static str]> {
    KEYWORD_SCHEMAS
        .iter()
        .find_map(|(name, keywords)| (*name == call_name).then_some(*keywords))
}

fn attribute_error(detail: &str) -> Option<(String, Vec<String>)> {
    // Typed records: "record `Step` has no field `id`; its fields are: a, b".
    let (received, fields) = match backticked_after(detail, "record has no attribute `") {
        Some(received) => (received, detail.split_once("; available fields: ")?.1),
        None => {
            let (_, rest) = detail.split_once("` has no field `")?;
            let (received, _) = rest.split_once('`')?;
            (received.to_string(), detail.split_once("; its fields are: ")?.1)
        }
    };
    let expected = fields
        .split(',')
        .map(|field| field.trim().trim_matches('`').to_string())
        .filter(|field| !field.is_empty())
        .take(MAX_EXPECTED)
        .collect::<Vec<_>>();
    (!expected.is_empty()).then_some((received, expected))
}

fn message_role_error(detail: &str) -> Option<(String, Vec<&str>)> {
    let rest = detail
        .strip_prefix("unsupported model_call message role ")
        .or_else(|| detail.strip_prefix("unsupported message role "))?;
    let (received, _) = rest.split_once(';')?;
    Some((
        received
            .trim()
            .trim_matches(|ch| ch == '\'' || ch == '"')
            .to_string(),
        vec!["system", "user", "assistant"],
    ))
}

fn swallowing_handler_error(detail: &str) -> Option<String> {
    let received = backticked_after(detail, "`")?;
    (received.starts_with("except ")
        && received.ends_with(": pass")
        && detail.contains("discards every"))
    .then_some(received)
}

fn exception_handler_binding_error(detail: &str) -> Option<String> {
    let received = backticked_after(detail, "unsupported exception handler `")?;
    let name = received.strip_prefix("except ")?.strip_suffix(':')?.trim();
    is_identifier(name).then(|| name.to_string())
}

fn mutable_default_error(detail: &str) -> Option<(String, String)> {
    let received = backticked_after(detail, "mutable default parameter value `")?;
    let (name, literal) = received.split_once('=')?;
    is_identifier(name).then(|| (name.to_string(), literal.to_string()))
}

fn backticked_after(detail: &str, prefix: &str) -> Option<String> {
    let rest = detail.split_once(prefix)?.1;
    let (value, _) = rest.split_once('`')?;
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_expected_words(values: &str) -> Vec<String> {
    values
        .replace(", or ", ",")
        .replace(" or ", ",")
        .split(',')
        .map(|value| {
            value
                .trim()
                .trim_end_matches('.')
                .trim_matches('`')
                .to_string()
        })
        .filter(|value| is_identifier(value))
        .take(MAX_EXPECTED)
        .collect()
}

fn source_function_names(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("def ")?;
            let name = rest.split_once('(')?.0.trim();
            is_identifier(name).then(|| name.to_string())
        })
        .collect()
}

fn rank_candidates(received: &str, expected: &[String]) -> Vec<RankedCandidate> {
    if !is_identifier(received) || received.len() > 128 {
        return Vec::new();
    }
    let max_distance = match received.chars().count() {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };
    let suffix = format!("_{received}");
    let mut ranked = expected
        .iter()
        .filter(|candidate| is_identifier(candidate) && candidate.len() <= 128)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|candidate| {
            let distance = if candidate.ends_with(&suffix) {
                1
            } else {
                damerau_levenshtein(received, &candidate)
            };
            (distance <= max_distance).then_some(RankedCandidate {
                replacement: candidate,
                distance,
            })
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.replacement.cmp(&right.replacement))
    });
    ranked.truncate(MAX_CANDIDATES);
    ranked
}

fn bounded_unique(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_EXPECTED)
        .collect()
}

fn unique_identifier_span(
    source: &str,
    identifier: &str,
    source_use: SourceUse,
) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut matches = source
        .match_indices(identifier)
        .filter_map(|(start, _)| {
            let end = start + identifier.len();
            let left_ok = start == 0 || !is_identifier_byte(bytes[start - 1]);
            let right_ok = end == bytes.len() || !is_identifier_byte(bytes[end]);
            if !left_ok || !right_ok {
                return None;
            }
            let matches_use = match source_use {
                SourceUse::Call => next_non_space(bytes, end) == Some(b'('),
                SourceUse::Keyword => next_non_space(bytes, end) == Some(b'='),
                SourceUse::Attribute => previous_non_space(bytes, start) == Some(b'.'),
            };
            matches_use.then_some((start, end))
        })
        .take(2);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn contains_identifier(source: &str, identifier: &str) -> bool {
    let bytes = source.as_bytes();
    source.match_indices(identifier).any(|(start, _)| {
        let end = start + identifier.len();
        (start == 0 || !is_identifier_byte(bytes[start - 1]))
            && (end == bytes.len() || !is_identifier_byte(bytes[end]))
    })
}

fn next_non_space(bytes: &[u8], mut index: usize) -> Option<u8> {
    while let Some(byte) = bytes.get(index) {
        if !byte.is_ascii_whitespace() {
            return Some(*byte);
        }
        index += 1;
    }
    None
}

fn previous_non_space(bytes: &[u8], mut index: usize) -> Option<u8> {
    while index > 0 {
        index -= 1;
        let byte = bytes[index];
        if !byte.is_ascii_whitespace() {
            return Some(byte);
        }
    }
    None
}

fn is_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_') && bytes.all(is_identifier_byte)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn damerau_levenshtein(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut rows = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (index, row) in rows.iter_mut().enumerate() {
        row[0] = index;
    }
    for index in 0..=right.len() {
        rows[0][index] = index;
    }
    for left_index in 1..=left.len() {
        for right_index in 1..=right.len() {
            let substitution = usize::from(left[left_index - 1] != right[right_index - 1]);
            let mut distance = (rows[left_index - 1][right_index] + 1)
                .min(rows[left_index][right_index - 1] + 1)
                .min(rows[left_index - 1][right_index - 1] + substitution);
            if left_index > 1
                && right_index > 1
                && left[left_index - 1] == right[right_index - 2]
                && left[left_index - 2] == right[right_index - 1]
            {
                distance = distance.min(rows[left_index - 2][right_index - 2] + 1);
            }
            rows[left_index][right_index] = distance;
        }
    }
    rows[left.len()][right.len()]
}

#[cfg(test)]
mod tests {
    use nu_protocol::{shell_error::generic::GenericError, ShellError};
    use serde_json::json;

    use super::{apply_correction, contains_identifier, correction_for_error, damerau_levenshtein};

    fn error(code: &'static str, detail: &str) -> ShellError {
        ShellError::Generic(
            GenericError::new_internal("Stone test error", detail.to_string()).with_code(code),
        )
    }

    #[test]
    fn ranks_missing_and_transposed_characters_as_one_edit() {
        assert_eq!(damerau_levenshtein("projet", "project"), 1);
        assert_eq!(damerau_levenshtein("wirte", "write"), 1);
    }

    #[test]
    fn pathological_identifiers_do_not_enter_distance_ranking() {
        let received = "x".repeat(129);
        let correction = correction_for_error(
            &error(
                "stone_script_error",
                &format!("unknown Stone function `{received}`; use help()"),
            ),
            &format!("{received}()"),
        );
        assert!(correction.is_none());
    }

    #[test]
    fn unknown_builtin_has_one_bounded_source_edit() {
        let source = "context_projet(focus=\"x\")";
        let correction = correction_for_error(
            &error(
                "stone_admission_error",
                "unknown Stone function `context_projet`; use help()",
            ),
            source,
        )
        .expect("correction");
        assert_eq!(correction["class"], json!("name"));
        assert_eq!(
            correction["candidates"][0]["replacement"],
            json!("context_project")
        );
        assert_eq!(correction["candidates"][0]["confidence"], json!("high"));
        assert_eq!(
            correction["candidates"][0]["edit"],
            json!({"start": 0, "end": 14, "replacement": "context_project"})
        );
        assert_eq!(correction["auto_apply"], json!(false));
        assert_eq!(correction["retry"], json!("explicit_only"));
        assert_eq!(correction["execution_state"], json!("not_started"));

        let preview = apply_correction(source, &correction, 0).expect("apply correction");
        assert_eq!(preview["source"], json!("context_project(focus=\"x\")"));
        assert_eq!(preview["executed"], json!(false));
        assert_eq!(preview["failed_execution_state"], json!("not_started"));
        assert_eq!(
            preview["previous_source_sha256"],
            correction["source_sha256"]
        );
    }

    #[test]
    fn correction_application_rejects_stale_or_semantic_source() {
        let source = "context_projet(focus=\"x\")";
        let correction = correction_for_error(
            &error(
                "stone_admission_error",
                "unknown Stone function `context_projet`; use help()",
            ),
            source,
        )
        .expect("correction");
        assert!(
            apply_correction("context_projet(focus=\"y\")", &correction, 0)
                .expect_err("stale source")
                .contains("does not match")
        );

        let semantic = correction_for_error(
            &error(
                "stone_script_unsupported",
                "Stone has no `global`: functions read module variables but do not rebind them",
            ),
            "def update():\n    global mode\n",
        )
        .expect("semantic correction");
        assert!(
            apply_correction("def update():\n    global mode\n", &semantic, 0)
                .expect_err("semantic correction")
                .contains("suggest-only")
        );
    }

    #[test]
    fn context_keyword_uses_its_call_schema() {
        let correction = correction_for_error(
            &error(
                "stone_admission_error",
                "unexpected context_project keyword argument `max_token`",
            ),
            "context_project(max_token = 32)",
        )
        .expect("correction");
        assert_eq!(correction["class"], json!("keyword"));
        assert_eq!(
            correction["candidates"][0]["replacement"],
            json!("max_tokens")
        );
    }

    #[test]
    fn attempt_branch_schema_includes_context_prompt_view() {
        let correction = correction_for_error(
            &error(
                "stone_admission_error",
                "unexpected attempt_branch keyword argument `context_prompt_vew`",
            ),
            "attempt_branch(frontier, context_prompt_vew={})",
        )
        .expect("correction");
        assert_eq!(correction["class"], json!("keyword"));
        assert_eq!(
            correction["candidates"][0]["replacement"],
            json!("context_prompt_view")
        );
    }

    #[test]
    fn typed_record_field_errors_suggest_the_nearest_field() {
        let correction = correction_for_error(
            &error(
                "stone_script_error",
                "record `Step` has no field `id`; its fields are: transition_id, phase",
            ),
            "emit(step.id)",
        )
        .expect("correction");
        assert_eq!(correction["class"], json!("field"));
        assert_eq!(
            correction["candidates"][0]["replacement"],
            json!("transition_id")
        );
    }

    #[test]
    fn suffix_field_alias_can_suggest_transition_id() {
        let correction = correction_for_error(
            &error(
                "stone_script_error",
                "record has no attribute `id`; available fields: `transition_id`, `kind`, `phase`",
            ),
            "emit(step.id)",
        )
        .expect("correction");
        assert_eq!(correction["class"], json!("field"));
        assert_eq!(
            correction["candidates"][0]["replacement"],
            json!("transition_id")
        );
    }

    #[test]
    fn semantic_constructs_require_repair_without_an_edit() {
        let global = correction_for_error(
            &error(
                "stone_script_unsupported",
                "Stone has no `global`: functions read module variables but do not rebind them",
            ),
            "def update():\n    global mode\n",
        )
        .expect("global correction");
        assert_eq!(global["safety"], json!("requires_repair"));
        assert_eq!(global["candidates"], json!([]));

        let role = correction_for_error(
            &error(
                "model_invalid_request",
                "unsupported model_call message role \"tool\"; expected system, user, or assistant",
            ),
            r#"model_call([{"role":"tool","content":"x"}])"#,
        )
        .expect("role correction");
        assert_eq!(role["class"], json!("message_role"));
        assert_eq!(role["safety"], json!("requires_repair"));
    }

    #[test]
    fn handler_rules_get_typed_repairs() {
        let bare = correction_for_error(
            &error(
                "stone_script_unsupported",
                crate::stone_ast::BARE_EXCEPT_UNSUPPORTED,
            ),
            "try:\n    x = 1\nexcept:\n    x = 2\n",
        )
        .expect("bare except correction");
        assert_eq!(bare["class"], json!("bare_except"));
        assert_eq!(bare["expected"], json!(["except Exception as error:"]));

        let swallowed = correction_for_error(
            &error(
                "stone_admission_error",
                "`except Exception: pass` discards every Exception without a trace",
            ),
            "try:\n    x = 1\nexcept Exception:\n    pass\n",
        )
        .expect("swallowed handler correction");
        assert_eq!(swallowed["class"], json!("swallowed_error"));
        assert_eq!(swallowed["received"], json!("except Exception: pass"));
        assert_eq!(
            swallowed["expected"][0],
            json!("except Exception as error: <use error.message or raise>")
        );

        let unknown = correction_for_error(
            &error(
                "stone_admission_error",
                "`except ValueEror`: unknown error type `ValueEror`. Builtin error types: ...",
            ),
            "try:\n    x = 1\nexcept ValueEror:\n    x = 2\n",
        )
        .expect("unknown error type correction");
        assert_eq!(unknown["class"], json!("error_type"));
        assert_eq!(
            unknown["expected"],
            json!(["ValueError", "class ValueEror(Exception):"])
        );
    }

    #[test]
    fn exception_handler_name_gets_binding_specific_repair() {
        let correction = correction_for_error(
            &error(
                "stone_script_unsupported",
                "unsupported exception handler `except error:`; Stone does not treat a handler name as an error binding",
            ),
            "try:\n    best.outcome\nexcept error:\n    emit(error.message)\n",
        )
        .expect("exception binding correction");
        assert_eq!(correction["class"], json!("exception_binding"));
        assert_eq!(correction["phase"], json!("admission"));
        assert_eq!(correction["execution_state"], json!("not_started"));
        assert_eq!(correction["safety"], json!("requires_repair"));
        assert_eq!(correction["received"], json!("except error:"));
        assert_eq!(
            correction["expected"],
            json!(["except Exception as error:"])
        );
        assert_eq!(correction["candidates"], json!([]));
        assert!(correction["guidance"]
            .as_str()
            .unwrap()
            .contains("does not bind the caught error"));
    }

    #[test]
    fn mutable_default_gets_none_idiom_repair() {
        let source = "def f(acc=[]):\n    return acc\n";
        let correction = correction_for_error(
            &error(
                "stone_script_unsupported",
                &crate::stone_ast::mutable_default_message("acc", "[]"),
            ),
            source,
        )
        .expect("mutable default correction");
        assert_eq!(correction["class"], json!("mutable_default"));
        assert_eq!(correction["safety"], json!("requires_repair"));
        assert_eq!(correction["received"], json!("acc=[]"));
        assert_eq!(correction["expected"], json!(["acc=None"]));
        assert!(correction["guidance"]
            .as_str()
            .unwrap()
            .contains("`if acc is None: acc = []`"));
    }

    #[test]
    fn finally_gets_reraise_repair_not_global_guidance() {
        // The source contains the identifier `global` (in a comment) to check
        // that the finally repair wins over the generic `global` guidance.
        let source = "# no global state\ntry:\n    x = 1\nexcept Exception as error:\n    raise error\nfinally:\n    x = 2\n";
        assert!(contains_identifier(source, "global"));
        let correction = correction_for_error(
            &error(
                "stone_script_unsupported",
                crate::stone_ast::FINALLY_UNSUPPORTED,
            ),
            source,
        )
        .expect("finally correction");
        assert_eq!(correction["class"], json!("finally"));
        assert_eq!(correction["received"], json!("finally:"));
        assert_eq!(
            correction["expected"],
            json!([
                "except Exception as error: <cleanup>; raise error",
                "<cleanup> after the try statement"
            ])
        );
        assert!(correction["guidance"]
            .as_str()
            .unwrap()
            .contains("host releases runtime resources"));
    }

    #[test]
    fn acceptance_handle_confusion_gets_typed_lifecycle_guidance() {
        let correction = correction_for_error(
            &error(
                "stone_script_error",
                "argument `repaired` expected attempt_handle, got attempt_acceptance",
            ),
            "return fixture_finalize(accepted)",
        )
        .expect("lifecycle type correction");
        assert_eq!(correction["class"], json!("lifecycle_type"));
        assert_eq!(correction["received"], json!("attempt_acceptance"));
        assert_eq!(correction["expected"], json!(["attempt_handle"]));
        assert_eq!(correction["safety"], json!("requires_repair"));
        assert!(correction["guidance"]
            .as_str()
            .unwrap()
            .contains("accepted.selected"));
    }
}
