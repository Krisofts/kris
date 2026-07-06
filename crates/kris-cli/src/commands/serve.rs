use std::io::Write;
use std::path::Path;
use std::time::Duration;

use kris_core::home::home_dir;

use crate::{
    command::Command,
    context::Context,
    style::{dim, green, red, yellow},
};

pub struct ServeCommand;

impl Command for ServeCommand {
    fn name(&self) -> &'static str {
        "serve"
    }

    fn description(&self) -> &'static str {
        "Start llama-server in the background if it isn't already running"
    }

    fn execute(&self, context: &mut Context, _args: &[&str]) {
        let settings = context.settings.clone();

        if check_health(&settings.llama_url) {
            println!(
                "{}",
                green(&format!(
                    "llama-server is already running at {}",
                    settings.llama_url
                ))
            );
            return;
        }

        if settings.model_path.is_empty() {
            println!("{}", red("No model_path configured."));
            println!("Set it: config set model_path /path/to/model.gguf");
            return;
        }

        if !Path::new(&settings.model_path).is_file() {
            println!(
                "{}",
                red(&format!("Model file not found: {}", settings.model_path))
            );
            return;
        }

        if !Path::new(&settings.llama_server_path).is_file() {
            println!(
                "{}",
                red(&format!(
                    "llama-server binary not found: {}",
                    settings.llama_server_path
                ))
            );
            println!("Set it: config set llama_server_path /path/to/llama-server");
            return;
        }

        let Some((host, port)) = parse_host_port(&settings.llama_url) else {
            println!(
                "{}",
                red(&format!(
                    "Could not parse host/port from llama_url: {}",
                    settings.llama_url
                ))
            );
            return;
        };

        let log_path = home_dir()
            .map(|home| home.join("llama-server.log"))
            .unwrap_or_else(|| Path::new("llama-server.log").to_path_buf());

        let mut command = format!(
            "nohup {} -m {} --host {host} --port {port} -c {} ",
            shell_quote(&settings.llama_server_path),
            shell_quote(&settings.model_path),
            settings.context_size,
        );

        if let Some(threads) = settings.threads {
            command.push_str(&format!("-t {threads} "));
        }

        if settings.mlock {
            command.push_str("--mlock ");
        }

        command.push_str(&format!(
            "> {} 2>&1 &",
            shell_quote(&log_path.display().to_string())
        ));

        println!(
            "{}",
            dim(&format!(
                "Starting llama-server (log: {})...",
                log_path.display()
            ))
        );

        if let Err(err) = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .status()
        {
            println!("{}", red(&format!("Failed to launch llama-server: {err}")));
            return;
        }

        print!("Waiting for llama-server to become ready");
        let _ = std::io::stdout().flush();

        for _ in 0..30 {
            std::thread::sleep(Duration::from_secs(2));
            print!(".");
            let _ = std::io::stdout().flush();

            if check_health(&settings.llama_url) {
                println!();
                println!(
                    "{}",
                    green(&format!("llama-server is up at {}", settings.llama_url))
                );
                return;
            }
        }

        println!();
        println!(
            "{}",
            yellow(&format!(
                "llama-server didn't respond within 60s - check {}",
                log_path.display()
            ))
        );
    }
}

fn parse_host_port(url: &str) -> Option<(String, String)> {
    let without_scheme = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let host_port = without_scheme.split('/').next()?;
    let (host, port) = host_port.split_once(':')?;

    Some((host.to_string(), port.to_string()))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Shared with the `health` command.
pub fn check_health(url: &str) -> bool {
    let client = kris_agent::LlamaClient::new(url.to_string(), String::new());

    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(client.health()),
        Err(_) => false,
    }
}
