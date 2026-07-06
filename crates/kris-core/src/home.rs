use std::path::PathBuf;

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Expands a leading `~/`, and resolves any other relative path against the
/// home directory (matching KRIS's own `$HOME/project`-style conventions),
/// rather than whatever the process's OS-level cwd happens to be. Absolute
/// paths are returned unchanged.
pub fn resolve_path(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }

    let path = PathBuf::from(input);

    if path.is_absolute() {
        return path;
    }

    match home_dir() {
        Some(home) => home.join(path),
        None => path,
    }
}
