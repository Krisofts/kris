use serde_json::Value;

use kris_agent::{Agent, LlamaClient};
use kris_tools::tool::ToolRegistry;

use crate::{command::Command, context::Context};

pub struct AskCommand;

impl Command for AskCommand {
    fn name(&self) -> &'static str {
        "ask"
    }

    fn description(&self) -> &'static str {
        "Ask the local coding assistant a question (needs a running llama-server)"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        if args.is_empty() {
            println!("Usage: ask <question>");
            return;
        }

        let Some(project) = context.workspace.clone() else {
            println!("No workspace loaded.");
            return;
        };

        let prompt = args.join(" ");
        let settings = context.settings.clone();

        let client = LlamaClient::new(settings.llama_url.clone(), settings.model.clone());
        let tools = ToolRegistry::with_defaults();
        let agent = Agent::new(
            client,
            tools,
            settings.temperature,
            settings.max_tokens,
            settings.max_tool_iterations,
        );

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                println!("Failed to start async runtime: {err}");
                return;
            }
        };

        let result = runtime.block_on(agent.run(
            &mut context.history,
            &project.root,
            &project.name,
            &prompt,
            |tool_name, args: &Value| {
                println!("→ using tool `{tool_name}` {args}");
            },
        ));

        match result {
            Ok(answer) => {
                println!();
                println!("{answer}");
            }
            Err(err) => {
                println!("Error talking to the model: {err}");
                println!(
                    "Make sure llama-server is running at {}",
                    context.settings.llama_url
                );
            }
        }
    }
}
