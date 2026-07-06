use std::path::Path;

use crate::{error::ToolError, file::read::read_file};

pub fn cat<P: AsRef<Path>>(path: P) -> Result<String, ToolError> {
    read_file(path)
}
