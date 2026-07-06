use std::env;

use crate::project::{Project, ProjectType};

use super::finder::find_project_root;

pub struct Workspace;

impl Workspace {
    pub fn discover() -> Option<Project> {
        let cwd = env::current_dir().ok()?;

        let root = find_project_root(&cwd)?;

        let name = root.file_name()?.to_string_lossy().to_string();

        let project_type = if root.join("Cargo.toml").exists() {
            ProjectType::Rust
        } else if root.join("package.json").exists() {
            ProjectType::Node
        } else if root.join("artisan").exists() {
            ProjectType::Laravel
        } else {
            ProjectType::Unknown
        };

        let git = root.join(".git").exists();

        Some(Project {
            name,
            root,
            project_type,
            git,
        })
    }
}
