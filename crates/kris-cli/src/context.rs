use std::path::Path;

use kris_agent::Message;
use kris_core::project::Project;
use kris_core::settings::Settings;
use kris_core::workspace::Workspace;

pub struct Context {
    pub workspace: Option<Project>,
    pub settings: Settings,
    pub history: Vec<Message>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            workspace: Workspace::discover(),
            settings: Settings::load(),
            history: Vec::new(),
        }
    }

    pub fn with_path(path: &Path) -> Self {
        Self {
            workspace: Workspace::open(path),
            settings: Settings::load(),
            history: Vec::new(),
        }
    }
}
