use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::style::dim;

use super::{Tool, ToolError, AWAITING_CONFIRMATION};

const MAX_OUTPUT: usize = 4000;
const TIMEOUT: Duration = Duration::from_secs(120);
const SPINNER_FRAMES: [&str; 4] = ["-", "\\", "|", "/"];

pub struct RunCommandTool {
    auto_approve: Cell<bool>,
}

impl RunCommandTool {
    pub fn new(auto_approve: bool) -> Self {
        Self {
            auto_approve: Cell::new(auto_approve),
        }
    }
}

impl Tool for RunCommandTool {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Run a shell command inside the project root (e.g. `cargo build`, `cargo test`, \
         `npm install`). Asks the user for a y/n confirmation before executing anything. \
         Output is captured and truncated if long. Killed after 2 minutes if it hasn't \
         finished - for a process that should keep running (a dev server), background it \
         yourself instead, e.g. `tmux new-session -d -s preview 'npm run dev'`, which \
         returns immediately."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to run, e.g. \"cargo test\"" }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("command".to_string()))?;

        if !self.auto_approve.get() {
            AWAITING_CONFIRMATION.store(true, Ordering::SeqCst);

            print!("\r{}\r", " ".repeat(60));
            println!("\n┌─ KRIS wants to run a command in {}:", root.display());
            println!("│  {command}");
            print!("└─ Run it? [y/N, or a = always for this session]: ");
            let _ = io::stdout().flush();

            let mut input = String::new();
            let read = io::stdin().read_line(&mut input);

            AWAITING_CONFIRMATION.store(false, Ordering::SeqCst);

            if read.is_err() {
                return Ok("Command not executed (could not read confirmation).".to_string());
            }

            match input.trim().to_lowercase().as_str() {
                "y" | "yes" => {}
                "a" | "always" => self.auto_approve.set(true),
                _ => return Ok("Command not executed (user declined).".to_string()),
            }
        }

        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(root)
            // Without this, the child inherits KRIS's own stdin - so a
            // command that turns out to need interactive input (e.g. `npm
            // create vite` asking "Which linter?") just sits there with
            // nobody answering it, confirmed on-device to run out the
            // full 120s TIMEOUT before getting killed instead of failing
            // fast. Closing stdin makes most CLIs either bail immediately
            // on EOF or fall back to a non-interactive default.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| ToolError::Tool(format!("failed to run command: {err}")))?;

        let stdout_rx = spawn_reader(child.stdout.take());
        let stderr_rx = spawn_reader(child.stderr.take());

        let start = Instant::now();
        let mut timed_out = false;
        let mut frame = 0usize;

        // KRIS's own "thinking..." spinner only covers waiting for the
        // *model*, not this - a slow command (confirmed on-device: `cargo
        // build` pulling dependencies) otherwise leaves the screen
        // completely still, with no way to tell it from a genuine hang,
        // for however long it takes to finish or hit the timeout below.
        let status_code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if start.elapsed() > TIMEOUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break None;
                    }
                    print!(
                        "\r\x1b[K{} {}",
                        dim(SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]),
                        dim(&format!("running... {}s", start.elapsed().as_secs()))
                    );
                    let _ = io::stdout().flush();
                    frame += 1;
                    thread::sleep(Duration::from_millis(150));
                }
                Err(_) => break None,
            }
        };

        // Clear the live status line - repl.rs prints its own "●"/boxed
        // result right after this returns, which shouldn't have to share
        // a line with a leftover spinner frame.
        print!("\r\x1b[K");
        let _ = io::stdout().flush();

        let mut combined = stdout_rx.recv().unwrap_or_default();
        combined.push_str(&stderr_rx.recv().unwrap_or_default());

        if combined.len() > MAX_OUTPUT {
            combined.truncate(MAX_OUTPUT);
            combined.push_str("\n...(output truncated)");
        }

        if timed_out {
            combined.push_str("\n(killed after 120s timeout)");
        }

        let status = status_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        Ok(format!("exit code: {status}\n{combined}"))
    }
}

fn spawn_reader<R>(pipe: Option<R>) -> mpsc::Receiver<String>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });

    rx
}
