use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Metadata error: {0}")]
    Metadata(String),

    #[error("Device error: {0}")]
    Device(String),

    #[error("Transfer error: {0}")]
    Transfer(String),

    #[error("No devices connected")]
    NoDevices,

    #[error("Transfer cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, AppError>;
