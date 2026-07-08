//! A minimal up/down-arrow list picker for the terminal, used by the
//! `workspace`/`project` REPL commands to choose a project interactively
//! instead of having to type its name. Uses crossterm only for raw-mode
//! toggling and key events - rendering stays on the existing ANSI/style
//! helpers used everywhere else in the app.

use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::style::{bold, cyan, dim};

/// Result of showing the picker - kept distinct from a plain
/// `Option<String>` so a caller can tell "the user backed out" apart
/// from "this terminal can't do an interactive picker at all" (e.g.
/// stdin/stdout isn't a real TTY) and fall back to something else
/// instead of silently doing nothing either way.
pub enum PickOutcome {
    Chosen(String),
    Cancelled,
    Unavailable,
}

/// Shows `prompt` followed by `options`, lets the user move the
/// selection with the up/down arrows (or j/k) and confirm with Enter.
/// Never blocks indefinitely on input that can't arrive - if raw mode
/// can't be enabled, returns `Unavailable` immediately.
pub fn pick(prompt: &str, options: &[String], active: Option<usize>) -> PickOutcome {
    if options.is_empty() {
        return PickOutcome::Unavailable;
    }

    if enable_raw_mode().is_err() {
        return PickOutcome::Unavailable;
    }

    let mut selected = active.unwrap_or(0).min(options.len() - 1);
    let result = run(prompt, options, &mut selected);

    let _ = disable_raw_mode();

    match result {
        Some(name) => PickOutcome::Chosen(name),
        None => PickOutcome::Cancelled,
    }
}

fn run(prompt: &str, options: &[String], selected: &mut usize) -> Option<String> {
    print!("{}\r\n", dim(prompt));
    render(options, *selected, false);

    loop {
        let Ok(event) = event::read() else {
            continue;
        };
        let Event::Key(key) = event else {
            continue;
        };
        // Windows reports both press and release; only act once per key.
        if key.kind == KeyEventKind::Release {
            continue;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.checked_sub(1).unwrap_or(options.len() - 1);
                redraw(options, *selected);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1) % options.len();
                redraw(options, *selected);
            }
            KeyCode::Enter => {
                render(options, *selected, true);
                return Some(options[*selected].clone());
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                clear_lines(options.len());
                print!("{}\r\n", dim("Cancelled."));
                let _ = io::stdout().flush();
                return None;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                clear_lines(options.len());
                return None;
            }
            _ => {}
        }
    }
}

fn redraw(options: &[String], selected: usize) {
    clear_lines(options.len());
    render(options, selected, false);
}

fn render(options: &[String], selected: usize, confirmed: bool) {
    let mut out = String::new();

    for (i, name) in options.iter().enumerate() {
        if i == selected {
            let pointer = if confirmed { "\u{2713}" } else { "\u{276f}" };
            out.push_str(&format!("{} {}\r\n", cyan(pointer), bold(name)));
        } else {
            out.push_str(&format!("  {}\r\n", dim(name)));
        }
    }

    print!("{out}");
    let _ = io::stdout().flush();
}

/// Moves the cursor back up to just below the prompt line and erases
/// everything from there to the end of the screen, so the next render
/// overwrites the list in place instead of scrolling a new copy down.
fn clear_lines(n: usize) {
    print!("\x1b[{n}A\x1b[J");
    let _ = io::stdout().flush();
}
