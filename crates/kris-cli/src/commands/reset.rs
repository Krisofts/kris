use crate::{command::Command, context::Context};

pub struct ResetCommand;

impl Command for ResetCommand {
    fn name(&self) -> &'static str {
        "reset"
    }

    fn description(&self) -> &'static str {
        "Reset the conversation with the assistant"
    }

    fn execute(&self, context: &mut Context, _args: &[&str]) {
        context.history.clear();
        println!("Conversation reset.");
    }
}
