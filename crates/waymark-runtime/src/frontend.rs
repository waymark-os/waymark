// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{
    engine::{EngineState, Stack},
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
