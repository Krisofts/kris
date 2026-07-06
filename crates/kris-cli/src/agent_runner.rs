use std::io::Write;
use std::time::Duration;

use serde_json::Value;
use tokio::time::sleep;

use kris_agent::{Agent, LlamaClient};
use kris_tools::tool::ToolRegistry;

use crate::context::Context;

/// Shared by `ask` and `fix`: builds the agent, runs one turn with a spinner
/// and tool-call feedback, and prints the result. `min_iterations` lets a
/// caller (e.g. `fix`) demand more tool-call rounds than the configured
/// default, for tasks that need many build/fix cycles.
pub fn run(context: &mut Context, prompt: &str, min_iterations: u32) {
    let Some(project) = context.workspace.clone() else {
        println!("No workspace loaded.");
        return;
    };

    let settings = context.settings.clone();

    let client = LlamaClient::new(settings.llama_url.clone(), settings.model.clone());
    let tools = ToolRegistry::with_defaults();
    let agent = Agent::new(
        client,
        tools,
        settings.temperature,
        settings.max_tokens,
        settings.max_tool_iterations.max(min_iterations),
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
                project.type_hint(),
                prompt,
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
        Ok(answer) => println!("{answer}"),
        Err(err) => {
            println!("Error talking to the model: {err}");
            println!(
                "Make sure llama-server is running at {}",
                context.settings.llama_url
            );
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
