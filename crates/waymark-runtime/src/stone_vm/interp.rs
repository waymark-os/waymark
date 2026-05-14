// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{ShellError, Span, Value};

use super::stone_runtime_value::{RuntimeValue, TextLines};
use super::{json_object_view_get, stone_error};

#[derive(Clone, Copy)]
pub(super) enum GenericVmNumber {
    I64(i64),
    F64(f64),
}

pub(super) enum GenericVmLoopResult {
    Executed { last_value: Option<RuntimeValue> },
    Unsupported,
}

pub(super) enum GenericVmInput<'a> {
    Values(&'a [RuntimeValue]),
    TextLines(&'a TextLines),
}

pub(super) fn generic_vm_number_from_runtime(value: &RuntimeValue) -> Option<GenericVmNumber> {
    match value {
        RuntimeValue::Nu(Value::Int { val, .. }) => Some(GenericVmNumber::I64(*val)),
        RuntimeValue::Nu(Value::Float { val, .. }) => Some(GenericVmNumber::F64(*val)),
        _ => None,
    }
}

pub(super) fn generic_vm_record_field_value(
    value: &RuntimeValue,
    field: &str,
) -> Result<Option<RuntimeValue>, ShellError> {
    match value {
        RuntimeValue::Nu(Value::Record { val, .. }) => {
            Ok(val.get(field).cloned().map(RuntimeValue::Nu))
        }
        RuntimeValue::JsonObjectView(view) => json_object_view_get(view, field),
        _ => Ok(None),
    }
}

pub(super) fn generic_vm_add_number(
    left: GenericVmNumber,
    right: GenericVmNumber,
) -> Result<GenericVmNumber, ShellError> {
    match (left, right) {
        (GenericVmNumber::I64(left), GenericVmNumber::I64(right)) => left
            .checked_add(right)
            .map(GenericVmNumber::I64)
            .ok_or_else(|| stone_error("hot loop", "integer addition overflow")),
        (GenericVmNumber::F64(left), GenericVmNumber::F64(right)) => {
            Ok(GenericVmNumber::F64(left + right))
        }
        (GenericVmNumber::I64(left), GenericVmNumber::F64(right)) => {
            Ok(GenericVmNumber::F64(left as f64 + right))
        }
        (GenericVmNumber::F64(left), GenericVmNumber::I64(right)) => {
            Ok(GenericVmNumber::F64(left + right as f64))
        }
    }
}

pub(super) fn generic_vm_number_to_value(value: GenericVmNumber) -> Value {
    match value {
        GenericVmNumber::I64(value) => Value::int(value, Span::unknown()),
        GenericVmNumber::F64(value) => Value::float(value, Span::unknown()),
    }
}
