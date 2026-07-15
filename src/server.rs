use crate::client::{Backend, ModelClient};
use crate::config::{Provider, Settings};
use crate::style::{green, red};

pub fn client_for(settings: &Settings) -> ModelClient {
    match settings.provider {
        Provider::Gemini => ModelClient::new(
            settings.gemini_url.clone(),
            settings.gemini_model.clone(),
            Backend::OpenAiCompat,
            settings.resolved_api_key(),
        ),
        Provider::Claude => ModelClient::new(
            settings.claude_url.clone(),
            settings.claude_model.clone(),
            Backend::Anthropic,
            settings.resolved_api_key(),
        ),
        Provider::OpenRouter => ModelClient::new(
            settings.openrouter_url.clone(),
            settings.openrouter_model.clone(),
            Backend::OpenAiCompat,
            settings.resolved_api_key(),
        )
        .with_reasoning_effort(
            (!settings.openrouter_reasoning_effort.is_empty())
                .then(|| settings.openrouter_reasoning_effort.clone()),
        ),
        Provider::Opper => ModelClient::new(
            settings.opper_url.clone(),
            settings.opper_model.clone(),
            Backend::OpenAiCompat,
            settings.resolved_api_key(),
        ),
        Provider::Opencode => ModelClient::new(
            settings.opencode_url.clone(),
            settings.opencode_model.clone(),
            Backend::OpenAiCompat,
            settings.resolved_api_key(),
        ),
    }
}

/// None of KRIS's providers has a cheap unauthenticated health endpoint;
/// treat "an API key is configured" as ready and let the first real
/// request surface any auth or network problem.
pub async fn check_health(settings: &Settings) -> bool {
    settings.resolved_api_key().is_some()
}

/// KRIS is online-only, so there's no local process to launch or wait on -
/// readiness is just "is there an API key?". Reports which model requests
/// will go to. Shared by `ask`/`fix` (called automatically before talking
/// to the model) and the `health` command.
pub async fn ensure_running(settings: &Settings) -> bool {
    let (env_var, config_key, model, url) = match settings.provider {
        Provider::Gemini => (
            "GEMINI_API_KEY",
            "gemini_api_key",
            &settings.gemini_model,
            &settings.gemini_url,
        ),
        Provider::Claude => (
            "ANTHROPIC_API_KEY",
            "claude_api_key",
            &settings.claude_model,
            &settings.claude_url,
        ),
        Provider::OpenRouter => (
            "OPENROUTER_API_KEY",
            "openrouter_api_key",
            &settings.openrouter_model,
            &settings.openrouter_url,
        ),
        Provider::Opper => (
            "OPPER_API_KEY",
            "opper_api_key",
            &settings.opper_model,
            &settings.opper_url,
        ),
        Provider::Opencode => (
            "OPENCODE_API_KEY",
            "opencode_api_key",
            &settings.opencode_model,
            &settings.opencode_url,
        ),
    };

    if settings.resolved_api_key().is_none() {
        println!("{}", red("No API key is set for the active provider."));
        println!("Set it: export {env_var}=...  (or `config set {config_key} <key>`)");
        return false;
    }

    println!("{}", green(&format!("Ready: {model} via {url}")));
    true
}
