mod app;
mod banner;
mod command;
mod commands;
mod context;
mod registry;
mod repl;

use std::path::PathBuf;

use app::App;
use kris_core::home::home_dir;

fn main() {
    banner::print_banner();

    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join("project")));

    let mut app = match path {
        Some(path) => {
            let app = App::with_path(&path);

            if app.context.workspace.is_none() {
                println!(
                    "Warning: could not open project folder \"{}\".",
                    path.display()
                );
                println!("Create it, or point KRIS elsewhere: kris-cli <path>");
            }

            app
        }
        None => App::new(),
    };

    repl::run(&mut app);
}
