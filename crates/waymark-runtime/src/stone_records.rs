// SPDX-License-Identifier: MIT OR Apache-2.0

//! Record types and typed values (design L5).
//!
//! `class Plan(Record):` with typed fields defines a record type. A record is
//! a fixed set of slots in declaration order (`RecordObj` in
//! `stone_containers`), shared like a dict. Its fields are checked when the
//! record is built and when a field is assigned. Nested lists and dicts are
//! not re-checked when they are later changed in place, as in pydantic.
//!
//! Types written in annotations (`TypeExpr`) are resolved against a table of
//! record types and type aliases (`TypeTable`) when they are first used, so
//! a field may name a record defined later in the file, or the record itself.
//!
//! Two operations use a resolved type (`TypeSpec`):
//!
//! - `check`: a value made by Stone code must already have the type. Nothing
//!   is converted, so the value keeps its identity. An `int` is accepted
//!   where a `float` is expected, as in Python's typing.
//! - `convert_json`: a JSON value from a model is turned into a typed value.
//!   Objects become records, integers become floats where a float is
//!   expected, and every problem is reported with its path so the model can
//!   repair its answer.
//!
//! `json_schema` derives the JSON Schema shown to the model from a type.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

use nu_protocol::{ShellError, Span, Value};
use serde_json::{json, Map as JsonMap, Value as JsonValue};

use super::stone_containers::{dict_from_entries, list_from_values, record_obj, ItemRef};
use super::stone_runtime_value::RuntimeValue;
use crate::json::json_to_nu_value;
use crate::stone_ast::{Expr, FieldLimits, LiteralValue, RecordFieldDef, TypeExpr};
use crate::stone_builtins::value_type_name;

/// Most problems reported for one model answer.
const MAX_ISSUES: usize = 20;

pub(super) struct RecordType {
    pub(super) name: String,
    pub(super) doc: Option<String>,
    pub(super) fields: Vec<RecordFieldDef>,
    resolved: OnceCell<Vec<TypeSpec>>,
}

impl RecordType {
    pub(super) fn new(name: String, doc: Option<String>, fields: Vec<RecordFieldDef>) -> Self {
        Self {
            name,
            doc,
            fields,
            resolved: OnceCell::new(),
        }
    }

    pub(super) fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|field| field.name == name)
    }

    pub(super) fn field_names(&self) -> String {
        if self.fields.is_empty() {
            return "none".to_string();
        }
        self.fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(super) fn no_field_message(&self, name: &str) -> String {
        format!(
            "record `{}` has no field `{name}`; its fields are: {}",
            self.name,
            self.field_names()
        )
    }

    /// The field types, resolved on first use.
    pub(super) fn field_types(&self, table: &TypeTable) -> Result<&[TypeSpec], ShellError> {
        if let Some(types) = self.resolved.get() {
            return Ok(types);
        }
        let types = self
            .fields
            .iter()
            .map(|field| {
                table.resolve(&field.ty).map_err(|message| {
                    record_error(format!(
                        "record `{}` field `{}`: {message}",
                        self.name, field.name
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.resolved.get_or_init(|| types))
    }
}

/// A resolved type.
#[derive(Clone)]
pub(super) enum TypeSpec {
    Any,
    Str,
    Int,
    Float,
    Bool,
    None,
    List(Box<TypeSpec>),
    Dict(Box<TypeSpec>),
    Literal(Rc<Vec<LiteralValue>>),
    Union(Rc<Vec<TypeSpec>>),
    Record(Rc<RecordType>),
    Model,
}

impl TypeSpec {
    /// `A | B`, flattened.
    pub(super) fn union(left: TypeSpec, right: TypeSpec) -> TypeSpec {
        let mut members = Vec::new();
        for side in [left, right] {
            match side {
                TypeSpec::Union(parts) => members.extend(parts.iter().cloned()),
                other => members.push(other),
            }
        }
        let mut unique: Vec<TypeSpec> = Vec::new();
        for member in members {
            if !unique.iter().any(|seen| same_type(seen, &member)) {
                unique.push(member);
            }
        }
        if unique.len() == 1 {
            unique.pop().expect("one member")
        } else {
            TypeSpec::Union(Rc::new(unique))
        }
    }

    /// How the type is written in Stone source.
    pub(super) fn display(&self) -> String {
        match self {
            TypeSpec::Any => "Any".to_string(),
            TypeSpec::Str => "str".to_string(),
            TypeSpec::Int => "int".to_string(),
            TypeSpec::Float => "float".to_string(),
            TypeSpec::Bool => "bool".to_string(),
            TypeSpec::None => "None".to_string(),
            TypeSpec::List(element) => match **element {
                TypeSpec::Any => "list".to_string(),
                ref element => format!("list[{}]", element.display()),
            },
            TypeSpec::Dict(value) => match **value {
                TypeSpec::Any => "dict".to_string(),
                ref value => format!("dict[str, {}]", value.display()),
            },
            TypeSpec::Literal(values) => format!(
                "Literal[{}]",
                values
                    .iter()
                    .map(literal_display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            TypeSpec::Union(members) => members
                .iter()
                .map(TypeSpec::display)
                .collect::<Vec<_>>()
                .join(" | "),
            TypeSpec::Record(record) => record.name.clone(),
            TypeSpec::Model => "Model".to_string(),
        }
    }
}

fn same_type(left: &TypeSpec, right: &TypeSpec) -> bool {
    match (left, right) {
        (TypeSpec::Record(left), TypeSpec::Record(right)) => Rc::ptr_eq(left, right),
        (TypeSpec::List(left), TypeSpec::List(right))
        | (TypeSpec::Dict(left), TypeSpec::Dict(right)) => same_type(left, right),
        (TypeSpec::Literal(left), TypeSpec::Literal(right)) => left == right,
        (TypeSpec::Union(left), TypeSpec::Union(right)) => {
            left.len() == right.len() && left.iter().zip(right.iter()).all(|(l, r)| same_type(l, r))
        }
        _ => std::mem::discriminant(left) == std::mem::discriminant(right),
    }
}

fn literal_display(value: &LiteralValue) -> String {
    match value {
        LiteralValue::Str(text) => format!("{text:?}"),
        LiteralValue::Int(value) => value.to_string(),
        LiteralValue::Bool(true) => "True".to_string(),
        LiteralValue::Bool(false) => "False".to_string(),
    }
}

/// Record types and type aliases by name. Like error types, record names
/// are global: a module's records keep their names when imported.
#[derive(Clone, Default)]
pub(crate) struct TypeTable {
    entries: HashMap<String, TypeSpec>,
}

impl TypeTable {
    pub(super) fn get(&self, name: &str) -> Option<&TypeSpec> {
        self.entries.get(name)
    }

    pub(super) fn record(&self, name: &str) -> Option<&Rc<RecordType>> {
        match self.entries.get(name) {
            Some(TypeSpec::Record(record)) if record.name == name => Some(record),
            _ => None,
        }
    }

    pub(super) fn define(&mut self, name: String, spec: TypeSpec) {
        self.entries.insert(name, spec);
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    pub(super) fn resolve(&self, expr: &TypeExpr) -> Result<TypeSpec, String> {
        Ok(match expr {
            TypeExpr::Any => TypeSpec::Any,
            TypeExpr::Str => TypeSpec::Str,
            TypeExpr::Int => TypeSpec::Int,
            TypeExpr::Float => TypeSpec::Float,
            TypeExpr::Bool => TypeSpec::Bool,
            TypeExpr::None => TypeSpec::None,
            TypeExpr::List(element) => TypeSpec::List(Box::new(self.resolve(element)?)),
            TypeExpr::Dict(value) => TypeSpec::Dict(Box::new(self.resolve(value)?)),
            TypeExpr::Literal(values) => TypeSpec::Literal(Rc::new(values.clone())),
            TypeExpr::Model => TypeSpec::Model,
            TypeExpr::Union(members) => {
                let mut spec = self.resolve(&members[0])?;
                for member in &members[1..] {
                    spec = TypeSpec::union(spec, self.resolve(member)?);
                }
                spec
            }
            TypeExpr::Named(name) => self.get(name).cloned().ok_or_else(|| {
                format!(
                    "unknown type `{name}`. Types are str, int, float, bool, None, Any, list[T], dict[str, T], Literal[...], A | B, a record class (`class {name}(Record):`), or an alias assigned at the top level (`{name} = A | B`)"
                )
            })?,
        })
    }
}

/// One problem with a value, at a path such as `steps[2]` or `plan.risky`.
#[derive(Clone, Debug)]
pub(super) struct TypeIssue {
    pub(super) path: String,
    pub(super) message: String,
}

impl TypeIssue {
    fn new(message: impl Into<String>) -> Self {
        Self {
            path: String::new(),
            message: message.into(),
        }
    }

    fn at_index(mut self, index: usize) -> Self {
        self.path = format!("[{index}]{}", self.path);
        self
    }

    fn at_key(mut self, key: &str) -> Self {
        self.path = format!(".{key}{}", self.path);
        self
    }

    /// `name.path: message`, with `name` for the value as a whole.
    pub(super) fn describe(&self, name: &str) -> String {
        format!("{name}{}: {}", self.path, self.message)
    }

    pub(super) fn to_json(&self) -> JsonValue {
        let path = if self.path.is_empty() {
            "$".to_string()
        } else {
            format!("${}", self.path)
        };
        json!({"path": path, "message": self.message})
    }
}

pub(super) fn record_error(message: impl Into<String>) -> ShellError {
    super::stone_error("record", message)
}

// ---------------------------------------------------------------------------
// Checking values made by Stone code.

/// Whether `value` already has type `ty`. Nothing is converted.
pub(super) fn check(
    value: &RuntimeValue,
    ty: &TypeSpec,
    table: &TypeTable,
) -> Result<(), TypeIssue> {
    match value {
        RuntimeValue::Nu(value) => check_plain(value, ty),
        RuntimeValue::List(list) => match ty {
            TypeSpec::Any => Ok(()),
            TypeSpec::List(element) => list.try_for_each(|index, item| {
                check_item(item, element, table).map_err(|issue| issue.at_index(index))
            }),
            TypeSpec::Union(members) => check_union(value, members, table),
            _ => Err(mismatch(ty, "list")),
        },
        RuntimeValue::Dict(dict) => match ty {
            TypeSpec::Any => Ok(()),
            TypeSpec::Dict(element) => dict.try_for_each(|key, item| {
                check_item(item, element, table).map_err(|issue| issue.at_key(key))
            }),
            TypeSpec::Union(members) => check_union(value, members, table),
            TypeSpec::Record(record) => Err(dict_for_record(record)),
            _ => Err(mismatch(ty, "dict")),
        },
        RuntimeValue::Model(_) => match ty {
            TypeSpec::Any | TypeSpec::Model => Ok(()),
            TypeSpec::Union(members) => check_union(value, members, table),
            _ => Err(mismatch(ty, "Model")),
        },
        RuntimeValue::Record(record) => match ty {
            TypeSpec::Any => Ok(()),
            TypeSpec::Record(expected) if Rc::ptr_eq(expected, &record.ty) => Ok(()),
            TypeSpec::Union(members) => check_union(value, members, table),
            _ => Err(mismatch(ty, &record.ty.name)),
        },
        other => match ty {
            TypeSpec::Any => Ok(()),
            TypeSpec::Union(members) => check_union(value, members, table),
            _ => match other.clone().into_nu_value("type check") {
                Ok(plain) if is_plain_view(other) => check_plain(&plain, ty),
                _ => Err(mismatch(ty, &runtime_kind(other))),
            },
        },
    }
}

/// JSON and text views stand for plain values; other runtime values do not.
fn is_plain_view(value: &RuntimeValue) -> bool {
    matches!(
        value,
        RuntimeValue::TextLines(_)
            | RuntimeValue::JsonlRows(_)
            | RuntimeValue::JsonObjectView(_)
            | RuntimeValue::JsonArrayView(_)
            | RuntimeValue::JsonScalarView(_)
    )
}

fn check_item(item: ItemRef<'_>, ty: &TypeSpec, table: &TypeTable) -> Result<(), TypeIssue> {
    match item {
        ItemRef::Plain(value) => check_plain(value, ty),
        ItemRef::Shared(value) => check(&value, ty, table),
    }
}

fn check_union(
    value: &RuntimeValue,
    members: &[TypeSpec],
    table: &TypeTable,
) -> Result<(), TypeIssue> {
    if members
        .iter()
        .any(|member| check(value, member, table).is_ok())
    {
        return Ok(());
    }
    Err(mismatch(
        &TypeSpec::Union(Rc::new(members.to_vec())),
        &runtime_kind(value),
    ))
}

fn check_plain(value: &Value, ty: &TypeSpec) -> Result<(), TypeIssue> {
    let ok = match ty {
        TypeSpec::Any => true,
        TypeSpec::Str => matches!(value, Value::String { .. } | Value::Glob { .. }),
        TypeSpec::Int => matches!(value, Value::Int { .. }),
        TypeSpec::Float => matches!(value, Value::Int { .. } | Value::Float { .. }),
        TypeSpec::Bool => matches!(value, Value::Bool { .. }),
        TypeSpec::None => matches!(value, Value::Nothing { .. }),
        TypeSpec::List(element) => {
            let Value::List { vals, .. } = value else {
                return Err(mismatch(ty, value_type_name(value)));
            };
            for (index, item) in vals.iter().enumerate() {
                check_plain(item, element).map_err(|issue| issue.at_index(index))?;
            }
            true
        }
        TypeSpec::Dict(element) => {
            let Value::Record { val, .. } = value else {
                return Err(mismatch(ty, value_type_name(value)));
            };
            for (key, item) in val.iter() {
                check_plain(item, element).map_err(|issue| issue.at_key(key))?;
            }
            true
        }
        TypeSpec::Literal(values) => {
            if values.iter().any(|literal| literal_matches(literal, value)) {
                true
            } else {
                return Err(TypeIssue::new(format!(
                    "expected {}, got {}",
                    ty.display(),
                    plain_preview(value)
                )));
            }
        }
        TypeSpec::Union(members) => {
            if members
                .iter()
                .any(|member| check_plain(value, member).is_ok())
            {
                true
            } else {
                return Err(TypeIssue::new(format!(
                    "expected {}, got {}",
                    ty.display(),
                    plain_preview(value)
                )));
            }
        }
        TypeSpec::Record(record) => {
            if matches!(value, Value::Record { .. }) {
                return Err(dict_for_record(record));
            }
            false
        }
        TypeSpec::Model => false,
    };
    if ok {
        Ok(())
    } else {
        Err(mismatch(ty, value_type_name(value)))
    }
}

fn dict_for_record(record: &RecordType) -> TypeIssue {
    TypeIssue::new(format!(
        "expected {name}, got dict; build the record with `{name}(field=value, ...)`",
        name = record.name
    ))
}

fn mismatch(ty: &TypeSpec, got: &str) -> TypeIssue {
    TypeIssue::new(format!("expected {}, got {got}", ty.display()))
}

fn literal_matches(literal: &LiteralValue, value: &Value) -> bool {
    match (literal, value) {
        (LiteralValue::Str(expected), Value::String { val, .. }) => expected == val,
        (LiteralValue::Int(expected), Value::Int { val, .. }) => expected == val,
        (LiteralValue::Bool(expected), Value::Bool { val, .. }) => expected == val,
        _ => false,
    }
}

/// A short view of a value for an error message: the value itself for
/// scalars, the type name for containers.
fn plain_preview(value: &Value) -> String {
    match value {
        Value::String { val, .. } => {
            let mut text: String = val.chars().take(40).collect();
            if val.chars().count() > 40 {
                text.push_str("...");
            }
            format!("{text:?}")
        }
        Value::Int { val, .. } => val.to_string(),
        Value::Bool { val: true, .. } => "True".to_string(),
        Value::Bool { val: false, .. } => "False".to_string(),
        Value::Nothing { .. } => "None".to_string(),
        other => value_type_name(other).to_string(),
    }
}

/// The type name of a runtime value, as `type()` reports it.
pub(super) fn runtime_kind(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::Nu(value) => value_type_name(value).to_string(),
        RuntimeValue::List(_) | RuntimeValue::TextLines(_) | RuntimeValue::JsonlRows(_) => {
            "list".to_string()
        }
        RuntimeValue::JsonArrayView(_) => "list".to_string(),
        RuntimeValue::Dict(_) | RuntimeValue::JsonObjectView(_) => "dict".to_string(),
        RuntimeValue::Record(record) => record.ty.name.clone(),
        RuntimeValue::Type(_) => "type".to_string(),
        RuntimeValue::Model(_) => "Model".to_string(),
        RuntimeValue::Callable(_) => "function".to_string(),
        _ => "value".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Building records.

/// The value of a literal default (`0`, `""`, `None`, `[]`, `{}`). Defaults
/// are evaluated for each record, so a default list is never shared.
pub(super) fn literal_default(expr: &Expr) -> Result<RuntimeValue, ShellError> {
    fn plain(expr: &Expr) -> Result<Value, ShellError> {
        let span = Span::unknown();
        Ok(match expr {
            Expr::None => Value::nothing(span),
            Expr::Bool(value) => Value::bool(*value, span),
            Expr::Int(text) => Value::int(
                text.parse::<i64>()
                    .map_err(|_| record_error(format!("integer default `{text}` is too large")))?,
                span,
            ),
            Expr::Float(value) => Value::float(*value, span),
            Expr::String(text) => Value::string(text.clone(), span),
            Expr::Neg(inner) => match plain(inner)? {
                Value::Int { val, .. } => Value::int(-val, span),
                Value::Float { val, .. } => Value::float(-val, span),
                _ => return Err(record_error("a negative default must be a number")),
            },
            Expr::List(items) | Expr::Tuple(items) => {
                Value::list(items.iter().map(plain).collect::<Result<_, _>>()?, span)
            }
            Expr::Record(entries) => {
                let mut record = nu_protocol::Record::with_capacity(entries.len());
                for (key, value) in entries {
                    record.insert(key.clone(), plain(value)?);
                }
                Value::record(record, span)
            }
            _ => return Err(record_error("a record field default must be a literal")),
        })
    }
    plain(expr).map(RuntimeValue::Nu)
}

/// `Name(positional..., keyword=...)`: match arguments to fields, fill
/// defaults, and check every value.
pub(super) fn construct(
    record: &Rc<RecordType>,
    positional: Vec<RuntimeValue>,
    named: Vec<(String, RuntimeValue)>,
    table: &TypeTable,
) -> Result<RuntimeValue, ShellError> {
    let name = &record.name;
    let types = record.field_types(table)?;
    if positional.len() > record.fields.len() {
        return Err(record_error(format!(
            "{name}() takes {} field value(s) but {} positional argument(s) were given; its fields are: {}",
            record.fields.len(),
            positional.len(),
            record.field_names()
        )));
    }
    let mut slots: Vec<Option<RuntimeValue>> = vec![None; record.fields.len()];
    for (index, value) in positional.into_iter().enumerate() {
        slots[index] = Some(value);
    }
    for (key, value) in named {
        let Some(index) = record.field_index(&key) else {
            return Err(record_error(format!(
                "{name}() got an unexpected field `{key}`; its fields are: {}",
                record.field_names()
            )));
        };
        if slots[index].is_some() {
            return Err(record_error(format!(
                "{name}() got field `{key}` more than once"
            )));
        }
        slots[index] = Some(value);
    }
    let mut missing = Vec::new();
    let mut values = Vec::with_capacity(slots.len());
    for ((field, slot), ty) in record.fields.iter().zip(slots).zip(types) {
        let value = match slot {
            Some(value) => {
                check(&value, ty, table)
                    .and_then(|()| check_limits(&value, &field.limits).map_err(TypeIssue::new))
                    .map_err(|issue| {
                        record_error(format!(
                            "{name}() field {}",
                            issue
                                .at_key(&field.name)
                                .describe("")
                                .trim_start_matches('.')
                        ))
                    })?;
                value
            }
            None => match &field.default {
                Some(default) => literal_default(default)?,
                None => {
                    missing.push(field.name.as_str());
                    continue;
                }
            },
        };
        values.push(value);
    }
    if !missing.is_empty() {
        return Err(record_error(format!(
            "{name}() is missing required field(s): {}",
            missing.join(", ")
        )));
    }
    Ok(record_obj(record.clone(), values))
}

/// A `Field(...)` limit, checked after the value's type. Number bounds
/// apply to ints and floats, length bounds to strings (in characters),
/// lists, and dicts; other values, such as `None`, pass.
pub(super) fn check_limits(value: &RuntimeValue, limits: &FieldLimits) -> Result<(), String> {
    if limits.has_number_bounds() {
        let number = match value {
            RuntimeValue::Nu(Value::Int { val, .. }) => Some((*val as f64, val.to_string())),
            RuntimeValue::Nu(Value::Float { val, .. }) => {
                Some((*val, crate::stone_builtins::python_float_repr(*val)))
            }
            _ => None,
        };
        if let Some((number, got)) = number {
            let broken = [
                (limits.ge.filter(|bound| number < *bound), "at least"),
                (limits.gt.filter(|bound| number <= *bound), "more than"),
                (limits.le.filter(|bound| number > *bound), "at most"),
                (limits.lt.filter(|bound| number >= *bound), "less than"),
            ];
            if let Some((Some(bound), words)) =
                broken.into_iter().find(|(bound, _)| bound.is_some())
            {
                return Err(format!(
                    "expected {words} {}, got {got}",
                    format_number_bound(bound)
                ));
            }
        }
    }
    if limits.has_length_bounds() {
        let measured = match value {
            RuntimeValue::Nu(Value::String { val, .. }) => Some((val.chars().count(), "character")),
            RuntimeValue::Nu(Value::List { vals, .. }) => Some((vals.len(), "item")),
            RuntimeValue::Nu(Value::Record { val, .. }) => Some((val.len(), "key")),
            RuntimeValue::List(list) => Some((list.len(), "item")),
            RuntimeValue::Dict(dict) => Some((dict.len(), "key")),
            _ => None,
        };
        if let Some((length, unit)) = measured {
            let count = |n: u64| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
            if let Some(min) = limits.min_length.filter(|min| (length as u64) < *min) {
                return Err(format!("expected at least {}, got {length}", count(min)));
            }
            if let Some(max) = limits.max_length.filter(|max| length as u64 > *max) {
                return Err(format!("expected at most {}, got {length}", count(max)));
            }
        }
    }
    Ok(())
}

/// Write `Field(...)` limits into a field's JSON Schema with the standard
/// keywords. In an `anyOf` (an `Optional` field), each limit goes on the
/// members it applies to.
fn add_limit_keywords(schema: &mut JsonValue, limits: &FieldLimits) {
    fn apply(schema: &mut JsonValue, limits: &FieldLimits) {
        let Some(object) = schema.as_object_mut() else {
            return;
        };
        if let Some(JsonValue::Array(members)) = object.get_mut("anyOf") {
            for member in members {
                apply(member, limits);
            }
            return;
        }
        let number = |bound: f64| {
            if bound.fract() == 0.0 && bound.abs() < 9.0e15 {
                json!(bound as i64)
            } else {
                json!(bound)
            }
        };
        match object.get("type").and_then(JsonValue::as_str) {
            Some("integer" | "number") => {
                for (key, bound) in [
                    ("minimum", limits.ge),
                    ("exclusiveMinimum", limits.gt),
                    ("maximum", limits.le),
                    ("exclusiveMaximum", limits.lt),
                ] {
                    if let Some(bound) = bound {
                        object.insert(key.to_string(), number(bound));
                    }
                }
            }
            Some(kind @ ("string" | "array" | "object")) => {
                let (min_key, max_key) = match kind {
                    "string" => ("minLength", "maxLength"),
                    "array" => ("minItems", "maxItems"),
                    _ => ("minProperties", "maxProperties"),
                };
                if let Some(min) = limits.min_length {
                    object.insert(min_key.to_string(), json!(min));
                }
                if let Some(max) = limits.max_length {
                    object.insert(max_key.to_string(), json!(max));
                }
            }
            _ => {}
        }
    }
    apply(schema, limits);
    if let (Some(description), Some(object)) = (&limits.description, schema.as_object_mut()) {
        object.insert("description".to_string(), json!(description));
    }
}

/// A bound as Python prints it: `120000`, `0.5`.
fn format_number_bound(bound: f64) -> String {
    if bound.fract() == 0.0 && bound.abs() < 9.0e15 {
        format!("{}", bound as i64)
    } else {
        format!("{bound}")
    }
}

/// `record.field = value`, checked against the field's type.
pub(super) fn assign_field(
    value: &RuntimeValue,
    field: &str,
    new_value: RuntimeValue,
    table: &TypeTable,
) -> Result<(), ShellError> {
    let RuntimeValue::Record(record) = value else {
        let kind = runtime_kind(value);
        return Err(super::stone_error(
            "attribute",
            if kind == "dict" {
                format!("cannot set attribute `{field}` on a dict; use item assignment: value[\"{field}\"] = ...")
            } else {
                format!(
                    "cannot set attribute `{field}` on {kind}: only record fields can be assigned"
                )
            },
        ));
    };
    let Some(index) = record.ty.field_index(field) else {
        return Err(super::stone_error(
            "attribute",
            record.ty.no_field_message(field),
        ));
    };
    super::stone_containers::ensure_mutable(value, "assignment")?;
    let ty = &record.ty.field_types(table)?[index];
    let limits = &record.ty.fields[index].limits;
    check(&new_value, ty, table)
        .and_then(|()| check_limits(&new_value, limits).map_err(TypeIssue::new))
        .map_err(|issue| {
            record_error(format!(
                "cannot set {}.{field}: {}{}",
                record.ty.name,
                issue.path.trim_start_matches('.'),
                if issue.path.is_empty() {
                    issue.message.clone()
                } else {
                    format!(": {}", issue.message)
                }
            ))
        })?;
    record.set(index, new_value);
    Ok(())
}

/// Python `isinstance` with a record type or a union of types. As in
/// Python, a parameterized type such as `list[str]` cannot be tested.
pub(super) fn isinstance(value: &RuntimeValue, ty: &TypeSpec) -> Result<bool, String> {
    Ok(match ty {
        TypeSpec::Record(record) => {
            matches!(value, RuntimeValue::Record(actual) if Rc::ptr_eq(&actual.ty, record))
        }
        TypeSpec::Union(members) => {
            for member in members.iter() {
                if isinstance(value, member)? {
                    return Ok(true);
                }
            }
            false
        }
        TypeSpec::None => matches!(value, RuntimeValue::Nu(Value::Nothing { .. })),
        TypeSpec::Str => matches!(value, RuntimeValue::Nu(Value::String { .. } | Value::Glob { .. })),
        TypeSpec::Int => matches!(value, RuntimeValue::Nu(Value::Int { .. } | Value::Bool { .. })),
        TypeSpec::Float => matches!(value, RuntimeValue::Nu(Value::Float { .. })),
        TypeSpec::Bool => matches!(value, RuntimeValue::Nu(Value::Bool { .. })),
        TypeSpec::Model => matches!(value, RuntimeValue::Model(_)),
        TypeSpec::List(element) if matches!(**element, TypeSpec::Any) => {
            runtime_kind(value) == "list"
        }
        TypeSpec::Dict(element) if matches!(**element, TypeSpec::Any) => {
            runtime_kind(value) == "dict"
        }
        TypeSpec::Any => true,
        other => {
            return Err(format!(
                "isinstance() cannot test the parameterized type `{}`, as in Python; test the outer type (`list`, `dict`) or a record type",
                other.display()
            ))
        }
    })
}

/// Records compare equal when they have the same type and equal fields, as
/// dataclasses do.
pub(super) fn records_equal(
    left: &super::stone_containers::RecordRef,
    right: &super::stone_containers::RecordRef,
    equal: &mut dyn FnMut(&RuntimeValue, &RuntimeValue) -> Result<bool, ShellError>,
) -> Result<bool, ShellError> {
    if Rc::ptr_eq(left, right) {
        return Ok(true);
    }
    if !Rc::ptr_eq(&left.ty, &right.ty) {
        return Ok(false);
    }
    for (left, right) in left
        .shared_fields()
        .iter()
        .zip(right.shared_fields().iter())
    {
        if !equal(left, right)? {
            return Ok(false);
        }
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Converting model output.

/// Turn a JSON value into a value of type `ty`, collecting every problem.
pub(super) fn convert_json(
    value: &JsonValue,
    ty: &TypeSpec,
    table: &TypeTable,
) -> Result<RuntimeValue, Vec<TypeIssue>> {
    let mut issues = Vec::new();
    match convert(value, ty, table, &mut issues) {
        Some(value) if issues.is_empty() => Ok(value),
        _ => {
            if issues.is_empty() {
                issues.push(TypeIssue::new(format!("expected {}", ty.display())));
            }
            issues.truncate(MAX_ISSUES);
            Err(issues)
        }
    }
}

fn json_kind(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(number) if number.is_i64() || number.is_u64() => "integer",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

fn json_preview(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => {
            let mut preview: String = text.chars().take(40).collect();
            if text.chars().count() > 40 {
                preview.push_str("...");
            }
            format!("{preview:?}")
        }
        JsonValue::Number(_) | JsonValue::Bool(_) | JsonValue::Null => value.to_string(),
        other => json_kind(other).to_string(),
    }
}

fn convert(
    value: &JsonValue,
    ty: &TypeSpec,
    table: &TypeTable,
    issues: &mut Vec<TypeIssue>,
) -> Option<RuntimeValue> {
    let span = Span::unknown();
    let wrong = |issues: &mut Vec<TypeIssue>| {
        issues.push(TypeIssue::new(format!(
            "expected {}, got {}",
            schema_word(ty),
            json_preview(value)
        )));
        None
    };
    match ty {
        TypeSpec::Any => Some(RuntimeValue::Nu(json_to_nu_value(value.clone(), span))),
        TypeSpec::Str => match value {
            JsonValue::String(text) => Some(RuntimeValue::Nu(Value::string(text.clone(), span))),
            _ => wrong(issues),
        },
        TypeSpec::Int => match value {
            JsonValue::Number(number) => {
                if let Some(int) = number.as_i64() {
                    Some(RuntimeValue::Nu(Value::int(int, span)))
                } else if let Some(float) = number
                    .as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64)
                {
                    Some(RuntimeValue::Nu(Value::int(float as i64, span)))
                } else {
                    wrong(issues)
                }
            }
            _ => wrong(issues),
        },
        TypeSpec::Float => match value.as_f64() {
            Some(float) if value.is_number() => Some(RuntimeValue::Nu(Value::float(float, span))),
            _ => wrong(issues),
        },
        TypeSpec::Bool => match value {
            JsonValue::Bool(flag) => Some(RuntimeValue::Nu(Value::bool(*flag, span))),
            _ => wrong(issues),
        },
        TypeSpec::None => match value {
            JsonValue::Null => Some(RuntimeValue::Nu(Value::nothing(span))),
            _ => wrong(issues),
        },
        TypeSpec::Literal(literals) => {
            let plain = json_to_nu_value(value.clone(), span);
            if literals
                .iter()
                .any(|literal| literal_matches(literal, &plain))
            {
                Some(RuntimeValue::Nu(plain))
            } else {
                issues.push(TypeIssue::new(format!(
                    "expected one of {}, got {}",
                    literals
                        .iter()
                        .map(|literal| literal_json(literal).to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    json_preview(value)
                )));
                None
            }
        }
        TypeSpec::List(element) => {
            let JsonValue::Array(items) = value else {
                return wrong(issues);
            };
            let mut values = Vec::with_capacity(items.len());
            let mut ok = true;
            for (index, item) in items.iter().enumerate() {
                let mut item_issues = Vec::new();
                match convert(item, element, table, &mut item_issues) {
                    Some(value) if item_issues.is_empty() => values.push(value),
                    _ => ok = false,
                }
                issues.extend(item_issues.into_iter().map(|issue| issue.at_index(index)));
                if issues.len() >= MAX_ISSUES {
                    return None;
                }
            }
            ok.then(|| list_from_values(values))
        }
        TypeSpec::Dict(element) => {
            let JsonValue::Object(entries) = value else {
                return wrong(issues);
            };
            let mut values = Vec::with_capacity(entries.len());
            let mut ok = true;
            for (key, item) in entries {
                let mut item_issues = Vec::new();
                match convert(item, element, table, &mut item_issues) {
                    Some(value) if item_issues.is_empty() => values.push((key.clone(), value)),
                    _ => ok = false,
                }
                issues.extend(item_issues.into_iter().map(|issue| issue.at_key(key)));
                if issues.len() >= MAX_ISSUES {
                    return None;
                }
            }
            ok.then(|| dict_from_entries(values))
        }
        TypeSpec::Record(record) => convert_record(value, record, table, issues),
        TypeSpec::Union(members) => convert_union(value, ty, members, table, issues),
        TypeSpec::Model => wrong(issues),
    }
}

fn convert_record(
    value: &JsonValue,
    record: &Rc<RecordType>,
    table: &TypeTable,
    issues: &mut Vec<TypeIssue>,
) -> Option<RuntimeValue> {
    let JsonValue::Object(entries) = value else {
        issues.push(TypeIssue::new(format!(
            "expected a {} object, got {}",
            record.name,
            json_preview(value)
        )));
        return None;
    };
    let types = match record.field_types(table) {
        Ok(types) => types,
        Err(error) => {
            issues.push(TypeIssue::new(error.to_string()));
            return None;
        }
    };
    let start = issues.len();
    let mut values = Vec::with_capacity(record.fields.len());
    for (field, ty) in record.fields.iter().zip(types) {
        match entries.get(&field.name) {
            Some(item) => {
                let mut item_issues = Vec::new();
                if let Some(value) = convert(item, ty, table, &mut item_issues) {
                    match check_limits(&value, &field.limits) {
                        Ok(()) => values.push(value),
                        Err(message) => item_issues.push(TypeIssue::new(message)),
                    }
                }
                issues.extend(
                    item_issues
                        .into_iter()
                        .map(|issue| issue.at_key(&field.name)),
                );
            }
            None => match &field.default {
                Some(default) => match literal_default(default) {
                    Ok(value) => values.push(value),
                    Err(error) => issues.push(TypeIssue::new(error.to_string())),
                },
                None => issues.push(TypeIssue::new(format!(
                    "missing required field `{}` ({})",
                    field.name,
                    schema_word(ty)
                ))),
            },
        }
    }
    for key in entries.keys() {
        if record.field_index(key).is_none() {
            issues.push(TypeIssue::new(format!(
                "unexpected field `{key}`; {} has only: {}",
                record.name,
                record.field_names()
            )));
        }
    }
    if issues.len() > start || values.len() != record.fields.len() {
        return None;
    }
    Some(record_obj(record.clone(), values))
}

/// A union: a member that the value matches exactly wins; for an object,
/// a record member whose `Literal` field matches the object's value (a
/// discriminator such as `kind: Literal["read"]`) is the only one tried.
fn convert_union(
    value: &JsonValue,
    ty: &TypeSpec,
    members: &[TypeSpec],
    table: &TypeTable,
    issues: &mut Vec<TypeIssue>,
) -> Option<RuntimeValue> {
    if let JsonValue::Object(entries) = value {
        if let Some(record) = discriminated_member(entries, members) {
            return convert_record(value, &record, table, issues);
        }
    }
    let mut best: Option<Vec<TypeIssue>> = None;
    for member in members {
        let mut attempt = Vec::new();
        if let Some(converted) = convert(value, member, table, &mut attempt) {
            if attempt.is_empty() {
                return Some(converted);
            }
        }
        // Report the member that came closest, judged by fewest problems
        // past the top level.
        let relevant = !attempt
            .iter()
            .all(|issue| issue.path.is_empty() && issue.message.starts_with("expected"));
        if relevant && best.as_ref().is_none_or(|best| attempt.len() < best.len()) {
            best = Some(attempt);
        }
    }
    match best {
        Some(best) => issues.extend(best),
        None => issues.push(TypeIssue::new(format!(
            "expected {}, got {}",
            schema_word(ty),
            json_preview(value)
        ))),
    }
    None
}

fn discriminated_member(
    entries: &JsonMap<String, JsonValue>,
    members: &[TypeSpec],
) -> Option<Rc<RecordType>> {
    let span = Span::unknown();
    let mut found = None;
    for member in members {
        let TypeSpec::Record(record) = member else {
            continue;
        };
        let matches = record.fields.iter().any(|field| {
            let TypeExpr::Literal(literals) = &field.ty else {
                return false;
            };
            entries.get(&field.name).is_some_and(|value| {
                let plain = json_to_nu_value(value.clone(), span);
                literals
                    .iter()
                    .any(|literal| literal_matches(literal, &plain))
            })
        });
        if matches {
            if found.is_some() {
                return None;
            }
            found = Some(record.clone());
        }
    }
    found
}

/// The type in the words of the JSON Schema shown to the model.
fn schema_word(ty: &TypeSpec) -> String {
    match ty {
        TypeSpec::Any => "any value".to_string(),
        TypeSpec::Str => "string".to_string(),
        TypeSpec::Int => "integer".to_string(),
        TypeSpec::Float => "number".to_string(),
        TypeSpec::Bool => "boolean".to_string(),
        TypeSpec::None => "null".to_string(),
        TypeSpec::List(_) => "array".to_string(),
        TypeSpec::Dict(_) => "object".to_string(),
        TypeSpec::Literal(values) => format!(
            "one of {}",
            values
                .iter()
                .map(|literal| literal_json(literal).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeSpec::Union(members) => members
            .iter()
            .map(schema_word)
            .collect::<Vec<_>>()
            .join(" or "),
        TypeSpec::Record(record) => format!("a {} object", record.name),
        TypeSpec::Model => "a model".to_string(),
    }
}

fn literal_json(literal: &LiteralValue) -> JsonValue {
    match literal {
        LiteralValue::Str(text) => JsonValue::String(text.clone()),
        LiteralValue::Int(value) => JsonValue::from(*value),
        LiteralValue::Bool(value) => JsonValue::Bool(*value),
    }
}

// ---------------------------------------------------------------------------
// JSON Schema for `output=`.

/// The JSON Schema for a model answer of type `ty`. The answer is always a
/// JSON object: a type whose values are not all objects is wrapped as
/// `{"value": ...}`, and `wrapped` says so.
pub(super) struct OutputSchema {
    pub(super) schema: JsonValue,
}

/// Whether an answer of this type is wrapped as `{"value": ...}` (any
/// type that is not a record or a union of records).
pub(super) fn is_wrapped_output(ty: &TypeSpec) -> bool {
    !is_object_type(ty)
}

pub(super) fn output_schema(ty: &TypeSpec, table: &TypeTable) -> Result<OutputSchema, ShellError> {
    let mut defs = JsonMap::new();
    let wrapped = !is_object_type(ty);
    let mut schema = if wrapped {
        json!({
            "type": "object",
            "properties": {"value": schema_for(ty, table, &mut defs)?},
            "required": ["value"],
            "additionalProperties": false,
        })
    } else if let TypeSpec::Record(record) = ty {
        record_schema(record, table, &mut defs)?
    } else {
        schema_for(ty, table, &mut defs)?
    };
    // The top-level record is written inline; drop its own definition
    // unless something refers back to it.
    if let TypeSpec::Record(record) = ty {
        let reference = format!("\"#/$defs/{}\"", record.name);
        let referenced = schema.to_string().contains(&reference)
            || defs
                .values()
                .any(|def| def.to_string().contains(&reference));
        if !referenced {
            defs.remove(&record.name);
        }
    }
    if !defs.is_empty() {
        schema
            .as_object_mut()
            .expect("output schemas are objects")
            .insert("$defs".to_string(), JsonValue::Object(defs));
    }
    Ok(OutputSchema { schema })
}

/// The answer's types written as Python (pydantic) class definitions, for
/// `model.ask(..., type_format="python")`: every record the type reaches,
/// each after the records it uses, with docstrings, defaults, and
/// `Field(...)` limits as the author wrote them.
pub(super) fn python_types(ty: &TypeSpec, table: &TypeTable) -> Result<String, ShellError> {
    fn collect(
        ty: &TypeSpec,
        table: &TypeTable,
        seen: &mut Vec<String>,
        out: &mut Vec<Rc<RecordType>>,
    ) -> Result<(), ShellError> {
        match ty {
            TypeSpec::List(inner) | TypeSpec::Dict(inner) => collect(inner, table, seen, out),
            TypeSpec::Union(members) => {
                for member in members.iter() {
                    collect(member, table, seen, out)?;
                }
                Ok(())
            }
            TypeSpec::Record(record) => {
                if seen.contains(&record.name) {
                    return Ok(());
                }
                seen.push(record.name.clone());
                for field_ty in record.field_types(table)? {
                    collect(field_ty, table, seen, out)?;
                }
                out.push(record.clone());
                Ok(())
            }
            TypeSpec::Model => Err(record_error(
                "a model answer cannot contain a Model; use a record of plain fields",
            )),
            _ => Ok(()),
        }
    }
    let mut records = Vec::new();
    collect(ty, table, &mut Vec::new(), &mut records)?;
    let mut text = String::new();
    for record in records {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("class {}(BaseModel):\n", record.name));
        if let Some(doc) = &record.doc {
            text.push_str(&format!("    \"\"\"{}\"\"\"\n", doc.trim()));
        }
        if record.fields.is_empty() {
            text.push_str("    pass\n");
        }
        for (field, field_ty) in record.fields.iter().zip(record.field_types(table)?) {
            let default = field
                .default
                .as_ref()
                .map(|default| literal_default(default).and_then(|value| python_repr(&value)))
                .transpose()?;
            let limits = &field.limits;
            let mut options = Vec::new();
            for (name, bound) in [
                ("ge", limits.ge),
                ("gt", limits.gt),
                ("le", limits.le),
                ("lt", limits.lt),
            ] {
                if let Some(bound) = bound {
                    options.push(format!("{name}={}", format_number_bound(bound)));
                }
            }
            if let Some(min) = limits.min_length {
                options.push(format!("min_length={min}"));
            }
            if let Some(max) = limits.max_length {
                options.push(format!("max_length={max}"));
            }
            if let Some(description) = &limits.description {
                options.push(format!(
                    "description={}",
                    serde_json::to_string(description).unwrap_or_default()
                ));
            }
            let value = match (default, options.is_empty()) {
                (None, true) => String::new(),
                (Some(default), true) => format!(" = {default}"),
                (None, false) => format!(" = Field({})", options.join(", ")),
                (Some(default), false) => format!(" = Field({default}, {})", options.join(", ")),
            };
            text.push_str(&format!(
                "    {}: {}{value}\n",
                field.name,
                field_ty.display()
            ));
        }
    }
    Ok(text)
}

fn is_object_type(ty: &TypeSpec) -> bool {
    match ty {
        TypeSpec::Record(_) => true,
        TypeSpec::Union(members) => members.iter().all(is_object_type),
        _ => false,
    }
}

fn schema_for(
    ty: &TypeSpec,
    table: &TypeTable,
    defs: &mut JsonMap<String, JsonValue>,
) -> Result<JsonValue, ShellError> {
    Ok(match ty {
        TypeSpec::Any => json!({}),
        TypeSpec::Str => json!({"type": "string"}),
        TypeSpec::Int => json!({"type": "integer"}),
        TypeSpec::Float => json!({"type": "number"}),
        TypeSpec::Bool => json!({"type": "boolean"}),
        TypeSpec::None => json!({"type": "null"}),
        TypeSpec::List(element) => {
            json!({"type": "array", "items": schema_for(element, table, defs)?})
        }
        TypeSpec::Dict(element) => match **element {
            TypeSpec::Any => json!({"type": "object"}),
            ref element => json!({
                "type": "object",
                "additionalProperties": schema_for(element, table, defs)?,
            }),
        },
        TypeSpec::Literal(values) => {
            let values = values.iter().map(literal_json).collect::<Vec<_>>();
            if values.len() == 1 {
                json!({"const": values[0]})
            } else {
                json!({"enum": values})
            }
        }
        TypeSpec::Union(members) => json!({
            "anyOf": members
                .iter()
                .map(|member| schema_for(member, table, defs))
                .collect::<Result<Vec<_>, _>>()?,
        }),
        TypeSpec::Record(record) => {
            if !defs.contains_key(&record.name) {
                // Reserve the name first so a record that refers to itself
                // ends the recursion.
                defs.insert(record.name.clone(), JsonValue::Null);
                let schema = record_schema(record, table, defs)?;
                defs.insert(record.name.clone(), schema);
            }
            json!({"$ref": format!("#/$defs/{}", record.name)})
        }
        TypeSpec::Model => {
            return Err(record_error(
                "a model answer cannot contain a Model; use a record of plain fields",
            ))
        }
    })
}

fn record_schema(
    record: &Rc<RecordType>,
    table: &TypeTable,
    defs: &mut JsonMap<String, JsonValue>,
) -> Result<JsonValue, ShellError> {
    let types = record.field_types(table)?;
    let mut properties = JsonMap::new();
    let mut required = Vec::new();
    for (field, ty) in record.fields.iter().zip(types) {
        let mut schema = schema_for(ty, table, defs)?;
        add_limit_keywords(&mut schema, &field.limits);
        match &field.default {
            Some(default) => {
                let default = literal_default(default)?.into_nu_value("schema")?;
                if let Some(object) = schema.as_object_mut() {
                    object.insert(
                        "default".to_string(),
                        crate::json::nu_to_json_value(&default),
                    );
                }
            }
            None => required.push(JsonValue::String(field.name.clone())),
        }
        properties.insert(field.name.clone(), schema);
    }
    let mut schema = JsonMap::new();
    schema.insert("title".to_string(), JsonValue::String(record.name.clone()));
    if let Some(doc) = &record.doc {
        schema.insert(
            "description".to_string(),
            JsonValue::String(doc.trim().to_string()),
        );
    }
    schema.insert("type".to_string(), json!("object"));
    schema.insert("properties".to_string(), JsonValue::Object(properties));
    schema.insert("required".to_string(), JsonValue::Array(required));
    schema.insert("additionalProperties".to_string(), JsonValue::Bool(false));
    Ok(JsonValue::Object(schema))
}

// ---------------------------------------------------------------------------
// Python text for str(), repr(), and f-strings.

/// `str(value)`: a string is itself; anything else is its repr, with
/// records written as Python writes dataclasses: `Plan(steps=[], risky=False)`.
pub(super) fn python_str(value: &RuntimeValue) -> Result<String, ShellError> {
    match value {
        RuntimeValue::Nu(Value::String { val, .. } | Value::Glob { val, .. }) => Ok(val.clone()),
        other => python_repr(other),
    }
}

pub(super) fn python_repr(value: &RuntimeValue) -> Result<String, ShellError> {
    let mut out = String::new();
    write_repr(value, &mut out, 0)?;
    Ok(out)
}

fn write_repr(value: &RuntimeValue, out: &mut String, depth: usize) -> Result<(), ShellError> {
    if depth > 512 {
        return Err(super::stone_error(
            "str",
            "a value contains itself (or nests more than 512 levels deep), so it cannot be printed",
        ));
    }
    match value {
        RuntimeValue::Record(record) => {
            out.push_str(&record.ty.name);
            out.push('(');
            for (index, (field, item)) in record
                .ty
                .fields
                .iter()
                .zip(record.shared_fields())
                .enumerate()
            {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&field.name);
                out.push('=');
                write_repr(&item, out, depth + 1)?;
            }
            out.push(')');
        }
        RuntimeValue::List(list) => {
            out.push('[');
            list.try_for_each(|index, item| {
                if index > 0 {
                    out.push_str(", ");
                }
                write_item_repr(item, out, depth)
            })?;
            out.push(']');
        }
        RuntimeValue::Dict(dict) => {
            out.push('{');
            let mut first = true;
            dict.try_for_each(|key, item| {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                out.push_str(&crate::stone_builtins::python_repr(&Value::string(
                    key,
                    Span::unknown(),
                ))?);
                out.push_str(": ");
                write_item_repr(item, out, depth)
            })?;
            out.push('}');
        }
        RuntimeValue::Type(spec) => match &**spec {
            TypeSpec::Record(record) => out.push_str(&format!("<class '{}'>", record.name)),
            other => out.push_str(&other.display()),
        },
        RuntimeValue::Model(_) => out.push_str("<model>"),
        other => out.push_str(&crate::stone_builtins::python_repr(
            &super::stone_containers::materialize(other, "str")?,
        )?),
    }
    Ok(())
}

fn write_item_repr(item: ItemRef<'_>, out: &mut String, depth: usize) -> Result<(), ShellError> {
    match item {
        ItemRef::Plain(value) => out.push_str(&crate::stone_builtins::python_repr(value)?),
        ItemRef::Shared(value) => write_repr(&value, out, depth + 1)?,
    }
    Ok(())
}
