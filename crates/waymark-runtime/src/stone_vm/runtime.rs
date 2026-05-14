// SPDX-License-Identifier: MIT OR Apache-2.0

use std::time::Instant;

use nu_protocol::{PipelineData, ShellError, Span, Value};

use super::stone_json_view::{jsonl_row_views, runtime_value_to_string_key, JsonlRows};
use super::stone_runtime_value::{RuntimeValue, TextLines};
use super::stone_vm_interp::{
    execute_generic_vm_add_assign as execute_generic_vm_add_assign_loop,
    execute_generic_vm_expr_body as execute_generic_vm_expr_body_loop,
    execute_generic_vm_list_append as execute_generic_vm_list_append_loop,
    execute_generic_vm_map_add_i64_const as execute_generic_vm_map_add_i64_const_loop,
    execute_generic_vm_map_add_i64_const_record_field as execute_generic_vm_map_add_i64_const_record_field_loop,
    execute_generic_vm_map_add_i64_const_record_string_field as execute_generic_vm_map_add_i64_const_record_string_field_loop,
    execute_generic_vm_text_parse_add_assign as execute_generic_vm_text_parse_add_assign_loop,
    GenericVmInput, GenericVmLoopResult,
};
use super::{stone_error, value_to_string, EvalFlow, Evaluator};
use crate::stone_builtins::values_equal;
use crate::stone_vm::{
    compile_generic_vm_function, generic_loop_compile_miss_reason, optimize_loop_ir,
    GenericLoopIter, GenericLoopOp, GenericLoopPlan, GenericParseNumber, GenericVmExprBody,
    GenericVmFunction, GenericVmOp, LoopIrFusedKernel, LoopIrOptimizationResult,
};

impl Evaluator<'_> {
    pub(super) fn try_eval_for_values_generic_vm(
        &mut self,
        targets: &[String],
        values: &[RuntimeValue],
        plan: &GenericLoopPlan,
    ) -> Result<Option<EvalFlow>, ShellError> {
        let Some(function) = compile_generic_vm_function(plan) else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss(generic_loop_compile_miss_reason(plan));
            return Ok(None);
        };
        let optimization = optimize_loop_ir(&function);
        let fused_kernel = self.record_loop_ir_function_selection(&optimization);
        let execution_started = Instant::now();
        let result = self
            .execute_generic_vm_function(&optimization.function, GenericVmInput::Values(values))?;
        let execution_duration = execution_started.elapsed();
        let GenericVmLoopResult::Executed { last_value } = result else {
            self.state.hot_loop_diagnostics.loop_fallback();
            return Ok(None);
        };
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.finish_generic_vm_loop(targets, last_value);
        if let Some(kernel) = fused_kernel {
            self.state
                .hot_loop_diagnostics
                .fused_kernel_executed(kernel);
            self.state
                .hot_loop_diagnostics
                .fused_kernel_time(execution_duration);
        }
        Ok(Some(EvalFlow::Output(PipelineData::empty())))
    }

    pub(super) fn try_eval_for_text_lines_generic_vm(
        &mut self,
        targets: &[String],
        lines: &TextLines,
        plan: &GenericLoopPlan,
    ) -> Result<Option<EvalFlow>, ShellError> {
        if !matches!(
            plan.iter,
            GenericLoopIter::OpenSplitlines | GenericLoopIter::MaterializedList
        ) {
            return Ok(None);
        }
        if let [GenericLoopOp::JsonlAggregation { .. }] = plan.ops.as_slice() {
            self.eval_for_text_lines_jsonl_generic_native_body(targets, lines, plan)?;
            return Ok(Some(EvalFlow::Output(PipelineData::empty())));
        }
        let Some(function) = compile_generic_vm_function(plan) else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss(generic_loop_compile_miss_reason(plan));
            return Ok(None);
        };
        let optimization = optimize_loop_ir(&function);
        let fused_kernel = self.record_loop_ir_function_selection(&optimization);
        let execution_started = Instant::now();
        let result = self.execute_generic_vm_function(
            &optimization.function,
            GenericVmInput::TextLines(lines),
        )?;
        let execution_duration = execution_started.elapsed();
        let GenericVmLoopResult::Executed { last_value } = result else {
            self.state.hot_loop_diagnostics.loop_fallback();
            return Ok(None);
        };
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.finish_generic_vm_loop(targets, last_value);
        if let Some(kernel) = fused_kernel {
            self.state
                .hot_loop_diagnostics
                .fused_kernel_executed(kernel);
            self.state
                .hot_loop_diagnostics
                .fused_kernel_time(execution_duration);
        }
        Ok(Some(EvalFlow::Output(PipelineData::empty())))
    }

    pub(super) fn try_eval_for_jsonl_rows_generic_vm(
        &mut self,
        targets: &[String],
        rows: &JsonlRows,
        plan: &GenericLoopPlan,
    ) -> Result<Option<EvalFlow>, ShellError> {
        if !matches!(
            plan.iter,
            GenericLoopIter::ReadJsonl
                | GenericLoopIter::ReadCsv
                | GenericLoopIter::MaterializedList
        ) {
            return Ok(None);
        }
        let [op] = plan.ops.as_slice() else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss(generic_loop_compile_miss_reason(plan));
            return Ok(None);
        };
        let GenericLoopOp::JsonlAggregation { .. } = op else {
            let Some(function) = compile_generic_vm_function(plan) else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss(generic_loop_compile_miss_reason(plan));
                return Ok(None);
            };
            let optimization = optimize_loop_ir(&function);
            let fused_kernel = self.record_loop_ir_function_selection(&optimization);
            let values = jsonl_row_views(rows);
            let execution_started = Instant::now();
            let result = self.execute_generic_vm_function(
                &optimization.function,
                GenericVmInput::Values(&values),
            )?;
            let execution_duration = execution_started.elapsed();
            let GenericVmLoopResult::Executed { last_value } = result else {
                self.state.hot_loop_diagnostics.loop_fallback();
                return Ok(None);
            };
            self.state
                .hot_loop_diagnostics
                .loop_vm_time(execution_duration);
            self.finish_generic_vm_loop(targets, last_value);
            if let Some(kernel) = fused_kernel {
                self.state
                    .hot_loop_diagnostics
                    .fused_kernel_executed(kernel);
                self.state
                    .hot_loop_diagnostics
                    .fused_kernel_time(execution_duration);
            }
            return Ok(Some(EvalFlow::Output(PipelineData::empty())));
        };
        self.eval_for_jsonl_rows_generic_native_body(targets, rows, plan)?;
        Ok(Some(EvalFlow::Output(PipelineData::empty())))
    }

    fn record_loop_ir_function_selection(
        &mut self,
        optimization: &LoopIrOptimizationResult,
    ) -> Option<LoopIrFusedKernel> {
        self.state.hot_loop_diagnostics.loop_ir_lowered();
        self.state
            .hot_loop_diagnostics
            .loop_ir_optimized(&optimization.diagnostics);
        if let Some(kernel) = optimization.selected_kernel {
            self.state
                .hot_loop_diagnostics
                .fused_kernel_selected(kernel);
            Some(kernel)
        } else {
            self.state
                .hot_loop_diagnostics
                .fusion_miss("no_fused_kernel");
            None
        }
    }

    pub(super) fn execute_generic_vm_function(
        &mut self,
        function: &GenericVmFunction,
        input: GenericVmInput<'_>,
    ) -> Result<GenericVmLoopResult, ShellError> {
        let [op] = function.ops.as_slice() else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss("unsupported_body_stmt");
            return Ok(GenericVmLoopResult::Unsupported);
        };
        let local_name = |local| {
            function
                .local_name(local)
                .ok_or_else(|| stone_error("hot loop", "generic VM local is out of range"))
        };
        match (op, input) {
            (GenericVmOp::AddAssign { local }, GenericVmInput::Values(values)) => {
                self.execute_generic_vm_add_assign(local_name(*local)?, values)
            }
            (GenericVmOp::AddAssignParsed { local, parse }, GenericVmInput::TextLines(lines))
                if function.iter == GenericLoopIter::OpenSplitlines =>
            {
                self.execute_generic_vm_text_parse_add_assign(local_name(*local)?, lines, *parse)
            }
            (GenericVmOp::MapAddI64Const { map, addend }, GenericVmInput::Values(values)) => {
                self.execute_generic_vm_map_add_i64_const(local_name(*map)?, *addend, values)
            }
            (
                GenericVmOp::MapAddI64ConstRecordField { map, field, addend },
                GenericVmInput::Values(values),
            ) => self.execute_generic_vm_map_add_i64_const_record_field(
                local_name(*map)?,
                field,
                *addend,
                values,
            ),
            (
                GenericVmOp::MapAddI64ConstRecordStringField {
                    map,
                    field,
                    strip,
                    lower,
                    addend,
                },
                GenericVmInput::Values(values),
            ) => self.execute_generic_vm_map_add_i64_const_record_string_field(
                local_name(*map)?,
                field,
                *strip,
                *lower,
                *addend,
                values,
            ),
            (GenericVmOp::ListAppend { list, unique }, GenericVmInput::Values(values)) => {
                self.execute_generic_vm_list_append(local_name(*list)?, values, *unique)
            }
            (GenericVmOp::ExprBody(body), GenericVmInput::Values(values)) => {
                self.execute_generic_vm_expr_body(function, body, values)
            }
            _ => {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_iterator");
                Ok(GenericVmLoopResult::Unsupported)
            }
        }
    }

    pub(super) fn finish_generic_vm_loop(
        &mut self,
        targets: &[String],
        last_value: Option<RuntimeValue>,
    ) {
        if let Some(value) = last_value {
            if let Some(target) = targets.first() {
                self.state.set_local(target.clone(), value);
            }
        }
        self.state.hot_loop_diagnostics.loop_vm_executed();
    }

    fn execute_generic_vm_add_assign(
        &mut self,
        local: &str,
        values: &[RuntimeValue],
    ) -> Result<GenericVmLoopResult, ShellError> {
        let Some(local_value) = self.state.get_local(local) else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss("unsupported_expr");
            return Ok(GenericVmLoopResult::Unsupported);
        };
        let Some(result) = execute_generic_vm_add_assign_loop(local_value, values, |reason| {
            self.state.hot_loop_diagnostics.lowering_miss(reason);
        })?
        else {
            return Ok(GenericVmLoopResult::Unsupported);
        };
        self.state.set_local(local.to_owned(), result.value);
        Ok(GenericVmLoopResult::Executed {
            last_value: result.last_value,
        })
    }

    fn execute_generic_vm_text_parse_add_assign(
        &mut self,
        local: &str,
        lines: &TextLines,
        parse: GenericParseNumber,
    ) -> Result<GenericVmLoopResult, ShellError> {
        let Some(local_value) = self.state.get_local(local) else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss("unsupported_expr");
            return Ok(GenericVmLoopResult::Unsupported);
        };
        let Some(result) =
            execute_generic_vm_text_parse_add_assign_loop(local_value, lines, parse, |reason| {
                self.state.hot_loop_diagnostics.lowering_miss(reason);
            })?
        else {
            return Ok(GenericVmLoopResult::Unsupported);
        };
        self.state.set_local(local.to_owned(), result.value);
        Ok(GenericVmLoopResult::Executed {
            last_value: result.last_value,
        })
    }

    fn execute_generic_vm_map_add_i64_const(
        &mut self,
        map: &str,
        addend: i64,
        values: &[RuntimeValue],
    ) -> Result<GenericVmLoopResult, ShellError> {
        let counts = self.load_i64_record_map(map)?;
        let Some(result) = execute_generic_vm_map_add_i64_const_loop(
            counts,
            addend,
            values,
            |value| value_to_string(value, "hot loop"),
            |reason| self.state.hot_loop_diagnostics.lowering_miss(reason),
        )?
        else {
            return Ok(GenericVmLoopResult::Unsupported);
        };
        self.store_i64_record_map(map, &result.counts);
        Ok(GenericVmLoopResult::Executed {
            last_value: result.last_value,
        })
    }

    fn execute_generic_vm_map_add_i64_const_record_field(
        &mut self,
        map: &str,
        field: &str,
        addend: i64,
        values: &[RuntimeValue],
    ) -> Result<GenericVmLoopResult, ShellError> {
        let counts = self.load_i64_record_map(map)?;
        let Some(result) = execute_generic_vm_map_add_i64_const_record_field_loop(
            counts,
            field,
            addend,
            values,
            |value| runtime_value_to_string_key(value, "hot loop"),
            |reason| self.state.hot_loop_diagnostics.lowering_miss(reason),
        )?
        else {
            return Ok(GenericVmLoopResult::Unsupported);
        };
        self.store_i64_record_map(map, &result.counts);
        Ok(GenericVmLoopResult::Executed {
            last_value: result.last_value,
        })
    }

    fn execute_generic_vm_map_add_i64_const_record_string_field(
        &mut self,
        map: &str,
        field: &str,
        strip: bool,
        lower: bool,
        addend: i64,
        values: &[RuntimeValue],
    ) -> Result<GenericVmLoopResult, ShellError> {
        let counts = self.load_i64_record_map(map)?;
        let Some(result) = execute_generic_vm_map_add_i64_const_record_string_field_loop(
            counts,
            field,
            strip,
            lower,
            addend,
            values,
            |value| runtime_value_to_string_key(value, "hot loop"),
            |reason| self.state.hot_loop_diagnostics.lowering_miss(reason),
        )?
        else {
            return Ok(GenericVmLoopResult::Unsupported);
        };
        self.store_i64_record_map(map, &result.counts);
        Ok(GenericVmLoopResult::Executed {
            last_value: result.last_value,
        })
    }

    fn execute_generic_vm_list_append(
        &mut self,
        list: &str,
        values: &[RuntimeValue],
        unique: bool,
    ) -> Result<GenericVmLoopResult, ShellError> {
        let current = self
            .state
            .get_local(list)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{list}`")))?;
        let Some(result) =
            execute_generic_vm_list_append_loop(current, values, unique, values_equal, |reason| {
                self.state.hot_loop_diagnostics.lowering_miss(reason)
            })?
        else {
            return Ok(GenericVmLoopResult::Unsupported);
        };
        self.state.set_local(
            list.to_owned(),
            RuntimeValue::Nu(Value::list(result.items, Span::unknown())),
        );
        Ok(GenericVmLoopResult::Executed {
            last_value: result.last_value,
        })
    }

    fn execute_generic_vm_expr_body(
        &mut self,
        function: &GenericVmFunction,
        body: &GenericVmExprBody,
        values: &[RuntimeValue],
    ) -> Result<GenericVmLoopResult, ShellError> {
        let mut locals = Vec::with_capacity(function.locals.len());
        for local in &function.locals {
            let value = self
                .state
                .get_local(local)
                .and_then(|value| value.into_nu_value("hot loop").ok())
                .and_then(|value| match value {
                    Value::Int { val, .. } => Some(val),
                    _ => None,
                });
            locals.push(value);
        }

        let Some(result) = execute_generic_vm_expr_body_loop(body, locals, values, |reason| {
            self.state.hot_loop_diagnostics.lowering_miss(reason);
        })?
        else {
            return Ok(GenericVmLoopResult::Unsupported);
        };

        for (name, value) in function.locals.iter().zip(result.locals.into_iter()) {
            if let Some(value) = value {
                self.state.set_local(
                    name.clone(),
                    RuntimeValue::Nu(Value::int(value, Span::unknown())),
                );
            }
        }

        Ok(GenericVmLoopResult::Executed {
            last_value: result.last_value,
        })
    }
}
