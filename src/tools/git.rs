use std::path::Path;

use serde_json::{json, Value};

use super::{Tool, ToolError};

const MAX_OUTPUT: usize = 4000;
const ALLOWED: [&str; 5] = ["status", "diff", "log", "show", "branch"];

pub struct GitTool;

impl Tool for GitTool {
    fn name(&self) -> &'static str {
        "git"
    }

    fn description(&self) -> &'static str {
        "Run a read-only git inspection command (status, diff, log, show, or branch) \
         inside the project. Never modifies anything, so unlike run_command this needs \
         no confirmation - for commits, pushes, resets, etc. use run_command instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subcommand": { "type": "string", "description": "One of: status, diff, log, show, branch" },
                "args": { "type": "string", "description": "Extra arguments, e.g. a file path for diff, or a commit hash for show" }
            },
            "required": ["subcommand"]
        })
    }

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError> {
        let subcommand = args
            .get("subcommand")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("subcommand".to_string()))?;

        if !ALLOWED.contains(&subcommand) {
            return Err(ToolError::InvalidArgs(format!(
                "subcommand must be one of: {}",
                ALLOWED.join(", ")
            )));
        }

        let extra = args.get("args").and_then(Value::as_str).unwrap_or("");

        let mut command = std::process::Command::new("git");
        command.arg(subcommand).current_dir(root);

        match subcommand {
            "log" => {
                command.args(["--oneline", "-n", "20"]);
            }
            "diff" => {
                command.arg("--no-color");
            }
            "status" => {
                command.arg("--short");
            }
            _ => {}
        }

        for token in extra.split_whitespace() {
            command.arg(token);
        }

        let output = command
            .output()
            .map_err(|err| ToolError::Tool(format!("failed to run git: {err}")))?;

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));

        if combined.len() > MAX_OUTPUT {
            combined.truncate(MAX_OUTPUT);
            combined.push_str("\n...(output truncated)");
        }

        if combined.trim().is_empty() {
            combined = "(no output)".to_string();
        }

        Ok(combined)
    }
}
