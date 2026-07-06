use std::collections::HashMap;

use crate::{command::Command, context::Context};

pub struct Registry {
    commands: HashMap<String, Box<dyn Command>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    pub fn register<C>(&mut self, command: C)
    where
        C: Command + 'static,
    {
        self.commands
            .insert(command.name().to_string(), Box::new(command));
    }

    pub fn execute(&self, context: &mut Context, input: &str) -> bool {
        let parts: Vec<&str> = input.split_whitespace().collect();

        if parts.is_empty() {
            return true;
        }

        let command_name = parts[0];
        let args = &parts[1..];

        match self.commands.get(command_name) {
            Some(command) => {
                command.execute(context, args);

                command_name != "exit"
            }
            None => {
                println!("Unknown command: {}", command_name);
                println!("Type 'help' to see available commands.");
                true
            }
        }
    }

    pub fn list(&self) -> Vec<(&str, &str)> {
        let mut commands = self
            .commands
            .values()
            .map(|command| (command.name(), command.description()))
            .collect::<Vec<_>>();

        commands.sort_by(|a, b| a.0.cmp(b.0));

        commands
    }
}
