//! Flat little-endian codec for the variable op payloads: a 16-byte envelope
//! (`magic`, `kind`, `body_len`, `blob_len`), a body of fixed-width records
//! (`u32`/`i32`, blob-referenced strings as `offset,len`, count-prefixed
//! lists), and a trailing UTF-8 string blob. Every record field is 4-byte, so a
//! record's byte image equals its `#[repr(C)]` layout. The reader bounds-checks
//! every field and blob slice, so a malformed buffer yields [`AbiError`] rather
//! than a panic or out-of-bounds read.

/// The envelope magic (`"LPAB"` little-endian).
pub const MAGIC: u32 = 0x4241_504c;

/// A payload failed to encode or (usually) decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbiError(pub String);

impl AbiError {
    fn new(msg: impl Into<String>) -> AbiError {
        AbiError(msg.into())
    }
}

impl std::fmt::Display for AbiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "abi decode error: {}", self.0)
    }
}

impl std::error::Error for AbiError {}

/// Builds a payload buffer: fixed records into `body`, strings/opaque bytes into
/// `blob`, then `finish` prepends the envelope.
pub struct Writer {
    body: Vec<u8>,
    blob: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer {
            body: Vec::new(),
            blob: Vec::new(),
        }
    }

    pub fn u32(&mut self, v: u32) {
        self.body.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }

    pub fn bool32(&mut self, v: bool) {
        self.u32(v as u32);
    }

    /// A required string: a `BlobStr` (`offset`, `len`) into the blob.
    pub fn str(&mut self, s: &str) {
        let (offset, len) = self.intern(s.as_bytes());
        self.u32(offset);
        self.u32(len);
    }

    /// An optional string: a presence flag then a `BlobStr`. `None` and
    /// `Some("")` are distinct (`0` vs `1` present).
    pub fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(v) => {
                self.u32(1);
                self.str(v);
            }
            None => {
                self.u32(0);
                self.u32(0);
                self.u32(0);
            }
        }
    }

    /// A required opaque byte payload (e.g. a completion item's `data`).
    pub fn opt_bytes(&mut self, b: Option<&[u8]>) {
        match b {
            Some(v) => {
                self.u32(1);
                let (offset, len) = self.intern(v);
                self.u32(offset);
                self.u32(len);
            }
            None => {
                self.u32(0);
                self.u32(0);
                self.u32(0);
            }
        }
    }

    /// A flattened `[start, end)` range: four `u32`s.
    pub fn range(
        &mut self,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) {
        self.u32(start_line);
        self.u32(start_character);
        self.u32(end_line);
        self.u32(end_character);
    }

    fn intern(&mut self, bytes: &[u8]) -> (u32, u32) {
        let offset = self.blob.len() as u32;
        self.blob.extend_from_slice(bytes);
        (offset, bytes.len() as u32)
    }

    /// Concatenates the envelope, body, and blob into the final buffer.
    pub fn finish(self, kind: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.body.len() + self.blob.len());
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(self.body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.body);
        out.extend_from_slice(&self.blob);
        out
    }
}

impl Default for Writer {
    fn default() -> Self {
        Writer::new()
    }
}

/// Reads a payload buffer produced by [`Writer`]. Borrows the body and blob.
pub struct Reader<'a> {
    body: &'a [u8],
    blob: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Validates the envelope (magic, kind, exact `16 + body_len + blob_len`
    /// length) and splits the buffer into its body and blob regions.
    pub fn new(buf: &'a [u8], expected_kind: u32) -> Result<Reader<'a>, AbiError> {
        if buf.len() < 16 {
            return Err(AbiError::new("buffer shorter than the 16-byte envelope"));
        }
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        if magic != MAGIC {
            return Err(AbiError::new(format!("bad magic 0x{magic:08x}")));
        }
        let kind = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if kind != expected_kind {
            return Err(AbiError::new(format!(
                "payload kind {kind} != expected {expected_kind}"
            )));
        }
        let body_len = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
        let blob_len = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as usize;
        let total = 16usize
            .checked_add(body_len)
            .and_then(|n| n.checked_add(blob_len))
            .ok_or_else(|| AbiError::new("length overflow"))?;
        if total != buf.len() {
            return Err(AbiError::new(format!(
                "declared length {total} != actual {}",
                buf.len()
            )));
        }
        Ok(Reader {
            body: &buf[16..16 + body_len],
            blob: &buf[16 + body_len..],
            pos: 0,
        })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], AbiError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| AbiError::new("body cursor overflow"))?;
        if end > self.body.len() {
            return Err(AbiError::new("body underrun"));
        }
        let slice = &self.body[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub fn u32(&mut self) -> Result<u32, AbiError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn i32(&mut self) -> Result<i32, AbiError> {
        Ok(self.u32()? as i32)
    }

    pub fn bool32(&mut self) -> Result<bool, AbiError> {
        Ok(self.u32()? != 0)
    }

    /// Reads a count and guards it against the remaining body: each element is
    /// at least 4 bytes, so a fabricated huge count is rejected before any
    /// allocation.
    pub fn count(&mut self) -> Result<usize, AbiError> {
        let n = self.u32()? as usize;
        let remaining = self.body.len() - self.pos;
        if n > remaining / 4 {
            return Err(AbiError::new(format!(
                "list count {n} exceeds the remaining body"
            )));
        }
        Ok(n)
    }

    fn blob_slice(&self, offset: u32, len: u32) -> Result<&'a [u8], AbiError> {
        let end = (offset as usize)
            .checked_add(len as usize)
            .ok_or_else(|| AbiError::new("blob slice overflow"))?;
        if end > self.blob.len() {
            return Err(AbiError::new("blob slice out of range"));
        }
        Ok(&self.blob[offset as usize..end])
    }

    pub fn str(&mut self) -> Result<String, AbiError> {
        let offset = self.u32()?;
        let len = self.u32()?;
        let bytes = self.blob_slice(offset, len)?;
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|_| AbiError::new("blob string is not valid UTF-8"))
    }

    pub fn opt_str(&mut self) -> Result<Option<String>, AbiError> {
        let present = self.u32()?;
        let offset = self.u32()?;
        let len = self.u32()?;
        if present == 0 {
            return Ok(None);
        }
        let bytes = self.blob_slice(offset, len)?;
        std::str::from_utf8(bytes)
            .map(|s| Some(s.to_string()))
            .map_err(|_| AbiError::new("optional blob string is not valid UTF-8"))
    }

    pub fn opt_bytes(&mut self) -> Result<Option<Vec<u8>>, AbiError> {
        let present = self.u32()?;
        let offset = self.u32()?;
        let len = self.u32()?;
        if present == 0 {
            return Ok(None);
        }
        Ok(Some(self.blob_slice(offset, len)?.to_vec()))
    }

    /// Reads a flattened range as `(start_line, start_character, end_line, end_character)`.
    pub fn range(&mut self) -> Result<(u32, u32, u32, u32), AbiError> {
        Ok((self.u32()?, self.u32()?, self.u32()?, self.u32()?))
    }

    /// Requires the body to be fully consumed (no trailing garbage).
    pub fn finish(self) -> Result<(), AbiError> {
        if self.pos != self.body.len() {
            return Err(AbiError::new(format!(
                "{} trailing body bytes",
                self.body.len() - self.pos
            )));
        }
        Ok(())
    }
}
