use crate::{command::Command, context::Context};

use kris_core::project::ProjectType;

pub struct WorkspaceCommand;

impl Command for WorkspaceCommand {
    fn name(&self) -> &'static str {
        "workspace"
    }

    fn description(&self) -> &'static str {
        "Show current workspace information"
    }

    fn execute(&self, context: &mut Context, _args: &[&str]) {
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
}
