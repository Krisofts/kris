use crate::context::Context;

pub trait Command {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn execute(&self, context: &mut Context, args: &[&str]);
}
