use std::path::Path;

use serde_json::Value;

use crate::error::ToolError;

pub trait Tool {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn parameters_schema(&self) -> Value;

    fn execute(&self, root: &Path, args: &Value) -> Result<String, ToolError>;
}
