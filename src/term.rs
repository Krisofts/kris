//! Small terminal-size helpers shared by anything that redraws a line (or
//! a block of lines) in place - the REPL's spinner and the arrow-key
//! `picker` both need to know exactly how much screen space their last
//! render used, or the cursor-repositioning escape codes they use to
//! overwrite it undercount and leave stale content behind.

/// Current terminal width in columns, falling back to a conservative
/// default (safe even on a fairly narrow phone terminal) if it can't be
/// queried - e.g. output isn't a real tty.
pub fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(cols, _)| cols as usize)
        .unwrap_or(40)
}

/// Truncates `text` to at most `max_width` characters, appending an
/// ellipsis in place of the last character when it doesn't fit - used to
/// keep a single redrawn line from ever wrapping to a second terminal
/// row, which would break a `\r`-based in-place redraw.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.chars().count() <= max_width {
        return text.to_string();
    }

    let mut truncated: String = text.chars().take(max_width.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

/// How many terminal rows a line of `content_width` visible characters
/// wraps to at the given terminal width - 1 if it fits on one row, more
/// if the terminal itself wraps it. Used where a full line can't just be
/// truncated (the picker's option list, which needs every option to stay
/// intact) but the exact row count still has to be known to correctly
/// move the cursor back up over a previous multi-line render.
pub fn rows_for_width(content_width: usize, terminal_width: usize) -> usize {
    if terminal_width == 0 {
        return 1;
    }
    content_width.max(1).div_ceil(terminal_width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_to_width_leaves_short_text_untouched() {
        assert_eq!(truncate_to_width("thinking... 4s", 40), "thinking... 4s");
    }

    #[test]
    fn truncate_to_width_caps_long_text_with_an_ellipsis() {
        let long = "thinking... 144s (unusually long - ...) · Read 21 files · Edited 2 files";
        let truncated = truncate_to_width(long, 20);

        assert_eq!(truncated.chars().count(), 20);
        assert!(truncated.ends_with('…'));
        assert!(long.starts_with(&truncated[..truncated.len() - '…'.len_utf8()]));
    }

    #[test]
    fn truncate_to_width_handles_zero_width() {
        assert_eq!(truncate_to_width("anything", 0), "");
    }

    #[test]
    fn rows_for_width_fits_on_one_row() {
        assert_eq!(rows_for_width(10, 40), 1);
        assert_eq!(rows_for_width(40, 40), 1);
    }

    #[test]
    fn rows_for_width_rounds_up_when_it_wraps() {
        assert_eq!(rows_for_width(41, 40), 2);
        assert_eq!(rows_for_width(80, 40), 2);
        assert_eq!(rows_for_width(81, 40), 3);
    }

    #[test]
    fn rows_for_width_treats_empty_content_as_one_row() {
        assert_eq!(rows_for_width(0, 40), 1);
    }
}
