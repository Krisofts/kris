use anyhow::Result;

use kris::config::Settings;
use kris::repl;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let settings = Settings::load()?;

    if args.is_empty() {
        repl::run_interactive(settings).await
    } else {
        repl::run_once(settings, &args.join(" ")).await
    }
}
