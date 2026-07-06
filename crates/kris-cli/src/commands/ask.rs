use crate::{command::Command, context::Context};

pub struct AskCommand;

impl Command for AskCommand {
    fn name(&self) -> &'static str {
        "ask"
    }

    fn description(&self) -> &'static str {
        "Ask the local coding assistant a question (needs a running llama-server)"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: ask <question>");
            return;
        }

        let prompt = args.join(" ");
        let min_iterations = context.settings.max_tool_iterations;

        crate::agent_runner::run(context, &prompt, min_iterations);
    }
}
