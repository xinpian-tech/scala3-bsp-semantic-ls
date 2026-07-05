//! Typed segment errors. A reader raises `SegmentError` on the first validation
//! failure and never partially serves a segment; the writer raises it on
//! invalid input.

/// A segment read/write failure.
#[derive(Debug)]
pub enum SegmentError {
    /// Filesystem I/O failure.
    Io(std::io::Error),
    /// `header.bin` magic did not match.
    BadMagic { found: u32 },
    /// `header.bin` version did not match.
    BadVersion { found: u16 },
    /// The header self-checksum did not match its bytes.
    HeaderChecksumMismatch,
    /// A checksummed file's CRC did not match its bytes.
    ChecksumMismatch { file: String },
    /// `checksums.bin` did not list exactly the expected files, in order.
    ChecksumListMismatch { detail: String },
    /// A file was shorter than its declared structure requires.
    Truncated { file: String },
    /// A structural cross-check failed (sizes/counts disagree).
    Structural { detail: String },
    /// Writer input was invalid (e.g. a duplicate symbol string).
    InvalidInput { detail: String },
}

impl std::fmt::Display for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SegmentError::Io(e) => write!(f, "segment io error: {e}"),
            SegmentError::BadMagic { found } => {
                write!(f, "bad segment magic: 0x{found:08x}")
            }
            SegmentError::BadVersion { found } => write!(f, "unsupported segment version: {found}"),
            SegmentError::HeaderChecksumMismatch => write!(f, "header checksum mismatch"),
            SegmentError::ChecksumMismatch { file } => write!(f, "checksum mismatch in {file}"),
            SegmentError::ChecksumListMismatch { detail } => {
                write!(f, "checksums.bin list mismatch: {detail}")
            }
            SegmentError::Truncated { file } => write!(f, "truncated/corrupt file: {file}"),
            SegmentError::Structural { detail } => write!(f, "structural mismatch: {detail}"),
            SegmentError::InvalidInput { detail } => write!(f, "invalid segment input: {detail}"),
        }
    }
}

impl std::error::Error for SegmentError {}

impl From<std::io::Error> for SegmentError {
    fn from(e: std::io::Error) -> Self {
        SegmentError::Io(e)
    }
}

/// Convenience alias for segment results.
pub type Result<T> = std::result::Result<T, SegmentError>;

/// A store-level failure. Wraps [`SegmentError`] and adds the manifest /
/// workspace-state / pairing failures the snapshot layer can raise. Never
/// panics on corrupt on-disk state.
#[derive(Debug)]
pub enum StoreError {
    /// A wrapped segment read/write failure.
    Segment(SegmentError),
    /// Filesystem I/O failure.
    Io(std::io::Error),
    /// `manifest.json` was missing a field, unparseable, or structurally invalid.
    ManifestCorrupt { detail: String },
    /// A `workspace-state-<gen>.bin` file was corrupt (magic/checksum/length).
    StateCorrupt { detail: String },
    /// The manifest and its paired state/segment disagree (generation, checksum,
    /// or record counts).
    PairMismatch { detail: String },
    /// A manifest or state file declared a schema version newer than supported.
    FutureSchema { what: String, found: u64 },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Segment(e) => write!(f, "{e}"),
            StoreError::Io(e) => write!(f, "store io error: {e}"),
            StoreError::ManifestCorrupt { detail } => write!(f, "manifest corrupt: {detail}"),
            StoreError::StateCorrupt { detail } => write!(f, "workspace-state corrupt: {detail}"),
            StoreError::PairMismatch { detail } => {
                write!(f, "segment/state pair mismatch: {detail}")
            }
            StoreError::FutureSchema { what, found } => {
                write!(f, "unsupported future {what} schema version: {found}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<SegmentError> for StoreError {
    fn from(e: SegmentError) -> Self {
        StoreError::Segment(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// Convenience alias for store results.
pub type StoreResult<T> = std::result::Result<T, StoreError>;
