use std::sync::Arc;

use nu_engine::eval_block;
use nu_parser::parse;
use nu_protocol::{
    ast::Block,
    debugger::WithoutDebug,
    engine::{EngineState, Stack, StateDelta, StateWorkingSet},
    shell_error::generic::GenericError,
    PipelineData, ShellError,
};

use crate::stone_frontend;

pub trait Frontend {
    fn eval(
        &self,
        engine_state: &mut EngineState,
        stack: &mut Stack,
        source: &str,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError>;
}

#[derive(Debug, Default)]
pub struct NuFrontend;

impl Frontend for NuFrontend {
    fn eval(
        &self,
        engine_state: &mut EngineState,
        stack: &mut Stack,
        source: &str,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let (block, delta) = parse_nu_block(engine_state, source)?;
        engine_state.merge_delta(delta)?;
        let pipeline = eval_block::<WithoutDebug>(engine_state, stack, &block, input)?;

        Ok(pipeline.body)
    }
}

#[derive(Debug, Default)]
pub struct StoneFrontend;

impl Frontend for StoneFrontend {
    fn eval(
        &self,
        engine_state: &mut EngineState,
        stack: &mut Stack,
        source: &str,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        stone_frontend::eval_stone_source(engine_state, stack, source, input)
    }
}

fn parse_nu_block(
    engine_state: &EngineState,
    source: &str,
) -> Result<(Arc<Block>, StateDelta), ShellError> {
    let mut working_set = StateWorkingSet::new(engine_state);
    let block = parse(&mut working_set, None, source.as_bytes(), false);

    if let Some(err) = working_set.parse_errors.first() {
        return Err(ShellError::Generic(
            GenericError::new_internal("parse error", err.to_string()).with_code("parse_error"),
        ));
    }

    if let Some(err) = working_set.compile_errors.first() {
        return Err(ShellError::Generic(
            GenericError::new_internal("compile error", err.to_string()).with_code("compile_error"),
        ));
    }

    Ok((block, working_set.render()))
}

#[cfg(test)]
mod tests {
    use super::{Frontend, StoneFrontend};
    use nu_protocol::{
        engine::{EngineState, Stack},
        PipelineData,
    };

    #[test]
    fn stone_frontend_executes_simple_stone_script() {
        let frontend = StoneFrontend;
        let mut engine_state = EngineState::new();
        crate::register_engine_commands(&mut engine_state).expect("register commands");
        let mut stack = Stack::new();

        let output = frontend
            .eval(
                &mut engine_state,
                &mut stack,
                r#"echo("stone")"#,
                PipelineData::empty(),
            )
            .expect("Stone frontend should execute simple Stone");

        let value = output
            .into_value(nu_protocol::Span::unknown())
            .expect("value");
        assert_eq!(value.as_str().expect("string"), "stone");
    }
}
