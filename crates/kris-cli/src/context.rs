use kris_core::project::Project;
use kris_core::workspace::Workspace;

pub struct Context {
    pub workspace: Option<Project>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            workspace: Workspace::discover(),
        }
    }
}
