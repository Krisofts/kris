use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("File not found")]
    FileNotFound,

    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),
}
