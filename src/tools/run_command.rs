use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::style::{blue, dim};
use crate::term::{terminal_width, truncate_to_width};

use super::{Tool, ToolError, AWAITING_CONFIRMATION, COMMAND_RUNNING};

const MAX_OUTPUT: usize = 4000;
const TIMEOUT: Duration = Duration::from_secs(120);
// How long execute() gives the reader threads to drain whatever's already
// sitting in the OS pipe buffer *after* the shell itself has already
// exited, before giving up and reading a snapshot of however much arrived
// - confirmed on-device to otherwise hang forever: `sh -c` returns
// immediately once it backgrounds a process (`cmd &`, `cmd | tee log`,
// ...), but that process inherits the same piped stdout/stderr and keeps
// them open for as long as it keeps running (a dev server, typically
// forever), so the reader threads never see EOF. A short bound is safe:
// the shell has already exited, so any of its own output is already
// sitting fully in the pipe buffer, readable near-instantly.
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
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
         finished - for a process that should keep running (a dev server), redirect its \
         output to a file and background it with `&` instead, e.g. `npm run dev > \
         dev.log 2>&1 &`, which returns immediately; don't rely on `tmux` being installed. \
         Check on it afterward by reading that log file, not by re-running the same \
         command."
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
            println!();
            print_confirmation_box(root, command);
            print!("Run it? [y/N, or a = always for this session]: ");
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

        let stdout_capture = spawn_reader(child.stdout.take());
        let stderr_capture = spawn_reader(child.stderr.take());

        // Told to the REPL's own spinner task so it stops redrawing its
        // "thinking..." line over this one while this blocks the thread
        // polling the agent's future - see `COMMAND_RUNNING`'s own doc
        // comment for why both would otherwise race over the same line.
        COMMAND_RUNNING.store(true, Ordering::SeqCst);

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

        COMMAND_RUNNING.store(false, Ordering::SeqCst);

        // Clear the live status line - repl.rs prints its own "●"/boxed
        // result right after this returns, which shouldn't have to share
        // a line with a leftover spinner frame.
        print!("\r\x1b[K");
        let _ = io::stdout().flush();

        // Give the reader threads a brief, bounded chance to drain whatever
        // arrived - most commands have already finished by this point (see
        // OUTPUT_DRAIN_TIMEOUT's own comment), so this is a quick poll
        // rather than an unconditional sleep, and only actually waits the
        // full timeout for the backgrounded-process case it exists for.
        let drain_deadline = Instant::now() + OUTPUT_DRAIN_TIMEOUT;
        while Instant::now() < drain_deadline
            && !(stdout_capture.finished.load(Ordering::SeqCst)
                && stderr_capture.finished.load(Ordering::SeqCst))
        {
            thread::sleep(Duration::from_millis(20));
        }

        // Snapshotting the shared buffers directly (rather than waiting on
        // a channel the reader threads only send over once they see EOF)
        // means whatever a still-running backgrounded process already
        // wrote before we stopped waiting is kept, instead of being
        // silently discarded just because the pipe never formally closed.
        let mut combined =
            String::from_utf8_lossy(&stdout_capture.buf.lock().unwrap()).into_owned();
        combined.push_str(&String::from_utf8_lossy(
            &stderr_capture.buf.lock().unwrap(),
        ));

        let incomplete_capture = !stdout_capture.finished.load(Ordering::SeqCst)
            || !stderr_capture.finished.load(Ordering::SeqCst);

        if combined.len() > MAX_OUTPUT {
            combined.truncate(MAX_OUTPUT);
            combined.push_str("\n...(output truncated)");
        }

        if timed_out {
            combined.push_str("\n(killed after 120s timeout)");
        }

        if incomplete_capture {
            combined.push_str(
                "\n(output capture incomplete - this command likely started a background \
                 process that's still holding stdout/stderr open, e.g. via `&` or `| tee`. \
                 Redirect its output to a file instead, e.g. `cmd > out.log 2>&1 &`, then \
                 read that file separately to check on it.)",
            );
        }

        let status = status_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        Ok(format!("exit code: {status}\n{combined}"))
    }
}

/// Closed box (╭─╮ / │ … │ / ╰─╯) around the command about to run - the
/// same border style `print_boxed_output` in repl.rs uses for a command's
/// result, so what's about to run and what it produced read as one
/// consistent shape instead of two different box styles.
fn print_confirmation_box(root: &Path, command: &str) {
    let title = format!("KRIS wants to run a command in {}:", root.display());

    let max_inner = terminal_width().saturating_sub(4).max(1);
    let title_line = truncate_to_width(&title, max_inner);
    let command_line = truncate_to_width(command, max_inner);

    let inner_width = title_line
        .chars()
        .count()
        .max(command_line.chars().count())
        .max(1);
    let rule = "─".repeat(inner_width + 2);

    let title_padded = format!("{title_line:<inner_width$}");
    let command_padded = dim(&format!("{command_line:<inner_width$}"));

    println!("{}", blue(&format!("╭{rule}╮")));
    println!("{} {} {}", blue("│"), title_padded, blue("│"));
    println!("{} {} {}", blue("│"), command_padded, blue("│"));
    println!("{}", blue(&format!("╰{rule}╯")));
}

/// A pipe drained on its own thread into a shared buffer, plus whether
/// that thread has actually seen EOF yet - checking `finished` lets the
/// caller take a snapshot of `buf` without having to wait for it, which
/// matters when the pipe's write end outlives the command that opened it
/// (a backgrounded child process inheriting the same stdout/stderr).
struct StreamCapture {
    buf: Arc<Mutex<Vec<u8>>>,
    finished: Arc<AtomicBool>,
}

fn spawn_reader<R>(pipe: Option<R>) -> StreamCapture
where
    R: Read + Send + 'static,
{
    let buf = Arc::new(Mutex::new(Vec::new()));
    let finished = Arc::new(AtomicBool::new(false));
    let capture = StreamCapture {
        buf: buf.clone(),
        finished: finished.clone(),
    };

    thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 4096];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut guard) = buf.lock() {
                            guard.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        }
        finished.store(true, Ordering::SeqCst);
    });

    capture
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_backgrounded_child_holding_stdout_open_does_not_hang_forever() {
        // Regression test: `sh -c "cmd &"` returns almost immediately once
        // it backgrounds a process, but that process inherits the same
        // piped stdout/stderr and can hold them open indefinitely (a dev
        // server, in practice forever) - confirmed on-device to hang the
        // whole agent loop forever with no error and no visible spinner,
        // since the old code waited on a channel the reader thread only
        // ever sent over after seeing EOF. execute() runs on its own
        // thread here so the test can bound how long it waits, rather than
        // hanging the whole suite if this regresses.
        let dir = tempfile::tempdir().unwrap();
        let tool = RunCommandTool::new(true);
        let root = dir.path().to_path_buf();

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = tool.execute(
                &root,
                &json!({ "command": "echo before; (sleep 30 &); echo after" }),
            );
            let _ = tx.send(result);
        });

        let result = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("execute() should return well within 10s, not hang forever")
            .expect("execute() should succeed");

        // Whatever was written before the backgrounded process took over
        // the pipe must still show up - not silently dropped just because
        // the pipe itself never formally closed.
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(result.contains("output capture incomplete"));
    }
}
