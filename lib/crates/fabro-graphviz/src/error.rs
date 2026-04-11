use thiserror::Error as ThisError;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Stylesheet error: {0}")]
    Stylesheet(String),
}

pub type Result<T> = std::result::Result<T, Error>;
