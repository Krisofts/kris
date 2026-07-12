//! Minimal ANSI helpers - deliberately not a crate dependency (crossterm
//! etc.) to keep the dependency tree, and Termux build time, small.

fn wrap(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

pub fn dim(text: &str) -> String {
    wrap("2", text)
}

pub fn bold(text: &str) -> String {
    wrap("1", text)
}

pub fn red(text: &str) -> String {
    wrap("31", text)
}

pub fn green(text: &str) -> String {
    wrap("32", text)
}

pub fn yellow(text: &str) -> String {
    wrap("33", text)
}

pub fn cyan(text: &str) -> String {
    wrap("36", text)
}

pub fn blue(text: &str) -> String {
    wrap("34", text)
}

pub fn light_blue(text: &str) -> String {
    wrap("94", text)
}
