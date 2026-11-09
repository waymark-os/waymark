//! Tuples and sets: Python's immutable sequence and its hash set.
//!
//! A tuple is an immutable sequence of values; it compares element by
//! element with another tuple, never with a list. A set holds hashable
//! values (None, bool, numbers, strings, tuples of those, types) once each,
//! keyed by value, in insertion order. Both become JSON arrays where values
//! leave the program (emit, builtins over plain data), as types become their
//! names.

use std::cell::RefCell;
use std::rc::Rc;

use indexmap::IndexMap;
use nu_protocol::{ShellError, Value};

use super::stone_error;
use super::stone_runtime_value::RuntimeValue;

pub(super) type TupleRef = Rc<Vec<RuntimeValue>>;
pub(super) type SetRef = Rc<SetObj>;

/// A tuple holds its elements as objects, as a list does: a list inside a
/// tuple is the same list wherever the tuple's element is read.
pub(super) fn tuple_value(items: Vec<RuntimeValue>) -> RuntimeValue {
    RuntimeValue::Tuple(Rc::new(
        items.into_iter().map(super::stone_containers::share).collect(),
    ))
}

#[derive(Default)]
pub(super) struct SetObj {
    items: RefCell<IndexMap<String, RuntimeValue>>,
}

/// What a member's key says about it, for the bool/number rule.
#[derive(Clone, Copy, PartialEq)]
enum KeyKind {
    Bool,
    Number,
    Other,
}

/// The hash key for a set member: equal values (1 and 1.0) share a key.
/// Lists, dicts, sets, and records are unhashable, as in Python.
fn hash_key(value: &RuntimeValue) -> Result<(String, KeyKind), ShellError> {
    let key = match value {
        RuntimeValue::Nu(Value::Nothing { .. }) => ("N".to_string(), KeyKind::Other),
        RuntimeValue::Nu(Value::Bool { val, .. }) => {
            (if *val { "B1" } else { "B0" }.to_string(), KeyKind::Bool)
        }
        RuntimeValue::Nu(Value::Int { val, .. }) => (format!("I{val}"), KeyKind::Number),
        RuntimeValue::Nu(Value::Float { val, .. }) => {
            let key = if val.fract() == 0.0 && val.abs() < 9.0e15 {
                format!("I{}", *val as i64)
            } else {
                format!("F{}", val.to_bits())
            };
            (key, KeyKind::Number)
        }
        RuntimeValue::Nu(Value::String { val, .. } | Value::Glob { val, .. }) => {
            (format!("S{val}"), KeyKind::Other)
        }
        RuntimeValue::Tuple(items) => {
            let mut key = String::from("T");
            for item in items.iter() {
                let (item_key, _) = hash_key(item)?;
                key.push_str(&format!("{}:{item_key}", item_key.len()));
            }
            (key, KeyKind::Other)
        }
        RuntimeValue::Type(spec) => (format!("Y{}", spec.display()), KeyKind::Other),
        other => {
            return Err(stone_error(
                "set",
                format!(
                    "unhashable type: '{}' cannot be a set member, as in Python; use a tuple (`tuple(xs)`) or a string",
                    kind_name(other)
                ),
            ))
        }
    };
    Ok(key)
}

fn kind_name(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::List(_) | RuntimeValue::Nu(Value::List { .. }) => "list",
        RuntimeValue::Dict(_) | RuntimeValue::Nu(Value::Record { .. }) => "dict",
        RuntimeValue::Set(_) => "set",
        RuntimeValue::Record(_) => "record",
        _ => "value",
    }
}

fn bool_number_member_error() -> ShellError {
    stone_error(
        "set",
        "this set would mix bools and numbers: Python treats True as 1, but Stone keeps bool and int apart. Keep one kind per set, or convert first with int(flag) or bool(count)",
    )
}

impl SetObj {
    pub(super) fn from_values(values: Vec<RuntimeValue>) -> Result<SetRef, ShellError> {
        let set = SetObj::default();
        for value in values {
            set.add(value)?;
        }
        Ok(Rc::new(set))
    }

    pub(super) fn len(&self) -> usize {
        self.items.borrow().len()
    }

    pub(super) fn values(&self) -> Vec<RuntimeValue> {
        self.items.borrow().values().cloned().collect()
    }

    fn check_kind(&self, kind: KeyKind) -> Result<(), ShellError> {
        let clash = match kind {
            KeyKind::Bool => KeyKind::Number,
            KeyKind::Number => KeyKind::Bool,
            KeyKind::Other => return Ok(()),
        };
        let clashes = self.items.borrow().keys().any(|key| {
            let other = match key.as_bytes().first() {
                Some(b'B') => KeyKind::Bool,
                Some(b'I' | b'F') => KeyKind::Number,
                _ => KeyKind::Other,
            };
            other == clash
        });
        if clashes {
            return Err(bool_number_member_error());
        }
        Ok(())
    }

    pub(super) fn add(&self, value: RuntimeValue) -> Result<(), ShellError> {
        let (key, kind) = hash_key(&value)?;
        self.check_kind(kind)?;
        self.items.borrow_mut().entry(key).or_insert(value);
        Ok(())
    }

    pub(super) fn contains(&self, value: &RuntimeValue) -> Result<bool, ShellError> {
        let (key, kind) = hash_key(value)?;
        self.check_kind(kind)?;
        Ok(self.items.borrow().contains_key(&key))
    }

    /// Remove `value`; whether it was there.
    pub(super) fn discard(&self, value: &RuntimeValue) -> Result<bool, ShellError> {
        let (key, _) = hash_key(value)?;
        Ok(self.items.borrow_mut().shift_remove(&key).is_some())
    }

    pub(super) fn clear(&self) {
        self.items.borrow_mut().clear();
    }

    pub(super) fn pop(&self) -> Option<RuntimeValue> {
        self.items.borrow_mut().shift_remove_index(0).map(|(_, value)| value)
    }

    fn keys(&self) -> Vec<String> {
        self.items.borrow().keys().cloned().collect()
    }

    fn has_key(&self, key: &str) -> bool {
        self.items.borrow().contains_key(key)
    }

    pub(super) fn equals(&self, other: &SetObj) -> bool {
        self.len() == other.len() && self.keys().iter().all(|key| other.has_key(key))
    }

    pub(super) fn is_subset(&self, other: &SetObj) -> bool {
        self.keys().iter().all(|key| other.has_key(key))
    }

    pub(super) fn union(&self, other: &SetObj) -> Result<SetRef, ShellError> {
        let mut values = self.values();
        values.extend(other.values());
        SetObj::from_values(values)
    }

    pub(super) fn intersection(&self, other: &SetObj) -> SetRef {
        self.filtered(|key| other.has_key(key))
    }

    pub(super) fn difference(&self, other: &SetObj) -> SetRef {
        self.filtered(|key| !other.has_key(key))
    }

    pub(super) fn symmetric_difference(&self, other: &SetObj) -> Result<SetRef, ShellError> {
        let mut values = self.difference(other).values();
        values.extend(other.difference(self).values());
        SetObj::from_values(values)
    }

    fn filtered(&self, keep: impl Fn(&str) -> bool) -> SetRef {
        let items = self
            .items
            .borrow()
            .iter()
            .filter(|(key, _)| keep(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Rc::new(SetObj {
            items: RefCell::new(items),
        })
    }
}

/// Tuples are equal when they have the same length and equal elements.
pub(super) fn tuples_equal(
    left: &[RuntimeValue],
    right: &[RuntimeValue],
    equal: &mut dyn FnMut(&RuntimeValue, &RuntimeValue) -> Result<bool, ShellError>,
) -> Result<bool, ShellError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.iter().zip(right.iter()) {
        if !equal(left, right)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Comparing a tuple or set with a list (or a set with a tuple) is a
/// `False` Python gives silently; Stone says so.
pub(super) fn sequence_kind_mismatch(left: &RuntimeValue, right: &RuntimeValue) -> Option<ShellError> {
    let kind = |value: &RuntimeValue| match value {
        RuntimeValue::Tuple(_) => Some("tuple"),
        RuntimeValue::Set(_) => Some("set"),
        RuntimeValue::List(_) | RuntimeValue::Nu(Value::List { .. }) => Some("list"),
        RuntimeValue::TextLines(_) | RuntimeValue::JsonArrayView(_) => Some("list"),
        _ => None,
    };
    let (Some(left_kind), Some(right_kind)) = (kind(left), kind(right)) else {
        return None;
    };
    if left_kind == right_kind {
        return None;
    }
    let convert = if left_kind == "list" || right_kind == "list" {
        "list(x)"
    } else {
        "set(x)"
    };
    Some(stone_error(
        "comparison",
        format!(
            "comparing a {left_kind} with a {right_kind}: they are never equal (in Python either). Convert one side first, as in `{convert} == y`"
        ),
    ))
}
