use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use crate::picker::{self, PickOutcome};
use crate::style::bold;

use super::{Tool, ToolError, AWAITING_CONFIRMATION};

/// Always appended as an extra choice, mirroring how a "select: Other"
/// path is offered automatically rather than the model having to spell
/// it out as one of its own options.
const OTHER_LABEL: &str = "Other (type your own answer)";

/// Lets the model pause an ambiguous request and ask the user to choose
/// between a few clear options instead of guessing - KRIS's equivalent of
/// Claude Code's AskUserQuestion, built on the same arrow-key `picker`
/// already used for the `project`/`workspace` commands.
pub struct AskQuestionTool;

impl Tool for AskQuestionTool {
    fn name(&self) -> &'static str {
        "ask_question"
    }

    fn description(&self) -> &'static str {
        "Ask the user to choose between 2-4 clear options when a request is ambiguous or \
         there's a real design decision to make, instead of guessing. Put the option you'd \
         pick yourself first and name it in recommended_index, if there's a clear best choice. \
         Don't add your own \"other\"/\"something else\" option - the user can always type a \
         custom answer instead of picking one of the listed options."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "The question to ask the user" },
                "options": {
                    "type": "array",
                    "description": "2 to 4 options for the user to choose from",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": { "type": "string", "description": "Short label for this choice" },
                            "description": { "type": "string", "description": "One-line explanation of this option's tradeoff" }
                        },
                        "required": ["label"]
                    }
                },
                "recommended_index": {
                    "type": "integer",
                    "description": "0-based index into options naming the recommended choice, if there is one"
                }
            },
            "required": ["question", "options"]
        })
    }

    fn execute(&self, _root: &Path, args: &Value) -> Result<String, ToolError> {
        let question = args
            .get("question")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("question".to_string()))?;

        let options = args
            .get("options")
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::InvalidArgs("options".to_string()))?;

        if options.is_empty() {
            return Err(ToolError::InvalidArgs(
                "options must not be empty".to_string(),
            ));
        }

        let recommended_index = args
            .get("recommended_index")
            .and_then(Value::as_u64)
            .map(|n| n as usize);

        let choices = build_choices(options, recommended_index);

        // Blocks on interactive input just like the y/N confirmation
        // prompts, so the spinner needs to stay quiet for the same reason.
        AWAITING_CONFIRMATION.store(true, Ordering::SeqCst);
        let answer = ask(question, &choices);
        AWAITING_CONFIRMATION.store(false, Ordering::SeqCst);

        Ok(answer)
    }
}

/// Builds (display label, clean label) pairs from the raw `options` JSON -
/// display decorates the label with "(Recommended)"/a description for the
/// picker, while the clean label is what's actually handed back to the
/// model as the answer, plus an always-appended "Other" escape hatch.
fn build_choices(options: &[Value], recommended_index: Option<usize>) -> Vec<(String, String)> {
    let mut choices: Vec<(String, String)> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let label = opt
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let description = opt.get("description").and_then(Value::as_str);

            let mut display = label.clone();
            if recommended_index == Some(i) {
                display.push_str(" (Recommended)");
            }
            if let Some(description) = description {
                display.push_str(&format!(" \u{2014} {description}"));
            }
            (display, label)
        })
        .collect();
    choices.push((OTHER_LABEL.to_string(), OTHER_LABEL.to_string()));
    choices
}

fn ask(question: &str, choices: &[(String, String)]) -> String {
    let display_labels: Vec<String> = choices.iter().map(|(display, _)| display.clone()).collect();

    match picker::pick(question, &display_labels, None) {
        PickOutcome::Chosen(display) => {
            let original = choices
                .iter()
                .find(|(d, _)| *d == display)
                .map(|(_, original)| original.clone())
                .unwrap_or(display);

            if original == OTHER_LABEL {
                read_custom_answer()
            } else {
                original
            }
        }
        PickOutcome::Cancelled => "(User cancelled without answering.)".to_string(),
        PickOutcome::Unavailable => ask_plain(question, choices),
    }
}

fn read_custom_answer() -> String {
    print!("Your answer: ");
    let _ = io::stdout().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return "(Could not read the user's answer.)".to_string();
    }

    let input = input.trim();
    if input.is_empty() {
        "(User gave no answer.)".to_string()
    } else {
        input.to_string()
    }
}

/// Numbered-list fallback for when the arrow-key picker isn't available
/// (no real TTY) - same choices, same "Other" escape hatch, just typed
/// instead of navigated.
fn ask_plain(question: &str, choices: &[(String, String)]) -> String {
    println!("{}", bold(question));
    for (i, (display, _)) in choices.iter().enumerate() {
        println!("  {}) {display}", i + 1);
    }
    print!("Choose a number, or type your own answer: ");
    let _ = io::stdout().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return "(Could not read the user's answer.)".to_string();
    }
    let input = input.trim();

    if let Ok(n) = input.parse::<usize>() {
        if n >= 1 && n <= choices.len() {
            let original = &choices[n - 1].1;
            return if original == OTHER_LABEL {
                read_custom_answer()
            } else {
                original.clone()
            };
        }
    }

    if input.is_empty() {
        "(User gave no answer.)".to_string()
    } else {
        input.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_choices_marks_the_recommended_option_and_appends_other() {
        let options = vec![
            json!({ "label": "Rewrite in Rust" }),
            json!({ "label": "Keep it in Python", "description": "less churn" }),
        ];

        let choices = build_choices(&options, Some(1));

        assert_eq!(choices.len(), 3);
        assert_eq!(
            choices[0],
            ("Rewrite in Rust".to_string(), "Rewrite in Rust".to_string())
        );
        assert_eq!(
            choices[1],
            (
                "Keep it in Python (Recommended) \u{2014} less churn".to_string(),
                "Keep it in Python".to_string()
            )
        );
        assert_eq!(choices[2].1, OTHER_LABEL);
    }

    #[test]
    fn build_choices_with_no_recommendation_decorates_nothing() {
        let options = vec![json!({ "label": "A" }), json!({ "label": "B" })];
        let choices = build_choices(&options, None);

        assert_eq!(choices[0], ("A".to_string(), "A".to_string()));
        assert_eq!(choices[1], ("B".to_string(), "B".to_string()));
    }

    #[test]
    fn ask_question_requires_question_and_options() {
        let tool = AskQuestionTool;

        let err = tool
            .execute(Path::new("."), &json!({ "options": [{"label": "A"}] }))
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));

        let err = tool
            .execute(Path::new("."), &json!({ "question": "Which?" }))
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn ask_question_rejects_empty_options() {
        let tool = AskQuestionTool;
        let err = tool
            .execute(
                Path::new("."),
                &json!({ "question": "Which?", "options": [] }),
            )
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
