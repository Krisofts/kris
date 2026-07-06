use crate::{command::Command, context::Context};

use kris_tools::file::list::list_directory;

pub struct LsCommand;

impl Command for LsCommand {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn description(&self) -> &'static str {
        "List files in current workspace"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        let path = if args.is_empty() {
            match &context.workspace {
                Some(project) => project.root.as_path(),
                None => {
                    println!("No workspace loaded.");
                    return;
                }
            }
        } else {
            std::path::Path::new(args[0])
        };

        match list_directory(path) {
            Ok(entries) => {
                if entries.is_empty() {
                    println!("Directory is empty.");
                    return;
                }

                for entry in entries {
                    println!("{entry}");
                }
            }
            Err(err) => {
                println!("Error: {err}");
            }
        }
    }
}
