mod app;
mod banner;
mod command;
mod commands;
mod context;
mod registry;
mod repl;

use app::App;

fn main() {
    banner::print_banner();

    let mut app = App::new();

    repl::run(&mut app);
}
