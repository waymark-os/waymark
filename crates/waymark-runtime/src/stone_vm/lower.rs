// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{
    GenericLoopIter, GenericParseNumber, GenericVmConst, GenericVmExprBody, GenericVmExprOp,
    GenericVmOp, LoopIrBlock, LoopIrDiagnostics, LoopIrFunction, LoopIrIteratorAdapter,
    LoopIrSnapshot, LoopIrSnapshotBoundary, LoopIrTerminator, StoneIrFunction,
};

use crate::stone_ast::{AssignTarget, AugOp, Expr, Stmt};
use crate::stone_ir::{compile_hot_jsonl_vm_function, GenericLoopOp, GenericLoopPlan};

pub(crate) fn compile_generic_vm_function(plan: &GenericLoopPlan) -> Option<LoopIrFunction> {
    let [op] = plan.ops.as_slice() else {
        return None;
    };
    let mut locals = Vec::new();
    let mut registers = 0;
    let mut constants = Vec::new();
    let lowering_path = match op {
        GenericLoopOp::AddAssign { local, item } => {
            if item != &plan.target {
                return None;
            }
            let local = generic_vm_local(&mut locals, local);
            ("add_assign", GenericVmOp::AddAssign { local })
        }
        GenericLoopOp::AddAssignParsedInt { local, item } => {
            if item != &plan.target {
                return None;
            }
            let local = generic_vm_local(&mut locals, local);
            (
                "add_assign_parsed_int",
                GenericVmOp::AddAssignParsed {
                    local,
                    parse: GenericParseNumber::Int,
                },
            )
        }
        GenericLoopOp::AddAssignParsedFloat { local, item } => {
            if item != &plan.target {
                return None;
            }
            let local = generic_vm_local(&mut locals, local);
            (
                "add_assign_parsed_float",
                GenericVmOp::AddAssignParsed {
                    local,
                    parse: GenericParseNumber::Float,
                },
            )
        }
        GenericLoopOp::MapAddI64Const { map, key, value } => {
            if key != &plan.target {
                return None;
            }
            let map = generic_vm_local(&mut locals, map);
            (
                "map_add_i64_const",
                GenericVmOp::MapAddI64Const {
                    map,
                    addend: *value,
                },
            )
        }
        GenericLoopOp::MapAddI64ConstRecordStringField {
            map,
            item,
            field,
            strip,
            lower,
            value,
        } => {
            if item != &plan.target {
                return None;
            }
            let map = generic_vm_local(&mut locals, map);
            (
                "map_add_i64_const_record_string_field",
                GenericVmOp::MapAddI64ConstRecordStringField {
                    map,
                    field: field.clone(),
                    strip: *strip,
                    lower: *lower,
                    addend: *value,
                },
            )
        }
        GenericLoopOp::ListAppend { list, item } => {
            if item != &plan.target {
                return None;
            }
            let list = generic_vm_local(&mut locals, list);
            (
                "list_append",
                GenericVmOp::ListAppend {
                    list,
                    unique: false,
                },
            )
        }
        GenericLoopOp::ListAppendUnique { list, item } => {
            if item != &plan.target {
                return None;
            }
            let list = generic_vm_local(&mut locals, list);
            (
                "list_append_unique",
                GenericVmOp::ListAppend { list, unique: true },
            )
        }
        GenericLoopOp::JsonlAggregation { .. } => return None,
        GenericLoopOp::ExprBody { body } => {
            let body = compile_generic_vm_expr_body(&plan.target, body, &mut locals)?;
            registers = body.registers;
            constants = body.constants.clone();
            ("expr_body", GenericVmOp::ExprBody(body))
        }
    };
    let (lowering_path, op) = lowering_path;
    let ops = vec![op];
    let snapshots = vec![
        LoopIrSnapshot {
            locals: (0..locals.len()).collect(),
            boundary: LoopIrSnapshotBoundary::LoopEntry,
        },
        LoopIrSnapshot {
            locals: (0..locals.len()).collect(),
            boundary: LoopIrSnapshotBoundary::IterationEnd,
        },
    ];
    Some(LoopIrFunction {
        iter: plan.iter.clone(),
        adapter: loop_ir_iterator_adapter(plan),
        locals,
        registers,
        constants,
        blocks: vec![LoopIrBlock {
            ops: ops.clone(),
            terminator: LoopIrTerminator::Return,
        }],
        entry: 0,
        snapshots,
        diagnostics: LoopIrDiagnostics { lowering_path },
        ops,
    })
}

pub(crate) fn compile_hot_jsonl_loop_ir_function(
    plan: &GenericLoopPlan,
) -> Option<StoneIrFunction> {
    let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
        return None;
    };
    let mut function = compile_hot_jsonl_vm_function(body)?;
    function.adapter = Some(loop_ir_iterator_adapter(plan));
    Some(function)
}

pub(crate) fn loop_ir_iterator_adapter(plan: &GenericLoopPlan) -> LoopIrIteratorAdapter {
    match (&plan.iter, plan.ops.as_slice()) {
        (GenericLoopIter::OpenSplitlines, [GenericLoopOp::JsonlAggregation { .. }]) => {
            LoopIrIteratorAdapter::JsonlRows { guarded: true }
        }
        (GenericLoopIter::ReadJsonl, _) => LoopIrIteratorAdapter::JsonlRows { guarded: false },
        (GenericLoopIter::ReadCsv, _) => LoopIrIteratorAdapter::CsvRows,
        (GenericLoopIter::OpenSplitlines, _) => LoopIrIteratorAdapter::TextLines,
        (GenericLoopIter::Range, _) => LoopIrIteratorAdapter::RangeValues,
        (GenericLoopIter::MaterializedList, _) => LoopIrIteratorAdapter::MaterializedValues,
    }
}

struct GenericVmExprBuilder<'a> {
    locals: &'a mut Vec<String>,
    constants: Vec<GenericVmConst>,
    ops: Vec<GenericVmExprOp>,
    next_register: usize,
}

impl<'a> GenericVmExprBuilder<'a> {
    fn new(locals: &'a mut Vec<String>) -> Self {
        Self {
            locals,
            constants: Vec::new(),
            ops: Vec::new(),
            next_register: 0,
        }
    }

    fn finish(self) -> GenericVmExprBody {
        GenericVmExprBody {
            registers: self.next_register,
            constants: self.constants,
            ops: self.ops,
        }
    }

    fn reg(&mut self) -> usize {
        let reg = self.next_register;
        self.next_register += 1;
        reg
    }

    fn local(&mut self, name: &str) -> usize {
        generic_vm_local(self.locals, name)
    }

    fn constant(&mut self, constant: GenericVmConst) -> usize {
        if let Some(index) = self.constants.iter().position(|item| item == &constant) {
            return index;
        }
        let index = self.constants.len();
        self.constants.push(constant);
        index
    }

    fn lower_stmt(&mut self, stmt: &Stmt) -> Option<()> {
        match stmt {
            Stmt::Assign {
                target: AssignTarget::Name(local),
                value,
            } => {
                let src = self.lower_expr(value)?;
                let local = self.local(local);
                self.ops.push(GenericVmExprOp::StoreLocal { local, src });
                Some(())
            }
            Stmt::AugAssign {
                target: AssignTarget::Name(local),
                op: AugOp::Add,
                value,
            } => {
                let lhs = self.reg();
                let local_id = self.local(local);
                self.ops.push(GenericVmExprOp::LoadLocal {
                    dst: lhs,
                    local: local_id,
                });
                let rhs = self.lower_expr(value)?;
                let dst = self.reg();
                self.ops.push(GenericVmExprOp::AddI64 { dst, lhs, rhs });
                self.ops.push(GenericVmExprOp::StoreLocal {
                    local: local_id,
                    src: dst,
                });
                Some(())
            }
            _ => None,
        }
    }

    fn lower_expr(&mut self, expr: &Expr) -> Option<usize> {
        match expr {
            Expr::Int(value) => {
                let value = value.parse().ok()?;
                let constant = self.constant(GenericVmConst::I64(value));
                let dst = self.reg();
                self.ops.push(GenericVmExprOp::LoadConst { dst, constant });
                Some(dst)
            }
            Expr::Name(name) => {
                let local = self.local(name);
                let dst = self.reg();
                self.ops.push(GenericVmExprOp::LoadLocal { dst, local });
                Some(dst)
            }
            Expr::Invert(value) => {
                let src = self.lower_expr(value)?;
                let dst = self.reg();
                self.ops.push(GenericVmExprOp::BitNotI64 { dst, src });
                Some(dst)
            }
            Expr::Add { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::AddI64 { dst, lhs, rhs }
            }),
            Expr::Sub { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::SubI64 { dst, lhs, rhs }
            }),
            Expr::Mul { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::MulI64 { dst, lhs, rhs }
            }),
            Expr::FloorDiv { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::FloorDivI64 { dst, lhs, rhs }
            }),
            Expr::BitAnd { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::BitAndI64 { dst, lhs, rhs }
            }),
            Expr::BitOr { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::BitOrI64 { dst, lhs, rhs }
            }),
            Expr::BitXor { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::BitXorI64 { dst, lhs, rhs }
            }),
            Expr::LShift { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::ShlI64 { dst, lhs, rhs }
            }),
            Expr::RShift { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::ShrI64 { dst, lhs, rhs }
            }),
            _ => None,
        }
    }

    fn lower_binary(
        &mut self,
        left: &Expr,
        right: &Expr,
        op: fn(usize, usize, usize) -> GenericVmExprOp,
    ) -> Option<usize> {
        let lhs = self.lower_expr(left)?;
        let rhs = self.lower_expr(right)?;
        let dst = self.reg();
        self.ops.push(op(dst, lhs, rhs));
        Some(dst)
    }
}

fn compile_generic_vm_expr_body(
    loop_target: &str,
    body: &[Stmt],
    locals: &mut Vec<String>,
) -> Option<GenericVmExprBody> {
    let mut builder = GenericVmExprBuilder::new(locals);
    builder.local(loop_target);
    for stmt in body {
        builder.lower_stmt(stmt)?;
    }
    Some(builder.finish())
}

fn generic_vm_local(locals: &mut Vec<String>, name: &str) -> usize {
    if let Some(index) = locals.iter().position(|local| local == name) {
        return index;
    }
    let index = locals.len();
    locals.push(name.to_owned());
    index
}
