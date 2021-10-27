#[derive(Debug)]
pub enum Error {
    SessionNotFound(String),
    RustError(Box<dyn std::error::Error + Send>),
}

impl<T: 'static> From<tokio::sync::broadcast::error::SendError<T>> for Error
where
    T: std::fmt::Debug + Send,
{
    fn from(err: tokio::sync::broadcast::error::SendError<T>) -> Self {
        Self::RustError(Box::new(err))
    }
}

impl<T: 'static> From<tokio::sync::mpsc::error::SendError<T>> for Error
where
    T: std::fmt::Debug + Send,
{
    fn from(err: tokio::sync::mpsc::error::SendError<T>) -> Self {
        Self::RustError(Box::new(err))
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::RustError(Box::new(err))
    }
}

impl From<Box<bincode::ErrorKind>> for Error {
    fn from(err: Box<bincode::ErrorKind>) -> Self {
        Self::RustError(err)
    }
}
