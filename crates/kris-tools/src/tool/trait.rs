pub trait Tool {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;
}
