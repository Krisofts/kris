use std::fs;
use std::path::Path;

use crate::error::ToolError;

pub fn list_directory<P: AsRef<Path>>(path: P) -> Result<Vec<String>, ToolError> {
    let mut result = Vec::new();

    for entry in fs::read_dir(path)? {
        let entry = entry?;

        result.push(entry.file_name().to_string_lossy().to_string());
    }

    result.sort();

    Ok(result)
}
