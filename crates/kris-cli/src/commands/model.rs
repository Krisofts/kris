use std::path::PathBuf;

use kris_core::home::home_dir;

use crate::{
    command::Command,
    context::Context,
    style::{bold, dim, green, yellow},
};

struct ModelPreset {
    label: &'static str,
    repo: &'static str,
    file: &'static str,
}

// `static`, not `const`: presets are looked up by a runtime index, and a
// `const` array would be re-materialized (as a temporary) at each use site
// rather than living at one fixed 'static address.
static PRESETS: [ModelPreset; 3] = [
    ModelPreset {
        label: "Qwen2.5-Coder-1.5B-Instruct (smallest, fastest)",
        repo: "Qwen/Qwen2.5-Coder-1.5B-Instruct-GGUF",
        file: "qwen2.5-coder-1.5b-instruct-q4_k_m.gguf",
    },
    ModelPreset {
        label: "Qwen2.5-Coder-3B-Instruct (balanced)",
        repo: "Qwen/Qwen2.5-Coder-3B-Instruct-GGUF",
        file: "qwen2.5-coder-3b-instruct-q4_k_m.gguf",
    },
    ModelPreset {
        label: "Qwen2.5-Coder-7B-Instruct (best quality, needs more RAM)",
        repo: "Qwen/Qwen2.5-Coder-7B-Instruct-GGUF",
        file: "qwen2.5-coder-7b-instruct-q4_k_m.gguf",
    },
];

pub struct ModelCommand;

impl Command for ModelCommand {
    fn name(&self) -> &'static str {
        "model"
    }

    fn description(&self) -> &'static str {
        "Show or switch between Qwen2.5-Coder model sizes (model <1|2|3>)"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        if args.is_empty() {
            print_menu(context);
            return;
        }

        let Some(preset) = resolve_preset(args[0]) else {
            println!("Usage: model <1|2|3> (or 1.5b/3b/7b)");
            return;
        };

        let model_path = preset_path(preset);

        if !model_path.is_file() {
            println!(
                "{}",
                yellow(&format!("{} isn't downloaded yet.", preset.label))
            );
            println!("Download it first:");
            println!(
                "  curl -L -o {} \"https://huggingface.co/{}/resolve/main/{}?download=true\"",
                model_path.display(),
                preset.repo,
                preset.file
            );
            return;
        }

        context.settings.model = preset.file.trim_end_matches(".gguf").to_string();
        context.settings.model_path = model_path.display().to_string();

        match context.settings.save() {
            Ok(()) => {
                println!("{}", green(&format!("Switched to {}", preset.label)));
                println!(
                    "llama-server can't swap models without restarting - if one is already \
                     running, stop it first (Ctrl-C in its session, or `pkill -f llama-server`), \
                     then run `serve`."
                );
            }
            Err(err) => println!("Failed to save settings: {err}"),
        }
    }
}

fn print_menu(context: &Context) {
    println!("Current model: {}", bold(&context.settings.model));
    println!();

    for (i, preset) in PRESETS.iter().enumerate() {
        let downloaded = preset_path(preset).is_file();
        let marker = if downloaded {
            green("downloaded")
        } else {
            dim("not downloaded")
        };

        println!("  {}. {} [{}]", i + 1, preset.label, marker);
    }

    println!();
    println!("Usage: model <1|2|3>");
}

fn preset_path(preset: &ModelPreset) -> PathBuf {
    home_dir()
        .map(|home| home.join(preset.file))
        .unwrap_or_else(|| PathBuf::from(preset.file))
}

fn resolve_preset(arg: &str) -> Option<&'static ModelPreset> {
    match arg.to_lowercase().as_str() {
        "1" | "1.5b" => Some(&PRESETS[0]),
        "2" | "3b" => Some(&PRESETS[1]),
        "3" | "7b" => Some(&PRESETS[2]),
        _ => None,
    }
}
