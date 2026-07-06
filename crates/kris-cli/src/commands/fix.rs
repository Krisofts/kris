use crate::{command::Command, context::Context};

const FIX_MIN_ITERATIONS: u32 = 24;

pub struct FixCommand;

impl Command for FixCommand {
    fn name(&self) -> &'static str {
        "fix"
    }

    fn description(&self) -> &'static str {
        "Build the project and iteratively fix errors until it compiles and tests pass"
    }

    fn execute(&self, context: &mut Context, args: &[&str]) {
        let extra = if args.is_empty() {
            String::new()
        } else {
            format!(" Additional context from the user: {}", args.join(" "))
        };

        let prompt = format!(
            "Build this project and fix any compile errors or failing tests, one at a \
             time, rebuilding after each fix, until the build succeeds cleanly (and tests \
             pass if there are any). Use run_command to build/test, and read_file / \
             search_code to investigate errors before editing.{extra}"
        );

        crate::agent_runner::run(context, &prompt, FIX_MIN_ITERATIONS);
    }
}
