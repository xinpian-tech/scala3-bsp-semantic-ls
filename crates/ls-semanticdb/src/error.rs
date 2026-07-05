//! The crate's typed error surface for parse and I/O failures.

use std::fmt;

/// A failure while decoding or reading a `.semanticdb` payload. Decoding never
/// panics — a malformed payload always surfaces as [`SemanticdbError::Parse`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SemanticdbError {
    /// The payload is not decodable protobuf wire data (mirrors the Scala
    /// `SemanticdbParseException`).
    Parse(String),
    /// A `.semanticdb` file could not be read from disk.
    Io(String),
}

impl fmt::Display for SemanticdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SemanticdbError::Parse(m) => write!(f, "semanticdb parse error: {m}"),
            SemanticdbError::Io(m) => write!(f, "semanticdb io error: {m}"),
        }
    }
}

impl std::error::Error for SemanticdbError {}

/// Convenience alias for fallible decode operations.
pub type SemanticdbResult<T> = Result<T, SemanticdbError>;
