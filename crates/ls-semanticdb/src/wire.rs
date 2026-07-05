//! Minimal protobuf wire-format reader over a byte slice.
//!
//! A verbatim port of the Scala `ProtoReader`: zero external dependencies by
//! design. The SemanticDB subset this crate consumes is decoded by hand against
//! field numbers taken from `scalameta/semanticdb.proto` (see [`crate::parser`]).
//! Unlike a generated decoder, this reader skips legacy group wire types (3/4)
//! and enforces exact varint-length / truncation semantics, which the ported
//! `WireDecoderSuite` asserts.
//!
//! Wire types (protobuf encoding spec): 0 varint, 1 fixed64, 2 length-delimited,
//! 3 group start (legacy, skipped recursively), 4 group end (legacy), 5 fixed32.

use crate::error::{SemanticdbError, SemanticdbResult};

pub(crate) struct ProtoReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> ProtoReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        ProtoReader {
            bytes,
            pos: 0,
            end: bytes.len(),
        }
    }

    fn slice(bytes: &'a [u8], start: usize, end: usize) -> Self {
        ProtoReader {
            bytes,
            pos: start,
            end,
        }
    }

    pub(crate) fn has_remaining(&self) -> bool {
        self.pos < self.end
    }

    fn fail<T>(&self, message: &str) -> SemanticdbResult<T> {
        Err(SemanticdbError::Parse(format!(
            "{message} (at byte offset {})",
            self.pos
        )))
    }

    /// Base-128 varint, at most 10 bytes. Bits above 63 are dropped, matching
    /// standard protobuf truncation semantics.
    pub(crate) fn read_varint(&mut self) -> SemanticdbResult<u64> {
        let mut shift: u32 = 0;
        let mut result: u64 = 0;
        loop {
            if shift >= 64 {
                return self.fail("malformed varint (more than 10 bytes)");
            }
            if self.pos >= self.end {
                return self.fail("truncated varint");
            }
            let b = self.bytes[self.pos];
            self.pos += 1;
            result |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// int32 fields are 64-bit varints truncated to `i32` (protobuf semantics:
    /// negative int32 values are sign-extended 10-byte varints).
    pub(crate) fn read_int32(&mut self) -> SemanticdbResult<i32> {
        Ok(self.read_varint()? as i32)
    }

    pub(crate) fn read_tag(&mut self) -> SemanticdbResult<u32> {
        let raw = self.read_varint()?;
        if raw == 0 || raw > i32::MAX as u64 {
            return self.fail(&format!("tag out of range: {raw}"));
        }
        let tag = raw as u32;
        if tag >> 3 == 0 {
            return self.fail("field number 0 is invalid");
        }
        Ok(tag)
    }

    /// Reads a length-delimited field header and returns its `(offset, len)`,
    /// advancing past the payload.
    fn read_length_delimited_slice(&mut self) -> SemanticdbResult<(usize, usize)> {
        let len = self.read_varint()?;
        if len > (self.end - self.pos) as u64 {
            return self.fail(&format!(
                "truncated length-delimited field (declared {len} bytes)"
            ));
        }
        let off = self.pos;
        let len = len as usize;
        self.pos += len;
        Ok((off, len))
    }

    /// UTF-8 string; invalid sequences are replaced (matching Java's
    /// `new String(bytes, UTF_8)`, which never throws).
    pub(crate) fn read_string(&mut self) -> SemanticdbResult<String> {
        let (off, len) = self.read_length_delimited_slice()?;
        Ok(String::from_utf8_lossy(&self.bytes[off..off + len]).into_owned())
    }

    /// Sub-reader over one embedded message; skips the payload in this reader
    /// without copying bytes.
    pub(crate) fn read_message(&mut self) -> SemanticdbResult<ProtoReader<'a>> {
        let (off, len) = self.read_length_delimited_slice()?;
        Ok(ProtoReader::slice(self.bytes, off, off + len))
    }

    fn skip_bytes(&mut self, n: usize) -> SemanticdbResult<()> {
        if n > self.end - self.pos {
            return self.fail(&format!("truncated field ({n} bytes expected)"));
        }
        self.pos += n;
        Ok(())
    }

    /// Skips one field payload of the given wire type. Unknown fields of every
    /// wire type are supported, so schema evolution never breaks decoding.
    pub(crate) fn skip_field(&mut self, wire_type: u32, field_number: u32) -> SemanticdbResult<()> {
        match wire_type {
            0 => {
                self.read_varint()?;
                Ok(())
            }
            1 => self.skip_bytes(8),
            2 => {
                self.read_length_delimited_slice()?;
                Ok(())
            }
            3 => {
                // Legacy group: skip nested fields until the matching end-group.
                loop {
                    let tag = self.read_tag()?;
                    let wt = tag & 7;
                    let field = tag >> 3;
                    if wt == 4 {
                        if field != field_number {
                            return self
                                .fail(&format!("mismatched end-group for field {field_number}"));
                        }
                        return Ok(());
                    }
                    self.skip_field(wt, field)?;
                }
            }
            4 => self.fail("unexpected end-group tag"),
            5 => self.skip_bytes(4),
            other => self.fail(&format!("unsupported wire type {other}")),
        }
    }
}
