use crate::{command::Command, context::Context};

pub struct HelpCommand {
    commands: Vec<(String, String)>,
}

impl HelpCommand {
    pub fn new(commands: Vec<(String, String)>) -> Self {
        Self { commands }
    }
}

impl Command for HelpCommand {
    fn name(&self) -> &'static str {
        "help"
    }

    fn description(&self) -> &'static str {
        "Show available commands"
    }

    fn execute(&self, _context: &mut Context, _args: &[&str]) {
        println!("Built-in commands:");
        println!("  {:<10} {}", self.name(), self.description());

        for (name, description) in &self.commands {
            println!("  {name:<10} {description}");
        }

        println!();
        println!("Anything else is run as a shell command inside the current workspace,");
        println!("e.g. `ls -la`, `git status`, `cargo build`, `npm install`, `python3 x.py`.");
    }
}
