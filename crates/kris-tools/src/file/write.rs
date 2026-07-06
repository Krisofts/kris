use std::fs;
use std::path::Path;

use crate::error::ToolError;

pub fn write_file<P: AsRef<Path>>(path: P, content: &str) -> Result<(), ToolError> {
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, content)?;

    Ok(())
}
