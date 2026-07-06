use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("File not found")]
    FileNotFound,

    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Unknown tool: {0}")]
    UnknownTool(String),

    #[error("Invalid or missing argument: {0}")]
    InvalidArgs(String),

    #[error("Tool error: {0}")]
    Tool(String),
}
