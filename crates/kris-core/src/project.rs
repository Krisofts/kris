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
