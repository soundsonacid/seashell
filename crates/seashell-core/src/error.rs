#[derive(Debug, thiserror::Error)]
pub enum SeashellError {
    #[error("{0}")]
    IoError(#[from] std::io::Error),

    #[error("{0}")]
    Custom(String),

    #[error("{0:?} is not an execution mode of solana-sbpf")]
    NotAnSbpfExecutionMode(crate::execution_mode::ExecutionMode),
}
