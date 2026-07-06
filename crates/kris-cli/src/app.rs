use std::path::Path;

use crate::{
    commands::{
        ask::AskCommand, clear::ClearCommand, config::ConfigCommand, exit::ExitCommand,
        help::HelpCommand, reset::ResetCommand, version::VersionCommand,
        workspace::WorkspaceCommand,
    },
    context::Context,
    registry::Registry,
};

pub struct App {
    pub context: Context,
    pub registry: Registry,
}

impl App {
    pub fn new() -> Self {
        Self {
            context: Context::new(),
            registry: build_registry(),
        }
    }

    pub fn with_path(path: &Path) -> Self {
        Self {
            context: Context::with_path(path),
            registry: build_registry(),
        }
    }
}

fn build_registry() -> Registry {
    let mut registry = Registry::new();

    registry.register(HelpCommand);
    registry.register(VersionCommand);
    registry.register(ClearCommand);
    registry.register(ExitCommand);
    registry.register(WorkspaceCommand);
    registry.register(AskCommand);
    registry.register(ResetCommand);
    registry.register(ConfigCommand);

    registry
}
