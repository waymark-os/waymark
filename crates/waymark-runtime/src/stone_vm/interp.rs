// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use nu_protocol::{ShellError, Span, Value};

use super::stone_runtime_value::{RuntimeValue, TextLines};
use super::{json_object_view_get, stone_error};
use crate::stone_vm::{GenericParseNumber, GenericVmConst, GenericVmExprBody, GenericVmExprOp};

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

pub(super) struct GenericVmExprBodyResult {
    pub(super) locals: Vec<Option<i64>>,
    pub(super) last_value: Option<RuntimeValue>,
}

pub(super) struct GenericVmAssignResult {
    pub(super) value: RuntimeValue,
    pub(super) last_value: Option<RuntimeValue>,
}

pub(super) struct GenericVmMapResult {
    pub(super) counts: HashMap<String, i64>,
    pub(super) last_value: Option<RuntimeValue>,
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

pub(super) fn execute_generic_vm_add_assign(
    local_value: RuntimeValue,
    values: &[RuntimeValue],
    mut lowering_miss: impl FnMut(&'static str),
) -> Result<Option<GenericVmAssignResult>, ShellError> {
    let mut accumulator = match generic_vm_number_from_runtime(&local_value) {
        Some(value) => value,
        None => {
            lowering_miss("unsupported_expr");
            return Ok(None);
        }
    };
    let mut last_value = None;
    for value in values {
        let Some(number) = generic_vm_number_from_runtime(value) else {
            lowering_miss("unsupported_expr");
            return Ok(None);
        };
        accumulator = generic_vm_add_number(accumulator, number)?;
        last_value = Some(value.clone());
    }

    Ok(Some(GenericVmAssignResult {
        value: RuntimeValue::Nu(generic_vm_number_to_value(accumulator)),
        last_value,
    }))
}

pub(super) fn execute_generic_vm_text_parse_add_assign(
    local_value: RuntimeValue,
    lines: &TextLines,
    parse: GenericParseNumber,
    mut lowering_miss: impl FnMut(&'static str),
) -> Result<Option<GenericVmAssignResult>, ShellError> {
    let mut accumulator = match generic_vm_number_from_runtime(&local_value) {
        Some(value) => value,
        None => {
            lowering_miss("unsupported_expr");
            return Ok(None);
        }
    };
    let mut last_value = None;
    for line in &lines.lines {
        let parsed = match parse {
            GenericParseNumber::Int => match line.trim().parse::<i64>() {
                Ok(value) => GenericVmNumber::I64(value),
                Err(_) => {
                    lowering_miss("unsupported_expr");
                    return Ok(None);
                }
            },
            GenericParseNumber::Float => match line.trim().parse::<f64>() {
                Ok(value) => GenericVmNumber::F64(value),
                Err(_) => {
                    lowering_miss("unsupported_expr");
                    return Ok(None);
                }
            },
        };
        accumulator = generic_vm_add_number(accumulator, parsed)?;
        last_value = Some(RuntimeValue::Nu(Value::string(
            line.clone(),
            Span::unknown(),
        )));
    }

    Ok(Some(GenericVmAssignResult {
        value: RuntimeValue::Nu(generic_vm_number_to_value(accumulator)),
        last_value,
    }))
}

pub(super) fn execute_generic_vm_map_add_i64_const(
    mut counts: HashMap<String, i64>,
    addend: i64,
    values: &[RuntimeValue],
    mut value_to_key: impl FnMut(&Value) -> Result<String, ShellError>,
    mut lowering_miss: impl FnMut(&'static str),
) -> Result<Option<GenericVmMapResult>, ShellError> {
    let mut last_value = None;
    for value in values {
        let RuntimeValue::Nu(value) = value else {
            lowering_miss("unsupported_expr");
            return Ok(None);
        };
        let Ok(key) = value_to_key(value) else {
            lowering_miss("unsupported_expr");
            return Ok(None);
        };
        let total = counts.entry(key).or_insert(0);
        *total = total
            .checked_add(addend)
            .ok_or_else(|| stone_error("hot loop", "integer addition overflow"))?;
        last_value = Some(RuntimeValue::Nu(value.clone()));
    }

    Ok(Some(GenericVmMapResult { counts, last_value }))
}

pub(super) fn generic_vm_register_i64(
    registers: &[Option<i64>],
    reg: usize,
) -> Result<i64, ShellError> {
    registers
        .get(reg)
        .copied()
        .flatten()
        .ok_or_else(|| stone_error("hot loop", "VM register is unset"))
}

pub(super) fn generic_vm_set_register(
    registers: &mut [Option<i64>],
    reg: usize,
    value: i64,
) -> Result<(), ShellError> {
    let Some(slot) = registers.get_mut(reg) else {
        return Err(stone_error("hot loop", "VM register is out of range"));
    };
    *slot = Some(value);
    Ok(())
}

pub(super) fn generic_vm_store_i64_binop(
    registers: &mut [Option<i64>],
    dst: usize,
    lhs: usize,
    rhs: usize,
    context: &str,
    op: impl FnOnce(i64, i64) -> Option<i64>,
) -> Result<(), ShellError> {
    let left = generic_vm_register_i64(registers, lhs)?;
    let right = generic_vm_register_i64(registers, rhs)?;
    let value = op(left, right)
        .ok_or_else(|| stone_error(context, format!("integer {context} overflow")))?;
    generic_vm_set_register(registers, dst, value)
}

pub(super) fn generic_vm_store_shift(
    registers: &mut [Option<i64>],
    dst: usize,
    lhs: usize,
    rhs: usize,
    context: &str,
    op: impl FnOnce(i64, u32) -> Option<i64>,
) -> Result<(), ShellError> {
    let left = generic_vm_register_i64(registers, lhs)?;
    let right = generic_vm_register_i64(registers, rhs)?;
    let shift = u32::try_from(right)
        .map_err(|_| stone_error(context, "shift count must be non-negative"))?;
    let value = op(left, shift).ok_or_else(|| stone_error(context, "shift count is too large"))?;
    generic_vm_set_register(registers, dst, value)
}

pub(super) fn execute_generic_vm_expr_ops(
    body: &GenericVmExprBody,
    locals: &mut [Option<i64>],
    registers: &mut [Option<i64>],
    mut lowering_miss: impl FnMut(&'static str),
) -> Result<bool, ShellError> {
    for op in &body.ops {
        match *op {
            GenericVmExprOp::LoadLocal { dst, local } => {
                let Some(value) = locals.get(local).copied().flatten() else {
                    lowering_miss("unsupported_expr_name");
                    return Ok(false);
                };
                let Some(slot) = registers.get_mut(dst) else {
                    return Err(stone_error("hot loop", "VM register is out of range"));
                };
                *slot = Some(value);
            }
            GenericVmExprOp::StoreLocal { local, src } => {
                let value = generic_vm_register_i64(registers, src)?;
                let Some(slot) = locals.get_mut(local) else {
                    return Err(stone_error("hot loop", "VM local is out of range"));
                };
                *slot = Some(value);
            }
            GenericVmExprOp::LoadConst { dst, constant } => {
                let Some(GenericVmConst::I64(value)) = body.constants.get(constant) else {
                    return Err(stone_error("hot loop", "VM constant is out of range"));
                };
                let Some(slot) = registers.get_mut(dst) else {
                    return Err(stone_error("hot loop", "VM register is out of range"));
                };
                *slot = Some(*value);
            }
            GenericVmExprOp::AddI64 { dst, lhs, rhs } => {
                generic_vm_store_i64_binop(registers, dst, lhs, rhs, "addition", i64::checked_add)?;
            }
            GenericVmExprOp::SubI64 { dst, lhs, rhs } => {
                generic_vm_store_i64_binop(
                    registers,
                    dst,
                    lhs,
                    rhs,
                    "subtraction",
                    i64::checked_sub,
                )?;
            }
            GenericVmExprOp::MulI64 { dst, lhs, rhs } => {
                generic_vm_store_i64_binop(
                    registers,
                    dst,
                    lhs,
                    rhs,
                    "multiplication",
                    i64::checked_mul,
                )?;
            }
            GenericVmExprOp::FloorDivI64 { dst, lhs, rhs } => {
                let left = generic_vm_register_i64(registers, lhs)?;
                let right = generic_vm_register_i64(registers, rhs)?;
                if right == 0 {
                    return Err(stone_error("floor division", "division by zero"));
                }
                generic_vm_set_register(registers, dst, left.div_euclid(right))?;
            }
            GenericVmExprOp::BitAndI64 { dst, lhs, rhs } => {
                let value = generic_vm_register_i64(registers, lhs)?
                    & generic_vm_register_i64(registers, rhs)?;
                generic_vm_set_register(registers, dst, value)?;
            }
            GenericVmExprOp::BitOrI64 { dst, lhs, rhs } => {
                let value = generic_vm_register_i64(registers, lhs)?
                    | generic_vm_register_i64(registers, rhs)?;
                generic_vm_set_register(registers, dst, value)?;
            }
            GenericVmExprOp::BitXorI64 { dst, lhs, rhs } => {
                let value = generic_vm_register_i64(registers, lhs)?
                    ^ generic_vm_register_i64(registers, rhs)?;
                generic_vm_set_register(registers, dst, value)?;
            }
            GenericVmExprOp::ShlI64 { dst, lhs, rhs } => {
                generic_vm_store_shift(registers, dst, lhs, rhs, "left shift", i64::checked_shl)?;
            }
            GenericVmExprOp::ShrI64 { dst, lhs, rhs } => {
                generic_vm_store_shift(registers, dst, lhs, rhs, "right shift", i64::checked_shr)?;
            }
            GenericVmExprOp::BitNotI64 { dst, src } => {
                let value = !generic_vm_register_i64(registers, src)?;
                generic_vm_set_register(registers, dst, value)?;
            }
        }
    }
    Ok(true)
}

pub(super) fn execute_generic_vm_expr_body(
    body: &GenericVmExprBody,
    mut locals: Vec<Option<i64>>,
    values: &[RuntimeValue],
    mut lowering_miss: impl FnMut(&'static str),
) -> Result<Option<GenericVmExprBodyResult>, ShellError> {
    let mut last_value = None;
    let mut registers = vec![None; body.registers];
    for value in values {
        let RuntimeValue::Nu(value) = value else {
            lowering_miss("unsupported_value_type");
            return Ok(None);
        };
        let Value::Int { val, .. } = value else {
            lowering_miss("unsupported_value_type");
            return Ok(None);
        };
        if locals.is_empty() {
            lowering_miss("unsupported_body_stmt");
            return Ok(None);
        }
        locals[0] = Some(*val);
        registers.fill(None);
        if !execute_generic_vm_expr_ops(body, &mut locals, &mut registers, &mut lowering_miss)? {
            return Ok(None);
        }
        last_value = Some(RuntimeValue::Nu(value.clone()));
    }

    Ok(Some(GenericVmExprBodyResult { locals, last_value }))
}
