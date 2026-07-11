//! A minimal up/down-arrow list picker for the terminal, used by the
//! `workspace`/`project` REPL commands to choose a project interactively
//! instead of having to type its name. Uses crossterm only for raw-mode
//! toggling and key events - rendering stays on the existing ANSI/style
//! helpers used everywhere else in the app.

use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::style::{bold, cyan, dim};
use crate::term::{rows_for_width, terminal_width};

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
                clear_lines(total_rows(options, terminal_width()));
                print!("{}\r\n", dim("Cancelled."));
                let _ = io::stdout().flush();
                return None;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                clear_lines(total_rows(options, terminal_width()));
                return None;
            }
            _ => {}
        }
    }
}

fn redraw(options: &[String], selected: usize) {
    clear_lines(total_rows(options, terminal_width()));
    render(options, selected, false);
}

/// How many terminal rows the current render of `options` actually
/// occupies at the given terminal width - each option is a "pointer +
/// space" (2 columns) prefix plus its label, so a long label can wrap
/// onto more than one row on a narrow terminal. `clear_lines` needs the
/// true row count, not just `options.len()`, or it moves the cursor up
/// too few lines and leaves the earlier wrapped rows behind - confirmed
/// on-device: a long ask_question option wrapping to 2 lines made every
/// arrow-key redraw stack a fresh, undeleted copy of the whole list
/// underneath the last one instead of overwriting it.
fn total_rows(options: &[String], width: usize) -> usize {
    const PREFIX_WIDTH: usize = 2; // "❯ " / "✓ " / "  "
    options
        .iter()
        .map(|name| rows_for_width(PREFIX_WIDTH + name.chars().count(), width))
        .sum()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_rows_is_one_per_option_when_everything_fits() {
        let options = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        assert_eq!(total_rows(&options, 40), 3);
    }

    #[test]
    fn total_rows_counts_wrapped_rows_for_a_long_option() {
        // Regression test: on-device, a long ask_question option label
        // wrapped to 2 terminal rows, but clear_lines was only ever told
        // to move up options.len() rows - one short per wrapped option -
        // so each arrow-key redraw left the previous render's overflow
        // behind instead of erasing it, stacking up duplicate copies of
        // the whole list on screen.
        let long_label = "x".repeat(50); // + 2-char prefix = 52 wide
        let options = vec!["short".to_string(), long_label];

        // "short" (2+5=7 wide) fits on one row; the 52-wide one wraps to
        // two rows at a 40-column terminal.
        assert_eq!(total_rows(&options, 40), 1 + 2);
    }

    #[test]
    fn total_rows_handles_an_empty_option_list() {
        assert_eq!(total_rows(&[], 40), 0);
    }
}
