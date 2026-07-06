use std::process::Command;

use crate::context::Context;

/// Runs anything that isn't a built-in KRIS command as a real shell command,
/// inside the current workspace (so `git status`, `cargo build`,
/// `npm install`, etc. all just work without leaving KRIS).
pub fn run(context: &Context, input: &str) {
    let mut command = Command::new("sh");
    command.arg("-c").arg(input);

    if let Some(project) = &context.workspace {
        command.current_dir(&project.root);
    }

    match command.status() {
        Ok(status) => {
            if !status.success() {
                match status.code() {
                    Some(code) => println!("(exit code {code})"),
                    None => println!("(terminated by signal)"),
                }
            }
        }
        Err(err) => {
            let command_name = input.split_whitespace().next().unwrap_or(input);
            println!("Unknown command: {command_name}");
            println!("(also failed to run it as a shell command: {err})");
            println!("Type 'help' to see built-in commands.");
        }
    }
}
