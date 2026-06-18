use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),

    #[error("policy denied: {0}")]
    PolicyDenied(String),

    #[error("invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("execution error: {0}")]
    ExecError(String),
}

impl ToolError {
    /// Short machine-readable kind, used when serializing the error as a tool
    /// result back to the model (design §5: error-as-tool-result).
    pub fn kind(&self) -> &'static str {
        match self {
            ToolError::NotFound(_) => "NotFound",
            ToolError::PolicyDenied(_) => "PolicyDenied",
            ToolError::InvalidArgs(_) => "InvalidArgs",
            ToolError::ExecError(_) => "ExecError",
        }
    }
}
