use kris_core::home::resolve_path;

use crate::{command::Command, context::Context, style::green};

pub struct ConfigCommand;

impl Command for ConfigCommand {
    fn name(&self) -> &'static str {
        "config"
    }

    fn description(&self) -> &'static str {
        "Show or change assistant settings (model, llama-server url, ...)"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        if args.is_empty() {
            let settings = &context.settings;
            println!("llama_url           : {}", settings.llama_url);
            println!("model                : {}", settings.model);
            println!("temperature          : {}", settings.temperature);
            println!("max_tokens           : {}", settings.max_tokens);
            println!("max_tool_iterations  : {}", settings.max_tool_iterations);
            println!("llama_server_path    : {}", settings.llama_server_path);
            println!("model_path           : {}", settings.model_path);
            println!("context_size         : {}", settings.context_size);
            println!(
                "threads              : {}",
                settings
                    .threads
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "auto".to_string())
            );
            println!("mlock                : {}", settings.mlock);
            println!();
            println!("Usage: config set <key> <value>");
            return;
        }

        if args[0] != "set" || args.len() < 3 {
            println!("Usage: config set <key> <value>");
            return;
        }

        let key = args[1];
        let value = args[2..].join(" ");

        match key {
            "llama_url" => context.settings.llama_url = value,
            "model" => context.settings.model = value,
            "llama_server_path" => {
                context.settings.llama_server_path = resolve_path(&value).display().to_string()
            }
            "model_path" => {
                context.settings.model_path = resolve_path(&value).display().to_string()
            }
            "temperature" => match value.parse() {
                Ok(parsed) => context.settings.temperature = parsed,
                Err(_) => {
                    println!("Invalid number for temperature: {value}");
                    return;
                }
            },
            "max_tokens" => match value.parse() {
                Ok(parsed) => context.settings.max_tokens = parsed,
                Err(_) => {
                    println!("Invalid number for max_tokens: {value}");
                    return;
                }
            },
            "max_tool_iterations" => match value.parse() {
                Ok(parsed) => context.settings.max_tool_iterations = parsed,
                Err(_) => {
                    println!("Invalid number for max_tool_iterations: {value}");
                    return;
                }
            },
            "context_size" => match value.parse() {
                Ok(parsed) => context.settings.context_size = parsed,
                Err(_) => {
                    println!("Invalid number for context_size: {value}");
                    return;
                }
            },
            "threads" => {
                if value.eq_ignore_ascii_case("auto") {
                    context.settings.threads = None;
                } else {
                    match value.parse() {
                        Ok(parsed) => context.settings.threads = Some(parsed),
                        Err(_) => {
                            println!("Invalid number for threads: {value} (or use \"auto\")");
                            return;
                        }
                    }
                }
            }
            "mlock" => match value.parse() {
                Ok(parsed) => context.settings.mlock = parsed,
                Err(_) => {
                    println!("Invalid bool for mlock: {value} (use true or false)");
                    return;
                }
            },
            other => {
                println!("Unknown setting: {other}");
                return;
            }
        }

        match context.settings.save() {
            Ok(()) => println!("{}", green("Saved.")),
            Err(err) => println!("Failed to save settings: {err}"),
        }
    }
}
