use std::fs;
use std::path::Path;

use crate::error::ToolError;

pub fn read_file<P: AsRef<Path>>(path: P) -> Result<String, ToolError> {
    Ok(fs::read_to_string(path)?)
}
