use crate::{command::Command, context::Context};

pub struct VersionCommand;

impl Command for VersionCommand {
    fn name(&self) -> &'static str {
        "version"
    }

    fn description(&self) -> &'static str {
        "Show KRIS version"
    }

    fn execute(&self, _context: &mut Context, _args: &[&str]) {
        println!("KRIS AI v0.1.0");
    }
}
