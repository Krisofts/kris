//! Persists a project's conversation history to disk between runs, so
//! closing KRIS (or having it killed - a backgrounded Termux app reaped, a
//! crash) doesn't throw away a conversation that was in the middle of
//! something. One JSON file per project root under
//! `~/.config/kris/sessions/`, keyed by the root's own path so switching
//! between projects resumes each one's own last conversation instead of
//! all projects sharing a single history. Each file also carries its own
//! `root` so `list_sessions` (KRIS's counterpart to Claude Code's `/resume`
//! picker) can tell which project a session belongs to without having to
//! reverse the sanitized filename back into a path.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::message::Message;

#[derive(Serialize, Deserialize)]
struct PersistedSession {
    root: PathBuf,
    history: Vec<Message>,
}

/// One saved session as surfaced to the `resume` command's picker - just
/// enough to label a choice and switch to it, not the full history (which
/// only actually gets loaded once a choice is made).
pub struct SessionSummary {
    pub root: PathBuf,
    pub message_count: usize,
    pub modified: SystemTime,
}

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

fn sessions_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".config").join("kris").join("sessions"))
}

fn session_path(root: &Path) -> Result<PathBuf> {
    Ok(sessions_dir()?.join(session_filename(root)))
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
    serde_json::from_str::<PersistedSession>(&raw)
        .map(|p| p.history)
        .unwrap_or_default()
}

/// Persists `history` for `root`, overwriting whatever was saved before.
pub fn save(root: &Path, history: &[Message]) -> Result<()> {
    let path = session_path(root)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let envelope = PersistedSession {
        root: root.to_path_buf(),
        history: history.to_vec(),
    };
    let raw = serde_json::to_string(&envelope).context("serializing session history")?;
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

/// Every saved session with a non-empty history, most recently modified
/// first - what the `resume` command shows in its picker. Silently returns
/// an empty list if the sessions directory can't be read at all (a fresh
/// install with nothing saved yet), rather than treating that as an error.
pub fn list_sessions() -> Vec<SessionSummary> {
    let Ok(dir) = sessions_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut sessions: Vec<SessionSummary> = entries
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|entry| {
            let raw = fs::read_to_string(entry.path()).ok()?;
            let envelope: PersistedSession = serde_json::from_str(&raw).ok()?;
            if envelope.history.is_empty() {
                return None;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some(SessionSummary {
                root: envelope.root,
                message_count: envelope.history.len(),
                modified,
            })
        })
        .collect();

    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
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

    #[test]
    fn list_sessions_reports_every_saved_project_with_its_root_and_count() {
        with_scratch_home(|| {
            let a = PathBuf::from("/projects/a");
            let b = PathBuf::from("/projects/b");

            save(&a, &[Message::user("hi")]).unwrap();
            save(&b, &[Message::user("hi"), Message::assistant_text("hello")]).unwrap();

            let mut sessions = list_sessions();
            sessions.sort_by(|x, y| x.root.cmp(&y.root));

            assert_eq!(sessions.len(), 2);
            assert_eq!(sessions[0].root, a);
            assert_eq!(sessions[0].message_count, 1);
            assert_eq!(sessions[1].root, b);
            assert_eq!(sessions[1].message_count, 2);
        });
    }

    #[test]
    fn list_sessions_skips_sessions_with_empty_history() {
        with_scratch_home(|| {
            let empty = PathBuf::from("/projects/never-actually-used");
            save(&empty, &[]).unwrap();

            assert!(list_sessions().is_empty());
        });
    }

    #[test]
    fn list_sessions_orders_most_recently_modified_first() {
        with_scratch_home(|| {
            let older = PathBuf::from("/projects/older");
            let newer = PathBuf::from("/projects/newer");

            save(&older, &[Message::user("first")]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
            save(&newer, &[Message::user("second")]).unwrap();

            let sessions = list_sessions();
            assert_eq!(sessions[0].root, newer);
            assert_eq!(sessions[1].root, older);
        });
    }
}
