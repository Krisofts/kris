use std::io::IsTerminal;
use std::sync::OnceLock;

fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::io::stdout().is_terminal())
}

fn wrap(text: &str, code: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn bold(text: &str) -> String {
    wrap(text, "1")
}

pub fn dim(text: &str) -> String {
    wrap(text, "2")
}

pub fn cyan(text: &str) -> String {
    wrap(text, "36")
}

pub fn green(text: &str) -> String {
    wrap(text, "32")
}

pub fn red(text: &str) -> String {
    wrap(text, "31")
}

pub fn yellow(text: &str) -> String {
    wrap(text, "33")
}
