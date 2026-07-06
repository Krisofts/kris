use std::path::{Path, PathBuf};

pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();

    loop {
        if current.join("Cargo.toml").exists()
            || current.join("package.json").exists()
            || current.join("artisan").exists()
        {
            return Some(current);
        }

        if !current.pop() {
            break;
        }
    }

    None
}
