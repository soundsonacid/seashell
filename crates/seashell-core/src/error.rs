#[derive(Debug, thiserror::Error)]
pub enum SeashellError {
    #[error("{0}")]
    IoError(#[from] std::io::Error),

    #[error("{0}")]
    Custom(String),
}
