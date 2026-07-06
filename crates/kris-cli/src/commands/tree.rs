use crate::{
    command::Command,
    context::Context,
};

use kris_tools::file::tree::tree;

pub struct TreeCommand;

impl Command for TreeCommand {
    fn name(&self) -> &'static str {
        "tree"
    }

    fn description(&self) -> &'static str {
        "Show workspace tree"
    }

    fn execute(
        &self,
        context: &mut Context,
        _args: &[&str],
    ) {
        let Some(project) = &context.workspace else {
            println!("No workspace loaded.");
            return;
        };

        match tree(&project.root) {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
            }
            Err(err) => {
                println!("Error: {err}");
            }
        }
    }
}