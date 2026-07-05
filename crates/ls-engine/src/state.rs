//! The generational workspace-state payload: the cross-generation residue that a
//! single segment cannot reconstruct — a per-document epoch counter and the
//! SemanticDB md5 recorded at ingest. Serialized as the opaque payload of the
//! `ls-store` workspace-state container (little-endian, length-prefixed).

use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocState {
    pub epoch: i32,
    pub md5: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IngestState {
    pub docs: BTreeMap<String, DocState>,
}

const MAGIC: u32 = 0x4c53_5354; // "LSST"

impl IngestState {
    pub fn get(&self, uri: &str) -> Option<&DocState> {
        self.docs.get(uri)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&(self.docs.len() as u32).to_le_bytes());
        for (uri, st) in &self.docs {
            put_str(&mut out, uri);
            out.extend_from_slice(&st.epoch.to_le_bytes());
            put_str(&mut out, &st.md5);
        }
        out
    }

    /// Lenient decode: an unrecognized or truncated payload yields whatever was
    /// read so far (a fresh state simply re-seeds epochs) rather than an error.
    pub fn decode(bytes: &[u8]) -> IngestState {
        let mut c = Cursor { bytes, pos: 0 };
        if c.u32() != Some(MAGIC) {
            return IngestState::default();
        }
        let Some(count) = c.u32() else {
            return IngestState::default();
        };
        let mut docs = BTreeMap::new();
        for _ in 0..count {
            let (Some(uri), Some(epoch), Some(md5)) = (c.take_str(), c.i32(), c.take_str()) else {
                return IngestState { docs };
            };
            docs.insert(uri, DocState { epoch, md5 });
        }
        IngestState { docs }
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.bytes.len() {
            return None;
        }
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }

    fn take_str(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).ok()
    }
}
