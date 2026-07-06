use crate::{
    commands::{
        cat::CatCommand, clear::ClearCommand, exit::ExitCommand, help::HelpCommand, ls::LsCommand,
        pwd::PwdCommand, tree::TreeCommand, version::VersionCommand, workspace::WorkspaceCommand,
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
        let mut registry = Registry::new();

        registry.register(HelpCommand);
        registry.register(VersionCommand);
        registry.register(ClearCommand);
        registry.register(ExitCommand);
        registry.register(WorkspaceCommand);
        registry.register(LsCommand);
        registry.register(CatCommand);
        registry.register(PwdCommand);
        registry.register(TreeCommand);

        Self {
            context: Context::new(),
            registry,
        }
    }
}
