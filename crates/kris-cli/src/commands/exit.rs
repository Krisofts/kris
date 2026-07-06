use crate::{command::Command, context::Context};

pub struct ExitCommand;

impl Command for ExitCommand {
    fn name(&self) -> &'static str {
        "exit"
    }

    fn description(&self) -> &'static str {
        "Exit KRIS"
    }

    fn execute(&self, _context: &mut Context, _args: &[&str]) {
        println!("Bye.");
    }
}
