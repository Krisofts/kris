use std::io::{self, Write};

use crate::app::App;

pub fn run(app: &mut App) {
    loop {
        print!("kris > ");
        io::stdout().flush().unwrap();

        let mut input = String::new();

        io::stdin().read_line(&mut input).unwrap();

        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        if !app.registry.execute(&mut app.context, input) {
            break;
        }
    }
}
