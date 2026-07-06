use std::fs;
use std::path::Path;

use crate::error::ToolError;

pub fn write_file<P: AsRef<Path>>(path: P, content: &str) -> Result<(), ToolError> {
    fs::write(path, content)?;

    Ok(())
}
