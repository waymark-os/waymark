// SPDX-License-Identifier: MIT OR Apache-2.0

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::ops::Range;
#[cfg(all(not(target_os = "hermit"), unix))]
use std::os::unix::fs::PermissionsExt;
#[cfg(all(not(target_os = "hermit"), unix))]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
#[cfg(not(target_os = "hermit"))]
use std::process::{Command, ExitStatus, Stdio};
#[cfg(not(target_os = "hermit"))]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
#[cfg(not(target_os = "hermit"))]
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use nu_protocol::{
    engine::{EngineState, Stack},
    shell_error::generic::GenericError,
    IntoPipelineData, PipelineData, Record, ShellError, Span, Value,
};
use regex::bytes::Regex;
use serde_json::{json, Value as JsonValue};

use crate::commands::{stone_help_overview, stone_help_topic};
use crate::json::{json_to_nu_value, nu_to_json_value};
use crate::stone_ast::{
    AssignTarget, AugOp, BoolOp, Call, CompareOp, Expr, FormattedStringPart, FunctionDef, Program,
    Stmt, StoneFormatSpec, StoneType,
};
use crate::stone_ir::{
    compile_generic_vm_function, compile_hot_jsonl_loop_ir_function, compile_hot_jsonl_trace_plan,
    compile_hot_jsonl_trace_plan_from_ir, compile_hot_jsonl_vm_function,
    generic_loop_compile_miss_reason, match_hot_jsonl_aggregation_body,
    match_outer_jsonl_file_loop_body, optimize_loop_ir, optimize_stone_loop_ir,
    try_lower_generic_loop, try_lower_hot_loop, validate_hot_jsonl_native_prefix, AccId, ConstId,
    GenericLoopIter, GenericLoopOp, GenericLoopPlan, GenericParseNumber, GenericVmConst,
    GenericVmExprBody, GenericVmExprOp, GenericVmFunction, GenericVmOp, HotJsonlAggregationBody,
    HotJsonlBodyOp, HotJsonlSlot, HotJsonlTracePlan, HotLoopIter, HotLoopOp, HotLoopPlan,
    LoopIrFunction, LoopIrFusedKernel, LoopIrOptimizationDiagnostic, LoopIrOptimizationResult, Reg,
    SnapshotId, StoneAccumulatorKind, StoneConst, StoneFallbackTarget, StoneGuardKind,
    StoneIrFunction, StoneLoopIrOptimizationResult, StoneOp, StoneTerminator,
};

const STONE_MAX_FIND_ENTRIES: usize = 4096;
const STONE_MAX_SEARCH_FILES: usize = 1024;
const STONE_MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
const STONE_MAX_SEARCH_MATCHES: usize = 1000;
#[cfg(not(target_os = "hermit"))]
const STONE_HELPER_OUTPUT_LIMIT: usize = 4000;
const STONE_LAST_RESULT_ENV: &str = "WAYMARK_LAST_RESULT_JSON";

#[cfg(all(not(target_os = "hermit"), unix))]
extern "C" {
    fn setsid() -> i32;
}

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

#[derive(Default)]
pub(crate) struct StoneSession {
    locals: HashMap<String, RuntimeValue>,
    functions: HashMap<String, FunctionDef>,
}

impl StoneSession {
    fn root_scope(&self) -> Scope {
        Scope {
            locals: self.locals.clone(),
            files: HashMap::new(),
        }
    }

    fn update_from_root_scope(&mut self, scope: &Scope) {
        self.locals = scope
            .locals
            .iter()
            .filter(|(_, value)| value.is_session_persistable())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }

    fn update_functions(&mut self, functions: &HashMap<String, FunctionDef>) {
        self.functions = functions.clone();
    }
}

#[derive(Clone)]
enum RuntimeValue {
    Nu(Value),
    File(FileHandle),
    TextLines(TextLines),
    JsonlRows(JsonlRows),
    JsonObjectView(JsonObjectView),
    JsonArrayView(JsonArrayView),
    JsonScalarView(JsonScalarView),
    Callable(CallableValue),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeValueTag {
    Nu,
    File,
    TextLines,
    JsonlRows,
    JsonObjectView,
    JsonArrayView,
    JsonScalarView,
    Callable,
}

impl RuntimeValueTag {
    #[allow(dead_code)]
    fn id(self) -> u8 {
        match self {
            RuntimeValueTag::Nu => 1,
            RuntimeValueTag::File => 2,
            RuntimeValueTag::TextLines => 3,
            RuntimeValueTag::JsonlRows => 4,
            RuntimeValueTag::JsonObjectView => 5,
            RuntimeValueTag::JsonArrayView => 6,
            RuntimeValueTag::JsonScalarView => 7,
            RuntimeValueTag::Callable => 8,
        }
    }
}

#[derive(Clone)]
struct CallableValue {
    function_id: u64,
    params: Vec<String>,
    body: Box<Expr>,
    captures: Vec<(String, RuntimeValue)>,
}

#[derive(Clone, Copy)]
struct FileHandle {
    scope_index: usize,
    file_id: u64,
}

enum RuntimeFile {
    Read { text: String, closed: bool },
    Write { path: PathBuf, file: Option<File> },
}

#[derive(Clone)]
struct TextLines {
    lines: Vec<String>,
    source: String,
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

#[derive(Clone, Copy)]
enum GenericVmNumber {
    I64(i64),
    F64(f64),
}

enum GenericVmLoopResult {
    Executed { last_value: Option<RuntimeValue> },
    Unsupported,
}

enum GenericVmInput<'a> {
    Values(&'a [RuntimeValue]),
    TextLines(&'a TextLines),
}

impl LoopIrFunction {
    fn local_name(&self, local: usize) -> Result<&str, ShellError> {
        self.locals
            .get(local)
            .map(String::as_str)
            .ok_or_else(|| stone_error("hot loop", "generic VM local is out of range"))
    }
}

enum StoneVmExecutionResult {
    Completed,
    Fallback { snapshot: SnapshotId },
}

#[derive(Clone)]
struct JsonlRows {
    bytes: Arc<[u8]>,
    lines: Arc<[JsonLineRange]>,
    source: Arc<str>,
}

#[derive(Clone)]
struct JsonLineRange {
    range: Range<usize>,
    line_number: usize,
}

#[derive(Clone)]
struct JsonObjectView {
    bytes: Arc<[u8]>,
    range: Range<usize>,
    source: Arc<str>,
    line_number: usize,
}

#[derive(Clone)]
struct JsonArrayView {
    bytes: Arc<[u8]>,
    range: Range<usize>,
}

#[derive(Clone)]
struct JsonScalarView {
    bytes: Arc<[u8]>,
    range: Range<usize>,
    source: Arc<str>,
    line_number: usize,
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

impl RuntimeValue {
    fn is_session_persistable(&self) -> bool {
        match self {
            RuntimeValue::Nu(_) => true,
            RuntimeValue::TextLines(_) => true,
            RuntimeValue::JsonlRows(_) => true,
            RuntimeValue::JsonObjectView(_) => true,
            RuntimeValue::JsonArrayView(_) => true,
            RuntimeValue::JsonScalarView(_) => true,
            RuntimeValue::Callable(callable) => callable
                .captures
                .iter()
                .all(|(_, value)| value.is_session_persistable()),
            RuntimeValue::File(_) => false,
        }
    }

    #[allow(dead_code)]
    fn type_tag(&self) -> RuntimeValueTag {
        match self {
            RuntimeValue::Nu(_) => RuntimeValueTag::Nu,
            RuntimeValue::File(_) => RuntimeValueTag::File,
            RuntimeValue::TextLines(_) => RuntimeValueTag::TextLines,
            RuntimeValue::JsonlRows(_) => RuntimeValueTag::JsonlRows,
            RuntimeValue::JsonObjectView(_) => RuntimeValueTag::JsonObjectView,
            RuntimeValue::JsonArrayView(_) => RuntimeValueTag::JsonArrayView,
            RuntimeValue::JsonScalarView(_) => RuntimeValueTag::JsonScalarView,
            RuntimeValue::Callable(_) => RuntimeValueTag::Callable,
        }
    }

    fn into_nu_value(self, context: &str) -> Result<Value, ShellError> {
        match self {
            RuntimeValue::Nu(value) => Ok(value),
            RuntimeValue::File(_) => Err(stone_error(
                context,
                "file objects are task-owned runtime values and cannot cross this boundary",
            )),
            RuntimeValue::TextLines(lines) => Ok(Value::list(
                lines
                    .lines
                    .into_iter()
                    .map(|line| Value::string(line, Span::unknown()))
                    .collect(),
                Span::unknown(),
            )),
            RuntimeValue::JsonlRows(rows) => materialize_jsonl_rows(&rows),
            RuntimeValue::JsonObjectView(view) => materialize_json_object_view(&view),
            RuntimeValue::JsonArrayView(view) => materialize_json_array_view(&view),
            RuntimeValue::JsonScalarView(view) => materialize_json_scalar_view(&view),
            RuntimeValue::Callable(callable) => Err(stone_error(
                context,
                format!(
                    "callable lambda#{} is a task-owned runtime value and cannot cross this boundary",
                    callable.function_id
                ),
            )),
        }
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
                let value = eval_aug_assign(&left, *op, &right)?;
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
        match (op, input) {
            (GenericVmOp::AddAssign { local }, GenericVmInput::Values(values)) => {
                self.execute_generic_vm_add_assign(function.local_name(*local)?, values)
            }
            (GenericVmOp::AddAssignParsed { local, parse }, GenericVmInput::TextLines(lines))
                if function.iter == GenericLoopIter::OpenSplitlines =>
            {
                self.execute_generic_vm_text_parse_add_assign(
                    function.local_name(*local)?,
                    lines,
                    *parse,
                )
            }
            (GenericVmOp::MapAddI64Const { map, addend }, GenericVmInput::Values(values)) => self
                .execute_generic_vm_map_add_i64_const(function.local_name(*map)?, *addend, values),
            (
                GenericVmOp::MapAddI64ConstRecordField { map, field, addend },
                GenericVmInput::Values(values),
            ) => self.execute_generic_vm_map_add_i64_const_record_field(
                function.local_name(*map)?,
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
                function.local_name(*map)?,
                field,
                *strip,
                *lower,
                *addend,
                values,
            ),
            (GenericVmOp::ListAppend { list, unique }, GenericVmInput::Values(values)) => {
                self.execute_generic_vm_list_append(function.local_name(*list)?, values, *unique)
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
        for value in values {
            let Some(number) = generic_vm_number_from_runtime(value) else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_expr");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            accumulator = generic_vm_add_number(accumulator, number)?;
            last_value = Some(value.clone());
        }
        self.state.set_local(
            local.to_owned(),
            RuntimeValue::Nu(generic_vm_number_to_value(accumulator)),
        );
        Ok(GenericVmLoopResult::Executed { last_value })
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

        let mut last_value = None;
        let mut registers = vec![None; body.registers];
        for value in values {
            let RuntimeValue::Nu(value) = value else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_value_type");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            let Value::Int { val, .. } = value else {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_value_type");
                return Ok(GenericVmLoopResult::Unsupported);
            };
            if locals.is_empty() {
                self.state
                    .hot_loop_diagnostics
                    .lowering_miss("unsupported_body_stmt");
                return Ok(GenericVmLoopResult::Unsupported);
            }
            locals[0] = Some(*val);
            registers.fill(None);
            if !self.execute_generic_vm_expr_ops(body, &mut locals, &mut registers)? {
                return Ok(GenericVmLoopResult::Unsupported);
            }
            last_value = Some(RuntimeValue::Nu(value.clone()));
        }

        for (name, value) in function.locals.iter().zip(locals.into_iter()) {
            if let Some(value) = value {
                self.state.set_local(
                    name.clone(),
                    RuntimeValue::Nu(Value::int(value, Span::unknown())),
                );
            }
        }

        Ok(GenericVmLoopResult::Executed { last_value })
    }

    fn execute_generic_vm_expr_ops(
        &mut self,
        body: &GenericVmExprBody,
        locals: &mut [Option<i64>],
        registers: &mut [Option<i64>],
    ) -> Result<bool, ShellError> {
        for op in &body.ops {
            match *op {
                GenericVmExprOp::LoadLocal { dst, local } => {
                    let Some(value) = locals.get(local).copied().flatten() else {
                        self.state
                            .hot_loop_diagnostics
                            .lowering_miss("unsupported_expr_name");
                        return Ok(false);
                    };
                    let Some(slot) = registers.get_mut(dst) else {
                        return Err(stone_error("hot loop", "VM register is out of range"));
                    };
                    *slot = Some(value);
                }
                GenericVmExprOp::StoreLocal { local, src } => {
                    let value = self.generic_vm_register_i64(registers, src)?;
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
                    self.generic_vm_store_i64_binop(
                        registers,
                        dst,
                        lhs,
                        rhs,
                        "addition",
                        i64::checked_add,
                    )?;
                }
                GenericVmExprOp::SubI64 { dst, lhs, rhs } => {
                    self.generic_vm_store_i64_binop(
                        registers,
                        dst,
                        lhs,
                        rhs,
                        "subtraction",
                        i64::checked_sub,
                    )?;
                }
                GenericVmExprOp::MulI64 { dst, lhs, rhs } => {
                    self.generic_vm_store_i64_binop(
                        registers,
                        dst,
                        lhs,
                        rhs,
                        "multiplication",
                        i64::checked_mul,
                    )?;
                }
                GenericVmExprOp::FloorDivI64 { dst, lhs, rhs } => {
                    let left = self.generic_vm_register_i64(registers, lhs)?;
                    let right = self.generic_vm_register_i64(registers, rhs)?;
                    if right == 0 {
                        return Err(stone_error("floor division", "division by zero"));
                    }
                    self.generic_vm_set_register(registers, dst, left.div_euclid(right))?;
                }
                GenericVmExprOp::BitAndI64 { dst, lhs, rhs } => {
                    let value = self.generic_vm_register_i64(registers, lhs)?
                        & self.generic_vm_register_i64(registers, rhs)?;
                    self.generic_vm_set_register(registers, dst, value)?;
                }
                GenericVmExprOp::BitOrI64 { dst, lhs, rhs } => {
                    let value = self.generic_vm_register_i64(registers, lhs)?
                        | self.generic_vm_register_i64(registers, rhs)?;
                    self.generic_vm_set_register(registers, dst, value)?;
                }
                GenericVmExprOp::BitXorI64 { dst, lhs, rhs } => {
                    let value = self.generic_vm_register_i64(registers, lhs)?
                        ^ self.generic_vm_register_i64(registers, rhs)?;
                    self.generic_vm_set_register(registers, dst, value)?;
                }
                GenericVmExprOp::ShlI64 { dst, lhs, rhs } => {
                    self.generic_vm_store_shift(
                        registers,
                        dst,
                        lhs,
                        rhs,
                        "left shift",
                        i64::checked_shl,
                    )?;
                }
                GenericVmExprOp::ShrI64 { dst, lhs, rhs } => {
                    self.generic_vm_store_shift(
                        registers,
                        dst,
                        lhs,
                        rhs,
                        "right shift",
                        i64::checked_shr,
                    )?;
                }
                GenericVmExprOp::BitNotI64 { dst, src } => {
                    let value = !self.generic_vm_register_i64(registers, src)?;
                    self.generic_vm_set_register(registers, dst, value)?;
                }
            }
        }
        Ok(true)
    }

    fn generic_vm_register_i64(
        &self,
        registers: &[Option<i64>],
        reg: usize,
    ) -> Result<i64, ShellError> {
        registers
            .get(reg)
            .copied()
            .flatten()
            .ok_or_else(|| stone_error("hot loop", "VM register is unset"))
    }

    fn generic_vm_set_register(
        &self,
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

    fn generic_vm_store_i64_binop(
        &self,
        registers: &mut [Option<i64>],
        dst: usize,
        lhs: usize,
        rhs: usize,
        context: &str,
        op: impl FnOnce(i64, i64) -> Option<i64>,
    ) -> Result<(), ShellError> {
        let left = self.generic_vm_register_i64(registers, lhs)?;
        let right = self.generic_vm_register_i64(registers, rhs)?;
        let value = op(left, right)
            .ok_or_else(|| stone_error(context, format!("integer {context} overflow")))?;
        self.generic_vm_set_register(registers, dst, value)
    }

    fn generic_vm_store_shift(
        &self,
        registers: &mut [Option<i64>],
        dst: usize,
        lhs: usize,
        rhs: usize,
        context: &str,
        op: impl FnOnce(i64, u32) -> Option<i64>,
    ) -> Result<(), ShellError> {
        let left = self.generic_vm_register_i64(registers, lhs)?;
        let right = self.generic_vm_register_i64(registers, rhs)?;
        let shift = u32::try_from(right)
            .map_err(|_| stone_error(context, "shift count must be non-negative"))?;
        let value =
            op(left, shift).ok_or_else(|| stone_error(context, "shift count is too large"))?;
        self.generic_vm_set_register(registers, dst, value)
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
        nested: &crate::stone_ir::HotJsonlNestedUserTotals,
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
        nested: &crate::stone_ir::HotJsonlNestedUserTotals,
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
            *slot = eval_add(slot, &addend)?;
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
                    value = RuntimeValue::Nu(eval_subscript(&target, &index)?);
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
                                    eval_slice(&value, lower, upper).map(RuntimeValue::Nu)
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
                    eval_neg(&value).map(RuntimeValue::Nu)
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
                    eval_add(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::Sub { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("subtraction")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("subtraction")?;
                    eval_sub(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::Mul { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("multiplication")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("multiplication")?;
                    eval_mul(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::Div { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("division")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("division")?;
                    eval_div(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::FloorDiv { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("floor division")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("floor division")?;
                    eval_floor_div(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::Mod { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("modulo")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("modulo")?;
                    eval_mod(&left, &right).map(RuntimeValue::Nu)
                }
                Expr::BitAnd { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("bitwise and")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("bitwise and")?;
                    eval_bitwise_int(&left, &right, "bitwise and", |left, right| left & right)
                        .map(RuntimeValue::Nu)
                }
                Expr::BitOr { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("bitwise or")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("bitwise or")?;
                    eval_bitwise_int(&left, &right, "bitwise or", |left, right| left | right)
                        .map(RuntimeValue::Nu)
                }
                Expr::BitXor { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("bitwise xor")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("bitwise xor")?;
                    eval_bitwise_int(&left, &right, "bitwise xor", |left, right| left ^ right)
                        .map(RuntimeValue::Nu)
                }
                Expr::LShift { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("left shift")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("left shift")?;
                    eval_shift(&left, &right, "left shift", i64::checked_shl).map(RuntimeValue::Nu)
                }
                Expr::RShift { left, right } => {
                    let left = self
                        .eval_expr_value(left, PipelineData::empty())?
                        .into_nu_value("right shift")?;
                    let right = self
                        .eval_expr_value(right, PipelineData::empty())?
                        .into_nu_value("right shift")?;
                    eval_shift(&left, &right, "right shift", i64::checked_shr).map(RuntimeValue::Nu)
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
                value_to_int(&value).map(RuntimeValue::Nu)
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
                value_to_f64(&value, "float")
                    .map(|value| RuntimeValue::Nu(Value::float(value, Span::unknown())))
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
                value_len(&value).map(|len| RuntimeValue::Nu(Value::int(len, Span::unknown())))
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
                Ok(RuntimeValue::Nu(Value::string(
                    value_to_display_string(&value)?,
                    Span::unknown(),
                )))
            }
            "type" => self.eval_type_call(call),
            "enumerate" => self.eval_enumerate_call(call),
            "echo" => self.eval_echo_call(call),
            "emit" => self.eval_emit_call(call, input),
            "fail" => self.eval_fail_call(call),
            "find" => self.eval_find_call(call),
            "format" => self.eval_format_call(call),
            "json_dumps" => self.eval_json_dumps_call(call),
            "json_loads" => self.eval_json_loads_call(call),
            "help" => self.eval_help_call(call),
            "max" => self.eval_min_max_call(call, MinMax::Max),
            "min" => self.eval_min_max_call(call, MinMax::Min),
            "open" => self.eval_open_call(call),
            "pwd" => self.eval_pwd_call(call),
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

        let runtime_file = match mode.as_str() {
            "r" | "rt" => {
                let mut file =
                    File::open(&target).map_err(|err| io_read_stone_error("open", err, &target))?;
                let mut text = String::new();
                file.read_to_string(&mut text)
                    .map_err(|err| io_stone_error("open", err, &target))?;
                RuntimeFile::Read {
                    text,
                    closed: false,
                }
            }
            "w" | "wt" => RuntimeFile::Write {
                path: target.clone(),
                file: Some({
                    ensure_parent_dir_for_write("open", &target)?;
                    OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(true)
                        .open(&target)
                        .map_err(|err| io_stone_error("open", err, &target))?
                }),
            },
            "a" | "at" => RuntimeFile::Write {
                path: target.clone(),
                file: Some({
                    ensure_parent_dir_for_write("open", &target)?;
                    OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&target)
                        .map_err(|err| io_stone_error("open", err, &target))?
                }),
            },
            other => {
                return Err(stone_error(
                    "open",
                    format!("unsupported mode `{other}`; expected r, w, or a"),
                ));
            }
        };

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
        let text =
            fs::read_to_string(&target).map_err(|err| io_read_stone_error("cat", err, &target))?;
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
        let text = stone_file_adapter().read_text(&target, max_bytes)?;
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
        let written = stone_file_adapter().write_text(&target, &text, append)?;
        Ok(RuntimeValue::Nu(file_write_record(
            written,
            Span::unknown(),
        )))
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
        let stat = stone_file_adapter().stat(&target, follow_symlinks)?;
        Ok(RuntimeValue::Nu(file_stat_record(stat, Span::unknown())))
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
        let mut entries = stone_file_adapter()
            .list_dir(&target)?
            .into_iter()
            .map(|entry| file_entry_record(entry, Span::unknown()))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            left.get_data_by_key("name")
                .and_then(|value| value.coerce_string().ok())
                .cmp(
                    &right
                        .get_data_by_key("name")
                        .and_then(|value| value.coerce_string().ok()),
                )
        });
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

        let mut name_glob = rest
            .first()
            .map(|expr| {
                self.eval_expr_value(expr, PipelineData::empty())
                    .and_then(|value| value.into_nu_value("find"))
                    .and_then(|value| value_to_string(&value, "find"))
            })
            .transpose()?;
        let mut name_contains: Option<String> = None;
        for (name, argument) in &call.named {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value("find")?;
            match name.as_str() {
                "name_glob" => name_glob = Some(value_to_string(&value, "find name_glob")?),
                "name_contains" => {
                    name_contains = Some(value_to_string(&value, "find name_contains")?)
                }
                other => {
                    return Err(stone_error(
                        "find",
                        format!(
                            "unsupported keyword `{other}`; expected name_glob or name_contains"
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
        let mut entries = Vec::new();
        let mut queue = VecDeque::from([root]);

        while let Some(path) = queue.pop_front() {
            if entries.len() >= STONE_MAX_FIND_ENTRIES {
                break;
            }
            let stat = stone_file_adapter().stat(&path, false)?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            if stone_name_matches(&name, name_contains.as_deref(), name_glob.as_deref()) {
                entries.push(file_entry_record(
                    StoneFileEntry {
                        name,
                        stat: stat.clone(),
                    },
                    Span::unknown(),
                ));
            }
            if stat.is_dir {
                for entry in stone_file_adapter().list_dir(&path)? {
                    queue.push_back(entry.stat.path);
                }
            }
        }

        entries.sort_by(|left, right| {
            left.get_data_by_key("path")
                .and_then(|value| value.coerce_string().ok())
                .cmp(
                    &right
                        .get_data_by_key("path")
                        .and_then(|value| value.coerce_string().ok()),
                )
        });
        Ok(RuntimeValue::Nu(Value::list(entries, Span::unknown())))
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
        let bytes =
            fs::read(&target).map_err(|err| io_read_stone_error("read_json", err, &target))?;
        let json = serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|err| stone_error("read_json", format!("{}: {}", target.display(), err)))?;
        Ok(RuntimeValue::Nu(json_to_nu_value(json, Span::unknown())))
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
        let text = fs::read_to_string(&target)
            .map_err(|err| io_read_stone_error("read_csv", err, &target))?;
        let rows = parse_csv_records(&text, limit)?;
        Ok(RuntimeValue::Nu(Value::list(rows, Span::unknown())))
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
        let bytes = fs::read(&target).map_err(|err| io_read_stone_error(context, err, &target))?;
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
        let json = nu_to_json_value(&value);
        let text = serde_json::to_string_pretty(&json)
            .map_err(|err| stone_error("write_json", err.to_string()))?
            + "\n";
        ensure_parent_dir_for_write("write_json", &target)?;
        fs::write(&target, text.as_bytes())
            .map_err(|err| io_stone_error("write_json", err, &target))?;
        Ok(RuntimeValue::Nu(Value::int(
            i64::try_from(text.len()).unwrap_or(i64::MAX),
            Span::unknown(),
        )))
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
        let mut text = String::new();
        for value in vals {
            let json = nu_to_json_value(&value);
            text.push_str(
                &serde_json::to_string(&json)
                    .map_err(|err| stone_error("write_jsonl", err.to_string()))?,
            );
            text.push('\n');
        }
        ensure_parent_dir_for_write("write_jsonl", &target)?;
        fs::write(&target, text.as_bytes())
            .map_err(|err| io_stone_error("write_jsonl", err, &target))?;
        Ok(RuntimeValue::Nu(Value::int(
            i64::try_from(text.len()).unwrap_or(i64::MAX),
            Span::unknown(),
        )))
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
        let Value::List { vals, .. } = values else {
            return Err(stone_error(
                name,
                format!("expected list, got {}", values.get_type()),
            ));
        };
        let count = match call.positional.get(1) {
            Some(count) => {
                let count = self
                    .eval_expr_value(count, PipelineData::empty())?
                    .into_nu_value(name)?;
                Some(value_to_limit(&count, name)?)
            }
            None => None,
        };
        Ok(RuntimeValue::Nu(match count {
            Some(count) => Value::list(vals.into_iter().take(count).collect(), Span::unknown()),
            None => vals
                .into_iter()
                .next()
                .unwrap_or_else(|| Value::nothing(Span::unknown())),
        }))
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
        let Value::List { vals, .. } = values else {
            return Err(stone_error(
                name,
                format!("expected list, got {}", values.get_type()),
            ));
        };
        let count = match call.positional.get(1) {
            Some(count) => {
                let count = self
                    .eval_expr_value(count, PipelineData::empty())?
                    .into_nu_value(name)?;
                Some(value_to_limit(&count, name)?)
            }
            None => None,
        };
        Ok(RuntimeValue::Nu(match count {
            Some(count) => {
                let start = vals.len().saturating_sub(count);
                Value::list(vals.into_iter().skip(start).collect(), Span::unknown())
            }
            None => vals
                .into_iter()
                .last()
                .unwrap_or_else(|| Value::nothing(Span::unknown())),
        }))
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
            fs::create_dir_all(&target).map_err(|err| io_stone_error("mkdir", err, &target))?;
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
            if target.is_dir() {
                fs::remove_dir_all(&target).map_err(|err| io_stone_error("rm", err, &target))?;
            } else {
                fs::remove_file(&target).map_err(|err| io_stone_error("rm", err, &target))?;
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
        if old.is_empty() {
            return Err(stone_error("edit", "old text must not be empty"));
        }
        let target = self.resolve_script_path(&path)?;
        let text =
            fs::read_to_string(&target).map_err(|err| io_read_stone_error("edit", err, &target))?;
        let matches = text.matches(&old).count();
        if matches == 0 {
            return Err(stone_error("edit", "old text was not found"));
        }
        let replaced = if replace_all {
            text.replace(&old, &new)
        } else {
            text.replacen(&old, &new, 1)
        };
        fs::write(&target, replaced.as_bytes())
            .map_err(|err| io_stone_error("edit", err, &target))?;
        let mut record = Record::with_capacity(4);
        record.push(
            "path",
            Value::string(target.display().to_string(), Span::unknown()),
        );
        record.push(
            "replacements",
            Value::int(
                if replace_all { matches as i64 } else { 1 },
                Span::unknown(),
            ),
        );
        record.push("matched", Value::int(matches as i64, Span::unknown()));
        record.push("all", Value::bool(replace_all, Span::unknown()));
        Ok(RuntimeValue::Nu(Value::record(record, Span::unknown())))
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
        if target.exists() && !append && !force {
            return Err(stone_error(
                "save",
                format!(
                    "{} already exists; pass force=True to overwrite",
                    target.display()
                ),
            ));
        }
        ensure_parent_dir_for_write("save", &target)?;
        let bytes = value_to_save_bytes(&value)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&target)
            .map_err(|err| io_stone_error("save", err, &target))?;
        file.write_all(&bytes)
            .map_err(|err| io_stone_error("save", err, &target))?;
        file.flush()
            .map_err(|err| io_stone_error("save", err, &target))?;
        let mut record = Record::with_capacity(3);
        record.push(
            "path",
            Value::string(target.display().to_string(), Span::unknown()),
        );
        record.push(
            "bytes",
            Value::int(
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                Span::unknown(),
            ),
        );
        record.push("append", Value::bool(append, Span::unknown()));
        Ok(RuntimeValue::Nu(Value::record(record, Span::unknown())))
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
        let matcher = StoneSearchMatcher::new(&needle, regex)?;
        let root = self.resolve_script_path(&root)?;
        let mut files_visited = 0usize;
        let mut matches = Vec::new();
        let mut queue = VecDeque::from([root]);
        while let Some(path) = queue.pop_front() {
            if files_visited >= STONE_MAX_SEARCH_FILES || matches.len() >= STONE_MAX_SEARCH_MATCHES
            {
                break;
            }
            let stat = stone_file_adapter().stat(&path, false)?;
            if stat.is_dir {
                for entry in stone_file_adapter().list_dir(&path)? {
                    queue.push_back(entry.stat.path);
                }
                continue;
            }
            if !stat.is_file || stat.size > STONE_MAX_SEARCH_FILE_BYTES {
                continue;
            }
            files_visited += 1;
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if stone_bytes_look_binary(&bytes) || !matcher.is_match(&bytes) {
                continue;
            }
            push_stone_search_line_matches(&mut matches, &path, &bytes, &matcher);
        }
        Ok(RuntimeValue::Nu(Value::list(matches, Span::unknown())))
    }

    fn eval_where_call(&mut self, call: &Call) -> Result<RuntimeValue, ShellError> {
        let [values, key, expected] = call.positional.as_slice() else {
            return Err(stone_error(
                "where",
                "where() requires rows, key, and expected arguments",
            ));
        };
        if !call.named.is_empty() {
            return Err(stone_error(
                "where",
                "where() keyword arguments are not supported",
            ));
        }
        let values = self
            .eval_expr_value(values, PipelineData::empty())?
            .into_nu_value("where")?;
        let key = self
            .eval_expr_value(key, PipelineData::empty())?
            .into_nu_value("where")?;
        let expected = self
            .eval_expr_value(expected, PipelineData::empty())?
            .into_nu_value("where")?;
        let key = value_to_string(&key, "where key")?;
        let Value::List { vals, .. } = values else {
            return Err(stone_error(
                "where",
                format!("expected list, got {}", values.get_type()),
            ));
        };
        let mut selected = Vec::new();
        for value in vals {
            if let Value::Record { val, .. } = &value {
                if val
                    .get(&key)
                    .is_some_and(|candidate| values_equal(candidate, &expected))
                {
                    selected.push(value);
                }
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

        let mut keyed = Vec::with_capacity(vals.len());
        let mut key_kind = None;
        for value in vals {
            let sort_key = match &key {
                SortKey::Callable(callable) => self
                    .invoke_callable(callable, vec![RuntimeValue::Nu(value.clone())])?
                    .into_nu_value("sort key")?,
                _ => extract_sort_key(&value, &key)?,
            };
            let next_key_kind = sort_key_kind(&sort_key)?;
            if let Some(key_kind) = key_kind {
                if key_kind != next_key_kind {
                    return Err(stone_error(
                        "sort",
                        "all sort keys must have compatible types",
                    ));
                }
            } else {
                key_kind = Some(next_key_kind);
            }
            keyed.push((sort_key, value));
        }
        keyed.sort_by(|(left_key, _), (right_key, _)| {
            let ordering = value_ordering(left_key, right_key)
                .expect("sort keys are validated before sorting");
            if reverse {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(RuntimeValue::Nu(Value::list(
            keyed.into_iter().map(|(_, value)| value).collect(),
            Span::unknown(),
        )))
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
        let mut seen = HashSet::new();
        let mut unique_values = Vec::new();
        for value in values {
            let value = value.into_nu_value("set")?;
            if seen.insert(value_identity_key(&value, "set")?) {
                unique_values.push(value);
            }
        }
        Ok(RuntimeValue::Nu(Value::list(
            unique_values,
            Span::unknown(),
        )))
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
        let Value::List { vals, .. } = values else {
            return Err(stone_error(
                "unique",
                format!("expected list, got {}", values.get_type()),
            ));
        };

        let mut seen = HashSet::new();
        let mut unique_values = Vec::new();
        for value in vals {
            let key = value_identity_key(&value, "unique")?;
            if seen.insert(key) {
                unique_values.push(value);
            }
        }
        Ok(RuntimeValue::Nu(Value::list(
            unique_values,
            Span::unknown(),
        )))
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
        let value = value_to_f64(&value, "round")?;
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
        if !(0..=9).contains(&digits) {
            return Err(stone_error(
                "round",
                "round() digits must be between 0 and 9",
            ));
        }
        let factor = 10_f64.powi(digits as i32);
        Ok(RuntimeValue::Nu(Value::float(
            (value * factor).round() / factor,
            Span::unknown(),
        )))
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
        match value_to_f64(&value, "parse_float") {
            Ok(value) => Ok(RuntimeValue::Nu(Value::float(value, Span::unknown()))),
            Err(_) => match default {
                Value::Float { .. } | Value::Int { .. } => Ok(RuntimeValue::Nu(default)),
                other => Err(stone_error(
                    "parse_float",
                    format!("default must be int or float, got {}", other.get_type()),
                )),
            },
        }
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
        match value_to_int(&value) {
            Ok(value) => Ok(RuntimeValue::Nu(value)),
            Err(_) => match default {
                Value::Int { .. } => Ok(RuntimeValue::Nu(default)),
                other => Err(stone_error(
                    "parse_int",
                    format!("default must be int, got {}", other.get_type()),
                )),
            },
        }
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
        eval_record_method(&record, &call.name, &args).map(RuntimeValue::Nu)
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
            if call.positional.is_empty() || call.positional.len() > 4 {
                return Err(stone_error(
                    "run",
                    "run() expects run(argv, cwd? = None, stdin? = None, timeout_ms? = None)",
                ));
            }
            let argv_expr = &call.positional[0];
            let argv_value = self
                .eval_expr_value(argv_expr, PipelineData::empty())?
                .into_nu_value("run")?;
            let argv = value_to_string_list(&argv_value, "run")?;
            if argv.is_empty() {
                return Err(stone_error("run", "run() argv list cannot be empty"));
            }

            let mut cwd: Option<String> = None;
            let mut stdin: Option<String> = None;
            let mut env_overrides: Vec<(String, String)> = Vec::new();
            let mut timeout_ms: i64 = 300_000;
            let mut max_stdout_bytes: usize = 1_048_576;
            let mut max_stderr_bytes: usize = 1_048_576;
            let mut stdout_target = RunOutputTarget::Capture;
            let mut stderr_target = RunOutputTarget::Capture;

            for (index, argument) in call.positional.iter().enumerate().skip(1) {
                let value = self
                    .eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value("run")?;
                match index {
                    1 => match value {
                        Value::Int { .. } | Value::Float { .. } => {
                            timeout_ms = value_to_i64(&value, "run timeout_ms")?;
                            if timeout_ms <= 0 {
                                return Err(stone_error("run", "timeout_ms must be positive"));
                            }
                        }
                        _ => cwd = Some(value_to_string(&value, "run cwd")?),
                    },
                    2 => match value {
                        Value::Int { .. } | Value::Float { .. } => {
                            timeout_ms = value_to_i64(&value, "run timeout_ms")?;
                            if timeout_ms <= 0 {
                                return Err(stone_error("run", "timeout_ms must be positive"));
                            }
                        }
                        _ => stdin = Some(value_to_string(&value, "run stdin")?),
                    },
                    3 => {
                        timeout_ms = value_to_i64(&value, "run timeout_ms")?;
                        if timeout_ms <= 0 {
                            return Err(stone_error("run", "timeout_ms must be positive"));
                        }
                    }
                    _ => unreachable!("run positional arity checked above"),
                }
            }

            for (name, argument) in &call.named {
                let value = self
                    .eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value("run")?;
                match name.as_str() {
                    "cwd" => cwd = Some(value_to_string(&value, "run cwd")?),
                    "stdin" => stdin = Some(value_to_string(&value, "run stdin")?),
                    "env" => env_overrides = value_to_string_pairs(&value, "run env")?,
                    "timeout_ms" => {
                        timeout_ms = value_to_i64(&value, "run timeout_ms")?;
                        if timeout_ms <= 0 {
                            return Err(stone_error("run", "timeout_ms must be positive"));
                        }
                    }
                    "max_stdout_bytes" => {
                        max_stdout_bytes = value_to_limit(&value, "run max_stdout_bytes")?;
                    }
                    "max_stderr_bytes" => {
                        max_stderr_bytes = value_to_limit(&value, "run max_stderr_bytes")?;
                    }
                    "stdout" => {
                        stdout_target = value_to_run_stdout_target(&value, "run stdout")?;
                    }
                    "stderr" => {
                        stderr_target = value_to_run_stderr_target(&value, "run stderr")?;
                    }
                    other => {
                        return Err(stone_error(
                            "run",
                            format!(
                                "unsupported keyword `{other}`; expected cwd, env, stdin, timeout_ms, max_stdout_bytes, max_stderr_bytes, stdout, or stderr"
                            ),
                        ));
                    }
                }
            }

            let cwd_path = match cwd {
                Some(path) => self.resolve_script_path(&path)?,
                None => self
                    .engine_state
                    .cwd_as_string(Some(self.stack))
                    .map(PathBuf::from)
                    .map_err(|err| stone_error("run", err.to_string()))?,
            };
            let mut record = run_posix_command(
                &argv,
                &cwd_path,
                &env_overrides,
                stdin.as_deref(),
                Duration::from_millis(timeout_ms as u64),
                stdout_target,
                stderr_target,
                max_stdout_bytes,
                max_stderr_bytes,
            )?;
            self.attach_run_helper_observations(
                &mut record,
                &argv,
                &cwd_path,
                &env_overrides,
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
            match hook.handler.invoke(self, &hook, &event, span) {
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

        self.state.push_scope();
        self.state.set_local(
            function.params[0].name.clone(),
            RuntimeValue::Nu(event_value),
        );
        let flow = self.eval_block(&function.body, PipelineData::empty(), false);
        self.state.pop_scope()?;
        let value = match flow? {
            EvalFlow::Return(value) => value,
            EvalFlow::Output(_) => Value::nothing(span),
            EvalFlow::Break => return Err(stone_error("break", "break outside loop")),
            EvalFlow::Continue => return Err(stone_error("continue", "continue outside loop")),
        };
        ensure_type(
            &value,
            function.return_type,
            &format!("{}() return value", function.name),
        )?;
        stone_helper_observations_from_value(value)
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
            let [name_expr] = call.positional.as_slice() else {
                return Err(stone_error(
                    "resolve_command",
                    "resolve_command() requires exactly one command name",
                ));
            };
            if !call.named.is_empty() {
                return Err(stone_error(
                    "resolve_command",
                    "resolve_command() keyword arguments are not supported",
                ));
            }
            let value = self
                .eval_expr_value(name_expr, PipelineData::empty())?
                .into_nu_value("resolve_command")?;
            let name = value_to_string(&value, "resolve_command")?;
            Ok(RuntimeValue::Nu(resolve_command_record(&name)))
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
            let [argv_expr] = call.positional.as_slice() else {
                return Err(stone_error(
                    "start_daemon",
                    "start_daemon() requires exactly one argv list",
                ));
            };
            let argv_value = self
                .eval_expr_value(argv_expr, PipelineData::empty())?
                .into_nu_value("start_daemon")?;
            let argv = value_to_string_list(&argv_value, "start_daemon")?;
            if argv.is_empty() {
                return Err(stone_error("start_daemon", "argv list cannot be empty"));
            }

            let mut cwd: Option<String> = None;
            let mut env_overrides: Vec<(String, String)> = Vec::new();
            let mut stdout: Option<String> = None;
            let mut stderr: Option<String> = None;

            for (name, argument) in &call.named {
                let value = self
                    .eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value("start_daemon")?;
                match name.as_str() {
                    "cwd" => cwd = Some(value_to_string(&value, "start_daemon cwd")?),
                    "env" => env_overrides = value_to_string_pairs(&value, "start_daemon env")?,
                    "stdout" => stdout = Some(value_to_string(&value, "start_daemon stdout")?),
                    "stderr" => stderr = Some(value_to_string(&value, "start_daemon stderr")?),
                    other => {
                        return Err(stone_error(
                            "start_daemon",
                            format!(
                                "unsupported keyword `{other}`; expected cwd, env, stdout, or stderr"
                            ),
                        ));
                    }
                }
            }

            let cwd_path = match cwd {
                Some(path) => self.resolve_script_path(&path)?,
                None => self
                    .engine_state
                    .cwd_as_string(Some(self.stack))
                    .map(PathBuf::from)
                    .map_err(|err| stone_error("start_daemon", err.to_string()))?,
            };
            let stdout_path = match stdout {
                Some(path) => self.resolve_script_path(&path)?,
                None => daemon_temp_path("stdout"),
            };
            let stderr_path = match stderr {
                Some(path) => self.resolve_script_path(&path)?,
                None => daemon_temp_path("stderr"),
            };

            let result =
                start_posix_daemon(&argv, &cwd_path, &env_overrides, &stdout_path, &stderr_path)?;
            Ok(RuntimeValue::Nu(result))
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
            let [daemon_expr] = call.positional.as_slice() else {
                return Err(stone_error(
                    "daemon_status",
                    "daemon_status() requires exactly one daemon handle or pid",
                ));
            };
            let daemon = self
                .eval_expr_value(daemon_expr, PipelineData::empty())?
                .into_nu_value("daemon_status")?;
            let pid = value_to_daemon_pid(&daemon, "daemon_status")?;
            let mut host = "127.0.0.1".to_owned();
            let mut port: Option<u16> = None;
            let mut log: Option<String> = None;
            let mut max_log_bytes: usize = 4000;

            for (name, argument) in &call.named {
                let value = self
                    .eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value("daemon_status")?;
                match name.as_str() {
                    "host" => host = value_to_string(&value, "daemon_status host")?,
                    "port" => port = Some(value_to_port(&value, "daemon_status port")?),
                    "log" => log = Some(value_to_string(&value, "daemon_status log")?),
                    "max_log_bytes" => {
                        max_log_bytes = value_to_limit(&value, "daemon_status max_log_bytes")?
                    }
                    other => {
                        return Err(stone_error(
                            "daemon_status",
                            format!(
                                "unsupported keyword `{other}`; expected host, port, log, or max_log_bytes"
                            ),
                        ));
                    }
                }
            }

            let log_path = match log {
                Some(path) => Some(self.resolve_script_path(&path)?),
                None => daemon_log_path(&daemon),
            };
            Ok(RuntimeValue::Nu(daemon_status_record(
                pid,
                port,
                &host,
                log_path.as_deref(),
                max_log_bytes,
            )))
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
            let [daemon_expr] = call.positional.as_slice() else {
                return Err(stone_error(
                    "stop_daemon",
                    "stop_daemon() requires exactly one daemon handle or pid",
                ));
            };
            let daemon = self
                .eval_expr_value(daemon_expr, PipelineData::empty())?
                .into_nu_value("stop_daemon")?;
            let pid = value_to_daemon_pid(&daemon, "stop_daemon")?;
            let mut timeout_ms: i64 = 5000;
            for (name, argument) in &call.named {
                let value = self
                    .eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value("stop_daemon")?;
                match name.as_str() {
                    "timeout_ms" => {
                        timeout_ms = value_to_i64(&value, "stop_daemon timeout_ms")?;
                        if timeout_ms <= 0 {
                            return Err(stone_error("stop_daemon", "timeout_ms must be positive"));
                        }
                    }
                    other => {
                        return Err(stone_error(
                            "stop_daemon",
                            format!("unsupported keyword `{other}`; expected timeout_ms"),
                        ));
                    }
                }
            }
            Ok(RuntimeValue::Nu(stop_daemon_record(
                pid,
                Duration::from_millis(timeout_ms as u64),
            )))
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
            let [port_expr] = call.positional.as_slice() else {
                return Err(stone_error(
                    "wait_port",
                    "wait_port() requires exactly one port",
                ));
            };
            let port_value = self
                .eval_expr_value(port_expr, PipelineData::empty())?
                .into_nu_value("wait_port")?;
            let port = value_to_port(&port_value, "wait_port port")?;
            let mut host = "127.0.0.1".to_owned();
            let mut timeout_ms: i64 = 30_000;
            for (name, argument) in &call.named {
                let value = self
                    .eval_expr_value(argument, PipelineData::empty())?
                    .into_nu_value("wait_port")?;
                match name.as_str() {
                    "host" => host = value_to_string(&value, "wait_port host")?,
                    "timeout_ms" => {
                        timeout_ms = value_to_i64(&value, "wait_port timeout_ms")?;
                        if timeout_ms <= 0 {
                            return Err(stone_error("wait_port", "timeout_ms must be positive"));
                        }
                    }
                    other => {
                        return Err(stone_error(
                            "wait_port",
                            format!("unsupported keyword `{other}`; expected host or timeout_ms"),
                        ));
                    }
                }
            }
            Ok(RuntimeValue::Nu(wait_port_record(
                &host,
                port,
                Duration::from_millis(timeout_ms as u64),
            )))
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
        match value {
            Value::List { vals, .. } => Ok(RuntimeValue::Nu(Value::list(vals, Span::unknown()))),
            Value::Record { .. } => eval_record_method(&value, "keys", &[]).map(RuntimeValue::Nu),
            other => Err(stone_error(
                "list",
                format!("expected list or record, got {}", other.get_type()),
            )),
        }
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

        let mut best: Option<Value> = None;
        for argument in &call.positional {
            let value = self
                .eval_expr_value(argument, PipelineData::empty())?
                .into_nu_value(operation.name())?;
            if !matches!(value, Value::Int { .. } | Value::Float { .. }) {
                return Err(stone_error(
                    operation.name(),
                    format!(
                        "{}() expected numbers, got {}",
                        operation.name(),
                        value.get_type()
                    ),
                ));
            }
            let Some(current) = &best else {
                best = Some(value);
                continue;
            };
            let ordering = value_ordering(&value, current)?;
            if operation.should_replace(ordering) {
                best = Some(value);
            }
        }
        Ok(RuntimeValue::Nu(best.expect("checked non-empty arguments")))
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
        let text = value_to_string(&text, "split")?;
        let parts = match call.positional.as_slice() {
            [_] => text.split_whitespace().collect::<Vec<_>>(),
            [_, separator] => {
                let separator = self
                    .eval_expr_value(separator, PipelineData::empty())?
                    .into_nu_value("split")?;
                let separator = value_to_string(&separator, "split")?;
                text.split(&separator).collect::<Vec<_>>()
            }
            _ => unreachable!(),
        };
        Ok(RuntimeValue::Nu(Value::list(
            parts
                .into_iter()
                .map(|part| Value::string(part.to_owned(), Span::unknown()))
                .collect(),
            Span::unknown(),
        )))
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
        let (items, separator) = match call.positional.as_slice() {
            [_] => (first, String::new()),
            [_, second] => {
                let second = self
                    .eval_expr_value(second, PipelineData::empty())?
                    .into_nu_value("join")?;
                match (&first, &second) {
                    (Value::List { .. }, _) => (first, value_to_string(&second, "join")?),
                    (_, Value::List { .. }) => (second, value_to_string(&first, "join")?),
                    _ => {
                        return Err(stone_error(
                            "join",
                            "join() requires a list and optional separator",
                        ));
                    }
                }
            }
            _ => unreachable!(),
        };
        let Value::List { vals, .. } = items else {
            return Err(stone_error(
                "join",
                format!("expected list, got {}", items.get_type()),
            ));
        };
        let mut parts = Vec::with_capacity(vals.len());
        for value in &vals {
            parts.push(value_to_string(value, "join")?);
        }
        Ok(RuntimeValue::Nu(Value::string(
            parts.join(&separator),
            Span::unknown(),
        )))
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
        eval_slice(&value, start, end).map(RuntimeValue::Nu)
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
        let text = value_to_string(&text, "starts_with")?;
        let prefix = value_to_string(&prefix, "starts_with")?;
        Ok(RuntimeValue::Nu(Value::bool(
            text.starts_with(&prefix),
            Span::unknown(),
        )))
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
        let template = value_to_string(&template, "format")?;
        let mut args = Vec::with_capacity(call.positional.len().saturating_sub(1));
        for arg in call.positional.iter().skip(1) {
            let value = self
                .eval_expr_value(arg, PipelineData::empty())?
                .into_nu_value("format")?;
            args.push(value_to_display_string(&value)?);
        }
        Ok(RuntimeValue::Nu(Value::string(
            format_template(&template, &args)?,
            Span::unknown(),
        )))
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
        let (start, stop, step) = match args.as_slice() {
            [stop] => (0, *stop, 1),
            [start, stop] => (*start, *stop, 1),
            [start, stop, step] => (*start, *stop, *step),
            _ => {
                return Err(stone_error(
                    "range",
                    "range() requires one to three integer arguments",
                ));
            }
        };
        if step == 0 {
            return Err(stone_error("range", "range() step must not be zero"));
        }
        let mut values = Vec::new();
        let mut current = start;
        while if step > 0 {
            current < stop
        } else {
            current > stop
        } {
            if values.len() >= 100_000 {
                return Err(stone_error("range", "range() produced too many values"));
            }
            values.push(Value::int(current, Span::unknown()));
            current = current
                .checked_add(step)
                .ok_or_else(|| stone_error("range", "range() integer overflow"))?;
        }
        Ok(RuntimeValue::Nu(Value::list(values, Span::unknown())))
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
        let mut output = Vec::with_capacity(values.len());
        for (offset, value) in values.into_iter().enumerate() {
            let value = value.into_nu_value("enumerate")?;
            let index = start
                .checked_add(
                    i64::try_from(offset)
                        .map_err(|_| stone_error("enumerate", "index is too large"))?,
                )
                .ok_or_else(|| stone_error("enumerate", "index overflow"))?;
            output.push(Value::list(
                vec![Value::int(index, Span::unknown()), value],
                Span::unknown(),
            ));
        }
        Ok(RuntimeValue::Nu(Value::list(output, Span::unknown())))
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
                "strip" => eval_string_method(&receiver, method, &args, |text, args| {
                    let stripped = match args {
                        [] => text.trim().to_owned(),
                        [chars] => {
                            let chars = value_to_string(chars, "strip")?;
                            text.trim_matches(|ch| chars.contains(ch)).to_owned()
                        }
                        _ => {
                            return Err(stone_error("strip", "strip() takes at most one argument"));
                        }
                    };
                    Ok(Value::string(stripped, Span::unknown()))
                })
                .map(RuntimeValue::Nu),
                "split" => eval_string_method(&receiver, method, &args, |text, args| {
                    let parts = match args {
                        [] => text.split_whitespace().collect::<Vec<_>>(),
                        [separator] => {
                            let separator = value_to_string(separator, "split")?;
                            text.split(&separator).collect::<Vec<_>>()
                        }
                        _ => {
                            return Err(stone_error("split", "split() takes at most one argument"));
                        }
                    };
                    Ok(Value::list(
                        parts
                            .into_iter()
                            .map(|part| Value::string(part.to_owned(), Span::unknown()))
                            .collect(),
                        Span::unknown(),
                    ))
                })
                .map(RuntimeValue::Nu),
                "splitlines" => eval_string_method(&receiver, method, &args, |text, args| {
                    let [] = args else {
                        return Err(stone_error(
                            "splitlines",
                            "splitlines() takes no arguments for now",
                        ));
                    };
                    Ok(Value::list(
                        text.lines()
                            .map(|part| Value::string(part.to_owned(), Span::unknown()))
                            .collect(),
                        Span::unknown(),
                    ))
                })
                .map(RuntimeValue::Nu),
                "replace" => eval_string_method(&receiver, method, &args, |text, args| {
                    let [old, new] = args else {
                        return Err(stone_error(
                            "replace",
                            "replace() requires old and new arguments",
                        ));
                    };
                    let old = value_to_string(old, "replace")?;
                    let new = value_to_string(new, "replace")?;
                    Ok(Value::string(text.replace(&old, &new), Span::unknown()))
                })
                .map(RuntimeValue::Nu),
                "join" => eval_string_method(&receiver, method, &args, |separator, args| {
                    let [items] = args else {
                        return Err(stone_error("join", "join() requires exactly one iterable"));
                    };
                    let Value::List { vals, .. } = items else {
                        return Err(stone_error(
                            "join",
                            format!("expected list, got {}", items.get_type()),
                        ));
                    };
                    let mut parts = Vec::with_capacity(vals.len());
                    for value in vals {
                        parts.push(value_to_string(value, "join")?);
                    }
                    Ok(Value::string(parts.join(separator), Span::unknown()))
                })
                .map(RuntimeValue::Nu),
                "get" | "items" | "keys" | "values" => {
                    eval_record_method(&receiver, method, &args).map(RuntimeValue::Nu)
                }
                "find" => eval_find_method(&receiver, &args).map(RuntimeValue::Nu),
                "index" => eval_index_method(&receiver, &args).map(RuntimeValue::Nu),
                "lower" => eval_string_method(&receiver, method, &args, |text, args| {
                    let [] = args else {
                        return Err(stone_error("lower", "lower() takes no arguments"));
                    };
                    Ok(Value::string(text.to_lowercase(), Span::unknown()))
                })
                .map(RuntimeValue::Nu),
                "upper" => eval_string_method(&receiver, method, &args, |text, args| {
                    let [] = args else {
                        return Err(stone_error("upper", "upper() takes no arguments"));
                    };
                    Ok(Value::string(text.to_uppercase(), Span::unknown()))
                })
                .map(RuntimeValue::Nu),
                "zfill" => eval_string_method(&receiver, method, &args, |text, args| {
                    let [width] = args else {
                        return Err(stone_error("zfill", "zfill() requires exactly one width"));
                    };
                    let width = value_to_i64(width, "zfill width")?;
                    if width < 0 {
                        return Err(stone_error("zfill", "width must be non-negative"));
                    }
                    let width = usize::try_from(width)
                        .map_err(|_| stone_error("zfill", "width is too large"))?;
                    Ok(Value::string(zfill(text, width), Span::unknown()))
                })
                .map(RuntimeValue::Nu),
                "startswith" => eval_string_method(&receiver, method, &args, |text, args| {
                    let [prefix] = args else {
                        return Err(stone_error(
                            "startswith",
                            "startswith() requires exactly one argument",
                        ));
                    };
                    let prefix = value_to_string(prefix, "startswith")?;
                    Ok(Value::bool(text.starts_with(&prefix), Span::unknown()))
                })
                .map(RuntimeValue::Nu),
                "endswith" => eval_string_method(&receiver, method, &args, |text, args| {
                    let [suffix] = args else {
                        return Err(stone_error(
                            "endswith",
                            "endswith() requires exactly one argument",
                        ));
                    };
                    let suffix = value_to_string(suffix, "endswith")?;
                    Ok(Value::bool(text.ends_with(&suffix), Span::unknown()))
                })
                .map(RuntimeValue::Nu),
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
            if is_map_builtin_name(func_name) {
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

        let values: Vec<RuntimeValue> = match arg {
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

        let mut int_total = 0i64;
        let mut float_total = 0.0f64;
        let mut has_float = false;
        for value in values {
            let value = value.into_nu_value("sum")?;
            match value_to_sum_number(&value)? {
                SumNumber::Int(value) if has_float => {
                    float_total += value as f64;
                }
                SumNumber::Int(value) => {
                    int_total = int_total
                        .checked_add(value)
                        .ok_or_else(|| stone_error("sum", "integer sum overflow"))?;
                }
                SumNumber::Float(value) if has_float => {
                    float_total += value;
                }
                SumNumber::Float(value) => {
                    has_float = true;
                    float_total = int_total as f64 + value;
                }
            }
        }
        if has_float {
            Ok(RuntimeValue::Nu(Value::float(float_total, Span::unknown())))
        } else {
            Ok(RuntimeValue::Nu(Value::int(int_total, Span::unknown())))
        }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SortKeyKind {
    Number,
    Text,
    Composite,
}

impl MinMax {
    fn name(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Max => "max",
        }
    }

    fn should_replace(self, ordering: std::cmp::Ordering) -> bool {
        match self {
            Self::Min => ordering.is_lt(),
            Self::Max => ordering.is_gt(),
        }
    }
}

fn value_to_int(value: &Value) -> Result<Value, ShellError> {
    value_to_i64(value, "int").map(|value| Value::int(value, Span::unknown()))
}

fn generic_vm_number_from_runtime(value: &RuntimeValue) -> Option<GenericVmNumber> {
    match value {
        RuntimeValue::Nu(Value::Int { val, .. }) => Some(GenericVmNumber::I64(*val)),
        RuntimeValue::Nu(Value::Float { val, .. }) => Some(GenericVmNumber::F64(*val)),
        _ => None,
    }
}

fn generic_vm_record_field_value(
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

fn generic_vm_add_number(
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

fn generic_vm_number_to_value(value: GenericVmNumber) -> Value {
    match value {
        GenericVmNumber::I64(value) => Value::int(value, Span::unknown()),
        GenericVmNumber::F64(value) => Value::float(value, Span::unknown()),
    }
}

fn value_to_i64(value: &Value, context: &str) -> Result<i64, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(*val),
        Value::Float { val, .. } => Ok(*val as i64),
        Value::String { val, .. } | Value::Glob { val, .. } => val
            .trim()
            .parse::<i64>()
            .map_err(|err| stone_error(context, format!("failed to parse integer: {err}"))),
        other => Err(stone_error(
            context,
            format!("expected integer, got {}", other.get_type()),
        )),
    }
}

fn value_to_limit(value: &Value, context: &str) -> Result<usize, ShellError> {
    let limit = value_to_i64(value, context)?;
    if limit < 0 {
        return Err(stone_error(context, "limit must be non-negative"));
    }
    usize::try_from(limit).map_err(|_| stone_error(context, "limit is too large"))
}

#[cfg(not(target_os = "hermit"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunOutputTarget {
    Capture,
    Suppress,
    Stdout,
}

#[cfg(not(target_os = "hermit"))]
fn value_to_run_stdout_target(value: &Value, context: &str) -> Result<RunOutputTarget, ShellError> {
    let target = value_to_string(value, context)?;
    match target.as_str() {
        "capture" | "pipe" => Ok(RunOutputTarget::Capture),
        "suppress" | "discard" | "null" | "none" | "devnull" => Ok(RunOutputTarget::Suppress),
        "stdout" => Err(stone_error(
            context,
            "stdout cannot be redirected to itself; use stdout=\"capture\" or stdout=\"suppress\"",
        )),
        other => Err(stone_error(
            context,
            format!(
                "unsupported stdout target `{other}`; expected capture, suppress, discard, null, none, or devnull"
            ),
        )),
    }
}

#[cfg(not(target_os = "hermit"))]
fn value_to_run_stderr_target(value: &Value, context: &str) -> Result<RunOutputTarget, ShellError> {
    let target = value_to_string(value, context)?;
    match target.as_str() {
        "capture" | "pipe" => Ok(RunOutputTarget::Capture),
        "suppress" | "discard" | "null" | "none" | "devnull" => Ok(RunOutputTarget::Suppress),
        "stdout" => Ok(RunOutputTarget::Stdout),
        other => Err(stone_error(
            context,
            format!(
                "unsupported stderr target `{other}`; expected capture, stdout, suppress, discard, null, none, or devnull"
            ),
        )),
    }
}

fn value_to_f64(value: &Value, context: &str) -> Result<f64, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(*val as f64),
        Value::Float { val, .. } => Ok(*val),
        Value::String { val, .. } | Value::Glob { val, .. } => val
            .trim()
            .parse::<f64>()
            .map_err(|err| stone_error(context, format!("failed to parse float: {err}"))),
        other => Err(stone_error(
            context,
            format!("expected number, got {}", other.get_type()),
        )),
    }
}

fn value_to_bool(value: &Value, context: &str) -> Result<bool, ShellError> {
    match value {
        Value::Bool { val, .. } => Ok(*val),
        other => Err(stone_error(
            context,
            format!("expected bool, got {}", other.get_type()),
        )),
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

fn value_to_string(value: &Value, context: &str) -> Result<String, ShellError> {
    match value {
        Value::String { val, .. } | Value::Glob { val, .. } => Ok(val.clone()),
        other => Err(stone_error(
            context,
            format!("expected string, got {}", other.get_type()),
        )),
    }
}

fn value_to_path_string(value: &Value, context: &str) -> Result<String, ShellError> {
    match value {
        Value::Record { val, .. } => {
            let Some(path) = val.get("path") else {
                return Err(stone_error(
                    context,
                    format!(
                        "record path argument is missing `path`; got {}",
                        value.get_type()
                    ),
                ));
            };
            value_to_string(path, context)
        }
        _ => value_to_string(value, context),
    }
}

#[cfg(not(target_os = "hermit"))]
fn value_to_string_list(value: &Value, context: &str) -> Result<Vec<String>, ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            context,
            format!(
                "expected list of strings, got {}; use {context}([\"cmd\", \"arg\"]) instead of {context}(\"cmd\")",
                value.get_type()
            ),
        ));
    };
    vals.iter()
        .map(|value| value_to_string(value, context))
        .collect()
}

#[cfg(not(target_os = "hermit"))]
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

#[cfg(not(target_os = "hermit"))]
fn value_to_port(value: &Value, context: &str) -> Result<u16, ShellError> {
    let port = value_to_i64(value, context)?;
    if !(1..=65_535).contains(&port) {
        return Err(stone_error(context, "port must be between 1 and 65535"));
    }
    Ok(port as u16)
}

#[cfg(not(target_os = "hermit"))]
fn value_to_daemon_pid(value: &Value, context: &str) -> Result<u32, ShellError> {
    let raw_pid = match value {
        Value::Record { val, .. } => {
            let Some(pid) = val.get("pid") else {
                return Err(stone_error(
                    context,
                    "daemon record is missing `pid`; pass the start_daemon() result or a pid",
                ));
            };
            value_to_i64(pid, context)?
        }
        _ => value_to_i64(value, context)?,
    };
    if raw_pid <= 0 {
        return Err(stone_error(context, "pid must be positive"));
    }
    u32::try_from(raw_pid).map_err(|_| stone_error(context, "pid is too large"))
}

#[cfg(not(target_os = "hermit"))]
fn daemon_log_path(value: &Value) -> Option<PathBuf> {
    let Value::Record { val, .. } = value else {
        return None;
    };
    val.get("stderr_path")
        .or_else(|| val.get("stdout_path"))
        .and_then(|value| value_to_string(value, "daemon log path").ok())
        .map(PathBuf::from)
}

#[cfg(not(target_os = "hermit"))]
fn daemon_temp_path(suffix: &str) -> PathBuf {
    static DAEMON_ID: AtomicU64 = AtomicU64::new(0);
    let temp_prefix = format!(
        "stone-daemon-{}-{}",
        std::process::id(),
        DAEMON_ID.fetch_add(1, AtomicOrdering::Relaxed)
    );
    env::temp_dir().join(format!("{temp_prefix}.{suffix}"))
}

#[cfg(not(target_os = "hermit"))]
fn resolve_command_record(name: &str) -> Value {
    let span = Span::unknown();
    let resolution = resolve_command(name);
    let ok = !resolution.matches.is_empty();
    let mut record = Record::new();
    record.push("ok", Value::bool(ok, span));
    record.push("name", Value::string(name.to_owned(), span));
    record.push(
        "path",
        resolution
            .matches
            .first()
            .map(|path| Value::string(path.display().to_string(), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "matches",
        Value::list(
            resolution
                .matches
                .iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "searched",
        Value::list(
            resolution
                .searched
                .iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "explanation",
        command_resolution_explanation(name, &resolution, span),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
struct CommandResolution {
    matches: Vec<PathBuf>,
    searched: Vec<PathBuf>,
}

#[cfg(not(target_os = "hermit"))]
fn resolve_command(name: &str) -> CommandResolution {
    if name.contains('/') {
        let path = PathBuf::from(name);
        return CommandResolution {
            matches: if is_executable_file(&path) {
                vec![path.clone()]
            } else {
                Vec::new()
            },
            searched: path.parent().map(Path::to_path_buf).into_iter().collect(),
        };
    }

    let searched: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    let mut matches = Vec::new();
    for dir in &searched {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            matches.push(candidate);
        }
    }
    CommandResolution { matches, searched }
}

#[cfg(not(target_os = "hermit"))]
fn resolve_command_with_env(name: &str, env_overrides: &[(String, String)]) -> CommandResolution {
    if name.contains('/') {
        return resolve_command(name);
    }

    let path_override = env_overrides
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.as_str());
    let Some(path) = path_override else {
        return resolve_command(name);
    };

    let searched: Vec<PathBuf> = env::split_paths(path).collect();
    let mut matches = Vec::new();
    for dir in &searched {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            matches.push(candidate);
        }
    }
    CommandResolution { matches, searched }
}

#[cfg(all(not(target_os = "hermit"), unix))]
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(all(not(target_os = "hermit"), not(unix)))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(not(target_os = "hermit"))]
fn maybe_python_runtime_context(
    argv: &[String],
    cwd: &Path,
    env_overrides: &[(String, String)],
) -> Option<Value> {
    let command = argv.first()?;
    let command_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command.as_str());
    let module_name =
        if command_name.starts_with("python") && argv.get(1).map(String::as_str) == Some("-m") {
            argv.get(2).map(String::as_str)
        } else {
            None
        };
    let is_python_adjacent = command_name.starts_with("python")
        || matches!(command_name, "pip" | "pip3" | "uv" | "pytest")
        || matches!(module_name, Some("pip" | "pytest" | "build" | "twine"));
    if !is_python_adjacent {
        return None;
    }

    let span = Span::unknown();
    let resolution = resolve_command_with_env(command, env_overrides);
    let env_virtual_env = env_value_with_overrides("VIRTUAL_ENV", env_overrides);
    let env_pythonpath = env_value_with_overrides("PYTHONPATH", env_overrides);
    let uv_project_environment = env_value_with_overrides("UV_PROJECT_ENVIRONMENT", env_overrides);

    let mut record = Record::new();
    record.push("kind", Value::string("python", span));
    record.push("command_name", Value::string(command_name.to_owned(), span));
    record.push(
        "resolved_executable",
        resolution
            .matches
            .first()
            .map(|path| Value::string(path.display().to_string(), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "matches",
        Value::list(
            resolution
                .matches
                .iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "env_virtual_env",
        env_virtual_env
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "env_pythonpath",
        env_pythonpath
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "uv_project_environment",
        uv_project_environment
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "cwd_project_markers",
        Value::list(
            python_project_markers(cwd)
                .into_iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );

    if command_name.starts_with("python") {
        match python_self_probe(command, cwd, env_overrides) {
            Ok(probe) => {
                record.push(
                    "python_executable",
                    string_or_null(probe.get("executable"), span),
                );
                record.push("python_version", string_or_null(probe.get("version"), span));
                record.push("sys_prefix", string_or_null(probe.get("prefix"), span));
                record.push(
                    "sys_base_prefix",
                    string_or_null(probe.get("base_prefix"), span),
                );
                record.push(
                    "pip_available",
                    Value::bool(
                        probe
                            .get("pip_available")
                            .and_then(JsonValue::as_bool)
                            .unwrap_or(false),
                        span,
                    ),
                );
            }
            Err(err) => record.push("python_probe_error", Value::string(err, span)),
        }
    }

    if command_name == "uv" {
        record.push(
            "note",
            Value::string(
                "uv may select a project environment when cwd contains Python project markers; compare env_virtual_env with the interpreter used by uv run.",
                span,
            ),
        );
    }

    Some(Value::record(record, span))
}

#[cfg(not(target_os = "hermit"))]
fn env_value_with_overrides(name: &str, env_overrides: &[(String, String)]) -> Option<String> {
    env_overrides
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .or_else(|| env::var(name).ok())
}

#[cfg(not(target_os = "hermit"))]
fn python_project_markers(cwd: &Path) -> Vec<PathBuf> {
    let marker_names = ["pyproject.toml", "setup.py", "setup.cfg", ".venv"];
    let mut markers = Vec::new();
    let mut dir = cwd;
    for _ in 0..=3 {
        for name in marker_names {
            let path = dir.join(name);
            if path.exists() {
                markers.push(path);
            }
        }
        let Some(parent) = dir.parent() else {
            break;
        };
        dir = parent;
    }
    markers
}

#[cfg(not(target_os = "hermit"))]
fn python_self_probe(
    python: &str,
    cwd: &Path,
    env_overrides: &[(String, String)],
) -> Result<JsonValue, String> {
    let source = r#"import importlib.util, json, sys
print(json.dumps({
    "executable": sys.executable,
    "version": sys.version.split()[0],
    "prefix": sys.prefix,
    "base_prefix": sys.base_prefix,
    "pip_available": importlib.util.find_spec("pip") is not None,
}))
"#;
    let mut child = Command::new(python)
        .arg("-c")
        .arg(source)
        .current_dir(cwd)
        .envs(env_overrides.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to probe Python runtime: {err}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= Duration::from_secs(2) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Python runtime probe timed out after 2 seconds".to_owned());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(format!("failed to wait for Python runtime probe: {err}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("failed to read Python runtime probe output: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Python runtime probe exited with code {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("failed to parse Python runtime probe output: {err}"))
}

#[cfg(not(target_os = "hermit"))]
fn string_or_null(value: Option<&JsonValue>, span: Span) -> Value {
    value
        .and_then(JsonValue::as_str)
        .map(|text| Value::string(text.to_owned(), span))
        .unwrap_or_else(|| Value::nothing(span))
}

fn runtime_state_record(cwd: &Path) -> Value {
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("cwd", Value::string(cwd.display().to_string(), span));
    record.push("git", git_state_record(cwd, span));
    record.push("tools", tool_state_record(cwd, span));
    Value::record(record, span)
}

#[cfg(target_os = "hermit")]
fn git_state_record(_cwd: &Path, span: Span) -> Value {
    let mut record = Record::new();
    record.push("ok", Value::bool(false, span));
    record.push("kind", Value::string("unavailable", span));
    record.push(
        "message",
        Value::string("git state is unavailable on Hermit", span),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn git_state_record(cwd: &Path, span: Span) -> Value {
    let cwd_arg = cwd.display().to_string();
    let Some(status) = bounded_command_stdout(
        "git",
        &[
            "-C",
            cwd_arg.as_str(),
            "status",
            "--porcelain=v1",
            "--branch",
        ],
        cwd,
        Duration::from_millis(750),
    ) else {
        let mut record = Record::new();
        record.push("ok", Value::bool(false, span));
        record.push("kind", Value::string("unavailable", span));
        record.push(
            "message",
            Value::string("git status did not complete", span),
        );
        return Value::record(record, span);
    };

    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0_i64;
    let mut behind = 0_i64;
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicted = Vec::new();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            let (head, tracking) = rest.split_once("...").unwrap_or((rest, ""));
            branch = Some(head.to_owned());
            if !tracking.is_empty() {
                let (name, counts) = tracking.split_once(' ').unwrap_or((tracking, ""));
                upstream = Some(name.to_owned());
                ahead = parse_git_count(counts, "ahead").unwrap_or(0);
                behind = parse_git_count(counts, "behind").unwrap_or(0);
            }
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let status = &line[..2];
        let path = line[3..].to_owned();
        if status == "??" {
            untracked.push(path);
            continue;
        }
        if status.contains('U') || matches!(status, "AA" | "DD") {
            conflicted.push(path.clone());
        }
        let mut chars = status.chars();
        let index = chars.next().unwrap_or(' ');
        let worktree = chars.next().unwrap_or(' ');
        if index != ' ' {
            staged.push(path.clone());
        }
        if worktree != ' ' {
            modified.push(path);
        }
    }

    let mut record = Record::new();
    record.push("ok", Value::bool(true, span));
    record.push(
        "branch",
        branch
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "upstream",
        upstream
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push("ahead", Value::int(ahead, span));
    record.push("behind", Value::int(behind, span));
    record.push(
        "dirty",
        Value::bool(
            !staged.is_empty()
                || !modified.is_empty()
                || !untracked.is_empty()
                || !conflicted.is_empty(),
            span,
        ),
    );
    record.push("staged_files", string_list_value(staged, span));
    record.push("modified_files", string_list_value(modified, span));
    record.push("untracked_files", string_list_value(untracked, span));
    record.push("conflicted_files", string_list_value(conflicted, span));
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn parse_git_count(text: &str, key: &str) -> Option<i64> {
    let marker = format!("{key} ");
    let start = text.find(&marker)? + marker.len();
    let digits = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

#[cfg(target_os = "hermit")]
fn tool_state_record(_cwd: &Path, span: Span) -> Value {
    let mut record = Record::new();
    record.push("available", Value::list(Vec::new(), span));
    record.push("unavailable", Value::list(Vec::new(), span));
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn tool_state_record(cwd: &Path, span: Span) -> Value {
    let names = [
        "python3", "python", "pip", "node", "npm", "cargo", "rustc", "go", "java", "javac", "gcc",
        "clang", "make", "git",
    ];
    let mut available = Vec::new();
    let mut unavailable = Vec::new();
    for name in names {
        let resolution = resolve_command(name);
        if let Some(path) = resolution.matches.first() {
            let mut record = Record::new();
            record.push("name", Value::string(name, span));
            record.push("path", Value::string(path.display().to_string(), span));
            if let Some(version) = tool_version(name, cwd) {
                record.push("version", Value::string(version, span));
            }
            available.push(Value::record(record, span));
        } else {
            unavailable.push(Value::string(name, span));
        }
    }
    let mut record = Record::new();
    record.push("available", Value::list(available, span));
    record.push("unavailable", Value::list(unavailable, span));
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn tool_version(name: &str, cwd: &Path) -> Option<String> {
    let args: &[&str] = match name {
        "python3" | "python" => &["--version"],
        "pip" => &["--version"],
        "node" | "npm" | "cargo" | "rustc" | "go" | "java" | "javac" | "gcc" | "clang" | "make"
        | "git" => &["--version"],
        _ => return None,
    };
    bounded_command_output(name, args, cwd, Duration::from_millis(750)).map(|text| {
        text.lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_owned()
    })
}

#[cfg(not(target_os = "hermit"))]
fn bounded_command_stdout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    bounded_command_output(program, args, cwd, timeout).filter(|text| !text.is_empty())
}

#[cfg(not(target_os = "hermit"))]
fn bounded_command_output(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Some(if stdout.trim().is_empty() {
        stderr.into_owned()
    } else {
        stdout.into_owned()
    })
}

fn string_list_value(items: Vec<String>, span: Span) -> Value {
    Value::list(
        items
            .into_iter()
            .map(|item| Value::string(item, span))
            .collect(),
        span,
    )
}

#[cfg(not(target_os = "hermit"))]
fn command_resolution_explanation(name: &str, resolution: &CommandResolution, span: Span) -> Value {
    if let Some(first) = resolution.matches.first() {
        let summary = format!(
            "Executable `{name}` resolves to `{}`; {} executable match(es) were found in PATH.",
            first.display(),
            resolution.matches.len()
        );
        command_explanation(
            "command_found",
            summary,
            "external command resolution; no process was started",
            &[
                "Use the first path when PATH ordering is intended.",
                "Use an absolute path if a later match is required.",
            ],
            span,
        )
    } else {
        command_explanation(
            "command_not_found",
            format!("Executable `{name}` was not found in PATH."),
            "external command resolution; no process was started",
            &[
                "Check the executable name.",
                "Inspect the searched PATH locations reported in `searched`.",
                "Install or provide the executable if the task requires it.",
            ],
            span,
        )
    }
}

#[cfg(not(target_os = "hermit"))]
fn command_spawn_error(context: &str, name: &str, err: &std::io::Error) -> ShellError {
    match err.kind() {
        ErrorKind::NotFound => {
            let resolution = resolve_command(name);
            let searched = if resolution.searched.is_empty() {
                "<empty PATH>".to_owned()
            } else {
                resolution
                    .searched
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            stone_error(
                context,
                format!(
                    "executable `{name}` was not found in PATH; searched: {searched}. Use resolve_command(\"{name}\") for structured command-resolution details."
                ),
            )
        }
        ErrorKind::PermissionDenied => stone_error(
            context,
            format!(
                "permission denied while spawning executable `{name}`. Use resolve_command(\"{name}\") and stat() to inspect the resolved file and permissions."
            ),
        ),
        ErrorKind::InvalidInput => stone_error(
            context,
            format!(
                "invalid process arguments or environment while spawning `{name}`; check argv and env for unsupported values such as interior NUL bytes."
            ),
        ),
        ErrorKind::Interrupted => stone_error(
            context,
            format!("process spawn for `{name}` was interrupted before exec; retry if the task state is otherwise unchanged."),
        ),
        ErrorKind::OutOfMemory => stone_error(
            context,
            format!("not enough memory to spawn executable `{name}`."),
        ),
        _ => {
            if let Some(raw) = err.raw_os_error() {
                return command_spawn_raw_os_error(context, name, raw, err);
            }
            stone_error(context, format!("failed to spawn `{name}`: {err}"))
        }
    }
}

#[cfg(not(target_os = "hermit"))]
fn command_spawn_raw_os_error(
    context: &str,
    name: &str,
    raw: i32,
    err: &std::io::Error,
) -> ShellError {
    let (kind, detail, next) = match raw {
        7 => (
            "argument_list_too_large",
            "the argv or environment block is too large",
            "Reduce argv/env size or pass large input through files/stdin.",
        ),
        8 => (
            "exec_format_error",
            "the file exists but is not executable for this OS/architecture, or a script is missing a valid shebang",
            "Inspect the executable with stat() and read_file(); use a valid interpreter or binary.",
        ),
        11 => (
            "spawn_resource_limit",
            "the OS temporarily could not allocate process resources",
            "Retry after other processes exit, or reduce concurrent process starts.",
        ),
        12 => (
            "spawn_resource_limit",
            "the OS reported insufficient memory",
            "Reduce memory pressure before spawning another process.",
        ),
        13 => (
            "permission_denied",
            "permission was denied by executable, directory, or mount permissions",
            "Use resolve_command() and stat() to inspect the executable and containing directories.",
        ),
        20 => (
            "path_component_not_directory",
            "a component in the executable or cwd path is not a directory",
            "Use stat() on the path components to find the non-directory entry.",
        ),
        23 | 24 => (
            "spawn_resource_limit",
            "the process or system file descriptor limit was reached",
            "Close unneeded files/processes before retrying.",
        ),
        26 => (
            "text_file_busy",
            "the executable is currently busy, often because it is being written",
            "Wait for file writes to finish or use a stable executable path.",
        ),
        _ => (
            "spawn_failed",
            "the OS rejected process spawn",
            "Inspect cwd, argv, environment, executable path, and permissions.",
        ),
    };
    stone_error(
        context,
        format!("{kind}: failed to spawn `{name}`: {err}. Cause: {detail}. Next step: {next}"),
    )
}

#[cfg(not(target_os = "hermit"))]
fn validate_command_cwd(context: &str, cwd: &Path) -> Result<(), ShellError> {
    let metadata = fs::metadata(cwd).map_err(|err| match err.kind() {
        ErrorKind::NotFound => stone_error(
            context,
            format!(
                "cwd `{}` does not exist; use stat() to inspect the intended working directory.",
                cwd.display()
            ),
        ),
        ErrorKind::PermissionDenied => stone_error(
            context,
            format!(
                "permission denied while accessing cwd `{}`; use stat() to inspect directory permissions.",
                cwd.display()
            ),
        ),
        _ => io_stone_error(context, err, cwd),
    })?;
    if !metadata.is_dir() {
        return Err(stone_error(
            context,
            format!(
                "cwd `{}` exists but is not a directory; choose a directory cwd.",
                cwd.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "hermit"))]
fn create_command_output_file(context: &str, label: &str, path: &Path) -> Result<File, ShellError> {
    File::create(path).map_err(|err| match err.kind() {
        ErrorKind::NotFound => stone_error(
            context,
            format!(
                "{label} path `{}` could not be created because a parent directory is missing; create the parent directory or choose another log path.",
                path.display()
            ),
        ),
        ErrorKind::PermissionDenied => stone_error(
            context,
            format!(
                "permission denied while creating {label} path `{}`; use stat() to inspect parent directory permissions.",
                path.display()
            ),
        ),
        ErrorKind::InvalidInput => stone_error(
            context,
            format!(
                "{label} path `{}` is invalid for file creation.",
                path.display()
            ),
        ),
        ErrorKind::OutOfMemory => stone_error(
            context,
            format!("not enough memory while creating {label} path `{}`.", path.display()),
        ),
        _ => {
            if let Some(raw) = err.raw_os_error() {
                if matches!(raw, 23 | 24 | 28) {
                    return stone_error(
                        context,
                        format!(
                            "resource limit while creating {label} path `{}`: {err}. Check file descriptor limits or available disk space.",
                            path.display()
                        ),
                    );
                }
            }
            io_stone_error(context, err, path)
        }
    })
}

#[cfg(not(target_os = "hermit"))]
fn command_explanation(
    kind: &str,
    summary: String,
    scope: &str,
    next_steps: &[&str],
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push("summary", Value::string(summary, span));
    record.push("scope", Value::string(scope.to_owned(), span));
    record.push(
        "next_steps",
        Value::list(
            next_steps
                .iter()
                .map(|step| Value::string((*step).to_owned(), span))
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn run_posix_command(
    argv: &[String],
    cwd: &Path,
    env_overrides: &[(String, String)],
    stdin: Option<&str>,
    timeout: Duration,
    stdout_target: RunOutputTarget,
    stderr_target: RunOutputTarget,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<Record, ShellError> {
    static RUN_ID: AtomicU64 = AtomicU64::new(0);

    let span = Span::unknown();
    let started = Instant::now();
    validate_command_cwd("run", cwd)?;
    let temp_prefix = format!(
        "stone-run-{}-{}",
        std::process::id(),
        RUN_ID.fetch_add(1, AtomicOrdering::Relaxed)
    );
    let stdout_path = env::temp_dir().join(format!("{temp_prefix}.stdout"));
    let stderr_path = env::temp_dir().join(format!("{temp_prefix}.stderr"));
    let stdout_file = (stdout_target == RunOutputTarget::Capture)
        .then(|| create_command_output_file("run", "stdout", &stdout_path))
        .transpose()?;
    let stderr_file = (stderr_target == RunOutputTarget::Capture)
        .then(|| create_command_output_file("run", "stderr", &stderr_path))
        .transpose()?;

    let mut command = Command::new(&argv[0]);
    let stdout_stdio = match stdout_target {
        RunOutputTarget::Capture => Stdio::from(
            stdout_file
                .as_ref()
                .expect("stdout capture file should exist")
                .try_clone()
                .map_err(|err| stone_error("run", format!("failed to clone stdout file: {err}")))?,
        ),
        RunOutputTarget::Suppress => Stdio::null(),
        RunOutputTarget::Stdout => unreachable!("stdout cannot target stdout"),
    };
    let stderr_stdio = match stderr_target {
        RunOutputTarget::Capture => Stdio::from(
            stderr_file
                .as_ref()
                .expect("stderr capture file should exist")
                .try_clone()
                .map_err(|err| stone_error("run", format!("failed to clone stderr file: {err}")))?,
        ),
        RunOutputTarget::Suppress => Stdio::null(),
        RunOutputTarget::Stdout => match stdout_target {
            RunOutputTarget::Capture => Stdio::from(
                stdout_file
                    .as_ref()
                    .expect("stdout capture file should exist")
                    .try_clone()
                    .map_err(|err| {
                        stone_error(
                            "run",
                            format!("failed to clone stdout file for stderr: {err}"),
                        )
                    })?,
            ),
            RunOutputTarget::Suppress => Stdio::null(),
            RunOutputTarget::Stdout => unreachable!("stdout cannot target stdout"),
        },
    };
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdout(stdout_stdio)
        .stderr(stderr_stdio);
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    for (key, value) in env_overrides {
        command.env(key, value);
    }

    let mut child = command.spawn().map_err(|err| {
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        command_spawn_error("run", &argv[0], &err)
    })?;
    drop(stdout_file);
    drop(stderr_file);

    if let Some(stdin) = stdin {
        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin
                .write_all(stdin.as_bytes())
                .map_err(|err| stone_error("run", format!("failed to write stdin: {err}")))?;
        }
    }

    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| stone_error("run", format!("failed to wait for child: {err}")))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait().map_err(|err| {
                stone_error("run", format!("failed to reap timed-out child: {err}"))
            })?;
        }
        thread::sleep(Duration::from_millis(10));
    };

    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let stdout_bytes = if stdout_target == RunOutputTarget::Capture {
        fs::read(&stdout_path).map_err(|err| io_stone_error("run", err, &stdout_path))?
    } else {
        Vec::new()
    };
    let stderr_bytes = if stderr_target == RunOutputTarget::Capture {
        fs::read(&stderr_path).map_err(|err| io_stone_error("run", err, &stderr_path))?
    } else {
        Vec::new()
    };
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);

    let (stdout_text, stdout_truncated) = lossy_limited_text(&stdout_bytes, max_stdout_bytes);
    let (stderr_text, stderr_truncated) = lossy_limited_text(&stderr_bytes, max_stderr_bytes);

    let mut truncated = Record::new();
    truncated.push("stdout", Value::bool(stdout_truncated, span));
    truncated.push("stderr", Value::bool(stderr_truncated, span));
    let mut suppressed = Record::new();
    suppressed.push(
        "stdout",
        Value::bool(stdout_target == RunOutputTarget::Suppress, span),
    );
    suppressed.push(
        "stderr",
        Value::bool(stderr_target == RunOutputTarget::Suppress, span),
    );

    let explanation = if timed_out {
        Some(run_timeout_explanation(argv, timeout, duration_ms, span))
    } else if status.success() {
        None
    } else {
        Some(external_process_failure_explanation(
            &argv,
            status,
            &stdout_text,
            &stderr_text,
            span,
        ))
    };

    let mut record = Record::new();
    record.push("ok", Value::bool(status.success() && !timed_out, span));
    record.push(
        "kind",
        Value::string(
            if timed_out {
                "timeout"
            } else if status.success() {
                "success"
            } else {
                "exec_failed"
            },
            span,
        ),
    );
    record.push(
        "exit_code",
        status
            .code()
            .map(|code| Value::int(i64::from(code), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push("duration_ms", Value::int(duration_ms, span));
    record.push("cwd", Value::string(cwd.display().to_string(), span));
    record.push(
        "argv",
        Value::list(
            argv.iter()
                .map(|arg| Value::string(arg.clone(), span))
                .collect(),
            span,
        ),
    );
    record.push("stdout", Value::string(stdout_text, span));
    record.push("stderr", Value::string(stderr_text, span));
    record.push("timed_out", Value::bool(timed_out, span));
    record.push("truncated", Value::record(truncated, span));
    record.push("suppressed", Value::record(suppressed, span));
    record.push(
        "stderr_to_stdout",
        Value::bool(stderr_target == RunOutputTarget::Stdout, span),
    );
    if let Some(runtime) = maybe_python_runtime_context(argv, cwd, env_overrides) {
        record.push("runtime", runtime);
    }
    if let Some(explanation) = explanation {
        record.push("explanation", explanation);
    }
    Ok(record)
}

#[cfg(not(target_os = "hermit"))]
#[derive(Clone, Debug, PartialEq)]
struct StoneHelperHook {
    event: String,
    family: String,
    argv0: Vec<String>,
    argv0_prefix: Vec<String>,
    handler: StoneHelperHandler,
    priority: i64,
    source: PathBuf,
}

#[cfg(not(target_os = "hermit"))]
#[derive(Clone, Debug, PartialEq)]
struct StoneHelperHandler {
    name: String,
    kind: StoneHelperHandlerKind,
}

#[cfg(not(target_os = "hermit"))]
#[derive(Clone, Debug, PartialEq)]
enum StoneHelperHandlerKind {
    StoneFunction(FunctionDef),
    PythonAfterFailure,
    NativeAfterFailure,
    MediaAfterRun,
    MlAfterRun,
    LlvmAfterRun,
    ServiceAfterRun,
    BuildAfterTimeout,
    Registered,
}

#[cfg(not(target_os = "hermit"))]
#[derive(Clone, Debug, Default, PartialEq)]
struct StoneHelperRegistry {
    hooks: Vec<StoneHelperHook>,
    family_by_argv0: HashMap<String, String>,
    family_prefix_matchers: Vec<StoneHelperFamilyMatcher>,
}

#[cfg(not(target_os = "hermit"))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct StoneHelperFamilyMatcher {
    family: String,
    argv0: Vec<String>,
    argv0_prefix: Vec<String>,
    priority: i64,
}

#[cfg(not(target_os = "hermit"))]
#[derive(Clone, Debug)]
struct StoneRunEvent<'a> {
    event: &'static str,
    family: String,
    argv: &'a [String],
    cwd: &'a Path,
    env_overrides: &'a [(String, String)],
    ok: bool,
    exit_code: Option<i64>,
    stdout: String,
    stderr: String,
    timed_out: bool,
    duration_ms: i64,
    explanation_kind: Option<String>,
}

#[cfg(not(target_os = "hermit"))]
fn stone_run_event_from_record<'a>(
    record: &Record,
    argv: &'a [String],
    cwd: &'a Path,
    env_overrides: &'a [(String, String)],
    registry: &StoneHelperRegistry,
) -> StoneRunEvent<'a> {
    let ok = record_bool(record, "ok").unwrap_or(false);
    let timed_out = record_bool(record, "timed_out").unwrap_or(false);
    let event = if timed_out {
        "run.after_timeout"
    } else if ok {
        "run.after_success"
    } else {
        "run.after_failure"
    };
    StoneRunEvent {
        event,
        family: registry.command_family(argv),
        argv,
        cwd,
        env_overrides,
        ok,
        exit_code: record_i64(record, "exit_code"),
        stdout: record_string(record, "stdout").unwrap_or_default(),
        stderr: record_string(record, "stderr").unwrap_or_default(),
        timed_out,
        duration_ms: record_i64(record, "duration_ms").unwrap_or_default(),
        explanation_kind: record_explanation_kind(record),
    }
}

#[cfg(not(target_os = "hermit"))]
impl StoneHelperRegistry {
    fn new(hooks: Vec<StoneHelperHook>) -> Self {
        let mut exact_matchers: Vec<StoneHelperFamilyMatcher> = hooks
            .iter()
            .filter(|hook| hook.family != "generic" && !hook.argv0.is_empty())
            .map(|hook| StoneHelperFamilyMatcher {
                family: hook.family.clone(),
                argv0: hook.argv0.clone(),
                argv0_prefix: Vec::new(),
                priority: hook.priority,
            })
            .collect();
        exact_matchers.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.family.cmp(&right.family))
        });
        let mut family_by_argv0 = HashMap::new();
        for matcher in exact_matchers {
            for argv0 in matcher.argv0 {
                family_by_argv0
                    .entry(argv0)
                    .or_insert(matcher.family.clone());
            }
        }

        let mut family_prefix_matchers: Vec<StoneHelperFamilyMatcher> = hooks
            .iter()
            .filter(|hook| hook.family != "generic" && !hook.argv0_prefix.is_empty())
            .map(|hook| StoneHelperFamilyMatcher {
                family: hook.family.clone(),
                argv0: Vec::new(),
                argv0_prefix: hook.argv0_prefix.clone(),
                priority: hook.priority,
            })
            .collect();
        family_prefix_matchers.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| right.max_prefix_len().cmp(&left.max_prefix_len()))
                .then_with(|| left.family.cmp(&right.family))
        });
        family_prefix_matchers.dedup();
        Self {
            hooks,
            family_by_argv0,
            family_prefix_matchers,
        }
    }

    fn command_family(&self, argv: &[String]) -> String {
        let argv0 = command_argv0(argv);
        if let Some(family) = self.family_by_argv0.get(&argv0) {
            return family.clone();
        }
        self.family_prefix_matchers
            .iter()
            .find(|matcher| matcher.matches_argv0(&argv0))
            .map(|matcher| matcher.family.clone())
            .unwrap_or_else(|| "generic".to_owned())
    }

    fn matching_hooks<'a>(&'a self, event: &StoneRunEvent<'_>) -> Vec<&'a StoneHelperHook> {
        let argv0 = command_argv0(event.argv);
        let mut hooks: Vec<&StoneHelperHook> = self
            .hooks
            .iter()
            .filter(|hook| {
                let family_matches = hook.family == event.family
                    || event.family.starts_with(&format!("{}/", hook.family))
                    || hook.family == "generic";
                let argv0_matches = (hook.argv0.is_empty() && hook.argv0_prefix.is_empty())
                    || hook.argv0.iter().any(|expected| expected == &argv0)
                    || hook
                        .argv0_prefix
                        .iter()
                        .any(|prefix| argv0.starts_with(prefix));
                hook.event == event.event && family_matches && argv0_matches
            })
            .collect();
        hooks.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.handler.name.cmp(&right.handler.name))
        });
        hooks
    }
}

#[cfg(not(target_os = "hermit"))]
impl StoneHelperFamilyMatcher {
    fn matches_argv0(&self, argv0: &str) -> bool {
        self.argv0.iter().any(|expected| expected == argv0)
            || self
                .argv0_prefix
                .iter()
                .any(|prefix| argv0.starts_with(prefix))
    }

    fn max_prefix_len(&self) -> usize {
        self.argv0_prefix.iter().map(String::len).max().unwrap_or(0)
    }
}

#[cfg(not(target_os = "hermit"))]
impl StoneHelperHandler {
    fn resolve(name: String, functions: &HashMap<String, FunctionDef>) -> Self {
        let kind = resolve_stone_helper_function(&name, functions)
            .map(StoneHelperHandlerKind::StoneFunction)
            .unwrap_or_else(|| match name.as_str() {
                "python.after_failure" | "python.pip_after_failure" => {
                    StoneHelperHandlerKind::PythonAfterFailure
                }
                "native.after_failure" => StoneHelperHandlerKind::NativeAfterFailure,
                "media.after_success" | "media.after_failure" => {
                    StoneHelperHandlerKind::MediaAfterRun
                }
                "ml.after_success" | "ml.after_failure" => StoneHelperHandlerKind::MlAfterRun,
                "llvm.after_success" | "llvm.after_failure" => StoneHelperHandlerKind::LlvmAfterRun,
                "service.after_success" | "service.after_failure" => {
                    StoneHelperHandlerKind::ServiceAfterRun
                }
                "build.after_timeout" => StoneHelperHandlerKind::BuildAfterTimeout,
                _ => StoneHelperHandlerKind::Registered,
            });
        Self { name, kind }
    }

    fn invoke(
        &self,
        evaluator: &mut Evaluator<'_>,
        hook: &StoneHelperHook,
        event: &StoneRunEvent<'_>,
        span: Span,
    ) -> Result<Vec<Value>, ShellError> {
        match &self.kind {
            StoneHelperHandlerKind::StoneFunction(function) => {
                evaluator.invoke_stone_helper_function(function, event, span)
            }
            StoneHelperHandlerKind::PythonAfterFailure => {
                Ok(python_helper_after_failure(hook, event, span)
                    .into_iter()
                    .collect())
            }
            StoneHelperHandlerKind::NativeAfterFailure => {
                Ok(native_helper_after_failure(hook, event, span)
                    .into_iter()
                    .collect())
            }
            StoneHelperHandlerKind::MediaAfterRun => Ok(media_helper_after_run(hook, event, span)
                .into_iter()
                .collect()),
            StoneHelperHandlerKind::MlAfterRun => {
                Ok(ml_helper_after_run(hook, event, span).into_iter().collect())
            }
            StoneHelperHandlerKind::LlvmAfterRun => Ok(llvm_helper_after_run(hook, event, span)
                .into_iter()
                .collect()),
            StoneHelperHandlerKind::ServiceAfterRun => {
                Ok(service_helper_after_run(hook, event, span)
                    .into_iter()
                    .collect())
            }
            StoneHelperHandlerKind::BuildAfterTimeout => {
                Ok(build_helper_after_timeout(hook, event, span)
                    .into_iter()
                    .collect())
            }
            StoneHelperHandlerKind::Registered => Ok(Vec::new()),
        }
    }
}

#[cfg(not(target_os = "hermit"))]
fn resolve_stone_helper_function(
    handler: &str,
    functions: &HashMap<String, FunctionDef>,
) -> Option<FunctionDef> {
    functions
        .get(handler)
        .cloned()
        .or_else(|| functions.get(&handler.replace('.', "_")).cloned())
}

#[cfg(not(target_os = "hermit"))]
fn stone_helper_registry(cwd: &Path) -> StoneHelperRegistry {
    let mut hooks = Vec::new();
    for dir in stone_helper_dirs(cwd) {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| matches!(path.extension().and_then(|ext| ext.to_str()), Some("stone")))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(source) = fs::read_to_string(&path) else {
                continue;
            };
            let functions = stone_helper_functions(&source);
            hooks.extend(parse_stone_helper_hooks(&source, &path, &functions));
        }
    }
    StoneHelperRegistry::new(hooks)
}

#[cfg(not(target_os = "hermit"))]
fn stone_helper_functions(source: &str) -> HashMap<String, FunctionDef> {
    let Ok(program) = crate::stone_ast::lower_source(source) else {
        return HashMap::new();
    };
    program
        .statements
        .into_iter()
        .filter_map(|statement| match statement {
            Stmt::FunctionDef(function) => Some((function.name.clone(), function)),
            _ => None,
        })
        .collect()
}

#[cfg(not(target_os = "hermit"))]
fn stone_helper_dirs(cwd: &Path) -> Vec<PathBuf> {
    if let Some(raw) = env::var_os("WAYMARK_STONE_HELPER_DIRS") {
        return env::split_paths(&raw).collect();
    }
    let mut dirs = Vec::new();
    dirs.push(cwd.join(".stone").join("helpers"));
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".stone").join("helpers"));
    }
    dirs.push(PathBuf::from("/usr/share/waymark/stone/helpers"));
    dirs
}

#[cfg(not(target_os = "hermit"))]
fn parse_stone_helper_hooks(
    source: &str,
    path: &Path,
    functions: &HashMap<String, FunctionDef>,
) -> Vec<StoneHelperHook> {
    let mut hooks = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("hook(") {
            continue;
        }
        let Some(event) = parse_hook_event(trimmed) else {
            continue;
        };
        let Some(family) = parse_named_string_arg(trimmed, "family") else {
            continue;
        };
        let Some(handler) = parse_named_string_arg(trimmed, "handler") else {
            continue;
        };
        let priority = parse_named_i64_arg(trimmed, "priority").unwrap_or(100);
        hooks.push(StoneHelperHook {
            event,
            family,
            argv0: parse_named_string_list_arg(trimmed, "argv0"),
            argv0_prefix: parse_named_string_list_arg(trimmed, "argv0_prefix"),
            handler: StoneHelperHandler::resolve(handler, functions),
            priority,
            source: path.to_path_buf(),
        });
    }
    hooks
}

#[cfg(not(target_os = "hermit"))]
fn parse_hook_event(line: &str) -> Option<String> {
    let rest = line.trim_start_matches("hook(").trim_start();
    parse_quoted_prefix(rest).map(|(value, _)| value)
}

#[cfg(not(target_os = "hermit"))]
fn parse_named_string_arg(line: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=");
    let index = line.find(&marker)?;
    parse_quoted_prefix(line[index + marker.len()..].trim_start()).map(|(value, _)| value)
}

#[cfg(not(target_os = "hermit"))]
fn parse_named_string_list_arg(line: &str, name: &str) -> Vec<String> {
    let marker = format!("{name}=");
    let Some(index) = line.find(&marker) else {
        return Vec::new();
    };
    let mut rest = line[index + marker.len()..].trim_start();
    if !rest.starts_with('[') {
        return Vec::new();
    }
    rest = &rest[1..];
    let mut values = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with(']') || rest.is_empty() {
            break;
        }
        let Some((value, next)) = parse_quoted_prefix(rest) else {
            break;
        };
        values.push(value);
        rest = next.trim_start();
        if rest.starts_with(',') {
            rest = &rest[1..];
        }
    }
    values
}

#[cfg(not(target_os = "hermit"))]
fn parse_named_i64_arg(line: &str, name: &str) -> Option<i64> {
    let marker = format!("{name}=");
    let index = line.find(&marker)?;
    let rest = line[index + marker.len()..].trim_start();
    let end = rest
        .find(|ch: char| !ch.is_ascii_digit() && ch != '-')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(not(target_os = "hermit"))]
fn parse_quoted_prefix(rest: &str) -> Option<(String, &str)> {
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let stripped = &rest[quote.len_utf8()..];
    let end = stripped.find(quote)?;
    Some((
        stripped[..end].to_owned(),
        &stripped[end + quote.len_utf8()..],
    ))
}

#[cfg(not(target_os = "hermit"))]
fn command_argv0(argv: &[String]) -> String {
    let Some(first) = argv.first() else {
        return String::new();
    };
    Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first)
        .to_owned()
}

#[cfg(not(target_os = "hermit"))]
fn python_helper_after_failure(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    span: Span,
) -> Option<Value> {
    let explanation_kind = event.explanation_kind.as_deref();
    if !matches!(
        explanation_kind,
        Some("python_module_not_found")
            | Some("python_module_attribute_missing")
            | Some("python_dependency_conflict")
            | Some("python_package_resolution_failed")
    ) {
        return None;
    }
    let python = python_install_command(event.argv);
    let mut evidence = Record::new();
    evidence.push("python", Value::string(python.clone(), span));
    evidence.push(
        "project_markers",
        Value::list(
            python_project_markers(event.cwd)
                .into_iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    evidence.push(
        "stderr_excerpt",
        Value::string(limit_helper_text(&event.stderr), span),
    );
    let mut checks = vec![vec![
        python.clone(),
        "-m".into(),
        "pip".into(),
        "check".into(),
    ]];
    let summary = match explanation_kind {
        Some("python_module_not_found") => {
            if let Some(module) = missing_python_module(&event.stderr)
                .or_else(|| missing_python_module(&event.stdout))
            {
                let package = python_package_for_module(&module);
                checks.insert(
                    0,
                    vec![
                        python.clone(),
                        "-m".into(),
                        "pip".into(),
                        "show".into(),
                        package.clone(),
                    ],
                );
                format!(
                    "Module `{module}` is missing from the Python runtime used by this command."
                )
            } else {
                "Python failed because an import was not available.".to_owned()
            }
        }
        Some("python_module_attribute_missing") => {
            if let Some((module, _attribute)) = missing_python_module_attribute(&event.stderr)
                .or_else(|| missing_python_module_attribute(&event.stdout))
            {
                checks.insert(
                    0,
                    vec![
                        python.clone(),
                        "-m".into(),
                        "pip".into(),
                        "show".into(),
                        python_package_for_module(&module),
                    ],
                );
            }
            "Python imported the module but the requested attribute is absent; this points to an API/version mismatch.".to_owned()
        }
        Some("python_dependency_conflict") => {
            "pip metadata reports an installed dependency conflict in this runtime.".to_owned()
        }
        Some("python_package_resolution_failed") => {
            "pip could not resolve a compatible set of requested package versions.".to_owned()
        }
        _ => return None,
    };
    Some(helper_observation(
        hook,
        event,
        explanation_kind.unwrap_or("python_failure"),
        summary,
        evidence,
        checks,
        span,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn native_helper_after_failure(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    span: Span,
) -> Option<Value> {
    let combined = format!("{}\n{}", event.stderr, event.stdout);
    if !combined.contains("cannot open shared object file")
        && !combined.contains("error while loading shared libraries")
        && !combined.contains("=> not found")
    {
        return None;
    }
    let mut evidence = Record::new();
    evidence.push(
        "missing_libraries",
        Value::list(
            missing_shared_libraries(&combined)
                .into_iter()
                .map(|name| Value::string(name, span))
                .collect(),
            span,
        ),
    );
    evidence.push(
        "ld_library_path",
        Value::string(
            env_override_or_current(event.env_overrides, "LD_LIBRARY_PATH"),
            span,
        ),
    );
    Some(helper_observation(
        hook,
        event,
        "native_shared_library_failure",
        "A native shared library or one of its transitive dependencies is missing.",
        evidence,
        vec![
            vec!["ldd".into(), "<failing-library-or-binary>".into()],
            vec!["file".into(), "<failing-library-or-binary>".into()],
        ],
        span,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn media_helper_after_run(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    span: Span,
) -> Option<Value> {
    let media_paths = media_candidate_paths(event);
    if media_paths.is_empty() && event.ok {
        return None;
    }
    let mut evidence = Record::new();
    evidence.push(
        "candidate_paths",
        Value::list(
            media_paths
                .iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    evidence.push(
        "stdout_excerpt",
        Value::string(limit_helper_text(&event.stdout), span),
    );
    evidence.push(
        "stderr_excerpt",
        Value::string(limit_helper_text(&event.stderr), span),
    );
    let mut checks = Vec::new();
    for path in &media_paths {
        checks.push(vec![
            "ffprobe".into(),
            "-v".into(),
            "error".into(),
            "-show_format".into(),
            "-show_streams".into(),
            "-of".into(),
            "json".into(),
            path.display().to_string(),
        ]);
    }
    Some(helper_observation(
        hook,
        event,
        "media_probe_available",
        "Media command completed; validate duration, streams, codecs, and container metadata before finalizing.",
        evidence,
        checks,
        span,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn ml_helper_after_run(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    span: Span,
) -> Option<Value> {
    let combined = format!(
        "{}\n{}\n{}",
        event.argv.join(" "),
        event.stdout,
        event.stderr
    );
    if !combined.contains("triton")
        && !combined.contains("torch")
        && !combined.contains("TRITON_INTERPRET")
        && !combined.contains("numpy")
    {
        return None;
    }
    let mut evidence = Record::new();
    evidence.push(
        "mentions_triton_interpret",
        Value::bool(
            combined.contains("TRITON_INTERPRET=1") || combined.contains("TRITON_INTERPRET"),
            span,
        ),
    );
    evidence.push(
        "output_excerpt",
        Value::string(limit_helper_text(&combined), span),
    );
    Some(helper_observation(
        hook,
        event,
        "ml_runtime_probe_available",
        "ML/Triton command evidence is present; small interpreter-mode checks may miss scale-sensitive or timeout-sensitive failures.",
        evidence,
        vec![
            vec!["python3".into(), "-c".into(), "import torch, triton, numpy; print(torch.__version__); print(triton.__version__)".into()],
        ],
        span,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn llvm_helper_after_run(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    span: Span,
) -> Option<Value> {
    let paths = llvm_candidate_paths(event);
    if paths.is_empty() && event.ok {
        return None;
    }
    let mut evidence = Record::new();
    evidence.push(
        "candidate_paths",
        Value::list(
            paths
                .iter()
                .map(|path| Value::string(path.display().to_string(), span))
                .collect(),
            span,
        ),
    );
    evidence.push(
        "output_mentions_debug_metadata",
        Value::bool(
            event.stdout.contains("!llvm.dbg.cu")
                || event.stderr.contains("!llvm.dbg.cu")
                || event.stdout.contains("DICompileUnit")
                || event.stderr.contains("DICompileUnit"),
            span,
        ),
    );
    let checks = paths
        .iter()
        .flat_map(|path| {
            [
                vec!["file".into(), path.display().to_string()],
                vec![
                    "llvm-dis".into(),
                    path.display().to_string(),
                    "-o".into(),
                    "-".into(),
                ],
            ]
        })
        .collect();
    Some(helper_observation(
        hook,
        event,
        "llvm_ir_probe_available",
        "LLVM command evidence is present; inspect bitcode/textual IR and debug metadata markers explicitly.",
        evidence,
        checks,
        span,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn service_helper_after_run(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    span: Span,
) -> Option<Value> {
    let combined = format!(
        "{}\n{}\n{}",
        event.argv.join(" "),
        event.stdout,
        event.stderr
    );
    if !combined.contains("grpc")
        && !combined.contains("server")
        && !combined.contains("daemon")
        && !combined.contains("port")
    {
        return None;
    }
    let mut evidence = Record::new();
    evidence.push(
        "output_excerpt",
        Value::string(limit_helper_text(&combined), span),
    );
    Some(helper_observation(
        hook,
        event,
        "service_probe_available",
        "Service evidence is present; validate readiness and protocol behavior from a fresh client process.",
        evidence,
        vec![
            vec!["daemon_status".into(), "<daemon-handle>".into()],
            vec!["python3".into(), "<fresh-client-probe.py>".into()],
        ],
        span,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn build_helper_after_timeout(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    span: Span,
) -> Option<Value> {
    if !event.timed_out {
        return None;
    }
    let mut evidence = Record::new();
    evidence.push("duration_ms", Value::int(event.duration_ms, span));
    evidence.push(
        "stdout_tail",
        Value::string(tail_helper_text(&event.stdout), span),
    );
    evidence.push(
        "stderr_tail",
        Value::string(tail_helper_text(&event.stderr), span),
    );
    Some(helper_observation(
        hook,
        event,
        "build_timeout",
        "The command timed out; inspect partial output and rerun with a larger timeout or a narrower build target.",
        evidence,
        vec![vec![
            "run".into(),
            "<same-argv>".into(),
            "timeout_ms=600000".into(),
        ]],
        span,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn helper_observation(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    kind: impl Into<String>,
    summary: impl Into<String>,
    evidence: Record,
    next_checks: Vec<Vec<String>>,
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("helper", Value::string(hook.handler.name.clone(), span));
    record.push("event", Value::string(event.event.to_owned(), span));
    record.push("family", Value::string(event.family.clone(), span));
    record.push("kind", Value::string(kind.into(), span));
    record.push("summary", Value::string(summary.into(), span));
    if let Some(exit_code) = event.exit_code {
        record.push("exit_code", Value::int(exit_code, span));
    }
    record.push(
        "source",
        Value::string(hook.source.display().to_string(), span),
    );
    record.push("evidence", Value::record(evidence, span));
    record.push(
        "next_checks",
        Value::list(
            next_checks
                .into_iter()
                .map(|argv| {
                    Value::list(
                        argv.into_iter()
                            .map(|arg| Value::string(arg, span))
                            .collect(),
                        span,
                    )
                })
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn helper_error_observation(
    hook: &StoneHelperHook,
    event: &StoneRunEvent<'_>,
    err: ShellError,
    span: Span,
) -> Value {
    let mut evidence = Record::new();
    evidence.push("error", Value::string(err.to_string(), span));
    evidence.push(
        "source",
        Value::string(hook.source.display().to_string(), span),
    );
    helper_observation(
        hook,
        event,
        "helper_error",
        format!(
            "Helper `{}` failed while handling {}.",
            hook.handler.name, hook.event
        ),
        evidence,
        Vec::<Vec<String>>::new(),
        span,
    )
}

#[cfg(not(target_os = "hermit"))]
fn stone_run_event_value(event: &StoneRunEvent<'_>, span: Span) -> Value {
    let mut record = Record::new();
    record.push("event", Value::string(event.event.to_owned(), span));
    record.push("family", Value::string(event.family.clone(), span));
    record.push(
        "argv",
        Value::list(
            event
                .argv
                .iter()
                .map(|arg| Value::string(arg.clone(), span))
                .collect(),
            span,
        ),
    );
    record.push("cwd", Value::string(event.cwd.display().to_string(), span));
    record.push(
        "env",
        Value::record(
            event
                .env_overrides
                .iter()
                .map(|(key, value)| (key.clone(), Value::string(value.clone(), span)))
                .collect(),
            span,
        ),
    );
    record.push("ok", Value::bool(event.ok, span));
    record.push(
        "exit_code",
        event
            .exit_code
            .map(|code| Value::int(code, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push("stdout", Value::string(event.stdout.clone(), span));
    record.push("stderr", Value::string(event.stderr.clone(), span));
    record.push("timed_out", Value::bool(event.timed_out, span));
    record.push("duration_ms", Value::int(event.duration_ms, span));
    record.push(
        "explanation_kind",
        event
            .explanation_kind
            .as_ref()
            .map(|kind| Value::string(kind.clone(), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn stone_helper_observations_from_value(value: Value) -> Result<Vec<Value>, ShellError> {
    match value {
        Value::Nothing { .. } => Ok(Vec::new()),
        Value::List { vals, .. } => Ok(vals),
        Value::Record { .. } => Ok(vec![value]),
        other => Err(stone_error(
            "helper",
            format!(
                "helper callback must return a record, list of records, or None; got {}",
                other.get_type()
            ),
        )),
    }
}

#[cfg(not(target_os = "hermit"))]
fn attach_service_helper_observation(
    record: &mut Record,
    event: &'static str,
    handler: &str,
    summary: &str,
    next_checks: &[&str],
    span: Span,
) {
    let mut evidence = Record::new();
    evidence.push(
        "ok",
        Value::bool(record_bool(record, "ok").unwrap_or(false), span),
    );
    if let Some(pid) = record_i64(record, "pid") {
        evidence.push("pid", Value::int(pid, span));
    }
    if let Some(port) = record_i64(record, "port") {
        evidence.push("port", Value::int(port, span));
    }
    let hook = StoneHelperHook {
        event: event.to_owned(),
        family: "service".to_owned(),
        argv0: Vec::new(),
        argv0_prefix: Vec::new(),
        handler: StoneHelperHandler::resolve(handler.to_owned(), &HashMap::new()),
        priority: 100,
        source: PathBuf::from("<builtin-service-lifecycle>"),
    };
    let synthetic_event = StoneRunEvent {
        event,
        family: "service".to_owned(),
        argv: &[],
        cwd: Path::new("."),
        env_overrides: &[],
        ok: record_bool(record, "ok").unwrap_or(false),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: event == "run.after_timeout",
        duration_ms: record_i64(record, "duration_ms").unwrap_or_default(),
        explanation_kind: None,
    };
    let observation = helper_observation(
        &hook,
        &synthetic_event,
        "service_lifecycle_probe",
        summary,
        evidence,
        next_checks
            .iter()
            .map(|check| vec![(*check).to_owned()])
            .collect(),
        span,
    );
    record.push("helpers", Value::list(vec![observation], span));
}

#[cfg(not(target_os = "hermit"))]
fn media_candidate_paths(event: &StoneRunEvent<'_>) -> Vec<PathBuf> {
    event
        .argv
        .iter()
        .filter(|arg| {
            let lower = arg.to_ascii_lowercase();
            matches!(
                Path::new(&lower).extension().and_then(|ext| ext.to_str()),
                Some("mp4" | "mkv" | "webm" | "mov" | "mp3" | "wav" | "flac" | "aac")
            )
        })
        .map(|arg| absolutize_arg_path(event.cwd, arg))
        .collect()
}

#[cfg(not(target_os = "hermit"))]
fn llvm_candidate_paths(event: &StoneRunEvent<'_>) -> Vec<PathBuf> {
    event
        .argv
        .iter()
        .filter(|arg| {
            matches!(
                Path::new(arg).extension().and_then(|ext| ext.to_str()),
                Some("ll" | "bc" | "o" | "a")
            )
        })
        .map(|arg| absolutize_arg_path(event.cwd, arg))
        .collect()
}

#[cfg(not(target_os = "hermit"))]
fn absolutize_arg_path(cwd: &Path, arg: &str) -> PathBuf {
    let path = Path::new(arg);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

#[cfg(not(target_os = "hermit"))]
fn missing_shared_libraries(text: &str) -> Vec<String> {
    let mut libs = Vec::new();
    for line in text.lines() {
        if let Some(index) = line.find("=> not found") {
            let name = line[..index].trim();
            if !name.is_empty() {
                libs.push(name.to_owned());
            }
        }
        if line.contains("cannot open shared object file") {
            let candidate = line
                .split(':')
                .find(|part| part.trim().contains(".so"))
                .map(str::trim);
            if let Some(name) = candidate {
                libs.push(name.to_owned());
            }
        }
        if let Some(index) = line.find("error while loading shared libraries:") {
            let rest = &line[index + "error while loading shared libraries:".len()..];
            if let Some(name) = rest
                .split(':')
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                libs.push(name.to_owned());
            }
        }
    }
    libs.sort();
    libs.dedup();
    libs
}

#[cfg(not(target_os = "hermit"))]
fn env_override_or_current(env_overrides: &[(String, String)], key: &str) -> String {
    env_overrides
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
        .or_else(|| env::var(key).ok())
        .unwrap_or_default()
}

#[cfg(not(target_os = "hermit"))]
fn limit_helper_text(text: &str) -> String {
    if text.len() <= STONE_HELPER_OUTPUT_LIMIT {
        text.to_owned()
    } else {
        format!("{}...", &text[..STONE_HELPER_OUTPUT_LIMIT])
    }
}

#[cfg(not(target_os = "hermit"))]
fn tail_helper_text(text: &str) -> String {
    if text.len() <= STONE_HELPER_OUTPUT_LIMIT {
        text.to_owned()
    } else {
        text[text.len() - STONE_HELPER_OUTPUT_LIMIT..].to_owned()
    }
}

#[cfg(not(target_os = "hermit"))]
fn record_bool(record: &Record, field: &str) -> Option<bool> {
    match record.get(field) {
        Some(Value::Bool { val, .. }) => Some(*val),
        _ => None,
    }
}

#[cfg(not(target_os = "hermit"))]
fn record_i64(record: &Record, field: &str) -> Option<i64> {
    match record.get(field) {
        Some(Value::Int { val, .. }) => Some(*val),
        _ => None,
    }
}

#[cfg(not(target_os = "hermit"))]
fn record_string(record: &Record, field: &str) -> Option<String> {
    match record.get(field) {
        Some(Value::String { val, .. }) | Some(Value::Glob { val, .. }) => Some(val.clone()),
        _ => None,
    }
}

#[cfg(not(target_os = "hermit"))]
fn record_explanation_kind(record: &Record) -> Option<String> {
    let Some(Value::Record { val, .. }) = record.get("explanation") else {
        return None;
    };
    match val.get("kind") {
        Some(Value::String { val, .. }) | Some(Value::Glob { val, .. }) => Some(val.clone()),
        _ => None,
    }
}

#[cfg(not(target_os = "hermit"))]
fn start_posix_daemon(
    argv: &[String],
    cwd: &Path,
    env_overrides: &[(String, String)],
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<Value, ShellError> {
    let span = Span::unknown();
    validate_command_cwd("start_daemon", cwd)?;
    let stdout_file = create_command_output_file("start_daemon", "stdout", stdout_path)?;
    let stderr_file = create_command_output_file("start_daemon", "stderr", stderr_path)?;

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    for (key, value) in env_overrides {
        command.env(key, value);
    }
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = command
        .spawn()
        .map_err(|err| command_spawn_error("start_daemon", &argv[0], &err))?;
    let pid = child.id();
    std::mem::forget(child);

    let mut record = Record::new();
    record.push("ok", Value::bool(true, span));
    record.push("kind", Value::string("started", span));
    record.push("pid", Value::int(i64::from(pid), span));
    record.push("cwd", Value::string(cwd.display().to_string(), span));
    record.push(
        "argv",
        Value::list(
            argv.iter()
                .map(|arg| Value::string(arg.clone(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "stdout_path",
        Value::string(stdout_path.display().to_string(), span),
    );
    record.push(
        "stderr_path",
        Value::string(stderr_path.display().to_string(), span),
    );
    record.push(
        "explanation",
        daemon_explanation(
            "daemon_started",
            "Stone spawned the process without waiting for it; use wait_port() and daemon_status() before final validation.",
            &[
                "Use run() for commands that should finish.",
                "Use start_daemon() for servers or services that tests must find still running later.",
                "Inspect stdout_path and stderr_path if the daemon exits early.",
            ],
            span,
        ),
    );
    attach_service_helper_observation(
        &mut record,
        "run.after_success",
        "service.start_daemon.after_success",
        "Stone started a long-lived daemon; validate it from a fresh client before finalizing.",
        &[
            "Use wait_port() for the expected service port.",
            "Use daemon_status() with the daemon handle to confirm the process survived.",
            "Inspect stdout_path and stderr_path if validation fails.",
        ],
        span,
    );
    Ok(Value::record(record, span))
}

#[cfg(not(target_os = "hermit"))]
fn daemon_status_record(
    pid: u32,
    port: Option<u16>,
    host: &str,
    log_path: Option<&Path>,
    max_log_bytes: usize,
) -> Value {
    let span = Span::unknown();
    let running = process_alive(pid);
    let port_open = port.map(|port| tcp_port_open(host, port, Duration::from_millis(200)));
    let ok = running && port_open.unwrap_or(true);
    let mut record = Record::new();
    record.push("ok", Value::bool(ok, span));
    record.push(
        "kind",
        Value::string(if ok { "running" } else { "not_ready" }, span),
    );
    record.push("pid", Value::int(i64::from(pid), span));
    record.push("running", Value::bool(running, span));
    if let Some(port) = port {
        record.push("host", Value::string(host.to_owned(), span));
        record.push("port", Value::int(i64::from(port), span));
        record.push("port_open", Value::bool(port_open.unwrap_or(false), span));
    }
    if let Some(log_path) = log_path {
        record.push(
            "log_path",
            Value::string(log_path.display().to_string(), span),
        );
        match read_log_tail(log_path, max_log_bytes) {
            Ok((tail, truncated)) => {
                record.push("log_tail", Value::string(tail, span));
                record.push("log_truncated", Value::bool(truncated, span));
            }
            Err(err) => {
                record.push("log_error", Value::string(err.to_string(), span));
            }
        }
    }
    if !ok {
        record.push(
            "explanation",
            daemon_explanation(
                "daemon_not_ready",
                "The daemon is not currently ready for validation.",
                &[
                    "If running is false, inspect the daemon log and restart it after fixing the command.",
                    "If port_open is false, wait for the service port or check that the daemon binds the expected host and port.",
                    "Use start_daemon() instead of shell backgrounding through run() for long-lived services.",
                ],
                span,
            ),
        );
    }
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn run_timeout_explanation(
    argv: &[String],
    timeout: Duration,
    duration_ms: i64,
    span: Span,
) -> Value {
    let timeout_ms = i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX);
    let command = argv.join(" ");
    let mut record = Record::new();
    record.push("kind", Value::string("timeout", span));
    record.push(
        "summary",
        Value::string(
            format!(
                "Stone stopped waiting for the external process after {timeout_ms} ms and killed it."
            ),
            span,
        ),
    );
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push("timeout_ms", Value::int(timeout_ms, span));
    record.push("duration_ms", Value::int(duration_ms, span));
    record.push(
        "argv",
        Value::list(
            argv.iter()
                .map(|arg| Value::string(arg.clone(), span))
                .collect(),
            span,
        ),
    );
    record.push("command", Value::string(command, span));
    record.push(
        "next_steps",
        Value::list(
            [
                "Inspect stdout and stderr for partial progress before retrying; a timed-out command may have left files or a partial checkout behind.",
                "If the command is expected to take longer, rerun it with a larger timeout_ms, for example run(argv, timeout_ms=600000).",
                "If the command should be quick, narrow the command or fix the reported stall before retrying.",
                "For services that should keep running, use start_daemon() instead of run().",
            ]
            .into_iter()
            .map(|step| Value::string(step, span))
            .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn stop_daemon_record(pid: u32, timeout: Duration) -> Value {
    let span = Span::unknown();
    let existed_before = process_alive(pid);
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
    let started = Instant::now();
    while process_alive(pid) && started.elapsed() < timeout {
        thread::sleep(Duration::from_millis(50));
    }
    let killed = if process_alive(pid) {
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status();
        thread::sleep(Duration::from_millis(50));
        true
    } else {
        false
    };
    let stopped = !process_alive(pid);
    let mut record = Record::new();
    record.push("ok", Value::bool(stopped, span));
    record.push(
        "kind",
        Value::string(if stopped { "stopped" } else { "still_running" }, span),
    );
    record.push("pid", Value::int(i64::from(pid), span));
    record.push("existed_before", Value::bool(existed_before, span));
    record.push("sent_kill", Value::bool(killed, span));
    if !stopped {
        record.push(
            "explanation",
            daemon_explanation(
                "daemon_stop_failed",
                "Stone sent termination signals but the process still appears to be running.",
                &[
                    "Inspect the process tree to see whether the service re-spawned or ignored signals.",
                    "Stop child processes explicitly if the daemon manager forked additional workers.",
                ],
                span,
            ),
        );
    }
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn wait_port_record(host: &str, port: u16, timeout: Duration) -> Value {
    let span = Span::unknown();
    let started = Instant::now();
    let mut open = false;
    while started.elapsed() < timeout {
        if tcp_port_open(host, port, Duration::from_millis(200)) {
            open = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let mut record = Record::new();
    record.push("ok", Value::bool(open, span));
    record.push(
        "kind",
        Value::string(if open { "open" } else { "timeout" }, span),
    );
    record.push("host", Value::string(host.to_owned(), span));
    record.push("port", Value::int(i64::from(port), span));
    record.push("duration_ms", Value::int(duration_ms, span));
    if !open {
        record.push(
            "explanation",
            daemon_explanation(
                "port_wait_timeout",
                format!("Port {host}:{port} did not accept TCP connections before the timeout."),
                &[
                    "Confirm the daemon is still running with daemon_status().",
                    "Check that the service binds the expected host and port.",
                    "Inspect daemon logs for startup errors.",
                ],
                span,
            ),
        );
    }
    attach_service_helper_observation(
        &mut record,
        if open {
            "run.after_success"
        } else {
            "run.after_timeout"
        },
        "service.wait_port.after_result",
        if open {
            "The TCP port accepted a connection; validate protocol behavior with a fresh client next."
        } else {
            "The TCP port did not become ready before the wait timeout."
        },
        &[
            "Confirm the daemon is still running with daemon_status().",
            "Run a fresh client process against the service, not only in-process checks.",
            "For gRPC tasks, verify the generated client can complete a real RPC handshake.",
        ],
        span,
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn process_alive(pid: u32) -> bool {
    let stat_path = Path::new("/proc").join(pid.to_string()).join("stat");
    let Ok(stat) = fs::read_to_string(stat_path) else {
        return false;
    };
    let Some(end_comm) = stat.rfind(") ") else {
        return true;
    };
    !stat[end_comm + 2..].starts_with("Z ")
}

#[cfg(not(target_os = "hermit"))]
fn tcp_port_open(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(mut addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, port)) else {
        return false;
    };
    addrs.any(|addr| std::net::TcpStream::connect_timeout(&addr, timeout).is_ok())
}

#[cfg(not(target_os = "hermit"))]
fn read_log_tail(path: &Path, max_bytes: usize) -> Result<(String, bool), std::io::Error> {
    let bytes = fs::read(path)?;
    let truncated = bytes.len() > max_bytes;
    let start = if truncated {
        bytes.len() - max_bytes
    } else {
        0
    };
    Ok((
        String::from_utf8_lossy(&bytes[start..]).into_owned(),
        truncated,
    ))
}

#[cfg(not(target_os = "hermit"))]
fn daemon_explanation(
    kind: &str,
    summary: impl Into<String>,
    next_steps: &[&str],
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push("summary", Value::string(summary.into(), span));
    record.push(
        "scope",
        Value::string("external daemon; Stone transport succeeded", span),
    );
    record.push(
        "next_steps",
        Value::list(
            next_steps
                .iter()
                .map(|step| Value::string((*step).to_owned(), span))
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn run_failure_explanation(kind: &str, summary: String, next_steps: &[&str], span: Span) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push("summary", Value::string(summary, span));
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push(
        "next_steps",
        Value::list(
            next_steps
                .iter()
                .map(|step| Value::string((*step).to_owned(), span))
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn external_process_failure_explanation(
    argv: &[String],
    status: ExitStatus,
    stdout: &str,
    stderr: &str,
    span: Span,
) -> Value {
    let exit_text = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        let mut next_steps = vec![
            "The command produced no stdout or stderr, so Stone cannot infer the program-internal cause from captured output.".to_owned(),
            "Rerun the same command with that program's verbose/debug/log option, or change the invoked script to write diagnostics to a file.".to_owned(),
            "If you do not know the option, inspect the program help with --help, -h, or man before choosing a retry.".to_owned(),
        ];
        if let Some(hint) = verbose_hint_for_command(argv.first().map(String::as_str).unwrap_or(""))
        {
            next_steps.push(hint.to_owned());
        }
        return run_failure_explanation_owned(
            "external_process_no_clear_error",
            format!(
                "Stone successfully ran the external process, but it exited with code {exit_text} and produced no clear error message."
            ),
            &next_steps,
            span,
        );
    }
    if let Some(missing) = missing_python_module(stderr).or_else(|| missing_python_module(stdout)) {
        return python_module_missing_explanation(argv, status, &missing, span);
    }
    if let Some((module, attribute)) =
        missing_python_module_attribute(stderr).or_else(|| missing_python_module_attribute(stdout))
    {
        return python_module_attribute_missing_explanation(
            argv, status, &module, &attribute, span,
        );
    }
    if let Some(conflict) = pip_check_conflict(stderr).or_else(|| pip_check_conflict(stdout)) {
        return python_dependency_conflict_explanation(argv, status, &conflict, span);
    }
    if pip_resolution_failed(stderr) || pip_resolution_failed(stdout) {
        return python_package_resolution_failed_explanation(argv, status, stdout, stderr, span);
    }
    run_failure_explanation(
        "external_process_exit",
        format!(
            "Stone successfully ran the external process, but it exited with code {exit_text}."
        ),
        &[
            "Treat stdout and stderr as feedback from the process, test runner, or tool.",
            "Fix the reported issue and rerun the command if this was validation.",
            "If the nonzero exit is expected, include that reason in the final summary.",
        ],
        span,
    )
}

#[cfg(not(target_os = "hermit"))]
struct PipDependencyConflict {
    dependent: String,
    requirement: String,
    installed: String,
}

#[cfg(not(target_os = "hermit"))]
fn python_dependency_conflict_explanation(
    argv: &[String],
    status: ExitStatus,
    conflict: &PipDependencyConflict,
    span: Span,
) -> Value {
    let exit_text = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let python = python_install_command(argv);
    let installed_package = conflict
        .installed
        .split_whitespace()
        .next()
        .unwrap_or(conflict.installed.as_str());
    let mut record = Record::new();
    record.push("kind", Value::string("python_dependency_conflict", span));
    record.push(
        "summary",
        Value::string(
            format!(
                "Python package metadata reports a dependency conflict after exit code {exit_text}: `{}` requires `{}`, but `{}` is installed.",
                conflict.dependent, conflict.requirement, conflict.installed
            ),
            span,
        ),
    );
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push("dependent", Value::string(conflict.dependent.clone(), span));
    record.push(
        "requirement",
        Value::string(conflict.requirement.clone(), span),
    );
    record.push("installed", Value::string(conflict.installed.clone(), span));
    record.push(
        "inspect_argv",
        Value::list(
            [
                vec![
                    python.clone(),
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "check".to_owned(),
                ],
                vec![
                    python.clone(),
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "show".to_owned(),
                    conflict.dependent.clone(),
                    installed_package.to_owned(),
                ],
            ]
            .into_iter()
            .map(|argv| {
                Value::list(
                    argv.into_iter()
                        .map(|arg| Value::string(arg, span))
                        .collect(),
                    span,
                )
            })
            .collect(),
            span,
        ),
    );
    record.push(
        "next_steps",
        Value::list(
            [
                format!(
                    "Run pip's consistency check in the same runtime: run([\"{python}\", \"-m\", \"pip\", \"check\"])."
                ),
                "Inspect the packages named in the conflict with pip show and compare them against project requirement files.".to_owned(),
                "Avoid changing versions blindly; choose versions that satisfy the project and the reported requirement together.".to_owned(),
            ]
            .into_iter()
            .map(|step| Value::string(step, span))
            .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn python_package_resolution_failed_explanation(
    argv: &[String],
    status: ExitStatus,
    stdout: &str,
    stderr: &str,
    span: Span,
) -> Value {
    let exit_text = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let python = python_install_command(argv);
    let mut record = Record::new();
    record.push(
        "kind",
        Value::string("python_package_resolution_failed", span),
    );
    record.push(
        "summary",
        Value::string(
            format!(
                "pip exited with code {exit_text} because it could not resolve compatible package versions."
            ),
            span,
        ),
    );
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push(
        "requested",
        Value::list(
            pip_requested_packages(argv)
                .into_iter()
                .map(|package| Value::string(package, span))
                .collect(),
            span,
        ),
    );
    record.push(
        "evidence",
        Value::string(pip_resolution_evidence(stdout, stderr), span),
    );
    record.push(
        "inspect_argv",
        Value::list(
            [
                vec![
                    python.clone(),
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "check".to_owned(),
                ],
                vec![
                    python.clone(),
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "debug".to_owned(),
                ],
            ]
            .into_iter()
            .map(|argv| {
                Value::list(
                    argv.into_iter()
                        .map(|arg| Value::string(arg, span))
                        .collect(),
                    span,
                )
            })
            .collect(),
            span,
        ),
    );
    record.push(
        "next_steps",
        Value::list(
            [
                format!(
                    "Inspect the resolver output and the requested packages; then run([\"{python}\", \"-m\", \"pip\", \"check\"]) to see the currently installed dependency state."
                ),
                "Inspect nearby pyproject.toml, setup.py, setup.cfg, requirements*.txt, or lock files for pinned or incompatible requirements.".to_owned(),
                "Do not retry by randomly upgrading or downgrading packages; choose versions that satisfy the conflicting requirements.".to_owned(),
            ]
            .into_iter()
            .map(|step| Value::string(step, span))
            .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn python_module_attribute_missing_explanation(
    argv: &[String],
    status: ExitStatus,
    module: &str,
    attribute: &str,
    span: Span,
) -> Value {
    let exit_text = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let package = python_package_for_module(module);
    let python = python_install_command(argv);
    let mut record = Record::new();
    record.push(
        "kind",
        Value::string("python_module_attribute_missing", span),
    );
    record.push(
        "summary",
        Value::string(
            format!(
                "Python exited with code {exit_text} because module `{module}` imported successfully but does not expose attribute `{attribute}`."
            ),
            span,
        ),
    );
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push("module", Value::string(module.to_owned(), span));
    record.push("attribute", Value::string(attribute.to_owned(), span));
    record.push("package", Value::string(package.clone(), span));
    record.push(
        "inspect_argv",
        Value::list(
            [
                vec![
                    python.clone(),
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "show".to_owned(),
                    package.clone(),
                ],
                vec![
                    python.clone(),
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "check".to_owned(),
                ],
            ]
            .into_iter()
            .map(|argv| {
                Value::list(
                    argv.into_iter()
                        .map(|arg| Value::string(arg, span))
                        .collect(),
                    span,
                )
            })
            .collect(),
            span,
        ),
    );
    record.push(
        "next_steps",
        Value::list(
            [
                format!(
                    "This is usually a package API/version mismatch, not a missing import; inspect the installed versions with run([\"{python}\", \"-m\", \"pip\", \"show\", \"{package}\"]) and run([\"{python}\", \"-m\", \"pip\", \"check\"])."
                ),
                "Verify the failing package is installed in the same Python runtime shown in the runtime block.".to_owned(),
                "Inspect nearby pyproject.toml, setup.py, setup.cfg, requirements*.txt, or lock files for the intended dependency versions.".to_owned(),
                "Avoid blindly upgrading or downgrading packages until you know which package requires the missing attribute.".to_owned(),
            ]
            .into_iter()
            .map(|step| Value::string(step, span))
            .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn python_module_missing_explanation(
    argv: &[String],
    status: ExitStatus,
    missing: &str,
    span: Span,
) -> Value {
    let exit_text = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let package = python_package_for_module(missing);
    let python = python_install_command(argv);
    let mut record = Record::new();
    record.push("kind", Value::string("python_module_not_found", span));
    record.push(
        "summary",
        Value::string(
            format!(
                "Python exited with code {exit_text} because module `{missing}` is not importable."
            ),
            span,
        ),
    );
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push("module", Value::string(missing.to_owned(), span));
    record.push("package", Value::string(package.clone(), span));
    record.push(
        "install_argv",
        Value::list(
            [python.as_str(), "-m", "pip", "install", package.as_str()]
                .iter()
                .map(|arg| Value::string((*arg).to_owned(), span))
                .collect(),
            span,
        ),
    );
    record.push(
        "next_steps",
        Value::list(
            [
                format!(
                    "Install the missing package into the same Python runtime, for example run([\"{python}\", \"-m\", \"pip\", \"install\", \"{package}\"])."
                ),
                "If pip is unavailable in this runtime, inspect the runtime block and choose the intended Python environment before installing.".to_owned(),
                "Rerun the original Python command after the module is available.".to_owned(),
            ]
            .into_iter()
            .map(|step| Value::string(step, span))
            .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn pip_check_conflict(text: &str) -> Option<PipDependencyConflict> {
    for line in text.lines() {
        let Some(has_requirement) = line.find(" has requirement ") else {
            continue;
        };
        let dependent = line[..has_requirement].trim();
        let rest = &line[has_requirement + " has requirement ".len()..];
        let Some(but_you_have) = rest.find(", but you have ") else {
            continue;
        };
        let requirement = rest[..but_you_have].trim();
        let installed = rest[but_you_have + ", but you have ".len()..]
            .trim()
            .trim_end_matches('.');
        if dependent.is_empty() || requirement.is_empty() || installed.is_empty() {
            continue;
        }
        return Some(PipDependencyConflict {
            dependent: dependent.to_owned(),
            requirement: requirement.to_owned(),
            installed: installed.to_owned(),
        });
    }
    None
}

#[cfg(not(target_os = "hermit"))]
fn pip_resolution_failed(text: &str) -> bool {
    text.contains("ResolutionImpossible")
        || text.contains("Cannot install")
            && text.contains("because these package versions have conflicting dependencies")
        || text.contains("The conflict is caused by:")
        || text.contains("because these package versions have incompatible dependencies")
}

#[cfg(not(target_os = "hermit"))]
fn pip_requested_packages(argv: &[String]) -> Vec<String> {
    let mut requested = Vec::new();
    let Some(pip_index) = argv
        .windows(3)
        .position(|window| window == ["python3", "-m", "pip"])
        .map(|index| index + 3)
        .or_else(|| {
            argv.iter()
                .position(|arg| {
                    Path::new(arg).file_name().and_then(|name| name.to_str()) == Some("pip")
                })
                .map(|index| index + 1)
        })
    else {
        return requested;
    };
    let Some(command) = argv.get(pip_index) else {
        return requested;
    };
    if command != "install" {
        return requested;
    }
    let mut skip_next = false;
    for arg in argv.iter().skip(pip_index + 1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(
            arg.as_str(),
            "-r" | "--requirement"
                | "-c"
                | "--constraint"
                | "-i"
                | "--index-url"
                | "--extra-index-url"
                | "-f"
                | "--find-links"
        ) {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        requested.push(arg.clone());
    }
    requested
}

#[cfg(not(target_os = "hermit"))]
fn pip_resolution_evidence(stdout: &str, stderr: &str) -> String {
    let combined = format!("{stdout}\n{stderr}");
    let mut lines = Vec::new();
    for line in combined.lines() {
        let trimmed = line.trim();
        if trimmed.contains("Cannot install")
            || trimmed.contains("ResolutionImpossible")
            || trimmed.contains("The conflict is caused by:")
            || trimmed.starts_with("ERROR:")
        {
            lines.push(trimmed.to_owned());
        }
        if lines.len() >= 6 {
            break;
        }
    }
    if lines.is_empty() {
        "pip reported a dependency resolution failure.".to_owned()
    } else {
        lines.join("\n")
    }
}

#[cfg(not(target_os = "hermit"))]
fn missing_python_module_attribute(text: &str) -> Option<(String, String)> {
    let marker = "AttributeError: module ";
    let index = text.find(marker)?;
    parse_missing_module_attribute(&text[index + marker.len()..])
}

#[cfg(not(target_os = "hermit"))]
fn parse_missing_module_attribute(rest: &str) -> Option<(String, String)> {
    let trimmed = rest.trim_start();
    let (module, after_module) = parse_python_error_name_prefix(trimmed)?;
    let after_marker = after_module
        .trim_start()
        .strip_prefix("has no attribute ")?;
    let (attribute, _) = parse_python_error_name_prefix(after_marker.trim_start())?;
    Some((module, attribute))
}

#[cfg(not(target_os = "hermit"))]
fn missing_python_module(text: &str) -> Option<String> {
    for marker in [
        "ModuleNotFoundError: No module named ",
        "ImportError: No module named ",
    ] {
        if let Some(index) = text.find(marker) {
            let rest = &text[index + marker.len()..];
            return parse_missing_module_name(rest);
        }
    }
    None
}

#[cfg(not(target_os = "hermit"))]
fn parse_missing_module_name(rest: &str) -> Option<String> {
    let (candidate, _) = parse_python_error_name_prefix(rest)?;
    let top_level = candidate.split('.').next().unwrap_or(&candidate);
    if top_level
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        Some(top_level.to_owned())
    } else {
        None
    }
}

#[cfg(not(target_os = "hermit"))]
fn parse_python_error_name_prefix(rest: &str) -> Option<(String, &str)> {
    let trimmed = rest.trim_start();
    let (candidate, remaining) = if let Some(stripped) = trimmed
        .strip_prefix('\'')
        .or_else(|| trimmed.strip_prefix('"'))
    {
        let end = stripped.find(['\'', '"'])?;
        (&stripped[..end], &stripped[end + 1..])
    } else {
        let end = trimmed
            .find(|ch: char| ch.is_whitespace() || matches!(ch, ':' | ',' | ';'))
            .unwrap_or(trimmed.len());
        (&trimmed[..end], &trimmed[end..])
    };
    if candidate.is_empty() {
        return None;
    }
    if candidate
        .chars()
        .all(|ch| ch == '_' || ch == '.' || ch.is_ascii_alphanumeric())
    {
        Some((candidate.to_owned(), remaining))
    } else {
        None
    }
}

#[cfg(not(target_os = "hermit"))]
fn python_package_for_module(module: &str) -> String {
    let package = match module {
        "PIL" => "Pillow",
        "cv2" => "opencv-python",
        "sklearn" => "scikit-learn",
        "skimage" => "scikit-image",
        "yaml" => "PyYAML",
        "bs4" => "beautifulsoup4",
        "Crypto" => "pycryptodome",
        "lxml" => "lxml",
        "numpy" => "numpy",
        "scipy" => "scipy",
        "pandas" => "pandas",
        "matplotlib" => "matplotlib",
        "seaborn" => "seaborn",
        "statsmodels" => "statsmodels",
        "sympy" => "sympy",
        "requests" => "requests",
        "pytest" => "pytest",
        "pyarrow" => "pyarrow",
        "duckdb" => "duckdb",
        "networkx" => "networkx",
        _ => module,
    };
    package.to_owned()
}

#[cfg(not(target_os = "hermit"))]
fn python_install_command(argv: &[String]) -> String {
    let command = argv.first().map(String::as_str).unwrap_or("python3");
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    if name.starts_with("python") {
        command.to_owned()
    } else {
        "python3".to_owned()
    }
}

#[cfg(not(target_os = "hermit"))]
fn run_failure_explanation_owned(
    kind: &str,
    summary: String,
    next_steps: &[String],
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push("summary", Value::string(summary, span));
    record.push(
        "scope",
        Value::string("external process; Stone transport succeeded", span),
    );
    record.push(
        "next_steps",
        Value::list(
            next_steps
                .iter()
                .map(|step| Value::string(step.clone(), span))
                .collect(),
            span,
        ),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn verbose_hint_for_command(command: &str) -> Option<&'static str> {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    match name {
        "vim" | "vi" => Some(
            "For Vim, a useful probe is to add verbose logging such as -V1/tmp/vim.log, then read that log file.",
        ),
        "pytest" | "py.test" => Some(
            "For pytest, useful probes include -vv, -s, and --tb=long to expose test output and full tracebacks.",
        ),
        "curl" => Some("For curl, -v shows request/response connection details."),
        "make" | "gmake" => Some(
            "For make, try VERBOSE=1 or --debug when the build hides the failing command.",
        ),
        "cmake" => Some(
            "For cmake, try --debug-output, --trace, or build with --verbose depending on the failing phase.",
        ),
        "npm" => Some("For npm, try --loglevel verbose or inspect the npm debug log path it prints."),
        "python" | "python3" => Some(
            "For Python, make sure tracebacks are not swallowed; -X dev can expose additional runtime warnings.",
        ),
        "sh" | "bash" => Some(
            "For shell scripts, add set -x or explicit echo/log lines around the failing step.",
        ),
        "gcc" | "g++" | "clang" | "clang++" => {
            Some("For C/C++ compilers, -v shows toolchain, include, and linker details.")
        }
        "cargo" => Some("For cargo, -vv shows the underlying rustc commands and build-script output."),
        _ => None,
    }
}

#[cfg(not(target_os = "hermit"))]
fn lossy_limited_text(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    let truncated = bytes.len() > max_bytes;
    let end = if truncated { max_bytes } else { bytes.len() };
    (
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
        truncated,
    )
}

fn value_to_display_string(value: &Value) -> Result<String, ShellError> {
    match value {
        Value::Nothing { .. } => Ok(String::new()),
        Value::Bool { val, .. } => Ok(val.to_string()),
        Value::Int { val, .. } => Ok(val.to_string()),
        Value::Float { val, .. } => Ok(val.to_string()),
        Value::String { val, .. } | Value::Glob { val, .. } => Ok(val.clone()),
        other => serde_json::to_string(&nu_to_json_value(other))
            .map_err(|err| stone_error("str", err.to_string())),
    }
}

fn value_to_save_bytes(value: &Value) -> Result<Vec<u8>, ShellError> {
    match value {
        Value::Binary { val, .. } => Ok(val.clone()),
        Value::String { val, .. } | Value::Glob { val, .. } => Ok(val.as_bytes().to_vec()),
        other => serde_json::to_vec(&nu_to_json_value(other))
            .map_err(|err| stone_error("save", err.to_string())),
    }
}

fn parse_csv_records(text: &str, limit: Option<usize>) -> Result<Vec<Value>, ShellError> {
    let mut lines = text.lines();
    let Some(header_line) = lines.next() else {
        return Ok(Vec::new());
    };
    let headers = parse_csv_line(header_line)?;
    let mut rows = Vec::new();
    for (line_index, line) in lines.enumerate() {
        if limit.is_some_and(|limit| rows.len() >= limit) {
            break;
        }
        if line.is_empty() {
            continue;
        }
        let fields = parse_csv_line(line).map_err(|err| {
            stone_error(
                "read_csv",
                format!("line {}: {}", line_index + 2, shell_error_message(&err)),
            )
        })?;
        if fields.len() != headers.len() {
            return Err(stone_error(
                "read_csv",
                format!(
                    "line {} has {} field(s), expected {}",
                    line_index + 2,
                    fields.len(),
                    headers.len()
                ),
            ));
        }
        let mut record = Record::with_capacity(headers.len());
        for (header, field) in headers.iter().zip(fields) {
            record.push(header.clone(), Value::string(field, Span::unknown()));
        }
        rows.push(Value::record(record, Span::unknown()));
    }
    Ok(rows)
}

fn parse_csv_line(line: &str) -> Result<Vec<String>, ShellError> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => {
                in_quotes = !in_quotes;
            }
            ',' if !in_quotes => {
                fields.push(std::mem::take(&mut field));
            }
            _ => field.push(ch),
        }
    }
    if in_quotes {
        return Err(stone_error("read_csv", "unterminated quoted field"));
    }
    fields.push(field);
    Ok(fields)
}

fn jsonl_rows_from_bytes(bytes: Vec<u8>, limit: Option<usize>, source: String) -> JsonlRows {
    let bytes: Arc<[u8]> = Arc::from(bytes);
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_number = 1usize;
    for end in memchr::memchr_iter(b'\n', &bytes) {
        push_jsonl_line_range(&bytes, start, end, line_number, limit, &mut lines);
        if limit.is_some_and(|limit| lines.len() >= limit) {
            break;
        }
        start = end + 1;
        line_number += 1;
    }
    if !limit.is_some_and(|limit| lines.len() >= limit) && start < bytes.len() {
        push_jsonl_line_range(&bytes, start, bytes.len(), line_number, limit, &mut lines);
    }
    JsonlRows {
        bytes,
        lines: Arc::from(lines),
        source: Arc::from(source),
    }
}

fn push_jsonl_line_range(
    bytes: &[u8],
    start: usize,
    end: usize,
    line_number: usize,
    limit: Option<usize>,
    lines: &mut Vec<JsonLineRange>,
) {
    if limit.is_some_and(|limit| lines.len() >= limit) {
        return;
    }
    let mut line_start = start;
    let mut line_end = end;
    while line_start < line_end && bytes[line_start].is_ascii_whitespace() {
        line_start += 1;
    }
    while line_end > line_start && bytes[line_end - 1].is_ascii_whitespace() {
        line_end -= 1;
    }
    if line_start < line_end {
        lines.push(JsonLineRange {
            range: line_start..line_end,
            line_number,
        });
    }
}

fn jsonl_row_views(rows: &JsonlRows) -> Vec<RuntimeValue> {
    rows.lines
        .iter()
        .cloned()
        .map(|line| jsonl_row_view(rows, line))
        .collect()
}

fn jsonl_row_view(rows: &JsonlRows, line: JsonLineRange) -> RuntimeValue {
    RuntimeValue::JsonObjectView(JsonObjectView {
        bytes: rows.bytes.clone(),
        range: line.range,
        source: rows.source.clone(),
        line_number: line.line_number,
    })
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

fn eval_runtime_subscript(value: RuntimeValue, index: &Value) -> Result<RuntimeValue, ShellError> {
    match value {
        RuntimeValue::JsonObjectView(view) => json_object_view_subscript(&view, index),
        RuntimeValue::JsonArrayView(view) => json_array_view_subscript(&view, index),
        other => {
            let value = other.into_nu_value("subscript")?;
            eval_subscript(&value, index).map(RuntimeValue::Nu)
        }
    }
}

fn json_object_view_subscript(
    view: &JsonObjectView,
    index: &Value,
) -> Result<RuntimeValue, ShellError> {
    let key = value_to_string(index, "subscript")?;
    json_object_view_get(view, &key)?
        .ok_or_else(|| stone_error("subscript", format!("record has no key `{key}`")))
}

fn json_array_view_subscript(
    view: &JsonArrayView,
    index: &Value,
) -> Result<RuntimeValue, ShellError> {
    let Value::Int { val: index, .. } = index else {
        return Err(stone_error(
            "subscript",
            "JSON array views require integer indexes",
        ));
    };
    let values = json_array_view_iter_values(view)?;
    let index = normalize_index(*index, values.len())?;
    values
        .into_iter()
        .nth(index)
        .ok_or_else(|| stone_error("subscript", format!("list index {index} is out of range")))
}

fn eval_json_object_view_method(
    view: &JsonObjectView,
    method: &str,
    args: &[Value],
) -> Result<RuntimeValue, ShellError> {
    match method {
        "get" => {
            let [key, default] = args else {
                return Err(stone_error("get", "record.get() requires key and default"));
            };
            let key = value_to_string(key, "get")?;
            Ok(json_object_view_get(view, &key)?
                .unwrap_or_else(|| RuntimeValue::Nu(default.clone())))
        }
        "items" | "keys" | "values" => {
            let materialized = materialize_json_object_view(view)?;
            eval_record_method(&materialized, method, args).map(RuntimeValue::Nu)
        }
        other => Err(stone_error(
            other,
            format!("JSON object views do not support method `{other}`"),
        )),
    }
}

fn json_object_view_get(
    view: &JsonObjectView,
    key: &str,
) -> Result<Option<RuntimeValue>, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(None);
    };
    let absolute = (view.range.start + value_range.start)..(view.range.start + value_range.end);
    let value = &view.bytes[absolute.clone()];
    let value = trim_json_bytes(value);
    if value.starts_with(b"[") {
        return Ok(Some(RuntimeValue::JsonArrayView(JsonArrayView {
            bytes: view.bytes.clone(),
            range: absolute,
        })));
    }
    if value.starts_with(b"{") {
        return Ok(Some(RuntimeValue::JsonObjectView(JsonObjectView {
            bytes: view.bytes.clone(),
            range: absolute,
            source: view.source.clone(),
            line_number: view.line_number,
        })));
    }
    if !json_value_may_be_number(value) {
        let json = serde_json::from_slice::<serde_json::Value>(value).map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?;
        return Ok(Some(RuntimeValue::Nu(json_to_nu_value(
            json,
            Span::unknown(),
        ))));
    }
    Ok(Some(RuntimeValue::JsonScalarView(JsonScalarView {
        bytes: view.bytes.clone(),
        range: absolute,
        source: view.source.clone(),
        line_number: view.line_number,
    })))
}

fn json_object_view_get_string_default(
    view: &JsonObjectView,
    key: &str,
    default: &str,
) -> Result<String, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(default.to_owned());
    };
    let value = trim_json_bytes(&bytes[value_range]);
    json_string_bytes_to_string(value).map_err(|err| {
        stone_error(
            "json view",
            format!("{} line {}: {}", view.source, view.line_number, err),
        )
    })
}

fn json_object_view_get_f64_default(
    view: &JsonObjectView,
    key: &str,
    default: f64,
) -> Result<f64, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(default);
    };
    let value = trim_json_bytes(&bytes[value_range]);
    std::str::from_utf8(value)
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?
        .parse::<f64>()
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })
}

fn json_object_view_get_i64_default(
    view: &JsonObjectView,
    key: &str,
    default: i64,
) -> Result<i64, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(default);
    };
    let value = trim_json_bytes(&bytes[value_range]);
    std::str::from_utf8(value)
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?
        .parse::<i64>()
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })
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

fn json_number_bytes_to_f64(
    bytes: &[u8],
    source: &str,
    line_number: usize,
) -> Result<f64, ShellError> {
    std::str::from_utf8(bytes)
        .map_err(|err| stone_error("json view", format!("{source} line {line_number}: {err}")))?
        .parse::<f64>()
        .map_err(|err| stone_error("json view", format!("{source} line {line_number}: {err}")))
}

fn json_number_bytes_to_i64(
    bytes: &[u8],
    source: &str,
    line_number: usize,
) -> Result<i64, ShellError> {
    std::str::from_utf8(bytes)
        .map_err(|err| stone_error("json view", format!("{source} line {line_number}: {err}")))?
        .parse::<i64>()
        .map_err(|err| stone_error("json view", format!("{source} line {line_number}: {err}")))
}

fn json_object_view_get_array_default(
    view: &JsonObjectView,
    key: &str,
) -> Result<RuntimeValue, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(RuntimeValue::Nu(Value::list(Vec::new(), Span::unknown())));
    };
    let absolute = (view.range.start + value_range.start)..(view.range.start + value_range.end);
    let value = trim_json_bytes(&view.bytes[absolute.clone()]);
    if value.starts_with(b"[") {
        return Ok(RuntimeValue::JsonArrayView(JsonArrayView {
            bytes: view.bytes.clone(),
            range: absolute,
        }));
    }
    Err(stone_error("json view", "expected JSON array"))
}

fn runtime_value_to_string_key(value: &RuntimeValue, context: &str) -> Result<String, ShellError> {
    match value {
        RuntimeValue::Nu(value) => value_to_string(value, context),
        RuntimeValue::JsonScalarView(view) => json_scalar_view_to_string(view).map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        }),
        other => {
            let value = other.clone().into_nu_value(context)?;
            value_to_string(&value, context)
        }
    }
}

fn json_string_bytes_to_string(bytes: &[u8]) -> Result<String, String> {
    let bytes = trim_json_bytes(bytes);
    if bytes.len() < 2 || bytes.first() != Some(&b'"') || bytes.last() != Some(&b'"') {
        return Err("expected JSON string".to_owned());
    }
    let inner = &bytes[1..bytes.len() - 1];
    if inner.contains(&b'\\') {
        serde_json::from_slice::<String>(bytes).map_err(|err| err.to_string())
    } else {
        std::str::from_utf8(inner)
            .map(str::to_owned)
            .map_err(|err| err.to_string())
    }
}

fn json_string_bytes_to_cow(bytes: &[u8]) -> Result<Cow<'_, str>, String> {
    let bytes = trim_json_bytes(bytes);
    if bytes.len() < 2 || bytes.first() != Some(&b'"') || bytes.last() != Some(&b'"') {
        return Err("expected JSON string".to_owned());
    }
    let inner = &bytes[1..bytes.len() - 1];
    if inner.contains(&b'\\') {
        serde_json::from_slice::<String>(bytes)
            .map(Cow::Owned)
            .map_err(|err| err.to_string())
    } else {
        std::str::from_utf8(inner)
            .map(Cow::Borrowed)
            .map_err(|err| err.to_string())
    }
}

fn json_key_matches(bytes: &[u8], range: Range<usize>, key: &str) -> bool {
    let raw = &bytes[range];
    if !raw.contains(&b'\\') {
        return raw == key.as_bytes();
    }
    let mut quoted = Vec::with_capacity(raw.len() + 2);
    quoted.push(b'"');
    quoted.extend_from_slice(raw);
    quoted.push(b'"');
    serde_json::from_slice::<String>(&quoted).is_ok_and(|decoded| decoded == key)
}

fn find_top_level_json_field(bytes: &[u8], key: &str) -> Result<Option<Range<usize>>, ShellError> {
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'{') {
        return Err(stone_error("json view", "JSONL row is not an object"));
    }
    index += 1;
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b'}') {
            return Ok(None);
        }
        if bytes.get(index) != Some(&b'"') {
            return Err(stone_error("json view", "expected object key string"));
        }
        let key_start = index + 1;
        let key_end = json_string_end(bytes, index)?;
        index = skip_json_ws(bytes, key_end + 1);
        if bytes.get(index) != Some(&b':') {
            return Err(stone_error("json view", "expected `:` after object key"));
        }
        index = skip_json_ws(bytes, index + 1);
        let value_start = index;
        let value_end = json_value_end(bytes, value_start)?;
        if json_key_matches(bytes, key_start..key_end, key) {
            return Ok(Some(value_start..value_end));
        }
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => return Ok(None),
            _ => return Err(stone_error("json view", "expected `,` or `}` after value")),
        }
    }
}

fn json_object_for_each_field(
    bytes: &[u8],
    mut f: impl FnMut(Range<usize>, Range<usize>) -> Result<(), ShellError>,
) -> Result<(), ShellError> {
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'{') {
        return Err(stone_error("json view", "JSONL row is not an object"));
    }
    index += 1;
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b'}') {
            return Ok(());
        }
        if bytes.get(index) != Some(&b'"') {
            return Err(stone_error("json view", "expected object key string"));
        }
        let key_start = index + 1;
        let key_end = json_string_end(bytes, index)?;
        index = skip_json_ws(bytes, key_end + 1);
        if bytes.get(index) != Some(&b':') {
            return Err(stone_error("json view", "expected `:` after object key"));
        }
        index = skip_json_ws(bytes, index + 1);
        let value_start = index;
        let value_end = json_value_end(bytes, value_start)?;
        f(key_start..key_end, value_start..value_end)?;
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => return Ok(()),
            _ => return Err(stone_error("json view", "expected `,` or `}` after value")),
        }
    }
}

fn json_string_end(bytes: &[u8], quote: usize) -> Result<usize, ShellError> {
    let mut index = quote + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Ok(index),
            _ => index += 1,
        }
    }
    Err(stone_error("json view", "unterminated string"))
}

fn json_value_end(bytes: &[u8], start: usize) -> Result<usize, ShellError> {
    let mut index = start;
    let mut depth = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => index = json_string_end(bytes, index)? + 1,
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            }
            b'}' | b']' if depth > 0 => {
                depth -= 1;
                index += 1;
            }
            b',' | b'}' | b']' if depth == 0 => return Ok(index),
            _ => index += 1,
        }
    }
    Ok(index)
}

fn skip_json_ws(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

fn trim_json_bytes(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn json_value_may_be_number(bytes: &[u8]) -> bool {
    matches!(bytes.first(), Some(b'-' | b'0'..=b'9'))
}

fn json_array_view_iter_values(view: &JsonArrayView) -> Result<Vec<RuntimeValue>, ShellError> {
    json_array_view_element_ranges(&view.bytes[view.range.clone()])?
        .into_iter()
        .map(|range| json_array_view_value_from_relative_range(view, range))
        .collect()
}

fn json_array_view_value_from_relative_range(
    view: &JsonArrayView,
    range: Range<usize>,
) -> Result<RuntimeValue, ShellError> {
    let absolute = (view.range.start + range.start)..(view.range.start + range.end);
    let value = trim_json_bytes(&view.bytes[absolute.clone()]);
    if value.starts_with(b"[") {
        return Ok(RuntimeValue::JsonArrayView(JsonArrayView {
            bytes: view.bytes.clone(),
            range: absolute,
        }));
    }
    if value.starts_with(b"{") {
        return Ok(RuntimeValue::Nu(json_to_nu_value(
            serde_json::from_slice::<serde_json::Value>(value)
                .map_err(|err| stone_error("json view", err.to_string()))?,
            Span::unknown(),
        )));
    }
    if json_value_may_be_number(value) || value.starts_with(b"\"") {
        return Ok(RuntimeValue::JsonScalarView(JsonScalarView {
            bytes: view.bytes.clone(),
            range: absolute,
            source: Arc::from("<json-array>"),
            line_number: 0,
        }));
    }
    Ok(RuntimeValue::Nu(json_to_nu_value(
        serde_json::from_slice::<serde_json::Value>(value)
            .map_err(|err| stone_error("json view", err.to_string()))?,
        Span::unknown(),
    )))
}

fn json_array_view_element_ranges(bytes: &[u8]) -> Result<Vec<Range<usize>>, ShellError> {
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'[') {
        return Err(stone_error("json view", "JSON view is not an array"));
    }
    index += 1;
    let mut ranges = Vec::new();
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b']') {
            return Ok(ranges);
        }
        let value_start = index;
        let value_end = json_value_end(bytes, value_start)?;
        ranges.push(value_start..value_end);
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b']') => return Ok(ranges),
            _ => {
                return Err(stone_error(
                    "json view",
                    "expected `,` or `]` after array value",
                ));
            }
        }
    }
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

fn json_array_bytes_for_each_range(
    bytes: &[u8],
    mut f: impl FnMut(Range<usize>) -> Result<(), ShellError>,
) -> Result<(), ShellError> {
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'[') {
        return Err(stone_error("json view", "JSON view is not an array"));
    }
    index += 1;
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b']') {
            return Ok(());
        }
        let value_start = index;
        let value_end = json_value_end(bytes, value_start)?;
        f(value_start..value_end)?;
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b']') => return Ok(()),
            _ => {
                return Err(stone_error(
                    "json view",
                    "expected `,` or `]` after array value",
                ));
            }
        }
    }
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

fn materialize_jsonl_rows(rows: &JsonlRows) -> Result<Value, ShellError> {
    let mut values = Vec::with_capacity(rows.lines.len());
    for row in jsonl_row_views(rows) {
        values.push(row.into_nu_value("read_jsonl")?);
    }
    Ok(Value::list(values, Span::unknown()))
}

fn materialize_json_object_view(view: &JsonObjectView) -> Result<Value, ShellError> {
    let json = serde_json::from_slice::<serde_json::Value>(&view.bytes[view.range.clone()])
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?;
    Ok(json_to_nu_value(json, Span::unknown()))
}

fn materialize_json_array_view(view: &JsonArrayView) -> Result<Value, ShellError> {
    let json = serde_json::from_slice::<serde_json::Value>(&view.bytes[view.range.clone()])
        .map_err(|err| stone_error("json view", err.to_string()))?;
    Ok(json_to_nu_value(json, Span::unknown()))
}

fn materialize_json_scalar_view(view: &JsonScalarView) -> Result<Value, ShellError> {
    let json = serde_json::from_slice::<serde_json::Value>(&view.bytes[view.range.clone()])
        .map_err(|err| json_scalar_view_error(view, err))?;
    Ok(json_to_nu_value(json, Span::unknown()))
}

fn json_scalar_view_to_string(view: &JsonScalarView) -> Result<String, String> {
    json_string_bytes_to_string(&view.bytes[view.range.clone()])
}

fn json_scalar_view_to_i64(view: &JsonScalarView) -> Result<i64, ShellError> {
    serde_json::from_slice::<i64>(&view.bytes[view.range.clone()])
        .map_err(|err| json_scalar_view_error(view, err))
}

fn json_scalar_view_to_f64(view: &JsonScalarView) -> Result<f64, ShellError> {
    serde_json::from_slice::<f64>(&view.bytes[view.range.clone()])
        .map_err(|err| json_scalar_view_error(view, err))
}

fn json_scalar_view_error(view: &JsonScalarView, err: serde_json::Error) -> ShellError {
    stone_error(
        "json view",
        format!("{} line {}: {}", view.source, view.line_number, err),
    )
}

fn extract_sort_key(value: &Value, key: &SortKey) -> Result<Value, ShellError> {
    match key {
        SortKey::Identity => Ok(value.clone()),
        SortKey::Field(field) => match value {
            Value::Record { val, .. } => val
                .get(field)
                .cloned()
                .ok_or_else(|| stone_error("sort", format!("record has no key `{field}`"))),
            other => Err(stone_error(
                "sort",
                format!("key= requires record rows, got {}", other.get_type()),
            )),
        },
        SortKey::Callable(_) => Err(stone_error(
            "sort",
            "internal error: callable sort keys must be invoked by eval_sort_call",
        )),
    }
}

fn sort_key_kind(value: &Value) -> Result<SortKeyKind, ShellError> {
    match value {
        Value::Int { .. } => Ok(SortKeyKind::Number),
        Value::Float { val, .. } if !val.is_nan() => Ok(SortKeyKind::Number),
        Value::Float { .. } => Err(stone_error("sort", "cannot sort NaN values")),
        Value::String { .. } | Value::Glob { .. } => Ok(SortKeyKind::Text),
        Value::List { .. } => Ok(SortKeyKind::Composite),
        other => Err(stone_error(
            "sort",
            format!("cannot sort by {}", other.get_type()),
        )),
    }
}

fn shell_error_message(error: &ShellError) -> String {
    match error {
        ShellError::Generic(error) => error.msg.to_string(),
        other => other.to_string(),
    }
}

fn is_map_builtin_name(func_name: &str) -> bool {
    matches!(func_name, "int" | "float" | "json_dumps" | "str")
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

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Nothing { .. } => "NoneType",
        Value::Bool { .. } => "bool",
        Value::Int { .. } => "int",
        Value::Float { .. } => "float",
        Value::String { .. } | Value::Glob { .. } => "str",
        Value::List { .. } => "list",
        Value::Record { .. } => "dict",
        Value::Binary { .. } => "bytes",
        _ => "value",
    }
}

fn value_identity_key(value: &Value, context: &str) -> Result<String, ShellError> {
    serde_json::to_string(&nu_to_json_value(value))
        .map_err(|err| stone_error(context, err.to_string()))
}

fn zfill(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_owned();
    }
    let (sign, digits) = if let Some(rest) = text.strip_prefix('-') {
        ("-", rest)
    } else if let Some(rest) = text.strip_prefix('+') {
        ("+", rest)
    } else {
        ("", text)
    };
    format!("{sign}{}{}", "0".repeat(width - len), digits)
}

fn format_fstring_value(value: &Value, spec: &StoneFormatSpec) -> Result<String, ShellError> {
    match spec {
        StoneFormatSpec::Fixed { precision } => {
            let value = value_to_f64(value, "f-string format")?;
            Ok(format!("{value:.precision$}"))
        }
        StoneFormatSpec::ZeroPadInt { width } => {
            let value = value_to_i64(value, "f-string format")?;
            Ok(zfill(&value.to_string(), *width))
        }
    }
}

fn map_builtin_value(func_name: &str, value: &Value) -> Result<Value, ShellError> {
    match func_name {
        "int" => value_to_int(value),
        "float" => value_to_f64(value, "float").map(|value| Value::float(value, Span::unknown())),
        "json_dumps" => serde_json::to_string(&nu_to_json_value(value))
            .map(|text| Value::string(text, Span::unknown()))
            .map_err(|err| stone_error("map", err.to_string())),
        "str" => value_to_display_string(value).map(|text| Value::string(text, Span::unknown())),
        other => Err(stone_error(
            "map",
            format!("map() does not support `{other}` yet"),
        )),
    }
}

enum SumNumber {
    Int(i64),
    Float(f64),
}

fn value_to_sum_number(value: &Value) -> Result<SumNumber, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(SumNumber::Int(*val)),
        Value::Float { val, .. } => Ok(SumNumber::Float(*val)),
        Value::String { val, .. } | Value::Glob { val, .. } => {
            let text = val.trim();
            if let Ok(value) = text.parse::<i64>() {
                return Ok(SumNumber::Int(value));
            }
            text.parse::<f64>()
                .map(SumNumber::Float)
                .map_err(|err| stone_error("sum", format!("failed to parse number: {err}")))
        }
        other => Err(stone_error(
            "sum",
            format!("expected number, got {}", other.get_type()),
        )),
    }
}

fn value_len(value: &Value) -> Result<i64, ShellError> {
    let len = match value {
        Value::List { vals, .. } => vals.len(),
        Value::Record { val, .. } => val.len(),
        Value::String { val, .. } | Value::Glob { val, .. } => val.chars().count(),
        other => {
            return Err(stone_error(
                "len",
                format!("len() does not support {}", other.get_type()),
            ));
        }
    };
    i64::try_from(len).map_err(|_| stone_error("len", "value is too large"))
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

fn eval_aug_assign(left: &Value, op: AugOp, right: &Value) -> Result<Value, ShellError> {
    match op {
        AugOp::Add => eval_add(left, right),
    }
}

fn eval_add(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left
            .checked_add(*right)
            .map(|value| Value::int(value, Span::unknown()))
            .ok_or_else(|| stone_error("addition", "integer addition overflow")),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(left + right, Span::unknown()))
        }
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(*left as f64 + right, Span::unknown()))
        }
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => {
            Ok(Value::float(left + *right as f64, Span::unknown()))
        }
        (Value::String { val: left, .. }, Value::String { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::String { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::String { val: right, .. }) => {
            Ok(Value::string(format!("{left}{right}"), Span::unknown()))
        }
        (Value::List { vals: left, .. }, Value::List { vals: right, .. }) => {
            let mut values = left.clone();
            values.extend(right.clone());
            Ok(Value::list(values, Span::unknown()))
        }
        _ => Err(stone_error(
            "addition",
            format!("cannot add {} and {}", left.get_type(), right.get_type()),
        )),
    }
}

fn eval_neg(value: &Value) -> Result<Value, ShellError> {
    match value {
        Value::Int { val, .. } => val
            .checked_neg()
            .map(|value| Value::int(value, Span::unknown()))
            .ok_or_else(|| stone_error("unary minus", "integer negation overflow")),
        Value::Float { val, .. } => Ok(Value::float(-val, Span::unknown())),
        other => Err(stone_error(
            "unary minus",
            format!("cannot negate {}", other.get_type()),
        )),
    }
}

fn eval_sub(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left
            .checked_sub(*right)
            .map(|value| Value::int(value, Span::unknown()))
            .ok_or_else(|| stone_error("subtraction", "integer subtraction overflow")),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(left - right, Span::unknown()))
        }
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(*left as f64 - right, Span::unknown()))
        }
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => {
            Ok(Value::float(left - *right as f64, Span::unknown()))
        }
        _ => Err(stone_error(
            "subtraction",
            format!(
                "cannot subtract {} and {}",
                left.get_type(),
                right.get_type()
            ),
        )),
    }
}

fn eval_mul(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left
            .checked_mul(*right)
            .map(|value| Value::int(value, Span::unknown()))
            .ok_or_else(|| stone_error("multiplication", "integer multiplication overflow")),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(left * right, Span::unknown()))
        }
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(*left as f64 * right, Span::unknown()))
        }
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => {
            Ok(Value::float(left * *right as f64, Span::unknown()))
        }
        _ => Err(stone_error(
            "multiplication",
            format!(
                "cannot multiply {} and {}",
                left.get_type(),
                right.get_type()
            ),
        )),
    }
}

fn eval_div(left: &Value, right: &Value) -> Result<Value, ShellError> {
    let (left, right) = match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => {
            (*left as f64, *right as f64)
        }
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => (*left, *right),
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => (*left as f64, *right),
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => (*left, *right as f64),
        _ => {
            return Err(stone_error(
                "division",
                format!("cannot divide {} and {}", left.get_type(), right.get_type()),
            ));
        }
    };
    if right == 0.0 {
        return Err(stone_error("division", "division by zero"));
    }
    Ok(Value::float(left / right, Span::unknown()))
}

fn eval_floor_div(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => {
            if *right == 0 {
                return Err(stone_error("floor division", "division by zero"));
            }
            Ok(Value::int(left.div_euclid(*right), Span::unknown()))
        }
        _ => {
            let Value::Float { val, .. } = eval_div(left, right)? else {
                unreachable!("eval_div returns a float")
            };
            Ok(Value::float(val.floor(), Span::unknown()))
        }
    }
}

fn eval_mod(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => {
            if *right == 0 {
                return Err(stone_error("modulo", "modulo by zero"));
            }
            Ok(Value::int(left.rem_euclid(*right), Span::unknown()))
        }
        _ => {
            let left = value_to_f64(left, "modulo")?;
            let right = value_to_f64(right, "modulo")?;
            if right == 0.0 {
                return Err(stone_error("modulo", "modulo by zero"));
            }
            Ok(Value::float(left.rem_euclid(right), Span::unknown()))
        }
    }
}

fn eval_bitwise_int(
    left: &Value,
    right: &Value,
    context: &str,
    op: impl FnOnce(i64, i64) -> i64,
) -> Result<Value, ShellError> {
    let left = value_to_i64(left, context)?;
    let right = value_to_i64(right, context)?;
    Ok(Value::int(op(left, right), Span::unknown()))
}

fn eval_shift(
    left: &Value,
    right: &Value,
    context: &str,
    op: impl FnOnce(i64, u32) -> Option<i64>,
) -> Result<Value, ShellError> {
    let left = value_to_i64(left, context)?;
    let right = value_to_i64(right, context)?;
    let shift = u32::try_from(right)
        .map_err(|_| stone_error(context, "shift count must be non-negative"))?;
    op(left, shift)
        .map(|value| Value::int(value, Span::unknown()))
        .ok_or_else(|| stone_error(context, "shift count is too large"))
}

fn eval_string_method(
    receiver: &Value,
    method: &str,
    args: &[Value],
    f: impl FnOnce(&str, &[Value]) -> Result<Value, ShellError>,
) -> Result<Value, ShellError> {
    match receiver {
        Value::String { val, .. } | Value::Glob { val, .. } => f(val, args),
        other => Err(stone_error(
            method,
            format!("{} has no {method}()", other.get_type()),
        )),
    }
}

fn eval_record_method(receiver: &Value, method: &str, args: &[Value]) -> Result<Value, ShellError> {
    let Value::Record { val, .. } = receiver else {
        return Err(stone_error(
            method,
            format!("{} has no {method}()", receiver.get_type()),
        ));
    };
    match method {
        "get" => {
            let ([key] | [key, _]) = args else {
                return Err(stone_error(
                    "get",
                    "record get() requires key and optional default",
                ));
            };
            let key = value_to_string(key, "get")?;
            if let Some(value) = val.get(&key) {
                return Ok(value.clone());
            }
            match args {
                [_] => Ok(Value::nothing(Span::unknown())),
                [_, default] => Ok(default.clone()),
                _ => unreachable!(),
            }
        }
        "keys" => {
            let [] = args else {
                return Err(stone_error("keys", "keys() takes no arguments"));
            };
            Ok(Value::list(
                val.iter()
                    .map(|(key, _)| Value::string(key.clone(), Span::unknown()))
                    .collect(),
                Span::unknown(),
            ))
        }
        "values" => {
            let [] = args else {
                return Err(stone_error("values", "values() takes no arguments"));
            };
            Ok(Value::list(
                val.iter().map(|(_, value)| value.clone()).collect(),
                Span::unknown(),
            ))
        }
        "items" => {
            let [] = args else {
                return Err(stone_error("items", "items() takes no arguments"));
            };
            Ok(Value::list(
                val.iter()
                    .map(|(key, value)| {
                        Value::list(
                            vec![Value::string(key.clone(), Span::unknown()), value.clone()],
                            Span::unknown(),
                        )
                    })
                    .collect(),
                Span::unknown(),
            ))
        }
        _ => unreachable!("record method dispatch is validated by caller"),
    }
}

fn eval_index_method(receiver: &Value, args: &[Value]) -> Result<Value, ShellError> {
    match receiver {
        Value::List { vals, .. } => {
            let [needle] = args else {
                return Err(stone_error(
                    "index",
                    "list index() requires exactly one argument",
                ));
            };
            vals.iter()
                .position(|value| values_equal(value, needle))
                .map(|index| Value::int(index as i64, Span::unknown()))
                .ok_or_else(|| stone_error("index", "value is not in list"))
        }
        Value::String { val, .. } | Value::Glob { val, .. } => {
            let ([needle] | [needle, _]) = args else {
                return Err(stone_error(
                    "index",
                    "string index() requires a substring and optional start",
                ));
            };
            let needle = value_to_string(needle, "index")?;
            let start = match args {
                [_] => 0,
                [_, start] => {
                    let start = value_to_i64(start, "index")?;
                    normalize_string_start(start, val.chars().count())?
                }
                _ => unreachable!(),
            };
            let prefix_len = val
                .char_indices()
                .nth(start)
                .map(|(index, _)| index)
                .unwrap_or_else(|| val.len());
            val[prefix_len..]
                .find(&needle)
                .map(|index| Value::int((prefix_len + index) as i64, Span::unknown()))
                .ok_or_else(|| stone_error("index", "substring not found"))
        }
        other => Err(stone_error(
            "index",
            format!("{} has no index()", other.get_type()),
        )),
    }
}

fn eval_find_method(receiver: &Value, args: &[Value]) -> Result<Value, ShellError> {
    let (Value::String { val, .. } | Value::Glob { val, .. }) = receiver else {
        return Err(stone_error(
            "find",
            format!("{} has no find()", receiver.get_type()),
        ));
    };
    let ([needle] | [needle, _]) = args else {
        return Err(stone_error(
            "find",
            "string find() requires a substring and optional start",
        ));
    };
    let needle = value_to_string(needle, "find")?;
    let start = match args {
        [_] => 0,
        [_, start] => {
            let start = value_to_i64(start, "find")?;
            normalize_string_start(start, val.chars().count())?
        }
        _ => unreachable!(),
    };
    let prefix_len = val
        .char_indices()
        .nth(start)
        .map(|(index, _)| index)
        .unwrap_or_else(|| val.len());
    let index = val[prefix_len..]
        .find(&needle)
        .map(|index| (prefix_len + index) as i64)
        .unwrap_or(-1);
    Ok(Value::int(index, Span::unknown()))
}

fn eval_subscript(value: &Value, index: &Value) -> Result<Value, ShellError> {
    match (value, index) {
        (Value::Record { val, .. }, Value::String { val: key, .. })
        | (Value::Record { val, .. }, Value::Glob { val: key, .. }) => val
            .get(key)
            .cloned()
            .ok_or_else(|| stone_error("subscript", format!("record has no key `{key}`"))),
        (Value::List { vals, .. }, Value::Int { val: index, .. }) => {
            let index = normalize_index(*index, vals.len())?;
            vals.get(index).cloned().ok_or_else(|| {
                stone_error("subscript", format!("list index {index} is out of range"))
            })
        }
        (Value::String { val, .. }, Value::Int { val: index, .. }) => {
            let chars = val.chars().collect::<Vec<_>>();
            let index = normalize_index(*index, chars.len())?;
            chars
                .get(index)
                .map(|ch| Value::string(ch.to_string(), Span::unknown()))
                .ok_or_else(|| {
                    stone_error("subscript", format!("string index {index} is out of range"))
                })
        }
        (value, index) => Err(stone_error(
            "subscript",
            format!(
                "cannot index {} with {}",
                value.get_type(),
                index.get_type()
            ),
        )),
    }
}

fn eval_slice(value: &Value, lower: Option<i64>, upper: Option<i64>) -> Result<Value, ShellError> {
    match value {
        Value::List { vals, .. } => {
            let (start, end) = normalize_slice_bounds(lower, upper, vals.len())?;
            Ok(Value::list(vals[start..end].to_vec(), Span::unknown()))
        }
        Value::String { val, .. } | Value::Glob { val, .. } => {
            let chars = val.chars().collect::<Vec<_>>();
            let (start, end) = normalize_slice_bounds(lower, upper, chars.len())?;
            Ok(Value::string(
                chars[start..end].iter().collect::<String>(),
                Span::unknown(),
            ))
        }
        other => Err(stone_error(
            "slice",
            format!("cannot slice {}", other.get_type()),
        )),
    }
}

fn normalize_slice_bounds(
    lower: Option<i64>,
    upper: Option<i64>,
    len: usize,
) -> Result<(usize, usize), ShellError> {
    let len_i64 =
        i64::try_from(len).map_err(|_| stone_error("slice", "collection is too large"))?;
    let start = lower.unwrap_or(0);
    let end = upper.unwrap_or(len_i64);
    let start = if start < 0 { len_i64 + start } else { start }.clamp(0, len_i64);
    let end = if end < 0 { len_i64 + end } else { end }.clamp(0, len_i64);
    let start = usize::try_from(start).map_err(|_| stone_error("slice", "start is too large"))?;
    let end = usize::try_from(end).map_err(|_| stone_error("slice", "end is too large"))?;
    if start > end {
        Ok((start, start))
    } else {
        Ok((start, end))
    }
}

fn normalize_index(index: i64, len: usize) -> Result<usize, ShellError> {
    let len =
        i64::try_from(len).map_err(|_| stone_error("subscript", "collection is too large"))?;
    let normalized = if index < 0 { len + index } else { index };
    if normalized < 0 || normalized >= len {
        return Err(stone_error(
            "subscript",
            format!("index {index} is out of range"),
        ));
    }
    usize::try_from(normalized).map_err(|_| stone_error("subscript", "index is too large"))
}

fn normalize_string_start(index: i64, len: usize) -> Result<usize, ShellError> {
    let len = i64::try_from(len).map_err(|_| stone_error("index", "string is too large"))?;
    let normalized = if index < 0 { len + index } else { index };
    let clamped = normalized.clamp(0, len);
    usize::try_from(clamped).map_err(|_| stone_error("index", "index is too large"))
}

fn format_template(template: &str, args: &[String]) -> Result<String, ShellError> {
    let mut output = String::new();
    let mut chars = template.chars().peekable();
    let mut arg_index = 0;
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                output.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                output.push('}');
            }
            '{' if chars.peek() == Some(&'}') => {
                chars.next();
                let Some(value) = args.get(arg_index) else {
                    return Err(stone_error(
                        "format",
                        "format() has fewer arguments than placeholders",
                    ));
                };
                output.push_str(value);
                arg_index += 1;
            }
            '{' | '}' => {
                return Err(stone_error(
                    "format",
                    "format() supports only `{}` placeholders and escaped `{{` or `}}`",
                ));
            }
            _ => output.push(ch),
        }
    }
    if arg_index != args.len() {
        return Err(stone_error(
            "format",
            "format() received more arguments than placeholders",
        ));
    }
    Ok(output)
}

fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Bool { val, .. } => *val,
        Value::Nothing { .. } => false,
        value => !value.is_empty(),
    }
}

fn compare_values(left: &Value, op: CompareOp, right: &Value) -> Result<bool, ShellError> {
    match op {
        CompareOp::Eq => Ok(values_equal(left, right)),
        CompareOp::NotEq => Ok(!values_equal(left, right)),
        CompareOp::Is => Ok(values_equal(left, right)),
        CompareOp::IsNot => Ok(!values_equal(left, right)),
        CompareOp::In => value_contains(right, left),
        CompareOp::NotIn => value_contains(right, left).map(|value| !value),
        CompareOp::Lt | CompareOp::LtE | CompareOp::Gt | CompareOp::GtE => {
            let ordering = value_ordering(left, right)?;
            Ok(match op {
                CompareOp::Lt => ordering == std::cmp::Ordering::Less,
                CompareOp::LtE => ordering != std::cmp::Ordering::Greater,
                CompareOp::Gt => ordering == std::cmp::Ordering::Greater,
                CompareOp::GtE => ordering != std::cmp::Ordering::Less,
                CompareOp::Eq
                | CompareOp::NotEq
                | CompareOp::In
                | CompareOp::NotIn
                | CompareOp::Is
                | CompareOp::IsNot => {
                    unreachable!()
                }
            })
        }
    }
}

fn value_contains(container: &Value, needle: &Value) -> Result<bool, ShellError> {
    match container {
        Value::String { val, .. } | Value::Glob { val, .. } => {
            let needle = value_to_string(needle, "membership")?;
            Ok(val.contains(&needle))
        }
        Value::List { vals, .. } => Ok(vals.iter().any(|value| values_equal(value, needle))),
        Value::Record { val, .. } => {
            let needle = value_to_string(needle, "membership")?;
            Ok(val.get(&needle).is_some())
        }
        other => Err(stone_error(
            "membership",
            format!("cannot test membership in {}", other.get_type()),
        )),
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Nothing { .. }, Value::Nothing { .. }) => true,
        (Value::Bool { val: left, .. }, Value::Bool { val: right, .. }) => left == right,
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left == right,
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => left == right,
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => (*left as f64) == *right,
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => *left == (*right as f64),
        (Value::String { val: left, .. }, Value::String { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::String { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::String { val: right, .. }) => left == right,
        _ => false,
    }
}

fn stone_name_matches(name: &str, contains: Option<&str>, glob: Option<&str>) -> bool {
    contains.is_none_or(|needle| name.contains(needle))
        && glob.is_none_or(|pattern| stone_wildcard_match(pattern, name))
}

fn stone_wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut pattern_index, mut text_index) = (0usize, 0usize);
    let mut star_index = None;
    let mut star_text_index = 0usize;

    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_text_index = text_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_text_index += 1;
            text_index = star_text_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }

    pattern_index == pattern.len()
}

fn value_ordering(left: &Value, right: &Value) -> Result<std::cmp::Ordering, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => Ok(left.cmp(right)),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => left
            .partial_cmp(right)
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => (*left as f64)
            .partial_cmp(right)
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => left
            .partial_cmp(&(*right as f64))
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::String { val: left, .. }, Value::String { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::String { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::String { val: right, .. }) => Ok(left.cmp(right)),
        (Value::List { vals: left, .. }, Value::List { vals: right, .. }) => {
            for (left, right) in left.iter().zip(right.iter()) {
                let ordering = value_ordering(left, right)?;
                if ordering != std::cmp::Ordering::Equal {
                    return Ok(ordering);
                }
            }
            Ok(left.len().cmp(&right.len()))
        }
        _ => Err(stone_error(
            "comparison",
            format!("cannot order {} and {}", left.get_type(), right.get_type()),
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

trait StoneFileAdapter {
    fn read_text(&self, path: &Path, max_bytes: usize) -> Result<String, ShellError>;
    fn write_text(
        &self,
        path: &Path,
        text: &str,
        append: bool,
    ) -> Result<StoneFileWrite, ShellError>;
    fn stat(&self, path: &Path, follow_symlinks: bool) -> Result<StoneFileStat, ShellError>;
    fn list_dir(&self, path: &Path) -> Result<Vec<StoneFileEntry>, ShellError>;
}

#[derive(Clone, Debug)]
struct StoneFileEntry {
    name: String,
    stat: StoneFileStat,
}

#[derive(Clone, Debug)]
struct StoneFileStat {
    path: PathBuf,
    kind: &'static str,
    is_file: bool,
    is_dir: bool,
    is_symlink: bool,
    readonly: bool,
    size: u64,
    modified_ms: Option<i64>,
    accessed_ms: Option<i64>,
    created_ms: Option<i64>,
}

#[derive(Clone, Debug)]
struct StoneFileWrite {
    path: PathBuf,
    bytes: usize,
    append: bool,
}

struct StdStoneFileAdapter;

static STD_STONE_FILE_ADAPTER: StdStoneFileAdapter = StdStoneFileAdapter;

fn stone_file_adapter() -> &'static dyn StoneFileAdapter {
    &STD_STONE_FILE_ADAPTER
}

impl StoneFileAdapter for StdStoneFileAdapter {
    fn read_text(&self, path: &Path, max_bytes: usize) -> Result<String, ShellError> {
        let mut bytes =
            fs::read(path).map_err(|err| io_read_stone_error("read_text", err, path))?;
        if bytes.len() > max_bytes {
            bytes.truncate(max_bytes);
            while std::str::from_utf8(&bytes).is_err() && !bytes.is_empty() {
                bytes.pop();
            }
        }
        String::from_utf8(bytes).map_err(|err| {
            stone_error(
                "read_text",
                format!("{}: invalid UTF-8: {err}", path.display()),
            )
        })
    }

    fn write_text(
        &self,
        path: &Path,
        text: &str,
        append: bool,
    ) -> Result<StoneFileWrite, ShellError> {
        ensure_parent_dir_for_write("write_text", path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(path)
            .map_err(|err| io_stone_error("write_text", err, path))?;
        file.write_all(text.as_bytes())
            .map_err(|err| io_stone_error("write_text", err, path))?;
        file.flush()
            .map_err(|err| io_stone_error("write_text", err, path))?;
        Ok(StoneFileWrite {
            path: path.to_path_buf(),
            bytes: text.len(),
            append,
        })
    }

    fn stat(&self, path: &Path, follow_symlinks: bool) -> Result<StoneFileStat, ShellError> {
        let metadata = if follow_symlinks {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        }
        .map_err(|err| io_read_stone_error("stat", err, path))?;
        Ok(file_stat_from_metadata(path.to_path_buf(), &metadata))
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<StoneFileEntry>, ShellError> {
        let mut entries = fs::read_dir(path)
            .map_err(|err| io_read_stone_error("list_dir", err, path))?
            .map(|entry| {
                let entry = entry.map_err(|err| io_stone_error("list_dir", err, path))?;
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|err| io_stone_error("list_dir", err, &path))?;
                Ok(StoneFileEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    stat: file_stat_from_metadata(path, &metadata),
                })
            })
            .collect::<Result<Vec<_>, ShellError>>()?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }
}

fn file_stat_from_metadata(path: PathBuf, metadata: &fs::Metadata) -> StoneFileStat {
    StoneFileStat {
        path,
        kind: file_type_name(metadata),
        is_file: metadata.is_file(),
        is_dir: metadata.is_dir(),
        is_symlink: metadata.file_type().is_symlink(),
        readonly: metadata.permissions().readonly(),
        size: metadata.len(),
        modified_ms: system_time_ms(metadata.modified().ok()),
        accessed_ms: system_time_ms(metadata.accessed().ok()),
        created_ms: system_time_ms(metadata.created().ok()),
    }
}

fn file_entry_record(entry: StoneFileEntry, span: Span) -> Value {
    let mut record = Record::with_capacity(7);
    record.push("name", Value::string(entry.name, span));
    record.push(
        "path",
        Value::string(entry.stat.path.display().to_string(), span),
    );
    record.push("type", Value::string(entry.stat.kind, span));
    record.push("is_file", Value::bool(entry.stat.is_file, span));
    record.push("is_dir", Value::bool(entry.stat.is_dir, span));
    record.push("is_symlink", Value::bool(entry.stat.is_symlink, span));
    record.push(
        "size",
        Value::int(i64::try_from(entry.stat.size).unwrap_or(i64::MAX), span),
    );
    Value::record(record, span)
}

fn search_match_record(path: &Path, line: usize, text: &str) -> Value {
    let span = Span::unknown();
    let mut record = Record::with_capacity(3);
    record.push("path", Value::string(path.display().to_string(), span));
    record.push(
        "line",
        Value::int(i64::try_from(line).unwrap_or(i64::MAX), span),
    );
    record.push("text", Value::string(text.to_string(), span));
    Value::record(record, span)
}

enum StoneSearchMatcher {
    Literal(Vec<u8>),
    Regex(Regex),
}

impl StoneSearchMatcher {
    fn new(needle: &str, regex: bool) -> Result<Self, ShellError> {
        if regex {
            Regex::new(needle)
                .map(Self::Regex)
                .map_err(|err| stone_error("search", format!("invalid regex: {err}")))
        } else {
            Ok(Self::Literal(needle.as_bytes().to_vec()))
        }
    }

    fn is_match(&self, bytes: &[u8]) -> bool {
        match self {
            Self::Literal(needle) => memchr::memmem::Finder::new(needle).find(bytes).is_some(),
            Self::Regex(regex) => regex.is_match(bytes),
        }
    }
}

fn push_stone_search_line_matches(
    matches: &mut Vec<Value>,
    path: &Path,
    content: &[u8],
    matcher: &StoneSearchMatcher,
) {
    let mut line_number = 1usize;
    let mut start = 0usize;
    for end in memchr::memchr_iter(b'\n', content).chain(std::iter::once(content.len())) {
        let line = trim_byte_line_end(&content[start..end]);
        if matcher.is_match(line) {
            matches.push(search_match_record(
                path,
                line_number,
                &String::from_utf8_lossy(line),
            ));
            if matches.len() >= STONE_MAX_SEARCH_MATCHES {
                break;
            }
        }
        if end == content.len() {
            break;
        }
        start = end + 1;
        line_number += 1;
    }
}

fn trim_byte_line_end(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn stone_bytes_look_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(1024).any(|byte| *byte == 0)
}

fn file_stat_record(stat: StoneFileStat, span: Span) -> Value {
    let mut record = Record::with_capacity(10);
    record.push("path", Value::string(stat.path.display().to_string(), span));
    record.push("type", Value::string(stat.kind, span));
    record.push("is_file", Value::bool(stat.is_file, span));
    record.push("is_dir", Value::bool(stat.is_dir, span));
    record.push("is_symlink", Value::bool(stat.is_symlink, span));
    record.push("readonly", Value::bool(stat.readonly, span));
    record.push(
        "size",
        Value::int(i64::try_from(stat.size).unwrap_or(i64::MAX), span),
    );
    record.push("modified_ms", optional_i64_value(stat.modified_ms, span));
    record.push("accessed_ms", optional_i64_value(stat.accessed_ms, span));
    record.push("created_ms", optional_i64_value(stat.created_ms, span));
    Value::record(record, span)
}

fn file_write_record(write: StoneFileWrite, span: Span) -> Value {
    let mut record = Record::with_capacity(3);
    record.push(
        "path",
        Value::string(write.path.display().to_string(), span),
    );
    record.push(
        "bytes",
        Value::int(i64::try_from(write.bytes).unwrap_or(i64::MAX), span),
    );
    record.push("append", Value::bool(write.append, span));
    Value::record(record, span)
}

fn file_type_name(metadata: &fs::Metadata) -> &'static str {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        "dir"
    } else if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_file() {
        "file"
    } else {
        "other"
    }
}

fn system_time_ms(time: Option<std::time::SystemTime>) -> Option<i64> {
    let Some(time) = time else {
        return None;
    };
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => Some(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)),
        Err(_) => None,
    }
}

fn optional_i64_value(value: Option<i64>, span: Span) -> Value {
    match value {
        Some(value) => Value::int(value, span),
        None => Value::nothing(span),
    }
}

fn io_stone_error(kind: &str, err: std::io::Error, path: &Path) -> ShellError {
    let path = path.to_path_buf();
    ShellError::Io(
        nu_protocol::shell_error::io::IoError::new_internal_with_path(
            err,
            format!("Stone {kind} I/O error at {}", path.display()),
            path,
        ),
    )
}

fn io_read_stone_error(kind: &str, err: std::io::Error, path: &Path) -> ShellError {
    if err.kind() != ErrorKind::NotFound {
        return io_stone_error(kind, err, path);
    }
    let suggestions = nearby_read_path_suggestions(path, 5);
    if suggestions.is_empty() {
        return io_stone_error(kind, err, path);
    }
    ShellError::Generic(
        GenericError::new_internal(
            format!("Stone {kind} I/O error"),
            format!(
                "{}: {}. Did you mean {}?",
                path.display(),
                err,
                suggestions.join(" or ")
            ),
        )
        .with_code("io_error"),
    )
}

fn nearby_read_path_suggestions(path: &Path, limit: usize) -> Vec<String> {
    let Some(root) = nearest_existing_search_root(path) else {
        return Vec::new();
    };
    if root == Path::new("/") {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    if let Some(expected_name) = path.file_name().and_then(|name| name.to_str()) {
        collect_read_path_suggestions(&root, limit, &mut candidates, &mut |candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == expected_name)
        });
    }
    if candidates.len() < limit {
        if let Some(expected_suffix) = path.extension().and_then(|suffix| suffix.to_str()) {
            collect_read_path_suggestions(&root, limit, &mut candidates, &mut |candidate| {
                candidate
                    .extension()
                    .and_then(|suffix| suffix.to_str())
                    .is_some_and(|suffix| suffix == expected_suffix)
            });
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates.truncate(limit);
    candidates
}

fn nearest_existing_search_root(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() || !parent.is_dir() {
        return None;
    }
    Some(parent.to_path_buf())
}

fn collect_read_path_suggestions(
    current: &Path,
    limit: usize,
    candidates: &mut Vec<String>,
    matches: &mut dyn FnMut(&Path) -> bool,
) {
    if candidates.len() >= limit {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if candidates.len() >= limit {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_read_path_suggestions(&path, limit, candidates, matches);
        } else if path.is_file() && matches(&path) {
            candidates.push(path.display().to_string());
        }
    }
}

fn ensure_parent_dir_for_write(kind: &str, path: &Path) -> Result<(), ShellError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.exists() {
        return Ok(());
    }
    fs::create_dir_all(parent).map_err(|err| io_stone_error(kind, err, parent))
}

#[cfg(test)]
mod tests {
    use super::{
        eval_program, eval_program_with_options, eval_program_with_output,
        match_fused_map_update_if, EvalHotLoopDiagnostics, EvalOptions, RuntimeValue, TextLines,
    };
    use crate::{json, stone_ast::lower_source, stone_ir::LoopIrOptimizationDiagnostic};
    use nu_protocol::{
        engine::{EngineState, Stack},
        PipelineData, ShellError, Span, Value,
    };
    use serde_json::json as json_value;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
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
        assert_hot_loop_matches_baseline(
            "generic-vm-unsupported-fallback",
            r#"total = ""
for n in ["a", "b", "c"]:
    total += n
emit(total)
"#,
            &[],
        )
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
            r#"hook("run.after_failure", family="python", argv0_prefix=["python"], handler="python.after_failure", priority=100)
"#,
        );
        let program = lower_source(
            r#"result = run(["python3", "-c", "import stone_definitely_missing_module"])
emit({
    "ok": result["ok"],
    "kind": result["explanation"]["kind"],
    "module": result["explanation"]["module"],
    "package": result["explanation"]["package"],
    "install_argv": result["explanation"]["install_argv"],
    "runtime_kind": result["runtime"]["kind"],
    "helper_count": len(result["helpers"]),
    "helper": result["helpers"][0]["helper"],
    "helper_kind": result["helpers"][0]["kind"],
    "helper_family": result["helpers"][0]["family"],
    "next_check": result["helpers"][0]["next_checks"][0],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["kind"], json_value!("python_module_not_found"));
        assert_eq!(
            value["module"],
            json_value!("stone_definitely_missing_module")
        );
        assert_eq!(
            value["package"],
            json_value!("stone_definitely_missing_module")
        );
        assert_eq!(
            value["install_argv"],
            json_value!([
                "python3",
                "-m",
                "pip",
                "install",
                "stone_definitely_missing_module"
            ])
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
        let (engine_state, mut stack, _root) = test_engine("run-python-missing-attribute")?;
        let program = lower_source(
            r#"result = run(["python3", "-c", "import math; math.stone_definitely_missing_attribute"])
emit({
    "ok": result["ok"],
    "kind": result["explanation"]["kind"],
    "module": result["explanation"]["module"],
    "attribute": result["explanation"]["attribute"],
    "package": result["explanation"]["package"],
    "inspect_argv": result["explanation"]["inspect_argv"],
    "runtime_kind": result["runtime"]["kind"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(
            value["kind"],
            json_value!("python_module_attribute_missing")
        );
        assert_eq!(value["module"], json_value!("math"));
        assert_eq!(
            value["attribute"],
            json_value!("stone_definitely_missing_attribute")
        );
        assert_eq!(value["package"], json_value!("math"));
        assert_eq!(
            value["inspect_argv"],
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
        let (engine_state, mut stack, _root) = test_engine("run-pip-check-conflict")?;
        let program = lower_source(
            r#"script = "import sys; print('demo 1.0 has requirement dep<2, but you have dep 3.0.', file=sys.stderr); sys.exit(1)"
result = run(["python3", "-c", script])
emit({
    "ok": result["ok"],
    "kind": result["explanation"]["kind"],
    "dependent": result["explanation"]["dependent"],
    "requirement": result["explanation"]["requirement"],
    "installed": result["explanation"]["installed"],
    "inspect_argv": result["explanation"]["inspect_argv"],
})
"#,
        )?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(value["kind"], json_value!("python_dependency_conflict"));
        assert_eq!(value["dependent"], json_value!("demo 1.0"));
        assert_eq!(value["requirement"], json_value!("dep<2"));
        assert_eq!(value["installed"], json_value!("dep 3.0"));
        assert_eq!(
            value["inspect_argv"],
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
    "requested": result["explanation"]["requested"],
    "evidence": result["explanation"]["evidence"],
    "inspect_argv": result["explanation"]["inspect_argv"],
}})
"#
        ))?;
        let output = eval_program(&engine_state, &mut stack, &program, PipelineData::empty())?;
        let value = json::pipeline_to_json_value(output, nu_protocol::Span::unknown())?;
        assert_eq!(value["ok"], json_value!(false));
        assert_eq!(
            value["kind"],
            json_value!("python_package_resolution_failed")
        );
        assert_eq!(value["requested"], json_value!(["alpha==1", "beta==2"]));
        assert!(value["evidence"]
            .as_str()
            .expect("resolver evidence")
            .contains("ResolutionImpossible"));
        assert_eq!(
            value["inspect_argv"],
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
            r#"hook("run.after_failure", family="generic", handler="native.after_failure", priority=100)
"#,
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
            r#"hook("run.after_failure", family="generic", handler="native.after_failure", priority=100)
"#,
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
            r#"hook("run.after_success", family="media", argv0=["ffmpeg","ffprobe"], argv0_prefix=["ffmpeg-","ffprobe-"], handler="media.after_success", priority=100)
"#,
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
            r#"hook("run.after_timeout", family="generic", handler="build.after_timeout", priority=100)
"#,
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
