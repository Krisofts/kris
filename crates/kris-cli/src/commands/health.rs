use crate::{
    command::Command,
    commands::serve::check_health,
    context::Context,
    style::{green, red},
};

pub struct HealthCommand;

impl Command for HealthCommand {
    fn name(&self) -> &'static str {
        "health"
    }

    fn description(&self) -> &'static str {
        "Check whether llama-server is reachable"
    }

    fn execute(&self, context: &mut Context, _args: &[&str]) {
        let url = &context.settings.llama_url;

        if check_health(url) {
            println!("{}", green(&format!("llama-server is up at {url}")));
        } else {
            println!("{}", red(&format!("llama-server is not reachable at {url}")));
            println!("Run `serve` to start it, or check ~/llama-server.log if it should already be running.");
        }
    }
}
