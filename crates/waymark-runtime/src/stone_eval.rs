// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{HashMap, HashSet};
use std::env;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use nu_protocol::{
    engine::{EngineState, Stack},
    shell_error::generic::GenericError,
    IntoPipelineData, PipelineData, Record, ShellError, Span, Value,
};
use serde_json::{json, Value as JsonValue};
use waymark_gateway_client::proto::{
    attempt_program, ArtifactProgram, AttemptProgram, BuiltinWorkflow, CapabilityRequest,
    ContextSource, StoneProgram, TaskSpec as GatewayTaskSpec, WorkspaceSource,
};

use crate::agent::{AgentAction, AgentSession, ReactAgentControl, ScriptedAgentControl};
use crate::commands::{stone_help_overview, stone_help_topic};
use crate::gateway_env;
use crate::gateway_runtime;
use crate::json::{json_to_nu_value, nu_to_json_value};
use crate::linux_tools::posix_tools;
#[cfg(all(not(target_os = "hermit"), test))]
use crate::linux_tools::process::cleanup_stale_run_temp_files;
use crate::linux_tools::process::{
    run_call_values, run_status_call_values, run_terminate_call_values, run_wait_call_values,
};
#[cfg(not(target_os = "hermit"))]
use crate::linux_tools::{
    daemon::{
        daemon_status_call_values, start_daemon_call_values, stop_daemon_call_values,
        wait_port_call_values,
    },
    process::resolve_command_call_values,
};
use crate::stone_agent_control::{AgentControlKind, AgentControlValue};
use crate::stone_ast::{
    AssignTarget, AugOp, BoolOp, Call, CompareOp, ComprehensionClause, ExceptHandler, Expr,
    FormattedStringPart, FunctionDef, Program, Stmt, StoneFormatSpec, StoneType,
};
use crate::stone_attempt_scope::AttemptScopeValue;
use crate::stone_attempt_value::{AttemptHandleValue, AttemptOutcomeValue};
use crate::stone_builtins::{
    add_values, bitwise_int_values, compare_values, div_values, enumerate_builtin,
    find_method_builtin, first_builtin, float_builtin, floor_div_values, format_builtin,
    index_method_builtin, int_builtin, join_builtin, last_builtin, len_builtin, list_builtin,
    map_builtin_value, min_max_builtin, mod_values, mul_values, neg_value, normalize_index,
    parse_float_builtin, parse_int_builtin, range_builtin, record_method_builtin, round_builtin,
    set_builtin, shift_value, slice_builtin, sort_builtin_values, sort_key_for_value,
    split_builtin, starts_with_builtin, str_builtin, string_method_builtin, sub_values,
    subscript_builtin, sum_builtin, unique_builtin, value_identity_key, value_to_bool,
    value_to_display_string, value_to_f64, value_to_i64, value_to_limit, value_to_operator_i64,
    value_to_path_string, value_to_string, value_to_u64, value_truthy, value_type_name,
    where_builtin, where_compare_builtin, zfill_text,
};
use crate::stone_file_ops::{
    cat_text, create_dir_all, diff_record_for_paths, edit_text_file, find_records, io_stone_error,
    list_dir_records, open_runtime_file, read_bytes_for_jsonl, read_csv_file, read_json_file,
    read_text as stone_read_text, remove_path, save_value_file, search_records, stat_record,
    write_json_file, write_jsonl_file, write_text as stone_write_text, RuntimeFile,
    StoneFindOptions,
};
use crate::stone_hash::hash_builtin;
#[cfg(not(target_os = "hermit"))]
use crate::stone_helpers::{
    helper_error_observation, stone_helper_observations_from_value, stone_helper_registry,
    stone_run_event_from_record, stone_run_event_value, StoneHelperHandlerKind, StoneHelperHook,
    StoneHelperRegistry, StoneRunEvent,
};
use crate::stone_vm::{
    match_hot_jsonl_aggregation_body, match_outer_jsonl_file_loop_body, try_lower_generic_loop,
    try_lower_hot_loop, ConstId, GenericLoopIter, GenericLoopOp, GenericLoopPlan, HotLoopIter,
    HotLoopPlan, LoopIrFusedKernel, LoopIrOptimizationDiagnostic, StoneConst, StoneIrFunction,
    StoneLoopIrOptimizationResult,
};
use crate::StoneGuest;

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
#[path = "stone_vm/jsonl_runtime.rs"]
mod stone_vm_jsonl_runtime;
#[path = "stone_vm/runtime.rs"]
mod stone_vm_runtime;
use stone_functions::CallableValue;
pub(crate) use stone_functions::StoneSession;
use stone_json_view::{
    eval_json_object_view_method, eval_runtime_subscript, json_array_view_iter_values,
    json_object_view_get, json_scalar_view_to_f64, json_scalar_view_to_i64, jsonl_row_view,
    jsonl_row_views, jsonl_rows_from_bytes, materialize_json_array_view,
    materialize_json_object_view, materialize_json_scalar_view, materialize_jsonl_rows,
    runtime_value_to_string_key, JsonlRows,
};
use stone_runtime_value::{FileHandle, RuntimeValue, TextLines};
use stone_state::runtime_state_record;

const STONE_LAST_RESULT_ENV: &str = "WAYMARK_LAST_RESULT_JSON";

#[derive(Default)]
pub struct EvalState {
    scopes: Vec<Scope>,
    functions: HashMap<String, FunctionDef>,
    next_file_id: u64,
    next_callable_id: u64,
    attempt_scopes: Vec<AttemptScopeValue>,
    current_program_source: Option<String>,
    current_program_entrypoint: Option<String>,
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
    eval_program_with_output_and_session(engine_state, stack, program, input, None, None, None)
}

pub(crate) fn eval_program_with_output_and_session(
    engine_state: &EngineState,
    stack: &mut Stack,
    program: &Program,
    input: PipelineData,
    session: Option<&mut StoneSession>,
    source: Option<&str>,
    entrypoint: Option<&str>,
) -> Result<EvalProgramOutput, ShellError> {
    eval_program_with_source_options(
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
        source,
        entrypoint,
    )
}

struct EvalOptions<'a> {
    hot_loop_enabled: bool,
    hot_loop_vm_interpreter: bool,
    hot_loop_validate_snapshot: bool,
    session: Option<&'a mut StoneSession>,
}

#[cfg(test)]
fn eval_program_with_options(
    engine_state: &EngineState,
    stack: &mut Stack,
    program: &Program,
    input: PipelineData,
    options: EvalOptions<'_>,
) -> Result<EvalProgramOutput, ShellError> {
    eval_program_with_source_options(engine_state, stack, program, input, options, None, None)
}

fn eval_program_with_source_options(
    engine_state: &EngineState,
    stack: &mut Stack,
    program: &Program,
    input: PipelineData,
    mut options: EvalOptions<'_>,
    source: Option<&str>,
    entrypoint: Option<&str>,
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
            attempt_scopes: Vec::new(),
            current_program_source: source.map(str::to_string),
            current_program_entrypoint: entrypoint.map(str::to_string),
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
    let pipeline = match entrypoint {
        Some(entrypoint) if !entrypoint.is_empty() => {
            evaluator.eval_entrypoint_program(program, input, entrypoint)
        }
        _ => evaluator.eval_program(program, input),
    };
    let cleanup = evaluator.close_open_attempt_scopes("Stone evaluation ended");
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
    let pipeline = match (pipeline, cleanup) {
        (Ok(pipeline), Ok(())) => pipeline,
        (Ok(_), Err(cleanup_error)) => return Err(cleanup_error),
        (Err(program_error), Ok(())) => return Err(program_error),
        (Err(program_error), Err(cleanup_error)) => {
            return Err(attach_cleanup_error(program_error, cleanup_error));
        }
    };
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
    Return(RuntimeValue),
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

    fn eval_entrypoint_program(
        &mut self,
        program: &Program,
        input: PipelineData,
        entrypoint: &str,
    ) -> Result<PipelineData, ShellError> {
        let mut module_functions = Vec::new();
        for (index, statement) in program.statements.iter().enumerate() {
            match statement {
                Stmt::FunctionDef(function) => module_functions.push(function.clone()),
                Stmt::Pass => {}
                _ => {
                    return Err(stone_error(
                        "entrypoint",
                        format!(
                            "named-entrypoint modules currently allow only top-level def and pass; executable statement {} would run ambiguously",
                            index + 1
                        ),
                    ));
                }
            }
        }
        let function = module_functions
            .iter()
            .find(|function| function.name == entrypoint)
            .cloned()
            .ok_or_else(|| {
                let mut names = module_functions
                    .iter()
                    .map(|function| function.name.clone())
                    .collect::<Vec<_>>();
                names.sort();
                stone_error(
                    "entrypoint",
                    format!(
                        "unknown Stone entrypoint `{entrypoint}`; available entrypoints: {}",
                        if names.is_empty() {
                            "none".to_string()
                        } else {
                            names.join(", ")
                        }
                    ),
                )
            })?;
        for function in module_functions {
            self.state.functions.insert(function.name.clone(), function);
        }
        if function.params.len() > 1 {
            return Err(stone_error(
                "entrypoint",
                format!(
                    "{}() has {} parameters; a Stone entrypoint must accept zero arguments or one structured task input",
                    function.name,
                    function.params.len()
                ),
            ));
        }

        const ENTRY_INPUT: &str = "__waymark_entrypoint_input";
        let positional = if function.params.is_empty() {
            Vec::new()
        } else {
            let input = input.into_value(Span::unknown())?;
            self.state
                .set_local(ENTRY_INPUT.to_string(), RuntimeValue::Nu(input));
            vec![Expr::Name(ENTRY_INPUT.to_string())]
        };
        let call = Call {
            name: entrypoint.to_string(),
            positional,
            named: Vec::new(),
        };
        let result = self.eval_user_function_call(&call);
        self.state.remove_local(ENTRY_INPUT);
        result?
            .into_nu_value("entrypoint result")
            .map(IntoPipelineData::into_pipeline_data)
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
                let value = apply_aug_op(*op, &left, &right)?;
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
                    Some(value) => self.eval_expr_value(value, input)?,
                    None => RuntimeValue::Nu(Value::nothing(Span::unknown())),
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
            Stmt::Try { body, handlers } => self.eval_try_stmt(body, handlers),
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

    fn eval_try_stmt(
        &mut self,
        body: &[Stmt],
        handlers: &[ExceptHandler],
    ) -> Result<EvalFlow, ShellError> {
        match self.eval_block(body, PipelineData::empty(), false) {
            Ok(flow) => Ok(flow),
            Err(err) => {
                let Some(handler) = handlers.first() else {
                    return Err(err);
                };
                let error_record = shell_error_record(&err);
                self.state.push_scope();
                if let Some(name) = &handler.name {
                    self.state
                        .set_local(name.clone(), RuntimeValue::Nu(error_record));
                }
                let result = self.eval_block(&handler.body, PipelineData::empty(), false);
                self.state
                    .pop_scope_merging_nu_locals(handler.name.as_deref())?;
                result
            }
        }
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
        elt: &Expr,
        clauses: &[ComprehensionClause],
    ) -> Result<RuntimeValue, ShellError> {
        let mut output = Vec::new();
        self.eval_comprehension_clauses("list comprehension", clauses, 0, &mut |evaluator| {
            output.push(
                evaluator
                    .eval_expr_value(elt, PipelineData::empty())?
                    .into_nu_value("list comprehension")?,
            );
            Ok(false)
        })?;
        Ok(RuntimeValue::Nu(Value::list(output, Span::unknown())))
    }

    fn eval_dict_comprehension(
        &mut self,
        key: &Expr,
        value: &Expr,
        clauses: &[ComprehensionClause],
    ) -> Result<RuntimeValue, ShellError> {
        let mut record = Record::new();
        self.eval_comprehension_clauses("dict comprehension", clauses, 0, &mut |evaluator| {
            let key = evaluator
                .eval_expr_value(key, PipelineData::empty())?
                .into_nu_value("dict comprehension")?;
            let key = value_to_string(&key, "dict comprehension key")?;
            let value = evaluator
                .eval_expr_value(value, PipelineData::empty())?
                .into_nu_value("dict comprehension")?;
            record.push(key, value);
            Ok(false)
        })?;
        Ok(RuntimeValue::Nu(Value::record(record, Span::unknown())))
    }

    fn eval_comprehension_clauses(
        &mut self,
        context: &str,
        clauses: &[ComprehensionClause],
        index: usize,
        visitor: &mut impl FnMut(&mut Self) -> Result<bool, ShellError>,
    ) -> Result<bool, ShellError> {
        let Some(clause) = clauses.get(index) else {
            return visitor(self);
        };
        let previous = self.capture_target_values(&clause.targets);
        let values = self.eval_iterable_expr(&clause.iter)?;
        for item in values {
            self.assign_loop_targets(&clause.targets, item)?;
            let mut keep = true;
            for filter in &clause.filters {
                let value = self
                    .eval_expr_value(filter, PipelineData::empty())?
                    .into_nu_value(context)?;
                if !value_truthy(&value) {
                    keep = false;
                    break;
                }
            }
            if keep && self.eval_comprehension_clauses(context, clauses, index + 1, visitor)? {
                self.restore_target_values(previous);
                return Ok(true);
            }
        }
        self.restore_target_values(previous);
        Ok(false)
    }

    fn capture_target_values(&self, targets: &[String]) -> Vec<(String, Option<RuntimeValue>)> {
        targets
            .iter()
            .map(|target| (target.clone(), self.state.get_local(target)))
            .collect()
    }

    fn restore_target_values(&mut self, previous: Vec<(String, Option<RuntimeValue>)>) {
        for (target, value) in previous {
            match value {
                Some(value) => self.state.set_local(target, value),
                None => self.state.remove_local(&target),
            }
        }
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
                Expr::ListComprehension { elt, clauses } => {
                    self.eval_list_comprehension(elt, clauses)
                }
                Expr::DictComprehension {
                    key,
                    value,
                    clauses,
                } => self.eval_dict_comprehension(key, value, clauses),
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
                Expr::Conditional {
                    then_expr,
                    condition,
                    else_expr,
                } => {
                    let condition = self
                        .eval_expr_value(condition, PipelineData::empty())?
                        .into_nu_value("conditional expression")?;
                    if value_truthy(&condition) {
                        self.eval_expr_value(then_expr, PipelineData::empty())
                    } else {
                        self.eval_expr_value(else_expr, PipelineData::empty())
                    }
                }
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
                    let value = value_to_operator_i64(&value, "bitwise invert")?;
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
                    "generator expressions are only supported inside sum(...), any(...), all(...), join(...), and set(...) for now; use a list comprehension like [expr for x in items] when you need a list",
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
                    named,
                } => self.eval_method_call(receiver, method, positional, named),
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
            "list" | "tuple" => self.eval_list_call(call),
            "repr" | "str" => {
                let name = call.name.as_str();
                let [arg] = call.positional.as_slice() else {
                    return Err(stone_error(
                        name,
                        format!("{name}() requires exactly one argument"),
                    ));
                };
                if !call.named.is_empty() {
                    return Err(stone_error(
                        name,
                        format!("{name}() keyword arguments are not supported"),
                    ));
                }
                let value = self
                    .eval_expr_value(arg, PipelineData::empty())?
                    .into_nu_value(name)?;
                str_builtin(&value).map(RuntimeValue::Nu)
            }
            "md5" | "sha1" | "sha256" => self.eval_hash_call(call),
            "type" => self.eval_type_call(call),
            "any" => self.eval_any_all_call(call, true),
            "all" => self.eval_any_all_call(call, false),
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
            "model_call" => self.eval_model_call_call(call),
            "task_spec" => self.eval_task_spec_call(call),
            "task_input" => self.eval_task_input_call(call),
            "agent_session" => self.eval_agent_session_call(call),
            "current_program" => self.eval_current_program_call(call),
            "react_control" => self.eval_react_control_call(call),
            "scripted_control" => self.eval_scripted_control_call(call),
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
            "must_run" => self.eval_must_run_call(call),
            "run_status" => self.eval_run_status_call(call),
            "run_wait" => self.eval_run_wait_call(call),
            "run_terminate" => self.eval_run_terminate_call(call),
            "resolve_command" => self.eval_resolve_command_call(call),
            "ps" | "process_list" => self.eval_ps_call(call),
            "sys" | "sys_info" | "sysinfo" => self.eval_sys_call(call),
            "state" => self.eval_state_call(call),
            "attempt_info" => self.eval_attempt_info_call(call),
            "attempt_start" => self.eval_attempt_start_call(call),
            "attempt_wait" => self.eval_attempt_wait_call(call),
            "attempt_join" => self.eval_attempt_join_call(call),
            "attempt_wait_any" => self.eval_attempt_wait_set_call(call, false),
            "attempt_wait_all" => self.eval_attempt_wait_set_call(call, true),
            "attempt_terminate" => self.eval_attempt_terminate_call(call),
            "attempt_scope" => self.eval_attempt_scope_call(call),
            "attempt_scope_add" => self.eval_attempt_scope_add_call(call),
            "attempt_scope_close" => self.eval_attempt_scope_close_call(call),
            "attempt_state" => self.eval_attempt_state_call(call),
            "attempts" | "attempt_list" => self.eval_attempts_call(call),
            "attempt_spawn" => self.eval_attempt_spawn_call(call),
            "attempt_fork" => self.eval_attempt_fork_call(call),
            "attempt_finish" => self.eval_attempt_finish_call(call),
            "attempt_report" => self.eval_attempt_report_call(call),
            "attempt_accept" => self.eval_attempt_accept_call(call),
            "attempt_discard" => self.eval_attempt_discard_call(call),
            "attempt_publish" => self.eval_attempt_publish_call(call),
            "attempt_run_process" => self.eval_attempt_run_process_call(call),
            "env_state" | "env_diff" => self.eval_env_state_call(call),
            "env_tx_info" => self.eval_env_tx_info_call(call),
            "env_txs" => self.eval_env_txs_call(call),
            "env_finish" => self.eval_env_finish_call(call),
            "env_restore" => self.eval_env_restore_call(call),
            "env_checkpoint" => self.eval_env_checkpoint_call(call),
            "env_fork" => self.eval_env_fork_call(call),
            "env_restore_checkpoint" => self.eval_env_restore_checkpoint_call(call),
            "env_checkpoints" => self.eval_env_checkpoints_call(call),
            "env_checkpoint_gc" => self.eval_env_checkpoint_gc_call(call),
            "env_discard_checkpoint" => self.eval_env_discard_checkpoint_call(call),
            "env_run_checkpoint" => self.eval_env_run_checkpoint_call(call),
            "env_commit" => self.eval_env_commit_call(call),
            "env_rollback" => self.eval_env_rollback_call(call),
            "last_result" => self.eval_last_result_call(call),
            "start_daemon" => self.eval_start_daemon_call(call),
            "starts_with" | "startswith" => self.eval_starts_with_call(call),
            "daemon_status" => self.eval_daemon_status_call(call),
            "stop_daemon" => self.eval_stop_daemon_call(call),
            "wait_port" => self.eval_wait_port_call(call),
            "wait_for" => self.eval_wait_for_call(call),
            "sort" | "sorted" => self.eval_sort_call(call),
            "set" => self.eval_set_call(call),
            "unique" => self.eval_unique_call(call),
            "write_csv" => self.eval_write_csv_call(call),
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
        let required = function
            .params
            .iter()
            .filter(|param| param.default.is_none())
            .count();
        if call.positional.len() < required || call.positional.len() > function.params.len() {
            return Err(stone_error(
                "function call",
                format!(
                    "{}() expected {} to {} argument(s), got {}",
                    function.name,
                    required,
                    function.params.len(),
                    call.positional.len()
                ),
            ));
        }

        let mut args = Vec::with_capacity(function.params.len());
        for (index, param) in function.params.iter().enumerate() {
            let value = match call.positional.get(index) {
                Some(expr) => self.eval_expr_value(expr, PipelineData::empty())?,
                None => self.eval_expr_value(
                    param.default.as_ref().ok_or_else(|| {
                        stone_error(
                            "function call",
                            format!("missing required argument `{}`", param.name),
                        )
                    })?,
                    PipelineData::empty(),
                )?,
            };
            ensure_runtime_type(&value, param.ty, &format!("argument `{}`", param.name))?;
            args.push((param.name.clone(), value));
        }

        self.state.push_scope();
        for (name, value) in args {
            self.state.set_local(name, value);
        }
        let flow = self.eval_block(&function.body, PipelineData::empty(), false);
        self.state.pop_scope()?;
        let value = match flow? {
            EvalFlow::Return(value) => value,
            EvalFlow::Output(_) => RuntimeValue::Nu(Value::nothing(Span::unknown())),
            EvalFlow::Break => return Err(stone_error("break", "break outside loop")),
            EvalFlow::Continue => return Err(stone_error("continue", "continue outside loop")),
        };
        ensure_runtime_type(
            &value,
            function.return_type,
            &format!("{}() return value", function.name),
        )?;
        Ok(value)
    }

    fn eval_named_callable_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.named.is_empty() {
            return Err(stone_error(
                "callable",
                "lambda calls only support positional arguments for now",
            ));
        }
        let callable = self
            .state
            .get_local(&call.name)
            .ok_or_else(|| unknown_stone_call_error(&call.name))?;
        let args = call
            .positional
            .iter()
            .map(|arg| self.eval_expr_value(arg, PipelineData::empty()))
            .collect::<Result<Vec<_>, _>>()?;
        match callable {
            RuntimeValue::Callable(callable) => self.invoke_callable(&callable, args),
            RuntimeValue::AgentControl(control) => self.invoke_agent_control(&control, args),
            _ => Err(stone_error(
                "callable",
                format!("{} is not callable", call.name),
            )),
        }
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
        if let RuntimeValue::AttemptScope(scope) = receiver {
            let state = scope.lock()?;
            let value = match attr {
                "id" => Value::int(scope.scope_id as i64, Span::unknown()),
                "exit_policy" => Value::string(state.exit_policy.clone(), Span::unknown()),
                "join_timeout_ms" => Value::int(state.join_timeout_ms as i64, Span::unknown()),
                "closed" => Value::bool(state.closed, Span::unknown()),
                "children" => Value::list(
                    state
                        .children
                        .iter()
                        .map(|child| Value::string(child.attempt.clone(), Span::unknown()))
                        .collect(),
                    Span::unknown(),
                ),
                _ => {
                    return Err(stone_error(
                        "attribute",
                        format!("attempt_scope has no attribute `{attr}`"),
                    ));
                }
            };
            return Ok(RuntimeValue::Nu(value));
        }
        if let RuntimeValue::AttemptHandle(handle) = receiver {
            return handle.attribute(attr).map(RuntimeValue::Nu);
        }
        if let RuntimeValue::AttemptOutcome(outcome) = receiver {
            return outcome.attribute(attr).map(RuntimeValue::Nu);
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
        let (path_expr, max_bytes_expr) = match call.positional.as_slice() {
            [] => (None, None),
            [path] => (Some(path), None),
            [path, max_bytes] => (Some(path), Some(max_bytes)),
            _ => {
                return Err(stone_error(
                    "read_text",
                    "read_text() requires a path and optional max_bytes",
                ));
            }
        };
        let mut path_value = None;
        let mut max_bytes = 1_048_576;
        let mut start_line: Option<usize> = None;
        let mut end_line: Option<usize> = None;
        if let Some(max_bytes_expr) = max_bytes_expr {
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
                "path" => {
                    if path_expr.is_some() {
                        return Err(stone_error(
                            "read_text",
                            "read_text() path was provided twice",
                        ));
                    }
                    path_value = Some(value_to_path_string(&value, "read_text path")?);
                }
                "max_bytes" | "limit" => max_bytes = value_to_limit(&value, "read_text max_bytes")?,
                "start_line" => start_line = Some(value_to_limit(&value, "read_text start_line")?),
                "end_line" => end_line = Some(value_to_limit(&value, "read_text end_line")?),
                other => {
                    return Err(stone_error(
                        "read_text",
                        format!(
                            "unsupported keyword `{other}`; expected path, max_bytes, limit, start_line, or end_line"
                        ),
                    ));
                }
            }
        }

        if let Some(path_expr) = path_expr {
            let value = self
                .eval_expr_value(path_expr, PipelineData::empty())?
                .into_nu_value("read_text")?;
            path_value = Some(value_to_path_string(&value, "read_text")?);
        }
        let Some(path) = path_value else {
            return Err(stone_error("read_text", "read_text() requires a path"));
        };
        let target = self.resolve_script_path(&path)?;
        let text = stone_read_text(&target, max_bytes)?;
        let text = if start_line.is_some() || end_line.is_some() {
            slice_text_lines(&text, start_line, end_line)?
        } else {
            text
        };
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
        let (root_expr, rest) = match call.positional.as_slice() {
            [root, rest @ ..] => (Some(root), rest),
            [] => (None, &[][..]),
        };
        if root_expr.is_none()
            && !call
                .named
                .iter()
                .any(|(name, _)| matches!(name.as_str(), "root" | "path"))
        {
            return Err(stone_error(
                "find",
                "find() requires root/path and optional name_glob arguments",
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
        let mut root_path = None;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("find")?;
            match name.as_str() {
                "root" | "path" => root_path = Some(value_to_path_string(&value, "find root")?),
                "name" | "name_glob" => {
                    options.name_glob = Some(value_to_string(&value, "find name_glob")?)
                }
                "name_contains" => {
                    options.name_contains = Some(value_to_string(&value, "find name_contains")?)
                }
                "glob" | "path_glob" => {
                    options.path_glob = Some(value_to_string(&value, "find path_glob")?)
                }
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
                "max_depth" => options.max_depth = Some(value_to_limit(&value, "find max_depth")?),
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
                            "unsupported keyword `{other}`; expected root/path, name/name_glob, name_contains, glob/path_glob, type, max_depth, min_size, max_size, modified_after_ms, or modified_before_ms"
                        ),
                    ));
                }
            }
        }

        if let Some(root_expr) = root_expr {
            let value = self
                .eval_expr_value(root_expr, PipelineData::empty())?
                .into_nu_value("find")?;
            root_path = Some(value_to_path_string(&value, "find")?);
        }
        let Some(root) = root_path else {
            return Err(stone_error("find", "find() requires a root/path argument"));
        };
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

    fn eval_write_csv_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let ([path, rows] | [path, rows, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "write_csv",
                "write_csv() requires path, rows, and optional columns",
            ));
        };
        let mut named_columns = None;
        for (name, argument) in &call.named {
            if name == "columns" && named_columns.is_none() {
                named_columns = Some(argument);
            } else {
                return Err(stone_error(
                    "write_csv",
                    format!("unsupported keyword `{name}`; expected columns"),
                ));
            }
        }
        if named_columns.is_some() && call.positional.len() > 2 {
            return Err(stone_error(
                "write_csv",
                "write_csv() got multiple columns values",
            ));
        }
        let path = self
            .eval_expr_value(path, PipelineData::empty())?
            .into_nu_value("write_csv")?;
        let path = value_to_string(&path, "write_csv")?;
        let target = self.resolve_script_path(&path)?;
        let rows = self
            .eval_expr_value(rows, PipelineData::empty())?
            .into_nu_value("write_csv")?;
        let columns = match call.positional.as_slice() {
            [_, _, columns] => Some(
                self.eval_expr_value(columns, PipelineData::empty())?
                    .into_nu_value("write_csv columns")?,
            ),
            _ => named_columns
                .map(|expr| self.eval_expr_value(expr, PipelineData::empty()))
                .transpose()?
                .map(|value| value.into_nu_value("write_csv columns"))
                .transpose()?,
        };
        let text = csv_text_from_rows(&rows, columns.as_ref())?;
        Ok(RuntimeValue::Nu(stone_write_text(&target, &text, false)?))
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
        let mut force = false;
        let mut path_values = Vec::new();
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("rm")?;
            match name.as_str() {
                "path" | "paths" => path_values.push(value),
                "force" | "missing_ok" => force = value_to_bool(&value, "rm force")?,
                "recursive" | "recurse" => {
                    let _ = value_to_bool(&value, "rm recursive")?;
                }
                other => {
                    return Err(stone_error(
                        "rm",
                        format!(
                            "unsupported keyword `{other}`; expected path, paths, force, missing_ok, recursive, or recurse"
                        ),
                    ));
                }
            }
        }
        for argument in &call.positional {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("rm")?;
            path_values.push(value);
        }
        if path_values.is_empty() {
            return Err(stone_error("rm", "rm() requires at least one path"));
        }
        for value in path_values {
            let values = match value {
                Value::List { vals, .. } => vals,
                other => vec![other],
            };
            for value in values {
                let path = value_to_path_string(&value, "rm")?;
                let target = self.resolve_script_path(&path)?;
                if force && !target.exists() {
                    continue;
                }
                remove_path(&target)?;
            }
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
        let (root_expr, needle_expr) = match call.positional.as_slice() {
            [root, needle] => (Some(root), Some(needle)),
            [] => (None, None),
            _ => {
                return Err(stone_error(
                    "search",
                    "search() requires root and needle arguments",
                ));
            }
        };
        let mut root = None;
        let mut needle = None;
        let mut regex = false;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("search")?;
            match name.as_str() {
                "root" | "path" => root = Some(value_to_path_string(&value, "search root")?),
                "needle" | "query" | "pattern" => {
                    needle = Some(value_to_string(&value, "search needle")?)
                }
                "regex" => regex = value_to_bool(&value, "search regex")?,
                other => {
                    return Err(stone_error(
                        "search",
                        format!(
                            "unsupported keyword `{other}`; expected root/path, needle/query/pattern, or regex"
                        ),
                    ));
                }
            }
        }
        if let Some(root_expr) = root_expr {
            let value = self
                .eval_expr_value(root_expr, PipelineData::empty())?
                .into_nu_value("search")?;
            root = Some(value_to_path_string(&value, "search")?);
        }
        if let Some(needle_expr) = needle_expr {
            let value = self
                .eval_expr_value(needle_expr, PipelineData::empty())?
                .into_nu_value("search")?;
            needle = Some(value_to_string(&value, "search needle")?);
        }
        let Some(root) = root else {
            return Err(stone_error(
                "search",
                "search() requires a root/path argument",
            ));
        };
        let Some(needle) = needle else {
            return Err(stone_error(
                "search",
                "search() requires a needle/query/pattern argument",
            ));
        };
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
        let mut indent = None;
        for (name, argument) in &call.named {
            match name.as_str() {
                "indent" => {
                    let value = self
                        .eval_expr_value(argument, PipelineData::empty())?
                        .into_nu_value("json_dumps")?;
                    if matches!(value, Value::Nothing { .. }) {
                        indent = None;
                    } else {
                        let value = value_to_limit(&value, "json_dumps indent")?;
                        if value != 2 {
                            return Err(stone_error(
                                "json_dumps",
                                "json_dumps() currently supports only indent=2",
                            ));
                        }
                        indent = Some(value);
                    }
                }
                "separators" => {
                    let value = self
                        .eval_expr_value(argument, PipelineData::empty())?
                        .into_nu_value("json_dumps")?;
                    validate_json_dumps_separators(&value)?;
                }
                other => {
                    return Err(stone_error(
                        "json_dumps",
                        format!("unsupported keyword `{other}`; expected indent or separators"),
                    ));
                }
            }
        }
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value("json_dumps")?;
        let json = nu_to_json_value(&value);
        let text = if indent.is_some() {
            serde_json::to_string_pretty(&json)
        } else {
            serde_json::to_string(&json)
        }
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
            [Expr::Generator { .. }] => {
                self.eval_generator_values(call.positional.first().unwrap())?
            }
            [iterable] => self
                .eval_iterable_expr(iterable)?
                .into_iter()
                .map(|value| value.into_nu_value("set"))
                .collect::<Result<Vec<_>, _>>()?,
            _ => unreachable!(),
        };
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

    fn eval_attempt_call_values(
        &mut self,
        call: &Call,
    ) -> Result<(Vec<Value>, Vec<(String, Value)>, Option<AttemptScopeValue>), ShellError> {
        let mut positional = Vec::with_capacity(call.positional.len());
        for argument in &call.positional {
            positional.push(
                self.eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value(&call.name)?,
            );
        }
        let mut named = Vec::with_capacity(call.named.len());
        let mut scope = None;
        for (name, argument) in &call.named {
            let value = self.eval_expr_value(argument, PipelineData::empty())?;
            if name == "scope" {
                let RuntimeValue::AttemptScope(value) = value else {
                    return Err(stone_error(
                        &call.name,
                        format!(
                            "scope must be an attempt_scope, got {}",
                            runtime_type_name(&value)
                        ),
                    ));
                };
                if scope.replace(value).is_some() {
                    return Err(stone_error(&call.name, "scope may be supplied only once"));
                }
            } else {
                named.push((name.clone(), value.into_nu_value(&call.name)?));
            }
        }
        Ok((positional, named, scope))
    }

    fn current_cwd_path(&mut self, context: &str) -> Result<PathBuf, ShellError> {
        self.engine_state
            .cwd_as_string(Some(self.stack))
            .map(PathBuf::from)
            .map_err(|err| stone_error(context, err.to_string()))
    }

    fn eval_run_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let record = self.eval_run_record(call, "run")?;
        Ok(RuntimeValue::Nu(Value::record(record, Span::unknown())))
    }

    fn eval_must_run_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let record = self.eval_run_record(call, "must_run")?;
        if run_record_ok(&record) {
            Ok(RuntimeValue::Nu(Value::record(record, Span::unknown())))
        } else {
            Err(must_run_failure_error(record))
        }
    }

    fn eval_run_record(&mut self, call: &Call, context: &str) -> Result<Record, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        let default_cwd = self.current_cwd_path(context)?;
        let invocation = run_call_values(context, &positional, &named, default_cwd, |path| {
            self.resolve_script_path(path)
        })?;
        let mut record = invocation.record;
        #[cfg(not(target_os = "hermit"))]
        self.attach_run_helper_observations(
            &mut record,
            &invocation.argv,
            &invocation.cwd,
            &invocation.env_overrides,
            Span::unknown(),
        );
        Ok(record)
    }

    fn eval_run_wait_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        run_wait_call_values(&positional, &named).map(RuntimeValue::Nu)
    }

    fn eval_run_status_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        run_status_call_values(&positional, &named).map(RuntimeValue::Nu)
    }

    fn eval_run_terminate_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        run_terminate_call_values(&positional, &named).map(RuntimeValue::Nu)
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
                let value = value.into_nu_value("helper return value")?;
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
            if gateway_runtime::enabled() {
                if positional.len() != 1 || !named.is_empty() {
                    return Err(stone_error(
                        "resolve_command",
                        "resolve_command() requires exactly one command name",
                    ));
                }
                let name = value_to_string(&positional[0], "resolve_command")?;
                let cwd = self.current_cwd_path("resolve_command")?;
                return gateway_runtime::resolve_command_record(&name, &cwd).map(RuntimeValue::Nu);
            }
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
        let state = runtime_state_record(Path::new(&cwd));
        if !gateway_env::enabled() {
            return Ok(RuntimeValue::Nu(state));
        }

        let span = Span::unknown();
        let mut record = match state {
            Value::Record { val, .. } => (*val).clone(),
            other => {
                let mut record = Record::new();
                record.push("runtime", other);
                record
            }
        };
        record.push("gateway", gateway_env::env_state(50)?);
        Ok(RuntimeValue::Nu(Value::record(record, span)))
    }

    fn eval_model_call_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        let [messages] = positional.as_slice() else {
            return Err(model_call_input_error(
                "model_call() requires exactly one positional messages list",
            ));
        };
        let Value::List { vals, .. } = messages else {
            return Err(model_call_input_error(format!(
                "model_call messages must be a list of records, got {}",
                messages.get_type()
            )));
        };
        if vals.is_empty() {
            return Err(model_call_input_error(
                "model_call messages list must not be empty",
            ));
        }

        let mut message_values = Vec::with_capacity(vals.len());
        for (index, value) in vals.iter().enumerate() {
            let Value::Record { val, .. } = value else {
                return Err(model_call_input_error(format!(
                    "model_call message {index} must be a record, got {}",
                    value.get_type()
                )));
            };
            for key in val.columns() {
                if !matches!(key.as_str(), "role" | "content" | "name") {
                    return Err(model_call_input_error(format!(
                        "unsupported model_call message field {key:?}; expected role, content, or name"
                    )));
                }
            }
            let role = val.get("role").ok_or_else(|| {
                model_call_input_error(format!("model_call message {index} requires role"))
            })?;
            let role = value_to_string(role, "model_call message role")
                .map_err(|err| model_call_input_error(err.to_string()))?;
            if !matches!(role.as_str(), "system" | "user" | "assistant") {
                return Err(model_call_input_error(format!(
                    "unsupported model_call message role {role:?}; expected system, user, or assistant"
                )));
            }
            let content = val.get("content").ok_or_else(|| {
                model_call_input_error(format!("model_call message {index} requires content"))
            })?;
            let content = value_to_string(content, "model_call message content")
                .map_err(|err| model_call_input_error(err.to_string()))?;
            let mut message = serde_json::Map::new();
            message.insert("role".to_string(), JsonValue::String(role));
            message.insert("content".to_string(), JsonValue::String(content));
            if let Some(name) = val.get("name") {
                if !matches!(name, Value::Nothing { .. }) {
                    let name = value_to_string(name, "model_call message name")
                        .map_err(|err| model_call_input_error(err.to_string()))?;
                    message.insert("name".to_string(), JsonValue::String(name));
                }
            }
            message_values.push(JsonValue::Object(message));
        }

        let mut request = serde_json::Map::new();
        request.insert("messages".to_string(), JsonValue::Array(message_values));
        let mut seen = HashSet::new();
        for (name, value) in named {
            if !seen.insert(name.clone()) {
                return Err(model_call_input_error(format!(
                    "duplicate model_call keyword argument `{name}`"
                )));
            }
            if matches!(value, Value::Nothing { .. }) {
                continue;
            }
            let json_value = match name.as_str() {
                "model_class" | "model" => JsonValue::String(
                    value_to_string(&value, &format!("model_call {name}"))
                        .map_err(|err| model_call_input_error(err.to_string()))?,
                ),
                "temperature" | "top_p" => {
                    let number = match value {
                        Value::Int { val, .. } => val as f64,
                        Value::Float { val, .. } => val,
                        other => {
                            return Err(model_call_input_error(format!(
                                "model_call {name} must be a number, got {}",
                                other.get_type()
                            )));
                        }
                    };
                    if !number.is_finite() || number < 0.0 {
                        return Err(model_call_input_error(format!(
                            "model_call {name} must be a finite non-negative number"
                        )));
                    }
                    if name == "top_p" && (number <= 0.0 || number > 1.0) {
                        return Err(model_call_input_error(
                            "model_call top_p must be greater than 0 and at most 1 with the current Gateway protocol",
                        ));
                    }
                    serde_json::Number::from_f64(number)
                        .map(JsonValue::Number)
                        .ok_or_else(|| {
                            model_call_input_error(format!(
                                "model_call {name} must be a finite JSON number"
                            ))
                        })?
                }
                "seed" | "max_output_tokens" => {
                    let number = match value {
                        Value::Int { val, .. } if val >= 0 => u64::try_from(val).unwrap_or(0),
                        Value::Int { .. } => {
                            return Err(model_call_input_error(format!(
                                "model_call {name} must be non-negative"
                            )));
                        }
                        other => {
                            return Err(model_call_input_error(format!(
                                "model_call {name} must be an integer, got {}",
                                other.get_type()
                            )));
                        }
                    };
                    let number = u32::try_from(number).map_err(|_| {
                        model_call_input_error(format!(
                            "model_call {name} exceeds the unsigned 32-bit limit"
                        ))
                    })?;
                    if name == "seed" && number == 0 {
                        return Err(model_call_input_error(
                            "model_call seed must be positive with the current Gateway protocol",
                        ));
                    }
                    JsonValue::Number(serde_json::Number::from(number))
                }
                "response_format" => match value {
                    value @ (Value::String { .. } | Value::Record { .. }) => {
                        nu_to_json_value(&value)
                    }
                    other => {
                        return Err(model_call_input_error(format!(
                            "model_call response_format must be a string or record, got {}",
                            other.get_type()
                        )));
                    }
                },
                "metadata" => {
                    let Value::Record { val, .. } = &value else {
                        return Err(model_call_input_error(format!(
                            "model_call metadata must be a record, got {}",
                            value.get_type()
                        )));
                    };
                    for (key, metadata_value) in val.iter() {
                        if !matches!(metadata_value, Value::String { .. } | Value::Glob { .. }) {
                            return Err(model_call_input_error(format!(
                                "model_call metadata field {key:?} must be a string, got {}",
                                metadata_value.get_type()
                            )));
                        }
                    }
                    nu_to_json_value(&value)
                }
                other => {
                    return Err(model_call_input_error(format!(
                        "unexpected model_call keyword argument `{other}`; expected model_class, model, temperature, top_p, seed, max_output_tokens, response_format, or metadata"
                    )));
                }
            };
            request.insert(name, json_value);
        }

        gateway_runtime::model_call_value(&JsonValue::Object(request), Span::unknown())
            .map(RuntimeValue::Nu)
    }

    fn eval_task_spec_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.positional.is_empty() || !call.named.is_empty() {
            return Err(stone_error("task_spec", "task_spec() accepts no arguments"));
        }
        gateway_runtime::task_spec_value(Span::unknown()).map(RuntimeValue::Nu)
    }

    fn eval_task_input_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.positional.is_empty() || !call.named.is_empty() {
            return Err(stone_error(
                "task_input",
                "task_input() accepts no arguments",
            ));
        }
        gateway_runtime::task_input_value(Span::unknown()).map(RuntimeValue::Nu)
    }

    fn eval_agent_session_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.positional.is_empty() || !call.named.is_empty() {
            return Err(stone_error(
                "agent_session",
                "agent_session() accepts no arguments",
            ));
        }

        let span = Span::unknown();
        let task = gateway_runtime::task_spec_value(span)?;
        let input = gateway_runtime::task_input_value(span)?;
        let attempt = gateway_env::attempt_info(String::new())?;
        Ok(RuntimeValue::Nu(agent_session_value(
            task, input, attempt, span,
        )))
    }

    fn eval_react_control_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "react_control",
                "react_control() accepts at most one positional model argument",
            ));
        }
        let mut model = positional
            .first()
            .map(|value| value_to_string(value, "react_control model"))
            .transpose()?
            .filter(|value| !value.is_empty());
        let mut max_rounds = 16;
        let mut max_turns = 16;
        let mut max_tool_ms = None;
        let mut completion_path = None;
        for (name, value) in named {
            match name.as_str() {
                "model" => {
                    model = value_to_string(&value, "react_control model")
                        .map(|value| (!value.is_empty()).then_some(value))?
                }
                "max_rounds" => max_rounds = value_to_limit(&value, "react_control max_rounds")?,
                "max_turns" => max_turns = value_to_limit(&value, "react_control max_turns")?,
                "max_tool_ms" => {
                    max_tool_ms = if matches!(value, Value::Nothing { .. }) {
                        None
                    } else {
                        Some(value_to_u64(&value, "react_control max_tool_ms")?)
                    }
                }
                "completion_path" => {
                    completion_path = value_to_string(&value, "react_control completion_path")
                        .map(|value| (!value.is_empty()).then_some(value))?
                }
                _ => {
                    return Err(stone_error(
                        "react_control",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        if max_rounds == 0 || max_turns == 0 {
            return Err(stone_error(
                "react_control",
                "max_rounds and max_turns must be greater than zero",
            ));
        }
        Ok(RuntimeValue::AgentControl(AgentControlValue {
            control_id: self.state.next_callable_id(),
            kind: AgentControlKind::React { model },
            max_rounds,
            max_turns,
            max_tool_ms,
            completion_path,
        }))
    }

    fn eval_current_program_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "current_program",
                "current_program() accepts at most one entrypoint argument",
            ));
        }
        let mut entrypoint = positional
            .first()
            .map(|value| value_to_string(value, "current_program entrypoint"))
            .transpose()?
            .or_else(|| self.state.current_program_entrypoint.clone())
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "entrypoint" => entrypoint = value_to_string(&value, "current_program entrypoint")?,
                _ => {
                    return Err(stone_error(
                        "current_program",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let source = self.state.current_program_source.clone().ok_or_else(|| {
            stone_error(
                "current_program",
                "the current evaluator was not created from Stone source",
            )
        })?;
        let span = Span::unknown();
        let mut stone = Record::new();
        stone.push("source", Value::string(source, span));
        stone.push("entrypoint", Value::string(entrypoint, span));
        let mut program = Record::new();
        program.push("kind", Value::string("stone", span));
        program.push("stone", Value::record(stone, span));
        Ok(RuntimeValue::Nu(Value::record(program, span)))
    }

    fn eval_scripted_control_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [actions] = call.positional.as_slice() else {
            return Err(stone_error(
                "scripted_control",
                "scripted_control() requires exactly one actions list",
            ));
        };
        let actions = self
            .eval_expr_value(actions, PipelineData::empty())?
            .into_nu_value("scripted_control actions")?;
        let actions = nu_to_json_value(&actions);
        let Some(actions) = actions.as_array() else {
            return Err(stone_error(
                "scripted_control",
                "scripted_control actions must be a list of action records",
            ));
        };
        let actions = actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                AgentAction::from_json(action).map_err(|error| {
                    stone_error(
                        "scripted_control",
                        format!("actions[{index}]: {}", error.message),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut max_turns = 16;
        let mut max_tool_ms = None;
        let mut completion_path = None;
        for (name, expression) in &call.named {
            let value = self
                .eval_expr_value(expression, PipelineData::empty())?
                .into_nu_value("scripted_control option")?;
            match name.as_str() {
                "max_turns" => max_turns = value_to_limit(&value, "scripted_control max_turns")?,
                "max_tool_ms" => {
                    max_tool_ms = if matches!(value, Value::Nothing { .. }) {
                        None
                    } else {
                        Some(value_to_u64(&value, "scripted_control max_tool_ms")?)
                    }
                }
                "completion_path" => {
                    completion_path = value_to_string(&value, "scripted_control completion_path")
                        .map(|value| (!value.is_empty()).then_some(value))?
                }
                _ => {
                    return Err(stone_error(
                        "scripted_control",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        if max_turns == 0 {
            return Err(stone_error(
                "scripted_control",
                "max_turns must be greater than zero",
            ));
        }
        Ok(RuntimeValue::AgentControl(AgentControlValue {
            control_id: self.state.next_callable_id(),
            kind: AgentControlKind::Scripted { actions },
            max_rounds: 0,
            max_turns,
            max_tool_ms,
            completion_path,
        }))
    }

    fn invoke_agent_control(
        &mut self,
        control: &AgentControlValue,
        mut args: Vec<RuntimeValue>,
    ) -> Result<RuntimeValue, ShellError> {
        if args.len() != 1 {
            return Err(stone_error(
                "agent control",
                format!(
                    "{}#{} expected one session argument, got {}",
                    control.name(),
                    control.control_id,
                    args.len()
                ),
            ));
        }
        let session = args
            .pop()
            .expect("agent control argument length checked")
            .into_nu_value("agent control session")?;
        let session = nu_to_json_value(&session);
        let cwd = self.current_cwd_path("agent control")?;
        let tools = crate::task::agent_control_tools(&cwd, control.max_tool_ms);
        let mut guest = StoneGuest::new(cwd)?;
        let mut runtime = AgentSession::new(tools)
            .with_max_turns(control.max_turns)
            .with_max_rounds(control.max_rounds.max(1))
            .with_completion_path(control.completion_path.clone());

        let result = match &control.kind {
            AgentControlKind::Scripted { actions } => {
                let mut builtin = ScriptedAgentControl::new(actions.clone());
                runtime.run_control(&mut guest, &mut builtin, None)
            }
            AgentControlKind::React { model } => {
                let task = agent_task_prompt(&session)?;
                let mut builtin = ReactAgentControl::new(task, model.as_deref());
                match gateway_runtime::GatewayAgentModelGateway::active() {
                    Some(mut gateway) => {
                        runtime.run_control(&mut guest, &mut builtin, Some(&mut gateway))
                    }
                    None => runtime.run_control(&mut guest, &mut builtin, None),
                }
            }
        };
        Ok(RuntimeValue::Nu(json_to_nu_value(
            result.to_json(),
            Span::unknown(),
        )))
    }

    fn eval_ps_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                &call.name,
                format!("{}() accepts at most one interval_ms argument", call.name),
            ));
        }
        let mut interval_ms = 0_u64;
        if let Some(value) = positional.first() {
            interval_ms = value_to_u64(value, "ps interval_ms")?;
        }
        for (name, value) in named {
            match name.as_str() {
                "interval_ms" => interval_ms = value_to_u64(&value, "ps interval_ms")?,
                _ => {
                    return Err(stone_error(
                        &call.name,
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        if gateway_runtime::enabled() {
            let cwd = self.current_cwd_path(&call.name)?;
            return gateway_runtime::ps_record(interval_ms, &cwd).map(RuntimeValue::Nu);
        }
        Ok(RuntimeValue::Nu(posix_tools::ps_record(interval_ms)))
    }

    fn eval_sys_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 || !named.is_empty() {
            return Err(stone_error(
                &call.name,
                format!("{}() accepts at most one section argument", call.name),
            ));
        }
        let section = positional
            .first()
            .map(|value| value_to_string(value, &call.name))
            .transpose()?;
        if gateway_runtime::enabled() {
            let cwd = self.current_cwd_path(&call.name)?;
            return gateway_runtime::sysinfo_record(section.as_deref(), &cwd).map(RuntimeValue::Nu);
        }
        posix_tools::sysinfo_record(section.as_deref())
            .map(RuntimeValue::Nu)
            .map_err(|err| stone_error(&call.name, err))
    }

    fn eval_attempt_info_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "attempt_info",
                "attempt_info() accepts at most one attempt argument",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_info attempt"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_info attempt")?,
                _ => {
                    return Err(stone_error(
                        "attempt_info",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::attempt_info(attempt).map(RuntimeValue::Nu)
    }

    fn eval_attempt_start_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "attempt_start",
                "attempt_start() accepts at most attempt argument",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_start attempt"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_start attempt")?,
                _ => {
                    return Err(stone_error(
                        "attempt_start",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::attempt_start(attempt).map(RuntimeValue::Nu)
    }

    fn eval_attempt_state_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_state",
                "attempt_state() accepts at most attempt and sample_limit arguments",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_state attempt"))
            .transpose()?
            .unwrap_or_default();
        let mut sample_limit = positional
            .get(1)
            .map(|value| {
                u32::try_from(value_to_u64(value, "attempt_state sample_limit")?)
                    .map_err(|_| stone_error("attempt_state", "sample_limit is too large"))
            })
            .transpose()?
            .unwrap_or(100);
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_state attempt")?,
                "sample_limit" => {
                    sample_limit =
                        u32::try_from(value_to_u64(&value, "attempt_state sample_limit")?)
                            .map_err(|_| {
                                stone_error("attempt_state", "sample_limit is too large")
                            })?;
                }
                _ => {
                    return Err(stone_error(
                        "attempt_state",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::attempt_state(attempt, sample_limit).map(RuntimeValue::Nu)
    }

    fn eval_attempts_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 3 {
            return Err(stone_error(
                &call.name,
                format!(
                    "{}() accepts at most task, workspace, and state arguments",
                    call.name
                ),
            ));
        }
        let mut task = positional
            .first()
            .map(|value| value_to_string(value, "attempts task"))
            .transpose()?
            .unwrap_or_default();
        let mut workspace = positional
            .get(1)
            .map(|value| value_to_string(value, "attempts workspace"))
            .transpose()?
            .unwrap_or_default();
        let mut state = positional
            .get(2)
            .map(|value| value_to_string(value, "attempts state"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "task" => task = value_to_string(&value, "attempts task")?,
                "workspace" => workspace = value_to_string(&value, "attempts workspace")?,
                "state" => state = value_to_string(&value, "attempts state")?,
                _ => {
                    return Err(stone_error(
                        &call.name,
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::attempts(task, workspace, state).map(RuntimeValue::Nu)
    }

    fn eval_attempt_spawn_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named, scope) = self.eval_attempt_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_spawn",
                "attempt_spawn() accepts at most task and workspace positional arguments",
            ));
        }
        let mut task = positional
            .first()
            .map(|value| value_to_string(value, "attempt_spawn task"))
            .transpose()?;
        let mut workspace = positional
            .get(1)
            .map(|value| value_to_string(value, "attempt_spawn workspace"))
            .transpose()?;
        let mut controller = String::new();
        let mut capability_profile = String::new();
        let mut container = String::new();
        let mut workspace_mount = String::new();
        let mut parent_attempt = String::new();
        let mut resource_limits = Vec::new();
        let mut metadata = Vec::new();
        let mut entrypoint = String::new();
        let mut spawn_v1 = gateway_env::AttemptSpawnV1::default();
        for (name, value) in named {
            match name.as_str() {
                "task" => task = Some(value_to_string(&value, "attempt_spawn task")?),
                "workspace" => {
                    workspace = Some(value_to_string(&value, "attempt_spawn workspace")?)
                }
                "controller" => controller = value_to_string(&value, "attempt_spawn controller")?,
                "capability_profile" => {
                    capability_profile =
                        value_to_string(&value, "attempt_spawn capability_profile")?
                }
                "container" => container = value_to_string(&value, "attempt_spawn container")?,
                "workspace_mount" => {
                    workspace_mount = value_to_string(&value, "attempt_spawn workspace_mount")?
                }
                "parent_attempt" => {
                    parent_attempt = attempt_id_from_value(&value, "attempt_spawn parent_attempt")?
                }
                "resource_limits" | "limits" => {
                    resource_limits =
                        value_to_string_pairs(&value, "attempt_spawn resource_limits")?
                }
                "metadata" | "meta" => {
                    metadata = value_to_string_pairs(&value, "attempt_spawn metadata")?
                }
                "task_spec" => {
                    let spec = task_spec_from_value(&value, "attempt_spawn task_spec")?;
                    if task.is_none() && !spec.id.is_empty() {
                        task = Some(spec.id.clone());
                    }
                    spawn_v1.task_spec = Some(spec);
                }
                "task_input" => {
                    spawn_v1.task_input_json = serde_json::to_string(&nu_to_json_value(&value))
                        .map_err(|error| {
                            stone_error(
                                "attempt_spawn task_input",
                                format!("failed to encode task input: {error}"),
                            )
                        })?;
                }
                "program" => {
                    spawn_v1.program = Some(program_from_value(&value, "attempt_spawn program")?);
                }
                "entrypoint" => entrypoint = value_to_string(&value, "attempt_spawn entrypoint")?,
                "workspace_source" => {
                    let source =
                        workspace_source_from_value(&value, "attempt_spawn workspace_source")?;
                    if workspace.is_none() && !source.workspace.is_empty() {
                        workspace = Some(source.workspace.clone());
                    }
                    spawn_v1.workspace_source = Some(source);
                }
                "context_source" => {
                    spawn_v1.context_source = Some(context_source_from_value(
                        &value,
                        "attempt_spawn context_source",
                    )?);
                }
                "capabilities" => {
                    spawn_v1.capabilities = Some(CapabilityRequest {
                        values: value_to_string_pairs(&value, "attempt_spawn capabilities")?
                            .into_iter()
                            .collect(),
                    });
                }
                "start" => spawn_v1.start = value_to_bool(&value, "attempt_spawn start")?,
                _ => {
                    return Err(stone_error(
                        "attempt_spawn",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let task = task.ok_or_else(|| stone_error("attempt_spawn", "missing task argument"))?;
        let workspace =
            workspace.ok_or_else(|| stone_error("attempt_spawn", "missing workspace argument"))?;
        apply_stone_entrypoint(
            &mut spawn_v1.program,
            &entrypoint,
            "attempt_spawn entrypoint",
        )?;
        let child = gateway_env::attempt_spawn(
            task,
            workspace,
            controller,
            capability_profile,
            container,
            workspace_mount,
            parent_attempt,
            resource_limits,
            metadata,
            spawn_v1,
        )?;
        let attempt = attempt_id_from_value(&child, "attempt_spawn result")?;
        if let Some(scope) = scope {
            scope.register(attempt.clone())?;
        }
        Ok(RuntimeValue::AttemptHandle(AttemptHandleValue::new(
            attempt, child,
        )))
    }

    fn eval_attempt_fork_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named, scope) = self.eval_attempt_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "attempt_fork",
                "attempt_fork() accepts at most one parent_attempt positional argument",
            ));
        }
        let mut parent_attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_fork parent_attempt"))
            .transpose()?
            .unwrap_or_default();
        let mut task = String::new();
        let mut controller = String::new();
        let mut capability_profile = String::new();
        let mut container = String::new();
        let mut workspace_mount = String::new();
        let mut resource_limits = Vec::new();
        let mut metadata = Vec::new();
        let mut task_input_json = String::new();
        let mut program = None;
        let mut entrypoint = String::new();
        let mut start = false;
        for (name, value) in named {
            match name.as_str() {
                "parent_attempt" | "attempt" => {
                    parent_attempt = attempt_id_from_value(&value, "attempt_fork parent_attempt")?
                }
                "task" => task = value_to_string(&value, "attempt_fork task")?,
                "controller" => controller = value_to_string(&value, "attempt_fork controller")?,
                "capability_profile" => {
                    capability_profile = value_to_string(&value, "attempt_fork capability_profile")?
                }
                "container" => container = value_to_string(&value, "attempt_fork container")?,
                "workspace_mount" => {
                    workspace_mount = value_to_string(&value, "attempt_fork workspace_mount")?
                }
                "resource_limits" | "limits" => {
                    resource_limits = value_to_string_pairs(&value, "attempt_fork resource_limits")?
                }
                "metadata" | "meta" => {
                    metadata = value_to_string_pairs(&value, "attempt_fork metadata")?
                }
                "input" | "task_input" => {
                    task_input_json =
                        serde_json::to_string(&nu_to_json_value(&value)).map_err(|error| {
                            stone_error(
                                "attempt_fork",
                                format!("failed to encode task input: {error}"),
                            )
                        })?
                }
                "program" => program = Some(program_from_value(&value, "attempt_fork program")?),
                "entrypoint" => entrypoint = value_to_string(&value, "attempt_fork entrypoint")?,
                "start" => start = value_to_bool(&value, "attempt_fork start")?,
                _ => {
                    return Err(stone_error(
                        "attempt_fork",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        apply_stone_entrypoint(&mut program, &entrypoint, "attempt_fork entrypoint")?;
        let child = gateway_env::attempt_fork(
            parent_attempt,
            task,
            controller,
            capability_profile,
            container,
            workspace_mount,
            resource_limits,
            metadata,
            task_input_json,
            program,
            start,
        )?;
        let attempt = attempt_id_from_value(&child, "attempt_fork result")?;
        if let Some(scope) = scope {
            scope.register(attempt.clone())?;
        }
        Ok(RuntimeValue::AttemptHandle(AttemptHandleValue::new(
            attempt, child,
        )))
    }

    fn eval_attempt_finish_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_finish",
                "attempt_finish() accepts at most action and attempt positional arguments",
            ));
        }
        let mut action = positional
            .first()
            .map(|value| value_to_string(value, "attempt_finish action"))
            .transpose()?;
        let mut attempt = positional
            .get(1)
            .map(|value| attempt_id_from_value(value, "attempt_finish attempt"))
            .transpose()?
            .unwrap_or_default();
        let mut message = String::new();
        let mut reason = String::new();
        let mut allow_risky = false;
        for (name, value) in named {
            match name.as_str() {
                "action" => action = Some(value_to_string(&value, "attempt_finish action")?),
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_finish attempt")?,
                "message" => message = value_to_string(&value, "attempt_finish message")?,
                "reason" => reason = value_to_string(&value, "attempt_finish reason")?,
                "allow_risky" => allow_risky = value_to_bool(&value, "attempt_finish allow_risky")?,
                _ => {
                    return Err(stone_error(
                        "attempt_finish",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let action =
            action.ok_or_else(|| stone_error("attempt_finish", "missing action argument"))?;
        gateway_env::attempt_finish(attempt, action, message, reason, allow_risky)
            .map(RuntimeValue::Nu)
    }

    fn eval_attempt_wait_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_wait",
                "attempt_wait() accepts at most attempt and timeout_ms positional arguments",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_wait attempt"))
            .transpose()?
            .unwrap_or_default();
        let mut timeout_ms = positional
            .get(1)
            .map(|value| value_to_u64(value, "attempt_wait timeout_ms"))
            .transpose()?
            .map(|value| {
                u32::try_from(value)
                    .map_err(|_| stone_error("attempt_wait", "timeout_ms is too large"))
            })
            .transpose()?;
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_wait attempt")?,
                "timeout_ms" => {
                    timeout_ms = Some(
                        u32::try_from(value_to_u64(&value, "attempt_wait timeout_ms")?)
                            .map_err(|_| stone_error("attempt_wait", "timeout_ms is too large"))?,
                    )
                }
                _ => {
                    return Err(stone_error(
                        "attempt_wait",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::attempt_wait(attempt, timeout_ms).map(RuntimeValue::Nu)
    }

    fn eval_attempt_join_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_join",
                "attempt_join() accepts at most attempt and timeout_ms arguments",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_join attempt"))
            .transpose()?
            .unwrap_or_default();
        let mut timeout_ms = positional
            .get(1)
            .map(|value| value_to_optional_timeout(value, "attempt_join timeout_ms"))
            .transpose()?
            .flatten();
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_join attempt")?,
                "timeout_ms" => {
                    timeout_ms = value_to_optional_timeout(&value, "attempt_join timeout_ms")?
                }
                _ => {
                    return Err(stone_error(
                        "attempt_join",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        if attempt.is_empty() {
            return Err(stone_error(
                "attempt_join",
                "attempt_join() requires a child attempt",
            ));
        }
        let waited = gateway_env::attempt_wait(attempt.clone(), timeout_ms)?;
        let state = attempt_record_state(&waited)
            .unwrap_or("unknown")
            .to_string();
        let controller_state = attempt_controller_state(&waited)
            .unwrap_or("unknown")
            .to_string();
        let joined = matches!(
            controller_state.as_str(),
            "exited" | "failed" | "terminated"
        );
        if joined {
            for scope in &self.state.attempt_scopes {
                scope.mark_joined(&attempt)?;
            }
        }
        Ok(RuntimeValue::AttemptOutcome(AttemptOutcomeValue {
            attempt,
            joined,
            timed_out: !joined,
            state,
            controller_state,
            record: waited,
        }))
    }

    fn eval_attempt_wait_set_call(
        &mut self,
        call: &Call,
        wait_all: bool,
    ) -> Result<RuntimeValue, ShellError> {
        if call.positional.len() > 2 {
            return Err(stone_error(
                &call.name,
                format!(
                    "{}() accepts at most children and timeout_ms arguments",
                    call.name
                ),
            ));
        }
        let mut children = call
            .positional
            .first()
            .map(|expr| self.eval_attempt_set_expr(expr, &call.name))
            .transpose()?;
        let mut timeout_ms = call
            .positional
            .get(1)
            .map(|expr| {
                self.eval_expr_value(expr, PipelineData::empty())?
                    .into_nu_value(&call.name)
                    .and_then(|value| value_to_optional_timeout(&value, &call.name))
            })
            .transpose()?
            .flatten();
        for (name, expr) in &call.named {
            match name.as_str() {
                "children" | "attempts" | "scope" => {
                    if children.is_some() {
                        return Err(stone_error(
                            &call.name,
                            "children may be supplied only once",
                        ));
                    }
                    children = Some(self.eval_attempt_set_expr(expr, &call.name)?);
                }
                "timeout_ms" => {
                    let value = self
                        .eval_expr_value(expr, PipelineData::empty())?
                        .into_nu_value(&call.name)?;
                    timeout_ms = value_to_optional_timeout(&value, &call.name)?;
                }
                _ => {
                    return Err(stone_error(
                        &call.name,
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let children = children
            .ok_or_else(|| stone_error(&call.name, format!("{}() requires children", call.name)))?;
        if children.is_empty() {
            return Err(stone_error(
                &call.name,
                "attempt wait set requires at least one child",
            ));
        }
        let waited = gateway_env::attempt_wait_set(children.clone(), wait_all, timeout_ms)?;
        let mut outcomes = Vec::with_capacity(waited.ready.len());
        for record in waited.ready {
            let attempt = attempt_id_from_value(&record, "attempt wait set result")?;
            for scope in &self.state.attempt_scopes {
                scope.mark_joined(&attempt)?;
            }
            outcomes.push(attempt_outcome_value(attempt, record));
        }
        if !wait_all {
            return Ok(RuntimeValue::AttemptOutcome(
                outcomes.into_iter().next().unwrap_or(AttemptOutcomeValue {
                    attempt: String::new(),
                    joined: false,
                    timed_out: waited.timed_out,
                    state: "waiting".to_string(),
                    controller_state: "running".to_string(),
                    record: Value::nothing(Span::unknown()),
                }),
            ));
        }

        let ready_ids = outcomes
            .iter()
            .map(|outcome| outcome.attempt.as_str())
            .collect::<HashSet<_>>();
        let pending = children
            .into_iter()
            .filter(|attempt| !ready_ids.contains(attempt.as_str()))
            .map(|attempt| Value::string(attempt, Span::unknown()))
            .collect();
        let span = Span::unknown();
        let mut result = Record::new();
        result.push("completed", Value::bool(waited.completed, span));
        result.push("timed_out", Value::bool(waited.timed_out, span));
        result.push(
            "outcomes",
            Value::list(
                outcomes
                    .into_iter()
                    .map(|outcome| outcome.materialize())
                    .collect(),
                span,
            ),
        );
        result.push("pending", Value::list(pending, span));
        Ok(RuntimeValue::Nu(Value::record(result, span)))
    }

    fn eval_attempt_set_expr(
        &mut self,
        expr: &Expr,
        context: &str,
    ) -> Result<Vec<String>, ShellError> {
        let value = self.eval_expr_value(expr, PipelineData::empty())?;
        match value {
            RuntimeValue::AttemptScope(scope) => Ok(scope
                .lock()?
                .children
                .iter()
                .filter(|child| !child.joined)
                .map(|child| child.attempt.clone())
                .collect()),
            RuntimeValue::AttemptHandle(handle) => Ok(vec![handle.attempt]),
            RuntimeValue::AttemptOutcome(outcome) => Ok(vec![outcome.attempt]),
            RuntimeValue::Nu(Value::List { vals, .. }) => vals
                .iter()
                .map(|value| attempt_id_from_value(value, context))
                .collect(),
            RuntimeValue::Nu(value) => attempt_id_from_value(&value, context).map(|id| vec![id]),
            other => Err(stone_error(
                context,
                format!(
                    "expected attempt_scope, attempt_handle, attempt id, or list; got {}",
                    runtime_type_name(&other)
                ),
            )),
        }
    }

    fn eval_attempt_terminate_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "attempt_terminate",
                "attempt_terminate() accepts exactly one attempt argument",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_terminate attempt"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_terminate attempt")?,
                _ => {
                    return Err(stone_error(
                        "attempt_terminate",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        if attempt.is_empty() {
            return Err(stone_error(
                "attempt_terminate",
                "attempt_terminate() requires a child attempt",
            ));
        }
        gateway_env::attempt_terminate(attempt).map(RuntimeValue::Nu)
    }

    fn eval_attempt_scope_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "attempt_scope",
                "attempt_scope() accepts at most one exit_policy argument",
            ));
        }
        let mut exit_policy = positional
            .first()
            .map(|value| value_to_string(value, "attempt_scope exit_policy"))
            .transpose()?
            .unwrap_or_else(|| "cancel_then_join".to_string());
        let mut join_timeout_ms = 5_000_u32;
        for (name, value) in named {
            match name.as_str() {
                "exit_policy" => {
                    exit_policy = value_to_string(&value, "attempt_scope exit_policy")?
                }
                "join_timeout_ms" => {
                    join_timeout_ms =
                        u32::try_from(value_to_u64(&value, "attempt_scope join_timeout_ms")?)
                            .map_err(|_| {
                                stone_error("attempt_scope", "join_timeout_ms is too large")
                            })?;
                }
                _ => {
                    return Err(stone_error(
                        "attempt_scope",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        if exit_policy != "cancel_then_join" {
            return Err(stone_error(
                "attempt_scope",
                "attempt_scope currently supports only exit_policy=\"cancel_then_join\"",
            ));
        }
        if join_timeout_ms == 0 {
            return Err(stone_error(
                "attempt_scope",
                "join_timeout_ms must be greater than zero",
            ));
        }
        let scope =
            AttemptScopeValue::new(self.state.next_callable_id(), exit_policy, join_timeout_ms);
        self.state.attempt_scopes.push(scope.clone());
        Ok(RuntimeValue::AttemptScope(scope))
    }

    fn eval_attempt_scope_add_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if call.positional.len() != 2 || !call.named.is_empty() {
            return Err(stone_error(
                "attempt_scope_add",
                "attempt_scope_add() requires scope and child arguments",
            ));
        }
        let scope = self.eval_attempt_scope_expr(&call.positional[0], "attempt_scope_add")?;
        let child = self
            .eval_expr_value(&call.positional[1], PipelineData::empty())?
            .into_nu_value("attempt_scope_add child")?;
        let attempt = attempt_id_from_value(&child, "attempt_scope_add child")?;
        scope.register(attempt)?;
        Ok(RuntimeValue::AttemptScope(scope))
    }

    fn eval_attempt_scope_close_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if call.positional.len() != 1 {
            return Err(stone_error(
                "attempt_scope_close",
                "attempt_scope_close() requires exactly one scope argument",
            ));
        }
        let scope = self.eval_attempt_scope_expr(&call.positional[0], "attempt_scope_close")?;
        let mut reason = "attempt scope closed".to_string();
        for (name, expression) in &call.named {
            let value = self
                .eval_expr_value(expression, PipelineData::empty())?
                .into_nu_value("attempt_scope_close reason")?;
            match name.as_str() {
                "reason" => reason = value_to_string(&value, "attempt_scope_close reason")?,
                _ => {
                    return Err(stone_error(
                        "attempt_scope_close",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        self.close_attempt_scope(&scope, &reason)
            .map(RuntimeValue::Nu)
    }

    fn eval_attempt_scope_expr(
        &mut self,
        expression: &Expr,
        context: &str,
    ) -> Result<AttemptScopeValue, ShellError> {
        match self.eval_expr_value(expression, PipelineData::empty())? {
            RuntimeValue::AttemptScope(scope) => Ok(scope),
            other => Err(stone_error(
                context,
                format!("expected attempt_scope, got {}", runtime_type_name(&other)),
            )),
        }
    }

    fn close_attempt_scope(
        &mut self,
        scope: &AttemptScopeValue,
        reason: &str,
    ) -> Result<Value, ShellError> {
        let (exit_policy, join_timeout_ms, children, already_closed) = {
            let state = scope.lock()?;
            let already_closed = state.closed;
            (
                state.exit_policy.clone(),
                state.join_timeout_ms,
                state.children.clone(),
                already_closed,
            )
        };
        if already_closed {
            return Ok(json_to_nu_value(
                json!({
                    "scope": scope.scope_id,
                    "exit_policy": exit_policy,
                    "closed": true,
                    "already_closed": true,
                    "clean": true,
                    "children": [],
                }),
                Span::unknown(),
            ));
        }

        let mut clean = true;
        let mut reports = Vec::with_capacity(children.len());
        for child in children {
            if child.resolved {
                reports.push(json!({
                    "attempt": child.attempt,
                    "joined": child.joined,
                    "resolved": true,
                    "action": "already_resolved",
                    "clean": true,
                }));
                continue;
            }

            let mut errors = Vec::new();
            let mut terminated = false;
            let mut joined = child.joined;
            let mut discarded = false;
            let mut state_name = "unknown".to_string();
            let mut controller_state = None;
            match gateway_env::attempt_state(child.attempt.clone(), 1) {
                Ok(state) => {
                    state_name = attempt_record_state(&state)
                        .unwrap_or("unknown")
                        .to_string();
                    controller_state = attempt_controller_state(&state).map(str::to_string);
                }
                Err(error) => errors.push(format!("state: {error}")),
            }

            if state_name == "active" {
                if controller_state
                    .as_deref()
                    .is_some_and(|state| matches!(state, "starting" | "running" | "terminating"))
                {
                    match gateway_env::attempt_terminate(child.attempt.clone()) {
                        Ok(_) => terminated = true,
                        Err(error) => errors.push(format!("terminate: {error}")),
                    }
                }
                if controller_state.is_some() && !joined {
                    match gateway_env::attempt_wait(child.attempt.clone(), Some(join_timeout_ms)) {
                        Ok(waited) => {
                            joined = attempt_controller_state(&waited).is_some_and(|state| {
                                matches!(state, "exited" | "failed" | "terminated")
                            });
                            if joined {
                                scope.mark_joined(&child.attempt)?;
                            }
                        }
                        Err(error) => errors.push(format!("join: {error}")),
                    }
                }
            }

            match gateway_env::attempt_state(child.attempt.clone(), 1) {
                Ok(state) => {
                    state_name = attempt_record_state(&state)
                        .unwrap_or("unknown")
                        .to_string();
                }
                Err(error) => errors.push(format!("final state: {error}")),
            }
            if state_name == "active" {
                match gateway_env::attempt_discard(child.attempt.clone(), reason.to_string()) {
                    Ok(_) => {
                        discarded = true;
                        state_name = "rolled_back".to_string();
                        scope.mark_resolved(&child.attempt)?;
                    }
                    Err(error) => errors.push(format!("discard: {error}")),
                }
            } else if state_name != "unknown" {
                scope.mark_resolved(&child.attempt)?;
            }
            let child_clean =
                errors.is_empty() && state_name != "active" && state_name != "unknown";
            clean &= child_clean;
            reports.push(json!({
                "attempt": child.attempt,
                "joined": joined,
                "terminated": terminated,
                "discarded": discarded,
                "state": state_name,
                "clean": child_clean,
                "errors": errors,
            }));
        }

        if clean {
            scope.lock()?.closed = true;
        }

        Ok(json_to_nu_value(
            json!({
                "scope": scope.scope_id,
                "exit_policy": exit_policy,
                "closed": clean,
                "already_closed": false,
                "clean": clean,
                "children": reports,
            }),
            Span::unknown(),
        ))
    }

    fn close_open_attempt_scopes(&mut self, reason: &str) -> Result<(), ShellError> {
        let scopes = self.state.attempt_scopes.clone();
        let mut failed = Vec::new();
        for scope in scopes {
            let is_closed = scope.lock()?.closed;
            if is_closed {
                continue;
            }
            let report = self.close_attempt_scope(&scope, reason)?;
            let report_json = nu_to_json_value(&report);
            if report_json.get("clean").and_then(JsonValue::as_bool) != Some(true) {
                failed.push(report_json);
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(stone_error(
                "attempt scope cleanup",
                format!(
                    "automatic cancel-then-join cleanup was incomplete: {}",
                    JsonValue::Array(failed)
                ),
            ))
        }
    }

    fn eval_attempt_report_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_report",
                "attempt_report() accepts at most status and result positional arguments",
            ));
        }
        let mut status = positional
            .first()
            .map(|value| value_to_string(value, "attempt_report status"))
            .transpose()?;
        let mut attempt = String::new();
        let mut result_json = positional
            .get(1)
            .map(|value| nu_to_json_value(value).to_string())
            .unwrap_or_default();
        let mut error_json = String::new();
        let mut reason = String::new();
        let mut metadata = Vec::new();
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_report attempt")?,
                "status" => status = Some(value_to_string(&value, "attempt_report status")?),
                "result" => result_json = nu_to_json_value(&value).to_string(),
                "error" => error_json = nu_to_json_value(&value).to_string(),
                "reason" => reason = value_to_string(&value, "attempt_report reason")?,
                "metadata" => metadata = value_to_string_pairs(&value, "attempt_report metadata")?,
                _ => {
                    return Err(stone_error(
                        "attempt_report",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let status =
            status.ok_or_else(|| stone_error("attempt_report", "missing status argument"))?;
        gateway_env::attempt_report(attempt, status, result_json, error_json, reason, metadata)
            .map(RuntimeValue::Nu)
    }

    fn eval_attempt_accept_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_accept",
                "attempt_accept() accepts parent and child arguments",
            ));
        }
        let mut parent = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_accept parent"))
            .transpose()?
            .unwrap_or_default();
        let mut child = positional
            .get(1)
            .map(|value| attempt_id_from_value(value, "attempt_accept child"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "parent" => parent = attempt_id_from_value(&value, "attempt_accept parent")?,
                "child" => child = attempt_id_from_value(&value, "attempt_accept child")?,
                _ => {
                    return Err(stone_error(
                        "attempt_accept",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let accepted = gateway_env::attempt_accept(parent, child.clone())?;
        for scope in &self.state.attempt_scopes {
            scope.mark_resolved(&child)?;
        }
        Ok(RuntimeValue::Nu(accepted))
    }

    fn eval_attempt_discard_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_discard",
                "attempt_discard() accepts at most attempt and reason arguments",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_discard attempt"))
            .transpose()?
            .unwrap_or_default();
        let mut reason = positional
            .get(1)
            .map(|value| value_to_string(value, "attempt_discard reason"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_discard attempt")?,
                "reason" => reason = value_to_string(&value, "attempt_discard reason")?,
                _ => {
                    return Err(stone_error(
                        "attempt_discard",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let discarded = gateway_env::attempt_discard(attempt.clone(), reason)?;
        for scope in &self.state.attempt_scopes {
            scope.mark_resolved(&attempt)?;
        }
        Ok(RuntimeValue::Nu(discarded))
    }

    fn eval_attempt_publish_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "attempt_publish",
                "attempt_publish() accepts at most attempt and expected_generation arguments",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_publish attempt"))
            .transpose()?
            .unwrap_or_default();
        let mut expected_generation = positional
            .get(1)
            .map(|value| value_to_string(value, "attempt_publish expected_generation"))
            .transpose()?
            .unwrap_or_default();
        let mut message = String::new();
        let mut allow_risky = false;
        for (name, value) in named {
            match name.as_str() {
                "attempt" => attempt = attempt_id_from_value(&value, "attempt_publish attempt")?,
                "expected_generation" => {
                    expected_generation =
                        value_to_string(&value, "attempt_publish expected_generation")?
                }
                "message" => message = value_to_string(&value, "attempt_publish message")?,
                "allow_risky" => {
                    allow_risky = value_to_bool(&value, "attempt_publish allow_risky")?
                }
                _ => {
                    return Err(stone_error(
                        "attempt_publish",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::attempt_publish(attempt, expected_generation, message, allow_risky)
            .map(RuntimeValue::Nu)
    }

    fn eval_attempt_run_process_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 3 {
            return Err(stone_error(
                "attempt_run_process",
                "attempt_run_process() accepts at most attempt, argv, and env positional arguments",
            ));
        }
        let mut attempt = positional
            .first()
            .map(|value| attempt_id_from_value(value, "attempt_run_process attempt"))
            .transpose()?
            .unwrap_or_default();
        let mut argv = positional
            .get(1)
            .map(|value| value_to_string_list(value, "attempt_run_process argv"))
            .transpose()?;
        let mut env = positional
            .get(2)
            .map(|value| value_to_string_pairs(value, "attempt_run_process env"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "attempt" => {
                    attempt = attempt_id_from_value(&value, "attempt_run_process attempt")?
                }
                "argv" => argv = Some(value_to_string_list(&value, "attempt_run_process argv")?),
                "env" => env = value_to_string_pairs(&value, "attempt_run_process env")?,
                _ => {
                    return Err(stone_error(
                        "attempt_run_process",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let argv =
            argv.ok_or_else(|| stone_error("attempt_run_process", "missing argv argument"))?;
        if argv.is_empty() {
            return Err(stone_error(
                "attempt_run_process",
                "argv list cannot be empty",
            ));
        }
        gateway_env::attempt_run_process(attempt, argv, env).map(RuntimeValue::Nu)
    }

    fn eval_env_state_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                &call.name,
                format!("{}() accepts at most one sample_limit argument", call.name),
            ));
        }
        let mut sample_limit = 100_u32;
        if let Some(value) = positional.first() {
            sample_limit = u32::try_from(value_to_u64(value, "env_state sample_limit")?)
                .map_err(|_| stone_error("env_state", "sample_limit is too large"))?;
        }
        for (name, value) in named {
            match name.as_str() {
                "sample_limit" => {
                    sample_limit =
                        u32::try_from(value_to_u64(&value, "env_state sample_limit")?)
                            .map_err(|_| stone_error("env_state", "sample_limit is too large"))?;
                }
                _ => {
                    return Err(stone_error(
                        &call.name,
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::env_state(sample_limit).map(RuntimeValue::Nu)
    }

    fn eval_env_tx_info_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "env_tx_info",
                "env_tx_info() accepts at most one tx argument",
            ));
        }
        let mut tx = positional
            .first()
            .map(|value| value_to_string(value, "env_tx_info tx"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "tx" => tx = value_to_string(&value, "env_tx_info tx")?,
                _ => {
                    return Err(stone_error(
                        "env_tx_info",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::env_tx_info(tx).map(RuntimeValue::Nu)
    }

    fn eval_env_txs_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 2 {
            return Err(stone_error(
                "env_txs",
                "env_txs() accepts at most workspace and purpose arguments",
            ));
        }
        let mut workspace = positional
            .first()
            .map(|value| value_to_string(value, "env_txs workspace"))
            .transpose()?
            .unwrap_or_default();
        let mut purpose = positional
            .get(1)
            .map(|value| value_to_string(value, "env_txs purpose"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "workspace" => workspace = value_to_string(&value, "env_txs workspace")?,
                "purpose" => purpose = value_to_string(&value, "env_txs purpose")?,
                _ => {
                    return Err(stone_error(
                        "env_txs",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::env_txs(workspace, purpose).map(RuntimeValue::Nu)
    }

    fn eval_env_finish_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.positional.is_empty() || !call.named.is_empty() {
            return Err(stone_error("env_finish", "env_finish() takes no arguments"));
        }
        gateway_env::env_finish().map(RuntimeValue::Nu)
    }

    fn eval_env_restore_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        let mut paths = Vec::new();
        for value in positional {
            collect_env_restore_paths(&value, &mut paths)?;
        }
        for (name, value) in named {
            match name.as_str() {
                "paths" => collect_env_restore_paths(&value, &mut paths)?,
                _ => {
                    return Err(stone_error(
                        "env_restore",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::env_restore(paths).map(RuntimeValue::Nu)
    }

    fn eval_env_checkpoint_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "env_checkpoint",
                "env_checkpoint() accepts at most one reason argument",
            ));
        }
        let mut reason = positional
            .first()
            .map(|value| value_to_string(value, "env_checkpoint reason"))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in named {
            match name.as_str() {
                "reason" => reason = value_to_string(&value, "env_checkpoint reason")?,
                _ => {
                    return Err(stone_error(
                        "env_checkpoint",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::env_checkpoint(reason).map(RuntimeValue::Nu)
    }

    fn eval_env_fork_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let checkpoint = self.required_checkpoint_arg(call, "env_fork")?;
        gateway_env::env_fork(checkpoint).map(RuntimeValue::Nu)
    }

    fn eval_env_restore_checkpoint_call(
        &mut self,
        call: &Call,
    ) -> Result<RuntimeValue, ShellError> {
        let checkpoint = self.required_checkpoint_arg(call, "env_restore_checkpoint")?;
        gateway_env::env_restore_checkpoint(checkpoint).map(RuntimeValue::Nu)
    }

    fn eval_env_checkpoints_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "env_checkpoints",
                "env_checkpoints() accepts at most one workspace argument",
            ));
        }
        let mut workspace = positional
            .first()
            .map(|value| value_to_string(value, "env_checkpoints workspace"))
            .transpose()?
            .unwrap_or_default();
        let mut include_discarded = false;
        for (name, value) in named {
            match name.as_str() {
                "workspace" => workspace = value_to_string(&value, "env_checkpoints workspace")?,
                "include_discarded" => {
                    include_discarded = value_to_bool(&value, "env_checkpoints include_discarded")?
                }
                _ => {
                    return Err(stone_error(
                        "env_checkpoints",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::env_checkpoints(workspace, include_discarded).map(RuntimeValue::Nu)
    }

    fn eval_env_checkpoint_gc_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "env_checkpoint_gc",
                "env_checkpoint_gc() accepts at most one apply argument",
            ));
        }
        let mut apply = positional
            .first()
            .map(|value| value_to_bool(value, "env_checkpoint_gc apply"))
            .transpose()?
            .unwrap_or(false);
        for (name, value) in named {
            match name.as_str() {
                "apply" => apply = value_to_bool(&value, "env_checkpoint_gc apply")?,
                _ => {
                    return Err(stone_error(
                        "env_checkpoint_gc",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::env_checkpoint_gc(apply).map(RuntimeValue::Nu)
    }

    fn eval_env_discard_checkpoint_call(
        &mut self,
        call: &Call,
    ) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "env_discard_checkpoint",
                "env_discard_checkpoint() accepts exactly one checkpoint argument",
            ));
        }
        let mut checkpoint = positional
            .first()
            .map(|value| value_to_string(value, "env_discard_checkpoint checkpoint"))
            .transpose()?;
        let mut force = false;
        for (name, value) in named {
            match name.as_str() {
                "checkpoint" => {
                    checkpoint = Some(value_to_string(
                        &value,
                        "env_discard_checkpoint checkpoint",
                    )?)
                }
                "force" => force = value_to_bool(&value, "env_discard_checkpoint force")?,
                _ => {
                    return Err(stone_error(
                        "env_discard_checkpoint",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        let checkpoint = checkpoint
            .ok_or_else(|| stone_error("env_discard_checkpoint", "missing checkpoint argument"))?;
        gateway_env::env_discard_checkpoint(checkpoint, force).map(RuntimeValue::Nu)
    }

    fn eval_env_run_checkpoint_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 3 {
            return Err(stone_error(
                "env_run_checkpoint",
                "env_run_checkpoint() expects checkpoint, image, and argv",
            ));
        }
        let first_positional_is_argv = matches!(positional.first(), Some(Value::List { .. }));
        let mut checkpoint = if first_positional_is_argv {
            None
        } else {
            positional
                .first()
                .map(|value| value_to_string(value, "env_run_checkpoint checkpoint"))
                .transpose()?
        };
        let mut image = if first_positional_is_argv {
            None
        } else {
            positional
                .get(1)
                .map(|value| value_to_string(value, "env_run_checkpoint image"))
                .transpose()?
        };
        let mut argv = if first_positional_is_argv {
            positional
                .first()
                .map(|value| value_to_string_list(value, "env_run_checkpoint argv"))
                .transpose()?
        } else {
            positional
                .get(2)
                .map(|value| value_to_string_list(value, "env_run_checkpoint argv"))
                .transpose()?
        };
        let mut workspace_mount = "/app".to_string();
        let mut workdir = "/app".to_string();
        let mut env = Vec::new();
        let mut user = String::new();
        let mut stdin = String::new();
        let mut timeout_ms = 300_000_u64;
        let mut keep_tx = false;
        for (name, value) in named {
            match name.as_str() {
                "checkpoint" => {
                    checkpoint = Some(value_to_string(&value, "env_run_checkpoint checkpoint")?)
                }
                "image" => image = Some(value_to_string(&value, "env_run_checkpoint image")?),
                "argv" => argv = Some(value_to_string_list(&value, "env_run_checkpoint argv")?),
                "workspace_mount" => {
                    workspace_mount = value_to_string(&value, "env_run_checkpoint workspace_mount")?
                }
                "workdir" | "cwd" => {
                    workdir = value_to_string(&value, "env_run_checkpoint workdir")?
                }
                "env" => env = value_to_string_pairs(&value, "env_run_checkpoint env")?,
                "user" => user = value_to_string(&value, "env_run_checkpoint user")?,
                "stdin" => stdin = value_to_string(&value, "env_run_checkpoint stdin")?,
                "timeout_ms" => {
                    timeout_ms = value_to_u64(&value, "env_run_checkpoint timeout_ms")?;
                    if timeout_ms == 0 {
                        return Err(stone_error(
                            "env_run_checkpoint",
                            "timeout_ms must be positive",
                        ));
                    }
                }
                "keep_tx" => keep_tx = value_to_bool(&value, "env_run_checkpoint keep_tx")?,
                other => {
                    return Err(stone_error(
                        "env_run_checkpoint",
                        format!("unexpected keyword argument `{other}`"),
                    ));
                }
            }
        }
        let checkpoint = checkpoint
            .ok_or_else(|| stone_error("env_run_checkpoint", "missing checkpoint argument"))?;
        let image =
            image.ok_or_else(|| stone_error("env_run_checkpoint", "missing image argument"))?;
        let argv =
            argv.ok_or_else(|| stone_error("env_run_checkpoint", "missing argv argument"))?;
        if argv.is_empty() {
            return Err(stone_error(
                "env_run_checkpoint",
                "argv list cannot be empty",
            ));
        }
        gateway_env::env_run_checkpoint(
            checkpoint,
            image,
            argv,
            workspace_mount,
            workdir,
            env,
            user,
            stdin,
            timeout_ms,
            keep_tx,
        )
        .map(RuntimeValue::Nu)
    }

    fn required_checkpoint_arg(
        &mut self,
        call: &Call,
        context: &str,
    ) -> Result<String, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                context,
                format!("{context}() accepts exactly one checkpoint argument"),
            ));
        }
        let mut checkpoint = positional
            .first()
            .map(|value| value_to_string(value, context))
            .transpose()?;
        for (name, value) in named {
            match name.as_str() {
                "checkpoint" => checkpoint = Some(value_to_string(&value, context)?),
                _ => {
                    return Err(stone_error(
                        context,
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        checkpoint.ok_or_else(|| stone_error(context, "missing checkpoint argument"))
    }

    fn eval_env_commit_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let (positional, named) = self.eval_call_values(call)?;
        if positional.len() > 1 {
            return Err(stone_error(
                "env_commit",
                "env_commit() accepts at most one message argument",
            ));
        }
        let mut message = positional
            .first()
            .map(|value| value_to_string(value, "env_commit message"))
            .transpose()?
            .unwrap_or_else(|| "agent commit".to_string());
        let mut allow_risky = false;
        for (name, value) in named {
            match name.as_str() {
                "message" => message = value_to_string(&value, "env_commit message")?,
                "allow_risky" => allow_risky = value_to_bool(&value, "env_commit allow_risky")?,
                _ => {
                    return Err(stone_error(
                        "env_commit",
                        format!("unexpected keyword argument `{name}`"),
                    ));
                }
            }
        }
        gateway_env::env_commit(message, allow_risky).map(RuntimeValue::Nu)
    }

    fn eval_env_rollback_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if !call.positional.is_empty() || !call.named.is_empty() {
            return Err(stone_error(
                "env_rollback",
                "env_rollback() takes no arguments",
            ));
        }
        gateway_env::env_rollback().map(RuntimeValue::Nu)
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
            let cwd = self.current_cwd_path("wait_port")?;
            wait_port_call_values(&positional, &named, Some(&cwd)).map(RuntimeValue::Nu)
        }
    }

    fn eval_wait_for_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        if call.positional.is_empty() || call.positional.len() > 3 {
            return Err(stone_error(
                "wait_for",
                "wait_for() requires predicate and optional timeout_ms, interval_ms",
            ));
        }
        let predicate = self.eval_callable_expr(&call.positional[0])?;
        if !predicate.params.is_empty() {
            return Err(stone_error(
                "wait_for",
                "wait_for() predicate must be a zero-argument lambda/callable",
            ));
        }

        let mut timeout_ms: i64 = match call.positional.get(1) {
            Some(value) => self
                .eval_expr_value(value, PipelineData::empty())?
                .into_nu_value("wait_for timeout_ms")
                .and_then(|value| value_to_i64(&value, "wait_for timeout_ms"))?,
            None => 30_000,
        };
        let mut interval_ms: i64 = match call.positional.get(2) {
            Some(value) => self
                .eval_expr_value(value, PipelineData::empty())?
                .into_nu_value("wait_for interval_ms")
                .and_then(|value| value_to_i64(&value, "wait_for interval_ms"))?,
            None => 100,
        };
        let mut ignore_errors = false;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("wait_for")?;
            match name.as_str() {
                "timeout_ms" => timeout_ms = value_to_i64(&value, "wait_for timeout_ms")?,
                "interval_ms" => interval_ms = value_to_i64(&value, "wait_for interval_ms")?,
                "ignore_errors" => ignore_errors = value_to_bool(&value, "wait_for ignore_errors")?,
                other => {
                    return Err(stone_error(
                        "wait_for",
                        format!(
                            "unsupported keyword `{other}`; expected timeout_ms, interval_ms, or ignore_errors"
                        ),
                    ));
                }
            }
        }
        if timeout_ms <= 0 {
            return Err(stone_error("wait_for", "timeout_ms must be positive"));
        }
        if interval_ms <= 0 {
            return Err(stone_error("wait_for", "interval_ms must be positive"));
        }

        let started = Instant::now();
        let timeout = Duration::from_millis(timeout_ms as u64);
        let interval = Duration::from_millis(interval_ms as u64);
        let mut attempts: i64 = 0;
        loop {
            attempts += 1;
            let last_value;
            let last_error;
            match self.invoke_callable(&predicate, Vec::new()) {
                Ok(value) => {
                    let value = value.into_nu_value("wait_for predicate")?;
                    if value_truthy(&value) {
                        return Ok(RuntimeValue::Nu(wait_for_record(
                            true,
                            attempts,
                            started.elapsed(),
                            Some(value),
                            None,
                        )));
                    }
                    last_value = Some(value);
                    last_error = None;
                }
                Err(err) if ignore_errors => {
                    last_error = Some(format!("{err:?}"));
                    last_value = None;
                }
                Err(err) => return Err(err),
            }
            if started.elapsed() >= timeout {
                return Ok(RuntimeValue::Nu(wait_for_record(
                    false,
                    attempts,
                    started.elapsed(),
                    last_value,
                    last_error,
                )));
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            thread::sleep(interval.min(remaining));
        }
    }

    fn eval_list_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let name = call.name.as_str();
        if !call.named.is_empty() {
            return Err(stone_error(
                name,
                format!("{name}() keyword arguments are not supported"),
            ));
        }
        let [value] = call.positional.as_slice() else {
            return Err(stone_error(
                name,
                format!("{name}() requires exactly one argument"),
            ));
        };
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value(name)?;
        list_builtin(&value).map(RuntimeValue::Nu)
    }

    fn eval_hash_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let name = call.name.as_str();
        if !call.named.is_empty() {
            return Err(stone_error(
                name,
                format!("{name}() keyword arguments are not supported"),
            ));
        }
        let [value] = call.positional.as_slice() else {
            return Err(stone_error(
                name,
                format!("{name}() requires exactly one argument"),
            ));
        };
        let value = self
            .eval_expr_value(value, PipelineData::empty())?
            .into_nu_value(name)?;
        let text = value_to_string(&value, name)?;
        hash_builtin(name, &text).map(RuntimeValue::Nu)
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
        let mut named_maxsplit = None;
        for (name, value) in &call.named {
            if name == "maxsplit" && named_maxsplit.is_none() {
                named_maxsplit = Some(value);
            } else {
                return Err(stone_error(
                    "split",
                    format!("split() unsupported keyword argument `{name}`"),
                ));
            }
        }
        if named_maxsplit.is_some() && call.positional.len() > 2 {
            return Err(stone_error("split", "split() got multiple maxsplit values"));
        }
        let ([text] | [text, _] | [text, _, _]) = call.positional.as_slice() else {
            return Err(stone_error(
                "split",
                "split() requires text, optional separator, and optional maxsplit",
            ));
        };
        let text = self
            .eval_expr_value(text, PipelineData::empty())?
            .into_nu_value("split")?;
        let separator = match call.positional.as_slice() {
            [_] => None,
            [_, separator] | [_, separator, _] => {
                let separator = self
                    .eval_expr_value(separator, PipelineData::empty())?
                    .into_nu_value("split")?;
                Some(separator)
            }
            _ => unreachable!(),
        };
        let maxsplit = match call.positional.as_slice() {
            [_, _, maxsplit] => Some(
                self.eval_expr_value(maxsplit, PipelineData::empty())?
                    .into_nu_value("split")?,
            ),
            _ => named_maxsplit
                .map(|expr| self.eval_expr_value(expr, PipelineData::empty()))
                .transpose()?
                .map(|value| value.into_nu_value("split"))
                .transpose()?,
        };
        split_builtin(&text, separator.as_ref(), maxsplit.as_ref()).map(RuntimeValue::Nu)
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
        let first = match items {
            Expr::Generator { .. } => {
                Value::list(self.eval_generator_values(items)?, Span::unknown())
            }
            _ => self
                .eval_expr_value(items, PipelineData::empty())?
                .into_nu_value("join")?,
        };
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
        if !call.named.is_empty() {
            return Err(stone_error(
                "print",
                "print() keyword arguments are not supported",
            ));
        }
        if call.positional.is_empty() {
            self.state.stdout.push('\n');
            return Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())));
        }
        let mut values = Vec::with_capacity(call.positional.len());
        let mut parts = Vec::with_capacity(call.positional.len());
        for arg in &call.positional {
            let value = self
                .eval_expr_value(arg, PipelineData::empty())?
                .into_nu_value("print")?;
            parts.push(value_to_display_string(&value)?);
            values.push(value);
        }
        self.state.stdout.push_str(&parts.join(" "));
        self.state.stdout.push('\n');
        let result = if values.len() == 1 {
            values.remove(0)
        } else {
            Value::list(values, Span::unknown())
        };
        Ok(RuntimeValue::Nu(result))
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
            "splitlines" | "readlines" => {
                let [] = args else {
                    return Err(stone_error(
                        method,
                        format!("{method}() takes no arguments for now"),
                    ));
                };
                match self.state.file_mut(handle)? {
                    RuntimeFile::Read { text, closed, .. } => {
                        if *closed {
                            return Err(stone_error(method, "I/O operation on closed file"));
                        }
                        Ok(RuntimeValue::TextLines(TextLines {
                            lines: text.lines().map(str::to_owned).collect(),
                            source: format!("open(...).{method}()"),
                        }))
                    }
                    RuntimeFile::Write { .. } => {
                        Err(stone_error(method, "file is not open for reading"))
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
        named: &[(String, Expr)],
    ) -> Result<RuntimeValue, ShellError> {
        let started = self.state.profiler.start();
        let result = (|| {
            if !named.is_empty() && method != "sort" {
                return Err(stone_error(
                    "method call",
                    format!(
                        "{method}() keyword arguments are not supported; method keyword arguments are supported only for split(maxsplit=...) and sort(key=..., reverse=...)"
                    ),
                ));
            }
            if method == "append" {
                return self.eval_append_call(receiver, positional);
            }
            if method == "extend" {
                return self.eval_extend_call(receiver, positional);
            }
            if method == "sort" {
                return self.eval_sort_method_call(receiver, positional, named);
            }
            if method == "add" {
                return self.eval_add_method_call(receiver, positional);
            }

            let receiver = self.eval_expr_value(receiver, PipelineData::empty())?;
            let args = positional
                .iter()
                .map(|arg| {
                    if method == "join" && matches!(arg, Expr::Generator { .. }) {
                        let values = self.eval_generator_values(arg)?;
                        Ok(Value::list(values, Span::unknown()))
                    } else {
                        self.eval_expr_value(arg, PipelineData::empty())?
                            .into_nu_value("method call")
                    }
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
                "strip" | "lstrip" | "rstrip" | "isdigit" | "isalpha" | "isalnum" | "split"
                | "rsplit" | "splitlines" | "replace" | "join" | "lower" | "upper" | "zfill"
                | "startswith" | "endswith" => {
                    string_method_builtin(&receiver, method, &args).map(RuntimeValue::Nu)
                }
                "count" => self.eval_count_method(receiver, &args),
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

    fn eval_extend_call(
        &mut self,
        receiver: &Expr,
        positional: &[Expr],
    ) -> Result<RuntimeValue, ShellError> {
        let [arg] = positional else {
            return Err(stone_error(
                "extend",
                "extend() requires exactly one iterable argument",
            ));
        };
        let values = self
            .eval_iterable_expr(arg)?
            .into_iter()
            .map(|value| value.into_nu_value("extend"))
            .collect::<Result<Vec<_>, _>>()?;
        let target = self.mutable_list_method_target(receiver, "extend")?;
        target.extend(values);
        Ok(RuntimeValue::Nu(Value::nothing(Span::unknown())))
    }

    fn eval_sort_method_call(
        &mut self,
        receiver: &Expr,
        positional: &[Expr],
        named: &[(String, Expr)],
    ) -> Result<RuntimeValue, ShellError> {
        if !positional.is_empty() {
            return Err(stone_error(
                "sort",
                "sort() method takes only keyword arguments key=... and reverse=...",
            ));
        }
        let mut key = SortKey::Identity;
        let mut reverse = false;
        for (name, argument) in named {
            match name.as_str() {
                "key" => key = self.eval_sort_key(argument)?,
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
        let values = self.mutable_list_method_target(receiver, "sort")?.clone();
        let sorted = sort_builtin_values(values, reverse, |value| match &key {
            SortKey::Callable(callable) => self
                .invoke_callable(callable, vec![RuntimeValue::Nu(value.clone())])?
                .into_nu_value("sort key"),
            SortKey::Identity => sort_key_for_value(value, None),
            SortKey::Field(field) => sort_key_for_value(value, Some(field)),
        })?;
        let Value::List { vals, .. } = sorted else {
            unreachable!("sort_builtin_values returns a list");
        };
        let target = self.mutable_list_method_target(receiver, "sort")?;
        *target = vals;
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

    fn eval_count_method(
        &mut self,
        receiver: Value,
        args: &[Value],
    ) -> Result<RuntimeValue, ShellError> {
        let [needle] = args else {
            return Err(stone_error(
                "count",
                "count() requires exactly one argument",
            ));
        };
        match receiver {
            Value::String { .. } | Value::Glob { .. } => {
                string_method_builtin(&receiver, "count", args).map(RuntimeValue::Nu)
            }
            Value::List { vals, .. } => {
                let needle_key = value_identity_key(needle, "count")?;
                let count = vals
                    .iter()
                    .map(|value| value_identity_key(value, "count"))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter(|key| key == &needle_key)
                    .count();
                let count =
                    i64::try_from(count).map_err(|_| stone_error("count", "count is too large"))?;
                Ok(RuntimeValue::Nu(Value::int(count, Span::unknown())))
            }
            other => Err(stone_error(
                "count",
                format!("{} has no count()", other.get_type()),
            )),
        }
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

    fn eval_any_all_call(&mut self, call: &Call, is_any: bool) -> Result<RuntimeValue, ShellError> {
        let context = if is_any { "any" } else { "all" };
        let [arg] = call.positional.as_slice() else {
            return Err(stone_error(
                context,
                format!("{context}() requires exactly one argument"),
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                context,
                format!("{context}() keyword arguments are not supported"),
            ));
        }

        let result = match arg {
            Expr::Generator { .. } => self.eval_generator_truthy(arg, is_any)?,
            _ => {
                let values = self.eval_iterable_expr(arg)?;
                if is_any {
                    values
                        .into_iter()
                        .map(|value| value.into_nu_value(context))
                        .try_fold(false, |found, value| {
                            if found {
                                Ok(true)
                            } else {
                                value.map(|value| value_truthy(&value))
                            }
                        })?
                } else {
                    values
                        .into_iter()
                        .map(|value| value.into_nu_value(context))
                        .try_fold(true, |all, value| {
                            if !all {
                                Ok(false)
                            } else {
                                value.map(|value| value_truthy(&value))
                            }
                        })?
                }
            }
        };
        Ok(RuntimeValue::Nu(Value::bool(result, Span::unknown())))
    }

    fn eval_generator_values(&mut self, expression: &Expr) -> Result<Vec<Value>, ShellError> {
        let Expr::Generator { elt, clauses } = expression else {
            return Err(stone_error("generator expression", "expected generator"));
        };
        let mut output = Vec::new();
        self.eval_comprehension_clauses("generator expression", clauses, 0, &mut |evaluator| {
            output.push(
                evaluator
                    .eval_expr_value(elt, PipelineData::empty())?
                    .into_nu_value("generator expression")?,
            );
            Ok(false)
        })?;
        Ok(output)
    }

    fn eval_generator_truthy(
        &mut self,
        expression: &Expr,
        is_any: bool,
    ) -> Result<bool, ShellError> {
        let Expr::Generator { elt, clauses } = expression else {
            return Err(stone_error("generator expression", "expected generator"));
        };
        let mut result = !is_any;
        self.eval_comprehension_clauses("generator expression", clauses, 0, &mut |evaluator| {
            let value = evaluator
                .eval_expr_value(elt, PipelineData::empty())?
                .into_nu_value("generator expression")?;
            let truthy = value_truthy(&value);
            if is_any && truthy {
                result = true;
                return Ok(true);
            }
            if !is_any && !truthy {
                result = false;
                return Ok(true);
            }
            Ok(false)
        })?;
        Ok(result)
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

const STONE_BUILTIN_NAMES: &[&str] = &[
    "cat",
    "diff",
    "edit",
    "edit_file",
    "echo",
    "emit",
    "attempt_finish",
    "attempt_fork",
    "attempt_accept",
    "attempt_discard",
    "attempt_info",
    "attempt_list",
    "attempt_publish",
    "attempt_report",
    "attempt_run_process",
    "attempt_spawn",
    "attempt_start",
    "attempt_state",
    "attempt_scope",
    "attempt_scope_add",
    "attempt_scope_close",
    "attempt_join",
    "attempt_terminate",
    "attempt_wait",
    "attempt_wait_all",
    "attempt_wait_any",
    "attempts",
    "env_checkpoint",
    "env_checkpoint_gc",
    "env_checkpoints",
    "env_commit",
    "env_discard_checkpoint",
    "env_diff",
    "env_finish",
    "env_fork",
    "env_restore",
    "env_restore_checkpoint",
    "env_run_checkpoint",
    "env_rollback",
    "env_state",
    "env_tx_info",
    "env_txs",
    "fail",
    "filter",
    "find",
    "float",
    "format",
    "first",
    "head",
    "from_json",
    "help",
    "int",
    "last",
    "len",
    "ls",
    "list",
    "list_dir",
    "join",
    "map",
    "max",
    "md5",
    "min",
    "model_call",
    "agent_session",
    "current_program",
    "react_control",
    "scripted_control",
    "task_spec",
    "task_input",
    "mkdir",
    "must_run",
    "open",
    "parse_float",
    "parse_int",
    "print",
    "process_list",
    "ps",
    "pwd",
    "cd",
    "range",
    "repr",
    "read_file",
    "read_text",
    "read_csv",
    "read_jsonl",
    "round",
    "rm",
    "run",
    "run_status",
    "run_terminate",
    "run_wait",
    "slice",
    "split",
    "resolve_command",
    "state",
    "sys",
    "sys_info",
    "sysinfo",
    "starts_with",
    "startswith",
    "tail",
    "tuple",
    "last_result",
    "start_daemon",
    "daemon_status",
    "stop_daemon",
    "wait_port",
    "wait_for",
    "save",
    "search",
    "sha1",
    "sha256",
    "sort",
    "sorted",
    "stat",
    "str",
    "sum",
    "to_json",
    "to_jsonl",
    "set",
    "unique",
    "type",
    "where",
    "json_dumps",
    "json_loads",
    "read_json",
    "write_csv",
    "write_file",
    "write_text",
    "write_json",
    "write_jsonl",
    "all",
    "any",
    "enumerate",
];

fn is_builtin_call(call: &Call) -> bool {
    if matches!(call.name.as_str(), "keys" | "values" | "items") {
        return call.positional.len() == 1;
    }
    if call.name == "get" {
        return !call.positional.is_empty();
    }
    STONE_BUILTIN_NAMES.contains(&call.name.as_str())
}

fn slice_text_lines(
    text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, ShellError> {
    let start = start_line.unwrap_or(1);
    if start == 0 {
        return Err(stone_error(
            "read_text",
            "start_line is 1-based and must be positive",
        ));
    }
    if let Some(end) = end_line {
        if end == 0 {
            return Err(stone_error(
                "read_text",
                "end_line is 1-based and must be positive",
            ));
        }
        if end < start {
            return Ok(String::new());
        }
    }

    let mut output = String::new();
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let line_number = index + 1;
        if line_number < start {
            continue;
        }
        if end_line.is_some_and(|end| line_number > end) {
            break;
        }
        output.push_str(line);
    }
    Ok(output)
}

fn validate_json_dumps_separators(value: &Value) -> Result<(), ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            "json_dumps",
            "json_dumps() separators must be a two-item list or tuple like [\",\", \":\"]",
        ));
    };
    let [item_separator, key_separator] = vals.as_slice() else {
        return Err(stone_error(
            "json_dumps",
            "json_dumps() separators must contain exactly two strings",
        ));
    };
    let item_separator = value_to_string(item_separator, "json_dumps separators")?;
    let key_separator = value_to_string(key_separator, "json_dumps separators")?;
    if item_separator == "," && key_separator == ":" {
        return Ok(());
    }
    Err(stone_error(
        "json_dumps",
        "json_dumps() currently supports only separators=(\",\", \":\")",
    ))
}

fn csv_text_from_rows(rows: &Value, columns: Option<&Value>) -> Result<String, ShellError> {
    let Value::List { vals, .. } = rows else {
        return Err(stone_error(
            "write_csv",
            format!("expected list of records, got {}", rows.get_type()),
        ));
    };
    let columns = match columns {
        Some(columns) => csv_columns_from_value(columns)?,
        None => infer_csv_columns(vals)?,
    };

    let mut text = String::new();
    write_csv_line(&mut text, &columns);
    for row in vals {
        let Value::Record { val, .. } = row else {
            return Err(stone_error(
                "write_csv",
                format!("expected record row, got {}", row.get_type()),
            ));
        };
        let mut fields = Vec::with_capacity(columns.len());
        for column in &columns {
            fields.push(
                val.get(column)
                    .map(csv_field_value)
                    .transpose()?
                    .unwrap_or_default(),
            );
        }
        write_csv_line(&mut text, &fields);
    }
    Ok(text)
}

fn csv_columns_from_value(value: &Value) -> Result<Vec<String>, ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            "write_csv",
            format!(
                "columns must be a list of strings, got {}",
                value.get_type()
            ),
        ));
    };
    let mut columns = Vec::with_capacity(vals.len());
    for value in vals {
        columns.push(value_to_string(value, "write_csv columns")?);
    }
    Ok(columns)
}

fn infer_csv_columns(rows: &[Value]) -> Result<Vec<String>, ShellError> {
    let mut columns = Vec::new();
    for row in rows {
        let Value::Record { val, .. } = row else {
            return Err(stone_error(
                "write_csv",
                format!("expected record row, got {}", row.get_type()),
            ));
        };
        for (key, _) in val.iter() {
            if !columns.contains(key) {
                columns.push(key.clone());
            }
        }
    }
    Ok(columns)
}

fn csv_field_value(value: &Value) -> Result<String, ShellError> {
    match value {
        Value::Nothing { .. } => Ok(String::new()),
        Value::Bool { val, .. } => Ok(val.to_string()),
        Value::Int { val, .. } => Ok(val.to_string()),
        Value::Float { val, .. } => Ok(val.to_string()),
        Value::String { val, .. } | Value::Glob { val, .. } => Ok(val.clone()),
        other => serde_json::to_string(&nu_to_json_value(other))
            .map_err(|err| stone_error("write_csv", err.to_string())),
    }
}

fn write_csv_line(text: &mut String, fields: &[String]) {
    let mut first = true;
    for field in fields {
        if !first {
            text.push(',');
        }
        first = false;
        write_csv_field(text, field);
    }
    text.push('\n');
}

fn write_csv_field(text: &mut String, field: &str) {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        text.push('"');
        for ch in field.chars() {
            if ch == '"' {
                text.push('"');
            }
            text.push(ch);
        }
        text.push('"');
    } else {
        text.push_str(&field);
    }
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

fn ensure_runtime_type(
    value: &RuntimeValue,
    expected: StoneType,
    context: &str,
) -> Result<(), ShellError> {
    if expected == StoneType::Any {
        return Ok(());
    }
    ensure_type(&value.clone().into_nu_value(context)?, expected, context)
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

fn apply_aug_op(op: AugOp, left: &Value, right: &Value) -> Result<Value, ShellError> {
    match op {
        AugOp::Add => add_values(left, right),
        AugOp::Sub => sub_values(left, right),
        AugOp::Mul => mul_values(left, right),
        AugOp::Div => div_values(left, right),
        AugOp::FloorDiv => floor_div_values(left, right),
        AugOp::Mod => mod_values(left, right),
        AugOp::BitAnd => bitwise_int_values(left, right, "bitwise and", |left, right| left & right),
        AugOp::BitOr => bitwise_int_values(left, right, "bitwise or", |left, right| left | right),
        AugOp::BitXor => bitwise_int_values(left, right, "bitwise xor", |left, right| left ^ right),
        AugOp::LShift => shift_value(left, right, "left shift", i64::checked_shl),
        AugOp::RShift => shift_value(left, right, "right shift", i64::checked_shr),
    }
}

fn shell_error_record(err: &ShellError) -> Value {
    let span = Span::unknown();
    let mut record = Record::new();
    match err {
        ShellError::Generic(generic) => {
            record.push("kind", Value::string(generic.error.to_string(), span));
            record.push("code", Value::string(generic.code.to_string(), span));
            record.push("message", Value::string(generic.msg.to_string(), span));
            if let Some(help) = &generic.help {
                record.push(
                    "suggested_next_action",
                    Value::string(help.to_string(), span),
                );
            }
        }
        ShellError::Io(io) => {
            record.push("kind", Value::string(io.to_string(), span));
            record.push("code", Value::string("stone_io_error", span));
            let message = match &io.path {
                Some(path) => format!("{io}: {}", path.display()),
                None => io.to_string(),
            };
            record.push("message", Value::string(message, span));
            if let Some(path) = &io.path {
                record.push("path", Value::string(path.display().to_string(), span));
            }
            record.push("operation", Value::string("io", span));
        }
        other => {
            record.push("kind", Value::string("runtime", span));
            record.push("code", Value::string("stone_runtime_error", span));
            record.push("message", Value::string(other.to_string(), span));
        }
    }
    Value::record(record, span)
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
        ..
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

fn stone_const_string(function: &StoneIrFunction, id: ConstId) -> Result<&str, ShellError> {
    match function.constants.get(id.0 as usize) {
        Some(StoneConst::String(value)) => Ok(value),
        _ => Err(stone_error("hot loop", "VM constant is not a string")),
    }
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
        RuntimeValue::AgentControl(_) => "agent_control",
        RuntimeValue::AttemptScope(_) => "attempt_scope",
        RuntimeValue::AttemptHandle(_) => "attempt_handle",
        RuntimeValue::AttemptOutcome(_) => "attempt_outcome",
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
        RuntimeValue::AgentControl(control) => Err(stone_error(
            "iteration",
            format!(
                "cannot iterate agent control {}#{}",
                control.name(),
                control.control_id
            ),
        )),
        RuntimeValue::AttemptScope(scope) => Err(stone_error(
            "iteration",
            format!(
                "cannot iterate attempt scope #{}; use scope.children",
                scope.scope_id
            ),
        )),
        RuntimeValue::AttemptHandle(handle) => {
            value_to_iter_values(&RuntimeValue::Nu(handle.materialize()))
        }
        RuntimeValue::AttemptOutcome(outcome) => {
            value_to_iter_values(&RuntimeValue::Nu(outcome.materialize()))
        }
    }
}

pub(super) fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}

fn attach_cleanup_error(program_error: ShellError, cleanup_error: ShellError) -> ShellError {
    match program_error {
        ShellError::Generic(error) => ShellError::Generic(error.with_inner(vec![cleanup_error])),
        other => ShellError::Generic(
            GenericError::new_internal(
                "Stone evaluation and attempt-scope cleanup both failed",
                "inspect both related errors before resuming the attempt",
            )
            .with_code("stone_script_error")
            .with_inner(vec![other, cleanup_error]),
        ),
    }
}

fn model_call_input_error(message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal("Stone model_call error", message.into())
            .with_code("model_invalid_request")
            .with_help(
                "Use help(\"model_call\") and correct the structured message or option value.",
            ),
    )
}

fn agent_session_value(task: Value, input: Value, attempt: Value, span: Span) -> Value {
    let limits = match &attempt {
        Value::Record { val, .. } => val
            .get("resource_limits")
            .cloned()
            .unwrap_or_else(|| Value::record(Record::new(), span)),
        _ => Value::record(Record::new(), span),
    };
    let names = |items: &[&str]| {
        Value::list(
            items
                .iter()
                .map(|name| Value::string(*name, span))
                .collect(),
            span,
        )
    };
    let mut tools = Record::new();
    tools.push(
        "resources",
        names(&["sysinfo", "state", "run_status", "run_wait"]),
    );
    tools.push(
        "files",
        names(&["read_file", "write_file", "find", "search", "diff"]),
    );
    tools.push(
        "linux",
        names(&["run", "must_run", "run_status", "run_wait", "run_terminate"]),
    );
    tools.push("model", names(&["model_call"]));
    tools.push("context", names(&["task_spec", "task_input"]));
    tools.push(
        "attempts",
        names(&[
            "attempt_spawn",
            "attempt_fork",
            "attempt_scope",
            "attempt_scope_close",
            "attempt_state",
            "attempt_wait",
            "attempt_join",
            "attempt_wait_any",
            "attempt_wait_all",
            "attempt_report",
            "attempt_accept",
            "attempt_discard",
            "attempt_publish",
        ]),
    );

    let mut session = Record::new();
    session.push("task", task);
    session.push("input", input);
    session.push("attempt", attempt);
    session.push("limits", limits);
    session.push("tools", Value::record(tools, span));
    Value::record(session, span)
}

fn agent_task_prompt(session: &JsonValue) -> Result<String, ShellError> {
    let session = session.as_object().ok_or_else(|| {
        stone_error(
            "agent control",
            "agent control session must be a record, normally from agent_session()",
        )
    })?;
    let task = session.get("task").ok_or_else(|| {
        stone_error(
            "agent control",
            "agent control session requires a structured `task` field",
        )
    })?;
    let input = session.get("input").cloned().unwrap_or(JsonValue::Null);
    let objective = task
        .get("objective")
        .and_then(JsonValue::as_str)
        .or_else(|| task.as_str())
        .unwrap_or("Complete the admitted task");
    let structured = serde_json::to_string(&json!({
        "task": task,
        "input": input,
    }))
    .map_err(|error| stone_error("agent control", error.to_string()))?;
    Ok(format!(
        "{objective}\nStructured task and input: {structured}"
    ))
}

fn attempt_id_from_value(value: &Value, context: &str) -> Result<String, ShellError> {
    match value {
        Value::String { val, .. } => Ok(val.clone()),
        Value::Record { val, .. } => val
            .get("attempt")
            .ok_or_else(|| stone_error(context, "attempt record has no `attempt` field"))
            .and_then(|value| value_to_string(value, context)),
        _ => Err(stone_error(
            context,
            format!("expected attempt id or record, got {}", value.get_type()),
        )),
    }
}

fn attempt_outcome_value(attempt: String, record: Value) -> AttemptOutcomeValue {
    let state = attempt_record_state(&record)
        .unwrap_or("unknown")
        .to_string();
    let controller_state = attempt_controller_state(&record)
        .unwrap_or("unknown")
        .to_string();
    let joined = matches!(
        controller_state.as_str(),
        "exited" | "failed" | "terminated"
    );
    AttemptOutcomeValue {
        attempt,
        joined,
        timed_out: !joined,
        state,
        controller_state,
        record,
    }
}

fn value_to_optional_timeout(value: &Value, context: &str) -> Result<Option<u32>, ShellError> {
    if matches!(value, Value::Nothing { .. }) {
        return Ok(None);
    }
    u32::try_from(value_to_u64(value, context)?)
        .map(Some)
        .map_err(|_| stone_error(context, "timeout_ms is too large"))
}

fn attempt_record_state(value: &Value) -> Option<&str> {
    let Value::Record { val, .. } = value else {
        return None;
    };
    let attempt = match val.get("attempt") {
        Some(Value::Record { val, .. }) => val.as_ref(),
        _ => val.as_ref(),
    };
    attempt.get("state").and_then(|value| value.as_str().ok())
}

fn attempt_controller_state(value: &Value) -> Option<&str> {
    let Value::Record { val, .. } = value else {
        return None;
    };
    let attempt = match val.get("attempt") {
        Some(Value::Record { val, .. }) => val.as_ref(),
        _ => val.as_ref(),
    };
    let Some(Value::Record { val: metadata, .. }) = attempt.get("metadata") else {
        return None;
    };
    metadata
        .get("controller_state")
        .and_then(|value| value.as_str().ok())
}

fn value_to_string_list(value: &Value, context: &str) -> Result<Vec<String>, ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            context,
            format!("expected list of strings, got {}", value.get_type()),
        ));
    };
    vals.iter()
        .map(|value| value_to_string(value, context))
        .collect()
}

fn value_to_string_pairs(
    value: &Value,
    context: &str,
) -> Result<Vec<(String, String)>, ShellError> {
    let Value::Record { val, .. } = value else {
        return Err(stone_error(
            context,
            format!("expected record of string values, got {}", value.get_type()),
        ));
    };
    val.iter()
        .map(|(key, value)| value_to_string(value, context).map(|value| (key.clone(), value)))
        .collect()
}

fn task_spec_from_value(value: &Value, context: &str) -> Result<GatewayTaskSpec, ShellError> {
    let record = value_record(value, context)?;
    Ok(GatewayTaskSpec {
        id: record_string_field(record, "id", context)?,
        objective: record_string_field(record, "objective", context)?,
        inputs: Vec::new(),
        outputs: Vec::new(),
        success: Vec::new(),
        constraints: Vec::new(),
        metadata: Default::default(),
    })
}

fn program_from_value(value: &Value, context: &str) -> Result<AttemptProgram, ShellError> {
    let record = value_record(value, context)?;
    if let Some(stone) = record_record_field(record, "stone", context)? {
        return Ok(AttemptProgram {
            program: Some(attempt_program::Program::Stone(StoneProgram {
                source: record_string_field(stone, "source", context)?,
                entrypoint: record_string_field(stone, "entrypoint", context)?,
            })),
        });
    }
    if let Some(builtin) = record_record_field(record, "builtin", context)? {
        return Ok(AttemptProgram {
            program: Some(attempt_program::Program::Builtin(BuiltinWorkflow {
                name: record_string_field(builtin, "name", context)?,
                args_json: record_string_field(builtin, "args_json", context)?,
            })),
        });
    }
    if let Some(artifact) = record_record_field(record, "artifact", context)? {
        return Ok(AttemptProgram {
            program: Some(attempt_program::Program::Artifact(ArtifactProgram {
                artifact: record_string_field(artifact, "artifact", context)?,
                entrypoint: record_string_field(artifact, "entrypoint", context)?,
                args_json: record_string_field(artifact, "args_json", context)?,
            })),
        });
    }

    let kind = record_string_field(record, "kind", context)?;
    let source = record_string_field(record, "source", context)?;
    let name = record_string_field(record, "name", context)?;
    let artifact = record_string_field(record, "artifact", context)?;
    match kind.as_str() {
        "stone" => Ok(AttemptProgram {
            program: Some(attempt_program::Program::Stone(StoneProgram {
                source,
                entrypoint: record_string_field(record, "entrypoint", context)?,
            })),
        }),
        "builtin" => Ok(AttemptProgram {
            program: Some(attempt_program::Program::Builtin(BuiltinWorkflow {
                name,
                args_json: record_string_field(record, "args_json", context)?,
            })),
        }),
        "artifact" => Ok(AttemptProgram {
            program: Some(attempt_program::Program::Artifact(ArtifactProgram {
                artifact,
                entrypoint: record_string_field(record, "entrypoint", context)?,
                args_json: record_string_field(record, "args_json", context)?,
            })),
        }),
        "" if !source.is_empty() => Ok(AttemptProgram {
            program: Some(attempt_program::Program::Stone(StoneProgram {
                source,
                entrypoint: record_string_field(record, "entrypoint", context)?,
            })),
        }),
        "" if !name.is_empty() => Ok(AttemptProgram {
            program: Some(attempt_program::Program::Builtin(BuiltinWorkflow {
                name,
                args_json: record_string_field(record, "args_json", context)?,
            })),
        }),
        "" if !artifact.is_empty() => Ok(AttemptProgram {
            program: Some(attempt_program::Program::Artifact(ArtifactProgram {
                artifact,
                entrypoint: record_string_field(record, "entrypoint", context)?,
                args_json: record_string_field(record, "args_json", context)?,
            })),
        }),
        other => Err(stone_error(
            context,
            format!("unknown attempt program kind `{other}`"),
        )),
    }
}

fn apply_stone_entrypoint(
    program: &mut Option<AttemptProgram>,
    entrypoint: &str,
    context: &str,
) -> Result<(), ShellError> {
    if entrypoint.is_empty() {
        return Ok(());
    }
    let Some(program) = program
        .as_mut()
        .and_then(|program| program.program.as_mut())
    else {
        return Err(stone_error(
            context,
            "entrypoint requires an explicit Stone program",
        ));
    };
    match program {
        attempt_program::Program::Stone(stone) => {
            stone.entrypoint = entrypoint.to_string();
            Ok(())
        }
        _ => Err(stone_error(
            context,
            "entrypoint is currently supported only for Stone programs",
        )),
    }
}

fn workspace_source_from_value(
    value: &Value,
    context: &str,
) -> Result<WorkspaceSource, ShellError> {
    let record = value_record(value, context)?;
    Ok(WorkspaceSource {
        kind: record_string_field(record, "kind", context)?,
        workspace: record_string_field(record, "workspace", context)?,
        generation: record_string_field(record, "generation", context)?,
        attempt: record_string_field(record, "attempt", context)?,
        checkpoint: record_string_field(record, "checkpoint", context)?,
    })
}

fn context_source_from_value(value: &Value, context: &str) -> Result<ContextSource, ShellError> {
    let record = value_record(value, context)?;
    Ok(ContextSource {
        kind: record_string_field(record, "kind", context)?,
        attempt: record_string_field(record, "attempt", context)?,
        context: record_string_field(record, "context", context)?,
        include_last_turns: record_u32_field(record, "include_last_turns", context)?,
    })
}

fn value_record<'a>(value: &'a Value, context: &str) -> Result<&'a Record, ShellError> {
    let Value::Record { val, .. } = value else {
        return Err(stone_error(
            context,
            format!("expected record, got {}", value.get_type()),
        ));
    };
    Ok(val)
}

fn record_record_field<'a>(
    record: &'a Record,
    field: &str,
    context: &str,
) -> Result<Option<&'a Record>, ShellError> {
    match record.get(field) {
        None | Some(Value::Nothing { .. }) => Ok(None),
        Some(value @ Value::Record { .. }) => Ok(Some(value_record(value, context)?)),
        Some(other) => Err(stone_error(
            context,
            format!(
                "record field `{field}` expected record, got {}",
                other.get_type()
            ),
        )),
    }
}

fn record_string_field(record: &Record, field: &str, context: &str) -> Result<String, ShellError> {
    record
        .get(field)
        .map(|value| value_to_string(value, context))
        .transpose()
        .map(|value| value.unwrap_or_default())
}

fn record_u32_field(record: &Record, field: &str, context: &str) -> Result<u32, ShellError> {
    let Some(value) = record.get(field) else {
        return Ok(0);
    };
    match value {
        Value::Int { val, .. } => u32::try_from(*val).map_err(|_| {
            stone_error(
                context,
                format!("record field `{field}` expects an unsigned integer"),
            )
        }),
        Value::Nothing { .. } => Ok(0),
        other => Err(stone_error(
            context,
            format!(
                "record field `{field}` expected int, got {}",
                other.get_type()
            ),
        )),
    }
}

fn run_record_ok(record: &Record) -> bool {
    matches!(record.get("ok"), Some(Value::Bool { val: true, .. }))
}

fn must_run_failure_error(record: Record) -> ShellError {
    let argv = record
        .get("argv")
        .map(|value| nu_to_json_value(value).to_string())
        .unwrap_or_else(|| "[]".to_owned());
    let kind = record
        .get("kind")
        .and_then(|value| match value {
            Value::String { val, .. } => Some(val.as_str()),
            _ => None,
        })
        .unwrap_or("failed");
    let exit_code = record
        .get("exit_code")
        .map(|value| nu_to_json_value(value).to_string())
        .unwrap_or_else(|| "null".to_owned());
    let summary = record
        .get("explanation")
        .and_then(|value| match value {
            Value::Record { val, .. } => val.get("summary"),
            _ => None,
        })
        .and_then(|value| match value {
            Value::String { val, .. } => Some(val.clone()),
            _ => None,
        })
        .unwrap_or_else(|| format!("external command failed with kind `{kind}`"));
    let detail = Value::record(record, Span::unknown());
    ShellError::Generic(
        GenericError::new(
            "Checked run failed",
            format!("must_run command failed: argv={argv}, exit_code={exit_code}; {summary}"),
            Span::unknown(),
        )
        .with_code("stone_must_run_failed")
        .with_help(
            "Use run(...) instead when a nonzero exit is expected and should be handled as data.",
        )
        .with_inner(vec![ShellError::Generic(
            GenericError::new_internal("must_run result", nu_to_json_value(&detail).to_string())
                .with_code("stone_must_run_detail"),
        )]),
    )
}

fn wait_for_record(
    ok: bool,
    attempts: i64,
    elapsed: Duration,
    value: Option<Value>,
    error: Option<String>,
) -> Value {
    let span = Span::unknown();
    let duration_ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
    let mut record = Record::new();
    record.push("ok", Value::bool(ok, span));
    record.push(
        "kind",
        Value::string(if ok { "ready" } else { "timeout" }, span),
    );
    record.push("attempts", Value::int(attempts, span));
    record.push("duration_ms", Value::int(duration_ms, span));
    if ok {
        record.push("value", value.unwrap_or_else(|| Value::bool(true, span)));
    } else {
        if let Some(value) = value {
            record.push("last_value", value);
        }
        if let Some(error) = error {
            record.push("last_error", Value::string(error, span));
        }
        let mut explanation = Record::new();
        explanation.push("kind", Value::string("wait_for_timeout", span));
        explanation.push(
            "summary",
            Value::string(
                "wait_for() predicate did not become truthy before the timeout.",
                span,
            ),
        );
        explanation.push(
            "next_steps",
            Value::list(
                vec![
                    Value::string("Inspect the condition inputs, such as logs, files, process status, or service health.", span),
                    Value::string("If startup is expected to be slow, rerun with a larger timeout_ms.", span),
                    Value::string("If the predicate can fail while resources are still appearing, use ignore_errors=True deliberately.", span),
                ],
                span,
            ),
        );
        record.push("explanation", Value::record(explanation, span));
    }
    Value::record(record, span)
}

fn unknown_stone_call_error(name: &str) -> ShellError {
    if name == "isinstance" {
        return stone_error(
            "function call",
            "isinstance(value, type) is not supported because Stone has no Python class model; use type(value) == \"list\"/\"str\"/\"int\"/\"float\"/\"record\" or direct structural checks",
        );
    }
    stone_error(
        "function call",
        format!("unknown Stone function `{name}`; use help() for available Stone functions"),
    )
}

fn collect_env_restore_paths(value: &Value, paths: &mut Vec<String>) -> Result<(), ShellError> {
    match value {
        Value::List { vals, .. } => {
            for value in vals {
                collect_env_restore_paths(value, paths)?;
            }
            Ok(())
        }
        _ => {
            paths.push(value_to_path_string(value, "env_restore paths")?);
            Ok(())
        }
    }
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
        agent_session_value, eval_program, eval_program_with_options, eval_program_with_output,
        match_fused_map_update_if, EvalHotLoopDiagnostics, EvalOptions, RuntimeValue, TextLines,
        STONE_BUILTIN_NAMES,
    };
    use crate::{
        commands::{
            stone_help_documented_names_for_tests, stone_help_entries_without_examples_for_tests,
        },
        json,
        stone_ast::lower_source,
        stone_vm::LoopIrOptimizationDiagnostic,
    };
    use nu_protocol::{
        engine::{EngineState, Stack},
        PipelineData, ShellError, Span, Value,
    };
    use serde_json::json as json_value;
    use std::{
        collections::BTreeSet,
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
    fn isinstance_error_suggests_stone_type_checks() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("isinstance-call")?;
        let program = lower_source(r#"emit(isinstance(["x"], list))"#)?;
        let err = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("isinstance should stay unsupported with a targeted hint");
        let text = format!("{err:?}");
        assert!(text.contains("type(value)"), "unexpected error: {text}");
        assert!(text.contains("list"), "unexpected error: {text}");

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
    fn evaluates_wait_for_predicate_builtin() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("wait-for-builtin")?;
        let program = lower_source(
            r#"ready = wait_for(lambda: True, timeout_ms=1000, interval_ms=5)
timed = wait_for(lambda: False, timeout_ms=10, interval_ms=5)
missing = wait_for(lambda: read_file("missing.log").find("READY") >= 0, timeout_ms=10, interval_ms=5, ignore_errors=True)
emit({
    "ready_ok": ready.ok,
    "ready_kind": ready.kind,
    "ready_value": ready.value,
    "timed_ok": timed.ok,
    "timed_kind": timed.kind,
    "timed_last_value": timed.last_value,
    "missing_ok": missing.ok,
    "missing_kind": missing.kind,
    "missing_has_error": "last_error" in missing,
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "ready_ok": true,
                "ready_kind": "ready",
                "ready_value": true,
                "timed_ok": false,
                "timed_kind": "timeout",
                "timed_last_value": false,
                "missing_ok": false,
                "missing_kind": "timeout",
                "missing_has_error": true,
            })
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
    "tuple_keys": tuple(record.keys()),
    "record_as_list": list(record),
    "record_as_tuple": tuple(record),
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
                "tuple_keys": ["a", "b"],
                "record_as_list": ["a", "b"],
                "record_as_tuple": ["a", "b"],
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
    fn untyped_zero_argument_function_returns_structured_value() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("untyped-zero-arg-return")?;
        let program = lower_source(
            r#"def solve():
    return {"answer": "ready", "turns": 1}

emit(solve())
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        assert_eq!(
            json::pipeline_to_json_value(output, Span::unknown())?,
            json_value!({"answer": "ready", "turns": 1})
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
    fn print_accepts_multiple_arguments() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("print-multiple-arguments")?;
        let program = lower_source(
            r#"print("Line", 1, ":", "value")
print()
"#,
        )?;
        let output =
            eval_program_with_output(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output.pipeline, nu_protocol::Span::unknown())?,
            json_value!(null)
        );
        assert_eq!(output.stdout, "Line 1 : value\n\n");

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
agent_control = help("agent_control")
agent_session = help("agent_session")
missing = help("not_a_builtin")
emit({
    "language": overview["language"],
    "has_unsupported": len(overview["unsupported"]) > 0,
    "has_session_topic": "session" in overview["topics"],
    "has_agent_control_topic": "agent_control" in overview["topics"],
    "for_llm_mentions_bindings": "bindings persist" in overview["for_llm"],
    "for_llm_mentions_multiline_eval": "multi-line script" in overview["for_llm"],
    "session_mentions_live_bindings": "live name binding" in session["bullets"][1],
    "session_mentions_binding_ack": "bound names" in session["bullets"][2],
    "agent_control_mentions_builtin": "optimized AgentControl" in agent_control["bullets"][6],
    "agent_session_signature": agent_session["signature"],
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
                "has_agent_control_topic": true,
                "for_llm_mentions_bindings": true,
                "for_llm_mentions_multiline_eval": true,
                "session_mentions_live_bindings": true,
                "session_mentions_binding_ack": true,
                "agent_control_mentions_builtin": true,
                "agent_session_signature": "agent_session() -> record",
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
    fn agent_session_value_exposes_structured_control_resources() {
        let span = Span::unknown();
        let session = agent_session_value(
            json::json_to_nu_value(json_value!({"objective": "solve"}), span),
            json::json_to_nu_value(json_value!({"strategy": "bounded"}), span),
            json::json_to_nu_value(
                json_value!({
                    "attempt": "attempt-7",
                    "resource_limits": {"model_calls": "4", "memory": "1GiB"},
                }),
                span,
            ),
            span,
        );
        let session = json::nu_to_json_value(&session);

        assert_eq!(session["task"]["objective"], json_value!("solve"));
        assert_eq!(session["input"]["strategy"], json_value!("bounded"));
        assert_eq!(session["attempt"]["attempt"], json_value!("attempt-7"));
        assert_eq!(session["limits"]["model_calls"], json_value!("4"));
        assert!(session["tools"]["model"]
            .as_array()
            .unwrap()
            .contains(&json_value!("model_call")));
        assert!(session["tools"]["attempts"]
            .as_array()
            .unwrap()
            .contains(&json_value!("attempt_fork")));
    }

    #[test]
    fn stone_help_covers_every_builtin_name() {
        let mut builtin_names: BTreeSet<&'static str> =
            STONE_BUILTIN_NAMES.iter().copied().collect();
        builtin_names.extend(["get", "keys", "values", "items"]);

        let documented = stone_help_documented_names_for_tests();
        let missing: Vec<_> = builtin_names.difference(&documented).copied().collect();

        assert!(
            missing.is_empty(),
            "every Stone builtin must have a help() entry or alias with examples; missing: {missing:?}"
        );

        let entries_without_examples = stone_help_entries_without_examples_for_tests();
        assert!(
            entries_without_examples.is_empty(),
            "every Stone help() entry must include at least one example; missing examples: {entries_without_examples:?}"
        );
    }

    #[test]
    fn model_call_help_exposes_explicit_single_effect() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("model-call-help")?;
        let program = lower_source(
            r#"topic = help("model_call")
emit({
    "found": topic.found,
    "has_messages": "messages: list[record]" in topic.signature,
    "gateway_only": "Gateway mode only" in topic.use_when,
    "no_retry": "exactly one model effect" in topic.avoid[1],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        assert_eq!(
            json::pipeline_to_json_value(output, Span::unknown())?,
            json_value!({
                "found": true,
                "has_messages": true,
                "gateway_only": true,
                "no_retry": true,
            })
        );
        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn model_call_rejects_invalid_messages_and_options_before_rpc() -> Result<(), ShellError> {
        let cases = [
            (r#"model_call([])"#, "messages list must not be empty"),
            (
                r#"model_call([{"role": "tool", "content": "result"}])"#,
                "unsupported model_call message role",
            ),
            (
                r#"model_call([{"role": "user", "content": "hello", "secret": "no"}])"#,
                "unsupported model_call message field",
            ),
            (
                r#"model_call([{"role": "user", "content": "hello"}], top_p=2.0)"#,
                "top_p must be greater than 0 and at most 1",
            ),
            (
                r#"model_call([{"role": "user", "content": "hello"}], seed=0)"#,
                "seed must be positive",
            ),
            (
                r#"model_call([{"role": "user", "content": "hello"}], metadata={"round": 1})"#,
                "metadata field",
            ),
        ];

        for (index, (source, expected)) in cases.into_iter().enumerate() {
            let (engine_state, mut stack, root) =
                test_engine(&format!("model-call-invalid-{index}"))?;
            let program = lower_source(source)?;
            let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
                .expect_err("invalid model_call should fail before Gateway access");
            let text = format!("{error:?}");
            assert!(text.contains("model_invalid_request"), "{text}");
            assert!(text.contains(expected), "expected {expected:?} in {text}");
            cleanup_dir(&root);
        }
        Ok(())
    }

    #[test]
    fn stone_help_supported_and_unsupported_syntax_stay_current() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("stone-help-syntax")?;
        let program = lower_source(
            r#"syntax = "\n".join(help("syntax")["bullets"])
unsupported = "\n".join(help("unsupported")["bullets"])
emit({
    "syntax_mentions_try": "try/except catches runtime evaluation errors" in syntax,
    "syntax_mentions_conditional": "value if condition else fallback" in syntax,
    "syntax_mentions_defaults": "immutable default values are supported" in syntax,
    "syntax_mentions_split": "default whitespace splitting" in syntax,
    "unsupported_try": "No try/except" in unsupported,
    "unsupported_conditional": "No conditional expression" in unsupported,
    "unsupported_default_args": "No default args" in unsupported,
    "unsupported_default_split": "No default split" in unsupported,
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "syntax_mentions_try": true,
                "syntax_mentions_conditional": true,
                "syntax_mentions_defaults": true,
                "syntax_mentions_split": true,
                "unsupported_try": false,
                "unsupported_conditional": false,
                "unsupported_default_args": false,
                "unsupported_default_split": false,
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_hash_builtins() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("hash-builtins")?;
        let program = lower_source(
            r#"
emit({
    "md5": md5("abcdefghijklmnopqrstuvwxyz"),
    "sha1": sha1("abcdefghijklmnopqrstuvwxyz"),
    "sha256": sha256("abcdefghijklmnopqrstuvwxyz"),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "md5": "c3fcd3d76192e4007dfb496cca67e13b",
                "sha1": "32d10c7b8cf96570ca04ce37f2a19d84240d3a89",
                "sha256": "71c480df93d6ae2f1efad1447c66c9525e316218cf51fc8d9ed832f2daf18b73",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_common_python_method_compatibility() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("common-methods")?;
        fs::write(root.join("input.txt"), "alpha\nbeta\n").expect("write input fixture");
        let program = lower_source(
            r#"
items = [3, 1]
items.extend([2, 3])
items.sort()
pairs = [["b", 1], ["a", 2], ["b", 0]]
pairs.sort(key=lambda pair: (pair[0], pair[1]), reverse=True)
lines = open("input.txt").readlines()
emit({
    "items": items,
    "pairs": pairs,
    "list_count": items.count(3),
    "text_count": "banana".count("an"),
    "alpha": "abcXYZ".isalpha(),
    "mixed_alpha": "abc123".isalpha(),
    "alnum": "abc123".isalnum(),
    "mixed_alnum": "abc-123".isalnum(),
    "rsplit": "a/b/c".rsplit("/", 1),
    "rsplit_words": " alpha  beta gamma ".rsplit(None, 1),
    "lines": lines,
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "items": [1, 2, 3, 3],
                "pairs": [["b", 1], ["b", 0], ["a", 2]],
                "list_count": 2,
                "text_count": 2,
                "alpha": true,
                "mixed_alpha": false,
                "alnum": true,
                "mixed_alnum": false,
                "rsplit": ["a/b", "c"],
                "rsplit_words": ["alpha beta", "gamma"],
                "lines": ["alpha", "beta"],
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn generator_expression_error_mentions_supported_contexts() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("generator-error")?;
        let program = lower_source(r#"emit(int(x) for x in ["1"])"#)?;
        let err = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("bare generator expression should fail");
        let text = format!("{err:?}");

        assert!(text.contains("sum(...), any(...), all(...), join(...), and set(...)"));
        assert!(text.contains("list comprehension"));

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
    fn evaluates_llm_compatibility_features() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("llm-compat-features")?;
        fs::write(root.join("bad.json"), "{not json").expect("write bad json");
        let program = lower_source(
            r#"row = {"size": 7}
empty = {}
ternary = {
    "true": row["size"] if "size" in row else unknown_name,
    "false": unknown_name if "size" in empty else 0,
    "nested": "big" if row["size"] > 10 else ("medium" if row["size"] > 5 else "small"),
}
total = 20
total -= 3
total *= 2
total //= 5
total %= 5
ratio = 9
ratio /= 2
mask = 1
mask |= 4
mask &= 5
mask ^= 1
mask <<= 2
mask >>= 1
stats = {"alice": {"count": 10, "mask": 1}}
stats["alice"]["count"] -= 3
stats["alice"]["mask"] |= 2
def head(path, limit=10):
    return path + ":" + str(limit)
def choose(value: int = 4, scale: int = 2) -> int:
    return value * scale
try:
    missing = read_text("missing.txt")
except Exception as e:
    missing = {"code": e.code, "message": e.message}
try:
    parsed = read_json("bad.json")
except:
    parsed = {"fallback": True}
try:
    ok = read_text("bad.json")
except Exception:
    ok = "handler should not run"
rows = [{"name": "ada", "items": [1, 2]}, {"name": "grace", "items": [3]}]
counts = {"ada": 2, "grace": 1}
pairs = [name + ":" + str(count) for name, count in counts.items()]
dict_pairs = {name: count * 10 for name, count in counts.items()}
flat = [row["name"] + ":" + str(item) for row in rows for item in row["items"] if item > 1]
total_from_filter = sum(int(x) for x in ["", "2", "3"] if x)
checks = {
    "any_empty": any([]),
    "all_empty": all([]),
    "any_list": any([False, 0, "ok"]),
    "all_list": all([1, "ok", True]),
    "any_records": any(row["name"] == "grace" for row in rows),
    "all_records": all("items" in row for row in rows),
    "any_string": any(line == "beta" for line in "alpha\nbeta".splitlines()),
    "any_short": any(flag or unknown_name for flag in [True]),
    "all_short": all(flag and unknown_name for flag in [False]),
}
seen = set(["a", "a", "b"])
seen.add("c")
seen.add("c")
emit({
    "ternary": ternary,
    "aug": {"total": total, "ratio": ratio, "mask": mask, "stats": stats},
    "defaults": [head("p"), head("p", 3), choose(), choose(5), choose(5, 3)],
    "try": {"missing_code": missing["code"], "missing_has_path": "missing.txt" in missing["message"], "parsed": parsed, "ok": ok},
    "comprehensions": {"pairs": pairs, "dict_pairs": dict_pairs, "flat": flat, "total": total_from_filter},
    "checks": checks,
    "set": seen,
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "ternary": {"true": 7, "false": 0, "nested": "medium"},
                "aug": {
                    "total": 1,
                    "ratio": 4.5,
                    "mask": 8,
                    "stats": {"alice": {"count": 7, "mask": 3}},
                },
                "defaults": ["p:10", "p:3", 8, 10, 15],
                "try": {
                    "missing_code": "stone_io_error",
                    "missing_has_path": true,
                    "parsed": {"fallback": true},
                    "ok": "{not json",
                },
                "comprehensions": {
                    "pairs": ["ada:2", "grace:1"],
                    "dict_pairs": {"ada": 20, "grace": 10},
                    "flat": ["ada:2", "grace:3"],
                    "total": 5,
                },
                "checks": {
                    "any_empty": false,
                    "all_empty": true,
                    "any_list": true,
                    "all_list": true,
                    "any_records": true,
                    "all_records": true,
                    "any_string": true,
                    "any_short": true,
                    "all_short": false,
                },
                "set": ["a", "b", "c"],
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
limited = "a:b:c".split(":", 1)
keyword_limited = "a:b:c".split(":", maxsplit=1)
top_limited = split("a:b:c", ":", 1)
top_keyword_limited = split("a b  c", None, maxsplit=1)
emit({
    "left": parts[0],
    "right": parts[1],
    "txt": parts[1].endswith(".txt"),
    "missing": "gamma" not in parts,
    "limited": limited,
    "keyword_limited": keyword_limited,
    "top_limited": top_limited,
    "top_keyword_limited": top_keyword_limited,
})
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
                "limited": ["a", "b:c"],
                "keyword_limited": ["a", "b:c"],
                "top_limited": ["a", "b:c"],
                "top_keyword_limited": ["a", "b  c"],
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
    "join_generator": join(part[0] for part in items),
    "join_reversed_args": join("/", ["tmp", "waymark"]),
    "slice": slice(items, 1, 3),
    "tail": slice(items, 1),
    "prefix": slice("abcdef", None, 3),
    "starts_with": starts_with("abcdef", "abc"),
    "startswith": startswith("abcdef", "def"),
    "set_generator": set(part[0] for part in items),
    "format": format("{}:{} {{ok}} {:.2f}", "port", 8080, 3),
    "format_index": format("{1}/{0}/{1:.1f}", "x", 2),
    "max": max(1, 7, 3),
    "repr": repr(["ok", 2]),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "split": ["alpha", "beta", "gamma"],
                "join": "alpha|beta|gamma",
                "join_generator": "abg",
                "join_reversed_args": "tmp/waymark",
                "slice": ["beta", "gamma"],
                "tail": ["beta", "gamma"],
                "prefix": "abc",
                "starts_with": true,
                "startswith": false,
                "set_generator": ["a", "b", "g"],
                "format": "port:8080 {ok} 3.00",
                "format_index": "2/x/2.0",
                "max": 7,
                "repr": "[\"ok\",2]",
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
    fn evaluates_read_csv_records_with_multiline_quoted_fields() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("read-csv-multiline")?;
        fs::write(
            root.join("input.csv"),
            "id,note\n1,\"alpha\nbeta\"\n2,\"said \"\"ok\"\"\"\n",
        )
        .expect("write csv");
        let program = lower_source(
            r#"rows = read_csv("input.csv", limit=1)
emit({
    "count": len(rows),
    "id": rows[0]["id"],
    "note": rows[0]["note"]
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "count": 1,
                "id": "1",
                "note": "alpha\nbeta",
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn evaluates_write_csv_records() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("write-csv")?;
        let program = lower_source(
            r#"rows = [
    {"name": "Ada", "score": 10, "note": "alpha, beta"},
    {"name": "Grace", "score": 7, "note": "said \"ok\"", "extra": True},
]
write_csv("out.csv", rows, columns=["name", "score", "note", "extra"])
roundtrip = read_csv("out.csv")
emit({"text": cat("out.csv"), "rows": roundtrip})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "text": "name,score,note,extra\nAda,10,\"alpha, beta\",\nGrace,7,\"said \"\"ok\"\"\",true\n",
                "rows": [
                    {"name": "Ada", "score": "10", "note": "alpha, beta", "extra": ""},
                    {"name": "Grace", "score": "7", "note": "said \"ok\"", "extra": "true"},
                ],
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
    fn differential_tests_compare_baseline_and_hot_loop_interpreters() -> Result<(), ShellError> {
        assert_hot_loop_matches_baseline(
            "differential-jsonl-aggregation",
            r#"user_amounts = {}
user_items = {}
tag_counts = {}
users = []
tags_seen = []
for row in read_jsonl("records.jsonl"):
    user = row.get("user", "unknown")
    amount = float(row.get("amount", 0.0))
    items = int(row.get("items", 0))
    if user not in users:
        users.append(user)
    if user in user_amounts:
        user_amounts[user] += amount
        user_items[user] += items
    else:
        user_amounts[user] = amount
        user_items[user] = items
    for tag in row.get("tags", []):
        if tag not in tags_seen:
            tags_seen.append(tag)
        if tag in tag_counts:
            tag_counts[tag] += 1
        else:
            tag_counts[tag] = 1
emit({"amounts": user_amounts, "items": user_items, "tags": tag_counts, "users": users, "tags_seen": tags_seen})
"#,
            &[(
                "records.jsonl",
                "{\"user\":\"ada\",\"amount\":1.5,\"items\":2,\"tags\":[\"a\",\"b\"]}\n\
                 {\"user\":\"grace\",\"amount\":2.0,\"items\":1,\"tags\":[\"a\"]}\n\
                 {\"user\":\"ada\",\"amount\":3.25,\"items\":4,\"tags\":[\"b\",\"c\"]}\n",
            )],
        )?;
        assert_hot_loop_matches_baseline(
            "differential-text-lines-parse",
            r#"total = 0
for line in open("numbers.txt").splitlines():
    total += int(line)
emit(total)
"#,
            &[("numbers.txt", "5\n-2\n9\n")],
        )?;
        assert_hot_loop_matches_baseline(
            "differential-range-expression-body",
            r#"total = 0
mask = 0
for n in range(8):
    total = total + (n * 3) - (n // 2)
    mask = (mask | n) ^ (n << 1)
emit({"total": total, "mask": mask})
"#,
            &[],
        )?;
        Ok(())
    }

    #[test]
    fn generated_semantic_cases_match_baseline_and_hot_loop() -> Result<(), ShellError> {
        let cases: &[(&str, &[i64])] = &[
            ("empty", &[]),
            ("single", &[7]),
            ("mixed", &[3, -1, 4, -1, 5]),
            ("zeros", &[0, 0, 0]),
        ];
        for (name, numbers) in cases {
            let literal = stone_int_list_literal(numbers);
            let expected_sum: i64 = numbers.iter().sum();
            let source = format!(
                r#"total = 0
for n in {literal}:
    total += n
emit(total)
"#
            );
            assert_hot_loop_matches_baseline(
                &format!("generated-sum-{name}-{expected_sum}"),
                &source,
                &[],
            )?;

            let source = format!(
                r#"seen = []
for n in {literal}:
    if not n in seen:
        seen.append(n)
emit(seen)
"#
            );
            assert_hot_loop_matches_baseline(&format!("generated-unique-{name}"), &source, &[])?;
        }

        let tag_cases: &[(&str, &[&str])] = &[
            ("empty-tags", &[]),
            ("repeated-tags", &["a", "b", "a", "c", "b", "a"]),
            ("case-sensitive-tags", &["A", "a", "A"]),
        ];
        for (name, tags) in tag_cases {
            let literal = stone_str_list_literal(tags);
            let source = format!(
                r#"counts = {{}}
for tag in {literal}:
    if tag in counts:
        counts[tag] += 1
    else:
        counts[tag] = 1
emit(counts)
"#
            );
            assert_hot_loop_matches_baseline(&format!("generated-counts-{name}"), &source, &[])?;
        }

        Ok(())
    }

    #[test]
    fn golden_semantic_tests_lock_down_interpreter_results() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("golden-semantics")?;
        let program = lower_source(
            r#"rows = [{"name": "Ada", "score": 3}, {"name": "Grace", "score": 5}, {"name": "Linus", "score": 5}]
scores = []
for row in rows:
    scores.append(row.score)
best = sort(rows, key=lambda row: row["name"])[-1]
counts = {}
for score in scores:
    key = str(score)
    if key in counts:
        counts[key] += 1
    else:
        counts[key] = 1
def scale(value: int) -> int:
    return value * 10
scaled = []
for score in scores:
    scaled.append(scale(score))
emit({
    "first_two": scores[0:2],
    "last": scores[-1],
    "best_name": best.name,
    "counts": counts,
    "scaled": scaled,
    "filtered": map(lambda row: row.name, filter(lambda row: row.score == 5, rows)),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "first_two": [3, 5],
                "last": 5,
                "best_name": "Linus",
                "counts": {"3": 1, "5": 2},
                "scaled": [30, 50, 50],
                "filtered": ["Grace", "Linus"],
            })
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn expression_operator_matrix_locks_down_baseline_semantics() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("expr-operator-matrix")?;
        let program = lower_source(
            r#"values = [1, 2, 3]
names = {"ada": 1, "grace": 2}
missing = None
present = 0
emit({
    "eq": 2 == 2,
    "eq_int_float": 2 == 2.0,
    "eq_bool_int_strong": True == 1,
    "eq_string": "a" == "a",
    "eq_list": [1, "a", [2]] == [1, "a", [2]],
    "eq_record": {"a": 1, "b": [2]} == {"b": [2], "a": 1},
    "neq_list": [1, 2] != [1, 3],
    "not_eq": 2 != 3,
    "lt": 1 < 2,
    "lt_float_mixed": 1.5 < 2,
    "lt_string": "a" < "b",
    "lt_list": [1, 2] < [1, 3],
    "lt_list_prefix": [1] < [1, 0],
    "lte_equal": 2 <= 2,
    "lte_less": 2 <= 3,
    "gt": 3 > 2,
    "gt_float_mixed": 3 > 2.5,
    "gt_string": "b" > "a",
    "gt_list": [1, 4] > [1, 3],
    "gte_equal": 3 >= 3,
    "gte_greater": 3 >= 2,
    "chain_true": 1 < 2 < 3,
    "chain_false": 1 < 3 < 2,
    "list_in": 2 in values,
    "list_not_in": 4 not in values,
    "dict_key_in": "ada" in names,
    "dict_key_not_in": "linus" not in names,
    "string_in": "mark" in "waymark",
    "string_not_in": "shell" not in "waymark",
    "is_none": missing is None,
    "is_not_none": present is not None,
    "and_true": True and 1 and "x",
    "and_zero_false": True and 0 and unknown_name,
    "and_false_short": True and False and unknown_name,
    "or_true": False or "" or "fallback",
    "or_short": True or unknown_name,
    "not_false": not False,
    "not_zero": not 0,
    "not_empty": not [],
    "neg_int": -7,
    "neg_float": -2.5,
    "add_int": 4 + 3,
    "add_float": 4.5 + 3,
    "add_string": "way" + "mark",
    "add_list": [1, 2] + [3],
    "sub_int": 4 - 7,
    "sub_float": 4 - 1.5,
    "mul_int": 6 * 7,
    "mul_float": 2.5 * 4,
    "div_int": 7 / 2,
    "div_float": 7.5 / 2.5,
    "floor_div": 7 // 2,
    "floor_div_negative_left": -7 // 2,
    "floor_div_negative_right": 7 // -2,
    "floor_div_both_negative": -7 // -2,
    "floor_div_float": -7.0 // 2,
    "mod_int": 7 % 3,
    "mod_negative_left": -7 % 2,
    "mod_negative_right": 7 % -2,
    "mod_both_negative": -7 % -2,
    "mod_float": -5.5 % 2.0,
    "bit_and": 6 & 3,
    "bit_or": 4 | 1,
    "bit_xor": 7 ^ 3,
    "left_shift": 3 << 2,
    "right_shift": 8 >> 1,
    "bit_invert": ~5
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        let actual = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        let expected = [
            ("eq", json_value!(true)),
            ("eq_int_float", json_value!(true)),
            ("eq_bool_int_strong", json_value!(false)),
            ("eq_string", json_value!(true)),
            ("eq_list", json_value!(true)),
            ("eq_record", json_value!(true)),
            ("neq_list", json_value!(true)),
            ("not_eq", json_value!(true)),
            ("lt", json_value!(true)),
            ("lt_float_mixed", json_value!(true)),
            ("lt_string", json_value!(true)),
            ("lt_list", json_value!(true)),
            ("lt_list_prefix", json_value!(true)),
            ("lte_equal", json_value!(true)),
            ("lte_less", json_value!(true)),
            ("gt", json_value!(true)),
            ("gt_float_mixed", json_value!(true)),
            ("gt_string", json_value!(true)),
            ("gt_list", json_value!(true)),
            ("gte_equal", json_value!(true)),
            ("gte_greater", json_value!(true)),
            ("chain_true", json_value!(true)),
            ("chain_false", json_value!(false)),
            ("list_in", json_value!(true)),
            ("list_not_in", json_value!(true)),
            ("dict_key_in", json_value!(true)),
            ("dict_key_not_in", json_value!(true)),
            ("string_in", json_value!(true)),
            ("string_not_in", json_value!(true)),
            ("is_none", json_value!(true)),
            ("is_not_none", json_value!(true)),
            ("and_true", json_value!(true)),
            ("and_zero_false", json_value!(false)),
            ("and_false_short", json_value!(false)),
            ("or_true", json_value!(true)),
            ("or_short", json_value!(true)),
            ("not_false", json_value!(true)),
            ("not_zero", json_value!(true)),
            ("not_empty", json_value!(true)),
            ("neg_int", json_value!(-7)),
            ("neg_float", json_value!(-2.5)),
            ("add_int", json_value!(7)),
            ("add_float", json_value!(7.5)),
            ("add_string", json_value!("waymark")),
            ("add_list", json_value!([1, 2, 3])),
            ("sub_int", json_value!(-3)),
            ("sub_float", json_value!(2.5)),
            ("mul_int", json_value!(42)),
            ("mul_float", json_value!(10.0)),
            ("div_int", json_value!(3.5)),
            ("div_float", json_value!(3.0)),
            ("floor_div", json_value!(3)),
            ("floor_div_negative_left", json_value!(-4)),
            ("floor_div_negative_right", json_value!(-4)),
            ("floor_div_both_negative", json_value!(3)),
            ("floor_div_float", json_value!(-4.0)),
            ("mod_int", json_value!(1)),
            ("mod_negative_left", json_value!(1)),
            ("mod_negative_right", json_value!(-1)),
            ("mod_both_negative", json_value!(-1)),
            ("mod_float", json_value!(0.5)),
            ("bit_and", json_value!(2)),
            ("bit_or", json_value!(5)),
            ("bit_xor", json_value!(4)),
            ("left_shift", json_value!(12)),
            ("right_shift", json_value!(4)),
            ("bit_invert", json_value!(-6)),
        ];
        for (key, expected) in expected {
            assert_eq!(actual[key], expected, "{key}");
        }

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn expression_operator_edge_errors_do_not_return_values() -> Result<(), ShellError> {
        let cases = [
            ("div-zero", r#"emit(1 / 0)"#, "division by zero"),
            ("floor-div-zero", r#"emit(1 // 0)"#, "division by zero"),
            ("mod-zero", r#"emit(1 % 0)"#, "modulo by zero"),
            (
                "add-overflow",
                r#"emit(9223372036854775807 + 1)"#,
                "integer addition overflow",
            ),
            (
                "sub-overflow",
                r#"emit((0 - 9223372036854775807) - 2)"#,
                "integer subtraction overflow",
            ),
            (
                "mul-overflow",
                r#"emit(9223372036854775807 * 2)"#,
                "integer multiplication overflow",
            ),
            (
                "neg-overflow",
                r#"emit(-((0 - 9223372036854775807) - 1))"#,
                "integer negation overflow",
            ),
            (
                "floor-div-overflow",
                r#"emit(((0 - 9223372036854775807) - 1) // -1)"#,
                "integer floor division overflow",
            ),
            (
                "mod-overflow",
                r#"emit(((0 - 9223372036854775807) - 1) % -1)"#,
                "integer floor division overflow",
            ),
            ("bad-add", r#"emit("1" + 1)"#, "cannot add"),
            ("bad-sub", r#"emit("1" - 1)"#, "cannot subtract"),
            ("bad-mul", r#"emit("1" * 2)"#, "cannot multiply"),
            ("bad-div", r#"emit("1" / 2)"#, "cannot divide"),
            ("bad-floor-div", r#"emit("1" // 2)"#, "cannot divide"),
            ("bad-mod", r#"emit("1" % 2)"#, "cannot modulo"),
            ("bad-order", r#"emit("1" < 2)"#, "cannot order"),
            (
                "bad-membership",
                r#"emit(1 in 2)"#,
                "cannot test membership",
            ),
            (
                "negative-shift",
                r#"emit(1 << -1)"#,
                "shift count must be non-negative",
            ),
            (
                "oversized-shift",
                r#"emit(1 << 100)"#,
                "shift count is too large",
            ),
            ("bad-unary-minus", r#"emit(-"not-number")"#, "cannot negate"),
            ("bad-bitwise", r#"emit("1" & 1)"#, "expected integer"),
            ("bad-bitwise-invert", r#"emit(~"1")"#, "expected integer"),
        ];

        for (name, source, expected) in cases {
            let (engine_state, mut stack, root) = test_engine(&format!("expr-edge-{name}"))?;
            let program = lower_source(source)?;
            let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
                .expect_err(name);
            let text = format!("{error:?}");
            assert!(text.contains(expected), "{name}: {text}");
            cleanup_dir(&root);
        }

        Ok(())
    }

    #[test]
    fn unsupported_expression_ops_in_hot_loop_match_baseline_by_fallback() -> Result<(), ShellError>
    {
        assert_hot_loop_matches_baseline(
            "hot-loop-fallback-div-mod-neg",
            r#"total = 0.0
for n in [2, 4, 6]:
    total = total + (n / 2)
    total = total + (n % 4)
    total = total + -1
emit(total)
"#,
            &[],
        )?;

        assert_hot_loop_matches_baseline(
            "hot-loop-fallback-comparison-bool",
            r#"seen = []
for n in [1, 2, 3, 4]:
    if (n > 1 and n <= 3) or n == 4:
        seen.append(n)
emit(seen)
"#,
            &[],
        )?;

        let source = r#"total = 0.0
for n in [2, 4, 6]:
    total = total + (n / 2)
emit(total)
"#;
        let (engine_state, mut stack, root) = test_engine("hot-loop-fallback-div-diagnostics")?;
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
            json_value!(6.0)
        );
        assert_eq!(
            output.diagnostics["hot_loop"]["generic_vm_loops_executed"],
            json_value!(0)
        );

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn negative_semantic_tests_fail_instead_of_returning_wrong_values() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("negative-semantics")?;
        let missing_key = lower_source(r#"emit({"a": 1}["b"])"#)?;
        let error = eval_program(
            &engine_state,
            &mut stack,
            &missing_key,
            PipelineData::empty(),
        )
        .expect_err("missing key should fail");
        let text = format!("{error:?}");
        assert!(
            text.contains("no key") || text.contains("cannot find column"),
            "{text}"
        );

        let bad_type = lower_source(
            r#"total = 0
for value in ["not-int"]:
    total += int(value)
emit(total)
"#,
        )?;
        let error = eval_program_hot_loop(&engine_state, &mut stack, &bad_type)
            .expect_err("bad int conversion should fail");
        let text = format!("{error:?}");
        assert!(
            text.contains("invalid digit") || text.contains("int"),
            "{text}"
        );

        for (name, source, expected) in [
            (
                "bad-augassign-sub",
                r#"value = "1"
value -= 1
"#,
                "cannot subtract",
            ),
            (
                "bad-augassign-bitwise",
                r#"value = "1"
value |= 1
"#,
                "expected integer",
            ),
            (
                "bad-augassign-zero-div",
                r#"value = 1
value /= 0
"#,
                "division by zero",
            ),
            (
                "bad-augassign-zero-mod",
                r#"value = 1
value %= 0
"#,
                "modulo by zero",
            ),
            (
                "bad-augassign-overflow",
                r#"value = 9223372036854775807
value += 1
"#,
                "integer addition overflow",
            ),
            ("bad-any-non-iterable", r#"any(1)"#, "cannot iterate int"),
            (
                "bad-default-type",
                r#"def f(value: int = "bad") -> int:
    return value
f()
"#,
                "argument `value` expected int",
            ),
        ] {
            let program = lower_source(source)?;
            let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
                .expect_err(name);
            let text = format!("{error:?}");
            assert!(text.contains(expected), "{name}: {text}");
        }

        cleanup_dir(&root);
        Ok(())
    }

    #[test]
    fn fallback_tests_match_baseline_without_partial_mutation() -> Result<(), ShellError> {
        let source = r#"total = "seed"
for part in ["a", "b", "c"]:
    total += part
emit(total)
"#;
        assert_hot_loop_matches_baseline("fallback-string-add-no-partial-mutation", source, &[])?;

        let (engine_state, mut stack, root) = test_engine("fallback-no-partial-diagnostics")?;
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
            json_value!("seedabc")
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
    fn expression_vm_matches_baseline_for_integer_arithmetic_and_bitwise() -> Result<(), ShellError>
    {
        assert_hot_loop_matches_baseline(
            "expression-vm-arithmetic-bitwise",
            r#"total = 0
mask = 0
for n in [1, 2, 3]:
    total += n * 2
    total = total + (-n % 4)
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
        assert_hot_loop_matches_baseline(
            "llm-compat-fallback-ternary-and-augassign",
            r#"total = 0
mask = 0
for n in [1, 2, 3]:
    total += n if n > 1 else 10
    total -= 1
    mask |= n
emit({"total": total, "mask": mask})
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
    fn evaluates_must_run_builtin_checks_process_result() -> Result<(), ShellError> {
        let (engine_state, mut stack, _root) = test_engine("must-run-builtin")?;
        let program = lower_source(
            r#"ok = must_run(["sh", "-c", "printf hello"])
emit({"ok": ok.ok, "stdout": ok.stdout, "kind": ok.kind})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(true));
        assert_eq!(value["stdout"], json_value!("hello"));
        assert_eq!(value["kind"], json_value!("success"));

        let program = lower_source(r#"must_run(["sh", "-c", "printf nope >&2; exit 7"])"#)?;
        let error = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())
            .expect_err("must_run should raise on nonzero process exit");
        let text = format!("{error:?}");
        assert!(text.contains("stone_must_run_failed"), "{text}");
        assert!(text.contains("exit_code=7"), "{text}");
        assert!(text.contains("nope"), "{text}");
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
named_none = run(["pwd"], cwd=None, stdin=None, timeout_ms=None, env=None)
positional_none = run(["sh", "-c", "printf none"], None, None, None)
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
    "named_none": named_none["stdout"],
    "positional_none": positional_none["stdout"],
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
        assert_eq!(
            value["named_none"],
            json_value!(format!("{}\n", root.display()))
        );
        assert_eq!(value["positional_none"], json_value!("none"));
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
        let source = r#"daemon = start_daemon(
    ["sh", "-c", "while true; do sleep 1; done"],
    stdout="daemon.out",
    stderr="daemon.err",
)
status = daemon_status(daemon, log="daemon.err")
closed = wait_port(9, timeout_ms=20)
udp = wait_port(9, protocol="udp", timeout_ms=20)
stopped = stop_daemon(daemon, timeout_ms=1000)
after = daemon_status(daemon)
emit({
    "started": daemon["ok"],
    "pid_positive": daemon["pid"] > 0,
    "running": status["running"],
    "status_ok": status["ok"],
    "closed_ok": closed["ok"],
    "closed_kind": closed["kind"],
    "closed_protocol": closed["protocol"],
    "udp_protocol": udp["protocol"],
    "stopped": stopped["ok"],
    "after_running": after["running"],
})
"#
        .to_owned();
        let program = lower_source(&source)?;
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
                "closed_protocol": "tcp",
                "udp_protocol": "udp",
                "stopped": true,
                "after_running": false,
            })
        );
        cleanup_dir(&root);
        Ok(())
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn evaluates_posix_system_builtins() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("posix-system-builtins")?;
        let program = lower_source(
            r#"info = sysinfo("os")
mem = sysinfo("mem")
cpu = sys("cpu")
procs = ps()
emit({
    "os": info["os"],
    "arch_is_string": type(info["arch"]) == "str",
    "cpu_count_positive": info["cpu_count"] > 0,
    "mem_has_total": "total" in mem,
    "cpu_is_list": type(cpu) == "list",
    "has_self": len(where(procs, lambda p: p["pid"] == info["current_pid"])) > 0,
    "process_fields": len(procs) == 0 or ("command" in procs[0] and "memory_bytes" in procs[0]),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["os"], json_value!(std::env::consts::OS));
        assert_eq!(value["arch_is_string"], json_value!(true));
        assert_eq!(value["cpu_count_positive"], json_value!(true));
        assert_eq!(value["mem_has_total"], json_value!(true));
        assert_eq!(value["cpu_is_list"], json_value!(true));
        assert_eq!(value["has_self"], json_value!(true));
        assert_eq!(value["process_fields"], json_value!(true));

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
    fn file_helpers_accept_common_agent_aliases() -> Result<(), ShellError> {
        let (engine_state, mut stack, root) = test_engine("file-helper-aliases")?;
        fs::create_dir_all(root.join("pkg/sub")).expect("create nested dirs");
        fs::write(root.join("pkg/main.py"), "alpha\nneedle\nomega\n").expect("write main");
        fs::write(root.join("pkg/sub/deep.py"), "needle\n").expect("write deep");
        fs::write(root.join("a.tmp"), "").expect("write a");
        fs::write(root.join("b.tmp"), "").expect("write b");

        let program = lower_source(
            r#"named = find(path=".", name="main.py")
shallow = find(path=".", glob="**/*.py", max_depth=2)
lines = read_text(path="pkg/main.py", start_line=2, end_line=2)
matches = search(path=".", query="needle")
rm(paths=["a.tmp", "b.tmp", "missing.tmp"], force=True)
emit({
    "named": [file["name"] for file in named],
    "shallow": [file["name"] for file in shallow],
    "lines": lines,
    "matches": len(matches),
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;

        assert_eq!(
            json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?,
            json_value!({
                "named": ["main.py"],
                "shallow": ["main.py"],
                "lines": "needle\n",
                "matches": 2,
            })
        );
        assert!(!root.join("a.tmp").exists());
        assert!(!root.join("b.tmp").exists());

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
pretty = json_dumps({"ok": True}, indent=2)
compact = json_dumps({"ok": True}, separators=(",", ":"))
decoded = json_loads(encoded)
rows = [{"name": "a"}, {"name": "b"}]
json_bytes = write_json("out.json", decoded)
jsonl_bytes = write_jsonl("rows.jsonl", rows)
emit({
    "decoded": decoded,
    "pretty": pretty,
    "compact": compact,
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
                "pretty": "{\n  \"ok\": true\n}",
                "compact": "{\"ok\":true}",
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
    "left_trimmed": "  svc  ".lstrip(),
    "right_trimmed": "  svc  ".rstrip(),
    "left_chars": "xyxsvc".lstrip("xy"),
    "right_chars": "svcxyx".rstrip("xy"),
    "digits": "12345".isdigit(),
    "empty_digits": "".isdigit(),
    "mixed_digits": "12a".isdigit(),
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
                "left_trimmed": "svc  ",
                "right_trimmed": "  svc",
                "left_chars": "svc",
                "right_chars": "svc",
                "digits": true,
                "empty_digits": false,
                "mixed_digits": false,
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

        let engine_state = EngineState::new();

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

    fn stone_int_list_literal(values: &[i64]) -> String {
        let values = values
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{values}]")
    }

    fn stone_str_list_literal(values: &[&str]) -> String {
        let values = values
            .iter()
            .map(|value| format!("{value:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{values}]")
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
