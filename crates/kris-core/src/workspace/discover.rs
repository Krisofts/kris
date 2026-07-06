use std::env;
use std::path::Path;

use crate::project::{Project, ProjectType};

use super::finder::find_project_root;

pub struct Workspace;

impl Workspace {
    /// Walks up from the current working directory looking for a
    /// recognizable project root (`Cargo.toml`, `package.json`, `artisan`).
    pub fn discover() -> Option<Project> {
        let cwd = env::current_dir().ok()?;

        let root = find_project_root(&cwd)?;

        Self::describe(&root)
    }

    /// Opens a project rooted exactly at `path`, without walking up parent
    /// directories. Used when the user explicitly points KRIS at a folder.
    pub fn open(path: &Path) -> Option<Project> {
        let root = path.canonicalize().ok()?;

        if !root.is_dir() {
            return None;
        }

        Self::describe(&root)
    }

    fn describe(root: &Path) -> Option<Project> {
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
            root: root.to_path_buf(),
            project_type,
            git,
        })
    }
}
