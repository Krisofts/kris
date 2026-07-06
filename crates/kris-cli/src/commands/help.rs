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
        println!("Available commands:");
        println!("  help       Show available commands");
        println!("  version    Show KRIS version");
        println!("  workspace  Show current workspace");
        println!("  ls         List files in workspace");
        println!("  clear      Clear terminal");
        println!("  exit       Exit KRIS");
        println!("  cat        Display file contents");
        println!("  pwd        Show current workspace");
        println!("  tree       Show workspace tree");
        println!("  find       Find files by name");
        println!("  ask        Ask the local coding assistant a question");
        println!("  reset      Reset the assistant conversation");
        println!("  config     Show or change assistant settings");
    }
}