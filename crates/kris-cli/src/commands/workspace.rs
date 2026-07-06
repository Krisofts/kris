use kris_core::home::resolve_path;
use kris_core::project::ProjectType;
use kris_core::workspace::Workspace;

use crate::{
    command::Command,
    context::Context,
    style::{green, yellow},
};

pub struct WorkspaceCommand;

impl Command for WorkspaceCommand {
    fn name(&self) -> &'static str {
        "workspace"
    }

    fn description(&self) -> &'static str {
        "Show current workspace info, or switch to another with `workspace <path>`"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        if args.is_empty() {
            print_info(context);
            return;
        }

        let path = resolve_path(&args.join(" "));

        match Workspace::open(&path) {
            Some(project) => {
                println!(
                    "{}",
                    green(&format!("Switched workspace to {}", project.root.display()))
                );
                context.workspace = Some(project);

                if !context.history.is_empty() {
                    context.history.clear();
                    println!("{}", yellow("(conversation history reset)"));
                }
            }
            None => {
                println!(
                    "{}",
                    yellow(&format!(
                        "Could not open project folder \"{}\".",
                        path.display()
                    ))
                );
            }
        }
    }
}

fn print_info(context: &Context) {
    match &context.workspace {
        Some(project) => {
            println!();
            println!("Workspace Information");
            println!("---------------------");
            println!("Name : {}", project.name);
            println!("Path : {}", project.root.display());

            let project_type = match project.project_type {
                ProjectType::Rust => "Rust",
                ProjectType::Node => "Node",
                ProjectType::Laravel => "Laravel",
                ProjectType::Unknown => "Unknown",
            };

            println!("Type : {}", project_type);
            println!("Git  : {}", if project.git { "Yes" } else { "No" });
        }
        None => {
            println!("No workspace detected.");
        }
    }
}
