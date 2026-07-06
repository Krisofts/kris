use crate::{command::Command, context::Context};

pub struct ClearCommand;

impl Command for ClearCommand {
    fn name(&self) -> &'static str {
        "clear"
    }

    fn description(&self) -> &'static str {
        "Clear terminal"
    }

    fn execute(&self, _context: &mut Context, _args: &[&str]) {
        print!("\x1B[2J\x1B[H");
    }
}
