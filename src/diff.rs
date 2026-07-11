use similar::{ChangeTag, TextDiff};

use crate::style::{bold, dim, green, red};

/// Cap how many diff lines get printed for a single edit, so a full-file
/// rewrite doesn't flood a phone-sized terminal - the tool result already
/// tells the model exactly what changed, this is purely for the human.
const MAX_DIFF_LINES: usize = 60;

/// Renders a colorized diff between `old` and `new` content for a given
/// display path, with a per-line number gutter and a trailing
/// "N addition(s), M removal(s)" summary - Claude Code's diff view rather
/// than a raw `--- /+++` unified diff. Always called before a file
/// write/edit is applied, so the user sees every change KRIS makes, even
/// though it auto-applies.
pub fn render_unified_diff(path: &str, old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);

    let mut out = String::new();
    out.push_str(&bold(path));
    out.push('\n');

    let mut printed = 0usize;
    let mut truncated = false;
    let mut additions = 0usize;
    let mut removals = 0usize;

    'outer: for group in diff.grouped_ops(3) {
        for op in group {
            for change in diff.iter_changes(&op) {
                if printed >= MAX_DIFF_LINES {
                    truncated = true;
                    break 'outer;
                }

                let line = change.to_string_lossy();
                let line = line.strip_suffix('\n').unwrap_or(&line);

                let lineno = match change.tag() {
                    ChangeTag::Delete => change.old_index(),
                    _ => change.new_index(),
                }
                .map(|i| (i + 1).to_string())
                .unwrap_or_default();

                match change.tag() {
                    ChangeTag::Delete => {
                        removals += 1;
                        out.push_str(&red(&format!("{lineno:>5} -{line}\n")));
                    }
                    ChangeTag::Insert => {
                        additions += 1;
                        out.push_str(&green(&format!("{lineno:>5} +{line}\n")));
                    }
                    ChangeTag::Equal => out.push_str(&dim(&format!("{lineno:>5}  {line}\n"))),
                }

                printed += 1;
            }
        }
    }

    if truncated {
        out.push_str(&dim("... diff truncated ...\n"));
    }

    out.push_str(&dim(&format!(
        "{additions} addition(s), {removals} removal(s)\n"
    )));

    out
}

/// Total (additions, removals) between `old` and `new`, for a compact
/// `+A -B` stat in a tool's one-line result (e.g. "Wrote foo.rs (+16
/// -0)") - unlike `render_unified_diff`'s own trailing count, this isn't
/// capped by `MAX_DIFF_LINES`, so it stays accurate even past that many
/// changed lines.
pub fn diff_stat(old: &str, new: &str) -> (usize, usize) {
    let diff = TextDiff::from_lines(old, new);

    let mut additions = 0usize;
    let mut removals = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => removals += 1,
            ChangeTag::Equal => {}
        }
    }

    (additions, removals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_additions_and_removals() {
        let out = render_unified_diff("a.rs", "line1\nline2\n", "line1\nline2 changed\n");
        assert!(out.contains("-line2"));
        assert!(out.contains("+line2 changed"));
    }

    #[test]
    fn truncates_very_long_diffs() {
        let old: String = (0..200).map(|i| format!("old{i}\n")).collect();
        let new: String = (0..200).map(|i| format!("new{i}\n")).collect();

        let out = render_unified_diff("big.rs", &old, &new);
        assert!(out.contains("truncated"));
    }

    #[test]
    fn diff_stat_counts_additions_and_removals() {
        let (add, rem) = diff_stat("line1\nline2\n", "line1\nline2 changed\nline3\n");
        assert_eq!(add, 2);
        assert_eq!(rem, 1);
    }

    #[test]
    fn diff_stat_is_not_capped_by_max_diff_lines() {
        // render_unified_diff's own trailing count only reflects what it
        // actually printed (capped at MAX_DIFF_LINES) - diff_stat must
        // still see the true total past that cap.
        let old = String::new();
        let new: String = (0..200).map(|i| format!("line{i}\n")).collect();

        let (add, rem) = diff_stat(&old, &new);
        assert_eq!(add, 200);
        assert_eq!(rem, 0);
    }
}
