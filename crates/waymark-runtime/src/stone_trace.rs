// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where an uncaught error came from: the file, line, and code of each
//! statement it left, the function each was in, and the values of the
//! variables on each of those lines.

use std::sync::Arc;

use nu_protocol::{ShellError, Span, Value};
use serde_json::{json, Value as JsonValue};

use super::stone_containers::ItemRef;
use super::{Evaluator, RuntimeValue};
use crate::stone_ast::{AssignTarget, ComprehensionClause, Expr, FormattedStringPart, SourceFile, Stmt};

/// At most this many variables are shown per line.
const MAX_VALUES: usize = 8;
/// A value's text is cut to about this many characters.
const VALUE_CHARS: usize = 160;
/// Containers nested deeper than this are shown as `...`.
const VALUE_DEPTH: usize = 4;

/// Where the error now unwinding came from: one frame per function it left,
/// innermost first. A frame is the statement that was running in that
/// function (or at top level), recorded by the innermost block that saw the
/// error. Kept beside the error, not in it, so error types are unchanged;
/// `key` ties the frames to their error.
#[derive(Default)]
pub(crate) struct ErrorTrace {
    frames: Vec<TraceFrame>,
    /// True once the error has left a function: the caller's block records
    /// the next frame.
    open: bool,
    key: String,
}

struct TraceFrame {
    file: Option<Arc<SourceFile>>,
    offset: Option<u32>,
    function: Option<String>,
    /// The variables the statement reads and their values when the error
    /// left it. Text is made only if the error ends the program.
    values: Vec<(String, Captured)>,
}

/// A variable's value as the error left it: shared containers and small
/// values are kept (cheap to hold); a large plain value is turned into its
/// short text at once rather than copied.
pub(super) enum Captured {
    Value(RuntimeValue),
    Text(String),
}

impl Captured {
    fn text(&self) -> Option<String> {
        match self {
            Captured::Value(value) => preview(value),
            Captured::Text(text) => Some(text.clone()),
        }
    }
}

/// What ties recorded frames to an error: its message.
fn error_trace_key(error: &ShellError) -> String {
    match error {
        ShellError::Generic(generic) => generic.msg.to_string(),
        other => other.to_string(),
    }
}

impl ErrorTrace {
    /// True while frames are recorded for an error not yet handled.
    pub(super) fn is_recording(&self) -> bool {
        !self.frames.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.frames.clear();
        self.open = false;
        self.key.clear();
    }

    /// `error` is leaving a statement: true when that statement is the
    /// next frame to record (the error is new, or has just left a function).
    pub(super) fn wants_frame(&mut self, error: &ShellError) -> bool {
        let key = error_trace_key(error);
        if self.frames.is_empty() || key != self.key {
            // A new error (anything recorded before was handled).
            self.clear();
            self.key = key;
            self.open = true;
        }
        self.open
    }

    pub(super) fn push(
        &mut self,
        file: Option<Arc<SourceFile>>,
        offset: Option<u32>,
        values: Vec<(String, Captured)>,
    ) {
        self.frames.push(TraceFrame {
            file,
            offset,
            function: None,
            values,
        });
        self.open = false;
    }

    /// The error left function `name`.
    pub(super) fn leave_function(&mut self, name: &str) {
        if let Some(frame) = self.frames.last_mut() {
            if !self.open {
                frame.function = Some(name.to_string());
                self.open = true;
            }
        }
    }

    /// The frames for `error`, outermost first, as `{file, line, code,
    /// function, values}` records; None when none were recorded for it.
    pub(crate) fn take_json(&mut self, error: &ShellError) -> Option<JsonValue> {
        if self.frames.is_empty() || error_trace_key(error) != self.key {
            self.clear();
            return None;
        }
        let frames = self
            .frames
            .iter()
            .rev()
            .map(|frame| {
                let mut fields = serde_json::Map::new();
                if let Some(file) = &frame.file {
                    fields.insert("file".into(), JsonValue::String(file.name.clone()));
                    if let Some(offset) = frame.offset {
                        let (line, code) = file.line_at(offset);
                        fields.insert("line".into(), json!(line));
                        fields.insert("code".into(), JsonValue::String(code.to_string()));
                    }
                }
                if let Some(function) = &frame.function {
                    fields.insert("function".into(), JsonValue::String(function.clone()));
                }
                let values = frame
                    .values
                    .iter()
                    .filter_map(|(name, value)| {
                        value.text().map(|text| (name.clone(), JsonValue::String(text)))
                    })
                    .collect::<serde_json::Map<_, _>>();
                if !values.is_empty() {
                    fields.insert("values".into(), JsonValue::Object(values));
                }
                JsonValue::Object(fields)
            })
            .collect();
        self.clear();
        Some(JsonValue::Array(frames))
    }
}

impl Evaluator<'_> {
    /// The variables `statement` reads (on its own line: a compound
    /// statement's condition or iterable, not its body) that hold data,
    /// with their values' text.
    pub(super) fn line_values(&self, statement: &Stmt) -> Vec<(String, Captured)> {
        let mut names = Vec::new();
        line_names(statement, &mut names);
        let mut values: Vec<(String, Captured)> = Vec::new();
        for name in names {
            if values.len() == MAX_VALUES {
                break;
            }
            // A module's names are qualified (`lib.LIMIT`); the line says
            // `LIMIT`.
            let shown = name.rsplit('.').next().unwrap_or(name);
            if values.iter().any(|(known, _)| known == shown) {
                continue;
            }
            let Some(value) = self.state.get_local(name) else {
                continue;
            };
            if value.opaque_repr().is_some() || matches!(value, RuntimeValue::Type(_)) {
                continue;
            }
            let large = match &value {
                RuntimeValue::Nu(Value::String { val, .. }) => val.len() > VALUE_CHARS,
                RuntimeValue::Str(text) => text.text.len() > VALUE_CHARS,
                RuntimeValue::Nu(Value::List { .. } | Value::Record { .. }) => true,
                _ => false,
            };
            let captured = if large {
                match preview(&value) {
                    Some(text) => Captured::Text(text),
                    None => continue,
                }
            } else {
                Captured::Value(value)
            };
            values.push((shown.to_string(), captured));
        }
        values
    }
}

/// The names `statement` reads on its own line, in source order. A plain
/// assignment's target is left out: its old value is not what the line
/// computes.
fn line_names<'a>(statement: &'a Stmt, names: &mut Vec<&'a str>) {
    match statement {
        Stmt::Assign { target, value } => {
            if !matches!(target, AssignTarget::Name(_) | AssignTarget::Tuple(_)) {
                target_names(target, names);
            }
            expr_names(value, &[], names);
        }
        Stmt::AugAssign { target, value, .. } => {
            target_names(target, names);
            expr_names(value, &[], names);
        }
        Stmt::For { iter, .. } => expr_names(iter, &[], names),
        Stmt::While { condition, .. } | Stmt::If { condition, .. } => {
            expr_names(condition, &[], names)
        }
        Stmt::With { context, .. } => expr_names(context, &[], names),
        Stmt::Raise(Some(expr)) | Stmt::Return(Some(expr)) | Stmt::Expr(expr) => {
            expr_names(expr, &[], names)
        }
        _ => {}
    }
}

fn target_names<'a>(target: &'a AssignTarget, names: &mut Vec<&'a str>) {
    match target {
        AssignTarget::Name(name) => names.push(name),
        AssignTarget::Tuple(items) => names.extend(items.iter().map(String::as_str)),
        AssignTarget::Unpack(items) => {
            for item in items {
                target_names(item, names);
            }
        }
        AssignTarget::Subscript { value, index } => {
            target_names(value, names);
            expr_names(index, &[], names);
        }
        AssignTarget::Attribute { value, .. } => target_names(value, names),
        AssignTarget::Expr(expr) => expr_names(expr, &[], names),
    }
}

/// Names read by `expr`, except those in `bound` (a lambda's parameters or
/// a comprehension's variables, which have no value once the error is
/// raised).
fn expr_names<'a>(expr: &'a Expr, bound: &[&str], names: &mut Vec<&'a str>) {
    let each = |expr: &'a Expr, names: &mut Vec<&'a str>| expr_names(expr, bound, names);
    match expr {
        Expr::None | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => {}
        Expr::Name(name) => {
            if !bound.contains(&name.as_str()) {
                names.push(name);
            }
        }
        Expr::FormattedString(parts) => {
            for part in parts {
                if let FormattedStringPart::Expr(expr) | FormattedStringPart::Formatted { expr, .. } =
                    part
                {
                    each(expr, names);
                }
            }
        }
        Expr::List(values) | Expr::Tuple(values) | Expr::BoolOp { values, .. } => {
            for value in values {
                each(value, names);
            }
        }
        Expr::Record(fields) => {
            for (_, value) in fields {
                each(value, names);
            }
        }
        Expr::ListComprehension { elt, clauses } | Expr::Generator { elt, clauses } => {
            comprehension_names(clauses, &[elt], bound, names)
        }
        Expr::DictComprehension {
            key,
            value,
            clauses,
        } => comprehension_names(clauses, &[key, value], bound, names),
        Expr::Subscript { value, index } => {
            each(value, names);
            each(index, names);
        }
        Expr::Attribute { value, .. }
        | Expr::Not(value)
        | Expr::Neg(value)
        | Expr::Invert(value)
        | Expr::Await(value) => each(value, names),
        Expr::Slice { lower, upper, step } => {
            for part in [lower, upper, step].into_iter().flatten() {
                each(part, names);
            }
        }
        Expr::Compare {
            left, comparators, ..
        } => {
            each(left, names);
            for value in comparators {
                each(value, names);
            }
        }
        Expr::Conditional {
            then_expr,
            condition,
            else_expr,
        } => {
            each(condition, names);
            each(then_expr, names);
            each(else_expr, names);
        }
        Expr::Add { left, right }
        | Expr::Sub { left, right }
        | Expr::Mul { left, right }
        | Expr::Div { left, right }
        | Expr::FloorDiv { left, right }
        | Expr::Mod { left, right }
        | Expr::BitAnd { left, right }
        | Expr::BitOr { left, right }
        | Expr::BitXor { left, right }
        | Expr::LShift { left, right }
        | Expr::RShift { left, right } => {
            each(left, names);
            each(right, names);
        }
        Expr::Lambda { params, body, .. } => {
            let mut inner = bound.to_vec();
            inner.extend(params.iter().map(String::as_str));
            expr_names(body, &inner, names);
        }
        Expr::MethodCall {
            receiver,
            positional,
            named,
            ..
        } => {
            each(receiver, names);
            for value in positional.iter().chain(named.iter().map(|(_, value)| value)) {
                each(value, names);
            }
        }
        Expr::CallValue {
            callee,
            positional,
            named,
        } => {
            each(callee, names);
            for value in positional.iter().chain(named.iter().map(|(_, value)| value)) {
                each(value, names);
            }
        }
        // The callee is a function, not data: only the arguments count.
        Expr::Call(call) => {
            for value in call
                .positional
                .iter()
                .chain(call.named.iter().map(|(_, value)| value))
            {
                each(value, names);
            }
        }
    }
}

fn comprehension_names<'a>(
    clauses: &'a [ComprehensionClause],
    results: &[&'a Expr],
    bound: &[&str],
    names: &mut Vec<&'a str>,
) {
    let mut inner = bound.to_vec();
    for clause in clauses {
        // A clause's iterable sees the variables of the clauses before it.
        expr_names(&clause.iter, &inner, names);
        inner.extend(clause.targets.iter().map(String::as_str));
        for filter in &clause.filters {
            expr_names(filter, &inner, names);
        }
    }
    for result in results {
        expr_names(result, &inner, names);
    }
}

/// Short Python-style text for a data value: containers show their first
/// items and how many more there are, long strings their start and length.
/// None for values that are not data (functions, types, the model).
pub(super) fn preview(value: &RuntimeValue) -> Option<String> {
    if value.opaque_repr().is_some() || matches!(value, RuntimeValue::Type(_)) {
        return None;
    }
    let mut writer = Preview {
        out: String::new(),
    };
    writer.runtime(value, 0);
    Some(writer.out)
}

struct Preview {
    out: String,
}

impl Preview {
    fn full(&self) -> bool {
        self.out.len() >= VALUE_CHARS
    }

    /// Write `count` items with `item(index)`, stopping once the text is
    /// long enough and saying how many were left out.
    fn items(&mut self, open: &str, close: &str, count: usize, mut item: impl FnMut(&mut Self, usize) -> bool) {
        self.out.push_str(open);
        let mut shown = 0;
        while shown < count {
            if shown > 0 {
                self.out.push_str(", ");
            }
            if self.full() || !item(self, shown) {
                break;
            }
            shown += 1;
        }
        self.more(count - shown);
        self.out.push_str(close);
    }

    fn item(&mut self, item: ItemRef<'_>, depth: usize) {
        match item {
            ItemRef::Plain(value) => self.nu(value, depth + 1),
            ItemRef::Shared(value) => self.runtime(&value, depth + 1),
        }
    }

    fn more(&mut self, left: usize) {
        if left > 0 {
            self.out.push_str(&format!("... {left} more"));
        }
    }

    fn runtime(&mut self, value: &RuntimeValue, depth: usize) {
        if depth > VALUE_DEPTH {
            self.out.push_str("...");
            return;
        }
        match value {
            RuntimeValue::Nu(value) => self.nu(value, depth),
            RuntimeValue::List(list) => {
                self.out.push('[');
                let mut shown = 0;
                let _ = list.try_for_each(|index, item| {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    if self.full() {
                        return Err(());
                    }
                    self.item(item, depth);
                    shown += 1;
                    Ok(())
                });
                self.more(list.len() - shown);
                self.out.push(']');
            }
            RuntimeValue::Dict(dict) => {
                self.out.push('{');
                let mut shown = 0;
                let _ = dict.try_for_each(|key, item| {
                    if shown > 0 {
                        self.out.push_str(", ");
                    }
                    if self.full() {
                        return Err(());
                    }
                    self.string(key);
                    self.out.push_str(": ");
                    self.item(item, depth);
                    shown += 1;
                    Ok(())
                });
                self.more(dict.len() - shown);
                self.out.push('}');
            }
            RuntimeValue::Record(record) => {
                let fields = record.shared_fields();
                let names = &record.ty.fields;
                self.items(&format!("{}(", record.ty.name), ")", fields.len(), |this, index| {
                    this.out.push_str(&names[index].name);
                    this.out.push('=');
                    this.runtime(&fields[index], depth + 1);
                    true
                });
            }
            RuntimeValue::Tuple(items) => {
                let items = items.iter().collect::<Vec<_>>();
                self.items("(", ")", items.len(), |this, index| {
                    this.runtime(items[index], depth + 1);
                    true
                });
            }
            RuntimeValue::Set(set) => {
                let items = set.values();
                self.items("{", "}", items.len(), |this, index| {
                    this.runtime(&items[index], depth + 1);
                    true
                });
            }
            other => match super::stone_records::python_repr(other) {
                Ok(text) => self.clipped(&text),
                Err(_) => self.out.push_str("<value>"),
            },
        }
    }

    fn nu(&mut self, value: &Value, depth: usize) {
        if depth > VALUE_DEPTH {
            self.out.push_str("...");
            return;
        }
        match value {
            Value::String { val, .. } => self.string(val),
            Value::List { vals, .. } => {
                self.items("[", "]", vals.len(), |this, index| {
                    this.nu(&vals[index], depth + 1);
                    true
                });
            }
            Value::Record { val, .. } => {
                let entries = val.iter().collect::<Vec<_>>();
                self.items("{", "}", entries.len(), |this, index| {
                    let (key, item) = entries[index];
                    this.string(key);
                    this.out.push_str(": ");
                    this.nu(item, depth + 1);
                    true
                });
            }
            other => match crate::stone_builtins::python_repr(other) {
                Ok(text) => self.clipped(&text),
                Err(_) => self.out.push_str("<value>"),
            },
        }
    }

    /// A string's repr; a long one shows its start and its length.
    fn string(&mut self, text: &str) {
        let room = VALUE_CHARS.saturating_sub(self.out.len()).max(20);
        let chars = text.chars().count();
        if chars <= room {
            self.repr(text);
        } else {
            let start = text.chars().take(room).collect::<String>();
            self.repr(&start);
            self.out.push_str(&format!("... ({chars} chars)"));
        }
    }

    fn repr(&mut self, text: &str) {
        match crate::stone_builtins::python_repr(&Value::string(text, Span::unknown())) {
            Ok(text) => self.out.push_str(&text),
            Err(_) => self.out.push_str("<str>"),
        }
    }

    fn clipped(&mut self, text: &str) {
        let room = VALUE_CHARS.saturating_sub(self.out.len()).max(20);
        if text.chars().count() <= room {
            self.out.push_str(text);
        } else {
            self.out.extend(text.chars().take(room));
            self.out.push_str("...");
        }
    }
}
