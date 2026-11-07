// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared lists and dicts (design L2): Python reference semantics.
//!
//! A list or dict bound to a variable is a shared object. `b = a`, passing it
//! to a function, putting it in another container, or reading it out of one
//! all share the same object, so a mutation through any of them is visible
//! through all of them, as in Python.
//!
//! Representation:
//!
//! - `ListObj`/`DictObj` behind an `Rc`, with the elements in a `RefCell`.
//!   Dicts are insertion-ordered hash maps (O(1) lookup).
//! - Elements are compact `Item`s (64 bytes): a plain Nu value inline, a
//!   pointer to a shared list or dict, or a boxed runtime value (a function,
//!   a file, an attempt handle). So containers can hold functions and other
//!   runtime values (R9) without every element paying for the largest one.
//! - Containers produced by builtins stay plain Nu values until they are bound
//!   to a variable (`share`). Nested plain containers become shared objects
//!   the first time they are read out of their parent (`Item::share`), so a
//!   large parsed document is not converted up front.
//! - Builtins that take plain values get a deep copy (`materialize`). This is
//!   what every builtin received before shared containers existed.
//! - A frozen object is read-only, deeply (host-reported errors, module
//!   values).
//! - Each object carries an information-flow label slot. It is not
//!   interpreted yet; it reserves the place where provenance labels will live
//!   without another change to the representation.
//!
//! Reference cycles (`xs.append(xs)`) are not collected; such a list lives
//! until the evaluation ends. Copying or printing one reports an error.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use indexmap::IndexMap;
use nu_protocol::{Record, ShellError, Span, Value};

use super::stone_error;
use super::stone_runtime_value::RuntimeValue;
use crate::stone_builtins::normalize_index;

/// Nesting depth at which materializing a container gives up. A container
/// that contains itself would otherwise recurse forever.
const MAX_MATERIALIZE_DEPTH: usize = 512;

/// Information-flow label slot. `Label(0)` is "no label". Reserved for
/// provenance tracking; nothing reads it yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Label(#[allow(dead_code)] pub(super) u32);

/// One element of a shared list or dict.
#[derive(Clone)]
enum Item {
    Plain(Value),
    List(ListRef),
    Dict(DictRef),
    Runtime(Box<RuntimeValue>),
}

impl Item {
    fn from_runtime(value: RuntimeValue) -> Self {
        match value {
            RuntimeValue::Nu(value) => Item::Plain(value),
            RuntimeValue::List(list) => Item::List(list),
            RuntimeValue::Dict(dict) => Item::Dict(dict),
            other => Item::Runtime(Box::new(other)),
        }
    }

    fn to_runtime(&self) -> RuntimeValue {
        match self {
            Item::Plain(value) => RuntimeValue::Nu(value.clone()),
            Item::List(list) => RuntimeValue::List(list.clone()),
            Item::Dict(dict) => RuntimeValue::Dict(dict.clone()),
            Item::Runtime(value) => (**value).clone(),
        }
    }

    /// The element as a value to store elsewhere: a plain nested container
    /// becomes a shared object in place first, so both places share it.
    fn share(&mut self) -> RuntimeValue {
        if let Item::Plain(Value::List { .. } | Value::Record { .. }) = self {
            let Item::Plain(value) =
                std::mem::replace(self, Item::Plain(Value::nothing(Span::unknown())))
            else {
                unreachable!("matched above");
            };
            *self = Item::from_runtime(share_nu(value));
        }
        self.to_runtime()
    }

    fn materialize(&self, context: &str, depth: usize) -> Result<Value, ShellError> {
        match self {
            Item::Plain(value) => Ok(value.clone()),
            Item::List(list) => list.materialize(context, depth + 1),
            Item::Dict(dict) => dict.materialize(context, depth + 1),
            Item::Runtime(value) => materialize_at(value, context, depth + 1),
        }
    }

    fn is_persistable(&self) -> bool {
        match self {
            Item::Plain(_) => true,
            Item::List(list) => containers_are_persistable(&RuntimeValue::List(list.clone())),
            Item::Dict(dict) => containers_are_persistable(&RuntimeValue::Dict(dict.clone())),
            Item::Runtime(value) => value.is_session_persistable(),
        }
    }
}

pub(super) struct ListObj {
    items: RefCell<Vec<Item>>,
    #[allow(dead_code)]
    label: Cell<Label>,
    frozen: Cell<bool>,
}

pub(super) struct DictObj {
    entries: RefCell<IndexMap<String, Item>>,
    #[allow(dead_code)]
    label: Cell<Label>,
    frozen: Cell<bool>,
}

pub(super) type ListRef = Rc<ListObj>;
pub(super) type DictRef = Rc<DictObj>;

fn list_obj(items: Vec<Item>) -> RuntimeValue {
    RuntimeValue::List(Rc::new(ListObj {
        items: RefCell::new(items),
        label: Cell::new(Label::default()),
        frozen: Cell::new(false),
    }))
}

fn dict_obj(entries: IndexMap<String, Item>) -> RuntimeValue {
    RuntimeValue::Dict(Rc::new(DictObj {
        entries: RefCell::new(entries),
        label: Cell::new(Label::default()),
        frozen: Cell::new(false),
    }))
}

/// A list built from evaluated elements (a literal or a comprehension). It
/// stays a plain value when every element is plain; an element that is a
/// shared container, a function, or another runtime value makes it a shared
/// list holding that element itself.
pub(super) fn list_from_values(values: Vec<RuntimeValue>) -> RuntimeValue {
    if values
        .iter()
        .all(|value| matches!(value, RuntimeValue::Nu(_)))
    {
        let vals = values
            .into_iter()
            .map(|value| match value {
                RuntimeValue::Nu(value) => value,
                _ => unreachable!("checked above"),
            })
            .collect();
        return RuntimeValue::Nu(Value::list(vals, Span::unknown()));
    }
    list_obj(values.into_iter().map(Item::from_runtime).collect())
}

/// The dict counterpart of `list_from_values`. Later duplicate keys win, as
/// in Python.
pub(super) fn dict_from_entries(entries: Vec<(String, RuntimeValue)>) -> RuntimeValue {
    if entries
        .iter()
        .all(|(_, value)| matches!(value, RuntimeValue::Nu(_)))
    {
        let mut record = Record::with_capacity(entries.len());
        for (key, value) in entries {
            let RuntimeValue::Nu(value) = value else {
                unreachable!("checked above");
            };
            record.insert(key, value);
        }
        return RuntimeValue::Nu(Value::record(record, Span::unknown()));
    }
    dict_obj(
        entries
            .into_iter()
            .map(|(key, value)| (key, Item::from_runtime(value)))
            .collect(),
    )
}

/// Turn a plain Nu list or record into a shared object (one level deep);
/// every other value is returned unchanged.
pub(super) fn share(value: RuntimeValue) -> RuntimeValue {
    match value {
        RuntimeValue::Nu(value) => share_nu(value),
        other => other,
    }
}

fn share_nu(value: Value) -> RuntimeValue {
    match value {
        Value::List { vals, .. } => list_obj(vals.into_iter().map(Item::Plain).collect()),
        Value::Record { val, .. } => dict_obj(
            val.into_owned()
                .into_iter()
                .map(|(key, value)| (key, Item::Plain(value)))
                .collect(),
        ),
        other => RuntimeValue::Nu(other),
    }
}

pub(super) fn is_plain_container(value: &RuntimeValue) -> bool {
    matches!(
        value,
        RuntimeValue::Nu(Value::List { .. } | Value::Record { .. })
    )
}

/// Make a value deeply read-only: it and every list or dict inside it.
/// Freezing is permanent; a changed copy is the way to edit.
pub(super) fn freeze(value: RuntimeValue) -> RuntimeValue {
    let value = share(value);
    freeze_in_place(&value, 0);
    value
}

fn freeze_in_place(value: &RuntimeValue, depth: usize) {
    if depth > MAX_MATERIALIZE_DEPTH {
        return;
    }
    match value {
        RuntimeValue::List(list) if !list.frozen.get() => {
            list.frozen.set(true);
            for item in list.shared_items() {
                freeze_in_place(&item, depth + 1);
            }
        }
        RuntimeValue::Dict(dict) if !dict.frozen.get() => {
            dict.frozen.set(true);
            for (_, item) in dict.shared_entries() {
                freeze_in_place(&item, depth + 1);
            }
        }
        _ => {}
    }
}

pub(super) fn is_frozen(value: &RuntimeValue) -> bool {
    match value {
        RuntimeValue::List(list) => list.frozen.get(),
        RuntimeValue::Dict(dict) => dict.frozen.get(),
        _ => false,
    }
}

/// The error for changing a frozen container, or `Ok` when it may change.
pub(super) fn ensure_mutable(container: &RuntimeValue, context: &str) -> Result<(), ShellError> {
    let (kind, copy) = match container {
        RuntimeValue::List(_) => ("list", "copy = list(items)"),
        RuntimeValue::Dict(_) => ("dict", "copy = {k: v for k, v in record.items()}"),
        _ => return Ok(()),
    };
    if is_frozen(container) {
        return Err(stone_error(
            context,
            format!(
                "this {kind} is read-only: errors the host reports (ExpectationFailed, UndeclaredEffect) and values imported from modules cannot be changed. Change a copy instead: `{copy}`"
            ),
        ));
    }
    Ok(())
}

/// A deep plain copy, for builtins and output.
pub(super) fn materialize(value: &RuntimeValue, context: &str) -> Result<Value, ShellError> {
    materialize_at(value, context, 0)
}

fn materialize_at(value: &RuntimeValue, context: &str, depth: usize) -> Result<Value, ShellError> {
    match value {
        RuntimeValue::List(list) => list.materialize(context, depth),
        RuntimeValue::Dict(dict) => dict.materialize(context, depth),
        other => other.clone().into_nu_value(context),
    }
}

fn depth_error(context: &str) -> ShellError {
    stone_error(
        context,
        "a list or dict contains itself (or nests more than 512 levels deep), so it cannot be copied or printed",
    )
}

/// Like `materialize`, but takes the elements without copying when nothing
/// else shares the object.
pub(super) fn materialize_owned(value: RuntimeValue, context: &str) -> Result<Value, ShellError> {
    match value {
        RuntimeValue::List(list) => match Rc::try_unwrap(list) {
            Ok(list) => {
                let items = list.items.into_inner();
                let mut vals = Vec::with_capacity(items.len());
                for item in items {
                    vals.push(match item {
                        Item::Plain(value) => value,
                        other => other.materialize(context, 0)?,
                    });
                }
                Ok(Value::list(vals, Span::unknown()))
            }
            Err(list) => list.materialize(context, 0),
        },
        RuntimeValue::Dict(dict) => match Rc::try_unwrap(dict) {
            Ok(dict) => {
                let entries = dict.entries.into_inner();
                let mut record = Record::with_capacity(entries.len());
                for (key, item) in entries {
                    let value = match item {
                        Item::Plain(value) => value,
                        other => other.materialize(context, 0)?,
                    };
                    record.push(key, value);
                }
                Ok(Value::record(record, Span::unknown()))
            }
            Err(dict) => dict.materialize(context, 0),
        },
        other => other.into_nu_value(context),
    }
}

/// A shared container as a plain value; other values unchanged. For code
/// that inspects a value's plain shape (a record's fields, a list of ids).
pub(super) fn unshare(value: RuntimeValue, context: &str) -> Result<RuntimeValue, ShellError> {
    match value {
        RuntimeValue::List(_) | RuntimeValue::Dict(_) => {
            materialize_owned(value, context).map(RuntimeValue::Nu)
        }
        other => Ok(other),
    }
}

impl ListObj {
    pub(super) fn len(&self) -> usize {
        self.items.borrow().len()
    }

    /// The element at a Python index (negative counts from the end), shared
    /// with the list.
    pub(super) fn get(&self, index: i64) -> Option<RuntimeValue> {
        let mut items = self.items.borrow_mut();
        let index = python_index(index, items.len())?;
        Some(items[index].share())
    }

    /// Every element, shared with the list (for iteration).
    pub(super) fn shared_items(&self) -> Vec<RuntimeValue> {
        let mut items = self.items.borrow_mut();
        items.iter_mut().map(Item::share).collect()
    }

    pub(super) fn push(&self, value: RuntimeValue) {
        self.items.borrow_mut().push(Item::from_runtime(value));
    }

    pub(super) fn extend(&self, values: Vec<RuntimeValue>) {
        self.items
            .borrow_mut()
            .extend(values.into_iter().map(Item::from_runtime));
    }

    pub(super) fn replace_items(&self, values: Vec<RuntimeValue>) {
        *self.items.borrow_mut() = values.into_iter().map(Item::from_runtime).collect();
    }

    /// Replace the contents with plain values (loop-engine results).
    pub(super) fn replace_plain(&self, values: Vec<Value>) {
        *self.items.borrow_mut() = values.into_iter().map(Item::Plain).collect();
    }

    fn materialize(&self, context: &str, depth: usize) -> Result<Value, ShellError> {
        if depth > MAX_MATERIALIZE_DEPTH {
            return Err(depth_error(context));
        }
        let items = self.items.borrow();
        let mut vals = Vec::with_capacity(items.len());
        for item in items.iter() {
            vals.push(item.materialize(context, depth)?);
        }
        Ok(Value::list(vals, Span::unknown()))
    }
}

impl DictObj {
    pub(super) fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// The value under `key`, shared with the dict.
    pub(super) fn get(&self, key: &str) -> Option<RuntimeValue> {
        let mut entries = self.entries.borrow_mut();
        entries.get_mut(key).map(Item::share)
    }

    pub(super) fn contains_key(&self, key: &str) -> bool {
        self.entries.borrow().contains_key(key)
    }

    pub(super) fn keys(&self) -> Vec<String> {
        self.entries.borrow().keys().cloned().collect()
    }

    /// `(key, value)` pairs, values shared with the dict.
    pub(super) fn shared_entries(&self) -> Vec<(String, RuntimeValue)> {
        let mut entries = self.entries.borrow_mut();
        entries
            .iter_mut()
            .map(|(key, value)| (key.clone(), value.share()))
            .collect()
    }

    pub(super) fn insert(&self, key: String, value: RuntimeValue) {
        self.entries
            .borrow_mut()
            .insert(key, Item::from_runtime(value));
    }

    /// Replace the contents with plain values (loop-engine results).
    pub(super) fn replace_plain(&self, record: Record) {
        *self.entries.borrow_mut() = record
            .into_iter()
            .map(|(key, value)| (key, Item::Plain(value)))
            .collect();
    }

    fn materialize(&self, context: &str, depth: usize) -> Result<Value, ShellError> {
        if depth > MAX_MATERIALIZE_DEPTH {
            return Err(depth_error(context));
        }
        let entries = self.entries.borrow();
        let mut record = Record::with_capacity(entries.len());
        for (key, item) in entries.iter() {
            record.push(key.clone(), item.materialize(context, depth)?);
        }
        Ok(Value::record(record, Span::unknown()))
    }
}

/// Python type name of a value as Stone's errors spell it.
fn kind(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::List(_) => "list".to_string(),
        RuntimeValue::Dict(_) => "record".to_string(),
        RuntimeValue::Nu(value) => value.get_type().to_string(),
        _ => "value".to_string(),
    }
}

/// Step into a container on the way to an item being mutated
/// (`x["a"][0] = v` steps into `x["a"]`). The child is shared with its
/// parent, so the mutation lands in the object every alias sees.
pub(super) fn child_for_mutation(
    parent: &RuntimeValue,
    index: &Value,
    context: &str,
) -> Result<RuntimeValue, ShellError> {
    match (parent, index) {
        (RuntimeValue::List(list), Value::Int { val, .. }) => {
            let mut items = list.items.borrow_mut();
            let position = normalize_index(*val, items.len())?;
            Ok(items[position].share())
        }
        (
            RuntimeValue::Dict(dict),
            Value::String { val: key, .. } | Value::Glob { val: key, .. },
        ) => {
            let mut entries = dict.entries.borrow_mut();
            entries
                .get_mut(key)
                .map(Item::share)
                .ok_or_else(|| stone_error(context, format!("record has no key `{key}`")))
        }
        (parent, index) => Err(stone_error(
            context,
            format!(
                "cannot mutate nested {} item with {} index",
                kind(parent),
                index.get_type()
            ),
        )),
    }
}

/// `container[index] = value`. The value is stored as is, so storing a list
/// or dict shares it.
pub(super) fn assign_item(
    container: &RuntimeValue,
    index: &Value,
    value: RuntimeValue,
) -> Result<(), ShellError> {
    ensure_mutable(container, "assignment")?;
    match (container, index) {
        (
            RuntimeValue::Dict(dict),
            Value::String { val: key, .. } | Value::Glob { val: key, .. },
        ) => {
            dict.insert(key.clone(), value);
            Ok(())
        }
        (RuntimeValue::List(list), Value::Int { val, .. }) => {
            let mut items = list.items.borrow_mut();
            let position = normalize_index(*val, items.len())?;
            items[position] = Item::from_runtime(value);
            Ok(())
        }
        (container, index) => Err(stone_error(
            "assignment",
            format!(
                "cannot assign {} item with {} index",
                kind(container),
                index.get_type()
            ),
        )),
    }
}

pub(super) fn python_index(index: i64, len: usize) -> Option<usize> {
    let len = i64::try_from(len).ok()?;
    let index = if index < 0 { len + index } else { index };
    (0..len).contains(&index).then_some(index as usize)
}

pub(super) fn containers_are_persistable(value: &RuntimeValue) -> bool {
    match value {
        RuntimeValue::List(list) => list
            .items
            .try_borrow()
            .map(|items| items.iter().all(Item::is_persistable))
            .unwrap_or(false),
        RuntimeValue::Dict(dict) => dict
            .entries
            .try_borrow()
            .map(|entries| entries.values().all(Item::is_persistable))
            .unwrap_or(false),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use nu_protocol::{Span, Value};

    use super::{freeze, is_frozen, list_from_values, materialize, share, RuntimeValue};

    fn int(value: i64) -> Value {
        Value::int(value, Span::unknown())
    }

    #[test]
    fn nested_containers_are_shared_once_read() {
        let outer = share(RuntimeValue::Nu(Value::list(
            vec![Value::list(vec![int(1)], Span::unknown())],
            Span::unknown(),
        )));
        let RuntimeValue::List(outer_list) = &outer else {
            panic!("list");
        };
        let Some(RuntimeValue::List(inner)) = outer_list.get(0) else {
            panic!("inner list");
        };
        inner.push(RuntimeValue::Nu(int(2)));
        let copy = materialize(&outer, "test").expect("materialize");
        assert_eq!(
            copy,
            Value::list(
                vec![Value::list(vec![int(1), int(2)], Span::unknown())],
                Span::unknown()
            )
        );
    }

    #[test]
    fn a_list_that_contains_itself_reports_instead_of_recursing() {
        let list = share(RuntimeValue::Nu(Value::list(Vec::new(), Span::unknown())));
        let RuntimeValue::List(inner) = &list else {
            panic!("list");
        };
        inner.push(list.clone());
        let error = materialize(&list, "print").expect_err("cycle");
        assert!(format!("{error:?}").contains("contains itself"));
        // Break the cycle so the test does not leak.
        inner.replace_items(Vec::new());
    }

    #[test]
    fn freezing_is_deep() {
        let value = freeze(RuntimeValue::Nu(Value::list(
            vec![Value::list(vec![int(1)], Span::unknown())],
            Span::unknown(),
        )));
        let RuntimeValue::List(list) = &value else {
            panic!("list");
        };
        assert!(is_frozen(&value));
        assert!(is_frozen(&list.get(0).expect("inner")));
    }

    #[test]
    fn plain_literals_stay_plain() {
        assert!(matches!(
            list_from_values(vec![RuntimeValue::Nu(int(1))]),
            RuntimeValue::Nu(Value::List { .. })
        ));
        assert!(matches!(
            share(RuntimeValue::Nu(int(3))),
            RuntimeValue::Nu(_)
        ));
    }

    #[test]
    fn elements_stay_compact() {
        assert!(std::mem::size_of::<super::Item>() <= 64);
    }
}
