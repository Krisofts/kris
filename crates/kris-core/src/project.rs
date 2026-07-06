use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum ProjectType {
    Rust,
    Node,
    Laravel,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub root: PathBuf,
    pub project_type: ProjectType,
    pub git: bool,
}

impl Project {
    /// Short, language-specific guidance for the AI agent (which build/test
    /// commands to reach for) based on the detected project type.
    pub fn type_hint(&self) -> &'static str {
        match self.project_type {
            ProjectType::Rust => {
                "This is a Rust project. Use `cargo check` for fast error checking, \
                 `cargo build` to build, and `cargo test` to run tests."
            }
            ProjectType::Node => {
                "This is a Node.js project. Use `npm install` to install dependencies, \
                 `npm run build` to build, and `npm test` to run tests."
            }
            ProjectType::Laravel => {
                "This is a Laravel project. Use `composer install` to install \
                 dependencies and `php artisan` for Artisan commands."
            }
            ProjectType::Unknown => "",
        }
    }
}
