use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::client::{Backend, ModelClient};
use crate::config::{Provider, Settings};
use crate::style::{dim, green, red, yellow};

pub fn client_for(settings: &Settings) -> ModelClient {
    match settings.provider {
        Provider::Local => {
            ModelClient::new(settings.llama_url.clone(), String::new(), Backend::Llama, None)
        }
        Provider::Gemini => ModelClient::new(
            settings.gemini_url.clone(),
            settings.gemini_model.clone(),
            Backend::OpenAiCompat,
            settings.resolved_api_key(),
        ),
    }
}

pub async fn check_health(settings: &Settings) -> bool {
    match settings.provider {
        Provider::Local => client_for(settings).health().await,
        // The online provider has no cheap unauthenticated health endpoint;
        // treat "an API key is configured" as ready and let the first real
        // request surface any auth or network problem.
        Provider::Gemini => settings.resolved_api_key().is_some(),
    }
}

/// Starts llama-server if it isn't already reachable, and waits (up to 60s)
/// for it to come up. Shared by the `serve` command and by `ask`/`fix`,
/// which call this automatically before talking to the model, and again if
/// a mid-session request fails with a connection error - so a phone that
/// killed the background process while KRIS was idle self-heals instead of
/// just erroring out.
pub async fn ensure_running(settings: &Settings) -> bool {
    // Online mode has no local process to launch or wait on - readiness is
    // just "is there an API key?". The rest of this function is entirely
    // about managing a local llama-server.
    if settings.provider == Provider::Gemini {
        return ensure_online_ready(settings);
    }

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

    // A slow/overloaded llama-server can miss the health check's short
    // timeout even though it's genuinely still running - checking
    // whether anything is bound to the port at all (a raw TCP connect,
    // which succeeds once the OS accepts it into the listen backlog,
    // without needing the application itself to respond) avoids
    // launching a second instance on top of it. That second instance
    // would try to load another multi-GB model into memory before even
    // discovering the port is taken - exactly the kind of compounding
    // memory pressure that turns "briefly slow" into the whole phone
    // swapping itself into the ground.
    if port_is_open(&host, &port).await {
        println!(
            "{}",
            yellow(&format!(
                "Something is already listening on {} (a busy llama-server, most likely) - \
                 waiting instead of starting a second one...",
                settings.llama_url
            ))
        );
        return wait_for_health(settings).await;
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

    wait_for_health(settings).await
}

/// Online-mode counterpart to `ensure_running`: there's nothing to start,
/// so this just checks a key is present and reports which model requests
/// will go to.
fn ensure_online_ready(settings: &Settings) -> bool {
    if settings.resolved_api_key().is_none() {
        println!("{}", red("Online mode selected but no API key is set."));
        println!(
            "Set it: export GEMINI_API_KEY=...  (or `config set gemini_api_key <key>`)"
        );
        return false;
    }

    println!(
        "{}",
        green(&format!(
            "Online mode: {} via {}",
            settings.gemini_model, settings.gemini_url
        ))
    );
    true
}

/// Polls `/health` every 2s for up to a minute, printing dots while
/// waiting. Shared by "just launched it" and "something's already on
/// the port, hopefully it's just busy" - the two situations that end
/// with the same "wait and see" step.
async fn wait_for_health(settings: &Settings) -> bool {
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
            "llama-server still hasn't responded after 60s - it may just be very busy \
             (check ~/llama-server.log for what it's doing) rather than actually down."
        ))
    );
    false
}

/// A raw TCP connect, succeeding as soon as the OS accepts it into the
/// listen backlog - unlike an HTTP health check, this doesn't need the
/// application itself to be responsive, so it's a reliable "is anything
/// bound here at all" signal even while llama-server is too CPU/memory-
/// starved to answer requests.
async fn port_is_open(host: &str, port: &str) -> bool {
    let addr = format!("{host}:{port}");
    tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(&addr))
        .await
        .map(|result| result.is_ok())
        .unwrap_or(false)
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
