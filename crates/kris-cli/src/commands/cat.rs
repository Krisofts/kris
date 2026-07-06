use crate::{command::Command, context::Context};

use kris_tools::file::cat::cat;

pub struct CatCommand;

impl Command for CatCommand {
    fn name(&self) -> &'static str {
        "cat"
    }

    fn description(&self) -> &'static str {
        "Display file contents"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: cat <file>");
            return;
        }

        let Some(project) = &context.workspace else {
            println!("No workspace loaded.");
            return;
        };

        let path = project.root.join(args[0]);

        match cat(path) {
            Ok(content) => println!("{content}"),
            Err(err) => println!("Error: {err}"),
        }
    }
}
