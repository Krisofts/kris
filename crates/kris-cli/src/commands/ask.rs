use std::io::Write;
use std::time::Duration;

use serde_json::Value;
use tokio::time::sleep;

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

        let result = runtime.block_on(async {
            let spinner = tokio::spawn(spin());

            let result = agent
                .run(
                    &mut context.history,
                    &project.root,
                    &project.name,
                    &prompt,
                    |tool_name, args: &Value, result: &str| {
                        clear_line();
                        println!("→ using tool `{tool_name}` {args}");

                        if let Some(err) = result.strip_prefix("Error: ") {
                            println!("  ✗ {err}");
                        }
                    },
                )
                .await;

            spinner.abort();
            clear_line();

            result
        });

        match result {
            Ok(answer) => {
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

fn clear_line() {
    print!("\r{}\r", " ".repeat(24));
    let _ = std::io::stdout().flush();
}

async fn spin() {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut i = 0;

    loop {
        print!("\r{} thinking...", FRAMES[i % FRAMES.len()]);
        let _ = std::io::stdout().flush();
        i += 1;
        sleep(Duration::from_millis(90)).await;
    }
}
