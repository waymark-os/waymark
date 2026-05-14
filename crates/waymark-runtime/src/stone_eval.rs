// SPDX-License-Identifier: MIT OR Apache-2.0

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nu_protocol::{
    engine::{EngineState, Stack},
    shell_error::generic::GenericError,
    IntoPipelineData, PipelineData, Record, ShellError, Span, Value,
};
use serde_json::{json, Value as JsonValue};

use crate::commands::{stone_help_overview, stone_help_topic};
use crate::json::{json_to_nu_value, nu_to_json_value};
use crate::stone_ast::{
    AssignTarget, AugOp, BoolOp, Call, CompareOp, Expr, FormattedStringPart, FunctionDef, Program,
    Stmt, StoneFormatSpec, StoneType,
};
use crate::stone_builtins::{
    add_values, bitwise_int_values, compare_values, div_values, enumerate_builtin,
    find_method_builtin, first_builtin, float_builtin, floor_div_values, format_builtin,
    index_method_builtin, int_builtin, join_builtin, last_builtin, len_builtin, list_builtin,
    map_builtin_value, min_max_builtin, mod_values, mul_values, neg_value, normalize_index,
    parse_float_builtin, parse_int_builtin, range_builtin, record_method_builtin, round_builtin,
    set_builtin, shift_value, slice_builtin, sort_builtin_values, sort_key_for_value,
    split_builtin, starts_with_builtin, str_builtin, string_method_builtin, sub_values,
    subscript_builtin, sum_builtin, unique_builtin, value_identity_key, value_to_bool,
    value_to_display_string, value_to_f64, value_to_i64, value_to_limit, value_to_path_string,
    value_to_string, value_to_u64, value_truthy, value_type_name, values_equal, where_builtin,
    where_compare_builtin, zfill_text,
};
#[cfg(not(target_os = "hermit"))]
use crate::stone_file_ops::{
    cat_text, create_dir_all, diff_record_for_paths, edit_text_file, find_records, io_stone_error,
    list_dir_records, open_runtime_file, read_bytes_for_jsonl, read_csv_file, read_json_file,
    read_text as stone_read_text, remove_path, save_value_file, search_records, stat_record,
    write_json_file, write_jsonl_file, write_text as stone_write_text, RuntimeFile,
    StoneFindOptions,
};
#[cfg(not(target_os = "hermit"))]
use crate::stone_helpers::{
    helper_error_observation, stone_helper_observations_from_value, stone_helper_registry,
    stone_run_event_from_record, stone_run_event_value, StoneHelperHandlerKind, StoneHelperHook,
    StoneHelperRegistry, StoneRunEvent,
};
#[cfg(all(not(target_os = "hermit"), test))]
use crate::stone_run::cleanup_stale_run_temp_files;
#[cfg(not(target_os = "hermit"))]
use crate::stone_run::{
    daemon_status_call_values, resolve_command_call_values, run_call_values,
    start_daemon_call_values, stop_daemon_call_values, wait_port_call_values,
};
use crate::stone_vm::{
    compile_generic_vm_function, compile_hot_jsonl_loop_ir_function, compile_hot_jsonl_trace_plan,
    compile_hot_jsonl_trace_plan_from_ir, compile_hot_jsonl_vm_function,
    generic_loop_compile_miss_reason, match_hot_jsonl_aggregation_body,
    match_outer_jsonl_file_loop_body, optimize_loop_ir, optimize_stone_loop_ir,
    try_lower_generic_loop, try_lower_hot_loop, validate_hot_jsonl_native_prefix, AccId, ConstId,
    GenericLoopIter, GenericLoopOp, GenericLoopPlan, GenericParseNumber, GenericVmExprBody,
    GenericVmFunction, GenericVmOp, HotJsonlAggregationBody, HotJsonlBodyOp,
    HotJsonlNestedUserTotals, HotJsonlSlot, HotJsonlTracePlan, HotLoopIter, HotLoopOp, HotLoopPlan,
    LoopIrFusedKernel, LoopIrOptimizationDiagnostic, LoopIrOptimizationResult, Reg, SnapshotId,
    StoneAccumulatorKind, StoneConst, StoneFallbackTarget, StoneGuardKind, StoneIrFunction,
    StoneLoopIrOptimizationResult, StoneOp, StoneTerminator,
};

#[path = "stone_functions.rs"]
mod stone_functions;
#[path = "stone_json_view.rs"]
mod stone_json_view;
#[path = "stone_runtime_value.rs"]
mod stone_runtime_value;
#[path = "stone_state.rs"]
mod stone_state;
#[path = "stone_vm/interp.rs"]
mod stone_vm_interp;
use stone_functions::CallableValue;
pub(crate) use stone_functions::StoneSession;
use stone_json_view::{
    eval_json_object_view_method, eval_runtime_subscript, find_top_level_json_field,
    json_array_bytes_for_each_range, json_array_view_iter_values, json_key_matches,
    json_number_bytes_to_f64, json_number_bytes_to_i64, json_object_for_each_field,
    json_object_view_get, json_object_view_get_array_default, json_object_view_get_f64_default,
    json_object_view_get_i64_default, json_object_view_get_string_default, json_scalar_view_to_f64,
    json_scalar_view_to_i64, json_string_bytes_to_cow, jsonl_row_view, jsonl_row_views,
    jsonl_rows_from_bytes, materialize_json_array_view, materialize_json_object_view,
    materialize_json_scalar_view, materialize_jsonl_rows, runtime_value_to_string_key,
    trim_json_bytes, JsonObjectView, JsonlRows,
};
use stone_runtime_value::{FileHandle, RuntimeValue, TextLines};
use stone_state::runtime_state_record;
use stone_vm_interp::{
    execute_generic_vm_add_assign as execute_generic_vm_add_assign_loop,
    execute_generic_vm_expr_body as execute_generic_vm_expr_body_loop, generic_vm_add_number,
    generic_vm_number_from_runtime, generic_vm_number_to_value, generic_vm_record_field_value,
    GenericVmInput, GenericVmLoopResult, GenericVmNumber,
};

const STONE_LAST_RESULT_ENV: &str = "WAYMARK_LAST_RESULT_JSON";

#[derive(Default)]
pub struct EvalState {
    scopes: Vec<Scope>,
    functions: HashMap<String, FunctionDef>,
    next_file_id: u64,
    next_callable_id: u64,
    stdout: String,
    session_bound_names: Vec<String>,
    profiler: EvalProfiler,
    hot_loop_diagnostics: EvalHotLoopDiagnostics,
    hot_loop_enabled: bool,
    hot_loop_vm_interpreter: bool,
    hot_loop_validate_snapshot: bool,
    #[cfg(not(target_os = "hermit"))]
    stone_helper_registry: StoneHelperRegistry,
}

#[derive(Default)]
struct EvalHotLoopDiagnostics {
    loop_candidates: u64,
    loop_ir_lowered: u64,
    loop_vm_executed: u64,
    loop_fused_kernels_selected: u64,
    loop_fused_kernels_executed: u64,
    loop_fallbacks: u64,
    loop_vm_total_us: u64,
    loop_fused_kernel_total_us: u64,
    generic_vm_loops_lowered: u64,
    generic_vm_loops_executed: u64,
    jsonl_fused_traces_lowered: u64,
    jsonl_fused_traces_executed: u64,
    lowering_missed_reasons: HashMap<&'static str, u64>,
    fusion_missed_reasons: HashMap<&'static str, u64>,
    optimization_counts: HashMap<&'static str, u64>,
}

impl EvalHotLoopDiagnostics {
    fn lowering_miss(&mut self, reason: &'static str) {
        *self.lowering_missed_reasons.entry(reason).or_insert(0) += 1;
    }

    fn fusion_miss(&mut self, reason: &'static str) {
        *self.fusion_missed_reasons.entry(reason).or_insert(0) += 1;
    }

    fn loop_ir_lowered(&mut self) {
        self.loop_ir_lowered += 1;
        self.generic_vm_loops_lowered += 1;
    }

    fn loop_ir_optimized(&mut self, diagnostics: &[LoopIrOptimizationDiagnostic]) {
        for diagnostic in diagnostics {
            let label = match diagnostic {
                LoopIrOptimizationDiagnostic::Canonicalized => "canonicalized",
            };
            *self.optimization_counts.entry(label).or_insert(0) += 1;
        }
    }

    fn fused_kernel_selected(&mut self, kernel: LoopIrFusedKernel) {
        self.loop_fused_kernels_selected += 1;
        if matches!(kernel, LoopIrFusedKernel::JsonlAggregation) {
            self.jsonl_fused_traces_lowered += 1;
        }
    }

    fn loop_vm_executed(&mut self) {
        self.loop_vm_executed += 1;
        self.generic_vm_loops_executed += 1;
    }

    fn loop_vm_time(&mut self, duration: Duration) {
        self.loop_vm_total_us = self
            .loop_vm_total_us
            .saturating_add(duration.as_micros().min(u128::from(u64::MAX)) as u64);
    }

    fn fused_kernel_executed(&mut self, kernel: LoopIrFusedKernel) {
        self.loop_fused_kernels_executed += 1;
        if matches!(kernel, LoopIrFusedKernel::JsonlAggregation) {
            self.jsonl_fused_traces_executed += 1;
        }
    }

    fn fused_kernel_time(&mut self, duration: Duration) {
        self.loop_fused_kernel_total_us = self
            .loop_fused_kernel_total_us
            .saturating_add(duration.as_micros().min(u128::from(u64::MAX)) as u64);
    }

    fn loop_fallback(&mut self) {
        self.loop_fallbacks += 1;
    }

    fn json_value(
        &self,
        hot_loop_enabled: bool,
        hot_loop_vm_interpreter: bool,
    ) -> Option<JsonValue> {
        if !hot_loop_enabled && self.loop_candidates == 0 {
            return None;
        }
        Some(json!({
            "hot_loop_enabled": hot_loop_enabled,
            "hot_loop_vm_interpreter": hot_loop_vm_interpreter,
            "loop_candidates": self.loop_candidates,
            "loop_ir_lowered": self.loop_ir_lowered,
            "loop_vm_executed": self.loop_vm_executed,
            "loop_fused_kernels_selected": self.loop_fused_kernels_selected,
            "loop_fused_kernels_executed": self.loop_fused_kernels_executed,
            "loop_fallbacks": self.loop_fallbacks,
            "loop_vm_total_us": self.loop_vm_total_us,
            "loop_fused_kernel_total_us": self.loop_fused_kernel_total_us,
            "generic_vm_loops_lowered": self.generic_vm_loops_lowered,
            "generic_vm_loops_executed": self.generic_vm_loops_executed,
            "jsonl_fused_traces_lowered": self.jsonl_fused_traces_lowered,
            "jsonl_fused_traces_executed": self.jsonl_fused_traces_executed,
            "loop_lowering_missed_reasons": self.lowering_missed_reasons,
            "loop_fusion_missed_reasons": self.fusion_missed_reasons,
            "loop_ir_optimization_counts": self.optimization_counts,
        }))
    }

    fn emit(&self, hot_loop_enabled: bool, hot_loop_vm_interpreter: bool) {
        if let Some(value) = self.json_value(hot_loop_enabled, hot_loop_vm_interpreter) {
            eprintln!("WAYMARK_STONE_HOT_LOOP_DIAGNOSTICS {value}");
        }
    }
}

#[derive(Clone, Copy, Default)]
struct EvalProfileMetric {
    calls: u64,
    total: Duration,
}

struct EvalProfiler {
    enabled: bool,
    metrics: [EvalProfileMetric; EVAL_PROFILE_BUCKETS],
}

#[derive(Clone, Copy)]
enum EvalProfileBucket {
    Stmt,
    Expr,
    Block,
    ForJsonlBody,
    ForValuesBody,
    Assign,
    AssignTargetValue,
    MethodCall,
    BuiltinCall,
    Compare,
    BoolOp,
}

const EVAL_PROFILE_BUCKETS: usize = 11;

impl Default for EvalProfiler {
    fn default() -> Self {
        Self {
            enabled: env::var_os("WAYMARK_STONE_EVAL_PROFILE").is_some(),
            metrics: [EvalProfileMetric::default(); EVAL_PROFILE_BUCKETS],
        }
    }
}

impl EvalProfiler {
    fn start(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    fn finish(&mut self, bucket: EvalProfileBucket, started: Option<Instant>) {
        let Some(started) = started else {
            return;
        };
        let metric = &mut self.metrics[bucket as usize];
        metric.calls += 1;
        metric.total += started.elapsed();
    }

    fn emit(&self) {
        if !self.enabled {
            return;
        }
        eprintln!("WAYMARK_STONE_EVAL_PROFILE_BEGIN");
        for (bucket, metric) in EVAL_PROFILE_BUCKET_NAMES.iter().zip(self.metrics.iter()) {
            if metric.calls == 0 {
                continue;
            }
            let total_ms = metric.total.as_secs_f64() * 1000.0;
            let avg_us = metric.total.as_secs_f64() * 1_000_000.0 / metric.calls as f64;
            eprintln!(
                "WAYMARK_STONE_EVAL_PROFILE bucket={} calls={} total_ms={:.3} avg_us={:.3}",
                bucket, metric.calls, total_ms, avg_us
            );
        }
        eprintln!("WAYMARK_STONE_EVAL_PROFILE_END");
    }
}

const EVAL_PROFILE_BUCKET_NAMES: [&str; EVAL_PROFILE_BUCKETS] = [
    "stmt",
    "expr",
    "block",
    "for_jsonl_body",
    "for_values_body",
    "assign",
    "assign_target_value",
    "method_call",
    "builtin_call",
    "compare",
    "bool_op",
];

#[derive(Default)]
struct Scope {
    locals: HashMap<String, RuntimeValue>,
    files: HashMap<u64, RuntimeFile>,
}

struct FusedMapUpdateIf {
    key_name: String,
    contains_map: String,
    updates: Vec<FusedMapUpdate>,
    append_list: Option<String>,
}

struct FusedMapUpdate {
    map_name: String,
    addend: Expr,
}

struct HotJsonlNativeAccumulators {
    user_amounts: HashMap<String, f64>,
    user_items: HashMap<String, i64>,
    users: Vec<String>,
    tag_counts: HashMap<String, i64>,
    tags: Vec<String>,
}

struct HotJsonlRowFields<'a> {
    user: Cow<'a, str>,
    amount: f64,
    items: i64,
    tags: HotJsonlStringArray<'a>,
}

#[derive(Clone, Copy)]
struct HotJsonlStringArray<'a> {
    bytes: &'a [u8],
}

struct HotJsonlRowSlice<'a> {
    bytes: &'a [u8],
    source: &'a str,
    line_number: usize,
}

#[derive(Default)]
struct HotJsonlNativeSlots<'a> {
    fields: Option<HotJsonlRowFields<'a>>,
}

#[derive(Clone)]
enum StoneVmSlot<'a> {
    Empty,
    Row(&'a HotJsonlRowSlice<'a>),
    String(String),
    F64(f64),
    I64(i64),
    StringArray(HotJsonlStringArray<'a>),
}

struct StoneMaterializedSnapshot {
    locals: Vec<(String, RuntimeValue)>,
}

enum StoneVmExecutionResult {
    Completed,
    Fallback { snapshot: SnapshotId },
}

impl EvalState {
    fn root_scope_mut(&mut self) -> &mut Scope {
        if self.scopes.is_empty() {
            self.scopes.push(Scope::default());
        }
        self.scopes
            .last_mut()
            .expect("eval state should have a root scope")
    }

    fn current_scope_index(&mut self) -> usize {
        if self.scopes.is_empty() {
            self.scopes.push(Scope::default());
        }
        self.scopes.len() - 1
    }

    fn get_local(&self, name: &str) -> Option<RuntimeValue> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.locals.get(name).cloned())
    }

    fn set_local(&mut self, name: String, value: RuntimeValue) {
        self.root_scope_mut().locals.insert(name, value);
    }

    fn record_session_binding(&mut self, name: &str) {
        if !self
            .session_bound_names
            .iter()
            .any(|existing| existing == name)
        {
            self.session_bound_names.push(name.to_owned());
        }
    }

    fn get_local_mut(&mut self, name: &str) -> Option<&mut RuntimeValue> {
        self.scopes
            .iter_mut()
            .rev()
            .find_map(|scope| scope.locals.get_mut(name))
    }

    fn remove_local(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.locals.remove(name).is_some() {
                return;
            }
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    fn pop_scope(&mut self) -> Result<(), ShellError> {
        if self.scopes.len() <= 1 {
            return Err(stone_error("scope", "cannot pop root scope"));
        }
        self.scopes.pop();
        Ok(())
    }

    fn pop_scope_merging_nu_locals(&mut self, exclude: Option<&str>) -> Result<(), ShellError> {
        if self.scopes.len() <= 1 {
            return Err(stone_error("scope", "cannot pop root scope"));
        }
        let scope = self.scopes.pop().expect("scope length checked before pop");
        let parent = self
            .scopes
            .last_mut()
            .expect("parent scope exists after checked pop");
        for (name, value) in scope.locals {
            if Some(name.as_str()) == exclude {
                continue;
            }
            if matches!(value, RuntimeValue::Nu(_)) {
                parent.locals.insert(name, value);
            }
        }
        Ok(())
    }

    fn insert_file(&mut self, file: RuntimeFile) -> FileHandle {
        let scope_index = self.current_scope_index();
        let file_id = self.next_file_id;
        self.next_file_id = self.next_file_id.checked_add(1).unwrap_or(0);
        self.scopes[scope_index].files.insert(file_id, file);
        FileHandle {
            scope_index,
            file_id,
        }
    }

    fn next_callable_id(&mut self) -> u64 {
        let id = self.next_callable_id;
        self.next_callable_id = self.next_callable_id.checked_add(1).unwrap_or(0);
        id
    }

    fn capture_locals(&self) -> Vec<(String, RuntimeValue)> {
        let mut seen = HashSet::new();
        let mut captures = Vec::new();
        for scope in self.scopes.iter().rev() {
            for (name, value) in &scope.locals {
                if seen.insert(name.clone()) {
                    captures.push((name.clone(), value.clone()));
                }
            }
        }
        captures
    }

    fn file_mut(&mut self, handle: FileHandle) -> Result<&mut RuntimeFile, ShellError> {
        self.scopes
            .get_mut(handle.scope_index)
            .and_then(|scope| scope.files.get_mut(&handle.file_id))
            .ok_or_else(|| stone_error("file", "file handle is no longer valid"))
    }
}

pub fn eval_program(
    engine_state: &EngineState,
    stack: &mut Stack,
    program: &Program,
    input: PipelineData,
) -> Result<PipelineData, ShellError> {
    eval_program_with_output(engine_state, stack, program, input).map(|output| output.pipeline)
}

pub struct EvalProgramOutput {
    pub pipeline: PipelineData,
    pub stdout: String,
    pub diagnostics: JsonValue,
}

pub fn eval_program_with_output(
    engine_state: &EngineState,
    stack: &mut Stack,
    program: &Program,
    input: PipelineData,
) -> Result<EvalProgramOutput, ShellError> {
    eval_program_with_output_and_session(engine_state, stack, program, input, None)
}

pub(crate) fn eval_program_with_output_and_session(
    engine_state: &EngineState,
    stack: &mut Stack,
    program: &Program,
    input: PipelineData,
    session: Option<&mut StoneSession>,
) -> Result<EvalProgramOutput, ShellError> {
    eval_program_with_options(
        engine_state,
        stack,
        program,
        input,
        EvalOptions {
            hot_loop_enabled: env::var_os("WAYMARK_STONE_HOT_LOOP").is_some(),
            hot_loop_vm_interpreter: env::var_os("WAYMARK_STONE_HOT_LOOP_VM").is_some(),
            hot_loop_validate_snapshot: env::var_os("WAYMARK_STONE_HOT_LOOP_VALIDATE_SNAPSHOT")
                .is_some(),
            session,
        },
    )
}

struct EvalOptions<'a> {
    hot_loop_enabled: bool,
    hot_loop_vm_interpreter: bool,
    hot_loop_validate_snapshot: bool,
    session: Option<&'a mut StoneSession>,
}

fn eval_program_with_options(
    engine_state: &EngineState,
    stack: &mut Stack,
    program: &Program,
    input: PipelineData,
    mut options: EvalOptions<'_>,
) -> Result<EvalProgramOutput, ShellError> {
    #[cfg(not(target_os = "hermit"))]
    let stone_helper_registry = {
        let cwd = engine_state
            .cwd_as_string(Some(stack))
            .map(PathBuf::from)
            .unwrap_or_else(|_| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        stone_helper_registry(&cwd)
    };
    let root_scope = options
        .session
        .as_ref()
        .map(|session| session.root_scope())
        .unwrap_or_default();
    let mut evaluator = Evaluator {
        engine_state,
        stack,
        state: EvalState {
            scopes: vec![root_scope],
            functions: options
                .session
                .as_ref()
                .map(|session| session.functions.clone())
                .unwrap_or_default(),
            next_file_id: 0,
            next_callable_id: 0,
            stdout: String::new(),
            session_bound_names: Vec::new(),
            profiler: EvalProfiler::default(),
            hot_loop_diagnostics: EvalHotLoopDiagnostics::default(),
            hot_loop_enabled: options.hot_loop_enabled,
            hot_loop_vm_interpreter: options.hot_loop_vm_interpreter,
            hot_loop_validate_snapshot: options.hot_loop_validate_snapshot,
            #[cfg(not(target_os = "hermit"))]
            stone_helper_registry,
        },
    };
    let pipeline = evaluator.eval_program(program, input);
    evaluator.state.profiler.emit();
    let hot_loop_diagnostics = evaluator.state.hot_loop_diagnostics.json_value(
        evaluator.state.hot_loop_enabled,
        evaluator.state.hot_loop_vm_interpreter,
    );
    evaluator.state.hot_loop_diagnostics.emit(
        evaluator.state.hot_loop_enabled,
        evaluator.state.hot_loop_vm_interpreter,
    );
    if let Some(session) = options.session.as_mut() {
        if let Some(root_scope) = evaluator.state.scopes.first() {
            session.update_from_root_scope(root_scope);
        }
        session.update_functions(&evaluator.state.functions);
    }
    let pipeline = pipeline?;
    let mut diagnostics = match hot_loop_diagnostics {
        Some(hot_loop) => json!({ "hot_loop": hot_loop }),
        None => json!({}),
    };
    if let Some(root_scope) = evaluator.state.scopes.first() {
        let mut bound = evaluator
            .state
            .session_bound_names
            .iter()
            .filter(|name| {
                root_scope
                    .locals
                    .get(*name)
                    .map(|value| value.is_session_persistable())
                    .unwrap_or_else(|| evaluator.state.functions.contains_key(*name))
            })
            .cloned()
            .collect::<Vec<_>>();
        bound.sort();
        bound.dedup();
        if !bound.is_empty() {
            diagnostics["session"] = json!({ "bound": bound });
        }
    }
    Ok(EvalProgramOutput {
        pipeline,
        stdout: evaluator.state.stdout,
        diagnostics,
    })
}

struct Evaluator<'a> {
    engine_state: &'a EngineState,
    stack: &'a mut Stack,
    state: EvalState,
}

enum EvalFlow {
    Output(PipelineData),
    Break,
    Continue,
    Return(Value),
}

impl Evaluator<'_> {
    fn eval_program(
        &mut self,
        program: &Program,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        match self.eval_block(&program.statements, input, true)? {
            EvalFlow::Output(output) => Ok(output),
            EvalFlow::Break => Err(stone_error("break", "break outside loop")),
            EvalFlow::Continue => Err(stone_error("continue", "continue outside loop")),
            EvalFlow::Return(_) => Err(stone_error("return", "return outside function")),
        }
    }

    fn eval_block(
        &mut self,
        statements: &[Stmt],
        mut input: PipelineData,
        record_session_bindings: bool,
    ) -> Result<EvalFlow, ShellError> {
        let started = self.state.profiler.start();
        let mut output = PipelineData::empty();
        let result = (|| {
            for statement in statements {
                let flow = self.eval_stmt(statement, input, record_session_bindings)?;
                input = PipelineData::empty();
                match flow {
                    EvalFlow::Output(value) => {
                        output = value;
                    }
                    EvalFlow::Break | EvalFlow::Continue | EvalFlow::Return(_) => return Ok(flow),
                }
            }

            Ok(EvalFlow::Output(output))
        })();
        self.state
            .profiler
            .finish(EvalProfileBucket::Block, started);
        result
    }

    fn eval_stmt(
        &mut self,
        statement: &Stmt,
        input: PipelineData,
        record_session_bindings: bool,
    ) -> Result<EvalFlow, ShellError> {
        let started = self.state.profiler.start();
        let result = match statement {
            Stmt::Assign { target, value } => {
                let value = self.eval_expr_value(value, input)?;
                self.assign_value(target, value, record_session_bindings)?;
                Ok(EvalFlow::Output(PipelineData::empty()))
            }
            Stmt::AugAssign { target, op, value } => {
                let left = self.eval_assign_target_value(target)?;
                let right = self
                    .eval_expr_value(value, input)?
                    .into_nu_value("augmented assignment")?;
                let value = match op {
                    AugOp::Add => add_values(&left, &right)?,
                };
                self.assign_value(target, RuntimeValue::Nu(value), record_session_bindings)?;
                Ok(EvalFlow::Output(PipelineData::empty()))
            }
            Stmt::Pass => Ok(EvalFlow::Output(PipelineData::empty())),
            Stmt::FunctionDef(function) => {
                if record_session_bindings {
                    self.state.record_session_binding(&function.name);
                }
                self.state
                    .functions
                    .insert(function.name.clone(), function.clone());
                Ok(EvalFlow::Output(PipelineData::empty()))
            }
            Stmt::Return(value) => {
                let value = match value {
                    Some(value) => self
                        .eval_expr_value(value, input)?
                        .into_nu_value("return value")?,
                    None => Value::nothing(Span::unknown()),
                };
                Ok(EvalFlow::Return(value))
            }
            Stmt::For {
                targets,
                iter,
                body,
            } => self.eval_for_stmt(targets, iter, body),
            Stmt::While { condition, body } => {
                let mut output = PipelineData::empty();
                for _ in 0..1_000_000 {
                    let condition = self
                        .eval_expr_value(condition, PipelineData::empty())?
                        .into_nu_value("while")?;
                    if !value_truthy(&condition) {
                        return Ok(EvalFlow::Output(output));
                    }
                    match self.eval_block(body, PipelineData::empty(), false)? {
                        EvalFlow::Output(value) => output = value,
                        EvalFlow::Break => return Ok(EvalFlow::Output(output)),
                        EvalFlow::Continue => continue,
                        flow @ EvalFlow::Return(_) => return Ok(flow),
                    }
                }
                Err(stone_error(
                    "while",
                    "while loop exceeded 1000000 iterations",
                ))
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if self.state.hot_loop_enabled
                    && self.try_eval_fused_map_update_if(condition, then_branch, else_branch)?
                {
                    return Ok(EvalFlow::Output(PipelineData::empty()));
                }
                let condition = self
                    .eval_expr_value(condition, input)?
                    .into_nu_value("if")?;
                let branch = if value_truthy(&condition) {
                    then_branch
                } else {
                    else_branch
                };
                self.eval_block(branch, PipelineData::empty(), false)
            }
            Stmt::With {
                target,
                context,
                body,
            } => self.eval_with_stmt(target.as_deref(), context, body),
            Stmt::Break => Ok(EvalFlow::Break),
            Stmt::Continue => Ok(EvalFlow::Continue),
            Stmt::Expr(expression) => self
                .eval_expr_pipeline(expression, input)
                .map(EvalFlow::Output),
        };
        self.state.profiler.finish(EvalProfileBucket::Stmt, started);
        result
    }

    fn eval_for_stmt(
        &mut self,
        targets: &[String],
        iter: &Expr,
        body: &[Stmt],
    ) -> Result<EvalFlow, ShellError> {
        let hot_loop_plan = if self.state.hot_loop_enabled {
            if targets.len() == 1 {
                self.state.hot_loop_diagnostics.loop_candidates += 1;
            }
            let plan = try_lower_hot_loop(targets, iter, body);
            let generic_plan = try_lower_generic_loop(targets, iter, body);
            if plan.is_none() && generic_plan.is_none() && targets.len() == 1 {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_iterator");
            }
            plan
        } else {
            None
        };
        let generic_loop_plan = self
            .state
            .hot_loop_enabled
            .then(|| try_lower_generic_loop(targets, iter, body))
            .flatten();
        let iterable = self.eval_expr_value(iter, PipelineData::empty())?;
        match iterable {
            RuntimeValue::JsonlRows(rows) => {
                if let Some(plan) = generic_loop_plan.as_ref() {
                    if let Some(flow) =
                        self.try_eval_for_jsonl_rows_generic_vm(targets, &rows, plan)?
                    {
                        return Ok(flow);
                    }
                }
                if let Some(plan) = hot_loop_plan.as_ref() {
                    self.eval_for_jsonl_rows_hot_prefix(targets, rows, body, plan)
                } else {
                    self.eval_for_jsonl_rows(targets, rows, body)
                }
            }
            RuntimeValue::TextLines(lines) => {
                if let Some(plan) = generic_loop_plan.as_ref() {
                    if let Some(flow) =
                        self.try_eval_for_text_lines_generic_vm(targets, &lines, plan)?
                    {
                        return Ok(flow);
                    }
                }
                if let Some(plan) = hot_loop_plan.as_ref() {
                    if matches!(plan.iter, HotLoopIter::JsonlTextLines { .. }) {
                        return self.eval_for_text_lines_hot_jsonl(targets, lines, body, plan);
                    }
                }
                let values = lines
                    .lines
                    .into_iter()
                    .map(|line| RuntimeValue::Nu(Value::string(line, Span::unknown())));
                self.eval_for_values(targets, values, body)
            }
            value => {
                let values = self.eval_iterable_value(value)?;
                if let Some(flow) = self.try_eval_outer_jsonl_file_loop(targets, body, &values)? {
                    return Ok(flow);
                }
                if let Some(plan) = generic_loop_plan.as_ref() {
                    if let Some(flow) =
                        self.try_eval_for_values_generic_vm(targets, &values, plan)?
                    {
                        return Ok(flow);
                    }
                }
                self.eval_for_values(targets, values, body)
            }
        }
    }

    fn try_eval_outer_jsonl_file_loop(
        &mut self,
        targets: &[String],
        body: &[Stmt],
        values: &[RuntimeValue],
    ) -> Result<Option<EvalFlow>, ShellError> {
        if !self.state.hot_loop_enabled {
            return Ok(None);
        }
        let [file_target] = targets else {
            return Ok(None);
        };
        let Some(plan) = match_outer_jsonl_file_loop_body(file_target, body) else {
            return Ok(None);
        };
        for value in values {
            let path = match runtime_value_record_string_field(value, "path")? {
                Some(path) => path,
                None => return Ok(None),
            };
            self.state.set_local(file_target.clone(), value.clone());
            let rows = self.read_jsonl_rows_from_path(&path, "read_jsonl")?;
            self.eval_for_jsonl_rows_generic_native_body(
                std::slice::from_ref(&plan.row_target),
                &rows,
                &GenericLoopPlan {
                    target: plan.row_target.clone(),
                    iter: GenericLoopIter::ReadJsonl,
                    ops: vec![GenericLoopOp::JsonlAggregation {
                        body: plan.body.clone(),
                    }],
                },
            )?;
        }
        Ok(Some(EvalFlow::Output(PipelineData::empty())))
    }

    fn try_eval_for_values_generic_vm(
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

    fn try_eval_for_text_lines_generic_vm(
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

    fn try_eval_for_jsonl_rows_generic_vm(
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

    fn record_hot_jsonl_ir_fused_selection(
        &mut self,
        optimization: &StoneLoopIrOptimizationResult,
    ) -> Result<(), ShellError> {
        self.state.hot_loop_diagnostics.loop_ir_lowered();
        self.state
            .hot_loop_diagnostics
            .loop_ir_optimized(&optimization.diagnostics);
        let Some(kernel) = optimization.selected_kernel else {
            self.state
                .hot_loop_diagnostics
                .fusion_miss("jsonl_ir_no_fused_kernel");
            return Err(stone_error(
                "hot loop",
                "unsupported JSONL native IR fused kernel",
            ));
        };
        self.state
            .hot_loop_diagnostics
            .fused_kernel_selected(kernel);
        Ok(())
    }

    fn execute_generic_vm_function(
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

    fn finish_generic_vm_loop(&mut self, targets: &[String], last_value: Option<RuntimeValue>) {
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
        let mut accumulator = match generic_vm_number_from_runtime(&local_value) {
            Some(value) => value,
            None => {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            }
        };
        let mut last_value = None;
        for line in &lines.lines {
            let parsed = match parse {
                GenericParseNumber::Int => match line.trim().parse::<i64>() {
                    Ok(value) => GenericVmNumber::I64(value),
                    Err(_) => {
                        self.state
                            .hot_loop_diagnostics
                            .lowering_miss("unsupported_expr");
                        return Ok(GenericVmLoopResult::Unsupported);
                    }
                },
                GenericParseNumber::Float => match line.trim().parse::<f64>() {
                    Ok(value) => GenericVmNumber::F64(value),
                    Err(_) => {
                        self.state
                            .hot_loop_diagnostics
                            .lowering_miss("unsupported_expr");
                        return Ok(GenericVmLoopResult::Unsupported);
                    }
                },
            };
            accumulator = generic_vm_add_number(accumulator, parsed)?;
            last_value = Some(RuntimeValue::Nu(Value::string(
                line.clone(),
                Span::unknown(),
            )));
        }
        self.state.set_local(
            local.to_owned(),
            RuntimeValue::Nu(generic_vm_number_to_value(accumulator)),
        );
        Ok(GenericVmLoopResult::Executed { last_value })
    }

    fn execute_generic_vm_map_add_i64_const(
        &mut self,
        map: &str,
        addend: i64,
        values: &[RuntimeValue],
    ) -> Result<GenericVmLoopResult, ShellError> {
        let mut counts = self.load_i64_record_map(map)?;
        let mut last_value = None;
        for value in values {
            let RuntimeValue::Nu(value) = value else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            let Ok(key) = value_to_string(value, "hot loop") else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            let total = counts.entry(key).or_insert(0);
            *total = total
                .checked_add(addend)
                .ok_or_else(|| stone_error("hot loop", "integer addition overflow"))?;
            last_value = Some(RuntimeValue::Nu(value.clone()));
        }
        self.state.set_local(
            map.to_owned(),
            RuntimeValue::Nu(Value::record(
                i64_record_from_native_map(&[], &counts),
                Span::unknown(),
            )),
        );
        Ok(GenericVmLoopResult::Executed { last_value })
    }

    fn execute_generic_vm_map_add_i64_const_record_field(
        &mut self,
        map: &str,
        field: &str,
        addend: i64,
        values: &[RuntimeValue],
    ) -> Result<GenericVmLoopResult, ShellError> {
        let mut counts = self.load_i64_record_map(map)?;
        let mut last_value = None;
        for value in values {
            let Some(field_value) = generic_vm_record_field_value(value, field)? else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            let Ok(key) = runtime_value_to_string_key(&field_value, "hot loop") else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            let total = counts.entry(key).or_insert(0);
            *total = total
                .checked_add(addend)
                .ok_or_else(|| stone_error("hot loop", "integer addition overflow"))?;
            last_value = Some(value.clone());
        }
        self.state.set_local(
            map.to_owned(),
            RuntimeValue::Nu(Value::record(
                i64_record_from_native_map(&[], &counts),
                Span::unknown(),
            )),
        );
        Ok(GenericVmLoopResult::Executed { last_value })
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
        let mut counts = self.load_i64_record_map(map)?;
        let mut last_value = None;
        for value in values {
            let Some(field_value) = generic_vm_record_field_value(value, field)? else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            let Ok(mut key) = runtime_value_to_string_key(&field_value, "hot loop") else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            if strip {
                key = key.trim().to_owned();
            }
            if lower {
                key = key.to_lowercase();
            }
            let total = counts.entry(key).or_insert(0);
            *total = total
                .checked_add(addend)
                .ok_or_else(|| stone_error("hot loop", "integer addition overflow"))?;
            last_value = Some(value.clone());
        }
        self.state.set_local(
            map.to_owned(),
            RuntimeValue::Nu(Value::record(
                i64_record_from_native_map(&[], &counts),
                Span::unknown(),
            )),
        );
        Ok(GenericVmLoopResult::Executed { last_value })
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
        let RuntimeValue::Nu(Value::List { vals, .. }) = current else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss("unsupported_expr");
            return Ok(GenericVmLoopResult::Unsupported);
        };
        let mut items = vals.to_vec();
        let mut last_value = None;
        for value in values {
            let RuntimeValue::Nu(value) = value else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            if unique && items.iter().any(|item| values_equal(item, value)) {
                last_value = Some(RuntimeValue::Nu(value.clone()));
                continue;
            }
            items.push(value.clone());
            last_value = Some(RuntimeValue::Nu(value.clone()));
        }
        self.state.set_local(
            list.to_owned(),
            RuntimeValue::Nu(Value::list(items, Span::unknown())),
        );
        Ok(GenericVmLoopResult::Executed { last_value })
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

    fn eval_for_jsonl_rows(
        &mut self,
        targets: &[String],
        rows: JsonlRows,
        body: &[Stmt],
    ) -> Result<EvalFlow, ShellError> {
        let mut output = PipelineData::empty();
        for line in rows.lines.iter().cloned() {
            let value = jsonl_row_view(&rows, line);
            self.assign_loop_targets(targets, value)?;
            let started = self.state.profiler.start();
            let flow = self.eval_block(body, PipelineData::empty(), false);
            self.state
                .profiler
                .finish(EvalProfileBucket::ForJsonlBody, started);
            match flow? {
                EvalFlow::Output(value) => output = value,
                EvalFlow::Break => break,
                EvalFlow::Continue => continue,
                flow @ EvalFlow::Return(_) => return Ok(flow),
            }
        }
        Ok(EvalFlow::Output(output))
    }

    fn eval_for_jsonl_rows_hot_prefix(
        &mut self,
        targets: &[String],
        rows: JsonlRows,
        body: &[Stmt],
        plan: &HotLoopPlan,
    ) -> Result<EvalFlow, ShellError> {
        let mut output = PipelineData::empty();
        let remaining_body = body.get(plan.body_start..).unwrap_or(&[]);
        let hot_body_plan = match_hot_jsonl_aggregation_body(&plan.target, remaining_body)
            .or_else(|| match_hot_jsonl_aggregation_body(&plan.target, body));
        if let Some(body_plan) = hot_body_plan.as_ref() {
            self.eval_for_jsonl_rows_hot_native_body(targets, &rows, plan, body_plan)?;
            return Ok(EvalFlow::Output(output));
        }
        for line in rows.lines.iter().cloned() {
            let value = jsonl_row_view(&rows, line);
            self.assign_loop_targets(targets, value.clone())?;
            let RuntimeValue::JsonObjectView(view) = value else {
                return Err(stone_error("hot loop", "expected JSON object row view"));
            };
            self.execute_hot_loop_prefix(plan, &view)?;
            let started = self.state.profiler.start();
            let flow = self.eval_block(remaining_body, PipelineData::empty(), false);
            self.state
                .profiler
                .finish(EvalProfileBucket::ForJsonlBody, started);
            match flow? {
                EvalFlow::Output(value) => output = value,
                EvalFlow::Break => break,
                EvalFlow::Continue => continue,
                flow @ EvalFlow::Return(_) => return Ok(flow),
            }
        }
        Ok(EvalFlow::Output(output))
    }

    fn eval_for_values<I>(
        &mut self,
        targets: &[String],
        values: I,
        body: &[Stmt],
    ) -> Result<EvalFlow, ShellError>
    where
        I: IntoIterator<Item = RuntimeValue>,
    {
        let mut output = PipelineData::empty();
        for value in values {
            self.assign_loop_targets(targets, value)?;
            let started = self.state.profiler.start();
            let flow = self.eval_block(body, PipelineData::empty(), false);
            self.state
                .profiler
                .finish(EvalProfileBucket::ForValuesBody, started);
            match flow? {
                EvalFlow::Output(value) => output = value,
                EvalFlow::Break => break,
                EvalFlow::Continue => continue,
                flow @ EvalFlow::Return(_) => return Ok(flow),
            }
        }
        Ok(EvalFlow::Output(output))
    }

    fn execute_hot_loop_prefix(
        &mut self,
        plan: &HotLoopPlan,
        row: &JsonObjectView,
    ) -> Result<(), ShellError> {
        for op in &plan.ops {
            match op {
                HotLoopOp::GenericFallback => {
                    return Err(stone_error(
                        "hot loop",
                        "generic fallback op cannot execute",
                    ));
                }
                HotLoopOp::JsonGetStrDefault {
                    target,
                    key,
                    default,
                } => {
                    let value = json_object_view_get_string_default(row, key, default)?;
                    self.state.set_local(
                        target.clone(),
                        RuntimeValue::Nu(Value::string(value, Span::unknown())),
                    );
                }
                HotLoopOp::JsonGetF64Default {
                    target,
                    key,
                    default,
                } => {
                    let value = json_object_view_get_f64_default(row, key, *default)?;
                    self.state.set_local(
                        target.clone(),
                        RuntimeValue::Nu(Value::float(value, Span::unknown())),
                    );
                }
                HotLoopOp::JsonGetI64Default {
                    target,
                    key,
                    default,
                } => {
                    let value = json_object_view_get_i64_default(row, key, *default)?;
                    self.state.set_local(
                        target.clone(),
                        RuntimeValue::Nu(Value::int(value, Span::unknown())),
                    );
                }
                HotLoopOp::JsonGetArrayDefault { target, key } => {
                    let value = json_object_view_get_array_default(row, key)?;
                    self.state.set_local(target.clone(), value);
                }
                HotLoopOp::JsonGetValue { target, key } => {
                    let value = json_object_view_get(row, key)?.ok_or_else(|| {
                        stone_error("hot loop", format!("record has no key `{key}`"))
                    })?;
                    self.state.set_local(target.clone(), value);
                }
            }
        }
        Ok(())
    }

    fn eval_for_jsonl_rows_hot_native_body(
        &mut self,
        targets: &[String],
        rows: &JsonlRows,
        prefix_plan: &HotLoopPlan,
        body_plan: &HotJsonlAggregationBody,
    ) -> Result<(), ShellError> {
        if !validate_hot_jsonl_native_prefix(prefix_plan, body_plan) {
            return Err(stone_error(
                "hot loop",
                "native JSONL aggregation prefix does not match body",
            ));
        }
        let vm_function = compile_hot_jsonl_vm_function(body_plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL native VM body ops"))?;
        let optimization = optimize_stone_loop_ir(&vm_function);
        let vm_function = &optimization.function;
        self.validate_hot_jsonl_vm_guards(vm_function)?;
        self.record_hot_jsonl_ir_fused_selection(&optimization)?;
        let trace_plan = compile_hot_jsonl_trace_plan_from_ir(vm_function)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL native loop IR"))?;
        let mut accumulators = self.load_hot_jsonl_native_accumulators(&trace_plan)?;
        let mut last_row = None;
        let execution_started = Instant::now();
        if self.state.profiler.enabled {
            for line in rows.lines.iter().cloned() {
                let row = HotJsonlRowSlice {
                    bytes: &rows.bytes[line.range.clone()],
                    source: rows.source.as_ref(),
                    line_number: line.line_number,
                };
                let started = self.state.profiler.start();
                if self.state.hot_loop_vm_interpreter {
                    self.execute_hot_jsonl_vm_function_or_fallback(
                        vm_function,
                        &row,
                        &mut accumulators,
                    )?;
                } else {
                    self.execute_hot_jsonl_aggregation_native_body(
                        &trace_plan,
                        &row,
                        &mut accumulators,
                    )?;
                }
                self.state
                    .profiler
                    .finish(EvalProfileBucket::ForJsonlBody, started);
                last_row = Some(line);
            }
        } else {
            for line in rows.lines.iter().cloned() {
                let row = HotJsonlRowSlice {
                    bytes: &rows.bytes[line.range.clone()],
                    source: rows.source.as_ref(),
                    line_number: line.line_number,
                };
                if self.state.hot_loop_vm_interpreter {
                    self.execute_hot_jsonl_vm_function_or_fallback(
                        vm_function,
                        &row,
                        &mut accumulators,
                    )?;
                } else {
                    self.execute_hot_jsonl_aggregation_native_body(
                        &trace_plan,
                        &row,
                        &mut accumulators,
                    )?;
                }
                last_row = Some(line);
            }
        }
        if let Some(line) = last_row {
            let row = JsonObjectView {
                bytes: rows.bytes.clone(),
                range: line.range,
                source: rows.source.clone(),
                line_number: line.line_number,
            };
            if let Some(target) = targets.first() {
                self.state
                    .set_local(target.clone(), RuntimeValue::JsonObjectView(row.clone()));
            }
            let snapshot = self.materialize_hot_jsonl_vm_snapshot_row_locals(
                vm_function,
                SnapshotId(0),
                &row,
            )?;
            if self.state.hot_loop_validate_snapshot {
                self.validate_stone_materialized_snapshot(&snapshot)?;
            }
            self.apply_stone_materialized_snapshot(snapshot);
        }
        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        let snapshot = self.materialize_hot_jsonl_vm_snapshot_accumulators(
            vm_function,
            SnapshotId(0),
            accumulators,
        )?;
        if self.state.hot_loop_validate_snapshot {
            self.validate_stone_materialized_snapshot(&snapshot)?;
        }
        self.apply_stone_materialized_snapshot(snapshot);
        self.apply_hot_jsonl_row_count(body_plan, rows.lines.len())?;
        Ok(())
    }

    fn eval_for_jsonl_rows_generic_native_body(
        &mut self,
        targets: &[String],
        rows: &JsonlRows,
        plan: &GenericLoopPlan,
    ) -> Result<(), ShellError> {
        let [GenericLoopOp::JsonlAggregation { body: body_plan }] = plan.ops.as_slice() else {
            return Err(stone_error(
                "hot loop",
                "expected JSONL aggregation loop IR",
            ));
        };
        if body_plan.nested_user_totals.is_some() {
            return self.eval_for_jsonl_rows_nested_totals_native_body(targets, rows, body_plan);
        }
        let vm_function = compile_hot_jsonl_loop_ir_function(plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL native loop IR"))?;
        let optimization = optimize_stone_loop_ir(&vm_function);
        let vm_function = &optimization.function;
        self.validate_hot_jsonl_vm_guards(vm_function)?;
        self.record_hot_jsonl_ir_fused_selection(&optimization)?;
        let trace_plan = compile_hot_jsonl_trace_plan_from_ir(vm_function)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL native loop IR"))?;
        let mut accumulators = self.load_hot_jsonl_native_accumulators(&trace_plan)?;
        let mut last_row = None;
        let execution_started = Instant::now();
        if self.state.profiler.enabled {
            for line in rows.lines.iter().cloned() {
                let row = HotJsonlRowSlice {
                    bytes: &rows.bytes[line.range.clone()],
                    source: rows.source.as_ref(),
                    line_number: line.line_number,
                };
                let started = self.state.profiler.start();
                if self.state.hot_loop_vm_interpreter {
                    self.execute_hot_jsonl_vm_function_or_fallback(
                        vm_function,
                        &row,
                        &mut accumulators,
                    )?;
                } else {
                    self.execute_hot_jsonl_aggregation_native_body(
                        &trace_plan,
                        &row,
                        &mut accumulators,
                    )?;
                }
                self.state
                    .profiler
                    .finish(EvalProfileBucket::ForJsonlBody, started);
                last_row = Some(line);
            }
        } else {
            for line in rows.lines.iter().cloned() {
                let row = HotJsonlRowSlice {
                    bytes: &rows.bytes[line.range.clone()],
                    source: rows.source.as_ref(),
                    line_number: line.line_number,
                };
                if self.state.hot_loop_vm_interpreter {
                    self.execute_hot_jsonl_vm_function_or_fallback(
                        vm_function,
                        &row,
                        &mut accumulators,
                    )?;
                } else {
                    self.execute_hot_jsonl_aggregation_native_body(
                        &trace_plan,
                        &row,
                        &mut accumulators,
                    )?;
                }
                last_row = Some(line);
            }
        }
        if let Some(line) = last_row {
            let row = JsonObjectView {
                bytes: rows.bytes.clone(),
                range: line.range,
                source: rows.source.clone(),
                line_number: line.line_number,
            };
            if let Some(target) = targets.first() {
                self.state
                    .set_local(target.clone(), RuntimeValue::JsonObjectView(row.clone()));
            }
            let snapshot = self.materialize_hot_jsonl_vm_snapshot_row_locals(
                vm_function,
                SnapshotId(0),
                &row,
            )?;
            if self.state.hot_loop_validate_snapshot {
                self.validate_stone_materialized_snapshot(&snapshot)?;
            }
            self.apply_stone_materialized_snapshot(snapshot);
        }
        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        let snapshot = self.materialize_hot_jsonl_vm_snapshot_accumulators(
            vm_function,
            SnapshotId(0),
            accumulators,
        )?;
        if self.state.hot_loop_validate_snapshot {
            self.validate_stone_materialized_snapshot(&snapshot)?;
        }
        self.apply_stone_materialized_snapshot(snapshot);
        self.apply_hot_jsonl_row_count(body_plan, rows.lines.len())?;
        Ok(())
    }

    fn eval_for_jsonl_rows_nested_totals_native_body(
        &mut self,
        targets: &[String],
        rows: &JsonlRows,
        body_plan: &HotJsonlAggregationBody,
    ) -> Result<(), ShellError> {
        let nested = body_plan
            .nested_user_totals
            .as_ref()
            .ok_or_else(|| stone_error("hot loop", "missing nested totals plan"))?;
        let trace_plan = hot_jsonl_trace_plan_from_body_plan(body_plan);
        self.state.hot_loop_diagnostics.loop_ir_lowered();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_selected(LoopIrFusedKernel::JsonlAggregation);
        let mut accumulators =
            self.load_nested_hot_jsonl_native_accumulators(&trace_plan, nested)?;
        let mut last_row = None;
        let execution_started = Instant::now();
        for line in rows.lines.iter().cloned() {
            let row = HotJsonlRowSlice {
                bytes: &rows.bytes[line.range.clone()],
                source: rows.source.as_ref(),
                line_number: line.line_number,
            };
            let fields = json_object_view_get_hot_jsonl_row_fields(
                &row,
                &trace_plan,
                &body_plan.user_key,
                &body_plan.user_amount_key,
                &body_plan.user_items_key,
                &body_plan.tags_key,
            )?;
            let user_key = fields.user.into_owned();
            match accumulators.user_amounts.get_mut(&user_key) {
                Some(total) => *total += fields.amount,
                None => {
                    accumulators
                        .user_amounts
                        .insert(user_key.clone(), fields.amount);
                    accumulators.users.push(user_key.clone());
                }
            }
            *accumulators.user_items.entry(user_key).or_insert(0) += fields.items;
            hot_jsonl_string_array_for_each_string(&fields.tags, |tag| {
                let tag_key = tag.as_ref();
                if let Some(count) = accumulators.tag_counts.get_mut(tag_key) {
                    *count += 1;
                } else {
                    accumulators.tag_counts.insert(tag_key.to_owned(), 1);
                    accumulators.tags.push(tag_key.to_owned());
                }
                Ok(())
            })?;
            last_row = Some(line);
        }

        if let Some(line) = last_row {
            let row = JsonObjectView {
                bytes: rows.bytes.clone(),
                range: line.range,
                source: rows.source.clone(),
                line_number: line.line_number,
            };
            if let Some(target) = targets.first() {
                self.state
                    .set_local(target.clone(), RuntimeValue::JsonObjectView(row.clone()));
            }
            let row = HotJsonlRowSlice {
                bytes: &row.bytes[row.range.clone()],
                source: row.source.as_ref(),
                line_number: row.line_number,
            };
            let user = if body_plan.user_has_default {
                hot_jsonl_row_get_string_default(
                    &row,
                    &body_plan.user_key,
                    &body_plan.user_default,
                )?
            } else {
                hot_jsonl_row_get_string_required(&row, &body_plan.user_key)?
            };
            self.state.set_local(
                body_plan.user_name.clone(),
                RuntimeValue::Nu(Value::string(user.into_owned(), Span::unknown())),
            );
        }

        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        self.state.set_local(
            nested.map_name.clone(),
            RuntimeValue::Nu(Value::record(
                nested_totals_record_from_native_maps(
                    &accumulators.users,
                    &accumulators.user_amounts,
                    &accumulators.user_items,
                    &nested.amount_field,
                    &nested.items_field,
                ),
                Span::unknown(),
            )),
        );
        self.state.set_local(
            body_plan.tag_counts_map.clone(),
            RuntimeValue::Nu(Value::record(
                i64_record_from_native_map(&accumulators.tags, &accumulators.tag_counts),
                Span::unknown(),
            )),
        );
        self.apply_hot_jsonl_row_count(body_plan, rows.lines.len())?;
        Ok(())
    }

    fn apply_hot_jsonl_row_count(
        &mut self,
        body_plan: &HotJsonlAggregationBody,
        row_count: usize,
    ) -> Result<(), ShellError> {
        let Some(local) = body_plan.row_count_local.as_deref() else {
            return Ok(());
        };
        let addend = i64::try_from(row_count)
            .map_err(|_| stone_error("hot loop", "row count is too large"))?;
        let value = self
            .state
            .get_local_mut(local)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{local}`")))?;
        let RuntimeValue::Nu(value) = value else {
            return Err(stone_error(
                "hot loop",
                format!("{local} is not an integer counter"),
            ));
        };
        let current = value_to_i64(value, "hot loop")?;
        *value = Value::int(
            current
                .checked_add(addend)
                .ok_or_else(|| stone_error("hot loop", "row count overflow"))?,
            Span::unknown(),
        );
        Ok(())
    }

    fn eval_for_text_lines_jsonl_generic_native_body(
        &mut self,
        targets: &[String],
        lines: &TextLines,
        plan: &GenericLoopPlan,
    ) -> Result<(), ShellError> {
        let [GenericLoopOp::JsonlAggregation { .. }] = plan.ops.as_slice() else {
            return Err(stone_error("hot loop", "expected JSONL text-lines loop IR"));
        };
        let vm_function = compile_hot_jsonl_loop_ir_function(plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL text-lines loop IR"))?;
        let optimization = optimize_stone_loop_ir(&vm_function);
        let vm_function = &optimization.function;
        self.validate_hot_jsonl_vm_guards(vm_function)?;
        self.record_hot_jsonl_ir_fused_selection(&optimization)?;
        let trace_plan = compile_hot_jsonl_trace_plan_from_ir(vm_function)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL text-lines loop IR"))?;
        let mut accumulators = self.load_hot_jsonl_native_accumulators(&trace_plan)?;
        let mut last_line = None;
        let execution_started = Instant::now();
        for (index, line) in lines.lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row = HotJsonlRowSlice {
                bytes: line.as_bytes(),
                source: lines.source.as_str(),
                line_number: index + 1,
            };
            let started = self.state.profiler.start();
            if self.state.hot_loop_vm_interpreter {
                self.execute_hot_jsonl_vm_function_or_fallback(
                    vm_function,
                    &row,
                    &mut accumulators,
                )?;
            } else {
                self.execute_hot_jsonl_aggregation_native_body(
                    &trace_plan,
                    &row,
                    &mut accumulators,
                )?;
            }
            self.state
                .profiler
                .finish(EvalProfileBucket::ForJsonlBody, started);
            last_line = Some(line.clone());
        }
        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        if let Some(line) = last_line {
            if let Some(target) = targets.first() {
                self.state.set_local(
                    target.clone(),
                    RuntimeValue::Nu(Value::string(line, Span::unknown())),
                );
            }
        }
        let snapshot = self.materialize_hot_jsonl_vm_snapshot_accumulators(
            vm_function,
            SnapshotId(0),
            accumulators,
        )?;
        if self.state.hot_loop_validate_snapshot {
            self.validate_stone_materialized_snapshot(&snapshot)?;
        }
        self.apply_stone_materialized_snapshot(snapshot);
        Ok(())
    }

    fn eval_for_text_lines_hot_jsonl(
        &mut self,
        targets: &[String],
        lines: TextLines,
        body: &[Stmt],
        plan: &HotLoopPlan,
    ) -> Result<EvalFlow, ShellError> {
        let remaining_body = body.get(plan.body_start..).unwrap_or(&[]);
        let Some(body_plan) = match_hot_jsonl_aggregation_body(&plan.target, remaining_body) else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss("unsupported_body_stmt");
            let values = lines
                .lines
                .into_iter()
                .map(|line| RuntimeValue::Nu(Value::string(line, Span::unknown())));
            return self.eval_for_values(targets, values, body);
        };
        let vm_function = compile_hot_jsonl_vm_function(&body_plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL text-lines VM body ops"))?;
        let optimization = optimize_stone_loop_ir(&vm_function);
        let vm_function = &optimization.function;
        self.validate_hot_jsonl_vm_guards(vm_function)?;
        self.record_hot_jsonl_ir_fused_selection(&optimization)?;
        let trace_plan = compile_hot_jsonl_trace_plan(&body_plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL text-lines body ops"))?;
        let mut accumulators = self.load_hot_jsonl_native_accumulators(&trace_plan)?;
        let mut last_line = None;
        let execution_started = Instant::now();
        let line_target = match &plan.iter {
            HotLoopIter::JsonlTextLines { line_target } => line_target,
            _ => return Err(stone_error("hot loop", "unsupported_iterator")),
        };
        for (index, line) in lines.lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row = HotJsonlRowSlice {
                bytes: line.as_bytes(),
                source: lines.source.as_str(),
                line_number: index + 1,
            };
            let started = self.state.profiler.start();
            if self.state.hot_loop_vm_interpreter {
                self.execute_hot_jsonl_vm_function_or_fallback(
                    vm_function,
                    &row,
                    &mut accumulators,
                )?;
            } else {
                self.execute_hot_jsonl_aggregation_native_body(
                    &trace_plan,
                    &row,
                    &mut accumulators,
                )?;
            }
            self.state
                .profiler
                .finish(EvalProfileBucket::ForJsonlBody, started);
            last_line = Some(line.clone());
        }
        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        if let Some(line) = last_line {
            if let Some(target) = targets.first() {
                self.state.set_local(
                    target.clone(),
                    RuntimeValue::Nu(Value::string(line.clone(), Span::unknown())),
                );
            }
            self.state.set_local(
                line_target.clone(),
                RuntimeValue::Nu(Value::string(line, Span::unknown())),
            );
        }
        let snapshot = self.materialize_hot_jsonl_vm_snapshot_accumulators(
            vm_function,
            SnapshotId(0),
            accumulators,
        )?;
        if self.state.hot_loop_validate_snapshot {
            self.validate_stone_materialized_snapshot(&snapshot)?;
        }
        self.apply_stone_materialized_snapshot(snapshot);
        Ok(EvalFlow::Output(PipelineData::empty()))
    }

    fn execute_hot_jsonl_aggregation_native_body(
        &mut self,
        plan: &HotJsonlTracePlan,
        row: &HotJsonlRowSlice<'_>,
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        self.execute_hot_jsonl_native_trace_body(plan, row, accumulators)
    }

    fn validate_hot_jsonl_vm_guards(&self, function: &StoneIrFunction) -> Result<(), ShellError> {
        for guard in &function.guards {
            if function.snapshots.get(guard.snapshot.0 as usize).is_none() {
                return Err(stone_error("hot loop", "VM guard snapshot is out of range"));
            }
            match guard.kind {
                StoneGuardKind::InputIsJsonObject { reg } => {
                    if reg != Reg(0) {
                        return Err(stone_error("hot loop", "unsupported VM input guard"));
                    }
                }
                StoneGuardKind::AccumulatorShape { acc, kind } => {
                    let spec = function.accumulators.get(acc.0 as usize).ok_or_else(|| {
                        stone_error("hot loop", "VM accumulator guard is out of range")
                    })?;
                    if spec.kind != kind {
                        return Err(stone_error(
                            "hot loop",
                            "VM accumulator guard does not match accumulator shape",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn materialize_hot_jsonl_vm_snapshot_accumulators(
        &self,
        function: &StoneIrFunction,
        snapshot_id: SnapshotId,
        accumulators: HotJsonlNativeAccumulators,
    ) -> Result<StoneMaterializedSnapshot, ShellError> {
        let snapshot = function
            .snapshots
            .get(snapshot_id.0 as usize)
            .ok_or_else(|| stone_error("hot loop", "VM snapshot is out of range"))?;
        if snapshot.resume != StoneFallbackTarget::LoopBody {
            return Err(stone_error("hot loop", "unsupported VM snapshot target"));
        }

        let mut locals = Vec::new();
        for accumulator in &snapshot.accumulators {
            let spec = function
                .accumulators
                .get(accumulator.acc.0 as usize)
                .ok_or_else(|| {
                    stone_error("hot loop", "VM snapshot accumulator is out of range")
                })?;
            match (accumulator.acc, spec.kind) {
                (AccId(0), StoneAccumulatorKind::F64Map) => {
                    locals.push((
                        accumulator.local_name.clone(),
                        RuntimeValue::Nu(Value::record(
                            f64_record_from_native_map(
                                &accumulators.users,
                                &accumulators.user_amounts,
                            ),
                            Span::unknown(),
                        )),
                    ));
                }
                (AccId(1), StoneAccumulatorKind::I64Map) => {
                    locals.push((
                        accumulator.local_name.clone(),
                        RuntimeValue::Nu(Value::record(
                            i64_record_from_native_map(
                                &accumulators.users,
                                &accumulators.user_items,
                            ),
                            Span::unknown(),
                        )),
                    ));
                }
                (AccId(2), StoneAccumulatorKind::StringList) => {
                    if !accumulator.local_name.is_empty() {
                        locals.push((
                            accumulator.local_name.clone(),
                            RuntimeValue::Nu(string_list_from_ordered_keys(&accumulators.users)),
                        ));
                    }
                }
                (AccId(3), StoneAccumulatorKind::I64Map) => {
                    locals.push((
                        accumulator.local_name.clone(),
                        RuntimeValue::Nu(Value::record(
                            i64_record_from_native_map(
                                &accumulators.tags,
                                &accumulators.tag_counts,
                            ),
                            Span::unknown(),
                        )),
                    ));
                }
                (AccId(4), StoneAccumulatorKind::StringList) => {
                    if !accumulator.local_name.is_empty() {
                        locals.push((
                            accumulator.local_name.clone(),
                            RuntimeValue::Nu(string_list_from_ordered_keys(&accumulators.tags)),
                        ));
                    }
                }
                _ => {
                    return Err(stone_error(
                        "hot loop",
                        "unsupported VM snapshot accumulator materialization",
                    ));
                }
            }
        }

        Ok(StoneMaterializedSnapshot { locals })
    }

    fn materialize_hot_jsonl_vm_snapshot_row_locals(
        &self,
        function: &StoneIrFunction,
        snapshot_id: SnapshotId,
        row: &JsonObjectView,
    ) -> Result<StoneMaterializedSnapshot, ShellError> {
        let snapshot = function
            .snapshots
            .get(snapshot_id.0 as usize)
            .ok_or_else(|| stone_error("hot loop", "VM snapshot is out of range"))?;
        let row = HotJsonlRowSlice {
            bytes: &row.bytes[row.range.clone()],
            source: row.source.as_ref(),
            line_number: row.line_number,
        };
        let mut locals = Vec::new();
        for local in &snapshot.locals {
            let local_name = function
                .locals
                .get(local.local.0 as usize)
                .ok_or_else(|| stone_error("hot loop", "VM snapshot local is out of range"))?
                .name
                .clone();
            match local.reg {
                Reg(1) => {
                    let value = self.materialize_hot_jsonl_vm_user_reg(function, &row)?;
                    locals.push((
                        local_name,
                        RuntimeValue::Nu(Value::string(value.into_owned(), Span::unknown())),
                    ));
                }
                _ => {
                    return Err(stone_error(
                        "hot loop",
                        "unsupported VM snapshot local register",
                    ));
                }
            }
        }
        Ok(StoneMaterializedSnapshot { locals })
    }

    fn apply_stone_materialized_snapshot(&mut self, snapshot: StoneMaterializedSnapshot) {
        for (name, value) in snapshot.locals {
            self.state.set_local(name, value);
        }
    }

    fn validate_stone_materialized_snapshot(
        &self,
        snapshot: &StoneMaterializedSnapshot,
    ) -> Result<(), ShellError> {
        for (name, value) in &snapshot.locals {
            if name.is_empty() {
                return Err(stone_error(
                    "hot loop",
                    "materialized VM snapshot contains an empty local name",
                ));
            }
            if !matches!(value, RuntimeValue::Nu(_) | RuntimeValue::JsonObjectView(_)) {
                return Err(stone_error(
                    "hot loop",
                    "materialized VM snapshot contains a non-materialized value",
                ));
            }
        }
        Ok(())
    }

    fn materialize_hot_jsonl_vm_user_reg<'a>(
        &self,
        function: &StoneIrFunction,
        row: &HotJsonlRowSlice<'a>,
    ) -> Result<Cow<'a, str>, ShellError> {
        let row_block = function
            .blocks
            .get(function.entry.0 as usize)
            .ok_or_else(|| stone_error("hot loop", "VM entry block is out of range"))?;
        let Some(user_op) = row_block.ops.first() else {
            return Err(stone_error("hot loop", "VM row block has no user op"));
        };
        match user_op {
            StoneOp::JsonGetStrDefault {
                dst: Reg(1),
                object: Reg(0),
                key,
                default,
            } => {
                let key = stone_const_string(function, *key)?;
                let default = stone_const_string(function, *default)?;
                hot_jsonl_row_get_string_default(row, key, default)
            }
            StoneOp::JsonGetValue {
                dst: Reg(1),
                object: Reg(0),
                key,
            } => {
                let key = stone_const_string(function, *key)?;
                hot_jsonl_row_get_string_required(row, key)
            }
            _ => Err(stone_error("hot loop", "unsupported VM user snapshot op")),
        }
    }

    fn execute_hot_jsonl_vm_function_or_fallback<'a>(
        &mut self,
        function: &StoneIrFunction,
        row: &'a HotJsonlRowSlice<'a>,
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        match self.execute_hot_jsonl_vm_function(function, row, accumulators)? {
            StoneVmExecutionResult::Completed => Ok(()),
            StoneVmExecutionResult::Fallback { snapshot } => {
                self.state.hot_loop_diagnostics.loop_fallback();
                Err(stone_error(
                    "hot loop",
                    format!(
                        "VM requested fallback to snapshot {}, but AST resume is not implemented",
                        snapshot.0
                    ),
                ))
            }
        }
    }

    fn execute_hot_jsonl_vm_function<'a>(
        &self,
        function: &StoneIrFunction,
        row: &'a HotJsonlRowSlice<'a>,
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<StoneVmExecutionResult, ShellError> {
        self.validate_hot_jsonl_vm_accumulators(function)?;
        let mut slots = vec![StoneVmSlot::Empty; function.registers as usize];
        self.set_stone_vm_slot(&mut slots, Reg(0), StoneVmSlot::Row(row))?;
        let mut block = function.entry;
        loop {
            let block_ref = function
                .blocks
                .get(block.0 as usize)
                .ok_or_else(|| stone_error("hot loop", "VM block is out of range"))?;
            if let Some(snapshot) =
                self.execute_hot_jsonl_vm_ops(function, &block_ref.ops, &mut slots, accumulators)?
            {
                return Ok(StoneVmExecutionResult::Fallback { snapshot });
            }
            match &block_ref.terminator {
                StoneTerminator::JsonEachStrArray {
                    array,
                    item,
                    body,
                    done,
                } => {
                    let tags = self.stone_vm_string_array(&slots, *array)?;
                    let mut fallback = None;
                    hot_jsonl_string_array_for_each_string(&tags, |tag| {
                        if fallback.is_some() {
                            return Ok(());
                        }
                        self.set_stone_vm_slot(
                            &mut slots,
                            *item,
                            StoneVmSlot::String(tag.into_owned()),
                        )?;
                        let body_ref = function.blocks.get(body.0 as usize).ok_or_else(|| {
                            stone_error("hot loop", "VM tag block is out of range")
                        })?;
                        if let Some(snapshot) = self.execute_hot_jsonl_vm_ops(
                            function,
                            &body_ref.ops,
                            &mut slots,
                            accumulators,
                        )? {
                            fallback = Some(snapshot);
                            return Ok(());
                        }
                        match body_ref.terminator {
                            StoneTerminator::Jump { target } if target == block => Ok(()),
                            _ => {
                                fallback = Some(SnapshotId(0));
                                Ok(())
                            }
                        }
                    })?;
                    if let Some(snapshot) = fallback {
                        return Ok(StoneVmExecutionResult::Fallback { snapshot });
                    }
                    block = *done;
                }
                StoneTerminator::Jump { target } => {
                    block = *target;
                }
                StoneTerminator::Return => return Ok(StoneVmExecutionResult::Completed),
            }
        }
    }

    fn execute_hot_jsonl_vm_ops<'a>(
        &self,
        function: &StoneIrFunction,
        ops: &[StoneOp],
        slots: &mut [StoneVmSlot<'a>],
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<Option<SnapshotId>, ShellError> {
        for op in ops {
            match op {
                StoneOp::JsonGetStrDefault {
                    dst,
                    object,
                    key,
                    default,
                } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let default = stone_const_string(function, *default)?;
                    let value = hot_jsonl_row_get_string_default(row, key, default)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::String(value.into_owned()))?;
                }
                StoneOp::JsonGetValue { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_string_required(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::String(value.into_owned()))?;
                }
                StoneOp::JsonGetF64Default {
                    dst,
                    object,
                    key,
                    default,
                } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_f64_default(row, key, *default)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::F64(value))?;
                }
                StoneOp::JsonGetF64Required { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_f64_required(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::F64(value))?;
                }
                StoneOp::JsonGetI64Default {
                    dst,
                    object,
                    key,
                    default,
                } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_i64_default(row, key, *default)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::I64(value))?;
                }
                StoneOp::JsonGetI64Required { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_i64_required(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::I64(value))?;
                }
                StoneOp::JsonGetArrayDefault { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_array_default(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::StringArray(value))?;
                }
                StoneOp::JsonGetArrayRequired { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_array_required(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::StringArray(value))?;
                }
                StoneOp::MapAddF64 {
                    map,
                    key,
                    value,
                    append,
                } => {
                    self.execute_hot_jsonl_vm_map_add_f64(
                        *map,
                        *key,
                        *value,
                        *append,
                        slots,
                        accumulators,
                    )?;
                }
                StoneOp::MapAddI64 {
                    map,
                    key,
                    value,
                    append,
                } => {
                    if append.is_some() {
                        return Err(stone_error("hot loop", "unsupported i64 append map op"));
                    }
                    self.execute_hot_jsonl_vm_map_add_i64(*map, *key, *value, slots, accumulators)?;
                }
                StoneOp::MapAddI64Const {
                    map,
                    key,
                    value,
                    append,
                } => {
                    self.execute_hot_jsonl_vm_map_add_i64_const(
                        *map,
                        *key,
                        *value,
                        *append,
                        slots,
                        accumulators,
                    )?;
                }
            }
        }
        Ok(None)
    }

    fn validate_hot_jsonl_vm_accumulators(
        &self,
        function: &StoneIrFunction,
    ) -> Result<(), ShellError> {
        let [user_amounts, user_items, users, tag_counts, tags] = function.accumulators.as_slice()
        else {
            return Err(stone_error("hot loop", "unsupported VM accumulator layout"));
        };
        if user_amounts.kind != StoneAccumulatorKind::F64Map
            || user_items.kind != StoneAccumulatorKind::I64Map
            || users.kind != StoneAccumulatorKind::StringList
            || tag_counts.kind != StoneAccumulatorKind::I64Map
            || tags.kind != StoneAccumulatorKind::StringList
        {
            return Err(stone_error("hot loop", "unsupported VM accumulator kind"));
        }
        Ok(())
    }

    fn execute_hot_jsonl_vm_map_add_f64<'a>(
        &self,
        map: AccId,
        key: Reg,
        value: Reg,
        append: Option<AccId>,
        slots: &[StoneVmSlot<'a>],
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        if map != AccId(0) || !matches!(append, None | Some(AccId(2))) {
            return Err(stone_error("hot loop", "unsupported VM f64 map add"));
        }
        let value = self.stone_vm_f64(slots, value)?;
        let key = self.stone_vm_string(slots, key)?;
        if let Some(total) = accumulators.user_amounts.get_mut(key.as_str()) {
            *total += value;
        } else {
            accumulators.user_amounts.insert(key.clone(), value);
            if append.is_some() {
                accumulators.users.push(key);
            }
        }
        Ok(())
    }

    fn execute_hot_jsonl_vm_map_add_i64<'a>(
        &self,
        map: AccId,
        key: Reg,
        value: Reg,
        slots: &[StoneVmSlot<'a>],
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        if map != AccId(1) {
            return Err(stone_error("hot loop", "unsupported VM i64 map add"));
        }
        let value = self.stone_vm_i64(slots, value)?;
        let key = self.stone_vm_string(slots, key)?;
        if let Some(total) = accumulators.user_items.get_mut(key.as_str()) {
            *total += value;
        } else {
            accumulators.user_items.insert(key, value);
        }
        Ok(())
    }

    fn execute_hot_jsonl_vm_map_add_i64_const<'a>(
        &self,
        map: AccId,
        key: Reg,
        value: i64,
        append: Option<AccId>,
        slots: &[StoneVmSlot<'a>],
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        if map != AccId(3) || !matches!(append, None | Some(AccId(4))) {
            return Err(stone_error("hot loop", "unsupported VM i64 const map add"));
        }
        let key = self.stone_vm_string(slots, key)?;
        if let Some(total) = accumulators.tag_counts.get_mut(key.as_str()) {
            *total += value;
        } else {
            accumulators.tag_counts.insert(key.clone(), value);
            if append.is_some() {
                accumulators.tags.push(key);
            }
        }
        Ok(())
    }

    fn set_stone_vm_slot<'a>(
        &self,
        slots: &mut [StoneVmSlot<'a>],
        reg: Reg,
        value: StoneVmSlot<'a>,
    ) -> Result<(), ShellError> {
        let slot = slots
            .get_mut(reg.0 as usize)
            .ok_or_else(|| stone_error("hot loop", "VM register is out of range"))?;
        *slot = value;
        Ok(())
    }

    fn stone_vm_row<'a>(
        &self,
        slots: &[StoneVmSlot<'a>],
        reg: Reg,
    ) -> Result<&'a HotJsonlRowSlice<'a>, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::Row(row)) => Ok(*row),
            _ => Err(stone_error("hot loop", "VM register is not a row")),
        }
    }

    fn stone_vm_string(&self, slots: &[StoneVmSlot<'_>], reg: Reg) -> Result<String, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::String(value)) => Ok(value.clone()),
            _ => Err(stone_error("hot loop", "VM register is not a string")),
        }
    }

    fn stone_vm_f64(&self, slots: &[StoneVmSlot<'_>], reg: Reg) -> Result<f64, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::F64(value)) => Ok(*value),
            _ => Err(stone_error("hot loop", "VM register is not an f64")),
        }
    }

    fn stone_vm_i64(&self, slots: &[StoneVmSlot<'_>], reg: Reg) -> Result<i64, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::I64(value)) => Ok(*value),
            _ => Err(stone_error("hot loop", "VM register is not an i64")),
        }
    }

    fn stone_vm_string_array<'a>(
        &self,
        slots: &[StoneVmSlot<'a>],
        reg: Reg,
    ) -> Result<HotJsonlStringArray<'a>, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::StringArray(value)) => Ok(*value),
            _ => Err(stone_error("hot loop", "VM register is not a string array")),
        }
    }

    fn execute_hot_jsonl_native_trace_body(
        &mut self,
        plan: &HotJsonlTracePlan,
        row: &HotJsonlRowSlice<'_>,
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        let fields = json_object_view_get_hot_jsonl_row_fields(
            row,
            plan,
            &plan.user_key,
            &plan.user_amount_key,
            &plan.user_items_key,
            &plan.tags_key,
        )?;

        if let Some(total) = accumulators.user_amounts.get_mut(fields.user.as_ref()) {
            *total += fields.amount;
            *accumulators
                .user_items
                .get_mut(fields.user.as_ref())
                .ok_or_else(|| {
                    stone_error("hot loop", "user item accumulator is inconsistent")
                })? += fields.items;
        } else {
            let user_key = fields.user.into_owned();
            accumulators
                .user_amounts
                .insert(user_key.clone(), fields.amount);
            accumulators
                .user_items
                .insert(user_key.clone(), fields.items);
            accumulators.users.push(user_key);
        }

        hot_jsonl_string_array_for_each_string(&fields.tags, |tag_key| {
            if let Some(count) = accumulators.tag_counts.get_mut(tag_key.as_ref()) {
                *count += 1;
            } else {
                let tag_key = tag_key.into_owned();
                accumulators.tag_counts.insert(tag_key.clone(), 1);
                accumulators.tags.push(tag_key);
            }
            Ok(())
        })?;

        Ok(())
    }

    #[allow(dead_code)]
    fn execute_hot_jsonl_native_ops<'a>(
        &mut self,
        ops: &[HotJsonlBodyOp],
        plan: &HotJsonlAggregationBody,
        row: &'a JsonObjectView,
        accumulators: &mut HotJsonlNativeAccumulators,
        slots: &mut HotJsonlNativeSlots<'a>,
    ) -> Result<(), ShellError> {
        for op in ops {
            match op {
                HotJsonlBodyOp::JsonGetFields {
                    user_key,
                    amount_key,
                    items_key,
                    tags_key,
                } => {
                    let trace_plan = compile_hot_jsonl_trace_plan(plan).ok_or_else(|| {
                        stone_error("hot loop", "unsupported JSONL native body ops")
                    })?;
                    let row_slice = HotJsonlRowSlice {
                        bytes: &row.bytes[row.range.clone()],
                        source: row.source.as_ref(),
                        line_number: row.line_number,
                    };
                    slots.fields = Some(json_object_view_get_hot_jsonl_row_fields(
                        &row_slice,
                        &trace_plan,
                        user_key,
                        amount_key,
                        items_key,
                        tags_key,
                    )?);
                }
                HotJsonlBodyOp::MapAddF64 {
                    map_name,
                    key_slot,
                    value_slot,
                    append_list,
                } => {
                    self.execute_hot_jsonl_map_add_f64(
                        map_name,
                        *key_slot,
                        *value_slot,
                        append_list.as_deref(),
                        plan,
                        accumulators,
                        slots,
                    )?;
                }
                HotJsonlBodyOp::MapAddI64 {
                    map_name,
                    key_slot,
                    value_slot,
                } => {
                    self.execute_hot_jsonl_map_add_i64(
                        map_name,
                        *key_slot,
                        *value_slot,
                        plan,
                        accumulators,
                        slots,
                    )?;
                }
                HotJsonlBodyOp::ForEachJsonString {
                    array_slot,
                    item_slot,
                    body,
                } => {
                    self.execute_hot_jsonl_for_each_string(
                        *array_slot,
                        *item_slot,
                        body,
                        plan,
                        accumulators,
                        slots,
                    )?;
                }
                HotJsonlBodyOp::MapAddI64Const { .. } => {
                    return Err(stone_error(
                        "hot loop",
                        "constant map update requires a string iteration slot",
                    ));
                }
            }
        }
        Ok(())
    }

    fn execute_hot_jsonl_map_add_f64<'a>(
        &self,
        _map_name: &str,
        key_slot: HotJsonlSlot,
        value_slot: HotJsonlSlot,
        append_list: Option<&str>,
        _plan: &HotJsonlAggregationBody,
        accumulators: &mut HotJsonlNativeAccumulators,
        slots: &HotJsonlNativeSlots<'a>,
    ) -> Result<(), ShellError> {
        if key_slot != HotJsonlSlot::User {
            return Err(stone_error("hot loop", "unsupported f64 map update"));
        }
        let value = match value_slot {
            HotJsonlSlot::Amount => hot_jsonl_fields(slots)?.amount,
            _ => return Err(stone_error("hot loop", "unsupported f64 value slot")),
        };
        let append_user = append_list.is_some();
        let user = hot_jsonl_user(slots)?;
        if let Some(total) = accumulators.user_amounts.get_mut(user.as_ref()) {
            *total += value;
        } else {
            let user_key = user.into_owned();
            accumulators.user_amounts.insert(user_key.clone(), value);
            if append_user {
                accumulators.users.push(user_key);
            }
        }
        Ok(())
    }

    fn execute_hot_jsonl_map_add_i64<'a>(
        &self,
        _map_name: &str,
        key_slot: HotJsonlSlot,
        value_slot: HotJsonlSlot,
        _plan: &HotJsonlAggregationBody,
        accumulators: &mut HotJsonlNativeAccumulators,
        slots: &HotJsonlNativeSlots<'a>,
    ) -> Result<(), ShellError> {
        if key_slot != HotJsonlSlot::User {
            return Err(stone_error("hot loop", "unsupported i64 map update"));
        }
        let value = match value_slot {
            HotJsonlSlot::Items => hot_jsonl_fields(slots)?.items,
            _ => return Err(stone_error("hot loop", "unsupported i64 value slot")),
        };
        let user = hot_jsonl_user(slots)?;
        if let Some(total) = accumulators.user_items.get_mut(user.as_ref()) {
            *total += value;
        } else {
            accumulators.user_items.insert(user.into_owned(), value);
        }
        Ok(())
    }

    fn execute_hot_jsonl_for_each_string<'a>(
        &self,
        array_slot: HotJsonlSlot,
        item_slot: HotJsonlSlot,
        body: &[HotJsonlBodyOp],
        plan: &HotJsonlAggregationBody,
        accumulators: &mut HotJsonlNativeAccumulators,
        slots: &HotJsonlNativeSlots<'a>,
    ) -> Result<(), ShellError> {
        if array_slot != HotJsonlSlot::Tags || item_slot != HotJsonlSlot::Tag {
            return Err(stone_error(
                "hot loop",
                "unsupported string iteration slots",
            ));
        }
        let tags = &hot_jsonl_fields(slots)?.tags;
        hot_jsonl_string_array_for_each_string(tags, |tag_key| {
            for op in body {
                match op {
                    HotJsonlBodyOp::MapAddI64Const {
                        map_name,
                        key_slot,
                        value,
                        append_list,
                    } => {
                        self.execute_hot_jsonl_tag_count_update(
                            map_name,
                            *key_slot,
                            *value,
                            append_list.as_deref(),
                            plan,
                            accumulators,
                            tag_key.as_ref(),
                        )?;
                    }
                    _ => {
                        return Err(stone_error(
                            "hot loop",
                            "unsupported string iteration body op",
                        ));
                    }
                }
            }
            Ok(())
        })
    }

    fn execute_hot_jsonl_tag_count_update(
        &self,
        _map_name: &str,
        key_slot: HotJsonlSlot,
        value: i64,
        append_list: Option<&str>,
        _plan: &HotJsonlAggregationBody,
        accumulators: &mut HotJsonlNativeAccumulators,
        tag_key: &str,
    ) -> Result<(), ShellError> {
        if key_slot != HotJsonlSlot::Tag {
            return Err(stone_error("hot loop", "unsupported tag count update"));
        }
        if let Some(count) = accumulators.tag_counts.get_mut(tag_key) {
            *count += value;
        } else {
            accumulators.tag_counts.insert(tag_key.to_owned(), value);
            if append_list.is_some() {
                accumulators.tags.push(tag_key.to_owned());
            }
        }
        Ok(())
    }

    fn load_hot_jsonl_native_accumulators(
        &self,
        plan: &HotJsonlTracePlan,
    ) -> Result<HotJsonlNativeAccumulators, ShellError> {
        Ok(HotJsonlNativeAccumulators {
            user_amounts: self.load_f64_record_map(&plan.user_amounts_map)?,
            user_items: self.load_i64_record_map(&plan.user_items_map)?,
            users: match &plan.users_list {
                Some(name) => self.load_string_list(name)?,
                None => Vec::new(),
            },
            tag_counts: self.load_i64_record_map(&plan.tag_counts_map)?,
            tags: match &plan.tags_list {
                Some(name) => self.load_string_list(name)?,
                None => Vec::new(),
            },
        })
    }

    fn load_nested_hot_jsonl_native_accumulators(
        &self,
        plan: &HotJsonlTracePlan,
        nested: &HotJsonlNestedUserTotals,
    ) -> Result<HotJsonlNativeAccumulators, ShellError> {
        let (users, user_amounts, user_items) = self.load_nested_totals_record_map(nested)?;
        Ok(HotJsonlNativeAccumulators {
            user_amounts,
            user_items,
            users,
            tag_counts: self.load_i64_record_map(&plan.tag_counts_map)?,
            tags: match &plan.tags_list {
                Some(name) => self.load_string_list(name)?,
                None => Vec::new(),
            },
        })
    }

    fn load_nested_totals_record_map(
        &self,
        nested: &HotJsonlNestedUserTotals,
    ) -> Result<(Vec<String>, HashMap<String, f64>, HashMap<String, i64>), ShellError> {
        let value = self.state.get_local(&nested.map_name).ok_or_else(|| {
            stone_error("hot loop", format!("unknown name `{}`", nested.map_name))
        })?;
        let RuntimeValue::Nu(Value::Record { val, .. }) = value else {
            return Err(stone_error(
                "hot loop",
                format!("{} is not a record", nested.map_name),
            ));
        };
        let mut users = Vec::with_capacity(val.len());
        let mut amounts = HashMap::with_capacity(val.len());
        let mut items = HashMap::with_capacity(val.len());
        for (user, value) in val.iter() {
            let Value::Record { val: totals, .. } = value else {
                return Err(stone_error(
                    "hot loop",
                    format!("{}[{user}] is not a record", nested.map_name),
                ));
            };
            let amount = totals
                .get(&nested.amount_field)
                .ok_or_else(|| {
                    stone_error(
                        "hot loop",
                        format!(
                            "{}[{user}] has no `{}`",
                            nested.map_name, nested.amount_field
                        ),
                    )
                })
                .and_then(|value| value_to_f64(value, "hot loop"))?;
            let item_count = totals
                .get(&nested.items_field)
                .ok_or_else(|| {
                    stone_error(
                        "hot loop",
                        format!(
                            "{}[{user}] has no `{}`",
                            nested.map_name, nested.items_field
                        ),
                    )
                })
                .and_then(|value| value_to_i64(value, "hot loop"))?;
            users.push(user.clone());
            amounts.insert(user.clone(), amount);
            items.insert(user.clone(), item_count);
        }
        Ok((users, amounts, items))
    }

    fn load_f64_record_map(&self, name: &str) -> Result<HashMap<String, f64>, ShellError> {
        let value = self
            .state
            .get_local(name)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{name}`")))?;
        let RuntimeValue::Nu(Value::Record { val, .. }) = value else {
            return Err(stone_error("hot loop", format!("{name} is not a record")));
        };
        let mut map = HashMap::with_capacity(val.len());
        for (key, value) in val.iter() {
            map.insert(key.clone(), value_to_f64(value, "hot loop")?);
        }
        Ok(map)
    }

    fn load_i64_record_map(&self, name: &str) -> Result<HashMap<String, i64>, ShellError> {
        let value = self
            .state
            .get_local(name)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{name}`")))?;
        let RuntimeValue::Nu(Value::Record { val, .. }) = value else {
            return Err(stone_error("hot loop", format!("{name} is not a record")));
        };
        let mut map = HashMap::with_capacity(val.len());
        for (key, value) in val.iter() {
            map.insert(key.clone(), value_to_i64(value, "hot loop")?);
        }
        Ok(map)
    }

    fn load_string_list(&self, name: &str) -> Result<Vec<String>, ShellError> {
        let value = self
            .state
            .get_local(name)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{name}`")))?;
        let RuntimeValue::Nu(Value::List { vals, .. }) = value else {
            return Err(stone_error("hot loop", format!("{name} is not a list")));
        };
        vals.iter()
            .map(|value| value_to_string(value, "hot loop"))
            .collect()
    }

    fn try_eval_fused_map_update_if(
        &mut self,
        condition: &Expr,
        then_branch: &[Stmt],
        else_branch: &[Stmt],
    ) -> Result<bool, ShellError> {
        let Some(plan) = match_fused_map_update_if(condition, then_branch, else_branch) else {
            return Ok(false);
        };
        self.execute_fused_map_update_if(plan)?;
        Ok(true)
    }

    fn execute_fused_map_update_if(&mut self, plan: FusedMapUpdateIf) -> Result<(), ShellError> {
        let key = self.state.get_local(&plan.key_name).ok_or_else(|| {
            stone_error("membership", format!("unknown name `{}`", plan.key_name))
        })?;
        let key_text = runtime_value_to_string_key(&key, "membership")?;
        let addends = plan
            .updates
            .iter()
            .map(|update| {
                self.eval_expr_value(&update.addend, PipelineData::empty())?
                    .into_nu_value("addition")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let exists = self.record_contains_key(&plan.contains_map, &key_text)?;
        for (update, addend) in plan.updates.iter().zip(addends.into_iter()) {
            self.apply_fused_map_update(&update.map_name, &key_text, addend, exists)?;
        }
        if !exists {
            if let Some(list_name) = plan.append_list {
                self.append_fused_key(&list_name, key.into_nu_value("append")?)?;
            }
        }
        Ok(())
    }

    fn record_contains_key(&mut self, map_name: &str, key: &str) -> Result<bool, ShellError> {
        let map = self
            .state
            .get_local_mut(map_name)
            .ok_or_else(|| stone_error("membership", format!("unknown name `{map_name}`")))?;
        let RuntimeValue::Nu(Value::Record { val, .. }) = map else {
            return Err(stone_error(
                "membership",
                format!("{map_name} is not a record"),
            ));
        };
        Ok(val.get(key).is_some())
    }

    fn apply_fused_map_update(
        &mut self,
        map_name: &str,
        key: &str,
        addend: Value,
        exists: bool,
    ) -> Result<(), ShellError> {
        let map = self
            .state
            .get_local_mut(map_name)
            .ok_or_else(|| stone_error("assignment", format!("unknown name `{map_name}`")))?;
        let RuntimeValue::Nu(Value::Record { val, .. }) = map else {
            return Err(stone_error(
                "assignment",
                format!("{map_name} is not a record"),
            ));
        };
        if exists {
            let slot = val
                .to_mut()
                .get_mut(key)
                .ok_or_else(|| stone_error("assignment", format!("record has no key `{key}`")))?;
            *slot = add_values(slot, &addend)?;
        } else {
            val.to_mut().insert(key.to_owned(), addend);
        }
        Ok(())
    }

    fn append_fused_key(&mut self, list_name: &str, key: Value) -> Result<(), ShellError> {
        let list = self
            .state
            .get_local_mut(list_name)
            .ok_or_else(|| stone_error("append", format!("unknown name `{list_name}`")))?;
        let RuntimeValue::Nu(Value::List { vals, .. }) = list else {
            return Err(stone_error("append", format!("{list_name} is not a list")));
        };
        vals.push(key);
        Ok(())
    }

    fn eval_with_stmt(
        &mut self,
        target: Option<&str>,
        context: &Expr,
        body: &[Stmt],
    ) -> Result<EvalFlow, ShellError> {
        self.state.push_scope();
        let value = match self.eval_expr_value(context, PipelineData::empty()) {
            Ok(value) => value,
            Err(err) => {
                self.state.pop_scope()?;
                return Err(err);
            }
        };
        if let Some(target) = target {
            self.state.set_local(target.to_owned(), value);
        }
        let result = self.eval_block(body, PipelineData::empty(), false);
        self.state.pop_scope_merging_nu_locals(target)?;
        result
    }

    fn assign_value(
        &mut self,
        target: &AssignTarget,
        value: RuntimeValue,
        record_session_bindings: bool,
    ) -> Result<(), ShellError> {
        let started = self.state.profiler.start();
        let result = match target {
            AssignTarget::Name(name) => {
                if record_session_bindings {
                    self.state.record_session_binding(name);
                }
                self.state.set_local(name.clone(), value);
                Ok(())
            }
            AssignTarget::Tuple(targets) => {
                if record_session_bindings {
                    for target in targets {
                        self.state.record_session_binding(target);
                    }
                }
                let value = value.into_nu_value("assignment")?;
                self.assign_unpack_targets("assignment", targets, value)
            }
            AssignTarget::Subscript { .. } => {
                let (name, indices) = self.eval_assign_target_path(target, "assignment")?;
                let value = value.into_nu_value("assignment")?;
                let target = self
                    .state
                    .get_local_mut(&name)
                    .ok_or_else(|| stone_error("assignment", format!("unknown name `{name}`")))?;
                let RuntimeValue::Nu(target) = target else {
                    return Err(stone_error(
                        "assignment",
                        format!("{name} does not support item assignment"),
                    ));
                };
                assign_subscript_path(target, &indices, value)
            }
        };
        self.state
            .profiler
            .finish(EvalProfileBucket::Assign, started);
        result
    }

    fn eval_assign_target_value(&mut self, target: &AssignTarget) -> Result<Value, ShellError> {
        let started = self.state.profiler.start();
        let result = match target {
            AssignTarget::Name(name) => self
                .state
                .get_local(name)
                .ok_or_else(|| {
                    stone_error("augmented assignment", format!("unknown name `{name}`"))
                })?
                .into_nu_value("augmented assignment"),
            AssignTarget::Subscript { .. } => {
                let (name, indices) =
                    self.eval_assign_target_path(target, "augmented assignment")?;
                let mut value = self.state.get_local(&name).ok_or_else(|| {
                    stone_error("augmented assignment", format!("unknown name `{name}`"))
                })?;
                for index in indices {
                    let target = value.into_nu_value("augmented assignment")?;
                    value = RuntimeValue::Nu(subscript_builtin(&target, &index)?);
                }
                value.into_nu_value("augmented assignment")
            }
            AssignTarget::Tuple(_) => Err(stone_error(
                "augmented assignment",
                "tuple/list destructuring cannot be used with augmented assignment",
            )),
        };
        self.state
            .profiler
            .finish(EvalProfileBucket::AssignTargetValue, started);
        result
    }

    fn eval_assign_target_path(
        &mut self,
        target: &AssignTarget,
        context: &str,
    ) -> Result<(String, Vec<Value>), ShellError> {
        match target {
            AssignTarget::Name(name) => Ok((name.clone(), Vec::new())),
            AssignTarget::Tuple(_) => Err(stone_error(
                context,
                "tuple/list destructuring does not support item assignment",
            )),
            AssignTarget::Subscript { value, index } => {
                let (name, mut indices) = self.eval_assign_target_path(value, context)?;
                let index = self
                    .eval_expr_value(index, PipelineData::empty())?
                    .into_nu_value(context)?;
                indices.push(index);
                Ok((name, indices))
            }
        }
    }

    fn eval_mutable_expr_path(
        &mut self,
        expression: &Expr,
        context: &str,
    ) -> Result<(String, Vec<Value>), ShellError> {
        match expression {
            Expr::Name(name) => Ok((name.clone(), Vec::new())),
            Expr::Subscript { value, index } => {
                let (name, mut indices) = self.eval_mutable_expr_path(value, context)?;
                let index = self
                    .eval_expr_value(index, PipelineData::empty())?
                    .into_nu_value(context)?;
                indices.push(index);
                Ok((name, indices))
            }
            Expr::Attribute { value, attr } => {
                let (name, mut indices) = self.eval_mutable_expr_path(value, context)?;
                indices.push(Value::string(attr.clone(), Span::unknown()));
                Ok((name, indices))
            }
            _ => Err(stone_error(
                context,
                format!("{context}() is only supported on local list paths"),
            )),
        }
    }

    fn assign_loop_targets(
        &mut self,
        targets: &[String],
        value: RuntimeValue,
    ) -> Result<(), ShellError> {
        if let [target] = targets {
            self.state.set_local(target.clone(), value);
            return Ok(());
        }

        let value = value.into_nu_value("for")?;
        let Value::List { vals, .. } = value else {
            return Err(stone_error(
                "for",
                "tuple loop target requires a list item to unpack",
            ));
        };
        if vals.len() != targets.len() {
            return Err(stone_error(
                "for",
                format!(
                    "tuple loop target expected {} values, got {}",
                    targets.len(),
                    vals.len()
                ),
            ));
        }
        for (target, value) in targets.iter().zip(vals.into_iter()) {
            self.state
                .set_local(target.clone(), RuntimeValue::Nu(value));
        }
        Ok(())
    }

    fn assign_unpack_targets(
        &mut self,
        context: &str,
        targets: &[String],
        value: Value,
    ) -> Result<(), ShellError> {
        let Value::List { vals, .. } = value else {
            return Err(stone_error(
                context,
                "tuple/list destructuring requires a list value to unpack",
            ));
        };
        if vals.len() != targets.len() {
            return Err(stone_error(
                context,
                format!(
                    "tuple/list destructuring expected {} values, got {}",
                    targets.len(),
                    vals.len()
                ),
            ));
        }
        for (target, value) in targets.iter().zip(vals.into_iter()) {
            self.state
                .set_local(target.clone(), RuntimeValue::Nu(value));
        }
        Ok(())
    }

    fn eval_list_comprehension(
        &mut self,
        target: &str,
        iter: &Expr,
        elt: &Expr,
        filters: &[Expr],
    ) -> Result<RuntimeValue, ShellError> {
        let values = self.eval_iterable_expr(iter)?;
        let previous = self.state.get_local(target);
        let mut output = Vec::new();

        for value in values {
            self.state.set_local(target.to_owned(), value);
            let mut keep = true;
            for filter in filters {
                let value = self
                    .eval_expr_value(filter, PipelineData::empty())?
                    .into_nu_value("list comprehension")?;
                if !value_truthy(&value) {
                    keep = false;
                    break;
                }
            }
            if keep {
                output.push(
                    self.eval_expr_value(elt, PipelineData::empty())?
                        .into_nu_value("list comprehension")?,
                );
            }
        }

        match previous {
            Some(value) => self.state.set_local(target.to_owned(), value),
            None => self.state.remove_local(target),
        }

        Ok(RuntimeValue::Nu(Value::list(output, Span::unknown())))
    }

    fn eval_dict_comprehension(
        &mut self,
        target: &str,
        iter: &Expr,
        key: &Expr,
        value: &Expr,
        filters: &[Expr],
    ) -> Result<RuntimeValue, ShellError> {
        let values = self.eval_iterable_expr(iter)?;
        let previous = self.state.get_local(target);
        let mut record = Record::with_capacity(values.len());

        for item in values {
            self.state.set_local(target.to_owned(), item);
            let mut keep = true;
            for filter in filters {
                let value = self
                    .eval_expr_value(filter, PipelineData::empty())?
                    .into_nu_value("dict comprehension")?;
                if !value_truthy(&value) {
                    keep = false;
                    break;
                }
            }
            if keep {
                let key = self
                    .eval_expr_value(key, PipelineData::empty())?
                    .into_nu_value("dict comprehension")?;
                let key = value_to_string(&key, "dict comprehension key")?;
                let value = self
                    .eval_expr_value(value, PipelineData::empty())?
                    .into_nu_value("dict comprehension")?;
                record.push(key, value);
            }
        }

        match previous {
            Some(value) => self.state.set_local(target.to_owned(), value),
            None => self.state.remove_local(target),
        }

        Ok(RuntimeValue::Nu(Value::record(record, Span::unknown())))
    }

    fn eval_formatted_string(
        &mut self,
        parts: &[FormattedStringPart],
    ) -> Result<RuntimeValue, ShellError> {
        let mut output = String::new();
        for part in parts {
            match part {
                FormattedStringPart::Literal(text) => output.push_str(text),
                FormattedStringPart::Expr(expr) => {
                    let value = self
                        .eval_expr_value(expr, PipelineData::empty())?
                        .into_nu_value("f-string")?;
                    output.push_str(&value_to_display_string(&value)?);
                }
                FormattedStringPart::Formatted { expr, spec } => {
                    let value = self
                        .eval_expr_value(expr, PipelineData::empty())?
                        .into_nu_value("f-string")?;
                    output.push_str(&format_fstring_value(&value, spec)?);
                }
            }
        }
        Ok(RuntimeValue::Nu(Value::string(output, Span::unknown())))
    }

    fn eval_expr_pipeline(
        &mut self,
        expression: &Expr,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        match expression {
            Expr::Call(call)
                if is_builtin_call(call)
                    || self.state.functions.contains_key(&call.name)
                    || self.state.get_local(&call.name).is_some() =>
            {
                Ok(self
                    .eval_expr_value(expression, input)?
                    .into_nu_value("pipeline")?
                    .into_pipeline_data())
            }
            Expr::Call(call) => Err(unknown_stone_call_error(&call.name)),
            other => Ok(self
                .eval_expr_value(other, input)?
                .into_nu_value("pipeline")?
                .into_pipeline_data()),
        }
    }

    fn eval_expr_value(
        &mut self,
        expression: &Expr,
        input: PipelineData,
    ) -> Result<RuntimeValue, ShellError> {
        let started = self.state.profiler.start();
        let result = (|| {
            let span = Span::unknown();
            match expression {
                Expr::None => Ok(RuntimeValue::Nu(Value::nothing(span))),
                Expr::Bool(value) => Ok(RuntimeValue::Nu(Value::bool(*value, span))),
                Expr::Int(value) => value
                    .parse::<i64>()
                    .map(|value| RuntimeValue::Nu(Value::int(value, span)))
                    .map_err(|err| stone_error("integer literal", err.to_string())),
                Expr::Float(value) => Ok(RuntimeValue::Nu(Value::float(*value, span))),
                Expr::String(value) => Ok(RuntimeValue::Nu(Value::string(value.clone(), span))),
                Expr::FormattedString(parts) => self.eval_formatted_string(parts),
                Expr::List(items) | Expr::Tuple(items) => {
                    let mut values = Vec::with_capacity(items.len());
                    for item in items {
                        values.push(
                            self.eval_expr_value(item, PipelineData::empty())?
                                .into_nu_value("sequence literal")?,
                        );
                    }
                    Ok(RuntimeValue::Nu(Value::list(values, span)))
                }
                Expr::ListComprehension {
                    target,
                    iter,
                    elt,
                    filters,
                } => self.eval_list_comprehension(target, iter, elt, filters),
                Expr::DictComprehension {
                    target,
                    iter,
                    key,
                    value,
                    filters,
                } => self.eval_dict_comprehension(target, iter, key, value, filters),
                Expr::Record(items) => {
                    let mut record = Record::with_capacity(items.len());
                    for (key, value) in items {
                        record.push(
                            key.clone(),
                            self.eval_expr_value(value, PipelineData::empty())?
                                .into_nu_value("record literal")?,
                        );
                    }
                    Ok(RuntimeValue::Nu(Value::record(record, span)))
                }
                Expr::Name(name) => self
                    .state
                    .get_local(name)
                    .ok_or_else(|| stone_error("name", format!("unknown name `{name}`"))),
                Expr::Subscript { value, index } => {
                    match self.try_eval_json_object_literal_subscript(value, index)? {
                        Some(value) => Ok(value),
                        None => {
                            let value = self.eval_expr_value(value, PipelineData::empty())?;
                            match index.as_ref() {
                                Expr::Slice { lower, upper } => {
                                    let value = value.into_nu_value("subscript")?;
                                    let lower = self.eval_optional_index(lower.as_deref())?;
                                    let upper = self.eval_optional_index(upper.as_deref())?;
                                    slice_builtin(&value, lower, upper).map(RuntimeValue::Nu)
                                }
                                index => {
                                    let index = self
                                        .eval_expr_value(index, PipelineData::empty())?
                                        .into_nu_value("subscript")?;
                                    eval_runtime_subscript(value, &index)
                                }
                            }
                        }
                    }
                }
                Expr::Attribute { value, attr } => self.eval_attribute_expr(value, attr),
                Expr::Slice { .. } => Err(stone_error(
                    "slice",
                    "slice expressions are only supported inside subscripts",
                )),
                Expr::Compare {
                    left,
                    ops,
                    comparators,
                } => self.eval_compare(left, ops, comparators),
                Expr::BoolOp { op, values } => self.eval_bool_op(*op, values),
                Expr::Not(value) => {
                    let value = self.eval_expr_value(value, PipelineData::empty())?;
                    let value = value.into_nu_value("not")?;
                    Ok(RuntimeValue::Nu(Value::bool(!value_truthy(&value), span)))
                }
                Expr::Neg(value) => {
                    let value = self
                        .eval_expr_value(value, PipelineData::empty())?
                        .into_nu_value("unary minus")?;
                    neg_value(&value).map(RuntimeValue::Nu)
                }
                Expr::Invert(value) => {
                    let value = self
                        .eval_expr_value(value, PipelineData::empty())?
                        .into_nu_value("bitwise invert")?;
                    let value = value_to_i64(&value, "bitwise invert")?;
                    Ok(RuntimeValue::Nu(Value::int(!value, span)))
                }
                Expr::Add { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("addition")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("addition")?;
                    add_values(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::Sub { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("subtraction")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("subtraction")?;
                    sub_values(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::Mul { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("multiplication")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("multiplication")?;
                    mul_values(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::Div { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("division")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("division")?;
                    div_values(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::FloorDiv { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("floor division")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("floor division")?;
                    floor_div_values(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::Mod { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("modulo")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("modulo")?;
                    mod_values(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::BitAnd { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("bitwise and")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("bitwise and")?;
                    bitwise_int_values(&left, &right, "bitwise and", |left, right| left & right)
                        .map(RuntimeValue::Nu)
                }
                Expr::BitOr { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("bitwise or")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("bitwise or")?;
                    bitwise_int_values(&left, &right, "bitwise or", |left, right| left | right)
                        .map(RuntimeValue::Nu)
                }
                Expr::BitXor { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("bitwise xor")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("bitwise xor")?;
                    bitwise_int_values(&left, &right, "bitwise xor", |left, right| left ^ right)
                        .map(RuntimeValue::Nu)
                }
                Expr::LShift { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("left shift")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("left shift")?;
                    shift_value(&left, &right, "left shift", i64::checked_shl).map(RuntimeValue::Nu)
                }
                Expr::RShift { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("right shift")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("right shift")?;
                    shift_value(&left, &right, "right shift", i64::checked_shr)
                        .map(RuntimeValue::Nu)
                }
                Expr::Generator { .. } => Err(stone_error(
                    "generator expression",
                    "generator expressions are only supported inside sum(...) for now",
                )),
                Expr::Lambda { params, body } => Ok(RuntimeValue::Callable(CallableValue {
                    function_id: self.state.next_callable_id(),
                    params: params.clone(),
                    body: body.clone(),
                    captures: self.state.capture_locals(),
                })),
                Expr::Call(call) if self.state.functions.contains_key(&call.name) => {
                    self.eval_user_function_call(call)
                }
                Expr::Call(call) if is_builtin_call(call) => self.eval_builtin_call(call, input),
                Expr::Call(call) if self.state.get_local(&call.name).is_some() => {
                    self.eval_named_callable_call(call)
                }
                Expr::MethodCall {
                    receiver,
                    method,
                    positional,
                } => self.eval_method_call(receiver, method, positional),
                Expr::Call(call) => Err(unknown_stone_call_error(&call.name)),
            }
        })();
        self.state.profiler.finish(EvalProfileBucket::Expr, started);
        result
    }

    fn try_eval_json_object_literal_subscript(
        &mut self,
        value: &Expr,
        index: &Expr,
    ) -> Result<Option<RuntimeValue>, ShellError> {
        let (Expr::Name(name), Expr::String(key)) = (value, index) else {
            return Ok(None);
        };
        let Some(RuntimeValue::JsonObjectView(view)) = self.state.get_local(name) else {
            return Ok(None);
        };
        let Some(value) = json_object_view_get(&view, key)? else {
            return Err(stone_error(
                "subscript",
                format!("record has no key `{key}`"),
            ));
        };
        Ok(Some(value))
    }

    fn eval_builtin_call(
        &mut self,
        call: &Call,
        input: PipelineData,
    ) -> Result<RuntimeValue, ShellError> {
        let started = self.state.profiler.start();
        let result = match call.name.as_str() {
            "int" => {
                let [arg] = call.positional.as_slice() else {
                    return Err(stone_error("int", "int() requires exactly one argument"));
                };
                if !call.named.is_empty() {
                    return Err(stone_error(
                        "int",
                        "int() keyword arguments are not supported",
                    ));
                }
                let value = self.eval_expr_value(arg, PipelineData::empty())?;
                if let RuntimeValue::JsonScalarView(view) = value {
                    return json_scalar_view_to_i64(&view)
                        .map(|value| RuntimeValue::Nu(Value::int(value, Span::unknown())));
                }
                let value = value.into_nu_value("int")?;
                int_builtin(&value).map(RuntimeValue::Nu)
            }
            "float" => {
                let [arg] = call.positional.as_slice() else {
                    return Err(stone_error(
                        "float",
                        "float() requires exactly one argument",
                    ));
                };
                if !call.named.is_empty() {
                    return Err(stone_error(
                        "float",
                        "float() keyword arguments are not supported",
                    ));
                }
                let value = self.eval_expr_value(arg, PipelineData::empty())?;
                if let RuntimeValue::JsonScalarView(view) = value {
                    return json_scalar_view_to_f64(&view)
                        .map(|value| RuntimeValue::Nu(Value::float(value, Span::unknown())));
                }
                let value = value.into_nu_value("float")?;
                float_builtin(&value).map(RuntimeValue::Nu)
            }
            "len" => {
                let [arg] = call.positional.as_slice() else {
                    return Err(stone_error("len", "len() requires exactly one argument"));
                };
                if !call.named.is_empty() {
                    return Err(stone_error(
                        "len",
                        "len() keyword arguments are not supported",
                    ));
                }
                let value = self
                    .eval_expr_value(arg, PipelineData::empty())?
                    .into_nu_value("len")?;
                len_builtin(&value).map(RuntimeValue::Nu)
            }
            "list" => self.eval_list_call(call),
            "str" => {
                let [arg] = call.positional.as_slice() else {
                    return Err(stone_error("str", "str() requires exactly one argument"));
                };
                if !call.named.is_empty() {
                    return Err(stone_error(
                        "str",
                        "str() keyword arguments are not supported",
                    ));
                }
                let value = self
                    .eval_expr_value(arg, PipelineData::empty())?
                    .into_nu_value("str")?;
                str_builtin(&value).map(RuntimeValue::Nu)
            }
            "type" => self.eval_type_call(call),
            "enumerate" => self.eval_enumerate_call(call),
            "echo" => self.eval_echo_call(call),
            "emit" => self.eval_emit_call(call, input),
            "fail" => self.eval_fail_call(call),
            "find" => self.eval_find_call(call),
            "diff" => self.eval_diff_call(call),
            "format" => self.eval_format_call(call),
            "json_dumps" => self.eval_json_dumps_call(call),
            "json_loads" => self.eval_json_loads_call(call),
            "help" => self.eval_help_call(call),
            "max" => self.eval_min_max_call(call, MinMax::Max),
            "min" => self.eval_min_max_call(call, MinMax::Min),
            "open" => self.eval_open_call(call),
            "pwd" => self.eval_pwd_call(call),
            "cd" => self.eval_cd_call(call),
            "cat" => self.eval_cat_call(call),
            "ls" => self.eval_list_dir_call(call),
            "list_dir" => self.eval_list_dir_call(call),
            "first" | "head" => self.eval_first_call(call),
            "last" | "tail" => self.eval_last_call(call),
            "read_text" | "read_file" => self.eval_read_text_call(call),
            "stat" => self.eval_stat_call(call),
            "mkdir" => self.eval_mkdir_call(call),
            "rm" => self.eval_rm_call(call),
            "edit" | "edit_file" => self.eval_edit_call(call),
            "save" => self.eval_save_call(call),
            "search" => self.eval_search_call(call),
            "where" => self.eval_where_call(call),
            "from_json" => self.eval_from_json_call(call),
            "to_json" => self.eval_to_json_call(call),
            "to_jsonl" => self.eval_to_jsonl_call(call),
            "write_text" | "write_file" => self.eval_write_text_call(call),
            "get" | "keys" | "values" | "items" => self.eval_record_helper_call(call, input),
            "parse_float" => self.eval_parse_float_call(call),
            "parse_int" => self.eval_parse_int_call(call),
            "print" => self.eval_print_call(call),
            "range" => self.eval_range_call(call),
            "slice" => self.eval_slice_call(call),
            "split" => self.eval_split_call(call),
            "read_csv" => self.eval_read_csv_call(call),
            "read_json" => self.eval_read_json_call(call),
            "read_jsonl" => self.eval_read_jsonl_call(call),
            "round" => self.eval_round_call(call),
            "run" => self.eval_run_call(call),
            "resolve_command" => self.eval_resolve_command_call(call),
            "state" => self.eval_state_call(call),
            "last_result" => self.eval_last_result_call(call),
            "start_daemon" => self.eval_start_daemon_call(call),
            "starts_with" | "startswith" => self.eval_starts_with_call(call),
            "daemon_status" => self.eval_daemon_status_call(call),
            "stop_daemon" => self.eval_stop_daemon_call(call),
            "wait_port" => self.eval_wait_port_call(call),
            "sort" | "sorted" => self.eval_sort_call(call),
            "set" => self.eval_set_call(call),
            "unique" => self.eval_unique_call(call),
            "write_json" => self.eval_write_json_call(call),
            "write_jsonl" => self.eval_write_jsonl_call(call),
            "filter" => self.eval_filter_call(call),
            "map" => self.eval_map_call(call),
            "sum" => self.eval_sum_call(call),
            "join" => self.eval_join_call(call),
            _ => Err(stone_error(
                "builtin",
                format!("unknown builtin `{}`", call.name),
            )),
        };
        self.state
            .profiler
            .finish(EvalProfileBucket::BuiltinCall, started);
        result
    }

    fn eval_user_function_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "function call",
                "user functions only support positional arguments for now",
            ));
        }
        let function = self
            .state
            .functions
            .get(&call.name)
            .cloned()
            .ok_or_else(|| {
                stone_error("function call", format!("unknown function `{}`", call.name))
            })?;
        if call.positional.len() != function.params.len() {
            return Err(stone_error(
                "function call",
                format!(
                    "{}() expected {} argument(s), got {}",
                    function.name,
                    function.params.len(),
                    call.positional.len()
                ),
            ));
        }

        let mut args = Vec::with_capacity(call.positional.len());
        for (expr, param) in call.positional.iter().zip(&function.params) {
            let value = self
                .eval_expr_value(expr, PipelineData::empty())?
                .into_nu_value("function argument")?;
            ensure_type(&value, param.ty, &format!("argument `{}`", param.name))?;
            args.push((param.name.clone(), value));
        }

        self.state.push_scope();
        for (name, value) in args {
            self.state.set_local(name, RuntimeValue::Nu(value));
        }
        let flow = self.eval_block(&function.body, PipelineData::empty(), false);
        self.state.pop_scope()?;
        let value = match flow? {
            EvalFlow::Return(value) => value,
            EvalFlow::Output(_) => Value::nothing(Span::unknown()),
            EvalFlow::Break => return Err(stone_error("break", "break outside loop")),
            EvalFlow::Continue => return Err(stone_error("continue", "continue outside loop")),
        };
        ensure_type(
            &value,
            function.return_type,
            &format!("{}() return value", function.name),
        )?;
        Ok(RuntimeValue::Nu(value))
    }

    fn eval_named_callable_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "callable",
                "lambda calls only support positional arguments for now",
            ));
        }
        let Some(RuntimeValue::Callable(callable)) = self.state.get_local(&call.name) else {
            return Err(unknown_stone_call_error(&call.name));
        };
        let args = call
            .positional
            .iter()
            .map(|arg| self.eval_expr_value(arg, PipelineData::empty()))
            .collect::<Result<Vec<_>, _>>()?;
        self.invoke_callable(&callable, args)
    }

    fn eval_attribute_expr(
        &mut self,
        value: &Expr,
        attr: &str,
    ) -> Result<RuntimeValue, ShellError> {
        let receiver = self.eval_expr_value(value, PipelineData::empty())?;
        if let RuntimeValue::JsonObjectView(view) = receiver {
            let Some(value) = json_object_view_get(&view, attr)? else {
                return Err(stone_error(
                    "attribute",
                    format!("record has no attribute `{attr}`"),
                ));
            };
            return Ok(value);
        }
        let receiver = receiver.into_nu_value("attribute")?;
        let Value::Record { val, .. } = receiver else {
            return Err(stone_error(
                "attribute",
                format!("{} has no attribute `{attr}`", receiver.get_type()),
            ));
        };
        val.get(attr)
            .cloned()
            .map(RuntimeValue::Nu)
            .ok_or_else(|| stone_error("attribute", format!("record has no attribute `{attr}`")))
    }

    fn eval_callable_expr(&mut self, expression: &Expr) -> Result<CallableValue, ShellError> {
        match self.eval_expr_value(expression, PipelineData::empty())? {
            RuntimeValue::Callable(callable) => Ok(callable),
            other => Err(stone_error(
                "callable",
                format!(
                    "expected lambda/callable, got {}",
                    other.into_nu_value("callable")?.get_type()
                ),
            )),
        }
    }

    fn invoke_callable(
        &mut self,
        callable: &CallableValue,
        args: Vec<RuntimeValue>,
    ) -> Result<RuntimeValue, ShellError> {
        if args.len() != callable.params.len() {
            return Err(stone_error(
                "callable",
                format!(
                    "lambda#{} expected {} argument(s), got {}",
                    callable.function_id,
                    callable.params.len(),
                    args.len()
                ),
            ));
        }
        self.state.push_scope();
        for (name, value) in &callable.captures {
            self.state.set_local(name.clone(), value.clone());
        }
        for (name, value) in callable.params.iter().zip(args) {
            self.state.set_local(name.clone(), value);
        }
        let result = self.eval_expr_value(&callable.body, PipelineData::empty());
        self.state.pop_scope()?;
        result
    }

    fn eval_open_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([path] | [path, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "open",
                "open() requires a path and optional mode",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "open",
                "open() keyword arguments are not supported",
            ));
        }

        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("open")?;
        let path = value_to_path_string(&path, "open")?;
        let target = self.resolve_script_path(&path)?;
        let mode = match call.positional.as_slice() {
            [_] => "r".to_owned(),
            [_, mode] => {
                let mode = self
                    .eval_expr_value(mode, PipelineData::empty())?
                    .into_nu_value("open")?;
                value_to_string(&mode, "open")?
            }
            _ => unreachable!(),
        };

        let runtime_file = open_runtime_file(&target, &mode)?;
        let handle = self.state.insert_file(runtime_file);
        Ok(RuntimeValue::File(handle))
    }

    fn eval_cat_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [path] = call.positional.as_slice() else {
            return Err(stone_error("cat", "cat() requires exactly one path"));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "cat",
                "cat() keyword arguments are not supported",
            ));
        }
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("cat")?;
        let path = value_to_path_string(&path, "cat")?;
        let target = self.resolve_script_path(&path)?;
        let text = cat_text(&target)?;
        Ok(RuntimeValue::Nu(Value::string(text, Span::unknown())))
    }

    fn eval_read_text_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([path] | [path, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "read_text",
                "read_text() requires a path and optional max_bytes",
            ));
        };
        let mut max_bytes = 1_048_576;
        if let Some(max_bytes_expr) = call.positional.get(1) {
            let value = self
                .eval_expr_value(max_bytes_expr, PipelineData::empty())?
                .into_nu_value("read_text")?;
            max_bytes = value_to_limit(&value, "read_text max_bytes")?;
        }
        for (name, argument) in &call.named {
            if matches!(name.as_str(), "max_bytes" | "limit") && call.positional.len() > 1 {
                return Err(stone_error(
                    "read_text",
                    "read_text() max_bytes was provided twice",
                ));
            }
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("read_text")?;
            match name.as_str() {
                "max_bytes" | "limit" => max_bytes = value_to_limit(&value, "read_text max_bytes")?,
                other => {
                    return Err(stone_error(
                        "read_text",
                        format!("unsupported keyword `{other}`; expected max_bytes or limit"),
                    ));
                }
            }
        }

        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("read_text")?;
        let path = value_to_path_string(&path, "read_text")?;
        let target = self.resolve_script_path(&path)?;
        let text = stone_read_text(&target, max_bytes)?;
        Ok(RuntimeValue::Nu(Value::string(text, Span::unknown())))
    }

    fn eval_write_text_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [path, value] = call.positional.as_slice() else {
            return Err(stone_error(
                "write_text",
                "write_text() requires path and text arguments",
            ));
        };
        let mut append = false;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("write_text")?;
            match name.as_str() {
                "append" => append = value_to_bool(&value, "write_text append")?,
                other => {
                    return Err(stone_error(
                        "write_text",
                        format!("unsupported keyword `{other}`; expected append"),
                    ));
                }
            }
        }

        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("write_text")?;
        let path = value_to_path_string(&path, "write_text")?;
        let target = self.resolve_script_path(&path)?;
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("write_text")?;
        let text = value_to_string(&value, "write_text")?;
        Ok(RuntimeValue::Nu(stone_write_text(&target, &text, append)?))
    }

    fn eval_stat_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [path] = call.positional.as_slice() else {
            return Err(stone_error("stat", "stat() requires exactly one path"));
        };
        let mut follow_symlinks = false;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("stat")?;
            match name.as_str() {
                "follow_symlinks" => {
                    follow_symlinks = value_to_bool(&value, "stat follow_symlinks")?
                }
                other => {
                    return Err(stone_error(
                        "stat",
                        format!("unsupported keyword `{other}`; expected follow_symlinks"),
                    ));
                }
            }
        }

        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("stat")?;
        let path = value_to_path_string(&path, "stat")?;
        let target = self.resolve_script_path(&path)?;
        Ok(RuntimeValue::Nu(stat_record(&target, follow_symlinks)?))
    }

    fn eval_list_dir_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let path_expr = match call.positional.as_slice() {
            [] => None,
            [path] => Some(path),
            _ => {
                return Err(stone_error("list_dir", "list_dir() takes at most one path"));
            }
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "list_dir",
                "list_dir() keyword arguments are not supported",
            ));
        }
        let target = match path_expr {
            Some(path) => {
                let path = self
                    .eval_expr_value(path, PipelineData::empty())?
                    .into_nu_value("list_dir")?;
                let path = value_to_path_string(&path, "list_dir")?;
                self.resolve_script_path(&path)?
            }
            None => self
                .engine_state
                .cwd_as_string(Some(self.stack))
                .map(PathBuf::from)
                .map_err(|err| stone_error("list_dir", err.to_string()))?,
        };
        let entries = list_dir_records(&target)?;
        Ok(RuntimeValue::Nu(Value::list(entries, Span::unknown())))
    }

    fn eval_echo_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "echo",
                "echo() keyword arguments are not supported",
            ));
        }
        let mut values = Vec::with_capacity(call.positional.len());
        for argument in &call.positional {
            values.push(
                self.eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value("echo")?,
            );
        }
        Ok(RuntimeValue::Nu(match values.as_slice() {
            [] => Value::nothing(Span::unknown()),
            [value] => value.clone(),
            _ => Value::list(values, Span::unknown()),
        }))
    }

    fn eval_emit_call(
        &mut self,
        call: &Call,
        input: PipelineData,
    ) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "emit",
                "emit() keyword arguments are not supported",
            ));
        }
        match call.positional.as_slice() {
            [] => input
                .into_value(Span::unknown())
                .map(RuntimeValue::Nu)
                .map_err(|err| stone_error("emit", err.to_string())),
            [value] => self.eval_expr_value(value, PipelineData::empty()),
            _ => Err(stone_error("emit", "emit() takes at most one value")),
        }
    }

    fn eval_fail_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [message] = call.positional.as_slice() else {
            return Err(stone_error("fail", "fail() requires exactly one message"));
        };
        let message = self
            .eval_expr_value(message, PipelineData::empty())?
            .into_nu_value("fail")?;
        let message = value_to_string(&message, "fail")?;
        let mut code = None;
        let mut detail = None;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("fail")?;
            match name.as_str() {
                "code" => code = Some(value_to_string(&value, "fail code")?),
                "detail" => detail = Some(value),
                other => {
                    return Err(stone_error(
                        "fail",
                        format!("unsupported keyword `{other}`; expected code or detail"),
                    ));
                }
            }
        }

        let mut error =
            GenericError::new("Task failure", message, Span::unknown()).with_code("task_failure");
        if let Some(code) = code {
            error = error.with_help(format!("code={code}"));
        }
        if let Some(detail) = detail {
            error = error.with_inner(vec![ShellError::Generic(
                GenericError::new_internal(
                    "Task failure detail",
                    nu_to_json_value(&detail).to_string(),
                )
                .with_code("task_failure_detail"),
            )]);
        }
        Err(ShellError::Generic(error))
    }

    fn eval_help_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "help",
                "help() keyword arguments are not supported",
            ));
        }
        let value = match call.positional.as_slice() {
            [] => stone_help_overview(Span::unknown()),
            [name] => {
                let name = self
                    .eval_expr_value(name, PipelineData::empty())?
                    .into_nu_value("help")?;
                stone_help_topic(&value_to_string(&name, "help")?, Span::unknown())
            }
            _ => return Err(stone_error("help", "help() takes at most one name")),
        };
        Ok(RuntimeValue::Nu(value))
    }

    fn eval_pwd_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.positional.is_empty() || !call.named.is_empty() {
            return Err(stone_error("pwd", "pwd() takes no arguments"));
        }
        let cwd = self
            .engine_state
            .cwd_as_string(Some(self.stack))
            .map_err(|err| stone_error("pwd", err.to_string()))?;
        Ok(RuntimeValue::Nu(Value::string(cwd, Span::unknown())))
    }

    fn eval_cd_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [path] = call.positional.as_slice() else {
            return Err(stone_error("cd", "cd() requires exactly one path"));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "cd",
                "cd() keyword arguments are not supported",
            ));
        }
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("cd")?;
        let path = value_to_path_string(&path, "cd")?;
        let target = self.resolve_script_path(&path)?;
        let target =
            std::fs::canonicalize(&target).map_err(|err| io_stone_error("cd", err, &target))?;
        if !target.is_dir() {
            return Err(stone_error(
                "cd",
                format!("cwd is not a directory: {}", target.display()),
            ));
        }
        self.stack
            .set_cwd(&target)
            .map_err(|err| stone_error("cd", err.to_string()))?;
        Ok(RuntimeValue::Nu(Value::string(
            target.display().to_string(),
            Span::unknown(),
        )))
    }

    fn eval_find_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [root, rest @ ..] = call.positional.as_slice() else {
            return Err(stone_error(
                "find",
                "find() requires root and optional name_glob arguments",
            ));
        };
        if rest.len() > 1 {
            return Err(stone_error(
                "find",
                "find() takes at most two positional arguments",
            ));
        }

        let name_glob = rest
            .first()
            .map(|expr| {
                self.eval_expr_value(expr, PipelineData::empty())
                    .and_then(|value| value.into_nu_value("find"))
                    .and_then(|value| value_to_string(&value, "find"))
            })
            .transpose()?;
        let mut options = StoneFindOptions {
            name_glob,
            ..StoneFindOptions::default()
        };
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("find")?;
            match name.as_str() {
                "name_glob" => options.name_glob = Some(value_to_string(&value, "find name_glob")?),
                "name_contains" => {
                    options.name_contains = Some(value_to_string(&value, "find name_contains")?)
                }
                "path_glob" => options.path_glob = Some(value_to_string(&value, "find path_glob")?),
                "type" => {
                    let kind = value_to_string(&value, "find type")?;
                    if !matches!(kind.as_str(), "file" | "dir" | "symlink" | "any") {
                        return Err(stone_error(
                            "find",
                            "type must be one of 'file', 'dir', 'symlink', or 'any'",
                        ));
                    }
                    options.kind_filter = Some(kind);
                }
                "min_size" => options.min_size = Some(value_to_u64(&value, "find min_size")?),
                "max_size" => options.max_size = Some(value_to_u64(&value, "find max_size")?),
                "modified_after_ms" => {
                    options.modified_after_ms =
                        Some(value_to_i64(&value, "find modified_after_ms")?)
                }
                "modified_before_ms" => {
                    options.modified_before_ms =
                        Some(value_to_i64(&value, "find modified_before_ms")?)
                }
                other => {
                    return Err(stone_error(
                        "find",
                        format!(
                            "unsupported keyword `{other}`; expected name_glob, name_contains, path_glob, type, min_size, max_size, modified_after_ms, or modified_before_ms"
                        ),
                    ));
                }
            }
        }

        let root = self
            .eval_expr_value(root, PipelineData::empty())?
            .into_nu_value("find")?;
        let root = value_to_path_string(&root, "find")?;
        let root = self.resolve_script_path(&root)?;
        let entries = find_records(root, options)?;
        Ok(RuntimeValue::Nu(Value::list(entries, Span::unknown())))
    }

    fn eval_diff_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [left, right] = call.positional.as_slice() else {
            return Err(stone_error("diff", "diff() requires two file paths"));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "diff",
                "diff() keyword arguments are not supported",
            ));
        }
        let left = self
            .eval_expr_value(left, PipelineData::empty())?
            .into_nu_value("diff")?;
        let right = self
            .eval_expr_value(right, PipelineData::empty())?
            .into_nu_value("diff")?;
        let left_path = self.resolve_script_path(&value_to_path_string(&left, "diff")?)?;
        let right_path = self.resolve_script_path(&value_to_path_string(&right, "diff")?)?;
        Ok(RuntimeValue::Nu(diff_record_for_paths(
            &left_path,
            &right_path,
        )?))
    }

    fn eval_read_json_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [path] = call.positional.as_slice() else {
            return Err(stone_error(
                "read_json",
                "read_json() requires exactly one path",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "read_json",
                "read_json() keyword arguments are not supported",
            ));
        }
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("read_json")?;
        let path = value_to_path_string(&path, "read_json")?;
        let target = self.resolve_script_path(&path)?;
        Ok(RuntimeValue::Nu(read_json_file(&target)?))
    }

    fn eval_read_csv_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([path] | [path, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "read_csv",
                "read_csv() requires a path and optional row limit",
            ));
        };
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("read_csv")?;
        let path = value_to_path_string(&path, "read_csv")?;
        let target = self.resolve_script_path(&path)?;
        let limit = self.eval_structured_read_limit("read_csv", call)?;
        Ok(RuntimeValue::Nu(read_csv_file(&target, limit)?))
    }

    fn eval_read_jsonl_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([path] | [path, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "read_jsonl",
                "read_jsonl() requires a path and optional row limit",
            ));
        };
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("read_jsonl")?;
        let path = value_to_path_string(&path, "read_jsonl")?;
        let limit = self.eval_structured_read_limit("read_jsonl", call)?;
        let rows = self.read_jsonl_rows_from_path_with_limit(&path, limit, "read_jsonl")?;
        Ok(RuntimeValue::JsonlRows(rows))
    }

    fn read_jsonl_rows_from_path(
        &self,
        path: &str,
        context: &'static str,
    ) -> Result<JsonlRows, ShellError> {
        self.read_jsonl_rows_from_path_with_limit(path, None, context)
    }

    fn read_jsonl_rows_from_path_with_limit(
        &self,
        path: &str,
        limit: Option<usize>,
        context: &'static str,
    ) -> Result<JsonlRows, ShellError> {
        let target = self.resolve_script_path(path)?;
        let bytes = read_bytes_for_jsonl(&target, context)?;
        Ok(jsonl_rows_from_bytes(
            bytes,
            limit,
            target.display().to_string(),
        ))
    }

    fn eval_structured_read_limit(
        &mut self,
        name: &str,
        call: &Call,
    ) -> Result<Option<usize>, ShellError> {
        let positional_limit = call.positional.get(1);
        let mut named_limit = None;
        for (keyword, argument) in &call.named {
            match keyword.as_str() {
                "limit" if named_limit.is_none() => named_limit = Some(argument),
                "limit" => {
                    return Err(stone_error(
                        name,
                        format!("{name}() got duplicate limit keyword"),
                    ));
                }
                other => {
                    return Err(stone_error(
                        name,
                        format!("unsupported keyword `{other}`; expected limit"),
                    ));
                }
            }
        }
        if positional_limit.is_some() && named_limit.is_some() {
            return Err(stone_error(
                name,
                format!("{name}() limit was provided twice"),
            ));
        }
        let Some(limit) = positional_limit.or(named_limit) else {
            return Ok(None);
        };
        let limit = self
            .eval_expr_value(limit, PipelineData::empty())?
            .into_nu_value(name)?;
        Ok(Some(value_to_limit(&limit, name)?))
    }

    fn eval_write_json_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [path, value] = call.positional.as_slice() else {
            return Err(stone_error(
                "write_json",
                "write_json() requires path and value arguments",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "write_json",
                "write_json() keyword arguments are not supported",
            ));
        }
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("write_json")?;
        let path = value_to_string(&path, "write_json")?;
        let target = self.resolve_script_path(&path)?;
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("write_json")?;
        Ok(RuntimeValue::Nu(write_json_file(&target, &value)?))
    }

    fn eval_write_jsonl_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [path, rows] = call.positional.as_slice() else {
            return Err(stone_error(
                "write_jsonl",
                "write_jsonl() requires path and rows arguments",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "write_jsonl",
                "write_jsonl() keyword arguments are not supported",
            ));
        }
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("write_jsonl")?;
        let path = value_to_string(&path, "write_jsonl")?;
        let target = self.resolve_script_path(&path)?;
        let rows = self
            .eval_expr_value(rows, PipelineData::empty())?
            .into_nu_value("write_jsonl")?;
        let Value::List { vals, .. } = rows else {
            return Err(stone_error(
                "write_jsonl",
                format!("expected list, got {}", rows.get_type()),
            ));
        };
        Ok(RuntimeValue::Nu(write_jsonl_file(&target, vals)?))
    }

    fn eval_first_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let name = call.name.as_str();
        let ([values] | [values, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                name,
                format!("{name}() requires a list and optional count"),
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                name,
                format!("{name}() keyword arguments are not supported"),
            ));
        }
        let values = self
            .eval_expr_value(values, PipelineData::empty())?
            .into_nu_value(name)?;
        let count = match call.positional.get(1) {
            Some(count) => {
                let count = self
                    .eval_expr_value(count, PipelineData::empty())?
                    .into_nu_value(name)?;
                Some(value_to_limit(&count, name)?)
            }
            None => None,
        };
        first_builtin(&values, count, name).map(RuntimeValue::Nu)
    }

    fn eval_last_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let name = call.name.as_str();
        let ([values] | [values, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                name,
                format!("{name}() requires a list and optional count"),
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                name,
                format!("{name}() keyword arguments are not supported"),
            ));
        }
        let values = self
            .eval_expr_value(values, PipelineData::empty())?
            .into_nu_value(name)?;
        let count = match call.positional.get(1) {
            Some(count) => {
                let count = self
                    .eval_expr_value(count, PipelineData::empty())?
                    .into_nu_value(name)?;
                Some(value_to_limit(&count, name)?)
            }
            None => None,
        };
        last_builtin(&values, count, name).map(RuntimeValue::Nu)
    }

    fn eval_mkdir_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if call.positional.is_empty() {
            return Err(stone_error("mkdir", "mkdir() requires at least one path"));
        }
        if !call.named.is_empty() {
            return Err(stone_error(
                "mkdir",
                "mkdir() keyword arguments are not supported",
            ));
        }
        for argument in &call.positional {
            let path = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("mkdir")?;
            let path = value_to_path_string(&path, "mkdir")?;
            let target = self.resolve_script_path(&path)?;
            create_dir_all(&target)?;
        }
        Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())))
    }

    fn eval_rm_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if call.positional.is_empty() {
            return Err(stone_error("rm", "rm() requires at least one path"));
        }
        if !call.named.is_empty() {
            return Err(stone_error(
                "rm",
                "rm() keyword arguments are not supported",
            ));
        }
        for argument in &call.positional {
            let path = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("rm")?;
            let path = value_to_path_string(&path, "rm")?;
            let target = self.resolve_script_path(&path)?;
            remove_path(&target)?;
        }
        Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())))
    }

    fn eval_edit_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [path, old, new] = call.positional.as_slice() else {
            return Err(stone_error(
                "edit",
                "edit() requires path, old, and new arguments",
            ));
        };
        let mut replace_all = false;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("edit")?;
            match name.as_str() {
                "all" => replace_all = value_to_bool(&value, "edit all")?,
                other => {
                    return Err(stone_error(
                        "edit",
                        format!("unsupported keyword `{other}`; expected all"),
                    ));
                }
            }
        }
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("edit")?;
        let old = self
            .eval_expr_value(old, PipelineData::empty())?
            .into_nu_value("edit")?;
        let new = self
            .eval_expr_value(new, PipelineData::empty())?
            .into_nu_value("edit")?;
        let path = value_to_path_string(&path, "edit")?;
        let old = value_to_string(&old, "edit old")?;
        let new = value_to_string(&new, "edit new")?;
        let target = self.resolve_script_path(&path)?;
        Ok(RuntimeValue::Nu(edit_text_file(
            &target,
            &old,
            &new,
            replace_all,
        )?))
    }

    fn eval_save_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [value, path] = call.positional.as_slice() else {
            return Err(stone_error(
                "save",
                "save() requires value and path arguments; use save(value, path, force=True)",
            ));
        };
        let mut append = false;
        let mut force = false;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("save")?;
            match name.as_str() {
                "append" => append = value_to_bool(&value, "save append")?,
                "force" => force = value_to_bool(&value, "save force")?,
                other => {
                    return Err(stone_error(
                        "save",
                        format!("unsupported keyword `{other}`; expected append or force"),
                    ));
                }
            }
        }
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("save")?;
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("save")?;
        let path = value_to_path_string(&path, "save")?;
        let target = self.resolve_script_path(&path)?;
        Ok(RuntimeValue::Nu(save_value_file(
            &target, &value, append, force,
        )?))
    }

    fn eval_search_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [root, needle] = call.positional.as_slice() else {
            return Err(stone_error(
                "search",
                "search() requires root and needle arguments",
            ));
        };
        let mut regex = false;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("search")?;
            match name.as_str() {
                "regex" => regex = value_to_bool(&value, "search regex")?,
                other => {
                    return Err(stone_error(
                        "search",
                        format!("unsupported keyword `{other}`; expected regex"),
                    ));
                }
            }
        }
        let root = self
            .eval_expr_value(root, PipelineData::empty())?
            .into_nu_value("search")?;
        let needle = self
            .eval_expr_value(needle, PipelineData::empty())?
            .into_nu_value("search")?;
        let root = value_to_path_string(&root, "search")?;
        let needle = value_to_string(&needle, "search needle")?;
        if needle.is_empty() {
            return Err(stone_error("search", "needle must not be empty"));
        }
        let root = self.resolve_script_path(&root)?;
        let matches = search_records(root, &needle, regex)?;
        Ok(RuntimeValue::Nu(Value::list(matches, Span::unknown())))
    }

    fn eval_where_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "where",
                "where() keyword arguments are not supported",
            ));
        }
        match call.positional.as_slice() {
            [values, predicate] => {
                let values = self
                    .eval_expr_value(values, PipelineData::empty())?
                    .into_nu_value("where")?;
                let predicate = self.eval_callable_expr(predicate)?;
                self.eval_where_predicate(&values, &predicate)
            }
            [values, key, expected] => {
                let values = self
                    .eval_expr_value(values, PipelineData::empty())?
                    .into_nu_value("where")?;
                let key = self
                    .eval_expr_value(key, PipelineData::empty())?
                    .into_nu_value("where")?;
                let expected = self
                    .eval_expr_value(expected, PipelineData::empty())?
                    .into_nu_value("where")?;
                where_builtin(&values, &key, &expected).map(RuntimeValue::Nu)
            }
            [values, key, op, expected] => {
                let values = self
                    .eval_expr_value(values, PipelineData::empty())?
                    .into_nu_value("where")?;
                let key = self
                    .eval_expr_value(key, PipelineData::empty())?
                    .into_nu_value("where")?;
                let op = self
                    .eval_expr_value(op, PipelineData::empty())?
                    .into_nu_value("where")?;
                let expected = self
                    .eval_expr_value(expected, PipelineData::empty())?
                    .into_nu_value("where")?;
                where_compare_builtin(&values, &key, &op, &expected).map(RuntimeValue::Nu)
            }
            _ => Err(stone_error(
                "where",
                "where() requires rows plus a predicate, or rows, key, and expected arguments",
            )),
        }
    }

    fn eval_where_predicate(
        &mut self,
        values: &Value,
        predicate: &CallableValue,
    ) -> Result<RuntimeValue, ShellError> {
        let Value::List { vals, .. } = values else {
            return Err(stone_error(
                "where",
                format!("expected list, got {}", values.get_type()),
            ));
        };
        let mut selected = Vec::new();
        for value in vals {
            let keep = self
                .invoke_callable(predicate, vec![RuntimeValue::Nu(value.clone())])?
                .into_nu_value("where predicate")?;
            if value_truthy(&keep) {
                selected.push(value.clone());
            }
        }
        Ok(RuntimeValue::Nu(Value::list(selected, Span::unknown())))
    }

    fn eval_from_json_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [text] = call.positional.as_slice() else {
            return Err(stone_error(
                "from_json",
                "from_json() requires exactly one string",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "from_json",
                "from_json() keyword arguments are not supported",
            ));
        }
        let text = self
            .eval_expr_value(text, PipelineData::empty())?
            .into_nu_value("from_json")?;
        let text = value_to_string(&text, "from_json")?;
        let json = serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|err| stone_error("from_json", err.to_string()))?;
        Ok(RuntimeValue::Nu(json_to_nu_value(json, Span::unknown())))
    }

    fn eval_to_json_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [value] = call.positional.as_slice() else {
            return Err(stone_error(
                "to_json",
                "to_json() requires exactly one value",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "to_json",
                "to_json() keyword arguments are not supported",
            ));
        }
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("to_json")?;
        let text = serde_json::to_string(&nu_to_json_value(&value))
            .map_err(|err| stone_error("to_json", err.to_string()))?;
        Ok(RuntimeValue::Nu(Value::string(text, Span::unknown())))
    }

    fn eval_to_jsonl_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [value] = call.positional.as_slice() else {
            return Err(stone_error(
                "to_jsonl",
                "to_jsonl() requires exactly one value",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "to_jsonl",
                "to_jsonl() keyword arguments are not supported",
            ));
        }
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("to_jsonl")?;
        let mut text = String::new();
        match value {
            Value::List { vals, .. } => {
                for value in vals {
                    text.push_str(
                        &serde_json::to_string(&nu_to_json_value(&value))
                            .map_err(|err| stone_error("to_jsonl", err.to_string()))?,
                    );
                    text.push('\n');
                }
            }
            value => {
                text.push_str(
                    &serde_json::to_string(&nu_to_json_value(&value))
                        .map_err(|err| stone_error("to_jsonl", err.to_string()))?,
                );
                text.push('\n');
            }
        }
        Ok(RuntimeValue::Nu(Value::string(text, Span::unknown())))
    }

    fn eval_json_loads_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [text] = call.positional.as_slice() else {
            return Err(stone_error(
                "json_loads",
                "json_loads() requires exactly one string",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "json_loads",
                "json_loads() keyword arguments are not supported",
            ));
        }
        let text = self
            .eval_expr_value(text, PipelineData::empty())?
            .into_nu_value("json_loads")?;
        let text = value_to_string(&text, "json_loads")?;
        let json = serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|err| stone_error("json_loads", err.to_string()))?;
        Ok(RuntimeValue::Nu(json_to_nu_value(json, Span::unknown())))
    }

    fn eval_json_dumps_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [value] = call.positional.as_slice() else {
            return Err(stone_error(
                "json_dumps",
                "json_dumps() requires exactly one value",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "json_dumps",
                "json_dumps() keyword arguments are not supported",
            ));
        }
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("json_dumps")?;
        let text = serde_json::to_string(&nu_to_json_value(&value))
            .map_err(|err| stone_error("json_dumps", err.to_string()))?;
        Ok(RuntimeValue::Nu(Value::string(text, Span::unknown())))
    }

    fn eval_sort_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([values] | [values, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "sort",
                "sort() requires a list and optional key field",
            ));
        };
        let values = self
            .eval_expr_value(values, PipelineData::empty())?
            .into_nu_value("sort")?;
        let Value::List { vals, .. } = values else {
            return Err(stone_error(
                "sort",
                format!("expected list, got {}", values.get_type()),
            ));
        };

        let mut key = SortKey::Identity;
        let mut reverse = false;
        if let Some(positional_key) = call.positional.get(1) {
            key = self.eval_sort_key(positional_key)?;
        }
        for (name, argument) in &call.named {
            match name.as_str() {
                "key" => {
                    if call.positional.len() > 1 {
                        return Err(stone_error(
                            "sort",
                            "sort() key was provided both positionally and by keyword",
                        ));
                    }
                    key = self.eval_sort_key(argument)?;
                }
                "reverse" => {
                    let value = self
                        .eval_expr_value(argument, PipelineData::empty())?
                        .into_nu_value("sort")?;
                    reverse = value_to_bool(&value, "sort reverse")?;
                }
                other => {
                    return Err(stone_error(
                        "sort",
                        format!("unsupported keyword `{other}`; expected key or reverse"),
                    ));
                }
            }
        }

        sort_builtin_values(vals, reverse, |value| match &key {
            SortKey::Callable(callable) => self
                .invoke_callable(callable, vec![RuntimeValue::Nu(value.clone())])?
                .into_nu_value("sort key"),
            SortKey::Identity => sort_key_for_value(value, None),
            SortKey::Field(field) => sort_key_for_value(value, Some(field)),
        })
        .map(RuntimeValue::Nu)
    }

    fn eval_sort_key(&mut self, expression: &Expr) -> Result<SortKey, ShellError> {
        match self.eval_expr_value(expression, PipelineData::empty())? {
            RuntimeValue::Callable(callable) => Ok(SortKey::Callable(callable)),
            other => {
                let value = other.into_nu_value("sort")?;
                value_to_string(&value, "sort key").map(SortKey::Field)
            }
        }
    }

    fn eval_set_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([] | [_]) = call.positional.as_slice() else {
            return Err(stone_error("set", "set() takes an optional iterable"));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "set",
                "set() keyword arguments are not supported",
            ));
        }
        let values = match call.positional.as_slice() {
            [] => Vec::new(),
            [iterable] => self.eval_iterable_expr(iterable)?,
            _ => unreachable!(),
        };
        let values = values
            .into_iter()
            .map(|value| value.into_nu_value("set"))
            .collect::<Result<Vec<_>, _>>()?;
        set_builtin(values).map(RuntimeValue::Nu)
    }

    fn eval_unique_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [values] = call.positional.as_slice() else {
            return Err(stone_error("unique", "unique() requires exactly one list"));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "unique",
                "unique() keyword arguments are not supported",
            ));
        }
        let values = self
            .eval_expr_value(values, PipelineData::empty())?
            .into_nu_value("unique")?;
        unique_builtin(&values).map(RuntimeValue::Nu)
    }

    fn eval_round_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([value] | [value, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "round",
                "round() requires a number and optional digits",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "round",
                "round() keyword arguments are not supported",
            ));
        }
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("round")?;
        let digits = match call.positional.as_slice() {
            [_] => 0,
            [_, digits] => {
                let digits = self
                    .eval_expr_value(digits, PipelineData::empty())?
                    .into_nu_value("round")?;
                value_to_i64(&digits, "round")?
            }
            _ => unreachable!(),
        };
        round_builtin(&value, digits).map(RuntimeValue::Nu)
    }

    fn eval_parse_float_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [value, default] = call.positional.as_slice() else {
            return Err(stone_error(
                "parse_float",
                "parse_float() requires value and default arguments",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "parse_float",
                "parse_float() keyword arguments are not supported",
            ));
        }
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("parse_float")?;
        let default = self
            .eval_expr_value(default, PipelineData::empty())?
            .into_nu_value("parse_float")?;
        parse_float_builtin(&value, default).map(RuntimeValue::Nu)
    }

    fn eval_parse_int_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [value, default] = call.positional.as_slice() else {
            return Err(stone_error(
                "parse_int",
                "parse_int() requires value and default arguments",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "parse_int",
                "parse_int() keyword arguments are not supported",
            ));
        }
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("parse_int")?;
        let default = self
            .eval_expr_value(default, PipelineData::empty())?
            .into_nu_value("parse_int")?;
        parse_int_builtin(&value, default).map(RuntimeValue::Nu)
    }

    fn eval_record_helper_call(
        &mut self,
        call: &Call,
        input: PipelineData,
    ) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                &call.name,
                format!("{}() keyword arguments are not supported", call.name),
            ));
        }
        let (record, arg_exprs) = if call.name == "get" && call.positional.len() == 1 {
            let record = input
                .into_value(Span::unknown())
                .map_err(|err| stone_error("get", err.to_string()))?;
            (record, call.positional.as_slice())
        } else {
            let ([record] | [record, _] | [record, _, _]) = call.positional.as_slice() else {
                return Err(stone_error(
                    &call.name,
                    format!("{}() requires a record argument", call.name),
                ));
            };
            let record = self
                .eval_expr_value(record, PipelineData::empty())?
                .into_nu_value(&call.name)?;
            (record, &call.positional[1..])
        };
        let mut args = Vec::new();
        for arg in arg_exprs {
            args.push(
                self.eval_expr_value(arg, PipelineData::empty())?
                    .into_nu_value(&call.name)?,
            );
        }
        record_method_builtin(&record, &call.name, &args).map(RuntimeValue::Nu)
    }

    #[cfg(not(target_os = "hermit"))]
    fn eval_call_values(
        &mut self,
        call: &Call,
    ) -> Result<(Vec<Value>, Vec<(String, Value)>), ShellError> {
        let mut positional = Vec::with_capacity(call.positional.len());
        for argument in &call.positional {
            positional.push(
                self.eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value(&call.name)?,
            );
        }
        let mut named = Vec::with_capacity(call.named.len());
        for (name, argument) in &call.named {
            named.push((
                name.clone(),
                self.eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value(&call.name)?,
            ));
        }
        Ok((positional, named))
    }

    #[cfg(not(target_os = "hermit"))]
    fn current_cwd_path(&mut self, context: &str) -> Result<PathBuf, ShellError> {
        self.engine_state
            .cwd_as_string(Some(self.stack))
            .map(PathBuf::from)
            .map_err(|err| stone_error(context, err.to_string()))
    }

    fn eval_run_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        #[cfg(target_os = "hermit")]
        {
            let _ = call;
            return Err(stone_error(
                "run",
                "run() requires the Linux/POSIX adapter and is unavailable on Hermit",
            ));
        }

        #[cfg(not(target_os = "hermit"))]
        {
            let (positional, named) = self.eval_call_values(call)?;
            let default_cwd = self.current_cwd_path("run")?;
            let invocation = run_call_values(&positional, &named, default_cwd, |path| {
                self.resolve_script_path(path)
            })?;
            let mut record = invocation.record;
            self.attach_run_helper_observations(
                &mut record,
                &invocation.argv,
                &invocation.cwd,
                &invocation.env_overrides,
                Span::unknown(),
            );
            Ok(RuntimeValue::Nu(Value::record(record, Span::unknown())))
        }
    }

    #[cfg(not(target_os = "hermit"))]
    fn attach_run_helper_observations(
        &mut self,
        record: &mut Record,
        argv: &[String],
        cwd: &Path,
        env_overrides: &[(String, String)],
        span: Span,
    ) {
        let event = stone_run_event_from_record(
            record,
            argv,
            cwd,
            env_overrides,
            &self.state.stone_helper_registry,
        );
        let hooks: Vec<StoneHelperHook> = self
            .state
            .stone_helper_registry
            .matching_hooks(&event)
            .into_iter()
            .cloned()
            .collect();
        let mut helpers = Vec::new();
        for hook in hooks {
            let result = match &hook.handler.kind {
                StoneHelperHandlerKind::StoneFunction {
                    function,
                    functions,
                } => self.invoke_stone_helper_function(function, functions, &event, span),
                StoneHelperHandlerKind::Registered => Ok(Vec::new()),
            };
            match result {
                Ok(mut observations) => helpers.append(&mut observations),
                Err(err) => helpers.push(helper_error_observation(&hook, &event, err, span)),
            }
        }
        if !helpers.is_empty() {
            record.push("helpers", Value::list(helpers, span));
        }
    }

    #[cfg(not(target_os = "hermit"))]
    fn invoke_stone_helper_function(
        &mut self,
        function: &FunctionDef,
        functions: &HashMap<String, FunctionDef>,
        event: &StoneRunEvent<'_>,
        span: Span,
    ) -> Result<Vec<Value>, ShellError> {
        if function.params.len() != 1 {
            return Err(stone_error(
                "helper",
                format!(
                    "{}() expected one run event argument, got {} parameter(s)",
                    function.name,
                    function.params.len()
                ),
            ));
        }
        let event_value = stone_run_event_value(event, span);
        ensure_type(
            &event_value,
            function.params[0].ty,
            &format!("argument `{}`", function.params[0].name),
        )?;

        let previous_functions = self.state.functions.clone();
        for (name, function) in functions {
            self.state.functions.insert(name.clone(), function.clone());
        }
        self.state.push_scope();
        self.state.set_local(
            function.params[0].name.clone(),
            RuntimeValue::Nu(event_value),
        );
        let flow = self.eval_block(&function.body, PipelineData::empty(), false);
        let pop_result = self.state.pop_scope();
        self.state.functions = previous_functions;
        pop_result?;
        let result = match flow? {
            EvalFlow::Return(value) => {
                ensure_type(
                    &value,
                    function.return_type,
                    &format!("{}() return value", function.name),
                )?;
                stone_helper_observations_from_value(value)
            }
            EvalFlow::Output(_) => stone_helper_observations_from_value(Value::nothing(span)),
            EvalFlow::Break => Err(stone_error("break", "break outside loop")),
            EvalFlow::Continue => Err(stone_error("continue", "continue outside loop")),
        };
        result
    }

    fn eval_resolve_command_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        #[cfg(target_os = "hermit")]
        {
            let _ = call;
            return Err(stone_error(
                "resolve_command",
                "resolve_command() requires the Linux/POSIX adapter and is unavailable on Hermit",
            ));
        }

        #[cfg(not(target_os = "hermit"))]
        {
            let (positional, named) = self.eval_call_values(call)?;
            resolve_command_call_values(&positional, &named).map(RuntimeValue::Nu)
        }
    }

    fn eval_state_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.positional.is_empty() || !call.named.is_empty() {
            return Err(stone_error("state", "state() takes no arguments"));
        }
        let cwd = self
            .engine_state
            .cwd_as_string(Some(self.stack))
            .map_err(|err| stone_error("state", err.to_string()))?;
        Ok(RuntimeValue::Nu(runtime_state_record(Path::new(&cwd))))
    }

    fn eval_last_result_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.positional.is_empty() || !call.named.is_empty() {
            return Err(stone_error(
                "last_result",
                "last_result() takes no arguments",
            ));
        }
        let span = Span::unknown();
        let Some(value) = self
            .stack
            .get_env_var(self.engine_state, STONE_LAST_RESULT_ENV)
        else {
            return Ok(RuntimeValue::Nu(Value::nothing(span)));
        };
        let text = value_to_string(&value, "last_result")?;
        let parsed: JsonValue = serde_json::from_str(&text).map_err(|err| {
            stone_error(
                "last_result",
                format!("stored previous result was not valid JSON: {err}"),
            )
        })?;
        Ok(RuntimeValue::Nu(json_to_nu_value(parsed, span)))
    }

    fn eval_start_daemon_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        #[cfg(target_os = "hermit")]
        {
            let _ = call;
            return Err(stone_error(
                "start_daemon",
                "start_daemon() requires the Linux/POSIX adapter and is unavailable on Hermit",
            ));
        }

        #[cfg(not(target_os = "hermit"))]
        {
            let (positional, named) = self.eval_call_values(call)?;
            let default_cwd = self.current_cwd_path("start_daemon")?;
            start_daemon_call_values(&positional, &named, default_cwd, |path| {
                self.resolve_script_path(path)
            })
            .map(RuntimeValue::Nu)
        }
    }

    fn eval_daemon_status_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        #[cfg(target_os = "hermit")]
        {
            let _ = call;
            return Err(stone_error(
                "daemon_status",
                "daemon_status() requires the Linux/POSIX adapter and is unavailable on Hermit",
            ));
        }

        #[cfg(not(target_os = "hermit"))]
        {
            let (positional, named) = self.eval_call_values(call)?;
            daemon_status_call_values(&positional, &named, |path| self.resolve_script_path(path))
                .map(RuntimeValue::Nu)
        }
    }

    fn eval_stop_daemon_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        #[cfg(target_os = "hermit")]
        {
            let _ = call;
            return Err(stone_error(
                "stop_daemon",
                "stop_daemon() requires the Linux/POSIX adapter and is unavailable on Hermit",
            ));
        }

        #[cfg(not(target_os = "hermit"))]
        {
            let (positional, named) = self.eval_call_values(call)?;
            stop_daemon_call_values(&positional, &named).map(RuntimeValue::Nu)
        }
    }

    fn eval_wait_port_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        #[cfg(target_os = "hermit")]
        {
            let _ = call;
            return Err(stone_error(
                "wait_port",
                "wait_port() requires the Linux/POSIX adapter and is unavailable on Hermit",
            ));
        }

        #[cfg(not(target_os = "hermit"))]
        {
            let (positional, named) = self.eval_call_values(call)?;
            wait_port_call_values(&positional, &named).map(RuntimeValue::Nu)
        }
    }

    fn eval_list_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "list",
                "list() keyword arguments are not supported",
            ));
        }
        let [value] = call.positional.as_slice() else {
            return Err(stone_error("list", "list() requires exactly one argument"));
        };
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("list")?;
        list_builtin(&value).map(RuntimeValue::Nu)
    }

    fn eval_type_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [arg] = call.positional.as_slice() else {
            return Err(stone_error("type", "type() requires exactly one argument"));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "type",
                "type() keyword arguments are not supported",
            ));
        }
        let value = self.eval_expr_value(arg, PipelineData::empty())?;
        Ok(RuntimeValue::Nu(Value::string(
            runtime_type_name(&value),
            Span::unknown(),
        )))
    }

    fn eval_min_max_call(
        &mut self,
        call: &Call,
        operation: MinMax,
    ) -> Result<RuntimeValue, ShellError> {
        if call.positional.is_empty() {
            return Err(stone_error(
                operation.name(),
                format!(
                    "{}() requires at least one numeric argument",
                    operation.name()
                ),
            ));
        }
        if !call.named.is_empty() {
            return Err(stone_error(
                operation.name(),
                format!("{}() keyword arguments are not supported", operation.name()),
            ));
        }

        let mut values = Vec::with_capacity(call.positional.len());
        for argument in &call.positional {
            values.push(
                self.eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value(operation.name())?,
            );
        }
        min_max_builtin(values, operation.name()).map(RuntimeValue::Nu)
    }

    fn eval_split_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "split",
                "split() keyword arguments are not supported",
            ));
        }
        let ([text] | [text, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "split",
                "split() requires text and optional separator",
            ));
        };
        let text = self
            .eval_expr_value(text, PipelineData::empty())?
            .into_nu_value("split")?;
        let separator = match call.positional.as_slice() {
            [_] => None,
            [_, separator] => {
                let separator = self
                    .eval_expr_value(separator, PipelineData::empty())?
                    .into_nu_value("split")?;
                Some(separator)
            }
            _ => unreachable!(),
        };
        split_builtin(&text, separator.as_ref()).map(RuntimeValue::Nu)
    }

    fn eval_join_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "join",
                "join() keyword arguments are not supported",
            ));
        }
        let ([items] | [items, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "join",
                "join() requires items and optional separator",
            ));
        };
        let first = self
            .eval_expr_value(items, PipelineData::empty())?
            .into_nu_value("join")?;
        let second = match call.positional.as_slice() {
            [_] => None,
            [_, second] => {
                let second = self
                    .eval_expr_value(second, PipelineData::empty())?
                    .into_nu_value("join")?;
                Some(second)
            }
            _ => unreachable!(),
        };
        join_builtin(&first, second.as_ref()).map(RuntimeValue::Nu)
    }

    fn eval_slice_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "slice",
                "slice() keyword arguments are not supported",
            ));
        }
        let ([value] | [value, _] | [value, _, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "slice",
                "slice() requires value, optional start, and optional end",
            ));
        };
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("slice")?;
        let start = call
            .positional
            .get(1)
            .map(|expr| self.eval_optional_slice_arg(expr, "slice start"))
            .transpose()?
            .flatten();
        let end = call
            .positional
            .get(2)
            .map(|expr| self.eval_optional_slice_arg(expr, "slice end"))
            .transpose()?
            .flatten();
        slice_builtin(&value, start, end).map(RuntimeValue::Nu)
    }

    fn eval_optional_slice_arg(
        &mut self,
        expression: &Expr,
        context: &str,
    ) -> Result<Option<i64>, ShellError> {
        let value = self
            .eval_expr_value(expression, PipelineData::empty())?
            .into_nu_value(context)?;
        if matches!(value, Value::Nothing { .. }) {
            Ok(None)
        } else {
            value_to_i64(&value, context).map(Some)
        }
    }

    fn eval_starts_with_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "starts_with",
                "starts_with() keyword arguments are not supported",
            ));
        }
        let [text, prefix] = call.positional.as_slice() else {
            return Err(stone_error(
                "starts_with",
                "starts_with() requires text and prefix",
            ));
        };
        let text = self
            .eval_expr_value(text, PipelineData::empty())?
            .into_nu_value("starts_with")?;
        let prefix = self
            .eval_expr_value(prefix, PipelineData::empty())?
            .into_nu_value("starts_with")?;
        starts_with_builtin(&text, &prefix).map(RuntimeValue::Nu)
    }

    fn eval_format_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "format",
                "format() keyword arguments are not supported",
            ));
        }
        let Some(template) = call.positional.first() else {
            return Err(stone_error("format", "format() requires a template string"));
        };
        let template = self
            .eval_expr_value(template, PipelineData::empty())?
            .into_nu_value("format")?;
        let mut args = Vec::with_capacity(call.positional.len().saturating_sub(1));
        for arg in call.positional.iter().skip(1) {
            args.push(
                self.eval_expr_value(arg, PipelineData::empty())?
                    .into_nu_value("format")?,
            );
        }
        format_builtin(&template, &args).map(RuntimeValue::Nu)
    }

    fn eval_print_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [arg] = call.positional.as_slice() else {
            return Err(stone_error(
                "print",
                "print() requires exactly one argument. Use string concatenation such as `print(\"Line \" + str(i))`, or emit a list/record such as `emit([\"Line\", i, value])`",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "print",
                "print() keyword arguments are not supported",
            ));
        }
        let value = self
            .eval_expr_value(arg, PipelineData::empty())?
            .into_nu_value("print")?;
        self.state
            .stdout
            .push_str(&value_to_display_string(&value)?);
        self.state.stdout.push('\n');
        Ok(RuntimeValue::Nu(value))
    }

    fn eval_range_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "range",
                "range() keyword arguments are not supported",
            ));
        }
        let args = call
            .positional
            .iter()
            .map(|arg| {
                let value = self
                    .eval_expr_value(arg, PipelineData::empty())?
                    .into_nu_value("range")?;
                value_to_i64(&value, "range")
            })
            .collect::<Result<Vec<_>, _>>()?;
        range_builtin(&args).map(RuntimeValue::Nu)
    }

    fn eval_enumerate_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([iterable] | [iterable, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "enumerate",
                "enumerate() requires an iterable and optional start",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "enumerate",
                "enumerate() keyword arguments are not supported",
            ));
        }
        let values = self.eval_iterable_expr(iterable)?;
        let start = match call.positional.as_slice() {
            [_] => 0,
            [_, start] => {
                let start = self
                    .eval_expr_value(start, PipelineData::empty())?
                    .into_nu_value("enumerate")?;
                value_to_i64(&start, "enumerate")?
            }
            _ => unreachable!(),
        };
        let values = values
            .into_iter()
            .map(|value| value.into_nu_value("enumerate"))
            .collect::<Result<Vec<_>, _>>()?;
        enumerate_builtin(values, start).map(RuntimeValue::Nu)
    }

    fn eval_optional_index(
        &mut self,
        expression: Option<&Expr>,
    ) -> Result<Option<i64>, ShellError> {
        expression
            .map(|expression| {
                let value = self
                    .eval_expr_value(expression, PipelineData::empty())?
                    .into_nu_value("slice")?;
                value_to_i64(&value, "slice")
            })
            .transpose()
    }

    fn eval_file_method(
        &mut self,
        handle: FileHandle,
        method: &str,
        args: &[Value],
    ) -> Result<RuntimeValue, ShellError> {
        match method {
            "read" => {
                let [] = args else {
                    return Err(stone_error("read", "read() takes no arguments for now"));
                };
                match self.state.file_mut(handle)? {
                    RuntimeFile::Read { text, closed, .. } => {
                        if *closed {
                            return Err(stone_error("read", "I/O operation on closed file"));
                        }
                        Ok(RuntimeValue::Nu(Value::string(
                            text.clone(),
                            Span::unknown(),
                        )))
                    }
                    RuntimeFile::Write { .. } => {
                        Err(stone_error("read", "file is not open for reading"))
                    }
                }
            }
            "split" => {
                let [separator] = args else {
                    return Err(stone_error(
                        "split",
                        "file.split() requires an explicit separator",
                    ));
                };
                let separator = value_to_string(separator, "split")?;
                match self.state.file_mut(handle)? {
                    RuntimeFile::Read { text, closed, .. } => {
                        if *closed {
                            return Err(stone_error("split", "I/O operation on closed file"));
                        }
                        Ok(RuntimeValue::Nu(Value::list(
                            text.split(&separator)
                                .map(|part| Value::string(part.to_owned(), Span::unknown()))
                                .collect(),
                            Span::unknown(),
                        )))
                    }
                    RuntimeFile::Write { .. } => {
                        Err(stone_error("split", "file is not open for reading"))
                    }
                }
            }
            "splitlines" => {
                let [] = args else {
                    return Err(stone_error(
                        "splitlines",
                        "splitlines() takes no arguments for now",
                    ));
                };
                match self.state.file_mut(handle)? {
                    RuntimeFile::Read { text, closed, .. } => {
                        if *closed {
                            return Err(stone_error("splitlines", "I/O operation on closed file"));
                        }
                        Ok(RuntimeValue::TextLines(TextLines {
                            lines: text.lines().map(str::to_owned).collect(),
                            source: "open(...).splitlines()".to_owned(),
                        }))
                    }
                    RuntimeFile::Write { .. } => {
                        Err(stone_error("splitlines", "file is not open for reading"))
                    }
                }
            }
            "write" => {
                let [value] = args else {
                    return Err(stone_error(
                        "write",
                        "write() requires exactly one argument",
                    ));
                };
                let text = value_to_string(value, "write")?;
                match self.state.file_mut(handle)? {
                    RuntimeFile::Write { path, file } => {
                        let file = file
                            .as_mut()
                            .ok_or_else(|| stone_error("write", "I/O operation on closed file"))?;
                        file.write_all(text.as_bytes())
                            .map_err(|err| io_stone_error("write", err, path))?;
                        file.flush()
                            .map_err(|err| io_stone_error("write", err, path))?;
                        Ok(RuntimeValue::Nu(Value::int(
                            i64::try_from(text.len()).unwrap_or(i64::MAX),
                            Span::unknown(),
                        )))
                    }
                    RuntimeFile::Read { .. } => {
                        Err(stone_error("write", "file is not open for writing"))
                    }
                }
            }
            "close" => {
                let [] = args else {
                    return Err(stone_error("close", "close() takes no arguments"));
                };
                match self.state.file_mut(handle)? {
                    RuntimeFile::Read { closed, .. } => {
                        *closed = true;
                        Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())))
                    }
                    RuntimeFile::Write { path, file } => {
                        if let Some(mut file) = file.take() {
                            file.flush()
                                .map_err(|err| io_stone_error("close", err, path))?;
                        }
                        Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())))
                    }
                }
            }
            other => Err(file_method_error(other)),
        }
    }

    fn resolve_script_path(&self, path: &str) -> Result<PathBuf, ShellError> {
        let path = Path::new(path);
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        let cwd = self
            .engine_state
            .cwd_as_string(Some(self.stack))
            .map_err(|err| stone_error("path", err.to_string()))?;
        Ok(Path::new(&cwd).join(path))
    }

    fn eval_method_call(
        &mut self,
        receiver: &Expr,
        method: &str,
        positional: &[Expr],
    ) -> Result<RuntimeValue, ShellError> {
        let started = self.state.profiler.start();
        let result = (|| {
            if method == "append" {
                return self.eval_append_call(receiver, positional);
            }
            if method == "add" {
                return self.eval_add_method_call(receiver, positional);
            }

            let receiver = self.eval_expr_value(receiver, PipelineData::empty())?;
            let args = positional
                .iter()
                .map(|arg| {
                    self.eval_expr_value(arg, PipelineData::empty())?
                        .into_nu_value("method call")
                })
                .collect::<Result<Vec<_>, _>>()?;

            if let RuntimeValue::File(handle) = receiver {
                return self.eval_file_method(handle, method, &args);
            }
            if let RuntimeValue::JsonObjectView(view) = receiver {
                return eval_json_object_view_method(&view, method, &args);
            }

            let receiver = receiver.into_nu_value("method call")?;
            match method {
                "strip" | "split" | "splitlines" | "replace" | "join" | "lower" | "upper"
                | "zfill" | "startswith" | "endswith" => {
                    string_method_builtin(&receiver, method, &args).map(RuntimeValue::Nu)
                }
                "get" | "items" | "keys" | "values" => {
                    record_method_builtin(&receiver, method, &args).map(RuntimeValue::Nu)
                }
                "find" => find_method_builtin(&receiver, &args).map(RuntimeValue::Nu),
                "index" => index_method_builtin(&receiver, &args).map(RuntimeValue::Nu),
                other => Err(stone_error(
                    "method call",
                    format!("unsupported method `{other}`"),
                )),
            }
        })();
        self.state
            .profiler
            .finish(EvalProfileBucket::MethodCall, started);
        result
    }

    fn eval_append_call(
        &mut self,
        receiver: &Expr,
        positional: &[Expr],
    ) -> Result<RuntimeValue, ShellError> {
        let [arg] = positional else {
            return Err(stone_error(
                "append",
                "append() requires exactly one argument",
            ));
        };
        let value = self
            .eval_expr_value(arg, PipelineData::empty())?
            .into_nu_value("append")?;
        let target = self.mutable_list_method_target(receiver, "append")?;
        target.push(value);
        Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())))
    }

    fn eval_add_method_call(
        &mut self,
        receiver: &Expr,
        positional: &[Expr],
    ) -> Result<RuntimeValue, ShellError> {
        let [arg] = positional else {
            return Err(stone_error("add", "add() requires exactly one argument"));
        };
        let value = self
            .eval_expr_value(arg, PipelineData::empty())?
            .into_nu_value("add")?;
        let key = value_identity_key(&value, "add")?;
        let vals = self.mutable_list_method_target(receiver, "add")?;
        for existing in vals.iter() {
            if value_identity_key(existing, "add")? == key {
                return Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())));
            }
        }
        vals.push(value);
        Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())))
    }

    fn mutable_list_method_target(
        &mut self,
        receiver: &Expr,
        context: &str,
    ) -> Result<&mut Vec<Value>, ShellError> {
        let (name, indices) = self.eval_mutable_expr_path(receiver, context)?;
        let target = self
            .state
            .get_local_mut(&name)
            .ok_or_else(|| stone_error(context, format!("unknown name `{name}`")))?;
        let RuntimeValue::Nu(target) = target else {
            return Err(stone_error(
                context,
                format!("{name} does not support {context}()"),
            ));
        };
        let target = if indices.is_empty() {
            target
        } else {
            subscript_path_mut(target, &indices, context)?
        };
        let Value::List { vals, .. } = target else {
            return Err(stone_error(
                context,
                format!("{} has no {context}()", target.get_type()),
            ));
        };
        Ok(vals)
    }

    fn eval_map_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [func, iterable] = call.positional.as_slice() else {
            return Err(stone_error("map", "map() requires exactly two arguments"));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "map",
                "map() keyword arguments are not supported",
            ));
        }
        let values = self.eval_iterable_expr(iterable)?;
        if let Expr::Name(func_name) = func {
            if crate::stone_builtins::is_map_builtin_name(func_name) {
                return values
                    .into_iter()
                    .map(|value| {
                        let value = value.into_nu_value("map")?;
                        map_builtin_value(func_name, &value)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|values| RuntimeValue::Nu(Value::list(values, Span::unknown())));
            }
        }
        let callable = self.eval_callable_expr(func)?;
        values
            .into_iter()
            .map(|value| {
                self.invoke_callable(&callable, vec![value])?
                    .into_nu_value("map")
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|values| RuntimeValue::Nu(Value::list(values, Span::unknown())))
    }

    fn eval_filter_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [func, iterable] = call.positional.as_slice() else {
            return Err(stone_error(
                "filter",
                "filter() requires exactly two arguments",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "filter",
                "filter() keyword arguments are not supported",
            ));
        }
        let callable = self.eval_callable_expr(func)?;
        let mut selected = Vec::new();
        for value in self.eval_iterable_expr(iterable)? {
            let keep = self
                .invoke_callable(&callable, vec![value.clone()])?
                .into_nu_value("filter")?;
            if value_truthy(&keep) {
                selected.push(value.into_nu_value("filter")?);
            }
        }
        Ok(RuntimeValue::Nu(Value::list(selected, Span::unknown())))
    }

    fn eval_sum_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [arg] = call.positional.as_slice() else {
            return Err(stone_error("sum", "sum() requires exactly one argument"));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "sum",
                "sum() keyword arguments are not supported",
            ));
        }

        let runtime_values: Vec<RuntimeValue> = match arg {
            Expr::Generator { .. } => self
                .eval_generator_values(arg)?
                .into_iter()
                .map(RuntimeValue::Nu)
                .collect(),
            _ => {
                let value = self.eval_expr_value(arg, PipelineData::empty())?;
                value_to_iter_values(&value)?
                    .into_iter()
                    .map(RuntimeValue::Nu)
                    .collect()
            }
        };

        let mut values = Vec::with_capacity(runtime_values.len());
        for value in runtime_values {
            values.push(value.into_nu_value("sum")?);
        }
        sum_builtin(values).map(RuntimeValue::Nu)
    }

    fn eval_generator_values(&mut self, expression: &Expr) -> Result<Vec<Value>, ShellError> {
        let Expr::Generator { target, iter, elt } = expression else {
            return Err(stone_error("generator expression", "expected generator"));
        };
        let values = self.eval_iterable_expr(iter)?;
        let previous = self.state.get_local(target);
        let mut output = Vec::with_capacity(values.len());
        for value in values {
            self.state.set_local(target.clone(), value);
            output.push(
                self.eval_expr_value(elt, PipelineData::empty())?
                    .into_nu_value("generator expression")?,
            );
        }
        match previous {
            Some(value) => {
                self.state.set_local(target.clone(), value);
            }
            None => {
                self.state.remove_local(target);
            }
        }
        Ok(output)
    }

    fn eval_iterable_expr(&mut self, expression: &Expr) -> Result<Vec<RuntimeValue>, ShellError> {
        let value = self.eval_expr_value(expression, PipelineData::empty())?;
        self.eval_iterable_value(value)
    }

    fn eval_iterable_value(
        &mut self,
        value: RuntimeValue,
    ) -> Result<Vec<RuntimeValue>, ShellError> {
        match value {
            RuntimeValue::File(handle) => match self.state.file_mut(handle)? {
                RuntimeFile::Read { text, closed, .. } => {
                    if *closed {
                        return Err(stone_error("iteration", "I/O operation on closed file"));
                    }
                    Ok(text
                        .lines()
                        .map(|line| {
                            RuntimeValue::Nu(Value::string(line.to_owned(), Span::unknown()))
                        })
                        .collect())
                }
                RuntimeFile::Write { .. } => {
                    Err(stone_error("iteration", "file is not open for reading"))
                }
            },
            RuntimeValue::TextLines(lines) => Ok(lines
                .lines
                .into_iter()
                .map(|line| RuntimeValue::Nu(Value::string(line, Span::unknown())))
                .collect()),
            RuntimeValue::JsonlRows(rows) => Ok(jsonl_row_views(&rows)),
            RuntimeValue::JsonArrayView(view) => json_array_view_iter_values(&view),
            value => Ok(value_to_iter_values(&value)?
                .into_iter()
                .map(RuntimeValue::Nu)
                .collect()),
        }
    }

    fn eval_compare(
        &mut self,
        left: &Expr,
        ops: &[CompareOp],
        comparators: &[Expr],
    ) -> Result<RuntimeValue, ShellError> {
        let started = self.state.profiler.start();
        let result = (|| {
            if ops.len() != comparators.len() {
                return Err(stone_error(
                    "comparison",
                    "comparison operator and comparator counts differ",
                ));
            }

            let mut left_value = self
                .eval_expr_value(left, PipelineData::empty())?
                .into_nu_value("comparison")?;
            for (op, comparator) in ops.iter().zip(comparators) {
                let right_value = self
                    .eval_expr_value(comparator, PipelineData::empty())?
                    .into_nu_value("comparison")?;
                if !compare_values(&left_value, *op, &right_value)? {
                    return Ok(RuntimeValue::Nu(Value::bool(false, Span::unknown())));
                }
                left_value = right_value;
            }

            Ok(RuntimeValue::Nu(Value::bool(true, Span::unknown())))
        })();
        self.state
            .profiler
            .finish(EvalProfileBucket::Compare, started);
        result
    }

    fn eval_bool_op(&mut self, op: BoolOp, values: &[Expr]) -> Result<RuntimeValue, ShellError> {
        let started = self.state.profiler.start();
        let result = (|| {
            if values.is_empty() {
                return Err(stone_error(
                    "boolean expression",
                    "boolean expression requires at least one value",
                ));
            }

            match op {
                BoolOp::And => {
                    for value in values {
                        let value = self
                            .eval_expr_value(value, PipelineData::empty())?
                            .into_nu_value("boolean expression")?;
                        if !value_truthy(&value) {
                            return Ok(RuntimeValue::Nu(Value::bool(false, Span::unknown())));
                        }
                    }
                    Ok(RuntimeValue::Nu(Value::bool(true, Span::unknown())))
                }
                BoolOp::Or => {
                    for value in values {
                        let value = self
                            .eval_expr_value(value, PipelineData::empty())?
                            .into_nu_value("boolean expression")?;
                        if value_truthy(&value) {
                            return Ok(RuntimeValue::Nu(Value::bool(true, Span::unknown())));
                        }
                    }
                    Ok(RuntimeValue::Nu(Value::bool(false, Span::unknown())))
                }
            }
        })();
        self.state
            .profiler
            .finish(EvalProfileBucket::BoolOp, started);
        result
    }
}

fn is_builtin_call(call: &Call) -> bool {
    if matches!(call.name.as_str(), "keys" | "values" | "items") {
        return call.positional.len() == 1;
    }
    if call.name == "get" {
        return !call.positional.is_empty();
    }
    matches!(
        call.name.as_str(),
        "cat"
            | "diff"
            | "edit"
            | "echo"
            | "emit"
            | "fail"
            | "filter"
            | "enumerate"
            | "find"
            | "float"
            | "format"
            | "first"
            | "head"
            | "from_json"
            | "help"
            | "int"
            | "last"
            | "len"
            | "ls"
            | "list_dir"
            | "list"
            | "join"
            | "map"
            | "max"
            | "min"
            | "mkdir"
            | "open"
            | "parse_float"
            | "parse_int"
            | "print"
            | "pwd"
            | "cd"
            | "range"
            | "read_file"
            | "read_text"
            | "read_csv"
            | "read_jsonl"
            | "round"
            | "rm"
            | "run"
            | "slice"
            | "split"
            | "resolve_command"
            | "state"
            | "starts_with"
            | "startswith"
            | "tail"
            | "last_result"
            | "start_daemon"
            | "daemon_status"
            | "stop_daemon"
            | "wait_port"
            | "save"
            | "search"
            | "sort"
            | "sorted"
            | "stat"
            | "str"
            | "sum"
            | "to_json"
            | "to_jsonl"
            | "set"
            | "unique"
            | "type"
            | "where"
            | "json_dumps"
            | "json_loads"
            | "read_json"
            | "write_file"
            | "write_text"
            | "edit_file"
            | "write_json"
            | "write_jsonl"
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MinMax {
    Min,
    Max,
}

#[derive(Clone)]
enum SortKey {
    Identity,
    Field(String),
    Callable(CallableValue),
}

impl MinMax {
    fn name(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Max => "max",
        }
    }
}

fn ensure_type(value: &Value, expected: StoneType, context: &str) -> Result<(), ShellError> {
    let ok = match expected {
        StoneType::Any => true,
        StoneType::Bool => matches!(value, Value::Bool { .. }),
        StoneType::Float => matches!(value, Value::Float { .. } | Value::Int { .. }),
        StoneType::Int => matches!(value, Value::Int { .. }),
        StoneType::List => matches!(value, Value::List { .. }),
        StoneType::None => matches!(value, Value::Nothing { .. }),
        StoneType::Record => matches!(value, Value::Record { .. }),
        StoneType::Str => matches!(value, Value::String { .. } | Value::Glob { .. }),
    };
    if ok {
        Ok(())
    } else {
        Err(stone_error(
            "type check",
            format!(
                "{context} expected {}, got {}",
                stone_type_name(expected),
                value.get_type()
            ),
        ))
    }
}

fn stone_type_name(ty: StoneType) -> &'static str {
    match ty {
        StoneType::Any => "Any",
        StoneType::Bool => "bool",
        StoneType::Float => "float",
        StoneType::Int => "int",
        StoneType::List => "list",
        StoneType::None => "None",
        StoneType::Record => "record",
        StoneType::Str => "str",
    }
}

fn match_fused_map_update_if(
    condition: &Expr,
    then_branch: &[Stmt],
    else_branch: &[Stmt],
) -> Option<FusedMapUpdateIf> {
    let (key_name, contains_map) = match_key_in_map(condition)?;
    if then_branch.is_empty() || else_branch.len() < then_branch.len() {
        return None;
    }
    let mut updates = Vec::with_capacity(then_branch.len());
    for (then_stmt, else_stmt) in then_branch.iter().zip(else_branch.iter()) {
        let (map_name, then_key, addend) = match_increment_assignment(then_stmt)?;
        if then_key != key_name {
            return None;
        }
        let (else_map, else_key, else_value) = match_insert_assignment(else_stmt)?;
        if else_map != map_name || else_key != key_name || else_value != addend {
            return None;
        }
        updates.push(FusedMapUpdate { map_name, addend });
    }
    let append_tail = &else_branch[then_branch.len()..];
    let append_list = match append_tail {
        [] => None,
        [stmt] => Some(match_append_key(stmt, &key_name)?),
        _ => return None,
    };
    Some(FusedMapUpdateIf {
        key_name,
        contains_map,
        updates,
        append_list,
    })
}

fn match_key_in_map(condition: &Expr) -> Option<(String, String)> {
    let Expr::Compare {
        left,
        ops,
        comparators,
    } = condition
    else {
        return None;
    };
    let ([CompareOp::In], [Expr::Name(map_name)]) = (ops.as_slice(), comparators.as_slice()) else {
        return None;
    };
    let Expr::Name(key_name) = left.as_ref() else {
        return None;
    };
    Some((key_name.clone(), map_name.clone()))
}

fn match_increment_assignment(stmt: &Stmt) -> Option<(String, String, Expr)> {
    match stmt {
        Stmt::Assign { target, value } => {
            let (target_map, target_key) = match_map_key_target(target)?;
            let Expr::Add { left, right } = value else {
                return None;
            };
            let (left_map, left_key) = match_map_key_expr(left)?;
            if left_map != target_map || left_key != target_key {
                return None;
            }
            Some((target_map, target_key, right.as_ref().clone()))
        }
        Stmt::AugAssign {
            target,
            op: AugOp::Add,
            value,
        } => {
            let (target_map, target_key) = match_map_key_target(target)?;
            Some((target_map, target_key, value.clone()))
        }
        _ => None,
    }
}

fn match_insert_assignment(stmt: &Stmt) -> Option<(String, String, Expr)> {
    let Stmt::Assign { target, value } = stmt else {
        return None;
    };
    let (map_name, key_name) = match_map_key_target(target)?;
    Some((map_name, key_name, value.clone()))
}

fn match_map_key_target(target: &AssignTarget) -> Option<(String, String)> {
    let AssignTarget::Subscript { value, index } = target else {
        return None;
    };
    let AssignTarget::Name(map_name) = value.as_ref() else {
        return None;
    };
    let Expr::Name(key_name) = index else {
        return None;
    };
    Some((map_name.clone(), key_name.clone()))
}

fn match_map_key_expr(value: &Expr) -> Option<(String, String)> {
    let Expr::Subscript { value, index } = value else {
        return None;
    };
    let Expr::Name(map_name) = value.as_ref() else {
        return None;
    };
    let Expr::Name(key_name) = index.as_ref() else {
        return None;
    };
    Some((map_name.clone(), key_name.clone()))
}

fn match_append_key(stmt: &Stmt, key_name: &str) -> Option<String> {
    let Stmt::Expr(Expr::MethodCall {
        receiver,
        method,
        positional,
    }) = stmt
    else {
        return None;
    };
    if method != "append" {
        return None;
    }
    let Expr::Name(list_name) = receiver.as_ref() else {
        return None;
    };
    let [Expr::Name(arg_name)] = positional.as_slice() else {
        return None;
    };
    if arg_name != key_name {
        return None;
    }
    Some(list_name.clone())
}

fn json_object_view_get_hot_jsonl_row_fields<'a>(
    view: &HotJsonlRowSlice<'a>,
    plan: &HotJsonlTracePlan,
    user_key: &str,
    amount_key: &str,
    items_key: &str,
    tags_key: &str,
) -> Result<HotJsonlRowFields<'a>, ShellError> {
    let bytes = view.bytes;
    let mut user = None;
    let mut amount = None;
    let mut items = None;
    let mut tags = None;

    json_object_for_each_field(bytes, |key_range, value_range| {
        if user.is_none() && json_key_matches(bytes, key_range.clone(), user_key) {
            user = Some(value_range);
        } else if amount.is_none() && json_key_matches(bytes, key_range.clone(), amount_key) {
            amount = Some(value_range);
        } else if items.is_none() && json_key_matches(bytes, key_range.clone(), items_key) {
            items = Some(value_range);
        } else if tags.is_none() && json_key_matches(bytes, key_range, tags_key) {
            tags = Some(value_range);
        }
        Ok(())
    })?;

    let user = match user {
        Some(range) => json_string_bytes_to_cow(trim_json_bytes(&bytes[range])).map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?,
        None if plan.user_has_default => Cow::Owned(plan.user_default.clone()),
        None => {
            return Err(stone_error(
                "json view",
                format!("record has no key `{user_key}`"),
            ));
        }
    };
    let amount = match amount {
        Some(range) => json_number_bytes_to_f64(
            trim_json_bytes(&bytes[range]),
            view.source,
            view.line_number,
        )?,
        None if plan.user_amount_has_default => plan.user_amount_default,
        None => {
            return Err(stone_error(
                "json view",
                format!("record has no key `{amount_key}`"),
            ));
        }
    };
    let items = match items {
        Some(range) => json_number_bytes_to_i64(
            trim_json_bytes(&bytes[range]),
            view.source,
            view.line_number,
        )?,
        None if plan.user_items_has_default => plan.user_items_default,
        None => {
            return Err(stone_error(
                "json view",
                format!("record has no key `{items_key}`"),
            ));
        }
    };
    let tags = match tags {
        Some(range) => {
            let value = trim_json_bytes(&bytes[range]);
            if !value.starts_with(b"[") {
                return Err(stone_error("json view", "expected JSON array"));
            }
            HotJsonlStringArray { bytes: value }
        }
        None if plan.tags_default_empty => HotJsonlStringArray { bytes: b"[]" },
        None => {
            return Err(stone_error(
                "json view",
                format!("record has no key `{tags_key}`"),
            ));
        }
    };

    Ok(HotJsonlRowFields {
        user,
        amount,
        items,
        tags,
    })
}

fn hot_jsonl_trace_plan_from_body_plan(body_plan: &HotJsonlAggregationBody) -> HotJsonlTracePlan {
    HotJsonlTracePlan {
        user_name: body_plan.user_name.clone(),
        user_key: body_plan.user_key.clone(),
        user_has_default: body_plan.user_has_default,
        user_default: body_plan.user_default.clone(),
        user_amounts_map: body_plan.user_amounts_map.clone(),
        user_amount_key: body_plan.user_amount_key.clone(),
        user_amount_has_default: body_plan.user_amount_has_default,
        user_amount_default: body_plan.user_amount_default,
        user_items_map: body_plan.user_items_map.clone(),
        user_items_key: body_plan.user_items_key.clone(),
        user_items_has_default: body_plan.user_items_has_default,
        user_items_default: body_plan.user_items_default,
        users_list: body_plan.users_list.clone(),
        tags_key: body_plan.tags_key.clone(),
        tags_default_empty: body_plan.tags_default_empty,
        tag_counts_map: body_plan.tag_counts_map.clone(),
        tags_list: body_plan.tags_list.clone(),
    }
}

fn stone_const_string(function: &StoneIrFunction, id: ConstId) -> Result<&str, ShellError> {
    match function.constants.get(id.0 as usize) {
        Some(StoneConst::String(value)) => Ok(value),
        _ => Err(stone_error("hot loop", "VM constant is not a string")),
    }
}

fn hot_jsonl_row_get_string_default<'a>(
    row: &HotJsonlRowSlice<'a>,
    key: &str,
    default: &str,
) -> Result<Cow<'a, str>, ShellError> {
    let Some(value_range) = find_top_level_json_field(row.bytes, key)? else {
        return Ok(Cow::Owned(default.to_owned()));
    };
    json_string_bytes_to_cow(trim_json_bytes(&row.bytes[value_range])).map_err(|err| {
        stone_error(
            "json view",
            format!("{} line {}: {}", row.source, row.line_number, err),
        )
    })
}

fn hot_jsonl_row_get_string_required<'a>(
    row: &HotJsonlRowSlice<'a>,
    key: &str,
) -> Result<Cow<'a, str>, ShellError> {
    let value_range = find_top_level_json_field(row.bytes, key)?
        .ok_or_else(|| stone_error("json view", format!("record has no key `{key}`")))?;
    json_string_bytes_to_cow(trim_json_bytes(&row.bytes[value_range])).map_err(|err| {
        stone_error(
            "json view",
            format!("{} line {}: {}", row.source, row.line_number, err),
        )
    })
}

fn hot_jsonl_row_get_f64_default(
    row: &HotJsonlRowSlice<'_>,
    key: &str,
    default: f64,
) -> Result<f64, ShellError> {
    let Some(value_range) = find_top_level_json_field(row.bytes, key)? else {
        return Ok(default);
    };
    json_number_bytes_to_f64(
        trim_json_bytes(&row.bytes[value_range]),
        row.source,
        row.line_number,
    )
}

fn hot_jsonl_row_get_f64_required(
    row: &HotJsonlRowSlice<'_>,
    key: &str,
) -> Result<f64, ShellError> {
    let value_range = find_top_level_json_field(row.bytes, key)?
        .ok_or_else(|| stone_error("json view", format!("record has no key `{key}`")))?;
    json_number_bytes_to_f64(
        trim_json_bytes(&row.bytes[value_range]),
        row.source,
        row.line_number,
    )
}

fn hot_jsonl_row_get_i64_default(
    row: &HotJsonlRowSlice<'_>,
    key: &str,
    default: i64,
) -> Result<i64, ShellError> {
    let Some(value_range) = find_top_level_json_field(row.bytes, key)? else {
        return Ok(default);
    };
    json_number_bytes_to_i64(
        trim_json_bytes(&row.bytes[value_range]),
        row.source,
        row.line_number,
    )
}

fn hot_jsonl_row_get_i64_required(
    row: &HotJsonlRowSlice<'_>,
    key: &str,
) -> Result<i64, ShellError> {
    let value_range = find_top_level_json_field(row.bytes, key)?
        .ok_or_else(|| stone_error("json view", format!("record has no key `{key}`")))?;
    json_number_bytes_to_i64(
        trim_json_bytes(&row.bytes[value_range]),
        row.source,
        row.line_number,
    )
}

fn hot_jsonl_row_get_array_default<'a>(
    row: &HotJsonlRowSlice<'a>,
    key: &str,
) -> Result<HotJsonlStringArray<'a>, ShellError> {
    let Some(value_range) = find_top_level_json_field(row.bytes, key)? else {
        return Ok(HotJsonlStringArray { bytes: b"[]" });
    };
    let value = trim_json_bytes(&row.bytes[value_range]);
    if !value.starts_with(b"[") {
        return Err(stone_error("json view", "expected JSON array"));
    }
    Ok(HotJsonlStringArray { bytes: value })
}

fn hot_jsonl_row_get_array_required<'a>(
    row: &HotJsonlRowSlice<'a>,
    key: &str,
) -> Result<HotJsonlStringArray<'a>, ShellError> {
    let value_range = find_top_level_json_field(row.bytes, key)?
        .ok_or_else(|| stone_error("json view", format!("record has no key `{key}`")))?;
    let value = trim_json_bytes(&row.bytes[value_range]);
    if !value.starts_with(b"[") {
        return Err(stone_error("json view", "expected JSON array"));
    }
    Ok(HotJsonlStringArray { bytes: value })
}

fn hot_jsonl_fields<'a>(
    slots: &'a HotJsonlNativeSlots<'_>,
) -> Result<&'a HotJsonlRowFields<'a>, ShellError> {
    slots
        .fields
        .as_ref()
        .ok_or_else(|| stone_error("hot loop", "JSONL field slots are not initialized"))
}

fn hot_jsonl_user<'a>(slots: &'a HotJsonlNativeSlots<'_>) -> Result<Cow<'a, str>, ShellError> {
    Ok(match &hot_jsonl_fields(slots)?.user {
        Cow::Borrowed(user) => Cow::Borrowed(*user),
        Cow::Owned(user) => Cow::Owned(user.clone()),
    })
}

fn hot_jsonl_string_array_for_each_string(
    view: &HotJsonlStringArray<'_>,
    mut f: impl for<'a> FnMut(Cow<'a, str>) -> Result<(), ShellError>,
) -> Result<(), ShellError> {
    json_array_bytes_for_each_range(view.bytes, |range| {
        let value = trim_json_bytes(&view.bytes[range]);
        let text = json_string_bytes_to_cow(value)
            .map_err(|err| stone_error("json view", err.to_string()))?;
        f(text)
    })
}

fn f64_record_from_native_map(keys: &[String], map: &HashMap<String, f64>) -> Record {
    let mut record = Record::with_capacity(map.len());
    for key in keys {
        if let Some(value) = map.get(key) {
            record.push(key.clone(), Value::float(*value, Span::unknown()));
        }
    }
    for (key, value) in map {
        if !keys.contains(key) {
            record.push(key.clone(), Value::float(*value, Span::unknown()));
        }
    }
    record
}

fn nested_totals_record_from_native_maps(
    keys: &[String],
    amounts: &HashMap<String, f64>,
    items: &HashMap<String, i64>,
    amount_field: &str,
    items_field: &str,
) -> Record {
    let mut record = Record::with_capacity(amounts.len().max(items.len()));
    for key in keys {
        if let Some(value) = nested_totals_value(key, amounts, items, amount_field, items_field) {
            record.push(key.clone(), value);
        }
    }
    for key in amounts.keys().chain(items.keys()) {
        if keys.contains(key) || record.get(key).is_some() {
            continue;
        }
        if let Some(value) = nested_totals_value(key, amounts, items, amount_field, items_field) {
            record.push(key.clone(), value);
        }
    }
    record
}

fn nested_totals_value(
    key: &str,
    amounts: &HashMap<String, f64>,
    items: &HashMap<String, i64>,
    amount_field: &str,
    items_field: &str,
) -> Option<Value> {
    let amount = amounts.get(key)?;
    let item_count = items.get(key)?;
    let mut totals = Record::with_capacity(2);
    totals.push(
        amount_field.to_owned(),
        Value::float(*amount, Span::unknown()),
    );
    totals.push(
        items_field.to_owned(),
        Value::int(*item_count, Span::unknown()),
    );
    Some(Value::record(totals, Span::unknown()))
}

fn i64_record_from_native_map(keys: &[String], map: &HashMap<String, i64>) -> Record {
    let mut record = Record::with_capacity(map.len());
    for key in keys {
        if let Some(value) = map.get(key) {
            record.push(key.clone(), Value::int(*value, Span::unknown()));
        }
    }
    for (key, value) in map {
        if !keys.contains(key) {
            record.push(key.clone(), Value::int(*value, Span::unknown()));
        }
    }
    record
}

fn string_list_from_ordered_keys(keys: &[String]) -> Value {
    Value::list(
        keys.iter()
            .map(|key| Value::string(key.clone(), Span::unknown()))
            .collect(),
        Span::unknown(),
    )
}

fn runtime_type_name(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::Nu(value) => value_type_name(value),
        RuntimeValue::File(_) => "file",
        RuntimeValue::TextLines(_) => "list",
        RuntimeValue::JsonlRows(_) => "list",
        RuntimeValue::JsonObjectView(_) => "dict",
        RuntimeValue::JsonArrayView(_) => "list",
        RuntimeValue::JsonScalarView(_) => "json",
        RuntimeValue::Callable(_) => "function",
    }
}

fn format_fstring_value(value: &Value, spec: &StoneFormatSpec) -> Result<String, ShellError> {
    match spec {
        StoneFormatSpec::Fixed { precision } => {
            let value = value_to_f64(value, "f-string format")?;
            Ok(format!("{value:.precision$}"))
        }
        StoneFormatSpec::ZeroPadInt { width } => {
            let value = value_to_i64(value, "f-string format")?;
            Ok(zfill_text(&value.to_string(), *width))
        }
    }
}

fn assign_subscript(target: &mut Value, index: &Value, value: Value) -> Result<(), ShellError> {
    match (target, index) {
        (Value::Record { val, .. }, Value::String { val: key, .. })
        | (Value::Record { val, .. }, Value::Glob { val: key, .. }) => {
            val.to_mut().insert(key.clone(), value);
            Ok(())
        }
        (Value::List { vals, .. }, Value::Int { val: index, .. }) => {
            let index = normalize_index(*index, vals.len())?;
            let slot = vals.get_mut(index).ok_or_else(|| {
                stone_error("assignment", format!("list index {index} is out of range"))
            })?;
            *slot = value;
            Ok(())
        }
        (target, index) => Err(stone_error(
            "assignment",
            format!(
                "cannot assign {} item with {} index",
                target.get_type(),
                index.get_type()
            ),
        )),
    }
}

fn assign_subscript_path(
    target: &mut Value,
    indices: &[Value],
    value: Value,
) -> Result<(), ShellError> {
    let Some((index, rest)) = indices.split_first() else {
        return Err(stone_error("assignment", "missing assignment index"));
    };
    if rest.is_empty() {
        return assign_subscript(target, index, value);
    }
    let target = subscript_mut(target, index, "assignment")?;
    assign_subscript_path(target, rest, value)
}

fn subscript_path_mut<'a>(
    target: &'a mut Value,
    indices: &[Value],
    context: &str,
) -> Result<&'a mut Value, ShellError> {
    let Some((index, rest)) = indices.split_first() else {
        return Ok(target);
    };
    let target = subscript_mut(target, index, context)?;
    subscript_path_mut(target, rest, context)
}

fn subscript_mut<'a>(
    target: &'a mut Value,
    index: &Value,
    context: &str,
) -> Result<&'a mut Value, ShellError> {
    match (target, index) {
        (Value::Record { val, .. }, Value::String { val: key, .. })
        | (Value::Record { val, .. }, Value::Glob { val: key, .. }) => val
            .to_mut()
            .get_mut(key)
            .ok_or_else(|| stone_error(context, format!("record has no key `{key}`"))),
        (Value::List { vals, .. }, Value::Int { val: index, .. }) => {
            let index = normalize_index(*index, vals.len())?;
            vals.get_mut(index)
                .ok_or_else(|| stone_error(context, format!("list index {index} is out of range")))
        }
        (target, index) => Err(stone_error(
            context,
            format!(
                "cannot mutate nested {} item with {} index",
                target.get_type(),
                index.get_type()
            ),
        )),
    }
}

fn runtime_value_record_string_field(
    value: &RuntimeValue,
    field: &str,
) -> Result<Option<String>, ShellError> {
    match value {
        RuntimeValue::Nu(Value::Record { val, .. }) => {
            let Some(value) = val.get(field) else {
                return Ok(None);
            };
            Ok(Some(value_to_string(value, "record field")?))
        }
        RuntimeValue::JsonObjectView(view) => {
            let Some(value) = json_object_view_get(view, field)? else {
                return Ok(None);
            };
            match value {
                RuntimeValue::Nu(Value::String { val, .. })
                | RuntimeValue::Nu(Value::Glob { val, .. }) => Ok(Some(val)),
                RuntimeValue::Nu(value) => Ok(Some(value_to_string(&value, "record field")?)),
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

fn value_to_iter_values(value: &RuntimeValue) -> Result<Vec<Value>, ShellError> {
    match value {
        RuntimeValue::Nu(Value::List { vals, .. }) => Ok(vals.clone()),
        RuntimeValue::Nu(Value::Record { val, .. }) => Ok(val
            .iter()
            .map(|(key, _)| Value::string(key.clone(), Span::unknown()))
            .collect()),
        RuntimeValue::Nu(Value::String { val, .. }) | RuntimeValue::Nu(Value::Glob { val, .. }) => {
            Ok(val
                .lines()
                .map(|line| Value::string(line.to_owned(), Span::unknown()))
                .collect())
        }
        RuntimeValue::Nu(other) => Err(stone_error(
            "iteration",
            format!("cannot iterate {}", other.get_type()),
        )),
        RuntimeValue::File(_) => Err(stone_error(
            "iteration",
            "file iteration is handled by the evaluator",
        )),
        RuntimeValue::TextLines(lines) => Ok(lines
            .lines
            .iter()
            .map(|line| Value::string(line.clone(), Span::unknown()))
            .collect()),
        RuntimeValue::JsonlRows(rows) => materialize_jsonl_rows(rows)
            .and_then(|value| value_to_iter_values(&RuntimeValue::Nu(value))),
        RuntimeValue::JsonObjectView(view) => materialize_json_object_view(view)
            .and_then(|value| value_to_iter_values(&RuntimeValue::Nu(value))),
        RuntimeValue::JsonArrayView(view) => materialize_json_array_view(view)
            .and_then(|value| value_to_iter_values(&RuntimeValue::Nu(value))),
        RuntimeValue::JsonScalarView(view) => materialize_json_scalar_view(view)
            .and_then(|value| value_to_iter_values(&RuntimeValue::Nu(value))),
        RuntimeValue::Callable(callable) => Err(stone_error(
            "iteration",
            format!("cannot iterate callable lambda#{}", callable.function_id),
        )),
    }
}

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}

fn unknown_stone_call_error(name: &str) -> ShellError {
    stone_error(
        "function call",
        format!("unknown Stone function `{name}`; use help() for available Stone functions"),
    )
}

fn file_method_error(method: &str) -> ShellError {
    let suggestion = match method {
        "splitlines" => Some(
            "read the file first with `text = f.read(); lines = text.splitlines()`, or iterate `for line in f`",
        ),
        "split" => Some(
            "read the file first with `text = f.read(); parts = text.split(...)`, or iterate `for line in f`",
        ),
        "strip" => {
            Some("iterate the file with `for line in f` and call `line.strip()` on each text line")
        }
        _ => None,
    };
    let message = match suggestion {
        Some(suggestion) => format!("file object has no {method}(). Did you mean to {suggestion}?"),
        None => format!("file object has no {method}()"),
    };
    stone_error("file method", message)
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "hermit"))]
    use super::cleanup_stale_run_temp_files;
    use super::{
        eval_program, eval_program_with_options, eval_program_with_output,
        match_fused_map_update_if, EvalHotLoopDiagnostics, EvalOptions, RuntimeValue, TextLines,
    };
    use crate::{json, stone_ast::lower_source, stone_vm::LoopIrOptimizationDiagnostic};
    use nu_protocol::{
        engine::{EngineState, Stack},
        PipelineData, ShellError, Span, Value,
    };
    use serde_json::json as json_value;
    use std::{
        fs,
        path::PathBuf,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn evaluates_command_call() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("pwd")?;
        let program = lower_source("pwd()")?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!(root.display().to_string())
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_cd_builtin_updates_session_cwd() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("cd-builtin")?;
        fs::create_dir_all(root.join("subdir")).expect("create subdir");
        fs::write(root.join("subdir/input.txt"), "hello").expect("write input");
        let program = lower_source(
            r#"new_cwd = cd("subdir")
text = read_text("input.txt")
emit({"cwd": pwd(), "new_cwd": new_cwd, "state_cwd": state()["cwd"], "text": text})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let expected_cwd = root.join("subdir").display().to_string();

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "cwd": expected_cwd,
                "new_cwd": expected_cwd,
                "state_cwd": expected_cwd,
                "text": "hello",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_coding_helpers_as_explicit_stone_calls() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("explicit-helpers")?;
        fs::write(root.join("input.txt"), "alpha\nbeta\nalpha\n").expect("write input");
        let program = lower_source(
            r#"mkdir("out")
saved = save("one\ntwo\n", "out/data.txt", force=True)
edited = edit("out/data.txt", "two", "three")
entries = ls("out")
found = find(".", "*.txt")
matches = search(".", "three")
regex_matches = search(".", "thr.e", regex=True)
rows = [{"region": "west", "qty": 2}, {"region": "east", "qty": 9}]
west = where(rows, "region", "west")
json_text = to_json({"west": west})
roundtrip = from_json(json_text)
emit({
    "saved_bytes": saved["bytes"],
    "edited": edited["replacements"],
    "entries": len(entries),
    "found": len(found),
    "matches": len(matches),
    "regex_matches": len(regex_matches),
    "first_region": first(west)["region"],
    "last_qty": last(rows)["qty"],
    "roundtrip_qty": roundtrip["west"][0]["qty"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "saved_bytes": 8,
                "edited": 1,
                "entries": 1,
                "found": 2,
                "matches": 1,
                "regex_matches": 1,
                "first_region": "west",
                "last_qty": 9,
                "roundtrip_qty": 2,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn where_supports_comparisons_and_predicates() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("where-comparisons")?;
        let program = lower_source(
            r#"rows = [
    {"region": "west", "status": "open", "qty": 2},
    {"region": "east", "status": "open", "qty": 9},
    {"region": "west", "status": "closed", "qty": 7},
    {"region": "west", "status": "open", "qty": 11},
]
large = where(rows, "qty", ">", 5)
not_east = where(rows, "region", "!=", "east")
open_west = where(rows, lambda r: r["status"] == "open" and r["region"] == "west")
emit({
    "large_qty": [row["qty"] for row in large],
    "not_east_count": len(not_east),
    "open_west_qty": [row["qty"] for row in open_west],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "large_qty": [9, 7, 11],
                "not_east_count": 3,
                "open_west_qty": [2, 11],
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_python_compat_set_type_and_record_attributes() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("python-compat-primitives")?;
        fs::write(root.join("large.txt"), "abcdef").expect("write large");
        let program = lower_source(
            r#"seen = set()
seen.add("alice")
seen.add("alice")
seen.add("bob")
rows = [{"name": "alice", "score": 3}, {"name": "bob", "score": 5}]
groups = {"alice": [], "bob": []}
groups["alice"].append("first")
groups.alice.append("second")
rows[0]["tags"] = []
rows[0].tags.append("blue")
groups["bob"].add("only")
groups["bob"].add("only")
result = run(["printf", "ok"])
emit({
    "seen": seen,
    "unique": set(["x", "x", "y"]),
    "name": rows[0].name,
    "group": groups["alice"],
    "tags": rows[0].tags,
    "bob": groups.bob,
    "score_type": type(rows[1].score),
    "seen_type": type(seen),
    "stdout": result.stdout,
    "prefix": read_file("large.txt", limit=3),
    "zfill": str(5).zfill(2),
    "zfill_sign": str(-5).zfill(3),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "seen": ["alice", "bob"],
                "unique": ["x", "y"],
                "name": "alice",
                "group": ["first", "second"],
                "tags": ["blue"],
                "bob": ["only"],
                "score_type": "int",
                "seen_type": "list",
                "stdout": "ok",
                "prefix": "abc",
                "zfill": "05",
                "zfill_sign": "-05",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn rejects_unknown_stone_calls_instead_of_falling_through_to_nu() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("unknown-call")?;
        let program = lower_source(r#"definitely_not_stone("/tmp/nope")"#)?;
        let err = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("unknown Stone call should fail before Nu lookup");
        let text = format!("{err:?}");
        assert!(
            text.contains("unknown Stone function"),
            "unexpected error: {text}"
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_assignment_and_local_name() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("assignment")?;
        let program = lower_source(
            r#"name = "stone"
echo(name)"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!("stone")
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_command_output_with_stone_loop() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("command-loop")?;
        fs::write(root.join("a.txt"), "a").expect("write file");
        fs::create_dir(root.join("dir")).expect("create dir");

        let program = lower_source(
            r#"files = ls(".")
names = []
for file in files:
    if file["type"] == "file":
        names.append(file["name"])
emit(names)
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!(["a.txt"])
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn sort_results_can_be_sliced_and_indexed() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("row-counts")?;
        let program = lower_source(r#"emit(sort([3, 1, 2])[-1:])"#)?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!([3])
        );

        let program = lower_source(r#"emit(sort([3, 1, 2])[0])"#)?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!(1)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn head_and_tail_alias_first_and_last() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("head-tail")?;
        let program = lower_source(
            r#"emit({
    "head": head([1, 2, 3, 4], 2),
    "tail": tail([1, 2, 3, 4], 2),
    "single_head": head([1, 2, 3]),
    "single_tail": tail([1, 2, 3]),
})"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "head": [1, 2],
                "tail": [3, 4],
                "single_head": 1,
                "single_tail": 3,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_integer_bitwise_operators() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("bitwise-operators")?;
        let program = lower_source(
            r#"emit({
    "and": 6 & 3,
    "or": 4 | 1,
    "xor": 6 ^ 3,
    "left": 3 << 2,
    "right": 8 >> 1,
    "invert": ~0,
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "and": 2,
                "or": 5,
                "xor": 5,
                "left": 12,
                "right": 4,
                "invert": -1,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_python_shaped_sort_builtin() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("sort-builtin")?;
        let program = lower_source(
            r#"rows = [
    {"name": "low", "count": 1},
    {"name": "high", "count": 3},
    {"name": "mid", "count": 2},
]
top = sort(rows, key="count", reverse=True)[:2]
emit([top[0]["name"], top[1]["name"]])
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!(["high", "mid"])
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_lambda_callbacks_for_sort_map_and_filter() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("lambda-callbacks")?;
        let program = lower_source(
            r#"rows = [
    {"name": "low", "count": 1, "status": 200},
    {"name": "high", "count": 3, "status": 404},
    {"name": "mid", "count": 2, "status": 404},
]
ranked = sort(rows, key=lambda r: (-r["count"], r["name"]))
errors = filter(lambda r: r["status"] == 404, rows)
names = map(lambda r: r["name"], errors)
emit({
    "ranked": [ranked[0]["name"], ranked[1]["name"], ranked[2]["name"]],
    "errors": names,
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "ranked": ["high", "mid", "low"],
                "errors": ["high", "mid"],
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_assigned_lambda_call_with_captures() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("lambda-call")?;
        let program = lower_source(
            r#"offset = 10
inc = lambda x: x + offset
offset = 100
emit(inc(5))
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!(15)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_python_shaped_unique_builtin() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("unique-builtin")?;
        let program = lower_source(
            r#"values = ["BUS-1", "BUS-2", "BUS-1", "BUS-3", "BUS-2"]
emit(unique(values))
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!(["BUS-1", "BUS-2", "BUS-3"])
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_nested_subscript_assignment() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("nested-assignment")?;
        let program = lower_source(
            r#"stats = {"alice": {"count": 1, "items": [10, 20]}}
stats["alice"]["count"] += 2
stats["alice"]["items"][1] = 25
emit(stats)
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({"alice": {"count": 3, "items": [10, 25]}})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_record_helper_methods() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("record-methods")?;
        let program = lower_source(
            r#"record = {"a": 1, "b": 2}
pairs = []
for key, value in record.items():
    pairs.append(key + ":" + str(value))
emit({
    "missing": record.get("missing", 9),
    "keys": record.keys(),
    "list_keys": list(record.keys()),
    "record_as_list": list(record),
    "values": record.values(),
    "pairs": pairs,
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "missing": 9,
                "keys": ["a", "b"],
                "list_keys": ["a", "b"],
                "record_as_list": ["a", "b"],
                "values": [1, 2],
                "pairs": ["a:1", "b:2"],
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_pass_statement() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("pass")?;
        let program = lower_source(
            r#"items = []
for value in [1, 2, 3]:
    if value in items:
        pass
    else:
        items.append(value)
emit(items)
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!([1, 2, 3])
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_typed_function_definition() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("typed-function")?;
        let program = lower_source(
            r#"def normalize(text: str) -> str:
    parts = text.split("/")
    return parts[2] + "-" + parts[0] + "-" + parts[1]

def add_one(value: int) -> int:
    return value + 1

emit({
    "date": normalize("01/02/2024"),
    "count": add_one(4),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({"date": "2024-01-02", "count": 5})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn function_argument_type_errors_are_explicit() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("typed-function-error")?;
        let program = lower_source(
            r#"def add_one(value: int) -> int:
    return value + 1

add_one("4")
"#,
        )?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("argument type should fail");
        let text = format!("{error:?}");
        assert!(text.contains("argument `value` expected int"), "{text}");

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn file_handles_support_splitlines_and_split() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("file-splitlines")?;
        fs::write(root.join("input.txt"), "a,b\nc,d\n").expect("write input");
        let program = lower_source(
            r#"lines = open("input.txt").splitlines()
parts = open("input.txt").split(",")
emit({"lines": lines, "parts": parts})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "lines": ["a,b", "c,d"],
                "parts": ["a", "b\nc", "d\n"],
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_subscript_compare_and_boolean_ops() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("agent-exprs")?;
        let program = lower_source(
            r#"item = {"name": "answer", "count": 3, "ready": True}
emit({
    "name": item["name"],
    "second": [10, 20, 30][1],
    "last": [10, 20, 30][-1],
    "ok": item["ready"] and item["count"] >= 2 and not False
})"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "name": "answer",
                "second": 20,
                "last": 30,
                "ok": true,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_if_else_over_typed_values() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("if-else")?;
        let program = lower_source(
            r#"item = {"ready": True, "count": 2}
if item["ready"] and item["count"] == 2:
    emit({"status": "ready"})
else:
    emit({"status": "blocked"})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({"status": "ready"})
        );

        let program = lower_source(
            r#"item = {"ready": False, "count": 2}
if item["ready"]:
    emit({"status": "ready"})
else:
    emit({"status": "blocked"})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({"status": "blocked"})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_elif_break_and_continue() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("elif-break-continue")?;
        fs::write(root.join("lines.txt"), "1\nskip\n2\nstop\n100\n").expect("write lines");

        let program = lower_source(
            r#"total = 0
for line in open("lines.txt"):
    text = line.strip()
    if text == "stop":
        break
    elif text == "skip":
        continue
    else:
        total += int(text)
emit({"total": total})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({"total": 3})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_python_shaped_sum_over_file_lines() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("sum-lines")?;
        fs::write(root.join("numbers.txt"), "10\n20\n-3\n").expect("write numbers");

        let program = lower_source(r#"sum(int(line) for line in open("numbers.txt"))"#)?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!(27)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_python_shaped_sum_over_map_int_file_lines() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("sum-map-lines")?;
        fs::write(root.join("numbers.txt"), "4\n5\n6\n").expect("write numbers");

        let program = lower_source(r#"sum(map(int, open("numbers.txt")))"#)?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!(15)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_python_shaped_file_read_helpers() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("file-read-helpers")?;
        fs::write(root.join("input.txt"), "alpha\nbeta\n").expect("write input");

        let program = lower_source(
            r#"f = open("input.txt")
emit({"read": f.read(), "cat": cat("input.txt")})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({"read": "alpha\nbeta\n", "cat": "alpha\nbeta\n"})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn file_method_errors_suggest_typed_read_patterns() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("file-method-suggestion")?;
        fs::write(root.join("input.txt"), "alpha\nbeta\n").expect("write input");

        let program = lower_source(
            r#"lines = open("input.txt").strip()
emit(lines)
"#,
        )?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("unsupported file string method should fail with suggestion");
        let text = format!("{error:?}");
        assert!(text.contains("file object has no strip()"));
        assert!(text.contains("iterate the file with `for line in f`"));

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn stone_read_errors_suggest_nearby_paths() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("stone-path-suggestion")?;
        fs::create_dir_all(root.join("data")).expect("create data dir");
        fs::write(root.join("data").join("input.csv"), "name\nalpha\n").expect("write csv");

        let program = lower_source(r#"read_csv("data/missing.csv")"#)?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("missing path should fail with suggestion");
        let text = format!("{error:?}");
        assert!(text.contains("Did you mean"), "{text}");
        assert!(text.contains("input.csv"), "{text}");

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn print_arity_error_suggests_typed_alternatives() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("print-arity-suggestion")?;
        let program = lower_source(
            r#"print("Line", 1, ":", "value")
"#,
        )?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("multi-argument print should fail with suggestion");
        let text = format!("{error:?}");
        assert!(text.contains("print() requires exactly one argument"));
        assert!(text.contains("string concatenation"));
        assert!(text.contains("emit a list/record"));

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn print_records_stdout_while_preserving_value() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("print-stdout")?;
        let program = lower_source(
            r#"print("alpha")
print({"name": "beta", "count": 2})
"#,
        )?;
        let output =
            eval_program_with_output(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?,
            json_value!({"name": "beta", "count": 2})
        );
        assert_eq!(output.stdout, "alpha\n{\"count\":2,\"name\":\"beta\"}\n");

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_python_shaped_file_write_handle() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("file-write-handle")?;

        let program = lower_source(
            r#"f = open("out.txt", "w")
written = f.write("hello\n")
f.write("world\n")
f.close()
print({"written": written, "content": cat("out.txt")})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({"written": 6, "content": "hello\nworld\n"})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_portable_file_adapter_primitives() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("file-adapter")?;

        let program = lower_source(
            r#"write_text("nested/out.txt", "alpha\n")
write_text("nested/out.txt", "beta\n", append=True)
edit_file("nested/out.txt", "beta", "gamma")
text = read_text("nested/out.txt")
write_file("alias.txt", "alias\n")
alias = read_file("alias.txt", 16)
info = stat("nested/out.txt")
entries = list_dir("nested")
emit({
    "text": text,
    "alias": alias,
    "type": info["type"],
    "is_file": info["is_file"],
    "listed": entries[0]["name"],
    "listed_type": entries[0]["type"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "text": "alpha\ngamma\n",
                "alias": "alias\n",
                "type": "file",
                "is_file": true,
                "listed": "out.txt",
                "listed_type": "file"
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_stone_help_contract() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("stone-help")?;

        let program = lower_source(
            r#"overview = help()
write = help("write_file")
edit = help("edit_file")
session = help("session")
missing = help("not_a_builtin")
emit({
    "language": overview["language"],
    "has_unsupported": len(overview["unsupported"]) > 0,
    "has_session_topic": "session" in overview["topics"],
    "for_llm_mentions_bindings": "bindings persist" in overview["for_llm"],
    "for_llm_mentions_multiline_eval": "multi-line script" in overview["for_llm"],
    "session_mentions_live_bindings": "live name binding" in session["bullets"][1],
    "session_mentions_binding_ack": "bound names" in session["bullets"][2],
    "write_signature": write["signature"],
    "write_example": write["examples"][0],
    "edit_signature": edit["signature"],
    "missing_found": missing["found"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "language": "Stone",
                "has_unsupported": true,
                "has_session_topic": true,
                "for_llm_mentions_bindings": true,
                "for_llm_mentions_multiline_eval": true,
                "session_mentions_live_bindings": true,
                "session_mentions_binding_ack": true,
                "write_signature": "write_file(path: str, text: str, append: bool = False) -> record",
                "write_example": "write_file(\"/app/report.txt\", \"ok\\n\")",
                "edit_signature": "edit(path: str, old: str, new: str, all: bool = False) -> record",
                "missing_found": false
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_common_python_shaped_ergonomics() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("common-ergonomics")?;
        let program = lower_source(
            r#"line = "alpha,7"
missing = None
present = "value"
name, raw_score = line.split(",")
score = parse_int(raw_score, 0)
fallback = parse_float("bad", 1.5)
rows = [{"name": name, "score": score}, {"name": "skip", "score": 0}]
lookup = {row["name"]: row["score"] for row in rows if row["score"] != 0}
ordered = sorted([3, 1, 2])
parts = []
for key, value in items(lookup):
    parts.append(f"{key}:{value}")
i = 0
while i < 3:
    i += 1
def finish() -> None:
    return None
finished = finish()
emit({
    "message": f"{name}:{score}",
    "none_check": missing is None and present is not None,
    "finished": finished is None,
    "lookup_keys": keys(lookup),
    "lookup_value": get(lookup, "alpha", 0),
    "parts": parts,
    "ordered": ordered,
    "fallback": fallback,
    "i": i,
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "message": "alpha:7",
                "none_check": true,
                "finished": true,
                "lookup_keys": ["alpha"],
                "lookup_value": 7,
                "parts": ["alpha:7"],
                "ordered": [1, 2, 3],
                "fallback": 1.5,
                "i": 3,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_direct_open_write_call() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("direct-open-write")?;

        let program = lower_source(
            r#"open("out.txt", "w").write("hello")
emit(cat("out.txt"))
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!("hello")
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_with_open_scope() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("with-open-scope")?;
        fs::write(root.join("input.txt"), "alpha\nbeta\n").expect("write input");

        let program = lower_source(
            r#"with open("input.txt") as f:
    with open("out.txt", "w") as out:
        out.write(f.read())
with open("input.txt") as f:
    text = f.read()
emit({"content": cat("out.txt"), "text": text})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({"content": "alpha\nbeta\n", "text": "alpha\nbeta\n"})
        );

        let program = lower_source(
            r#"with open("input.txt") as f:
    text = f.read()
emit(f.read())
"#,
        )?;
        let err = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("with binding should not leak");
        let debug = format!("{err:?}");
        assert!(
            debug.contains("unknown name `f`"),
            "unexpected error: {debug}"
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn rejects_file_object_at_json_boundary() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("file-boundary")?;
        fs::write(root.join("input.txt"), "content").expect("write input");

        let program = lower_source(r#"open("input.txt")"#)?;
        let err = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("file object should not serialize");
        let debug = format!("{err:?}");
        assert!(
            debug.contains("file objects are task-owned runtime values"),
            "unexpected error: {debug}"
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_for_augassign_and_string_methods() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("for-string-methods")?;
        fs::write(root.join("numbers.txt"), " 10\n# skip\n20\n-3\n").expect("write numbers");

        let program = lower_source(
            r##"total = 0
kept = []
for line in open("numbers.txt"):
    text = line.strip()
    if text.startswith("#"):
        emit(None)
    else:
        total += int(text)
        kept.append(text)
emit({"total": total, "kept": kept, "count": len(kept), "has_20": "20" in kept})
"##,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "total": 27,
                "kept": ["10", "20", "-3"],
                "count": 3,
                "has_20": true,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_split_endswith_and_not_in() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("split-endswith")?;
        let program = lower_source(
            r#"parts = "alpha,beta.txt".split(",")
emit({"left": parts[0], "right": parts[1], "txt": parts[1].endswith(".txt"), "missing": "gamma" not in parts})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "left": "alpha",
                "right": "beta.txt",
                "txt": true,
                "missing": true,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_range_enumerate_slices_and_text_helpers() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("python-helpers")?;
        let program = lower_source(
            r#"items = ["zero", "one", "two", "three"]
pairs = []
for pair in enumerate(items[1:3], 10):
    pairs.append(pair)
nums = []
for number in range(1, 6, 2):
    nums.append(number)
lines = "alpha\nbeta\n".splitlines()
emit({
    "slice": items[1:3],
    "tail": items[2:],
    "prefix": "abcdef"[:3],
    "suffix": "abcdef"[-3:],
    "pairs": pairs,
    "nums": nums,
    "lines": lines,
    "replace": "alpha beta".replace(" ", "-"),
    "join": ",".join(["a", "b", "c"])
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "slice": ["one", "two"],
                "tail": ["two", "three"],
                "prefix": "abc",
                "suffix": "def",
                "pairs": [[10, "one"], [11, "two"]],
                "nums": [1, 3, 5],
                "lines": ["alpha", "beta"],
                "replace": "alpha-beta",
                "join": "a,b,c",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_top_level_text_list_helper_builtins() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("top-level-text-list-helpers")?;
        let program = lower_source(
            r#"items = split("alpha,beta,gamma", ",")
emit({
    "split": items,
    "join": join(items, "|"),
    "join_reversed_args": join("/", ["tmp", "waymark"]),
    "slice": slice(items, 1, 3),
    "tail": slice(items, 1),
    "prefix": slice("abcdef", None, 3),
    "starts_with": starts_with("abcdef", "abc"),
    "startswith": startswith("abcdef", "def"),
    "format": format("{}:{} {{ok}}", "port", 8080),
    "max": max(1, 7, 3),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "split": ["alpha", "beta", "gamma"],
                "join": "alpha|beta|gamma",
                "join_reversed_args": "tmp/waymark",
                "slice": ["beta", "gamma"],
                "tail": ["beta", "gamma"],
                "prefix": "abc",
                "starts_with": true,
                "startswith": false,
                "format": "port:8080 {ok}",
                "max": 7,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_min_max_numeric_builtins() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("min-max")?;
        let program = lower_source(
            r#"items = ["zero", "one", "two", "three"]
limit = min(2, len(items))
emit({
    "limit": limit,
    "max_int": max(3, 9, -1),
    "min_float": min(3.5, 2, 8.25)
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "limit": 2,
                "max_int": 9,
                "min_float": 2,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_float_conversion_builtin() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("float-conversion")?;
        let program = lower_source(
            r#"values = ["10.5", "2.25"]
converted = map(float, values)
emit({
    "first": float(values[0]),
    "total": sum(converted),
    "rounded": round(float("3.14159"), 2)
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "first": 10.5,
                "total": 12.75,
                "rounded": 3.14,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_string_case_helpers() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("string-case")?;
        let program = lower_source(
            r#"name = "North West Capital"
emit({
    "lower": name.lower(),
    "upper": name.upper(),
    "match": name.lower().startswith("north west"),
    "fixed": f"{12.345:.2f}",
    "zero": f"{7:03d}",
    "signed": f"{-7:03d}",
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "lower": "north west capital",
                "upper": "NORTH WEST CAPITAL",
                "match": true,
                "fixed": "12.35",
                "zero": "007",
                "signed": "-07",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_read_csv_records() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("read-csv")?;
        fs::write(
            root.join("input.csv"),
            "Company Name,Account Number,Description\nNorth West Capital,BUS-1,\"alpha, beta\"\n",
        )
        .expect("write csv");
        let program = lower_source(
            r#"rows = read_csv("input.csv")
emit({
    "count": len(rows),
    "name": rows[0]["Company Name"],
    "account": rows[0]["Account Number"],
    "description": rows[0]["Description"]
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "count": 1,
                "name": "North West Capital",
                "account": "BUS-1",
                "description": "alpha, beta",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn generic_vm_unique_append_preserves_value_equality() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("generic-vm-unique-append")?;
        let program = lower_source(
            r#"seen = []
for n in [1, 2, 1, 3, 2]:
    if not n in seen:
        seen.append(n)
emit(seen)
"#,
        )?;
        let output = eval_program_with_options(
            &engine_state,
            &mut stack,
            &program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )?;

        assert_eq!(
            json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?,
            json_value!([1, 2, 3])
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fused_kernels_selected"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fused_kernels_executed"],
            json_value!(1)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn hot_loop_diagnostics_are_returned_with_output() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("hot-loop-diagnostics-output")?;
        let program = lower_source(
            r#"total = 0
for n in [1, 2, 3]:
    total += n
emit(total)
"#,
        )?;
        let output = eval_program_with_options(
            &engine_state,
            &mut stack,
            &program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )?;

        assert_eq!(
            output.diagnostics["hot_loop"]["hot_loop_enabled"],
            json_value!(true)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["generic_vm_loops_executed"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_ir_lowered"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fusion_missed_reasons"],
            json_value!({"no_fused_kernel": 1})
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_ir_optimization_counts"],
            json_value!({})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn runtime_values_expose_compact_type_tags() {
        assert_eq!(
            RuntimeValue::Nu(Value::int(1, Span::unknown()))
                .type_tag()
                .id(),
            1
        );
        assert_eq!(
            RuntimeValue::TextLines(TextLines {
                lines: vec!["a".to_owned()],
                source: "test".to_owned(),
            })
            .type_tag()
            .id(),
            3
        );
    }

    #[test]
    fn hot_loop_executes_nested_user_totals_jsonl_aggregation() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("nested-user-totals-jsonl")?;
        fs::write(
            root.join("records.jsonl"),
            r#"{"user":"alice","amount":1.5,"items":2,"tags":["a","b"]}
{"user":"bob","amount":3.0,"items":1,"tags":["a"]}
{"user":"alice","amount":2.25,"items":4,"tags":["b"]}
"#,
        )
        .expect("write fixture");
        let program = lower_source(
            r#"user_totals = {}
tag_counts = {}
rows = read_jsonl("records.jsonl")
for row in rows:
    user = row.user
    if user not in user_totals:
        user_totals[user] = {"total_amount": 0.0, "total_items": 0}
    user_totals[user]["total_amount"] += float(row.amount)
    user_totals[user]["total_items"] += int(row.items)
    for tag in row.tags:
        if tag in tag_counts:
            tag_counts[tag] += 1
        else:
            tag_counts[tag] = 1
emit({"user_totals": user_totals, "tag_counts": tag_counts})
"#,
        )?;
        let output = eval_program_with_options(
            &engine_state,
            &mut stack,
            &program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )?;
        let value = json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?;

        assert_eq!(
            value,
            json_value!({
                "user_totals": {
                    "alice": {"total_amount": 3.75, "total_items": 6},
                    "bob": {"total_amount": 3.0, "total_items": 1},
                },
                "tag_counts": {
                    "a": 2,
                    "b": 2,
                },
            })
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["jsonl_fused_traces_executed"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fused_kernels_selected"],
            json_value!(1)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn hot_loop_executes_init_then_add_jsonl_aggregation() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("init-then-add-jsonl")?;
        fs::write(
            root.join("records.jsonl"),
            r#"{"user":"alice","amount":1.5,"items":2,"tags":["a","b"]}
{"user":"bob","amount":3.0,"items":1,"tags":["a"]}
{"user":"alice","amount":2.25,"items":4,"tags":["b"]}
"#,
        )
        .expect("write fixture");
        let program = lower_source(
            r#"user_amounts = {}
user_items = {}
tag_counts = {}
rows = read_jsonl("records.jsonl")
for row in rows:
    user = row["user"]
    if user not in user_amounts:
        user_amounts[user] = 0.0
        user_items[user] = 0
    user_amounts[user] += float(row["amount"])
    user_items[user] += int(row["items"])
    for tag in row["tags"]:
        if tag not in tag_counts:
            tag_counts[tag] = 0
        tag_counts[tag] += 1
emit({"user_amounts": user_amounts, "user_items": user_items, "tag_counts": tag_counts})
"#,
        )?;
        let output = eval_program_with_options(
            &engine_state,
            &mut stack,
            &program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )?;
        let value = json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?;

        assert_eq!(
            value,
            json_value!({
                "user_amounts": {"alice": 3.75, "bob": 3.0},
                "user_items": {"alice": 6, "bob": 1},
                "tag_counts": {"a": 2, "b": 2},
            })
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["jsonl_fused_traces_executed"],
            json_value!(1)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn hot_loop_executes_outer_jsonl_file_loop_directly() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("outer-jsonl-file-loop")?;
        fs::write(
            root.join("records_1.jsonl"),
            r#"{"user":"alice","amount":1.5,"items":2,"tags":["a","b"]}
{"user":"bob","amount":3.0,"items":1,"tags":["a"]}
"#,
        )
        .expect("write first fixture");
        fs::write(
            root.join("records_2.jsonl"),
            r#"{"user":"alice","amount":2.25,"items":4,"tags":["b"]}
"#,
        )
        .expect("write second fixture");
        let program = lower_source(
            r#"files = [{"path": "records_1.jsonl"}, {"path": "records_2.jsonl"}]
user_amounts = {}
user_items = {}
tag_counts = {}
record_count = 0
for f in files:
    rows = read_jsonl(f["path"])
    for row in rows:
        user = row["user"]
        if user not in user_amounts:
            user_amounts[user] = 0.0
            user_items[user] = 0
        user_amounts[user] += float(row["amount"])
        user_items[user] += int(row["items"])
        record_count += 1
        for tag in row["tags"]:
            if tag not in tag_counts:
                tag_counts[tag] = 0
            tag_counts[tag] += 1
emit({"user_amounts": user_amounts, "user_items": user_items, "tag_counts": tag_counts, "record_count": record_count})
"#,
        )?;
        let output = eval_program_with_options(
            &engine_state,
            &mut stack,
            &program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )?;
        let value = json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?;

        assert_eq!(
            value,
            json_value!({
                "user_amounts": {"alice": 3.75, "bob": 3.0},
                "user_items": {"alice": 6, "bob": 1},
                "tag_counts": {"a": 2, "b": 2},
                "record_count": 3,
            })
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["jsonl_fused_traces_executed"],
            json_value!(2)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fused_kernels_selected"],
            json_value!(2)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_lowering_missed_reasons"],
            json_value!({})
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fusion_missed_reasons"],
            json_value!({})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn generic_vm_map_count_overflow_errors() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("generic-vm-count-overflow")?;
        let program = lower_source(
            r#"counts = {"a": 9223372036854775807}
for tag in ["a"]:
    if tag in counts:
        counts[tag] += 1
    else:
        counts[tag] = 1
"#,
        )?;
        let error = eval_program_hot_loop(&engine_state, &mut stack, &program)
            .expect_err("overflow should fail");
        let text = format!("{error:?}");
        assert!(text.contains("integer addition overflow"), "{text}");

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn generic_vm_executes_read_csv_record_count_through_loop_ir() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("generic-vm-csv-loop-ir")?;
        fs::write(root.join("input.csv"), "status\nopen\nclosed\nopen\n")
            .expect("write csv fixture");
        let program = lower_source(
            r#"counts = {}
for row in read_csv("input.csv"):
    status = row["status"]
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
emit(counts)
"#,
        )?;
        let output = eval_program_with_options(
            &engine_state,
            &mut stack,
            &program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )?;

        assert_eq!(
            json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?,
            json_value!({"open": 2, "closed": 1})
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_ir_lowered"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fused_kernels_selected"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fused_kernels_executed"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_ir_optimization_counts"],
            json_value!({"canonicalized": 1})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn generic_vm_executes_read_jsonl_record_count_through_loop_ir() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("generic-vm-jsonl-loop-ir")?;
        fs::write(
            root.join("input.jsonl"),
            "{\"status\":\" open \"}\n{\"status\":\"closed\"}\n{\"status\":\"OPEN\"}\n",
        )
        .expect("write jsonl fixture");
        let program = lower_source(
            r#"counts = {}
for row in read_jsonl("input.jsonl"):
    status = row["status"].strip().lower()
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
emit(counts)
"#,
        )?;
        let output = eval_program_with_options(
            &engine_state,
            &mut stack,
            &program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )?;

        assert_eq!(
            json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?,
            json_value!({"open": 2, "closed": 1})
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_ir_lowered"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fused_kernels_selected"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fused_kernels_executed"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_ir_optimization_counts"],
            json_value!({})
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn hot_loop_diagnostics_report_loop_ir_canonicalization_count() {
        let mut diagnostics = EvalHotLoopDiagnostics::default();
        diagnostics.loop_ir_optimized(&[LoopIrOptimizationDiagnostic::Canonicalized]);
        let output = diagnostics
            .json_value(true, true)
            .expect("hot loop diagnostics");
        assert_eq!(
            output["loop_ir_optimization_counts"],
            json_value!({"canonicalized": 1})
        );
    }

    #[test]
    fn generic_vm_matches_baseline_for_supported_ops() -> Result<(), ShellError> {
        assert_hot_loop_matches_baseline(
            "generic-vm-add-assign",
            r#"total = 0
for n in [1, 2, 3]:
    total += n
emit(total)
"#,
            &[],
        )?;
        assert_hot_loop_matches_baseline(
            "generic-vm-parse-int",
            r#"total = 0
for line in open("numbers.txt").splitlines():
    total += int(line)
emit(total)
"#,
            &[("numbers.txt", "1\n2\n3\n")],
        )?;
        assert_hot_loop_matches_baseline(
            "generic-vm-parse-float",
            r#"total = 0.0
for line in open("numbers.txt").splitlines():
    total += float(line)
emit(total)
"#,
            &[("numbers.txt", "1.25\n2.5\n")],
        )?;
        assert_hot_loop_matches_baseline(
            "generic-vm-map-count",
            r#"counts = {}
for tag in ["a", "b", "a"]:
    if tag in counts:
        counts[tag] += 1
    else:
        counts[tag] = 1
emit(counts)
"#,
            &[],
        )?;
        assert_hot_loop_matches_baseline(
            "generic-vm-record-field-count",
            r#"counts = {}
for row in read_csv("input.csv"):
    status = row["status"]
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
emit(counts)
"#,
            &[("input.csv", "status\nopen\nclosed\nopen\n")],
        )?;
        assert_hot_loop_matches_baseline(
            "generic-vm-normalized-record-field-count",
            r#"counts = {}
for row in read_csv("input.csv"):
    status = row["status"].strip().lower()
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
emit(counts)
"#,
            &[("input.csv", "status\n Open \nopen\nCLOSED\n")],
        )?;
        assert_hot_loop_matches_baseline(
            "generic-vm-list-append",
            r#"items = []
for tag in ["a", "b", "a"]:
    items.append(tag)
emit(items)
"#,
            &[],
        )?;
        assert_hot_loop_matches_baseline(
            "generic-vm-list-append-unique",
            r#"seen = []
for tag in ["a", "b", "a"]:
    if not tag in seen:
        seen.append(tag)
emit(seen)
"#,
            &[],
        )?;
        Ok(())
    }

    #[test]
    fn expression_vm_matches_baseline_for_integer_arithmetic_and_bitwise() -> Result<(), ShellError>
    {
        assert_hot_loop_matches_baseline(
            "expression-vm-arithmetic-bitwise",
            r#"total = 0
mask = 0
for n in [1, 2, 3]:
    total += n * 2
    mask = mask | (n << 1)
emit({"total": total, "mask": mask})
"#,
            &[],
        )?;
        assert_hot_loop_matches_baseline(
            "expression-vm-all-bitwise",
            r#"mask = 0
for n in [1, 2, 3]:
    mask = (mask & 7) ^ (n >> 1)
    mask = mask + ~n
emit(mask)
"#,
            &[],
        )?;
        assert_hot_loop_matches_baseline(
            "expression-vm-floor-div-sub",
            r#"total = 20
for n in [1, 2, 3]:
    total = total - (n // 2)
emit(total)
"#,
            &[],
        )?;
        Ok(())
    }

    #[test]
    fn generic_vm_empty_loop_preserves_baseline_target_semantics() -> Result<(), ShellError> {
        assert_hot_loop_matches_baseline(
            "generic-vm-empty-loop-target",
            r#"n = "before"
total = 0
for n in []:
    total += n
emit({"n": n, "total": total})
"#,
            &[],
        )
    }

    #[test]
    fn generic_vm_record_field_count_overflow_errors() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("generic-vm-record-overflow")?;
        fs::write(root.join("input.csv"), "status\na\n").expect("write csv");
        let program = lower_source(
            r#"counts = {"a": 9223372036854775807}
for row in read_csv("input.csv"):
    status = row["status"]
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
"#,
        )?;
        let error = eval_program_hot_loop(&engine_state, &mut stack, &program)
            .expect_err("overflow should fail");
        let text = format!("{error:?}");
        assert!(text.contains("integer addition overflow"), "{text}");

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn generic_vm_unsupported_value_type_falls_back_without_partial_mutation(
    ) -> Result<(), ShellError> {
        let source = r#"total = ""
for n in ["a", "b", "c"]:
    total += n
emit(total)
"#;
        assert_hot_loop_matches_baseline("generic-vm-unsupported-fallback", source, &[])?;

        let (engine_state, mut stack, root) = test_engine("generic-vm-unsupported-diagnostics")?;
        let program = lower_source(source)?;
        let output = eval_program_with_options(
            &engine_state,
            &mut stack,
            &program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )?;

        assert_eq!(
            json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?,
            json_value!("abc")
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_ir_lowered"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["loop_fallbacks"],
            json_value!(1)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["generic_vm_loops_executed"],
            json_value!(0)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn bounded_structured_reads_return_prefix_rows() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("bounded-reads")?;
        fs::write(root.join("input.csv"), "name,count\na,1\nb,2\nc,3\n").expect("write csv");
        fs::write(
            root.join("input.jsonl"),
            "{\"name\":\"a\"}\n{\"name\":\"b\"}\n{\"name\":\"c\"}\n",
        )
        .expect("write jsonl");

        let program = lower_source(
            r#"csv_rows = read_csv("input.csv", 2)
jsonl_rows = read_jsonl("input.jsonl", 1)
emit({
    "csv_count": len(csv_rows),
    "csv_last": csv_rows[1]["name"],
    "jsonl_count": len(jsonl_rows),
    "jsonl_first": jsonl_rows[0]["name"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "csv_count": 2,
                "csv_last": "b",
                "jsonl_count": 1,
                "jsonl_first": "a",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn bounded_structured_reads_accept_limit_keyword() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("bounded-reads-keyword")?;
        fs::write(root.join("input.csv"), "name,count\na,1\nb,2\n").expect("write csv");
        fs::write(
            root.join("input.jsonl"),
            "{\"name\":\"a\"}\n{\"name\":\"b\"}\n",
        )
        .expect("write jsonl");

        let program = lower_source(
            r#"csv_rows = read_csv("input.csv", limit=1)
jsonl_rows = read_jsonl("input.jsonl", limit=1)
emit({
    "csv_count": len(csv_rows),
    "csv_first": csv_rows[0]["name"],
    "jsonl_count": len(jsonl_rows),
    "jsonl_first": jsonl_rows[0]["name"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "csv_count": 1,
                "csv_first": "a",
                "jsonl_count": 1,
                "jsonl_first": "a",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_success_and_failure() -> Result<(), ShellError> {
        let (engine_state, mut stack, _root) = test_engine("run-builtin")?;
        let program = lower_source(
            r#"ok = run(["sh", "-c", "printf hello"])
bad = run(["sh", "-c", "printf nope >&2; exit 7"])
quiet = run(["sh", "-c", "exit 9"])
emit({
    "ok": ok["ok"],
    "stdout": ok["stdout"],
    "bad_ok": bad["ok"],
    "bad_kind": bad["kind"],
    "bad_exit": bad["exit_code"],
    "bad_stderr": bad["stderr"],
    "bad_explanation": bad["explanation"]["summary"],
    "bad_scope": bad["explanation"]["scope"],
    "quiet_kind": quiet["explanation"]["kind"],
    "quiet_summary": quiet["explanation"]["summary"],
    "quiet_step_count": len(quiet["explanation"]["next_steps"]),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(true));
        assert_eq!(value["stdout"], json_value!("hello"));
        assert_eq!(value["bad_ok"], json_value!(false));
        assert_eq!(value["bad_kind"], json_value!("exec_failed"));
        assert_eq!(value["bad_exit"], json_value!(7));
        assert_eq!(value["bad_stderr"], json_value!("nope"));
        assert_eq!(
            value["bad_explanation"],
            json_value!("Stone successfully ran the external process, but it exited with code 7.")
        );
        assert_eq!(
            value["bad_scope"],
            json_value!("external process; Stone transport succeeded")
        );
        assert_eq!(
            value["quiet_kind"],
            json_value!("external_process_no_clear_error")
        );
        assert_eq!(
            value["quiet_summary"],
            json_value!(
                "Stone successfully ran the external process, but it exited with code 9 and produced no clear error message."
            )
        );
        assert_eq!(value["quiet_step_count"], json_value!(4));
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_cwd_env_stdin_and_timeout() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-builtin-options")?;
        fs::create_dir_all(root.join("subdir")).expect("create subdir");
        let program = lower_source(
            r#"echoed = run(
    ["sh", "-c", "printf '%s:%s:%s' \"$PWD\" \"$STONE_TEST_ENV\" \"$(cat)\""],
    cwd="subdir",
    env={"STONE_TEST_ENV": "set"},
    stdin="input",
)
timed = run(["sh", "-c", "sleep 1"], timeout_ms=20)
positional_cwd = run(["pwd"], "subdir")
positional_timeout = run(["sh", "-c", "sleep 1"], 20)
emit({
    "echoed_ok": echoed["ok"],
    "echoed": echoed["stdout"],
    "timed_ok": timed["ok"],
    "timed_kind": timed["kind"],
    "timed_out": timed["timed_out"],
    "timed_summary": timed["explanation"]["summary"],
    "timed_timeout_ms": timed["explanation"]["timeout_ms"],
    "timed_steps": timed["explanation"]["next_steps"],
    "positional_cwd": positional_cwd["stdout"],
    "positional_timeout": positional_timeout["timed_out"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["echoed_ok"], json_value!(true));
        assert_eq!(
            value["echoed"],
            json_value!(format!("{}:set:input", root.join("subdir").display()))
        );
        assert_eq!(value["timed_ok"], json_value!(false));
        assert_eq!(value["timed_kind"], json_value!("timeout"));
        assert_eq!(value["timed_out"], json_value!(true));
        assert_eq!(value["timed_timeout_ms"], json_value!(20));
        assert!(value["timed_summary"]
            .as_str()
            .expect("timeout summary string")
            .contains("stopped waiting"));
        assert!(value["timed_steps"][0]
            .as_str()
            .expect("timeout next step")
            .contains("partial checkout"));
        assert_eq!(
            value["positional_cwd"],
            json_value!(format!("{}\n", root.join("subdir").display()))
        );
        assert_eq!(value["positional_timeout"], json_value!(true));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_output_controls() -> Result<(), ShellError> {
        let (engine_state, mut stack, _root) = test_engine("run-builtin-output-controls")?;
        let program = lower_source(
            r#"limited = run(["sh", "-c", "printf abcdef; printf err >&2"], max_stdout_bytes=3, max_stderr_bytes=2)
suppressed = run(["sh", "-c", "printf hidden; printf also-hidden >&2"], stdout="suppress", stderr="discard")
merged = run(["sh", "-c", "printf out; printf err >&2"], stderr="stdout")
emit({
    "limited_stdout": limited["stdout"],
    "limited_stderr": limited["stderr"],
    "limited_stdout_truncated": limited["truncated"]["stdout"],
    "limited_stderr_truncated": limited["truncated"]["stderr"],
    "suppressed_stdout": suppressed["stdout"],
    "suppressed_stderr": suppressed["stderr"],
    "suppressed_stdout_flag": suppressed["suppressed"]["stdout"],
    "suppressed_stderr_flag": suppressed["suppressed"]["stderr"],
    "merged_stdout": merged["stdout"],
    "merged_stderr": merged["stderr"],
    "merged_flag": merged["stderr_to_stdout"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["limited_stdout"], json_value!("abc"));
        assert_eq!(value["limited_stderr"], json_value!("er"));
        assert_eq!(value["limited_stdout_truncated"], json_value!(true));
        assert_eq!(value["limited_stderr_truncated"], json_value!(true));
        assert_eq!(value["suppressed_stdout"], json_value!(""));
        assert_eq!(value["suppressed_stderr"], json_value!(""));
        assert_eq!(value["suppressed_stdout_flag"], json_value!(true));
        assert_eq!(value["suppressed_stderr_flag"], json_value!(true));
        assert!(value["merged_stdout"]
            .as_str()
            .expect("merged stdout string")
            .contains("out"));
        assert!(value["merged_stdout"]
            .as_str()
            .expect("merged stdout string")
            .contains("err"));
        assert_eq!(value["merged_stderr"], json_value!(""));
        assert_eq!(value["merged_flag"], json_value!(true));
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn cleanup_stale_run_temp_files_removes_only_waymark_captures() {
        let root = test_root("run-temp-cleanup");
        fs::create_dir_all(&root).expect("create temp cleanup root");
        let stale_stdout = root.join("stone-run-123-0.stdout");
        let stale_stderr = root.join("stone-run-123-0.stderr");
        let unrelated = root.join("stone-run-123-0.log");
        fs::write(&stale_stdout, "out").expect("write stdout temp");
        fs::write(&stale_stderr, "err").expect("write stderr temp");
        fs::write(&unrelated, "log").expect("write unrelated temp");

        cleanup_stale_run_temp_files(&root, Duration::ZERO);

        assert!(!stale_stdout.exists());
        assert!(!stale_stderr.exists());
        assert!(unrelated.exists());
        cleanup_dir(&root);
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_python_runtime_context() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-host-python-context")?;
        fs::write(
            root.join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("write pyproject");
        let program = lower_source(
            r#"result = run(["python3", "-c", "print('ok')"])
emit({
    "ok": result["ok"],
    "stdout": result["stdout"],
    "runtime_kind": result["runtime"]["kind"],
    "command_name": result["runtime"]["command_name"],
    "resolved_is_string": type(result["runtime"]["resolved_executable"]) == "str",
    "python_executable_is_string": type(result["runtime"]["python_executable"]) == "str",
    "marker_count": len(result["runtime"]["cwd_project_markers"]),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(true));
        assert_eq!(value["stdout"], json_value!("ok\n"));
        assert_eq!(value["runtime_kind"], json_value!("python"));
        assert_eq!(value["command_name"], json_value!("python3"));
        assert_eq!(value["resolved_is_string"], json_value!(true));
        assert_eq!(value["python_executable_is_string"], json_value!(true));
        assert_eq!(value["marker_count"], json_value!(1));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_python_missing_module_hint() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-python-missing-module")?;
        write_helper(
            &root,
            "python.stone",
            include_str!("../../../.stone/helpers/python.stone"),
        );
        let program = lower_source(
            r#"result = run(["python3", "-c", "import stone_definitely_missing_module"])
emit({
    "ok": result["ok"],
    "kind": result["explanation"]["kind"],
    "runtime_kind": result["runtime"]["kind"],
    "helper_count": len(result["helpers"]),
    "helper": result["helpers"][0]["helper"],
    "helper_kind": result["helpers"][0]["kind"],
    "helper_family": result["helpers"][0]["family"],
    "module": result["helpers"][0]["evidence"]["module"],
    "package": result["helpers"][0]["evidence"]["package"],
    "next_check": result["helpers"][0]["next_checks"][0],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["kind"], json_value!("external_process_exit"));
        assert_eq!(
            value["module"],
            json_value!("stone_definitely_missing_module")
        );
        assert_eq!(
            value["package"],
            json_value!("stone_definitely_missing_module")
        );
        assert_eq!(value["runtime_kind"], json_value!("python"));
        assert_eq!(value["helper_count"], json_value!(1));
        assert_eq!(value["helper"], json_value!("python.after_failure"));
        assert_eq!(value["helper_kind"], json_value!("python_module_not_found"));
        assert_eq!(value["helper_family"], json_value!("python"));
        assert_eq!(
            value["next_check"],
            json_value!([
                "python3",
                "-m",
                "pip",
                "show",
                "stone_definitely_missing_module"
            ])
        );
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_python_missing_attribute_hint() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-python-missing-attribute")?;
        write_helper(
            &root,
            "python.stone",
            include_str!("../../../.stone/helpers/python.stone"),
        );
        let program = lower_source(
            r#"result = run(["python3", "-c", "import math; math.stone_definitely_missing_attribute"])
emit({
    "ok": result["ok"],
    "kind": result["explanation"]["kind"],
    "runtime_kind": result["runtime"]["kind"],
    "helper_count": len(result["helpers"]),
    "helper_kind": result["helpers"][0]["kind"],
    "module": result["helpers"][0]["evidence"]["module"],
    "attribute": result["helpers"][0]["evidence"]["attribute"],
    "package": result["helpers"][0]["evidence"]["package"],
    "next_checks": result["helpers"][0]["next_checks"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["kind"], json_value!("external_process_exit"));
        assert_eq!(value["helper_count"], json_value!(1));
        assert_eq!(
            value["helper_kind"],
            json_value!("python_module_attribute_missing")
        );
        assert_eq!(value["module"], json_value!("math"));
        assert_eq!(
            value["attribute"],
            json_value!("stone_definitely_missing_attribute")
        );
        assert_eq!(value["package"], json_value!("math"));
        assert_eq!(
            value["next_checks"],
            json_value!([
                ["python3", "-m", "pip", "show", "math"],
                ["python3", "-m", "pip", "check"]
            ])
        );
        assert_eq!(value["runtime_kind"], json_value!("python"));
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_pip_check_conflict_hint() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-pip-check-conflict")?;
        write_helper(
            &root,
            "python.stone",
            include_str!("../../../.stone/helpers/python.stone"),
        );
        let program = lower_source(
            r#"script = "import sys; print('demo 1.0 has requirement dep<2, but you have dep 3.0.', file=sys.stderr); sys.exit(1)"
result = run(["python3", "-c", script])
emit({
    "ok": result["ok"],
    "kind": result["explanation"]["kind"],
    "helper_count": len(result["helpers"]),
    "helper_kind": result["helpers"][0]["kind"],
    "dependent": result["helpers"][0]["evidence"]["dependent"],
    "requirement": result["helpers"][0]["evidence"]["requirement"],
    "installed": result["helpers"][0]["evidence"]["installed"],
    "next_checks": result["helpers"][0]["next_checks"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["kind"], json_value!("external_process_exit"));
        assert_eq!(value["helper_count"], json_value!(1));
        assert_eq!(
            value["helper_kind"],
            json_value!("python_dependency_conflict")
        );
        assert_eq!(value["dependent"], json_value!("demo 1.0"));
        assert_eq!(value["requirement"], json_value!("dep<2"));
        assert_eq!(value["installed"], json_value!("dep 3.0"));
        assert_eq!(
            value["next_checks"],
            json_value!([
                ["python3", "-m", "pip", "check"],
                ["python3", "-m", "pip", "show", "demo 1.0", "dep"]
            ])
        );
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_pip_resolution_failure_hint() -> Result<(), ShellError> {
        use std::os::unix::fs::PermissionsExt;

        let (engine_state, mut stack, root) = test_engine("run-pip-resolution-failure")?;
        write_helper(
            &root,
            "python.stone",
            include_str!("../../../.stone/helpers/python.stone"),
        );
        let pip = root.join("pip");
        fs::write(
            &pip,
            "#!/bin/sh\necho 'ERROR: Cannot install alpha==1 and beta==2 because these package versions have conflicting dependencies.' >&2\necho 'ERROR: ResolutionImpossible' >&2\nexit 1\n",
        )
        .expect("write fake pip");
        fs::set_permissions(&pip, fs::Permissions::from_mode(0o755)).expect("chmod fake pip");
        let argv = json_value!([
            pip.display().to_string(),
            "install",
            "alpha==1",
            "beta==2",
            "-r",
            "requirements.txt",
            "--dry-run",
            "-c",
            "constraints.txt",
            "-f",
            "/tmp/wheels",
            "--index-url",
            "https://example.invalid/simple",
            "--extra-index-url",
            "https://extra.invalid/simple"
        ])
        .to_string();
        let program = lower_source(&format!(
            r#"result = run({argv})
emit({{
    "ok": result["ok"],
    "kind": result["explanation"]["kind"],
    "helper_count": len(result["helpers"]),
    "helper_kind": result["helpers"][0]["kind"],
    "helper": result["helpers"][0]["helper"],
    "requested": result["helpers"][0]["evidence"]["requested"],
    "evidence": result["helpers"][0]["evidence"]["evidence"],
    "next_checks": result["helpers"][0]["next_checks"],
}})
"#
        ))?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["kind"], json_value!("external_process_exit"));
        assert_eq!(value["helper_count"], json_value!(1));
        assert_eq!(
            value["helper_kind"],
            json_value!("python_package_resolution_failed")
        );
        assert_eq!(value["helper"], json_value!("python.pip_after_failure"));
        assert_eq!(value["requested"], json_value!(["alpha==1", "beta==2"]));
        assert!(value["evidence"]
            .as_str()
            .expect("resolver evidence")
            .contains("ResolutionImpossible"));
        assert_eq!(
            value["next_checks"],
            json_value!([
                ["python3", "-m", "pip", "check"],
                ["python3", "-m", "pip", "debug"]
            ])
        );
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn stone_helper_registry_discovers_and_orders_hooks() -> Result<(), ShellError> {
        let root = test_root("helper-registry");
        fs::create_dir_all(root.join(".stone/helpers")).expect("create helper dir");
        fs::write(
            root.join(".stone/helpers/b.stone"),
            r#"hook("run.after_failure", family="generic", handler="generic.after_failure", priority=10)
"#,
        )
        .expect("write helper b");
        fs::write(
            root.join(".stone/helpers/a.stone"),
            r#"hook("run.after_failure", family="python", argv0_prefix=["python"], handler="python.after_failure", priority=100)
"#,
        )
        .expect("write helper a");

        let registry = super::stone_helper_registry(&root);
        assert_eq!(registry.hooks.len(), 2);
        assert_eq!(registry.hooks[0].handler.name, "python.after_failure");
        assert_eq!(registry.hooks[1].handler.name, "generic.after_failure");

        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn stone_helper_registry_parses_argv0_filters() -> Result<(), ShellError> {
        let root = test_root("helper-registry-argv0");
        fs::create_dir_all(root.join(".stone/helpers")).expect("create helper dir");
        fs::write(
            root.join(".stone/helpers/media.stone"),
            r#"hook("run.after_success", family="media", argv0=["ffmpeg","ffprobe"], argv0_prefix=["ffmpeg-","ffprobe-"], handler="media.after_success", priority=100)
"#,
        )
        .expect("write helper");

        let registry = super::stone_helper_registry(&root);
        assert_eq!(registry.hooks.len(), 1);
        assert_eq!(registry.hooks[0].family, "media");
        assert_eq!(registry.hooks[0].argv0, vec!["ffmpeg", "ffprobe"]);
        assert_eq!(registry.hooks[0].argv0_prefix, vec!["ffmpeg-", "ffprobe-"]);

        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn stone_helper_registry_classifies_command_families_from_registered_matchers() {
        let root = test_root("helper-registry-family-lookup");
        fs::create_dir_all(root.join(".stone/helpers")).expect("create helper dir");
        fs::write(
            root.join(".stone/helpers/families.stone"),
            r#"hook("run.after_failure", family="python", argv0_prefix=["python"], handler="python.after_failure", priority=100)
hook("run.after_failure", family="python/pip", argv0=["pip"], argv0_prefix=["pip"], handler="python.pip_after_failure", priority=110)
hook("run.after_failure", family="llvm", argv0=["llvm-dis"], handler="llvm.after_failure", priority=100)
hook("run.after_failure", family="native", argv0=["file","ldd","readelf","objdump"], handler="native.after_failure", priority=100)
hook("run.after_success", family="media", argv0=["ffprobe"], argv0_prefix=["ffmpeg-"], handler="media.after_success", priority=100)
hook("run.after_timeout", family="build", argv0=["make"], handler="build.after_timeout", priority=100)
"#,
        )
        .expect("write helper");
        let registry = super::stone_helper_registry(&root);
        assert_eq!(
            registry.command_family(&["python3".into(), "-m".into(), "pip".into()]),
            "python"
        );
        assert_eq!(
            registry.command_family(&["pip3".into(), "install".into(), "pytest".into()]),
            "python/pip"
        );
        assert_eq!(
            registry.command_family(&["/usr/bin/python3.12".into()]),
            "python"
        );
        assert_eq!(
            registry.command_family(&["llvm-dis".into(), "binary.bc".into()]),
            "llvm"
        );
        assert_eq!(
            registry.command_family(&["readelf".into(), "-Ws".into(), "lib.so".into()]),
            "native"
        );
        assert_eq!(
            registry.command_family(&["ffprobe".into(), "out.mp4".into()]),
            "media"
        );
        assert_eq!(
            registry.command_family(&["/venv/bin/ffmpeg-linux-x86_64-v7.0.2".into()]),
            "media"
        );
        assert_eq!(
            registry.command_family(&["make".into(), "all".into()]),
            "build"
        );
        assert_eq!(
            registry.command_family(&["sh".into(), "-c".into(), "true".into()]),
            "generic"
        );
        cleanup_dir(&root);
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_native_helper_observation() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-native-helper")?;
        write_helper(
            &root,
            "native.stone",
            include_str!("../../../.stone/helpers/native.stone"),
        );
        let program = lower_source(
            r#"result = run(["sh", "-c", "echo 'libOpenGL.so.0 => not found' >&2; exit 1"])
emit({
    "ok": result["ok"],
    "helper": result["helpers"][0]["helper"],
    "kind": result["helpers"][0]["kind"],
    "missing": result["helpers"][0]["evidence"]["missing_libraries"][0],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["helper"], json_value!("native.after_failure"));
        assert_eq!(value["kind"], json_value!("native_shared_library_failure"));
        assert_eq!(value["missing"], json_value!("libOpenGL.so.0"));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_uses_eval_init_helper_cache() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-helper-cache")?;
        write_helper(
            &root,
            "native.stone",
            include_str!("../../../.stone/helpers/native.stone"),
        );
        let program = lower_source(
            r#"write_file(".stone/helpers/z-late.stone", "hook(\"run.after_failure\", family=\"generic\", handler=\"generic.after_failure\", priority=1000)\n")
result = run(["sh", "-c", "echo 'libLate.so => not found' >&2; exit 1"])
emit({
    "helper": result["helpers"][0]["helper"],
    "kind": result["helpers"][0]["kind"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["helper"], json_value!("native.after_failure"));
        assert_eq!(value["kind"], json_value!("native_shared_library_failure"));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_media_helper_from_argv0_filter() -> Result<(), ShellError> {
        use std::os::unix::fs::PermissionsExt;

        let (engine_state, mut stack, root) = test_engine("run-media-helper")?;
        write_helper(
            &root,
            "media.stone",
            include_str!("../../../.stone/helpers/media.stone"),
        );
        let fake_ffmpeg = root.join("ffmpeg-linux-x86_64-v7.0.2");
        fs::write(&fake_ffmpeg, "#!/bin/sh\nexit 0\n").expect("write fake ffmpeg");
        fs::set_permissions(&fake_ffmpeg, fs::Permissions::from_mode(0o755))
            .expect("chmod fake ffmpeg");
        let source = format!(
            r#"result = run(["{}", "-i", "input.wav", "out.mp4"])
emit({{
    "ok": result["ok"],
    "helper": result["helpers"][0]["helper"],
    "family": result["helpers"][0]["family"],
    "kind": result["helpers"][0]["kind"],
    "media_path": result["helpers"][0]["evidence"]["candidate_paths"][1],
    "next_check": result["helpers"][0]["next_checks"][0][0],
}})
"#,
            fake_ffmpeg.display()
        );
        let program = lower_source(&source)?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(true));
        assert_eq!(value["helper"], json_value!("media.after_success"));
        assert_eq!(value["family"], json_value!("media"));
        assert_eq!(value["kind"], json_value!("media_probe_available"));
        assert_eq!(value["next_check"], json_value!("ffprobe"));
        assert!(value["media_path"]
            .as_str()
            .expect("media path")
            .ends_with("out.mp4"));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_dynamic_stone_helper_callback() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-dynamic-helper-callback")?;
        write_helper(
            &root,
            "conda.stone",
            r#"def conda_after_failure(event):
    return {
        "helper": "conda.after_failure",
        "event": event["event"],
        "family": event["family"],
        "kind": "conda_solver_failure",
        "summary": event["stderr"],
        "evidence": {"argv0": event["argv"][0]},
        "next_checks": [["conda", "info"]],
    }

hook("run.after_failure", family="conda", argv0=["sh"], handler="conda.after_failure", priority=100)
"#,
        );
        let program = lower_source(
            r#"result = run(["sh", "-c", "echo conda solve failed >&2; exit 1"])
emit({
    "helper": result["helpers"][0]["helper"],
    "kind": result["helpers"][0]["kind"],
    "family": result["helpers"][0]["family"],
    "argv0": result["helpers"][0]["evidence"]["argv0"],
    "next_check": result["helpers"][0]["next_checks"][0][0],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["helper"], json_value!("conda.after_failure"));
        assert_eq!(value["kind"], json_value!("conda_solver_failure"));
        assert_eq!(value["family"], json_value!("conda"));
        assert_eq!(value["argv0"], json_value!("sh"));
        assert_eq!(value["next_check"], json_value!("conda"));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_checked_in_conda_helper() -> Result<(), ShellError> {
        use std::os::unix::fs::PermissionsExt;

        let (engine_state, mut stack, root) = test_engine("run-conda-helper")?;
        write_helper(
            &root,
            "conda.stone",
            include_str!("../../../.stone/helpers/conda.stone"),
        );
        let fake_conda = root.join("conda");
        fs::write(
            &fake_conda,
            "#!/bin/sh\necho 'LibMambaUnsatisfiableError: package conflicts with python' >&2\nexit 1\n",
        )
        .expect("write fake conda");
        fs::set_permissions(&fake_conda, fs::Permissions::from_mode(0o755))
            .expect("chmod fake conda");
        let source = format!(
            r#"result = run(["{}", "install", "demo"])
emit({{
    "helper": result["helpers"][0]["helper"],
    "kind": result["helpers"][0]["kind"],
    "conflict": result["helpers"][0]["evidence"]["conflict_excerpt"],
    "next_check": result["helpers"][0]["next_checks"][0][0],
}})
"#,
            fake_conda.display()
        );
        let program = lower_source(&source)?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["helper"], json_value!("conda.after_failure"));
        assert_eq!(value["kind"], json_value!("conda_unsatisfiable"));
        assert!(value["conflict"]
            .as_str()
            .expect("conflict excerpt")
            .contains("conflicts with"));
        assert_eq!(value["next_check"], json_value!("conda"));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_checked_in_latex_helper() -> Result<(), ShellError> {
        use std::os::unix::fs::PermissionsExt;

        let (engine_state, mut stack, root) = test_engine("run-latex-helper")?;
        write_helper(
            &root,
            "latex.stone",
            include_str!("../../../.stone/helpers/latex.stone"),
        );
        let fake_pdflatex = root.join("pdflatex");
        fs::write(
            &fake_pdflatex,
            "#!/bin/sh\necho 'Overfull \\\\hbox (1.0pt too wide) in paragraph at lines 1--2'\nexit 0\n",
        )
        .expect("write fake pdflatex");
        fs::set_permissions(&fake_pdflatex, fs::Permissions::from_mode(0o755))
            .expect("chmod fake pdflatex");
        let source = format!(
            r#"result = run(["{}", "paper.tex"])
emit({{
    "helper": result["helpers"][0]["helper"],
    "kind": result["helpers"][0]["kind"],
    "overfull": result["helpers"][0]["evidence"]["overfull_lines"],
    "next_check": result["helpers"][0]["next_checks"][0][0],
}})
"#,
            fake_pdflatex.display()
        );
        let program = lower_source(&source)?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["helper"], json_value!("latex.after_success"));
        assert_eq!(value["kind"], json_value!("latex_warnings"));
        assert!(value["overfull"]
            .as_str()
            .expect("overfull lines")
            .contains("Overfull"));
        assert_eq!(value["next_check"], json_value!("grep"));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn suppresses_unimplemented_registered_helper_observation() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-unimplemented-helper")?;
        write_helper(
            &root,
            "typo.stone",
            r#"hook("run.after_failure", family="generic", handler="typo.after_failure", priority=100)
"#,
        );
        let program = lower_source(
            r#"result = run(["sh", "-c", "exit 1"])
emit({
    "ok": result["ok"],
    "helper_count": len(result.get("helpers", [])),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["helper_count"], json_value!(0));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn helper_error_observation_includes_source_in_evidence() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-helper-error-source")?;
        write_helper(
            &root,
            "broken.stone",
            r#"def broken_after_failure():
    return None

hook("run.after_failure", family="generic", handler="broken.after_failure", priority=100)
"#,
        );
        let program = lower_source(
            r#"result = run(["sh", "-c", "exit 1"])
emit({
    "kind": result["helpers"][0]["kind"],
    "source": result["helpers"][0]["evidence"]["source"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["kind"], json_value!("helper_error"));
        assert!(value["source"]
            .as_str()
            .expect("helper source")
            .ends_with(".stone/helpers/broken.stone"));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_run_builtin_timeout_helper_observation() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("run-timeout-helper")?;
        write_helper(
            &root,
            "build.stone",
            include_str!("../../../.stone/helpers/build.stone"),
        );
        let program = lower_source(
            r#"result = run(["sh", "-c", "printf building; sleep 1"], timeout_ms=20)
emit({
    "ok": result["ok"],
    "kind": result["kind"],
    "helper": result["helpers"][0]["helper"],
    "helper_kind": result["helpers"][0]["kind"],
    "stdout_tail": result["helpers"][0]["evidence"]["stdout_tail"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["kind"], json_value!("timeout"));
        assert_eq!(value["helper"], json_value!("build.after_timeout"));
        assert_eq!(value["helper_kind"], json_value!("build_timeout"));
        assert_eq!(value["stdout_tail"], json_value!("building"));
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_resolve_command_and_spawn_not_found_feedback() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("resolve-command")?;
        let program = lower_source(
            r#"found = resolve_command("sh")
missing = resolve_command("stone-command-that-does-not-exist")
emit({
    "found_ok": found["ok"],
    "found_path_is_string": type(found["path"]) == "str",
    "found_matches": len(found["matches"]) > 0,
    "found_searched": len(found["searched"]) > 0,
    "found_kind": found["explanation"]["kind"],
    "missing_ok": missing["ok"],
    "missing_path": missing["path"],
    "missing_matches": len(missing["matches"]),
    "missing_kind": missing["explanation"]["kind"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "found_ok": true,
                "found_path_is_string": true,
                "found_matches": true,
                "found_searched": true,
                "found_kind": "command_found",
                "missing_ok": false,
                "missing_path": null,
                "missing_matches": 0,
                "missing_kind": "command_not_found",
            })
        );

        let program = lower_source(r#"run(["stone-command-that-does-not-exist"])"#)?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("missing executable should fail before returning a run record");
        let text = format!("{error:?}");
        assert!(text.contains("was not found in PATH"), "{text}");
        assert!(text.contains("resolve_command"), "{text}");

        let program = lower_source(r#"run(["sh", "-c", "true"], cwd="missing-dir")"#)?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("missing cwd should fail before spawn");
        let text = format!("{error:?}");
        assert!(text.contains("cwd"), "{text}");
        assert!(text.contains("does not exist"), "{text}");

        fs::write(root.join("not-a-dir"), "file").expect("write cwd file");
        let program = lower_source(r#"run(["sh", "-c", "true"], cwd="not-a-dir")"#)?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("file cwd should fail before spawn");
        let text = format!("{error:?}");
        assert!(text.contains("not a directory"), "{text}");

        let program = lower_source(
            r#"start_daemon(["sh", "-c", "true"], stdout="missing/log.out", stderr="log.err")"#,
        )?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("missing stdout parent should fail before spawn");
        let text = format!("{error:?}");
        assert!(text.contains("stdout path"), "{text}");
        assert!(text.contains("parent directory is missing"), "{text}");
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_daemon_builtins() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("daemon-builtins")?;
        let program = lower_source(
            r#"daemon = start_daemon(
    ["sh", "-c", "while true; do sleep 1; done"],
    stdout="daemon.out",
    stderr="daemon.err",
)
status = daemon_status(daemon, log="daemon.err")
closed = wait_port(9, timeout_ms=20)
stopped = stop_daemon(daemon, timeout_ms=1000)
after = daemon_status(daemon)
emit({
    "started": daemon["ok"],
    "pid_positive": daemon["pid"] > 0,
    "running": status["running"],
    "status_ok": status["ok"],
    "closed_ok": closed["ok"],
    "closed_kind": closed["kind"],
    "stopped": stopped["ok"],
    "after_running": after["running"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "started": true,
                "pid_positive": true,
                "running": true,
                "status_ok": true,
                "closed_ok": false,
                "closed_kind": "timeout",
                "stopped": true,
                "after_running": false,
            })
        );
        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn find_records_flow_into_read_helpers_and_positional_sort_key() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("find-record-read")?;
        fs::write(
            root.join("records_1.jsonl"),
            "{\"name\":\"a\",\"score\":2}\n{\"name\":\"b\",\"score\":5}\n",
        )
        .expect("write jsonl");
        fs::write(root.join("notes.txt"), "ignored\n").expect("write text");

        let program = lower_source(
            r#"files = find(".", "records_*.jsonl")
rows = read_jsonl(files[0])
ordered = sort(rows, "score", reverse=True)
emit({
    "file_count": len(files),
    "file_name": files[0]["name"],
    "top": ordered[0]["name"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "file_count": 1,
                "file_name": "records_1.jsonl",
                "top": "b",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn find_supports_path_type_size_and_mtime_filters() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("find-rich-filters")?;
        fs::create_dir_all(root.join("pkg/sub")).expect("create nested dirs");
        fs::write(root.join("pkg/main.py"), "print('main')\n").expect("write main");
        fs::write(root.join("pkg/sub/test.py"), "print('test')\n").expect("write test");
        fs::write(root.join("pkg/sub/notes.txt"), "").expect("write notes");

        let program = lower_source(
            r#"py = find(".", path_glob="**/*.py", type="file", min_size=1)
small = find(".", path_glob="**/*.txt", type="file", max_size=0)
old = find(".", type="file", modified_before_ms=1)
emit({
    "py_names": sort([file["name"] for file in py]),
    "small_names": [file["name"] for file in small],
    "old_count": len(old),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "py_names": ["main.py", "test.py"],
                "small_names": ["notes.txt"],
                "old_count": 0,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn diff_returns_structured_hunks() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("diff-builtin")?;
        fs::write(root.join("before.txt"), "alpha\nbeta\ngamma\n").expect("write before");
        fs::write(root.join("after.txt"), "alpha\nbravo\ngamma\ndelta\n").expect("write after");

        let program = lower_source(
            r#"changes = diff("before.txt", "after.txt")
emit({
    "changed": changes["changed"],
    "hunk_count": len(changes["hunks"]),
    "old_start": changes["hunks"][0]["old_start"],
    "new_start": changes["hunks"][0]["new_start"],
    "first_kind": changes["hunks"][0]["lines"][0]["kind"],
    "first_text": changes["hunks"][0]["lines"][0]["text"],
    "last_kind": changes["hunks"][1]["lines"][0]["kind"],
    "last_text": changes["hunks"][1]["lines"][0]["text"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "changed": true,
                "hunk_count": 2,
                "old_start": 2,
                "new_start": 2,
                "first_kind": "-",
                "first_text": "beta",
                "last_kind": "+",
                "last_text": "delta",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn recognizes_fused_map_update_if_shapes() -> Result<(), ShellError> {
        let program = lower_source(
            r#"
if user in user_amounts:
    user_amounts[user] = user_amounts[user] + record["amount"]
    user_items[user] = user_items[user] + record["items"]
else:
    user_amounts[user] = record["amount"]
    user_items[user] = record["items"]
    users.append(user)
if tag in tag_counts:
    tag_counts[tag] += 1
else:
    tag_counts[tag] = 1
    tags.append(tag)
"#,
        )?;
        let first = match &program.statements[0] {
            crate::stone_ast::Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => match_fused_map_update_if(condition, then_branch, else_branch)
                .expect("gold-style map update should fuse"),
            _ => panic!("expected first if"),
        };
        assert_eq!(first.key_name, "user");
        assert_eq!(first.contains_map, "user_amounts");
        assert_eq!(first.updates.len(), 2);
        assert_eq!(first.append_list.as_deref(), Some("users"));

        let second = match &program.statements[1] {
            crate::stone_ast::Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => match_fused_map_update_if(condition, then_branch, else_branch)
                .expect("augassign map update should fuse"),
            _ => panic!("expected second if"),
        };
        assert_eq!(second.key_name, "tag");
        assert_eq!(second.contains_map, "tag_counts");
        assert_eq!(second.updates.len(), 1);
        assert_eq!(second.append_list.as_deref(), Some("tags"));
        Ok(())
    }

    #[test]
    fn bounded_structured_reads_reject_negative_limits() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("bounded-read-limit")?;
        fs::write(root.join("input.jsonl"), "{\"name\":\"a\"}\n").expect("write jsonl");
        let program = lower_source(r#"read_jsonl("input.jsonl", -1)"#)?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("negative limit should fail");
        let text = format!("{error:?}");
        assert!(text.contains("limit must be non-negative"), "{text}");

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_json_file_and_text_helpers() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("json-helpers")?;
        fs::write(root.join("input.json"), r#"{"items":[1,2],"ok":true}"#).expect("write json");
        fs::write(
            root.join("input.jsonl"),
            "{\"name\":\"a\",\"amount\":1.5}\n\n{\"name\":\"b\",\"amount\":2.25}\n",
        )
        .expect("write jsonl");

        let program = lower_source(
            r#"data = read_json("input.json")
input_rows = read_jsonl("input.jsonl")
encoded = json_dumps({"ok": data["ok"], "count": len(data["items"])})
decoded = json_loads(encoded)
rows = [{"name": "a"}, {"name": "b"}]
json_bytes = write_json("out.json", decoded)
jsonl_bytes = write_jsonl("rows.jsonl", rows)
emit({
    "decoded": decoded,
    "json": read_json("out.json"),
    "json_bytes": json_bytes,
    "jsonl": cat("rows.jsonl"),
    "jsonl_bytes": jsonl_bytes,
    "input_rows": len(input_rows),
    "input_amount": input_rows[0]["amount"] + input_rows[1]["amount"]
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "decoded": {"ok": true, "count": 2},
                "json": {"ok": true, "count": 2},
                "json_bytes": 31,
                "jsonl": "{\"name\":\"a\"}\n{\"name\":\"b\"}\n",
                "jsonl_bytes": 26,
                "input_rows": 2,
                "input_amount": 3.75,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn write_helpers_create_parent_directories() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("nested-write-helpers")?;
        let program = lower_source(
            r#"write_json("data/out.json", {"ok": True})
write_jsonl("logs/rows.jsonl", [{"row": 1}])
f = open("text/report.txt", "w")
f.write("done")
f.close()
emit({
    "json": read_json("data/out.json")["ok"],
    "jsonl": cat("logs/rows.jsonl"),
    "text": cat("text/report.txt")
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "json": true,
                "jsonl": "{\"row\":1}\n",
                "text": "done",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn read_jsonl_reports_line_numbers() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("invalid-jsonl")?;
        fs::write(root.join("bad.jsonl"), "{\"ok\":true}\nnot-json\n").expect("write jsonl");
        let program = lower_source(
            r#"read_jsonl("bad.jsonl")
"#,
        )?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("invalid jsonl should fail");
        let text = format!("{error:?}");
        assert!(
            text.contains("bad.jsonl line 2"),
            "unexpected error: {text}"
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_str_index_and_local_item_assignment() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("item-assignment")?;
        let program = lower_source(
            r#"row = {}
row["region"] = "west"
row["qty"] = str(12)
row["count"] = 1
row["count"] += 2
items = ["alpha", "beta"]
items[1] = "bee"
positions = []
for i, item in enumerate(items):
    positions.append(str(i) + ":" + item)
keys = []
for key in row:
    keys.append(key)
json_lines = map(json_dumps, [row])
emit({
    "row": row,
    "item": items[1],
    "next_index": items.index("bee") + 1,
    "item_index": items.index("bee"),
    "substring_index": "alpha beta".index("beta"),
    "substring_from": "alpha beta beta".index("beta", 7),
    "substring_find": "alpha beta".find("gamma"),
    "substring_find_from": "alpha beta beta".find("beta", 7),
    "trimmed": "[svc]:".strip("[]:"),
    "positions": positions,
    "keys": keys,
    "json_line": json_lines[0]
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "row": {"region": "west", "qty": "12", "count": 3},
                "item": "bee",
                "next_index": 2,
                "item_index": 1,
                "substring_index": 6,
                "substring_from": 11,
                "substring_find": -1,
                "substring_find_from": 11,
                "trimmed": "svc",
                "positions": ["0:alpha", "1:bee"],
                "keys": ["region", "qty", "count"],
                "json_line": "{\"count\":3,\"qty\":\"12\",\"region\":\"west\"}",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_simple_list_comprehensions() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("list-comprehension")?;
        let program = lower_source(
            r#"items = [" 1 ", "", " 2", "skip", "3 "]
numbers = [int(text.strip()) for text in items if text.strip() and text.strip() != "skip"]
outer = "kept"
trimmed = [outer + ":" + text.strip() for text in items if text.strip()]
emit({"numbers": numbers, "trimmed": trimmed, "outer": outer})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "numbers": [1, 2, 3],
                "trimmed": ["kept:1", "kept:2", "kept:skip", "kept:3"],
                "outer": "kept",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_numeric_subtraction_and_division() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("numeric-sub-div")?;
        let program = lower_source(
            r#"total = 0
total += 10 - 3
avg = total / 2
emit({
    "difference": 10 - 3,
    "average": avg,
    "mod": 78 % 100,
    "float_mod": 5.5 % 2.0,
    "rounded": round(1.2345, 2),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "difference": 7,
                "average": 3.5,
                "mod": 78,
                "float_mod": 1.5,
                "rounded": 1.23,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    fn test_engine(name: &str) -> Result<(EngineState, Stack, PathBuf), ShellError> {
        let root = test_root(name);
        fs::create_dir_all(&root).expect("create root");

        let mut engine_state = EngineState::new();
        super::super::register_engine_commands(&mut engine_state)?;

        let mut stack = Stack::new();
        stack.set_cwd(&root)?;

        Ok((engine_state, stack, root))
    }

    #[cfg(not(target_os = "hermit"))]
    fn write_helper(root: &std::path::Path, name: &str, source: &str) {
        let helper_dir = root.join(".stone/helpers");
        fs::create_dir_all(&helper_dir).expect("create helper dir");
        fs::write(helper_dir.join(name), source).expect("write helper");
    }

    fn eval_program_hot_loop(
        engine_state: &EngineState,
        stack: &mut Stack,
        program: &crate::stone_ast::Program,
    ) -> Result<PipelineData, ShellError> {
        eval_program_with_options(
            engine_state,
            stack,
            program,
            PipelineData::empty(),
            EvalOptions {
                hot_loop_enabled: true,
                hot_loop_vm_interpreter: true,
                hot_loop_validate_snapshot: false,
                session: None,
            },
        )
        .map(|output| output.pipeline)
    }

    fn assert_hot_loop_matches_baseline(
        name: &str,
        source: &str,
        files: &[(&str, &str)],
    ) -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine(&format!("{name}-baseline"))?;
        for (path, content) in files {
            fs::write(root.join(path), content).expect("write baseline fixture");
        }
        let program = lower_source(source)?;
        let baseline = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let baseline = json::pipeline_to_json_value(baseline, nu_protocol::Span::unknown())?;
        cleanup_dir(&root);

        let (engine_state, mut stack, root) = test_engine(&format!("{name}-hot"))?;
        for (path, content) in files {
            fs::write(root.join(path), content).expect("write hot fixture");
        }
        let hot = eval_program_hot_loop(&engine_state, &mut stack, &program)?;
        let hot = json::pipeline_to_json_value(hot, nu_protocol::Span::unknown())?;
        cleanup_dir(&root);

        assert_eq!(hot, baseline, "{name}");
        Ok(())
    }

    fn test_root(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        path.push(format!("waymark-stone-eval-{name}-{nanos}"));
        path
    }

    fn cleanup_dir(path: &PathBuf) {
        if path.exists() {
            fs::remove_dir_all(path).expect("cleanup test dir");
        }
    }
}
