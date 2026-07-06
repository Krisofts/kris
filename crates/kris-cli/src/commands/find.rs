use crate::{
    command::Command,
    context::Context,
};

use kris_tools::file::find::find;

pub struct FindCommand;

impl Command for FindCommand {
    fn name(&self) -> &'static str {
        "find"
    }

    fn description(&self) -> &'static str {
        "Find files by name"
    }

    fn execute(
        &self,
        context: &mut Context,
        args: &[&str],
    ) {
        if args.is_empty() {
            println!("Usage: find <keyword>");
            return;
        }

        let Some(project) = &context.workspace else {
            println!("No workspace loaded.");
            return;
        };

        match find(&project.root, args[0]) {
            Ok(files) => {
                if files.is_empty() {
                    println!("No files found.");
                } else {
                    for file in files {
                        println!("{file}");
                    }
                }
            }
            Err(err) => {
                println!("Error: {err}");
            }
        }
    }
}