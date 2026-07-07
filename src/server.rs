use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::client::LlamaClient;
use crate::config::Settings;
use crate::style::{dim, green, red, yellow};

pub fn client_for(settings: &Settings) -> LlamaClient {
    LlamaClient::new(settings.llama_url.clone(), String::new())
}

pub async fn check_health(settings: &Settings) -> bool {
    client_for(settings).health().await
}

/// Starts llama-server if it isn't already reachable, and waits (up to 60s)
/// for it to come up. Shared by the `serve` command and by `ask`/`fix`,
/// which call this automatically before talking to the model, and again if
/// a mid-session request fails with a connection error - so a phone that
/// killed the background process while KRIS was idle self-heals instead of
/// just erroring out.
pub async fn ensure_running(settings: &Settings) -> bool {
    if check_health(settings).await {
        println!(
            "{}",
            green(&format!(
                "llama-server is already running at {}",
                settings.llama_url
            ))
        );
        return true;
    }

    if settings.model_path.is_empty() {
        println!("{}", red("No model_path configured."));
        println!("Set it: config set model_path /path/to/model.gguf");
        return false;
    }

    if !Path::new(&settings.model_path).is_file() {
        println!(
            "{}",
            red(&format!("Model file not found: {}", settings.model_path))
        );
        return false;
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
        return false;
    }

    let Some((host, port)) = parse_host_port(&settings.llama_url) else {
        println!(
            "{}",
            red(&format!(
                "Could not parse host/port from llama_url: {}",
                settings.llama_url
            ))
        );
        return false;
    };

    let log_path = dirs::home_dir()
        .map(|home| home.join("llama-server.log"))
        .unwrap_or_else(|| Path::new("llama-server.log").to_path_buf());

    let mut command = format!(
        "nohup {} -m {} --host {host} --port {port} -c {} --jinja ",
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
    if settings.flash_attn {
        // Explicit value, not a bare flag: recent llama-server builds parse
        // --flash-attn as taking an on/off/auto argument, and a bare flag
        // would swallow the next flag's token as its value.
        command.push_str("--flash-attn on ");
    }
    if let Some(cache_type) = &settings.cache_type_k {
        command.push_str(&format!("--cache-type-k {} ", shell_quote(cache_type)));
    }
    if let Some(cache_type) = &settings.cache_type_v {
        command.push_str(&format!("--cache-type-v {} ", shell_quote(cache_type)));
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

    if let Err(err) = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .status()
        .await
    {
        println!("{}", red(&format!("Failed to launch llama-server: {err}")));
        return false;
    }

    print!("Waiting for llama-server to become ready");
    let _ = std::io::stdout().flush();

    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        print!(".");
        let _ = std::io::stdout().flush();

        if check_health(settings).await {
            println!();
            println!(
                "{}",
                green(&format!("llama-server is up at {}", settings.llama_url))
            );
            return true;
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
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_and_port_from_url() {
        assert_eq!(
            parse_host_port("http://127.0.0.1:8080"),
            Some(("127.0.0.1".to_string(), "8080".to_string()))
        );
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
