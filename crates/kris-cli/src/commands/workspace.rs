use std::path::PathBuf;

use kris_core::home::home_dir;
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

/// Expands a leading `~/`, and resolves any other relative path against the
/// home directory (matching KRIS's default `$HOME/project` convention),
/// rather than whatever the process's OS-level cwd happens to be.
fn resolve_path(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }

    let path = PathBuf::from(input);

    if path.is_absolute() {
        return path;
    }

    match home_dir() {
        Some(home) => home.join(path),
        None => path,
    }
}
