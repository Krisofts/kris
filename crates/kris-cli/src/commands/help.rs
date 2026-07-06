use crate::{
    command::Command,
    context::Context,
};

pub struct HelpCommand;

impl Command for HelpCommand {
    fn name(&self) -> &'static str {
        "help"
    }

    fn description(&self) -> &'static str {
        "Show available commands"
    }

    fn execute(
        &self,
        _context: &mut Context,
        _args: &[&str],
    ) {
        println!("Built-in commands:");
        println!("  help       Show available commands");
        println!("  version    Show KRIS version");
        println!("  workspace  Show current workspace, or `workspace <path>` to switch");
        println!("  ask        Ask the local coding assistant a question");
        println!("  reset      Reset the assistant conversation");
        println!("  config     Show or change assistant settings");
        println!("  clear      Clear terminal");
        println!("  exit       Exit KRIS");
        println!();
        println!("Anything else is run as a shell command inside the current workspace,");
        println!("e.g. `ls -la`, `git status`, `cargo build`, `npm install`, `python3 x.py`.");
    }
}
