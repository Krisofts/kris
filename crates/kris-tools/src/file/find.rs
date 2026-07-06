use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

pub fn find<P: AsRef<Path>>(
    root: P,
    keyword: &str,
) -> Result<Vec<String>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();

        if let Some(name) = path.file_name() {
            let name = name.to_string_lossy();

            if name.contains(keyword) {
                files.push(path.display().to_string());
            }
        }
    }

    files.sort();

    Ok(files)
}