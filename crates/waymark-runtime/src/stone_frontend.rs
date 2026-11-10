// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{
    engine::{EngineState, Stack},
    PipelineData, ShellError,
};

use crate::stone_admission::validate_program;
use crate::stone_ast::{lower_source, Program};
use crate::stone_eval::{eval_program_with_output_and_session, EvalProgramOutput, StoneSession};
use crate::stone_modules::link_program;

/// The command's working directory, where module lookup starts.
fn command_cwd(engine_state: &EngineState, stack: &Stack) -> std::path::PathBuf {
    engine_state
        .cwd_as_string(Some(stack))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| ".".into()))
}

pub fn parse_stone_source(source: &str) -> Result<(), ShellError> {
    lower_stone_source(source).map(|_| ())
}

pub fn lower_stone_source(source: &str) -> Result<Program, ShellError> {
    lower_source(source)
}

pub(crate) fn eval_stone_source_with_output_and_session(
    engine_state: &EngineState,
    stack: &mut Stack,
    source: &str,
    input: PipelineData,
    session: &mut StoneSession,
) -> Result<EvalProgramOutput, ShellError> {
    eval_stone_source_with_output_session_and_entrypoint(
        engine_state,
        stack,
        source,
        input,
        session,
        None,
    )
}

pub(crate) fn eval_stone_source_with_output_session_and_entrypoint(
    engine_state: &EngineState,
    stack: &mut Stack,
    source: &str,
    input: PipelineData,
    session: &mut StoneSession,
    entrypoint: Option<&str>,
) -> Result<EvalProgramOutput, ShellError> {
    let mut program = lower_stone_source(source)?;
    link_program(&mut program, &command_cwd(engine_state, stack))?;
    validate_program(&program, &session.admission_bound_names())?;
    eval_program_with_output_and_session(
        engine_state,
        stack,
        &program,
        input,
        Some(session),
        Some(source),
        entrypoint,
    )
}

#[cfg(test)]
mod tests {
    use super::parse_stone_source;

    #[test]
    fn lowers_simple_stone_script() {
        let source = r#"
items = []
for item in ls("/work"):
    if item["kind"] == "dir":
        items.append(item["name"])
"#;

        parse_stone_source(source).expect("source should be valid Stone");
    }

    #[test]
    fn rejects_invalid_python_syntax() {
        let err = parse_stone_source("if true print('oops')").expect_err("source should fail");
        let debug = format!("{err:?}");
        assert!(
            debug.contains("stone_parse_error"),
            "unexpected error: {debug}"
        );
    }
}
