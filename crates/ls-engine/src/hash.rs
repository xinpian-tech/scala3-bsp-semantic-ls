//! Stable FNV-1a 64 hashing used to derive collision-free-enough persistent ids
//! (doc ids, symbol ids) without a central intern store. The values are stable
//! across generations for the same input, so a uri always qualifies its local
//! symbols the same way.

use ls_index_model::{DocId, SymbolKey};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn mix(h: u64, b: u64) -> u64 {
    (h ^ (b & 0xff)).wrapping_mul(FNV_PRIME)
}

pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h = mix(h, b as u64);
    }
    h
}

/// Stable, positive doc id for a SemanticDB uri.
pub fn doc_id_for(uri: &str) -> DocId {
    DocId::new(fnv1a_64(uri.as_bytes()) >> 1)
}

/// Stable, positive target id for a BSP target id.
pub fn target_id_for(bsp_id: &str) -> i64 {
    (fnv1a_64(bsp_id.as_bytes()) >> 1) as i64
}

/// Stable, positive symbol id (mirrors the Scala ingest's FNV-1a over the symbol
/// string plus the local doc id, low byte first).
pub fn symbol_id_for(key: &SymbolKey) -> i64 {
    let mut h = fnv1a_64(key.semantic_symbol.as_bytes());
    if let Some(doc) = key.local_doc {
        let mut v = doc.value();
        for _ in 0..8 {
            h = mix(h, v);
            v >>= 8;
        }
    }
    (h >> 1) as i64
}
