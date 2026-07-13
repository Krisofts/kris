//! Persists a project's conversation history to disk between runs, so
//! closing KRIS (or having it killed - a backgrounded Termux app reaped, a
//! crash) doesn't throw away a conversation that was in the middle of
//! something. One JSON file per project root under
//! `~/.config/kris/sessions/`, keyed by the root's own path so switching
//! between projects resumes each one's own last conversation instead of
//! all projects sharing a single history.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::message::Message;

/// Filesystem-safe, human-recognizable-enough name for a project root's
/// session file: the path with anything not alphanumeric replaced by `_`,
/// plus a hash of the *original* path so two different paths that happen
/// to sanitize to the same string (e.g. one with a real `_` where the
/// other has a `/`) still land on different files.
fn session_filename(root: &Path) -> String {
    let raw = root.display().to_string();
    let sanitized: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    let mut hasher = DefaultHasher::new();
    raw.hash(&mut hasher);

    format!("{sanitized}-{:016x}.json", hasher.finish())
}

fn session_path(root: &Path) -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home
        .join(".config")
        .join("kris")
        .join("sessions")
        .join(session_filename(root)))
}

/// Loads the persisted history for `root`, or an empty history if none was
/// ever saved, the file is missing, or it fails to parse (a corrupt or
/// stale-format session file should never block startup - just start
/// fresh instead).
pub fn load(root: &Path) -> Vec<Message> {
    let Ok(path) = session_path(root) else {
        return Vec::new();
    };
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Persists `history` for `root`, overwriting whatever was saved before.
pub fn save(root: &Path, history: &[Message]) -> Result<()> {
    let path = session_path(root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string(history).context("serializing session history")?;
    fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Deletes the persisted session for `root`, if any - used by the `clear`
/// command so starting fresh actually starts fresh instead of the old
/// conversation coming back on the next restart.
pub fn clear(root: &Path) {
    if let Ok(path) = session_path(root) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Session paths live under $HOME, which is process-global - shares
    // `crate::test_support::HOME_ENV_LOCK` with every other module's own
    // `with_scratch_home` (repl.rs has one too) rather than a lock of its
    // own, since two independent per-module locks don't stop one module's
    // test from repointing $HOME out from under another module's test
    // running concurrently on a different thread.
    fn with_scratch_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_support::HOME_ENV_LOCK.lock().unwrap();
        let original_home = std::env::var("HOME").ok();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let result = f();

        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    #[test]
    fn loading_a_project_with_no_saved_session_returns_empty_history() {
        with_scratch_home(|| {
            let root = PathBuf::from("/some/project/never-saved");
            assert!(load(&root).is_empty());
        });
    }

    #[test]
    fn save_then_load_round_trips_the_history() {
        with_scratch_home(|| {
            let root = PathBuf::from("/some/project/tridjaya");
            let history = vec![Message::user("halo"), Message::assistant_text("halo juga!")];

            save(&root, &history).unwrap();
            let loaded = load(&root);

            assert_eq!(loaded.len(), 2);
            assert_eq!(loaded[0].content.as_deref(), Some("halo"));
            assert_eq!(loaded[1].content.as_deref(), Some("halo juga!"));
        });
    }

    #[test]
    fn different_project_roots_get_different_session_files() {
        with_scratch_home(|| {
            let a = PathBuf::from("/projects/a");
            let b = PathBuf::from("/projects/b");

            save(&a, &[Message::user("in project a")]).unwrap();
            save(&b, &[Message::user("in project b")]).unwrap();

            assert_eq!(load(&a)[0].content.as_deref(), Some("in project a"));
            assert_eq!(load(&b)[0].content.as_deref(), Some("in project b"));
        });
    }

    #[test]
    fn clear_removes_the_saved_session() {
        with_scratch_home(|| {
            let root = PathBuf::from("/some/project/tridjaya");
            save(&root, &[Message::user("hi")]).unwrap();
            assert_eq!(load(&root).len(), 1);

            clear(&root);

            assert!(load(&root).is_empty());
        });
    }

    #[test]
    fn clear_on_a_project_with_nothing_saved_does_not_error() {
        with_scratch_home(|| {
            let root = PathBuf::from("/some/project/never-saved");
            clear(&root); // must not panic
        });
    }

    #[test]
    fn a_corrupt_session_file_falls_back_to_empty_history_instead_of_failing() {
        with_scratch_home(|| {
            let root = PathBuf::from("/some/project/tridjaya");
            let path = session_path(&root).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "not valid json").unwrap();

            assert!(load(&root).is_empty());
        });
    }
}
