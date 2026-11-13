// SPDX-License-Identifier: MIT OR Apache-2.0

//! Named inputs to model calls (design 5.9).
//!
//! `message(text, inputs={...})` and `model.ask(text, inputs={...})` take
//! the values a model step works from as named values, and the runtime, not
//! the program, turns them into prompt text. That keeps three things the
//! program would otherwise lose when it flattens evidence into a string:
//!
//! - which values a model call saw, by name and type;
//! - where each came from: a runtime result (the world), a model's typed
//!   answer, or a value the program made;
//! - byte-stable prompts. Providers reuse the processed prefix of a prompt
//!   (Anthropic at cache markers, OpenAI and vLLM automatically, llama.cpp
//!   per slot), and one changed byte invalidates everything after it. The
//!   same value always renders to the same bytes: record fields in
//!   declaration order, dict keys in insertion order, and no sequence
//!   numbers, transition ids, timings, or hashes from runtime results.

use nu_protocol::Value;
use serde_json::{json, Map, Value as JsonValue};

use super::stone_containers::{label_of, ItemRef};
use super::stone_provenance::Provenance;
use super::stone_runtime_value::RuntimeValue;

/// Fields of runtime results that differ between two runs of the same
/// command. They stay in the trace and out of the prompt.
const VOLATILE_HOST_FIELDS: &[&str] = &[
    "seq",
    "transition_id",
    "transition_ids",
    "duration_ms",
    "sha256",
];

/// Most inputs on one message.
pub(super) const MAX_INPUTS: usize = 32;
/// Largest rendered message, so a forgotten clip fails loudly instead of
/// sending megabytes to the model.
pub(super) const MAX_RENDERED_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 64;

/// The prompt text for `text` with `inputs`: one block per input, in the
/// order given, then the text (long material first, the request last). A
/// string input is shown as it is; anything else as indented JSON.
pub(super) fn render(text: &str, inputs: &[(String, RuntimeValue)]) -> Result<String, String> {
    let mut out = String::new();
    for (name, value) in inputs {
        let body = match value {
            RuntimeValue::Nu(Value::String { val, .. }) => val.clone(),
            RuntimeValue::Str(text) => text.text.clone(),
            other => serde_json::to_string_pretty(&to_json(other, 0)?)
                .map_err(|error| format!("input `{name}` could not be shown as JSON: {error}"))?,
        };
        out.push_str("<input name=\"");
        out.push_str(name);
        out.push_str("\" type=\"");
        out.push_str(&type_name(value));
        out.push_str("\">\n");
        out.push_str(&body);
        out.push_str("\n</input>\n\n");
    }
    out.push_str(text);
    if out.len() > MAX_RENDERED_BYTES {
        return Err(format!(
            "the message with its inputs is {} bytes, more than {MAX_RENDERED_BYTES}; pass a shorter part of the largest input (a slice of the text, or the fields the step needs)",
            out.len()
        ));
    }
    Ok(out)
}

/// Check an input name: a Python identifier, so it reads as a name in the
/// prompt and in `model.last`.
pub(super) fn check_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid = matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(format!(
            "input name {name:?} must be an identifier, such as \"issue\" or \"fix_report\""
        ))
    }
}

/// The type an input is shown with: a record's class name, otherwise the
/// Python type name.
pub(super) fn type_name(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::Record(record) => record.ty.name.clone(),
        RuntimeValue::Str(_) => "str".to_string(),
        RuntimeValue::List(_) => "list".to_string(),
        RuntimeValue::Dict(_) => "dict".to_string(),
        RuntimeValue::Tuple(_) => "tuple".to_string(),
        RuntimeValue::Set(_) => "set".to_string(),
        RuntimeValue::Nu(value) => match value {
            Value::String { .. } => "str",
            Value::Int { .. } => "int",
            Value::Float { .. } => "float",
            Value::Bool { .. } => "bool",
            Value::Nothing { .. } => "None",
            Value::List { .. } => "list",
            Value::Record { .. } => "dict",
            _ => "value",
        }
        .to_string(),
        _ => "value".to_string(),
    }
}

/// Where an input came from:
/// - `{"kind": "runtime", "type": ...}` for a result only the runtime makes
///   (`run`, `run_tests`, `snapshot`, ...), or a list of them;
/// - `{"kind": "model", "model": ..., "transition_id": ...}` for a value
///   that carries one model call's label: an answer, a record, container, or
///   string inside one, or a string built from it (stone_provenance);
/// - `{"kind": "mixed", "sources": [...]}` for a value built from several;
/// - `{"kind": "program"}` for anything else, such as a container the
///   program built (a container's own label is what counts here).
pub(super) fn source(value: &RuntimeValue, provenance: &Provenance) -> JsonValue {
    if let RuntimeValue::Record(record) = value {
        if record.ty.host {
            return json!({"kind": "runtime", "type": record.ty.name});
        }
    }
    let mut sources = provenance.sources_of(label_of(value));
    match sources.len() {
        0 => {}
        1 => return sources.remove(0),
        _ => return json!({"kind": "mixed", "sources": sources}),
    }
    if let RuntimeValue::List(list) = value {
        let mut host_type = None;
        let mut all_host = list.len() > 0;
        let _ = list.try_for_each(|_, item| -> Result<(), ()> {
            match item {
                ItemRef::Shared(RuntimeValue::Record(record)) if record.ty.host => {
                    host_type.get_or_insert_with(|| record.ty.name.clone());
                }
                _ => all_host = false,
            }
            Ok(())
        });
        if all_host {
            if let Some(name) = host_type {
                return json!({"kind": "runtime", "type": format!("list[{name}]")});
            }
        }
    }
    json!({"kind": "program"})
}

/// An input value as JSON, deterministically: record fields in declaration
/// order (volatile runtime fields left out), dict keys in insertion order.
fn to_json(value: &RuntimeValue, depth: usize) -> Result<JsonValue, String> {
    if depth > MAX_DEPTH {
        return Err(format!("an input is nested more than {MAX_DEPTH} levels deep"));
    }
    let item = |item: ItemRef<'_>| -> Result<JsonValue, String> {
        match item {
            ItemRef::Plain(value) => Ok(crate::json::nu_to_json_value(value)),
            ItemRef::Shared(value) => to_json(&value, depth + 1),
        }
    };
    match value {
        RuntimeValue::Nu(value) => Ok(crate::json::nu_to_json_value(value)),
        RuntimeValue::Str(text) => Ok(JsonValue::String(text.text.clone())),
        RuntimeValue::Record(record) => {
            let mut object = Map::new();
            record.try_for_each(|index, value| {
                let name = &record.ty.fields[index].name;
                if !(record.ty.host && VOLATILE_HOST_FIELDS.contains(&name.as_str())) {
                    object.insert(name.clone(), item(value)?);
                }
                Ok::<(), String>(())
            })?;
            Ok(JsonValue::Object(object))
        }
        RuntimeValue::List(list) => {
            let mut items = Vec::with_capacity(list.len());
            list.try_for_each(|_, value| {
                items.push(item(value)?);
                Ok::<(), String>(())
            })?;
            Ok(JsonValue::Array(items))
        }
        RuntimeValue::Dict(dict) => {
            let mut object = Map::new();
            dict.try_for_each(|key, value| {
                object.insert(key.to_string(), item(value)?);
                Ok::<(), String>(())
            })?;
            Ok(JsonValue::Object(object))
        }
        RuntimeValue::Tuple(_) | RuntimeValue::Set(_) => {
            let value = super::stone_containers::materialize(value, "model input")
                .map_err(|error| error.to_string())?;
            Ok(crate::json::nu_to_json_value(&value))
        }
        other => Err(format!(
            "{} cannot be a model input; pass data: strings, numbers, lists, dicts, records, or runtime results",
            super::stone_records::runtime_kind(other)
        )),
    }
}
