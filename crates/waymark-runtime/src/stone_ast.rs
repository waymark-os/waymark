// SPDX-License-Identifier: MIT OR Apache-2.0

use std::borrow::Cow;

use nu_protocol::{shell_error::generic::GenericError, ShellError};
use ruff_python_ast as py;

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub statements: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Assign {
        target: AssignTarget,
        value: Expr,
    },
    AugAssign {
        target: AssignTarget,
        op: AugOp,
        value: Expr,
    },
    For {
        targets: Vec<String>,
        iter: Expr,
        body: Vec<Stmt>,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
    },
    If {
        condition: Expr,
        then_branch: Vec<Stmt>,
        else_branch: Vec<Stmt>,
    },
    With {
        is_async: bool,
        target: Option<String>,
        context: Expr,
        body: Vec<Stmt>,
    },
    Try {
        body: Vec<Stmt>,
        handlers: Vec<ExceptHandler>,
    },
    FunctionDef(FunctionDef),
    Return(Option<Expr>),
    Break,
    Continue,
    Pass,
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDef {
    pub name: String,
    pub is_async: bool,
    pub params: Vec<FunctionParam>,
    pub return_type: StoneType,
    pub body: Vec<Stmt>,
    pub stage: Option<Box<StageDecorator>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageDecorator {
    pub evidence: Expr,
    pub name: Option<Expr>,
    pub goal: Option<Expr>,
    pub inputs: Option<Expr>,
    pub agent_loop: Option<Expr>,
    pub repair: Option<Expr>,
    pub max_attempts: Option<Expr>,
    pub max_actions: Option<Expr>,
    pub checkpoint: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionParam {
    pub name: String,
    pub ty: StoneType,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoneType {
    Any,
    AttemptAcceptance,
    AttemptHandle,
    AttemptOutcome,
    AttemptScope,
    Bool,
    Float,
    Int,
    List,
    None,
    Record,
    SemanticFrontier,
    Str,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssignTarget {
    Name(String),
    Tuple(Vec<String>),
    Subscript {
        value: Box<AssignTarget>,
        index: Expr,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    None,
    Bool(bool),
    Int(String),
    Float(f64),
    String(String),
    FormattedString(Vec<FormattedStringPart>),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    ListComprehension {
        elt: Box<Expr>,
        clauses: Vec<ComprehensionClause>,
    },
    Record(Vec<(String, Expr)>),
    DictComprehension {
        key: Box<Expr>,
        value: Box<Expr>,
        clauses: Vec<ComprehensionClause>,
    },
    Name(String),
    Subscript {
        value: Box<Expr>,
        index: Box<Expr>,
    },
    Attribute {
        value: Box<Expr>,
        attr: String,
    },
    Slice {
        lower: Option<Box<Expr>>,
        upper: Option<Box<Expr>>,
    },
    Compare {
        left: Box<Expr>,
        ops: Vec<CompareOp>,
        comparators: Vec<Expr>,
    },
    BoolOp {
        op: BoolOp,
        values: Vec<Expr>,
    },
    Conditional {
        then_expr: Box<Expr>,
        condition: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Invert(Box<Expr>),
    Await(Box<Expr>),
    Add {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Sub {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Mul {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Div {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    FloorDiv {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Mod {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    BitAnd {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    BitOr {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    BitXor {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    LShift {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    RShift {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Generator {
        elt: Box<Expr>,
        clauses: Vec<ComprehensionClause>,
    },
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        positional: Vec<Expr>,
        named: Vec<(String, Expr)>,
    },
    Call(Call),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormattedStringPart {
    Literal(String),
    Expr(Expr),
    Formatted { expr: Expr, spec: StoneFormatSpec },
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoneFormatSpec {
    Fixed { precision: usize },
    ZeroPadInt { width: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    In,
    NotIn,
    Is,
    IsNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AugOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    LShift,
    RShift,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExceptHandler {
    pub name: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub positional: Vec<Expr>,
    pub named: Vec<(String, Expr)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComprehensionClause {
    pub targets: Vec<String>,
    pub iter: Expr,
    pub filters: Vec<Expr>,
}

pub fn lower_source(source: &str) -> Result<Program, ShellError> {
    let expanded = expand_workflow_block_syntax(source)?;
    let parsed = ruff_python_parser::parse_module(&expanded).map_err(|err| {
        let mut error = GenericError::new_internal("python parse error", err.to_string())
            .with_code("stone_parse_error");
        if source
            .lines()
            .any(|line| line.trim_start().starts_with("//"))
        {
            error = error.with_help(
                "Stone comments use #. The // operator is floor division, not a comment.",
            );
        }
        ShellError::Generic(error)
    })?;

    lower_module(parsed.into_syntax())
}

fn expand_workflow_block_syntax(source: &str) -> Result<Cow<'_, str>, ShellError> {
    if !source
        .lines()
        .any(|line| line.starts_with("workflow ") || line.starts_with("run "))
    {
        return Ok(Cow::Borrowed(source));
    }

    let lines = source.lines().collect::<Vec<_>>();
    let mut output = Vec::with_capacity(lines.len());
    let mut workflows = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if let Some(name) = workflow_block_name(line)? {
            if workflows.iter().any(|known| known == &name) {
                return Err(workflow_syntax_error(format!(
                    "duplicate workflow block `{name}`"
                )));
            }
            workflows.push(name.clone());
            let workflow_index = workflows.len() - 1;
            index += 1;
            let mut stages = Vec::new();
            let mut stage_bindings = Vec::new();
            while index < lines.len() {
                let current = lines[index];
                if current.trim().is_empty() || current.trim_start().starts_with('#') {
                    index += 1;
                    continue;
                }
                let indent = leading_spaces(current)?;
                if indent == 0 {
                    break;
                }
                if indent != 4 || !current.trim_start().starts_with("stage ") {
                    return Err(workflow_syntax_error(format!(
                        "workflow `{name}` accepts stage blocks indented by four spaces; found `{}`",
                        current.trim()
                    )));
                }

                let header_start = index;
                let mut header = current.trim().to_string();
                while !stage_header_complete(&header) {
                    index += 1;
                    if index >= lines.len() {
                        return Err(workflow_syntax_error(format!(
                            "unterminated stage header in workflow `{name}`"
                        )));
                    }
                    let continuation = lines[index];
                    if leading_spaces(continuation)? < 4 {
                        return Err(workflow_syntax_error(format!(
                            "unterminated stage header beginning on line {}",
                            header_start + 1
                        )));
                    }
                    header.push(' ');
                    header.push_str(continuation.trim());
                }
                let (stage_name, arguments) = parse_stage_header(&header)?;
                if stages.iter().any(|known| known == &stage_name) {
                    return Err(workflow_syntax_error(format!(
                        "workflow `{name}` has duplicate stage `{stage_name}`"
                    )));
                }
                stages.push(stage_name.clone());
                let stage_binding = format!(
                    "__stone_workflow_{workflow_index}_stage_{}",
                    stage_bindings.len()
                );
                stage_bindings.push(stage_binding.clone());
                index += 1;

                let body_start = index;
                while index < lines.len() {
                    let body_line = lines[index];
                    if body_line.trim().is_empty() {
                        index += 1;
                        continue;
                    }
                    let indent = leading_spaces(body_line)?;
                    if indent <= 4 {
                        break;
                    }
                    index += 1;
                }
                let (actions, contracts, has_agent_loop) =
                    parse_stage_body(&name, &stage_name, &lines[body_start..index])?;
                if contracts.is_empty() {
                    return Err(workflow_syntax_error(format!(
                        "workflow `{name}` stage `{stage_name}` requires at least one direct `ensure <typed evidence>` contract"
                    )));
                }

                let evidence = format!("all_evidence({})", contracts.join(", "));
                let generated = if has_agent_loop {
                    "agent_loop=True"
                } else {
                    "agent_loop=False"
                };
                let decorator = if arguments.trim().is_empty() {
                    format!("@stage(evidence={evidence}, name={stage_name:?}, {generated})")
                } else {
                    format!(
                        "@stage(evidence={evidence}, name={stage_name:?}, {generated}, {arguments})"
                    )
                };
                output.push(decorator);
                output.push(format!("def {stage_binding}(__stone_step):"));
                if has_agent_loop {
                    output.push("    return agent_loop(__stone_step)".to_string());
                } else {
                    let mut emitted_action = false;
                    for action in actions {
                        if action.trim().is_empty() {
                            output.push(String::new());
                        } else {
                            output.push(action);
                            emitted_action = true;
                        }
                    }
                    if !emitted_action {
                        output.push("    pass".to_string());
                    }
                    output
                        .push("    return {\"ok\": True, \"kind\": \"stage_action\"}".to_string());
                }
                output.push(String::new());
            }
            if stages.is_empty() {
                return Err(workflow_syntax_error(format!(
                    "workflow `{name}` requires at least one stage"
                )));
            }
            output.push(format!(
                "{name} = workflow(\"{name}\", {})",
                stage_bindings.join(", ")
            ));
            output.push(String::new());
            continue;
        }

        if let Some(name) = run_workflow_name(line)? {
            if !workflows.iter().any(|known| known == &name) {
                return Err(workflow_syntax_error(format!(
                    "`run {name}` does not name a preceding workflow block"
                )));
            }
            output.push(format!("emit(workflow_main({name}))"));
        } else {
            output.push(line.to_string());
        }
        index += 1;
    }

    let mut expanded = output.join("\n");
    if source.ends_with('\n') {
        expanded.push('\n');
    }
    Ok(Cow::Owned(expanded))
}

fn workflow_block_name(line: &str) -> Result<Option<String>, ShellError> {
    if !line.starts_with("workflow ") {
        return Ok(None);
    }
    let Some(name) = line
        .strip_prefix("workflow ")
        .and_then(|rest| rest.strip_suffix(':'))
        .map(str::trim)
    else {
        return Err(workflow_syntax_error(
            "workflow block header must be `workflow <name>:`",
        ));
    };
    validate_workflow_syntax_identifier(name, "workflow")?;
    Ok(Some(name.to_string()))
}

fn run_workflow_name(line: &str) -> Result<Option<String>, ShellError> {
    if !line.starts_with("run ") || line.contains('(') {
        return Ok(None);
    }
    let name = line
        .strip_prefix("run ")
        .expect("run prefix checked")
        .trim();
    validate_workflow_syntax_identifier(name, "run target")?;
    Ok(Some(name.to_string()))
}

fn stage_header_complete(header: &str) -> bool {
    header.ends_with(':') && delimiter_balance(header.trim_end_matches(':')) == Some(0)
}

fn parse_stage_header(header: &str) -> Result<(String, String), ShellError> {
    let Some(rest) = header
        .strip_prefix("stage ")
        .and_then(|value| value.strip_suffix(':'))
        .map(str::trim)
    else {
        return Err(workflow_syntax_error(
            "stage header must be `stage <name>(...):`",
        ));
    };
    let (name, arguments) = match rest.find('(') {
        Some(open) => {
            if !rest.ends_with(')') {
                return Err(workflow_syntax_error("stage arguments must end with `):`"));
            }
            (rest[..open].trim(), rest[open + 1..rest.len() - 1].trim())
        }
        None => (rest, ""),
    };
    validate_workflow_syntax_identifier(name, "stage")?;
    Ok((name.to_string(), arguments.to_string()))
}

fn parse_stage_body(
    workflow: &str,
    stage: &str,
    lines: &[&str],
) -> Result<(Vec<String>, Vec<String>, bool), ShellError> {
    let mut actions = Vec::new();
    let mut contracts = Vec::new();
    let mut agent_loop = false;
    let mut other_action = false;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            actions.push(String::new());
            index += 1;
            continue;
        }
        let indent = leading_spaces(line)?;
        if indent < 8 {
            return Err(workflow_syntax_error(format!(
                "workflow `{workflow}` stage `{stage}` body must be indented by eight spaces"
            )));
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("ensure ") {
            if indent != 8 {
                return Err(workflow_syntax_error(format!(
                    "workflow `{workflow}` stage `{stage}` contracts must use direct stage indentation"
                )));
            }
            let contract = trimmed
                .strip_prefix("ensure ")
                .expect("ensure prefix checked")
                .trim();
            if contract.is_empty() {
                return Err(workflow_syntax_error(format!(
                    "workflow `{workflow}` stage `{stage}` ensure requires a typed evidence expression"
                )));
            }
            let mut contract = contract.to_string();
            while delimiter_balance(&contract).is_some_and(|balance| balance > 0) {
                index += 1;
                if index >= lines.len() {
                    return Err(workflow_syntax_error(format!(
                        "workflow `{workflow}` stage `{stage}` has an unterminated ensure expression"
                    )));
                }
                let continuation = lines[index];
                if continuation.trim().is_empty() || leading_spaces(continuation)? < 8 {
                    return Err(workflow_syntax_error(format!(
                        "workflow `{workflow}` stage `{stage}` multiline ensure continuation must remain inside the stage body"
                    )));
                }
                contract.push(' ');
                contract.push_str(continuation.trim());
            }
            if delimiter_balance(&contract) != Some(0) {
                return Err(workflow_syntax_error(format!(
                    "workflow `{workflow}` stage `{stage}` ensure contract has unbalanced delimiters"
                )));
            }
            contracts.push(contract);
            index += 1;
            continue;
        }
        if indent == 8 && trimmed == "agent_loop()" {
            if agent_loop {
                return Err(workflow_syntax_error(format!(
                    "workflow `{workflow}` stage `{stage}` may contain at most one agent_loop()"
                )));
            }
            agent_loop = true;
            index += 1;
            continue;
        }
        if !trimmed.starts_with('#') {
            other_action = true;
        }
        actions.push(line[4..].to_string());
        index += 1;
    }
    if agent_loop && other_action {
        return Err(workflow_syntax_error(format!(
            "workflow `{workflow}` stage `{stage}` agent_loop() must be the only executable stage action; move setup into another stage or an agent-loop adapter"
        )));
    }
    Ok((actions, contracts, agent_loop))
}

fn leading_spaces(line: &str) -> Result<usize, ShellError> {
    if line.as_bytes().contains(&b'\t') {
        return Err(workflow_syntax_error(
            "workflow block syntax requires spaces, not tabs",
        ));
    }
    Ok(line.len() - line.trim_start_matches(' ').len())
}

fn delimiter_balance(source: &str) -> Option<i32> {
    let mut balance = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' | '[' | '{' => balance += 1,
            ')' | ']' | '}' => {
                balance -= 1;
                if balance < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    (quote.is_none()).then_some(balance)
}

fn validate_workflow_syntax_identifier(name: &str, kind: &str) -> Result<(), ShellError> {
    let mut characters = name.chars();
    let valid = characters
        .next()
        .map(|character| character == '_' || character.is_ascii_alphabetic())
        .unwrap_or(false)
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if !valid {
        return Err(workflow_syntax_error(format!(
            "{kind} name `{name}` must be a Stone identifier"
        )));
    }
    Ok(())
}

fn workflow_syntax_error(message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal("Stone workflow syntax error", message.into())
            .with_code("stone_workflow_syntax_error")
            .with_help(
                "Use `workflow name:`, four-space `stage name(...):` blocks, direct `ensure evidence(...)` contracts, and `run name`.",
            ),
    )
}

fn lower_module(module: py::ModModule) -> Result<Program, ShellError> {
    let mut statements = Vec::with_capacity(module.body.len());
    for statement in module.body {
        statements.push(lower_stmt(statement)?);
    }

    Ok(Program { statements })
}

fn lower_stmt(statement: py::Stmt) -> Result<Stmt, ShellError> {
    match statement {
        py::Stmt::Assign(assign) => lower_assign(assign),
        py::Stmt::AugAssign(assign) => lower_aug_assign(assign),
        py::Stmt::Break(_) => Ok(Stmt::Break),
        py::Stmt::Continue(_) => Ok(Stmt::Continue),
        py::Stmt::For(for_stmt) => lower_for(for_stmt),
        py::Stmt::FunctionDef(function) => lower_function_def(function),
        py::Stmt::If(if_stmt) => lower_if(if_stmt),
        py::Stmt::Import(import) => Err(unsupported_import_statement(&import.names)),
        py::Stmt::ImportFrom(import) => Err(unsupported_import_from_statement(
            import.module.as_ref().map(|module| module.to_string()),
            &import.names,
        )),
        py::Stmt::Pass(_) => Ok(Stmt::Pass),
        py::Stmt::Return(return_stmt) => lower_return(return_stmt),
        py::Stmt::Try(try_stmt) => lower_try(try_stmt),
        py::Stmt::While(while_stmt) => lower_while(while_stmt),
        py::Stmt::With(with_stmt) => lower_with(with_stmt),
        py::Stmt::Expr(expr) => Ok(Stmt::Expr(lower_expr(*expr.value)?)),
        unsupported => Err(unsupported_error("statement", &unsupported)),
    }
}

fn lower_function_def(function: py::StmtFunctionDef) -> Result<Stmt, ShellError> {
    let is_async = function.is_async;
    let stage = lower_function_stage_decorator(function.decorator_list)?;
    if is_async && stage.is_some() {
        return Err(unsupported_message(
            "function definition",
            "@stage on async def is not supported yet; put async attempt control inside an undecorated helper",
        ));
    }
    if function.type_params.is_some() {
        return Err(unsupported_message(
            "function definition",
            "type parameters are not supported",
        ));
    }
    let parameters = function.parameters;
    if !parameters.posonlyargs.is_empty()
        || parameters.vararg.is_some()
        || !parameters.kwonlyargs.is_empty()
        || parameters.kwarg.is_some()
    {
        return Err(unsupported_message(
            "function definition",
            "only simple positional parameters are supported",
        ));
    }
    let mut params = Vec::with_capacity(parameters.args.len());
    for parameter in parameters.args {
        let name = parameter.name().to_string();
        let ty = match parameter.parameter.annotation.as_deref() {
            Some(annotation) => lower_type_annotation(annotation)?,
            None => StoneType::Any,
        };
        let default = parameter
            .default
            .map(|default| lower_function_default(*default))
            .transpose()?;
        params.push(FunctionParam { name, ty, default });
    }
    let return_type = match function.returns {
        Some(return_type) => lower_type_annotation(&return_type)?,
        None => StoneType::Any,
    };
    Ok(Stmt::FunctionDef(FunctionDef {
        name: function.name.to_string(),
        is_async,
        params,
        return_type,
        body: lower_stmt_block(function.body)?,
        stage: stage.map(Box::new),
    }))
}

fn lower_function_stage_decorator(
    decorators: Vec<py::Decorator>,
) -> Result<Option<StageDecorator>, ShellError> {
    let [] = decorators.as_slice() else {
        if decorators.len() != 1 {
            return Err(unsupported_message(
                "function definition",
                "a Stone function accepts at most one @stage(...) decorator",
            ));
        }
        let decorator = decorators
            .into_iter()
            .next()
            .expect("one decorator checked");
        let py::Expr::Call(call) = decorator.expression else {
            return Err(unsupported_message(
                "function definition",
                "only @stage(...) decorators are supported; include parentheses and evidence=",
            ));
        };
        let py::Expr::Name(name) = *call.func else {
            return Err(unsupported_message(
                "function definition",
                "only the built-in @stage(...) decorator is supported",
            ));
        };
        if name.id.as_str() != "stage" {
            return Err(unsupported_message(
                "function definition",
                format!(
                    "unsupported decorator @{}; only @stage(...) is supported",
                    name.id
                ),
            ));
        }
        if !call.arguments.args.is_empty() {
            return Err(unsupported_message(
                "stage declaration",
                "@stage(...) accepts only evidence=, name=, goal=, inputs=, agent_loop=, repair=, max_attempts=, max_actions=, and checkpoint= keyword arguments",
            ));
        }
        let mut evidence = None;
        let mut stage_name = None;
        let mut goal = None;
        let mut inputs = None;
        let mut agent_loop = None;
        let mut repair = None;
        let mut max_attempts = None;
        let mut max_actions = None;
        let mut checkpoint = None;
        for keyword in call.arguments.keywords {
            let Some(name) = keyword.arg else {
                return Err(unsupported_message(
                    "stage declaration",
                    "@stage(...) does not support keyword spread",
                ));
            };
            let slot = match name.as_str() {
                "evidence" => &mut evidence,
                "name" => &mut stage_name,
                "goal" => &mut goal,
                "inputs" => &mut inputs,
                "agent_loop" => &mut agent_loop,
                "repair" => &mut repair,
                "max_attempts" => &mut max_attempts,
                "max_actions" => &mut max_actions,
                "checkpoint" => &mut checkpoint,
                other => {
                    return Err(unsupported_message(
                        "stage declaration",
                        format!(
                            "unsupported @stage field `{other}`; expected evidence, name, goal, inputs, agent_loop, repair, max_attempts, max_actions, or checkpoint"
                        ),
                    ));
                }
            };
            if slot.is_some() {
                return Err(unsupported_message(
                    "stage declaration",
                    format!("duplicate @stage field `{name}`"),
                ));
            }
            *slot = Some(lower_expr(keyword.value)?);
        }
        let evidence = evidence.ok_or_else(|| {
            unsupported_message(
                "stage declaration",
                "@stage(...) requires evidence= so advancement is explicitly gated",
            )
        })?;
        return Ok(Some(StageDecorator {
            evidence,
            name: stage_name,
            goal,
            inputs,
            agent_loop,
            repair,
            max_attempts,
            max_actions,
            checkpoint,
        }));
    };
    Ok(None)
}

fn lower_function_default(default: py::Expr) -> Result<Expr, ShellError> {
    match default {
        py::Expr::List(_) | py::Expr::Dict(_) | py::Expr::Set(_) => Err(unsupported_message(
            "function definition",
            "mutable default parameter values are not supported",
        )),
        other => lower_expr(other),
    }
}

fn lower_return(return_stmt: py::StmtReturn) -> Result<Stmt, ShellError> {
    Ok(Stmt::Return(
        return_stmt
            .value
            .map(|value| lower_expr(*value))
            .transpose()?,
    ))
}

fn lower_type_annotation(annotation: &py::Expr) -> Result<StoneType, ShellError> {
    match annotation {
        py::Expr::Name(name) => match name.id.as_str() {
            "Any" | "any" => Ok(StoneType::Any),
            "attempt_acceptance" => Ok(StoneType::AttemptAcceptance),
            "attempt_handle" => Ok(StoneType::AttemptHandle),
            "attempt_outcome" => Ok(StoneType::AttemptOutcome),
            "attempt_scope" => Ok(StoneType::AttemptScope),
            "bool" => Ok(StoneType::Bool),
            "float" => Ok(StoneType::Float),
            "int" => Ok(StoneType::Int),
            "list" => Ok(StoneType::List),
            "None" => Ok(StoneType::None),
            "record" | "dict" => Ok(StoneType::Record),
            "semantic_frontier" => Ok(StoneType::SemanticFrontier),
            "str" => Ok(StoneType::Str),
            other => Err(unsupported_message(
                "type annotation",
                format!("unsupported type `{other}`"),
            )),
        },
        py::Expr::NoneLiteral(_) => Ok(StoneType::None),
        py::Expr::Subscript(subscript) => match subscript.value.as_ref() {
            py::Expr::Name(name) if name.id.as_str() == "list" => Ok(StoneType::List),
            _ => Err(unsupported_message(
                "type annotation",
                "only list[T] generic annotations are supported",
            )),
        },
        unsupported => Err(unsupported_error("type annotation", unsupported)),
    }
}

fn lower_assign(assign: py::StmtAssign) -> Result<Stmt, ShellError> {
    let [target] = assign.targets.as_slice() else {
        return Err(unsupported_message(
            "assignment",
            "multiple assignment targets are not supported yet",
        ));
    };

    let target = lower_assign_target(target)?;

    Ok(Stmt::Assign {
        target,
        value: lower_expr(*assign.value)?,
    })
}

fn lower_assign_target(target: &py::Expr) -> Result<AssignTarget, ShellError> {
    match target {
        py::Expr::Name(name) => Ok(AssignTarget::Name(name.id.to_string())),
        py::Expr::Tuple(tuple) => lower_tuple_assign_targets(&tuple.elts),
        py::Expr::List(list) => lower_tuple_assign_targets(&list.elts),
        py::Expr::Subscript(subscript) => Ok(AssignTarget::Subscript {
            value: Box::new(lower_assign_target(subscript.value.as_ref())?),
            index: lower_expr(*subscript.slice.clone())?,
        }),
        py::Expr::Attribute(attribute) => Err(unsupported_message(
            "assignment",
            format!(
                "attribute assignment like record.{} = value is not supported; use item assignment record[\"{}\"] = value",
                attribute.attr, attribute.attr
            ),
        )),
        _ => Err(unsupported_message(
            "assignment",
            "only simple name, fixed tuple/list, and item assignment are supported yet",
        )),
    }
}

fn lower_tuple_assign_targets(elements: &[py::Expr]) -> Result<AssignTarget, ShellError> {
    let mut targets = Vec::with_capacity(elements.len());
    for element in elements {
        let py::Expr::Name(name) = element else {
            return Err(unsupported_message(
                "assignment",
                "tuple/list destructuring only supports simple name targets",
            ));
        };
        targets.push(name.id.to_string());
    }
    if targets.is_empty() {
        return Err(unsupported_message(
            "assignment",
            "empty tuple/list destructuring is not supported",
        ));
    }
    Ok(AssignTarget::Tuple(targets))
}

fn lower_aug_assign(assign: py::StmtAugAssign) -> Result<Stmt, ShellError> {
    let target = lower_assign_target(&assign.target)?;
    let op = match assign.op {
        py::Operator::Add => AugOp::Add,
        py::Operator::Sub => AugOp::Sub,
        py::Operator::Mult => AugOp::Mul,
        py::Operator::Div => AugOp::Div,
        py::Operator::FloorDiv => AugOp::FloorDiv,
        py::Operator::Mod => AugOp::Mod,
        py::Operator::BitAnd => AugOp::BitAnd,
        py::Operator::BitOr => AugOp::BitOr,
        py::Operator::BitXor => AugOp::BitXor,
        py::Operator::LShift => AugOp::LShift,
        py::Operator::RShift => AugOp::RShift,
        unsupported => {
            return Err(unsupported_message(
                "augmented assignment",
                format!("unsupported augmented assignment operator: {unsupported:?}"),
            ));
        }
    };
    Ok(Stmt::AugAssign {
        target,
        op,
        value: lower_expr(*assign.value)?,
    })
}

fn lower_try(try_stmt: py::StmtTry) -> Result<Stmt, ShellError> {
    if try_stmt.is_star {
        return Err(unsupported_message(
            "try statement",
            "except* handlers are not supported",
        ));
    }
    if !try_stmt.orelse.is_empty() {
        return Err(unsupported_message(
            "try statement",
            "try/else is not supported yet",
        ));
    }
    if !try_stmt.finalbody.is_empty() {
        return Err(unsupported_message(
            "try statement",
            "finally is not supported yet",
        ));
    }
    if try_stmt.handlers.is_empty() {
        return Err(unsupported_message(
            "try statement",
            "try requires at least one except handler",
        ));
    }
    let mut handlers = Vec::with_capacity(try_stmt.handlers.len());
    for handler in try_stmt.handlers {
        let py::ExceptHandler::ExceptHandler(handler) = handler;
        match handler.type_.as_deref() {
            None => {}
            Some(py::Expr::Name(name)) if name.id.as_str() == "Exception" => {}
            Some(_) => {
                return Err(unsupported_message(
                    "try statement",
                    "only bare except and except Exception handlers are supported",
                ));
            }
        }
        handlers.push(ExceptHandler {
            name: handler.name.map(|name| name.to_string()),
            body: lower_stmt_block(handler.body)?,
        });
    }
    Ok(Stmt::Try {
        body: lower_stmt_block(try_stmt.body)?,
        handlers,
    })
}

fn lower_for(for_stmt: py::StmtFor) -> Result<Stmt, ShellError> {
    if for_stmt.is_async {
        return Err(unsupported_message(
            "for statement",
            "async for statements are not supported",
        ));
    }
    if !for_stmt.orelse.is_empty() {
        return Err(unsupported_message(
            "for statement",
            "for/else is not supported yet",
        ));
    }
    let targets = lower_loop_targets(*for_stmt.target)?;
    Ok(Stmt::For {
        targets,
        iter: lower_expr(*for_stmt.iter)?,
        body: lower_stmt_block(for_stmt.body)?,
    })
}

fn lower_loop_targets(target: py::Expr) -> Result<Vec<String>, ShellError> {
    match target {
        py::Expr::Name(target) => Ok(vec![target.id.to_string()]),
        py::Expr::Tuple(tuple) => {
            let mut targets = Vec::with_capacity(tuple.elts.len());
            for elt in tuple.elts {
                let py::Expr::Name(name) = elt else {
                    return Err(unsupported_message(
                        "for statement",
                        "only simple name tuple loop targets are supported yet",
                    ));
                };
                targets.push(name.id.to_string());
            }
            if targets.is_empty() {
                return Err(unsupported_message(
                    "for statement",
                    "empty tuple loop targets are not supported",
                ));
            }
            Ok(targets)
        }
        _ => Err(unsupported_message(
            "for statement",
            "only simple name and tuple loop targets are supported yet",
        )),
    }
}

fn lower_if(if_stmt: py::StmtIf) -> Result<Stmt, ShellError> {
    let mut else_branch = Vec::new();
    for clause in if_stmt.elif_else_clauses.into_iter().rev() {
        else_branch = match clause.test {
            Some(test) => vec![Stmt::If {
                condition: lower_expr(test)?,
                then_branch: lower_stmt_block(clause.body)?,
                else_branch,
            }],
            None => {
                if !else_branch.is_empty() {
                    return Err(unsupported_message(
                        "if statement",
                        "else before elif is not supported",
                    ));
                }
                lower_stmt_block(clause.body)?
            }
        }
    }

    Ok(Stmt::If {
        condition: lower_expr(*if_stmt.test)?,
        then_branch: lower_stmt_block(if_stmt.body)?,
        else_branch,
    })
}

fn lower_while(while_stmt: py::StmtWhile) -> Result<Stmt, ShellError> {
    if !while_stmt.orelse.is_empty() {
        return Err(unsupported_message(
            "while statement",
            "while/else is not supported",
        ));
    }
    Ok(Stmt::While {
        condition: lower_expr(*while_stmt.test)?,
        body: lower_stmt_block(while_stmt.body)?,
    })
}

fn lower_with(with_stmt: py::StmtWith) -> Result<Stmt, ShellError> {
    let is_async = with_stmt.is_async;
    let [item] = with_stmt.items.as_slice() else {
        return Err(unsupported_message(
            "with statement",
            "only a single with item is supported yet",
        ));
    };
    let target = match &item.optional_vars {
        Some(var) => {
            let py::Expr::Name(name) = var.as_ref() else {
                return Err(unsupported_message(
                    "with statement",
                    "only simple name with targets are supported yet",
                ));
            };
            Some(name.id.to_string())
        }
        None => None,
    };
    Ok(Stmt::With {
        is_async,
        target,
        context: lower_expr(item.context_expr.clone())?,
        body: lower_stmt_block(with_stmt.body)?,
    })
}

fn lower_stmt_block(statements: Vec<py::Stmt>) -> Result<Vec<Stmt>, ShellError> {
    statements
        .into_iter()
        .map(lower_stmt)
        .collect::<Result<Vec<_>, _>>()
}

fn lower_expr(expression: py::Expr) -> Result<Expr, ShellError> {
    match expression {
        py::Expr::NoneLiteral(_) => Ok(Expr::None),
        py::Expr::BooleanLiteral(boolean) => Ok(Expr::Bool(boolean.value)),
        py::Expr::NumberLiteral(number) => lower_number(number.value),
        py::Expr::StringLiteral(string) => Ok(Expr::String(string.value.to_string())),
        py::Expr::FString(fstring) => lower_fstring(fstring),
        py::Expr::Name(name) => Ok(Expr::Name(name.id.to_string())),
        py::Expr::List(list) => list
            .elts
            .into_iter()
            .map(lower_expr)
            .collect::<Result<Vec<_>, _>>()
            .map(Expr::List),
        py::Expr::Tuple(tuple) => tuple
            .elts
            .into_iter()
            .map(lower_expr)
            .collect::<Result<Vec<_>, _>>()
            .map(Expr::Tuple),
        py::Expr::ListComp(list_comp) => lower_list_comprehension(list_comp),
        py::Expr::Dict(dict) => lower_dict(dict),
        py::Expr::DictComp(dict_comp) => lower_dict_comprehension(dict_comp),
        py::Expr::Call(call) => lower_expr_call(call),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::Add) => Ok(Expr::Add {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::Sub) => Ok(Expr::Sub {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::Mult) => Ok(Expr::Mul {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::Div) => Ok(Expr::Div {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::FloorDiv) => {
            Ok(Expr::FloorDiv {
                left: Box::new(lower_expr(*bin_op.left)?),
                right: Box::new(lower_expr(*bin_op.right)?),
            })
        }
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::Mod) => Ok(Expr::Mod {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::BitAnd) => Ok(Expr::BitAnd {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::BitOr) => Ok(Expr::BitOr {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::BitXor) => Ok(Expr::BitXor {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::LShift) => Ok(Expr::LShift {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::BinOp(bin_op) if matches!(bin_op.op, py::Operator::RShift) => Ok(Expr::RShift {
            left: Box::new(lower_expr(*bin_op.left)?),
            right: Box::new(lower_expr(*bin_op.right)?),
        }),
        py::Expr::Compare(compare) => lower_compare(compare),
        py::Expr::If(if_expr) => Ok(Expr::Conditional {
            then_expr: Box::new(lower_expr(*if_expr.body)?),
            condition: Box::new(lower_expr(*if_expr.test)?),
            else_expr: Box::new(lower_expr(*if_expr.orelse)?),
        }),
        py::Expr::BoolOp(bool_op) => lower_bool_op(bool_op),
        py::Expr::UnaryOp(unary_op) if matches!(unary_op.op, py::UnaryOp::Not) => {
            Ok(Expr::Not(Box::new(lower_expr(*unary_op.operand)?)))
        }
        py::Expr::UnaryOp(unary_op) if matches!(unary_op.op, py::UnaryOp::USub) => {
            Ok(Expr::Neg(Box::new(lower_expr(*unary_op.operand)?)))
        }
        py::Expr::UnaryOp(unary_op) if matches!(unary_op.op, py::UnaryOp::Invert) => {
            Ok(Expr::Invert(Box::new(lower_expr(*unary_op.operand)?)))
        }
        py::Expr::Await(await_expr) => Ok(Expr::Await(Box::new(lower_expr(*await_expr.value)?))),
        py::Expr::Lambda(lambda) => lower_lambda(lambda),
        py::Expr::Generator(generator) => lower_generator(generator),
        py::Expr::Subscript(subscript) => Ok(Expr::Subscript {
            value: Box::new(lower_expr(*subscript.value)?),
            index: Box::new(lower_expr(*subscript.slice)?),
        }),
        py::Expr::Attribute(attribute) => Ok(Expr::Attribute {
            value: Box::new(lower_expr(*attribute.value)?),
            attr: attribute.attr.to_string(),
        }),
        py::Expr::Slice(slice) => lower_slice(slice),
        unsupported => Err(unsupported_error("expression", &unsupported)),
    }
}

fn lower_slice(slice: py::ExprSlice) -> Result<Expr, ShellError> {
    if slice.step.is_some() {
        return Err(unsupported_message(
            "slice",
            "slice steps are not supported yet",
        ));
    }
    Ok(Expr::Slice {
        lower: slice
            .lower
            .map(|value| lower_expr(*value).map(Box::new))
            .transpose()?,
        upper: slice
            .upper
            .map(|value| lower_expr(*value).map(Box::new))
            .transpose()?,
    })
}

fn lower_lambda(lambda: py::ExprLambda) -> Result<Expr, ShellError> {
    let Some(parameters) = lambda.parameters else {
        return Ok(Expr::Lambda {
            params: Vec::new(),
            body: Box::new(lower_expr(*lambda.body)?),
        });
    };
    if parameters.vararg.is_some()
        || parameters.kwarg.is_some()
        || !parameters.kwonlyargs.is_empty()
    {
        return Err(unsupported_message(
            "lambda",
            "lambda supports only positional parameters for now",
        ));
    }
    let mut params = Vec::with_capacity(parameters.posonlyargs.len() + parameters.args.len());
    for parameter in parameters.posonlyargs.iter().chain(parameters.args.iter()) {
        if parameter.default.is_some() || parameter.parameter.annotation.is_some() {
            return Err(unsupported_message(
                "lambda",
                "lambda defaults and annotations are not supported yet",
            ));
        }
        let name = parameter.parameter.name.to_string();
        if params.iter().any(|param| param == &name) {
            return Err(unsupported_message(
                "lambda",
                format!("duplicate lambda parameter `{name}`"),
            ));
        }
        params.push(name);
    }
    Ok(Expr::Lambda {
        params,
        body: Box::new(lower_expr(*lambda.body)?),
    })
}

fn lower_number(number: py::Number) -> Result<Expr, ShellError> {
    match number {
        py::Number::Int(value) => Ok(Expr::Int(value.to_string())),
        py::Number::Float(value) => Ok(Expr::Float(value)),
        py::Number::Complex { .. } => Err(unsupported_message(
            "number literal",
            "complex numbers are not supported yet",
        )),
    }
}

fn lower_compare(compare: py::ExprCompare) -> Result<Expr, ShellError> {
    let ops = compare
        .ops
        .into_vec()
        .into_iter()
        .map(lower_compare_op)
        .collect::<Result<Vec<_>, _>>()?;
    let comparators = compare
        .comparators
        .into_vec()
        .into_iter()
        .map(lower_expr)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Expr::Compare {
        left: Box::new(lower_expr(*compare.left)?),
        ops,
        comparators,
    })
}

fn lower_compare_op(op: py::CmpOp) -> Result<CompareOp, ShellError> {
    match op {
        py::CmpOp::Eq => Ok(CompareOp::Eq),
        py::CmpOp::NotEq => Ok(CompareOp::NotEq),
        py::CmpOp::Lt => Ok(CompareOp::Lt),
        py::CmpOp::LtE => Ok(CompareOp::LtE),
        py::CmpOp::Gt => Ok(CompareOp::Gt),
        py::CmpOp::GtE => Ok(CompareOp::GtE),
        py::CmpOp::In => Ok(CompareOp::In),
        py::CmpOp::NotIn => Ok(CompareOp::NotIn),
        py::CmpOp::Is => Ok(CompareOp::Is),
        py::CmpOp::IsNot => Ok(CompareOp::IsNot),
    }
}

fn lower_bool_op(bool_op: py::ExprBoolOp) -> Result<Expr, ShellError> {
    let op = match bool_op.op {
        py::BoolOp::And => BoolOp::And,
        py::BoolOp::Or => BoolOp::Or,
    };
    let values = bool_op
        .values
        .into_iter()
        .map(lower_expr)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Expr::BoolOp { op, values })
}

fn lower_fstring(fstring: py::ExprFString) -> Result<Expr, ShellError> {
    let mut parts = Vec::new();
    for element in fstring.value.elements() {
        match element {
            py::InterpolatedStringElement::Literal(literal) => {
                parts.push(FormattedStringPart::Literal(literal.value.to_string()));
            }
            py::InterpolatedStringElement::Interpolation(interpolation) => {
                if interpolation.debug_text.is_some() {
                    return Err(unsupported_message(
                        "f-string",
                        "debug expressions are not supported",
                    ));
                }
                let expr = lower_expr(interpolation.expression.as_ref().clone())?;
                if let Some(format_spec) = interpolation.format_spec.as_ref() {
                    parts.push(FormattedStringPart::Formatted {
                        expr,
                        spec: lower_fstring_format_spec(format_spec)?,
                    });
                } else {
                    parts.push(FormattedStringPart::Expr(expr));
                }
            }
        }
    }
    Ok(Expr::FormattedString(parts))
}

fn lower_fstring_format_spec(
    format_spec: &py::InterpolatedStringFormatSpec,
) -> Result<StoneFormatSpec, ShellError> {
    let mut spec = String::new();
    for element in &format_spec.elements {
        match element {
            py::InterpolatedStringElement::Literal(literal) => {
                spec.push_str(&literal.value);
            }
            py::InterpolatedStringElement::Interpolation(_) => {
                return Err(unsupported_message(
                    "f-string",
                    "dynamic format specifiers are not supported",
                ));
            }
        }
    }
    if let Some(precision) = spec
        .strip_prefix('.')
        .and_then(|rest| rest.strip_suffix('f'))
    {
        let precision = precision.parse::<usize>().map_err(|err| {
            unsupported_message(
                "f-string",
                format!("invalid fixed precision `{spec}`: {err}"),
            )
        })?;
        return Ok(StoneFormatSpec::Fixed { precision });
    }
    if spec.starts_with('0') && spec.ends_with('d') && spec.len() > 2 {
        let width = spec[1..spec.len() - 1].parse::<usize>().map_err(|err| {
            unsupported_message("f-string", format!("invalid zero-pad spec `{spec}`: {err}"))
        })?;
        return Ok(StoneFormatSpec::ZeroPadInt { width });
    }
    Err(unsupported_message(
        "f-string",
        format!("unsupported format specifier `{spec}`"),
    ))
}

fn lower_generator(generator: py::ExprGenerator) -> Result<Expr, ShellError> {
    Ok(Expr::Generator {
        elt: Box::new(lower_expr(*generator.elt)?),
        clauses: lower_comprehension_clauses("generator expression", &generator.generators)?,
    })
}

fn lower_list_comprehension(list_comp: py::ExprListComp) -> Result<Expr, ShellError> {
    Ok(Expr::ListComprehension {
        elt: Box::new(lower_expr(*list_comp.elt)?),
        clauses: lower_comprehension_clauses("list comprehension", &list_comp.generators)?,
    })
}

fn lower_dict_comprehension(dict_comp: py::ExprDictComp) -> Result<Expr, ShellError> {
    Ok(Expr::DictComprehension {
        key: Box::new(lower_expr(*dict_comp.key)?),
        value: Box::new(lower_expr(*dict_comp.value)?),
        clauses: lower_comprehension_clauses("dict comprehension", &dict_comp.generators)?,
    })
}

fn lower_comprehension_clauses(
    context: &str,
    comprehensions: &[py::Comprehension],
) -> Result<Vec<ComprehensionClause>, ShellError> {
    if comprehensions.is_empty() {
        return Err(unsupported_message(
            context,
            "at least one for clause is required",
        ));
    }
    comprehensions
        .iter()
        .map(|comprehension| {
            if comprehension.is_async {
                return Err(unsupported_message(
                    context,
                    "async comprehension clauses are not supported",
                ));
            }
            Ok(ComprehensionClause {
                targets: lower_loop_targets(comprehension.target.clone())?,
                iter: lower_expr(comprehension.iter.clone())?,
                filters: comprehension
                    .ifs
                    .iter()
                    .cloned()
                    .map(lower_expr)
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect()
}

fn lower_dict(dict: py::ExprDict) -> Result<Expr, ShellError> {
    let mut items = Vec::with_capacity(dict.items.len());
    for item in dict.items {
        let Some(key) = item.key else {
            return Err(unsupported_message(
                "record literal",
                "dictionary spread is not supported yet",
            ));
        };

        let key = match key {
            py::Expr::StringLiteral(string) => string.value.to_string(),
            py::Expr::NumberLiteral(number) => record_number_key(number.value)?,
            py::Expr::BooleanLiteral(boolean) => boolean.value.to_string(),
            unsupported => {
                return Err(unsupported_message(
                    "record literal",
                    format!("unsupported record key expression: {unsupported:?}"),
                ));
            }
        };

        items.push((key, lower_expr(item.value)?));
    }

    Ok(Expr::Record(items))
}

fn record_number_key(number: py::Number) -> Result<String, ShellError> {
    match number {
        py::Number::Int(value) => Ok(value.to_string()),
        py::Number::Float(value) => Ok(value.to_string()),
        py::Number::Complex { .. } => Err(unsupported_message(
            "record literal",
            "complex number keys are not supported yet",
        )),
    }
}

fn lower_expr_call(call: py::ExprCall) -> Result<Expr, ShellError> {
    let positional = call
        .arguments
        .args
        .into_vec()
        .into_iter()
        .map(lower_expr)
        .collect::<Result<Vec<_>, _>>()?;

    match *call.func {
        py::Expr::Name(name) => {
            let named = call
                .arguments
                .keywords
                .into_vec()
                .into_iter()
                .map(|keyword| {
                    let Some(name) = keyword.arg else {
                        return Err(unsupported_message(
                            "command call",
                            "keyword spread is not supported yet",
                        ));
                    };
                    Ok((name.to_string(), lower_expr(keyword.value)?))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::Call(Call {
                name: name.id.to_string(),
                positional,
                named,
            }))
        }
        py::Expr::Attribute(attribute) => {
            let method = attribute.attr.to_string();
            if !matches!(
                method.as_str(),
                "append"
                    | "add"
                    | "close"
                    | "find"
                    | "get"
                    | "items"
                    | "index"
                    | "join"
                    | "keys"
                    | "isdigit"
                    | "isalpha"
                    | "isalnum"
                    | "lstrip"
                    | "lower"
                    | "read"
                    | "readlines"
                    | "accept"
                    | "branch"
                    | "discard"
                    | "inspect"
                    | "release"
                    | "wait"
                    | "wait_any"
                    | "wait_all"
                    | "replace"
                    | "rsplit"
                    | "rstrip"
                    | "count"
                    | "extend"
                    | "sort"
                    | "strip"
                    | "split"
                    | "splitlines"
                    | "startswith"
                    | "endswith"
                    | "upper"
                    | "values"
                    | "write"
                    | "zfill"
            ) {
                return Err(unsupported_message(
                    "method call",
                    format!("unsupported method `{method}`"),
                ));
            }
            let mut positional = positional;
            let mut named = Vec::new();
            if !call.arguments.keywords.is_empty() {
                if method == "split" {
                    for keyword in call.arguments.keywords.into_vec() {
                        let Some(name) = keyword.arg else {
                            return Err(unsupported_message(
                                "method call",
                                "keyword spread is not supported yet",
                            ));
                        };
                        if name.as_str() != "maxsplit" {
                            return Err(unsupported_message(
                                "method call",
                                format!("unsupported split() keyword argument `{name}`"),
                            ));
                        }
                        match positional.len() {
                            0 => {
                                positional.push(Expr::None);
                                positional.push(lower_expr(keyword.value)?);
                            }
                            1 => positional.push(lower_expr(keyword.value)?),
                            _ => {
                                return Err(unsupported_message(
                                    "method call",
                                    "split() got multiple maxsplit values",
                                ));
                            }
                        }
                    }
                } else if method == "sort" {
                    for keyword in call.arguments.keywords.into_vec() {
                        let Some(name) = keyword.arg else {
                            return Err(unsupported_message(
                                "method call",
                                "keyword spread is not supported yet",
                            ));
                        };
                        match name.as_str() {
                            "key" | "reverse" => {
                                named.push((name.to_string(), lower_expr(keyword.value)?));
                            }
                            _ => {
                                return Err(unsupported_message(
                                    "method call",
                                    format!("unsupported sort() keyword argument `{name}`"),
                                ));
                            }
                        }
                    }
                } else if matches!(
                    method.as_str(),
                    "accept"
                        | "branch"
                        | "discard"
                        | "inspect"
                        | "release"
                        | "wait"
                        | "wait_any"
                        | "wait_all"
                ) {
                    for keyword in call.arguments.keywords.into_vec() {
                        let Some(name) = keyword.arg else {
                            return Err(unsupported_message(
                                "method call",
                                "keyword spread is not supported yet",
                            ));
                        };
                        named.push((name.to_string(), lower_expr(keyword.value)?));
                    }
                } else {
                    for keyword in call.arguments.keywords.into_vec() {
                        let Some(name) = keyword.arg else {
                            return Err(unsupported_message(
                                "method call",
                                "keyword spread is not supported yet",
                            ));
                        };
                        return Err(unsupported_message(
                            "method call",
                            format!(
                                "unsupported {method}() keyword argument `{name}`; keyword arguments are supported on split, sort, and nominal attempt-resource methods"
                            ),
                        ));
                    }
                }
            }
            Ok(Expr::MethodCall {
                receiver: Box::new(lower_expr(*attribute.value)?),
                method,
                positional,
                named,
            })
        }
        unsupported => Err(unsupported_message(
            "command call",
            format!("unsupported call target: {unsupported:?}"),
        )),
    }
}

fn unsupported_error(kind: &str, value: &impl std::fmt::Debug) -> ShellError {
    unsupported_message(kind, format!("unsupported {kind}: {value:?}"))
}

fn unsupported_import_statement(aliases: &[py::Alias]) -> ShellError {
    let modules = aliases
        .iter()
        .map(|alias| alias.name.to_string())
        .collect::<Vec<_>>();
    unsupported_import_modules("import", &modules)
}

fn unsupported_import_from_statement(module: Option<String>, aliases: &[py::Alias]) -> ShellError {
    let module = module.unwrap_or_else(|| "<relative>".to_owned());
    let names = aliases
        .iter()
        .map(|alias| alias.name.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    unsupported_message(
        "import",
        format!(
            "Python import is not supported: from {module} import {names}. {}",
            import_suggestions_for_module(&module)
        ),
    )
}

fn unsupported_import_modules(kind: &str, modules: &[String]) -> ShellError {
    let modules_text = if modules.is_empty() {
        "<unknown>".to_owned()
    } else {
        modules.join(", ")
    };
    let suggestions = modules
        .iter()
        .map(|module| import_suggestions_for_module(module))
        .collect::<Vec<_>>()
        .join(" ");
    unsupported_message(
        "import",
        format!("Python {kind} is not supported: {modules_text}. {suggestions}"),
    )
}

fn import_suggestions_for_module(module: &str) -> &'static str {
    match module.split('.').next().unwrap_or(module) {
        "os" => {
            "Stone replacements for os: use ls(path) or find(path, \"*\") for directory listings; use string concatenation for simple path joins; use pwd()/cd(path) for cwd."
        }
        "pathlib" => {
            "Stone replacements for pathlib: use string paths directly, ls(path)/find(path, \"*\"), read_file(path), write_file(path, text), and path string concatenation."
        }
        "json" => {
            "Stone replacements for json: use json_loads(text), json_dumps(value), read_json(path), and read_jsonl(path)."
        }
        "csv" => {
            "Stone replacements for csv: use read_csv(path) for record rows and write_csv(path, rows) for CSV outputs."
        }
        "glob" => {
            "Stone replacements for glob: use find(root, pattern) for recursive matches or ls(path) for directory entries."
        }
        "base64" => {
            "Stone has no base64 builtin yet; use run([\"base64\", ...]) only when the task explicitly allows POSIX tools."
        }
        "hashlib" => {
            "Stone replacements for hashlib: use md5(text), sha1(text), or sha256(text) for lowercase hexadecimal digests."
        }
        "re" => {
            "Stone has no regex module yet; use search(root, needle) for literal file search or string split/find/startswith/endswith for simple parsing."
        }
        "datetime" | "time" => {
            "Stone has no datetime module yet; parse fixed-format dates with string split/slice and int() when possible."
        }
        "math" => {
            "Stone has typed numeric operators and round(value, ndigits), but no math module."
        }
        _ => "Use help() to inspect Stone builtins and replace module APIs with typed Stone file, JSON, CSV, text, and run helpers.",
    }
}

fn unsupported_message(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("unsupported Stone {kind}"), message.into())
            .with_code("stone_script_unsupported"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        lower_source, AssignTarget, AugOp, BoolOp, Call, CompareOp, Expr, Program, Stmt, StoneType,
    };

    #[test]
    fn lowers_command_call() {
        let program = lower_source(r#"pwd()"#).expect("lower source");
        assert_eq!(
            program,
            Program {
                statements: vec![Stmt::Expr(Expr::Call(Call {
                    name: "pwd".to_string(),
                    positional: vec![],
                    named: vec![],
                }))],
            }
        );
    }

    #[test]
    fn lowers_assignment_and_bitwise_or() {
        let program = lower_source("mask = left | right").expect("lower source");
        assert_eq!(program.statements.len(), 1);
        let Stmt::Assign { target, value } = &program.statements[0] else {
            panic!("expected assignment");
        };
        assert_eq!(target, &AssignTarget::Name("mask".to_string()));
        assert!(matches!(value, Expr::BitOr { .. }));
    }

    #[test]
    fn lowers_integer_operator_expressions() {
        for source in [
            "value = a * b",
            "value = a // b",
            "value = a & b",
            "value = a | b",
            "value = a ^ b",
            "value = a << b",
            "value = a >> b",
        ] {
            lower_source(source).expect(source);
        }
    }

    #[test]
    fn lowers_unary_bitwise_invert() {
        let program = lower_source("value = ~mask").expect("lower source");
        let Stmt::Assign { value, .. } = &program.statements[0] else {
            panic!("expected assignment");
        };
        assert!(matches!(value, Expr::Invert(_)));
    }

    #[test]
    fn accepts_v0_literals_and_collections() {
        let cases = [
            ("None", Expr::None),
            ("True", Expr::Bool(true)),
            ("False", Expr::Bool(false)),
            ("42", Expr::Int("42".to_string())),
            ("1.5", Expr::Float(1.5)),
            (r#""hello""#, Expr::String("hello".to_string())),
            (
                r#"[1, "two", False]"#,
                Expr::List(vec![
                    Expr::Int("1".to_string()),
                    Expr::String("two".to_string()),
                    Expr::Bool(false),
                ]),
            ),
            (
                r#"(1, "two", False)"#,
                Expr::Tuple(vec![
                    Expr::Int("1".to_string()),
                    Expr::String("two".to_string()),
                    Expr::Bool(false),
                ]),
            ),
            (
                r#"{"name": "demo", "kind": "file"}"#,
                Expr::Record(vec![
                    ("name".to_string(), Expr::String("demo".to_string())),
                    ("kind".to_string(), Expr::String("file".to_string())),
                ]),
            ),
            (
                r#"{1: "one", 2.5: "two", True: "yes"}"#,
                Expr::Record(vec![
                    ("1".to_string(), Expr::String("one".to_string())),
                    ("2.5".to_string(), Expr::String("two".to_string())),
                    ("true".to_string(), Expr::String("yes".to_string())),
                ]),
            ),
        ];

        for (source, expected) in cases {
            let program = lower_source(source).expect(source);
            assert_eq!(
                program,
                Program {
                    statements: vec![Stmt::Expr(expected)]
                },
                "source: {source}"
            );
        }
    }

    #[test]
    fn accepts_agent_expression_features() {
        let cases = [
            (
                r#"item["name"]"#,
                Expr::Subscript {
                    value: Box::new(Expr::Name("item".to_string())),
                    index: Box::new(Expr::String("name".to_string())),
                },
            ),
            (
                "count >= 2",
                Expr::Compare {
                    left: Box::new(Expr::Name("count".to_string())),
                    ops: vec![CompareOp::GtE],
                    comparators: vec![Expr::Int("2".to_string())],
                },
            ),
            (
                r#""needle" in text"#,
                Expr::Compare {
                    left: Box::new(Expr::String("needle".to_string())),
                    ops: vec![CompareOp::In],
                    comparators: vec![Expr::Name("text".to_string())],
                },
            ),
            (
                "ready and not blocked",
                Expr::BoolOp {
                    op: BoolOp::And,
                    values: vec![
                        Expr::Name("ready".to_string()),
                        Expr::Not(Box::new(Expr::Name("blocked".to_string()))),
                    ],
                },
            ),
        ];

        for (source, expected) in cases {
            let program = lower_source(source).expect(source);
            assert_eq!(
                program,
                Program {
                    statements: vec![Stmt::Expr(expected)]
                },
                "source: {source}"
            );
        }
    }

    #[test]
    fn accepts_slices_and_python_helper_calls() {
        let program = lower_source(
            r#"items[1:]
"a\nb".splitlines()
",".join(["a", "b"])
range(3)
enumerate(items)
read_json("/app/data.json")
write_json("/work/out.json", {"ok": True})
write_jsonl("/work/out.jsonl", [{"ok": True}])
json_loads("{\"ok\": true}")
json_dumps({"ok": True})
"#,
        )
        .expect("lower helper calls");

        assert_eq!(program.statements.len(), 10);
        assert!(matches!(
            program.statements[0],
            Stmt::Expr(Expr::Subscript { ref index, .. })
                if matches!(index.as_ref(), Expr::Slice { .. })
        ));
    }

    #[test]
    fn accepts_simple_list_comprehensions() {
        let program = lower_source(
            r#"numbers = [int(text) for text in items if text.strip()]
trimmed = [item.strip() for item in items]
"#,
        )
        .expect("lower list comprehensions");

        assert_eq!(program.statements.len(), 2);
        assert!(matches!(
            program.statements[0],
            Stmt::Assign {
                value:
                    Expr::ListComprehension {
                        ref clauses,
                        ..
                    },
                ..
            } if clauses.len() == 1
                && clauses[0].targets == vec!["text".to_string()]
                && clauses[0].filters.len() == 1
        ));
    }

    #[test]
    fn accepts_llm_compatibility_shapes() {
        let program = lower_source(
            r#"value = row["size"] if "size" in row else 0
total = 10
total -= 1
total *= 2
total /= 3
total //= 2
total %= 2
mask = 1
mask |= 2
mask &= 3
mask ^= 1
mask <<= 2
mask >>= 1
def head(path, limit=10):
    return limit
try:
    text = read_text(path)
except Exception as e:
    text = e.message
pairs = [name + ":" + str(count) for name, count in counts.items()]
flat = [item for row in rows for item in row["items"]]
total = sum(int(x) for x in values if x)
"#,
        )
        .expect("lower llm compatibility features");

        assert!(matches!(
            program.statements[0],
            Stmt::Assign {
                value: Expr::Conditional { .. },
                ..
            }
        ));
        let aug_ops = program
            .statements
            .iter()
            .filter_map(|statement| match statement {
                Stmt::AugAssign { op, .. } => Some(*op),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(aug_ops.len(), 10);
        for (actual, op) in aug_ops.into_iter().zip(
            [
                AugOp::Sub,
                AugOp::Mul,
                AugOp::Div,
                AugOp::FloorDiv,
                AugOp::Mod,
                AugOp::BitOr,
                AugOp::BitAnd,
                AugOp::BitXor,
                AugOp::LShift,
                AugOp::RShift,
            ]
            .into_iter(),
        ) {
            assert_eq!(actual, op);
        }
        let Stmt::FunctionDef(function) = &program.statements[13] else {
            panic!("expected function definition");
        };
        assert!(matches!(function.params[1].default, Some(Expr::Int(_))));
        assert!(matches!(
            program.statements[14],
            Stmt::Try { ref handlers, .. } if handlers[0].name.as_deref() == Some("e")
        ));
        assert!(matches!(
            program.statements[15],
            Stmt::Assign {
                value: Expr::ListComprehension { ref clauses, .. },
                ..
            } if clauses[0].targets == vec!["name".to_string(), "count".to_string()]
        ));
        assert!(matches!(
            program.statements[16],
            Stmt::Assign {
                value: Expr::ListComprehension { ref clauses, .. },
                ..
            } if clauses.len() == 2
        ));
        assert!(matches!(
            program.statements[17],
            Stmt::Assign {
                value: Expr::Call(ref call),
                ..
            } if matches!(call.positional[0], Expr::Generator { ref clauses, .. } if clauses[0].filters.len() == 1)
        ));
    }

    #[test]
    fn accepts_for_augassign_and_method_calls() {
        let program = lower_source(
            r#"total = 0
rows = []
for line in open("/app/numbers.txt"):
    total += int(line.strip())
    rows.append(line.split(","))
emit({"total": total, "rows": len(rows)})
"#,
        )
        .expect("lower source");

        assert_eq!(program.statements.len(), 4);
        assert!(matches!(
            program.statements[2],
            Stmt::For {
                ref targets,
                ref body,
                ..
            } if targets == &vec!["line".to_string()]
                && matches!(body[0], Stmt::AugAssign { op: AugOp::Add, .. })
                && matches!(body[1], Stmt::Expr(Expr::MethodCall { .. }))
        ));
    }

    #[test]
    fn accepts_local_item_assignment_and_index_method() {
        let program = lower_source(
            r#"row = {}
row["region"] = "west"
items = ["a", "b"]
items[1] = "bee"
idx = items.index("bee")
pos = "alpha beta".find("beta")
for i, item in enumerate(items):
    idx = i + 1
"#,
        )
        .expect("lower source");

        assert_eq!(program.statements.len(), 7);
        assert!(matches!(
            program.statements[1],
            Stmt::Assign {
                target: AssignTarget::Subscript {
                    ref value,
                    ..
                },
                ..
            } if matches!(value.as_ref(), AssignTarget::Name(name) if name == "row")
        ));
        assert!(matches!(
            program.statements[4],
            Stmt::Assign {
                value: Expr::MethodCall { ref method, .. },
                ..
            } if method == "index"
        ));
        assert!(matches!(
            program.statements[5],
            Stmt::Assign {
                value: Expr::MethodCall { ref method, .. },
                ..
            } if method == "find"
        ));
        assert!(matches!(
            program.statements[6],
            Stmt::For {
                ref targets,
                ref body,
                ..
            } if targets == &vec!["i".to_string(), "item".to_string()]
                && matches!(
                    body[0],
                    Stmt::Assign {
                        value: Expr::Add { .. },
                        ..
                    }
                )
        ));
    }

    #[test]
    fn attribute_assignment_suggests_item_assignment() {
        let error = lower_source(
            r#"record = {"status": "rejected"}
record.status = "accepted"
"#,
        )
        .expect_err("attribute assignment should explain the Stone form");
        let text = format!("{error:?}");
        assert!(
            text.contains("record.status = value")
                && text.contains(r#"record[\"status\"] = value"#),
            "{text}"
        );
    }

    #[test]
    fn accepts_common_python_shaped_ergonomics() {
        let program = lower_source(
            r#"a, b = line.split(",")
message = f"{a}:{b}"
lookup = {row["name"]: row["score"] for row in rows if row["score"]}
while a != b:
    break
values = sorted([3, 1, 2])
"#,
        )
        .expect("lower common ergonomics");

        assert_eq!(program.statements.len(), 5);
        assert!(matches!(
            program.statements[0],
            Stmt::Assign {
                target: AssignTarget::Tuple(ref targets),
                ..
            } if targets == &vec!["a".to_string(), "b".to_string()]
        ));
        assert!(matches!(
            program.statements[1],
            Stmt::Assign {
                value: Expr::FormattedString(_),
                ..
            }
        ));
        assert!(matches!(
            program.statements[2],
            Stmt::Assign {
                value: Expr::DictComprehension { .. },
                ..
            }
        ));
        assert!(matches!(program.statements[3], Stmt::While { .. }));
    }

    #[test]
    fn lowers_stage_decorator_to_typed_function_metadata() {
        let program = lower_source(
            r#"@stage(evidence=file_nonempty("artifact.txt"), goal="build the artifact", inputs=["inspect"], repair=repair_artifact, max_attempts=2, max_actions=6, checkpoint="workspace")
def artifact(step):
    return run(["make", "artifact"])
"#,
        )
        .expect("lower stage declaration");
        let Stmt::FunctionDef(function) = &program.statements[0] else {
            panic!("expected function definition");
        };
        let stage = function.stage.as_ref().expect("stage metadata");
        assert!(matches!(
            stage.evidence,
            Expr::Call(ref call) if call.name == "file_nonempty"
        ));
        assert!(
            matches!(stage.goal, Some(Expr::String(ref value)) if value == "build the artifact")
        );
        assert!(matches!(
            stage.inputs,
            Some(Expr::List(ref values))
                if matches!(values.as_slice(), [Expr::String(value)] if value == "inspect")
        ));
        assert!(matches!(stage.repair, Some(Expr::Name(ref name)) if name == "repair_artifact"));
        assert!(matches!(stage.max_attempts, Some(Expr::Int(ref value)) if value == "2"));
        assert!(matches!(stage.max_actions, Some(Expr::Int(ref value)) if value == "6"));
        assert!(matches!(stage.checkpoint, Some(Expr::String(ref value)) if value == "workspace"));
    }

    #[test]
    fn lowers_workflow_blocks_and_ensure_contracts_to_typed_kernel() {
        let program = lower_source(
            r#"workflow task:
    stage build(
        goal="produce the artifact",
        max_actions=8,
        checkpoint="repairable",
    ):
        run(["sh", "-c", "printf ready > artifact.txt"])
        ensure file_nonempty("artifact.txt")

    stage verify(goal="verify output", inputs=["build"], max_actions=2):
        ensure all_evidence(
            file_nonempty("artifact.txt"),
            file_nonempty("artifact.txt"),
        )

run task
"#,
        )
        .expect("lower workflow block");

        assert_eq!(program.statements.len(), 4);
        let Stmt::FunctionDef(build) = &program.statements[0] else {
            panic!("expected build stage function");
        };
        let build_stage = build.stage.as_ref().expect("build stage metadata");
        assert_eq!(build.name, "__stone_workflow_0_stage_0");
        assert!(matches!(
            build_stage.name,
            Some(Expr::String(ref value)) if value == "build"
        ));
        assert!(matches!(
            build_stage.evidence,
            Expr::Call(ref call) if call.name == "all_evidence" && call.positional.len() == 1
        ));
        assert!(matches!(
            build_stage.max_actions,
            Some(Expr::Int(ref value)) if value == "8"
        ));
        assert!(matches!(build.body.last(), Some(Stmt::Return(Some(_)))));

        let Stmt::FunctionDef(verify) = &program.statements[1] else {
            panic!("expected verify stage function");
        };
        assert!(matches!(
            verify.stage.as_ref().and_then(|stage| stage.inputs.as_ref()),
            Some(Expr::List(values))
                if matches!(values.as_slice(), [Expr::String(value)] if value == "build")
        ));

        let Stmt::Assign { value, .. } = &program.statements[2] else {
            panic!("expected workflow construction");
        };
        assert!(
            matches!(value, Expr::Call(call) if call.name == "workflow" && call.positional.len() == 3)
        );
        assert!(matches!(
            program.statements[3],
            Stmt::Expr(Expr::Call(ref emit))
                if emit.name == "emit"
                    && matches!(emit.positional[0], Expr::Call(ref run) if run.name == "workflow_main")
        ));
    }

    #[test]
    fn workflow_block_agent_loop_lowers_to_stage_context_call() {
        let program = lower_source(
            r#"workflow task:
    stage build(goal="build", max_actions=4):
        agent_loop()
        ensure file_nonempty("artifact.txt")

run task
"#,
        )
        .expect("lower workflow agent loop");
        let Stmt::FunctionDef(function) = &program.statements[0] else {
            panic!("expected stage function");
        };
        assert!(matches!(
            function.body.as_slice(),
            [Stmt::Return(Some(Expr::Call(call)))]
                if call.name == "agent_loop"
                    && matches!(call.positional.as_slice(), [Expr::Name(name)] if name == "__stone_step")
        ));
    }

    #[test]
    fn workflow_stage_names_do_not_shadow_builtin_calls() {
        let program = lower_source(
            r#"workflow task:
    stage run(goal="execute command", max_actions=1):
        run(["true"])
        ensure command_succeeded(["true"])

run task
"#,
        )
        .expect("lower stage named after builtin");
        let Stmt::FunctionDef(function) = &program.statements[0] else {
            panic!("expected stage function");
        };
        assert_eq!(function.name, "__stone_workflow_0_stage_0");
        assert!(function.body.iter().any(|statement| matches!(
            statement,
            Stmt::Expr(Expr::Call(call)) if call.name == "run"
        )));
    }

    #[test]
    fn workflow_block_rejects_missing_or_nested_contracts() {
        let missing = lower_source(
            r#"workflow task:
    stage build(goal="build"):
        pass
"#,
        )
        .expect_err("missing ensure");
        assert!(format!("{missing:?}").contains("requires at least one direct `ensure"));

        let nested = lower_source(
            r#"workflow task:
    stage build(goal="build"):
        if True:
            ensure file_nonempty("artifact.txt")
"#,
        )
        .expect_err("nested ensure");
        assert!(format!("{nested:?}").contains("direct stage indentation"));
    }

    #[test]
    fn standard_stage_agent_library_is_valid_visible_stone() {
        let source = include_str!("../../../examples/scripts/standard_stage_agent.stone");
        let program = lower_source(source).expect("lower standard stage agent library");
        let names = program
            .statements
            .iter()
            .filter_map(|statement| match statement {
                Stmt::FunctionDef(function) => Some(function.name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(names.contains(&"stage_agent_messages"));
        assert!(names.contains(&"stage_agent_dispatch"));
        assert!(names.contains(&"agent_loop"));
        assert_eq!(
            source.matches("model_infer(").count(),
            1,
            "one stage callback must make one visible model decision"
        );
        assert!(!source.contains("react_control("));
    }

    #[test]
    fn blind_authored_staged_doom_reference_is_admitted() {
        let library = include_str!("../../../examples/scripts/standard_stage_agent.stone");
        let harness = include_str!("../../../examples/references/staged_doom_harness.stone");
        let program = lower_source(&format!("{library}\n{harness}"))
            .expect("lower staged Doom reference with visible control library");
        assert!(program.statements.iter().any(|statement| matches!(
            statement,
            Stmt::Expr(Expr::Call(call))
                if call.name == "emit"
                    && matches!(call.positional.as_slice(), [Expr::Call(run)] if run.name == "workflow_main")
        )));
    }

    #[test]
    fn stage_scoped_doom_repair_is_admitted_with_larger_budget() {
        let library = include_str!("../../../examples/scripts/standard_stage_agent.stone");
        let harness =
            include_str!("../../../examples/references/staged_doom_harness_repair_v1.stone");
        let program = lower_source(&format!("{library}\n{harness}"))
            .expect("lower staged Doom repair with visible control library");
        assert!(program.statements.iter().any(|statement| matches!(
            statement,
            Stmt::FunctionDef(function)
                if function.stage.as_ref().is_some_and(|stage| {
                    matches!(stage.name, Some(Expr::String(ref name)) if name == "build")
                        && matches!(stage.max_actions, Some(Expr::Int(ref value)) if value == "12")
                })
        )));
    }

    #[test]
    fn rejects_unknown_or_underspecified_decorators() {
        let unknown = lower_source(
            r#"@cache()
def artifact(step):
    return step
"#,
        )
        .expect_err("unknown decorator");
        assert!(format!("{unknown:?}").contains("only @stage"));

        let missing_evidence = lower_source(
            r#"@stage(max_attempts=2)
def artifact(step):
    return step
"#,
        )
        .expect_err("stage without evidence");
        assert!(format!("{missing_evidence:?}").contains("requires evidence="));
    }

    #[test]
    fn accepts_nested_item_assignment() {
        let program = lower_source(
            r#"stats = {"alice": {"count": 1}}
stats["alice"]["count"] += 1
"#,
        )
        .expect("lower source");

        assert!(matches!(
            program.statements[1],
            Stmt::AugAssign {
                target: AssignTarget::Subscript {
                    ref value,
                    ..
                },
                ..
            } if matches!(value.as_ref(), AssignTarget::Subscript { .. })
        ));
    }

    #[test]
    fn accepts_record_helper_methods() {
        let program = lower_source(
            r#"record = {"a": 1}
value = record.get("a", 0)
keys = record.keys()
values = record.values()
for key, value in record.items():
    emit(key)
"#,
        )
        .expect("lower source");

        assert_eq!(program.statements.len(), 5);
        assert!(matches!(
            program.statements[4],
            Stmt::For {
                ref targets,
                ..
            } if targets == &vec!["key".to_string(), "value".to_string()]
        ));
    }

    #[test]
    fn accepts_pass_statement() {
        let program = lower_source(
            r#"if True:
    pass
else:
    emit("unused")
"#,
        )
        .expect("lower source");

        assert!(matches!(
            program.statements[0],
            Stmt::If {
                ref then_branch,
                ..
            } if matches!(then_branch.as_slice(), [Stmt::Pass])
        ));
    }

    #[test]
    fn accepts_typed_function_definition() {
        let program = lower_source(
            r#"def normalize(text: str) -> str:
    parts = text.split("/")
    return parts[2] + "-" + parts[0] + "-" + parts[1]

value = normalize("01/02/2024")
"#,
        )
        .expect("lower source");

        let Stmt::FunctionDef(function) = &program.statements[0] else {
            panic!("expected function definition");
        };
        assert_eq!(function.name, "normalize");
        assert_eq!(function.params.len(), 1);
        assert_eq!(function.params[0].name, "text");
        assert_eq!(function.params[0].ty, StoneType::Str);
        assert_eq!(function.params[0].default, None);
        assert_eq!(function.return_type, StoneType::Str);
        assert!(matches!(function.body.last(), Some(Stmt::Return(Some(_)))));
    }

    #[test]
    fn accepts_nominal_attempt_control_annotations() {
        let program = lower_source(
            r#"def select(
    frontier: semantic_frontier,
    scope: attempt_scope,
    child: attempt_handle,
    outcome: attempt_outcome,
    accepted: attempt_acceptance,
) -> attempt_handle:
    return accepted.selected
"#,
        )
        .expect("lower nominal attempt types");

        let Stmt::FunctionDef(function) = &program.statements[0] else {
            panic!("expected function definition");
        };
        assert_eq!(
            function
                .params
                .iter()
                .map(|param| param.ty)
                .collect::<Vec<_>>(),
            vec![
                StoneType::SemanticFrontier,
                StoneType::AttemptScope,
                StoneType::AttemptHandle,
                StoneType::AttemptOutcome,
                StoneType::AttemptAcceptance,
            ]
        );
        assert_eq!(function.return_type, StoneType::AttemptHandle);
    }

    #[test]
    fn accepts_no_arg_untyped_function_with_any_return() {
        let program = lower_source(
            r#"def solve():
    pass

solve()
"#,
        )
        .expect("lower source");

        let Stmt::FunctionDef(function) = &program.statements[0] else {
            panic!("expected function definition");
        };
        assert_eq!(function.name, "solve");
        assert!(function.params.is_empty());
        assert_eq!(function.return_type, StoneType::Any);
    }

    #[test]
    fn accepts_untyped_function_with_parameters_as_any() {
        let program = lower_source(
            r#"def normalize(text):
    return text
"#,
        )
        .expect("lower source");
        let Stmt::FunctionDef(function) = &program.statements[0] else {
            panic!("expected function definition");
        };
        assert_eq!(function.params[0].ty, StoneType::Any);
        assert_eq!(function.return_type, StoneType::Any);
    }

    #[test]
    fn accepts_minimal_if_else() {
        let program = lower_source(
            r#"if item["ready"]:
    emit({"status": "ready"})
else:
    emit({"status": "blocked"})
"#,
        )
        .expect("lower if");

        assert_eq!(program.statements.len(), 1);
        let Stmt::If {
            condition,
            then_branch,
            else_branch,
        } = &program.statements[0]
        else {
            panic!("expected if statement");
        };
        assert!(matches!(condition, Expr::Subscript { .. }));
        assert_eq!(then_branch.len(), 1);
        assert_eq!(else_branch.len(), 1);
    }

    #[test]
    fn accepts_elif_break_continue_and_with() {
        let program = lower_source(
            r#"with open("/app/input.txt") as f:
    for line in f:
        if line == "stop":
            break
        elif line == "skip":
            continue
        else:
            emit(line)
"#,
        )
        .expect("lower control flow");

        let [Stmt::With {
            target,
            context,
            body,
            ..
        }] = program.statements.as_slice()
        else {
            panic!("expected with statement");
        };
        assert_eq!(target.as_deref(), Some("f"));
        assert!(matches!(context, Expr::Call(call) if call.name == "open"));
        let Stmt::For { body: for_body, .. } = &body[0] else {
            panic!("expected for in with body");
        };
        let Stmt::If { else_branch, .. } = &for_body[0] else {
            panic!("expected if in for body");
        };
        assert!(matches!(else_branch[0], Stmt::If { .. }));
    }

    #[test]
    fn accepts_nominal_attempt_resource_methods_with_keywords() {
        let program = lower_source(
            r#"with semantic_frontier(checkpoint) as frontier:
    with attempt_scope() as scope:
        child = scope.branch(frontier, entrypoint="worker", start=True)
        outcome = child.wait(timeout_ms=30000)
        batch = scope.wait_all(timeout_ms=30000)
        detail = child.inspect(include_details=True)
        accepted = root.accept(child)
        child.discard(reason="not selected")
        frontier.release()
"#,
        )
        .expect("lower nominal resource methods");

        let [Stmt::With { body, .. }] = program.statements.as_slice() else {
            panic!("expected frontier with statement");
        };
        let [Stmt::With { body, .. }] = body.as_slice() else {
            panic!("expected attempt-scope with statement");
        };
        assert_eq!(body.len(), 7);
        assert!(body.iter().all(|statement| matches!(
            statement,
            Stmt::Assign {
                value: Expr::MethodCall { .. },
                ..
            } | Stmt::Expr(Expr::MethodCall { .. })
        )));
    }

    #[test]
    fn accepts_v0_call_arguments() {
        let program =
            lower_source(r#"save(["a"], "/work/out.json", force=True)"#).expect("lower source");

        assert_eq!(
            program,
            Program {
                statements: vec![Stmt::Expr(Expr::Call(Call {
                    name: "save".to_string(),
                    positional: vec![
                        Expr::List(vec![Expr::String("a".to_string())]),
                        Expr::String("/work/out.json".to_string()),
                    ],
                    named: vec![("force".to_string(), Expr::Bool(true))],
                }))]
            }
        );
    }

    #[test]
    fn accepts_sum_int_generator_expression() {
        let program = lower_source(r#"sum(int(line) for line in open("/app/numbers.txt"))"#)
            .expect("lower source");

        let Stmt::Expr(Expr::Call(call)) = &program.statements[0] else {
            panic!("expected call");
        };
        assert_eq!(call.name, "sum");
        assert!(matches!(call.positional[0], Expr::Generator { .. }));
    }

    #[test]
    fn rejects_unsupported_python_statements() {
        for source in [
            "import os",
            "from json import loads",
            "class C:\n    pass",
            "try:\n    x = 1\nfinally:\n    x = 2",
            "try:\n    x = 1\nexcept ValueError:\n    x = 2",
            "def f(x=[]):\n    return x",
        ] {
            assert_unsupported(source);
        }
    }

    #[test]
    fn accepts_narrow_async_attempt_control_syntax() {
        let program = lower_source(
            r#"async def main() -> attempt_outcome:
    async with attempt_scope() as scope:
        child = scope.branch(frontier, entrypoint="worker")
        outcome = await child.wait(timeout_ms=30000)
    return outcome
"#,
        )
        .expect("lower async attempt control");

        let [Stmt::FunctionDef(function)] = program.statements.as_slice() else {
            panic!("expected async function");
        };
        assert!(function.is_async);
        let [Stmt::With { is_async, body, .. }, Stmt::Return(_)] = function.body.as_slice() else {
            panic!("expected async with followed by return");
        };
        assert!(*is_async);
        assert!(matches!(
            body.get(1),
            Some(Stmt::Assign {
                value: Expr::Await(_),
                ..
            })
        ));
    }

    #[test]
    fn import_errors_include_module_specific_suggestions() {
        let os_error = lower_source("import os").expect_err("import os");
        let os_debug = format!("{os_error:?}");
        assert!(os_debug.contains("Python import is not supported"));
        assert!(os_debug.contains("ls(path)"));
        assert!(os_debug.contains("find(path"));

        let json_error =
            lower_source("from json import loads").expect_err("from json import loads");
        let json_debug = format!("{json_error:?}");
        assert!(json_debug.contains("from json import loads"));
        assert!(json_debug.contains("json_loads(text)"));
        assert!(json_debug.contains("read_json(path)"));

        let hashlib_error = lower_source("import hashlib").expect_err("import hashlib");
        let hashlib_debug = format!("{hashlib_error:?}");
        assert!(hashlib_debug.contains("Python import is not supported"));
        assert!(hashlib_debug.contains("md5(text)"));
        assert!(hashlib_debug.contains("sha1(text)"));
        assert!(hashlib_debug.contains("sha256(text)"));
    }

    #[test]
    fn parse_errors_suggest_hash_comments_for_slash_comments() {
        let err = lower_source("// inspect input\nrows = []").expect_err("slash comment");
        let debug = format!("{err:?}");
        assert!(debug.contains("stone_parse_error"));
        assert!(debug.contains("Stone comments use #"));
        assert!(debug.contains("// operator is floor division"));
    }

    #[test]
    fn rejects_unsupported_python_expressions() {
        for source in ["b'abc'"] {
            assert_unsupported(source);
        }
    }

    #[test]
    fn rejects_unsupported_stone_shapes() {
        for source in [
            "obj.method()",
            "module.command()",
            "{name: \"demo\"}",
            "f(*args)",
            "f(**kwargs)",
            "a = b = 1",
            "obj.items[0] = 1",
            "for a, (b, c) in items:\n    emit(a)",
            "for item in items:\n    emit(item)\nelse:\n    emit(None)",
        ] {
            assert_unsupported(source);
        }
    }

    fn assert_unsupported(source: &str) {
        let err = lower_source(source).expect_err(source);
        let debug = format!("{err:?}");
        assert!(
            debug.contains("stone_script_unsupported"),
            "source: {source}\nunexpected error: {debug}"
        );
    }
}
