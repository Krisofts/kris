//! The template KRIS seeds into every brand-new project's own `KRIS.md` -
//! house rules the model should follow there. Kept short and readable on
//! purpose: unlike the rest of the system prompt (reprocessed every turn
//! and deliberately kept minimal), a project's own `KRIS.md` only gets
//! folded in once, on the first turn of a session (`Agent::run`), so it's
//! fine for it to say more. Customize `~/.config/kris/KRIS.md` to change
//! what gets seeded into new projects from then on; falls back to
//! `DEFAULT_TEMPLATE` if that doesn't exist.

use std::fs;
use std::path::Path;

const DEFAULT_TEMPLATE: &str = "\
# KRIS.md

Project conventions KRIS should follow here.

## Code quality

- Keep the codebase clean, modular, and easy to maintain.
- Keep each file under 300 lines. If a file would grow past that, split
  the new functionality into its own, appropriately named file instead
  of letting it keep growing.

## Before finishing a task

- Always run the project's full test suite after making changes - not
  just after writing code that looks correct, and not just a quick,
  partial check.
- Always run the project's own linter and formatter, if it has one, and
  make sure both are clean (e.g. `cargo clippy` + `cargo fmt` for Rust,
  `eslint`/`prettier` for JS/TS, `ruff`/`black` for Python).
- Always do a smoke test - actually exercise the change - before
  considering a task done, not just relying on it compiling, passing
  tests, or passing lint.
";

/// The user's own customized `~/.config/kris/KRIS.md`, if they've edited
/// one - otherwise the built-in default above.
fn template() -> String {
    dirs::home_dir()
        .map(|home| home.join(".config").join("kris").join("KRIS.md"))
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_else(|| DEFAULT_TEMPLATE.to_string())
}

/// Seeds `root`'s own `KRIS.md` from the template if it looks like a
/// brand-new, empty project (no files at all yet) with no `KRIS.md`
/// already - an existing project is left untouched, since writing into
/// it uninvited would be surprising, and it might already have its own
/// project-specific `KRIS.md` from the `init` command. Failures (a
/// directory that can't be read, a file that can't be written) are
/// silently ignored - seeding the template is a nice-to-have, not
/// something that should ever block switching to a project.
pub fn seed_new_project(root: &Path) {
    let is_empty = fs::read_dir(root)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    if !is_empty {
        return;
    }

    let path = root.join("KRIS.md");
    if path.exists() {
        return;
    }

    let _ = fs::write(path, template());
}

/// Reads `root`'s own `KRIS.md`, if it has one - what `Agent::run` folds
/// into the system prompt so a project's conventions are actually
/// followed every turn, instead of sitting there as a file the model
/// only sees if it happens to `read_file` it.
pub fn read_project_conventions(root: &Path) -> Option<String> {
    fs::read_to_string(root.join("KRIS.md")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_the_template_into_a_brand_new_empty_project() {
        let dir = tempfile::tempdir().unwrap();
        seed_new_project(dir.path());

        let content = fs::read_to_string(dir.path().join("KRIS.md")).unwrap();
        assert!(content.contains("300 lines"));
        assert!(content.contains("full test suite"));
        assert!(content.contains("cargo clippy"));
        assert!(content.contains("cargo fmt"));
        assert!(content.contains("smoke test"));
    }

    #[test]
    fn does_not_touch_a_project_that_already_has_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

        seed_new_project(dir.path());

        assert!(!dir.path().join("KRIS.md").exists());
    }

    #[test]
    fn does_not_overwrite_an_existing_kris_md() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("KRIS.md"), "custom project notes").unwrap();

        seed_new_project(dir.path());

        let content = fs::read_to_string(dir.path().join("KRIS.md")).unwrap();
        assert_eq!(content, "custom project notes");
    }

    #[test]
    fn read_project_conventions_returns_none_when_there_is_no_kris_md() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_project_conventions(dir.path()).is_none());
    }

    #[test]
    fn read_project_conventions_returns_the_files_content() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("KRIS.md"), "follow these rules").unwrap();

        assert_eq!(
            read_project_conventions(dir.path()).as_deref(),
            Some("follow these rules")
        );
    }
}
