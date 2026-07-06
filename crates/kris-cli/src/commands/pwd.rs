use crate::{command::Command, context::Context};

pub struct PwdCommand;

impl Command for PwdCommand {
    fn name(&self) -> &'static str {
        "pwd"
    }

    fn description(&self) -> &'static str {
        "Show current workspace path"
    }

    fn execute(&self, context: &mut Context, _args: &[&str]) {
        match &context.workspace {
            Some(project) => {
                println!("{}", project.root.display());
            }
            None => {
                println!("No workspace loaded.");
            }
        }
    }
}
