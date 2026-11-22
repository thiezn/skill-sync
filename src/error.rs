use std::error::Error as StdError;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    message: String,
    #[source]
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    pub(crate) fn with_source<E>(message: impl Into<String>, source: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::with_source("I/O operation failed", source)
    }
}

impl From<git2::Error> for Error {
    fn from(source: git2::Error) -> Self {
        Self::with_source("Git operation failed", source)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
