use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{message}")]
    Generic { message: String },

    #[error("{0}")]
    GenericWindows(#[from] windows::core::Error),

    #[error("failed to mark projection root: {0}")]
    MarkProjectionRoot(windows::core::Error),

    #[error("failed to start projection: {0}")]
    StartProjection(windows::core::Error),
}
