pub mod agent;
pub mod client;
pub mod config;
pub mod diff;
pub mod message;
pub mod picker;
pub mod repl;
pub mod server;
pub mod session_store;
pub mod style;
pub mod term;
pub mod tools;

/// `$HOME` is a single process-global value, so any test that points it at
/// a scratch directory (to sandbox config/session file I/O) needs to
/// exclude every *other* test doing the same thing, not just others in its
/// own module - two independent per-module locks don't stop one module's
/// test from repointing `$HOME` out from under another module's test
/// running concurrently on a different thread. One shared lock, used by
/// every `with_scratch_home` helper across the crate, closes that gap.
#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
