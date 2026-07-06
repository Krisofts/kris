mod app;
mod banner;
mod command;
mod commands;
mod context;
mod registry;
mod repl;

use std::path::PathBuf;

use app::App;

fn main() {
    banner::print_banner();

    let mut app = match std::env::args().nth(1) {
        Some(path) => {
            let app = App::with_path(&PathBuf::from(path.clone()));

            if app.context.workspace.is_none() {
                println!("Warning: could not open project folder \"{path}\".");
            }

            app
        }
        None => App::new(),
    };

    repl::run(&mut app);
}
