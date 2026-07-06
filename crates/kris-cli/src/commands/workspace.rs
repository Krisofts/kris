use std::fs;
use std::path::{Path, PathBuf};

use kris_core::home::{home_dir, resolve_path};
use kris_core::project::ProjectType;
use kris_core::workspace::Workspace;

use crate::{
    command::Command,
    context::Context,
    style::{dim, green, yellow},
};

pub struct WorkspaceCommand;

impl Command for WorkspaceCommand {
    fn name(&self) -> &'static str {
        "workspace"
    }

    fn description(&self) -> &'static str {
        "Show current workspace info, or switch with `workspace <path|number>`"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        if args.is_empty() {
            print_info(context);
            print_candidates();
            return;
        }

        // Candidates are recomputed fresh rather than cached from the last
        // bare `workspace` call - fine as long as the folder list doesn't
        // change between seeing the menu and picking a number, which holds
        // for the normal "run `workspace`, then `workspace <n>`" flow.
        let path = match args[0].parse::<usize>() {
            Ok(n) if n >= 1 => match candidate_dirs().into_iter().nth(n - 1) {
                Some(path) => path,
                None => {
                    println!("{}", yellow(&format!("No folder numbered {n}. Run `workspace` to see the list.")));
                    return;
                }
            },
            _ => resolve_path(&args.join(" ")),
        };

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

/// Folders you could plausibly switch to: immediate subfolders of `$HOME`
/// and of `$HOME/project` (KRIS's default workspace root), deduplicated.
/// Lets `workspace <number>` work without typing a path at all.
fn candidate_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };

    let mut dirs = Vec::new();
    collect_subdirs(&home, &mut dirs);
    collect_subdirs(&home.join("project"), &mut dirs);

    dirs.sort();
    dirs.dedup();
    dirs
}

fn collect_subdirs(parent: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let is_hidden = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'));

        if path.is_dir() && !is_hidden {
            out.push(path);
        }
    }
}

fn looks_like_project(path: &Path) -> bool {
    path.join(".git").exists()
        || path.join("Cargo.toml").exists()
        || path.join("package.json").exists()
        || path.join("artisan").exists()
}

fn print_candidates() {
    let candidates = candidate_dirs();

    if candidates.is_empty() {
        return;
    }

    println!();
    println!("Switch with `workspace <number>`:");

    for (i, path) in candidates.iter().enumerate() {
        let marker = if looks_like_project(path) {
            ""
        } else {
            "  (no project files detected)"
        };

        println!(
            "  {}. {}{}",
            i + 1,
            path.display(),
            dim(marker)
        );
    }
}
