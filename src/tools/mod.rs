mod ask;
mod edit;
mod fs;
mod git;
mod outline;
mod run_command;

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use thiserror::Error;

/// Set to true immediately before any tool blocks on a stdin y/N
/// confirmation prompt, and back to false right after. The REPL's
/// "thinking..." spinner runs on its own tokio task and checks this so it
/// stops overwriting (and hiding) the prompt while KRIS is genuinely
/// waiting on the user rather than the model - without this, a
/// confirmation box can get silently clobbered by the next spinner frame
/// a moment after it's printed, making KRIS look stuck in an infinite
/// "thinking..." loop when it's actually just waiting for a y/N that was
/// never seen.
pub static AWAITING_CONFIRMATION: AtomicBool = AtomicBool::new(false);

/// Set to true while `run_command`'s subprocess is actually executing
/// (after the user has already confirmed it, if confirmation was needed) -
/// distinct from `AWAITING_CONFIRMATION`, which only covers the y/N prompt
/// itself. `run_command.execute()` runs synchronously on whatever thread is
/// polling the agent's future, so on a multi-threaded runtime the REPL's
/// "thinking..." spinner (its own tokio task, on another worker thread)
/// would otherwise keep ticking the whole time too - two independent
/// `\r`-redrawing loops racing over the same terminal line, garbling both.
pub static COMMAND_RUNNING: AtomicBool = AtomicBool::new(false);

/// Truncates `s` to at most `max_len` bytes, backing off to the nearest
/// earlier UTF-8 char boundary rather than cutting mid-character. Plain
/// `String::truncate(max_len)` panics whenever `max_len` doesn't land on a
/// char boundary - command/git output routinely contains multi-byte UTF-8
/// (unicode arrows in diffs, emoji, non-ASCII text) that can straddle
/// exactly that byte offset, which crashed the whole process the moment it
/// did.
pub(crate) fn truncate_to_byte_limit(s: &mut String, max_len: usize) {
    let mut cut = max_len.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Unknown tool: {0}")]
    UnknownTool(String),
    #[error("Invalid or missing argument: {0}")]
    InvalidArgs(String),
    #[error("{0}")]
    Tool(String),
}

pub trait Tool {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> Value;
    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError>;
}

/// Which of Claude Code's three broad action groups a tool call belongs
/// to, for the "Read N files · Edited N files · Ran N commands" recap
/// (both the REPL's own, and `run_command`'s live status line - see
/// `SharedTurnCounts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Read,
    Edit,
    Command,
    Other,
}

pub fn tool_category(name: &str) -> ToolCategory {
    match name {
        "read_file" | "list_directory" | "tree" | "find_files" | "search_code" | "outline_file"
        | "git" => ToolCategory::Read,
        "write_file" | "append_file" | "edit_file" | "delete_file" | "delete_directory"
        | "move_file" | "create_directory" => ToolCategory::Edit,
        "run_command" | "git_commit" => ToolCategory::Command,
        _ => ToolCategory::Other,
    }
}

#[derive(Default)]
pub struct TurnCounts {
    pub read: usize,
    pub edited: usize,
    pub commands: usize,
}

impl TurnCounts {
    pub fn summary(&self) -> Option<String> {
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });

        let parts: Vec<String> = [
            (self.read, "Read", "file"),
            (self.edited, "Edited", "file"),
            (self.commands, "Ran", "command"),
        ]
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, verb, word)| format!("{verb} {}", plural(n, word)))
        .collect();

        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// Thread-safe tally shared between the REPL's closure recording tool
/// calls (on the main task), its spinner (its own tokio task) that
/// displays a running "Read N files · Ran N commands" recap live while a
/// turn is in progress, and `run_command`'s own synchronous status line -
/// so a slow command in progress shows the same live recap instead of
/// going blank for however long it takes to finish.
#[derive(Default)]
pub struct SharedTurnCounts {
    read: AtomicUsize,
    edited: AtomicUsize,
    commands: AtomicUsize,
}

impl SharedTurnCounts {
    pub fn record(&self, tool_name: &str) {
        let counter = match tool_category(tool_name) {
            ToolCategory::Read => &self.read,
            ToolCategory::Edit => &self.edited,
            ToolCategory::Command => &self.commands,
            ToolCategory::Other => return,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> TurnCounts {
        TurnCounts {
            read: self.read.load(Ordering::Relaxed),
            edited: self.edited.load(Ordering::Relaxed),
            commands: self.commands.load(Ordering::Relaxed),
        }
    }
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    turn_counts: Arc<SharedTurnCounts>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_defaults(false, false)
    }
}

impl ToolRegistry {
    /// `bypass_permissions` seeds every tool's confirmation gate as
    /// already-approved (as if "always" had been answered up front),
    /// for `config set bypass_permissions true` / the `--yes`-style
    /// unattended use case - off means every gate starts fresh and asks.
    /// `auto_approve_edits` is the narrower `config set auto_approve_edits
    /// true`: it only seeds the file-editing tools' shared gate, leaving
    /// run_command and git_commit still asking - has no extra effect once
    /// `bypass_permissions` is already true.
    pub fn with_defaults(bypass_permissions: bool, auto_approve_edits: bool) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
            turn_counts: Arc::new(SharedTurnCounts::default()),
        };

        // Shared so approving one filesystem change with "always" covers
        // writes, edits, deletes, and moves for the rest of the session,
        // rather than needing a separate "always" per tool.
        let auto_approve = Rc::new(Cell::new(bypass_permissions || auto_approve_edits));

        registry.register(fs::ReadFileTool);
        registry.register(fs::ListDirectoryTool);
        registry.register(fs::TreeTool);
        registry.register(fs::FindFilesTool);
        registry.register(fs::SearchCodeTool);
        registry.register(edit::WriteFileTool::new(auto_approve.clone()));
        registry.register(edit::AppendFileTool::new(auto_approve.clone()));
        registry.register(edit::EditFileTool::new(auto_approve.clone()));
        registry.register(edit::DeleteFileTool::new(auto_approve.clone()));
        registry.register(edit::DeleteDirectoryTool::new(auto_approve.clone()));
        registry.register(edit::MoveFileTool::new(auto_approve.clone()));
        registry.register(edit::CreateDirectoryTool::new(auto_approve));
        registry.register(run_command::RunCommandTool::new(
            bypass_permissions,
            registry.turn_counts.clone(),
        ));
        registry.register(git::GitTool);
        registry.register(git::GitCommitTool::new(bypass_permissions));
        registry.register(outline::OutlineFileTool);
        registry.register(ask::AskQuestionTool);

        registry
    }

    /// The same shared per-turn tally `run_command`'s own live status line
    /// reads from - exposed so a caller (the REPL) can record completed
    /// tool calls into it and read it back for its own spinner/recap,
    /// without constructing a second, disconnected instance that
    /// `run_command` would never see.
    pub fn turn_counts(&self) -> Arc<SharedTurnCounts> {
        self.turn_counts.clone()
    }

    fn register<T>(&mut self, tool: T)
    where
        T: Tool + 'static,
    {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
    }

    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// OpenAI `tools` array shape (`{"type":"function","function":{...}}`),
    /// the base every OpenAI-compatible provider expects so the model gets
    /// structured tool calls instead of having to be told the schema in
    /// the system prompt as prose. `describe_all_gemini` sanitizes this
    /// further for providers with a stricter schema validator.
    pub fn describe_all(&self) -> Vec<Value> {
        let mut names = self.names();
        names.sort_unstable();

        names
            .into_iter()
            .map(|name| {
                let tool = &self.tools[name];
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name(),
                        "description": tool.description(),
                        "parameters": tool.parameters_schema(),
                    }
                })
            })
            .collect()
    }

    /// Same tool list as `describe_all`, but with each parameter schema
    /// sanitized down to the subset a remote OpenAI-compatible provider
    /// (Gemini, OpenRouter, Opper, OpenCode Zen) accepts. Several validate
    /// the schema strictly and reject the whole request on keywords they
    /// don't know (`additionalProperties`, `$schema`, `default`, …), so
    /// those are stripped here. KRIS's built-in tools already use a clean
    /// subset, so this is mostly a guard for any future tool - but a
    /// single unknown keyword would otherwise 400 the entire turn.
    pub fn describe_all_gemini(&self) -> Vec<Value> {
        let mut schemas = self.describe_all();
        for entry in &mut schemas {
            if let Some(parameters) = entry.pointer_mut("/function/parameters") {
                sanitize_remote_schema(parameters);
            }
        }
        schemas
    }

    /// Claude's native Messages API tools shape: flat `{"name",
    /// "description", "input_schema"}` objects (no `{"type":"function",
    /// "function": {...}}` wrapper the OpenAI-compatible backends use).
    /// Schemas are sanitized the same way as Gemini's, since Claude's
    /// schema validator is similarly strict about unrecognized keywords.
    pub fn describe_all_anthropic(&self) -> Vec<Value> {
        let mut names = self.names();
        names.sort_unstable();

        names
            .into_iter()
            .map(|name| {
                let tool = &self.tools[name];
                let mut parameters = tool.parameters_schema();
                sanitize_remote_schema(&mut parameters);
                json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "input_schema": parameters,
                })
            })
            .collect()
    }

    pub fn execute(&self, name: &str, root: &Path, args: &Value) -> Result<String, ToolError> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(root, args),
            None => Err(ToolError::UnknownTool(name.to_string())),
        }
    }
}

/// Recursively strips JSON Schema keywords that remote providers' (Gemini,
/// Claude) function-calling schema validators reject, so a schema written
/// against the fuller JSON Schema subset still passes there. Only removes
/// keys; the structural `type`/`properties`/`required`/`items`/`enum`/
/// `description` that both providers do support are left untouched.
fn sanitize_remote_schema(value: &mut Value) {
    const UNSUPPORTED: &[&str] = &[
        "additionalProperties",
        "$schema",
        "$id",
        "$ref",
        "definitions",
        "title",
        "default",
        "examples",
        "const",
    ];

    match value {
        Value::Object(map) => {
            for key in UNSUPPORTED {
                map.remove(*key);
            }
            for child in map.values_mut() {
                sanitize_remote_schema(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_remote_schema(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_category_classifies_every_built_in_tool() {
        assert_eq!(tool_category("read_file"), ToolCategory::Read);
        assert_eq!(tool_category("search_code"), ToolCategory::Read);
        assert_eq!(tool_category("git"), ToolCategory::Read);
        assert_eq!(tool_category("write_file"), ToolCategory::Edit);
        assert_eq!(tool_category("append_file"), ToolCategory::Edit);
        assert_eq!(tool_category("run_command"), ToolCategory::Command);
        assert_eq!(tool_category("git_commit"), ToolCategory::Command);
        assert_eq!(tool_category("something_unknown"), ToolCategory::Other);
    }

    #[test]
    fn turn_counts_summary_omits_zero_categories_and_pluralizes() {
        let counts = SharedTurnCounts::default();
        assert_eq!(counts.snapshot().summary(), None);

        counts.record("read_file");
        assert_eq!(counts.snapshot().summary().as_deref(), Some("Read 1 file"));

        counts.record("edit_file");
        assert_eq!(
            counts.snapshot().summary().as_deref(),
            Some("Read 1 file · Edited 1 file")
        );

        counts.record("run_command");
        counts.record("run_command");
        assert_eq!(
            counts.snapshot().summary().as_deref(),
            Some("Read 1 file · Edited 1 file · Ran 2 commands")
        );
    }

    #[test]
    fn turn_counts_ignores_uncategorized_tools() {
        let counts = SharedTurnCounts::default();
        counts.record("outline_file");
        counts.record("something_unknown");
        assert_eq!(counts.snapshot().summary().as_deref(), Some("Read 1 file"));
    }

    #[test]
    fn auto_approve_edits_seeds_edit_tools_without_reading_stdin() {
        // The confirm() gate checks auto_approve before ever touching stdin,
        // so if this is seeded correctly write_file succeeds immediately
        // instead of hanging on (or failing to read) a confirmation prompt.
        let dir = tempfile::tempdir().unwrap();
        let registry = ToolRegistry::with_defaults(false, true);

        let result = registry
            .execute(
                "write_file",
                dir.path(),
                &json!({ "path": "f.txt", "content": "hi" }),
            )
            .unwrap();

        assert!(result.starts_with("Wrote"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn describe_all_uses_openai_function_shape() {
        let registry = ToolRegistry::with_defaults(false, false);
        let described = registry.describe_all();

        assert!(!described.is_empty());
        for entry in &described {
            assert_eq!(entry["type"], "function");
            assert!(entry["function"]["name"].is_string());
            assert!(entry["function"]["parameters"].is_object());
        }
    }

    #[test]
    fn gemini_schemas_keep_openai_shape_and_stay_clean() {
        let registry = ToolRegistry::with_defaults(false, false);
        let described = registry.describe_all_gemini();

        assert!(!described.is_empty());
        for entry in &described {
            assert_eq!(entry["type"], "function");
            assert!(entry["function"]["name"].is_string());
            let params = &entry["function"]["parameters"];
            assert_eq!(params["type"], "object");
            // Nothing Gemini would reject should survive anywhere in the schema.
            assert!(!schema_contains_key(params, "additionalProperties"));
            assert!(!schema_contains_key(params, "default"));
            assert!(!schema_contains_key(params, "$schema"));
        }
    }

    #[test]
    fn anthropic_schemas_use_flat_shape_and_stay_clean() {
        let registry = ToolRegistry::with_defaults(false, false);
        let described = registry.describe_all_anthropic();

        assert!(!described.is_empty());
        for entry in &described {
            assert!(entry["name"].is_string());
            assert!(entry["description"].is_string());
            let params = &entry["input_schema"];
            assert_eq!(params["type"], "object");
            assert!(entry.get("type").is_none());
            assert!(!schema_contains_key(params, "additionalProperties"));
            assert!(!schema_contains_key(params, "default"));
            assert!(!schema_contains_key(params, "$schema"));
        }
    }

    #[test]
    fn sanitizer_strips_unsupported_keywords_recursively() {
        let mut schema = json!({
            "type": "object",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "additionalProperties": false,
            "properties": {
                "path": { "type": "string", "default": "x", "title": "Path" },
                "opts": {
                    "type": "object",
                    "additionalProperties": true,
                    "properties": {
                        "n": { "type": "integer", "const": 3 }
                    }
                }
            },
            "required": ["path"]
        });

        sanitize_remote_schema(&mut schema);

        // Supported structure is preserved...
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["required"][0], "path");
        // ...unsupported keywords are gone at every depth.
        assert!(!schema_contains_key(&schema, "additionalProperties"));
        assert!(!schema_contains_key(&schema, "$schema"));
        assert!(!schema_contains_key(&schema, "default"));
        assert!(!schema_contains_key(&schema, "title"));
        assert!(!schema_contains_key(&schema, "const"));
    }

    /// True if `key` appears anywhere in the (possibly nested) schema.
    fn schema_contains_key(value: &Value, key: &str) -> bool {
        match value {
            Value::Object(map) => {
                map.contains_key(key) || map.values().any(|v| schema_contains_key(v, key))
            }
            Value::Array(items) => items.iter().any(|v| schema_contains_key(v, key)),
            _ => false,
        }
    }

    #[test]
    fn execute_reports_unknown_tool() {
        let registry = ToolRegistry::with_defaults(false, false);
        let err = registry
            .execute("does_not_exist", Path::new("."), &json!({}))
            .unwrap_err();

        assert!(matches!(err, ToolError::UnknownTool(_)));
    }

    #[test]
    fn truncate_to_byte_limit_backs_off_from_a_split_multibyte_char() {
        // Regression test: plain `String::truncate(n)` panics outright if
        // `n` doesn't land on a UTF-8 char boundary - command/git output
        // routinely has a multi-byte character (accented text, emoji,
        // unicode arrows in a diff) straddling exactly the byte offset
        // run_command.rs/git.rs cut at, which used to crash the whole
        // process instead of just truncating the output.
        let mut s = "a".repeat(9);
        s.push('é'); // 2-byte UTF-8 character; s.len() is now 11
        s.push_str(&"b".repeat(5));

        truncate_to_byte_limit(&mut s, 10);

        assert_eq!(s, "a".repeat(9));
        assert!(s.is_char_boundary(s.len()));
    }

    #[test]
    fn truncate_to_byte_limit_is_a_no_op_when_already_under_the_limit() {
        let mut s = "short".to_string();
        truncate_to_byte_limit(&mut s, 100);
        assert_eq!(s, "short");
    }
}
