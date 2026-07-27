// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashSet;

use serde_json::{json, Map, Value as JsonValue};

const MAX_SCHEMA_DEPTH: usize = 32;
const MAX_SCHEMA_BRANCHES: usize = 32;
const MAX_SCHEMA_KEY_BYTES: usize = 128;
pub(crate) const MAX_VALIDATION_ISSUES: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidationIssue {
    pub(crate) path: String,
    pub(crate) keyword: String,
    pub(crate) message: String,
}

impl ValidationIssue {
    pub(crate) fn to_json(&self) -> JsonValue {
        json!({
            "path": self.path,
            "keyword": self.keyword,
            "message": self.message,
        })
    }
}

pub(crate) fn validate_schema_definition(schema: &JsonValue) -> Result<(), String> {
    validate_schema_node(schema, "$", 0)
}

pub(crate) fn validate_instance(schema: &JsonValue, instance: &JsonValue) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    validate_instance_node(schema, instance, "$", 0, &mut issues);
    issues
}

fn validate_schema_node(schema: &JsonValue, path: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!(
            "{path} exceeds the supported schema depth of {MAX_SCHEMA_DEPTH}"
        ));
    }
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path} must be a schema object"))?;

    for keyword in object.keys() {
        if !matches!(
            keyword.as_str(),
            "$schema"
                | "$id"
                | "title"
                | "description"
                | "type"
                | "properties"
                | "required"
                | "additionalProperties"
                | "items"
                | "minItems"
                | "maxItems"
                | "minLength"
                | "maxLength"
                | "minimum"
                | "maximum"
                | "enum"
                | "const"
                | "oneOf"
                | "anyOf"
        ) {
            return Err(format!(
                "{path} uses unsupported JSON Schema keyword {keyword:?}"
            ));
        }
    }

    if let Some(value) = object.get("type") {
        let kind = value
            .as_str()
            .ok_or_else(|| format!("{path}.type must be one string"))?;
        if !matches!(
            kind,
            "object" | "array" | "string" | "integer" | "number" | "boolean" | "null"
        ) {
            return Err(format!("{path}.type has unsupported value {kind:?}"));
        }
    }

    if let Some(value) = object.get("properties") {
        let properties = value
            .as_object()
            .ok_or_else(|| format!("{path}.properties must be an object"))?;
        for (name, child) in properties {
            if name.len() > MAX_SCHEMA_KEY_BYTES {
                return Err(format!(
                    "{path}.properties contains a name longer than {MAX_SCHEMA_KEY_BYTES} bytes"
                ));
            }
            validate_schema_node(child, &property_path(path, name), depth + 1)?;
        }
    }

    if let Some(value) = object.get("required") {
        let required = value
            .as_array()
            .ok_or_else(|| format!("{path}.required must be a list of strings"))?;
        let mut seen = HashSet::new();
        for name in required {
            let name = name
                .as_str()
                .ok_or_else(|| format!("{path}.required must contain only strings"))?;
            if name.len() > MAX_SCHEMA_KEY_BYTES {
                return Err(format!(
                    "{path}.required contains a name longer than {MAX_SCHEMA_KEY_BYTES} bytes"
                ));
            }
            if !seen.insert(name) {
                return Err(format!("{path}.required contains duplicate {name:?}"));
            }
        }
    }

    if let Some(value) = object.get("additionalProperties") {
        if !value.is_boolean() {
            return Err(format!(
                "{path}.additionalProperties supports only a boolean"
            ));
        }
    }

    if let Some(child) = object.get("items") {
        validate_schema_node(child, &format!("{path}.items"), depth + 1)?;
    }

    validate_non_negative_integer_pair(object, path, "minItems", "maxItems")?;
    validate_non_negative_integer_pair(object, path, "minLength", "maxLength")?;
    validate_number_pair(object, path, "minimum", "maximum")?;

    if let Some(value) = object.get("enum") {
        let values = value
            .as_array()
            .ok_or_else(|| format!("{path}.enum must be a non-empty list"))?;
        if values.is_empty() {
            return Err(format!("{path}.enum must not be empty"));
        }
    }

    for keyword in ["oneOf", "anyOf"] {
        let Some(value) = object.get(keyword) else {
            continue;
        };
        let branches = value
            .as_array()
            .ok_or_else(|| format!("{path}.{keyword} must be a non-empty list of schemas"))?;
        if branches.is_empty() || branches.len() > MAX_SCHEMA_BRANCHES {
            return Err(format!(
                "{path}.{keyword} must contain between 1 and {MAX_SCHEMA_BRANCHES} schemas"
            ));
        }
        for (index, branch) in branches.iter().enumerate() {
            validate_schema_node(branch, &format!("{path}.{keyword}[{index}]"), depth + 1)?;
        }
    }
    Ok(())
}

fn validate_non_negative_integer_pair(
    object: &Map<String, JsonValue>,
    path: &str,
    minimum: &str,
    maximum: &str,
) -> Result<(), String> {
    let min = optional_usize(object, path, minimum)?;
    let max = optional_usize(object, path, maximum)?;
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            return Err(format!("{path}.{minimum} must not exceed {maximum}"));
        }
    }
    Ok(())
}

fn optional_usize(
    object: &Map<String, JsonValue>,
    path: &str,
    keyword: &str,
) -> Result<Option<usize>, String> {
    let Some(value) = object.get(keyword) else {
        return Ok(None);
    };
    let number = value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("{path}.{keyword} must be a non-negative integer"))?;
    Ok(Some(number))
}

fn validate_number_pair(
    object: &Map<String, JsonValue>,
    path: &str,
    minimum: &str,
    maximum: &str,
) -> Result<(), String> {
    let min = optional_number(object, path, minimum)?;
    let max = optional_number(object, path, maximum)?;
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            return Err(format!("{path}.{minimum} must not exceed {maximum}"));
        }
    }
    Ok(())
}

fn optional_number(
    object: &Map<String, JsonValue>,
    path: &str,
    keyword: &str,
) -> Result<Option<f64>, String> {
    let Some(value) = object.get(keyword) else {
        return Ok(None);
    };
    let number = value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("{path}.{keyword} must be a finite number"))?;
    Ok(Some(number))
}

fn validate_instance_node(
    schema: &JsonValue,
    instance: &JsonValue,
    path: &str,
    depth: usize,
    issues: &mut Vec<ValidationIssue>,
) {
    if issues.len() >= MAX_VALIDATION_ISSUES || depth > MAX_SCHEMA_DEPTH {
        return;
    }
    let Some(object) = schema.as_object() else {
        return;
    };

    if let Some(expected) = object.get("const") {
        if instance != expected {
            push_issue(issues, path, "const", "value does not equal const");
        }
    }
    if let Some(allowed) = object.get("enum").and_then(JsonValue::as_array) {
        if !allowed.iter().any(|candidate| candidate == instance) {
            push_issue(
                issues,
                path,
                "enum",
                "value is not one of the allowed values",
            );
        }
    }

    if let Some(kind) = object.get("type").and_then(JsonValue::as_str) {
        if !instance_has_type(instance, kind) {
            push_issue(
                issues,
                path,
                "type",
                &format!("expected {kind}, got {}", instance_type(instance)),
            );
            return;
        }
    }

    if let Some(branches) = object.get("oneOf").and_then(JsonValue::as_array) {
        let matches = branches
            .iter()
            .filter(|branch| validate_instance(branch, instance).is_empty())
            .count();
        if matches != 1 {
            push_issue(
                issues,
                path,
                "oneOf",
                &format!("expected exactly one matching schema, found {matches}"),
            );
        }
    }
    if let Some(branches) = object.get("anyOf").and_then(JsonValue::as_array) {
        if !branches
            .iter()
            .any(|branch| validate_instance(branch, instance).is_empty())
        {
            push_issue(issues, path, "anyOf", "value matches no allowed schema");
        }
    }

    if let Some(value) = instance.as_object() {
        let properties = object.get("properties").and_then(JsonValue::as_object);
        if let Some(required) = object.get("required").and_then(JsonValue::as_array) {
            for name in required.iter().filter_map(JsonValue::as_str) {
                if !value.contains_key(name) {
                    push_issue(
                        issues,
                        &property_path(path, name),
                        "required",
                        "required property is missing",
                    );
                }
            }
        }
        for (name, child) in value {
            if let Some(property_schema) = properties.and_then(|schemas| schemas.get(name)) {
                validate_instance_node(
                    property_schema,
                    child,
                    &property_path(path, name),
                    depth + 1,
                    issues,
                );
            } else if object
                .get("additionalProperties")
                .and_then(JsonValue::as_bool)
                == Some(false)
            {
                push_issue(
                    issues,
                    &property_path(path, name),
                    "additionalProperties",
                    "additional property is not allowed",
                );
            }
        }
    }

    if let Some(values) = instance.as_array() {
        if let Some(minimum) = object.get("minItems").and_then(JsonValue::as_u64) {
            if values.len() < minimum as usize {
                push_issue(
                    issues,
                    path,
                    "minItems",
                    &format!("expected at least {minimum} items"),
                );
            }
        }
        if let Some(maximum) = object.get("maxItems").and_then(JsonValue::as_u64) {
            if values.len() > maximum as usize {
                push_issue(
                    issues,
                    path,
                    "maxItems",
                    &format!("expected at most {maximum} items"),
                );
            }
        }
        if let Some(item_schema) = object.get("items") {
            for (index, value) in values.iter().enumerate() {
                validate_instance_node(
                    item_schema,
                    value,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    issues,
                );
                if issues.len() >= MAX_VALIDATION_ISSUES {
                    break;
                }
            }
        }
    }

    if let Some(value) = instance.as_str() {
        let length = value.chars().count();
        if let Some(minimum) = object.get("minLength").and_then(JsonValue::as_u64) {
            if length < minimum as usize {
                push_issue(
                    issues,
                    path,
                    "minLength",
                    &format!("expected at least {minimum} characters"),
                );
            }
        }
        if let Some(maximum) = object.get("maxLength").and_then(JsonValue::as_u64) {
            if length > maximum as usize {
                push_issue(
                    issues,
                    path,
                    "maxLength",
                    &format!("expected at most {maximum} characters"),
                );
            }
        }
    }

    if let Some(value) = instance.as_f64() {
        if let Some(minimum) = object.get("minimum").and_then(JsonValue::as_f64) {
            if value < minimum {
                push_issue(
                    issues,
                    path,
                    "minimum",
                    &format!("expected a value of at least {minimum}"),
                );
            }
        }
        if let Some(maximum) = object.get("maximum").and_then(JsonValue::as_f64) {
            if value > maximum {
                push_issue(
                    issues,
                    path,
                    "maximum",
                    &format!("expected a value of at most {maximum}"),
                );
            }
        }
    }
}

fn push_issue(issues: &mut Vec<ValidationIssue>, path: &str, keyword: &str, message: &str) {
    if issues.len() < MAX_VALIDATION_ISSUES {
        issues.push(ValidationIssue {
            path: path.to_string(),
            keyword: keyword.to_string(),
            message: message.to_string(),
        });
    }
}

fn instance_has_type(value: &JsonValue, kind: &str) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value
            .as_f64()
            .is_some_and(|number| number.is_finite() && number.fract() == 0.0),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn instance_type(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

fn property_path(parent: &str, name: &str) -> String {
    if name
        .chars()
        .all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        format!("{parent}.{name}")
    } else {
        format!(
            "{parent}[{}]",
            serde_json::to_string(name).unwrap_or_default()
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{validate_instance, validate_schema_definition};

    #[test]
    fn strict_object_schema_reports_bounded_paths() {
        let schema = json!({
            "type": "object",
            "properties": {
                "actions": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {"tool": {"type": "string", "enum": ["read", "write"]}},
                        "required": ["tool"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["actions"],
            "additionalProperties": false
        });
        validate_schema_definition(&schema).unwrap();

        let issues = validate_instance(
            &schema,
            &json!({"actions": [{"tool": "delete", "secret": true}]}),
        );
        assert_eq!(issues.len(), 2);
        assert!(issues
            .iter()
            .any(|issue| issue.path == "$.actions[0].tool" && issue.keyword == "enum"));
        assert!(issues.iter().any(|issue| {
            issue.path == "$.actions[0].secret" && issue.keyword == "additionalProperties"
        }));
    }

    #[test]
    fn unsupported_keywords_fail_closed() {
        let error = validate_schema_definition(&json!({
            "type": "string",
            "pattern": "ready"
        }))
        .unwrap_err();
        assert!(error.contains("unsupported JSON Schema keyword \"pattern\""));
    }

    #[test]
    fn one_of_requires_exactly_one_branch() {
        let schema = json!({
            "oneOf": [
                {"type": "object", "properties": {"kind": {"const": "run"}}, "required": ["kind"]},
                {"type": "object", "properties": {"kind": {"const": "finish"}}, "required": ["kind"]}
            ]
        });
        validate_schema_definition(&schema).unwrap();
        assert!(validate_instance(&schema, &json!({"kind": "run"})).is_empty());
        assert_eq!(
            validate_instance(&schema, &json!({"kind": "other"}))[0].keyword,
            "oneOf"
        );
    }
}
