use std::fmt;

/// A typesetting failure the caller can recover from.
#[derive(Debug)]
pub enum TypesetError {
    /// The font bytes were not a TrueType or CFF font.
    Font(String),
    /// A block id was not in the document.
    UnknownBlock(u64),
}

impl fmt::Display for TypesetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Font(message) => write!(f, "font: {message}"),
            Self::UnknownBlock(id) => write!(f, "unknown block {id}"),
        }
    }
}

impl std::error::Error for TypesetError {}
