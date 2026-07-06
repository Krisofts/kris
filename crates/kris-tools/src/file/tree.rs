use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

pub fn tree<P: AsRef<Path>>(path: P) -> Result<Vec<String>> {
    let root = path.as_ref();

    let mut lines = Vec::new();

    lines.push(".".to_string());

    for entry in WalkDir::new(root).min_depth(1).sort_by_file_name() {
        let entry = entry?;

        let depth = entry.depth();

        let indent = "│   ".repeat(depth.saturating_sub(1));

        let name = entry.file_name().to_string_lossy();

        lines.push(format!("{indent}├── {name}"));
    }

    Ok(lines)
}
