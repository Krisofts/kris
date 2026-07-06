use crate::style::{cyan, dim};

pub fn print_banner() {
    println!(
        "{}",
        cyan(
            r#"
╔══════════════════════════════╗
║          KRIS AI             ║
║   Local Coding Assistant     ║
╚══════════════════════════════╝"#
        )
    );
    println!();
    println!("Type \"help\" to begin.");
    println!(
        "{}",
        dim("Anything else runs as a shell command in the current workspace.")
    );
    println!();
}
