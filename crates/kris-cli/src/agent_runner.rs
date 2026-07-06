use std::io::Write;
use std::time::Duration;

use serde_json::Value;
use tokio::time::sleep;

use kris_agent::{Agent, LlamaClient};
use kris_tools::tool::ToolRegistry;

use crate::context::Context;
use crate::style::{bold, dim, red};

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

    println!();

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
                    println!("{} {}", dim("●"), bold(&format_tool_call(tool_name, args)));

                    if let Some(err) = result.strip_prefix("Error: ") {
                        println!("  {} {}", red("✗"), red(err));
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
            println!();
            println!("{answer}");
        }
        Err(err) => {
            println!();
            println!("{} {err}", red("Error talking to the model:"));
            println!(
                "{}",
                dim(&format!(
                    "Make sure llama-server is running at {}",
                    context.settings.llama_url
                ))
            );
        }
    }
}

/// Renders a tool call the way Claude Code shows them: `name(primary arg)`
/// instead of dumping the raw JSON args, so the chat stays readable.
fn format_tool_call(tool_name: &str, args: &Value) -> String {
    if tool_name == "move_file"
        && let (Some(from), Some(to)) = (
            args.get("from").and_then(Value::as_str),
            args.get("to").and_then(Value::as_str),
        )
    {
        return format!("{tool_name}({from} → {to})");
    }

    let summary = ["command", "path", "pattern", "keyword"]
        .into_iter()
        .find_map(|key| args.get(key).and_then(Value::as_str));

    match summary {
        Some(summary) => format!("{tool_name}({summary})"),
        None => format!("{tool_name}()"),
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
        print!("\r{}", dim(&format!("{} thinking...", FRAMES[i % FRAMES.len()])));
        let _ = std::io::stdout().flush();
        i += 1;
        sleep(Duration::from_millis(90)).await;
    }
}
