//! MD5 freshness check — a stored `TextDocument.md5` compared against current
//! source text. Anything but [`FreshnessCheck::Fresh`] means the SemanticDB
//! document must not be used as semantic truth for that source.
//!
//! MD5 is implemented here (zero-dependency, matching the crate's design) and is
//! exercised against the standard RFC 1321 test vectors.

use crate::model::SdbDocument;
use ls_index_model::NormalizedDocument;

/// Result of comparing current source text against a stored md5.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FreshnessCheck {
    Fresh,
    /// The document carries no md5 at all; cannot prove freshness.
    MissingMd5,
    Stale {
        document_md5: String,
        source_md5: String,
    },
}

impl FreshnessCheck {
    pub fn is_fresh(&self) -> bool {
        matches!(self, FreshnessCheck::Fresh)
    }
}

/// Uppercase-hex MD5 of the UTF-8 bytes of `text` — scalameta's convention for
/// `TextDocument.md5`.
pub fn compute_hex(text: &str) -> String {
    let digest = md5_digest(text.as_bytes());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(32);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Compares `source_text` against a stored md5 (case-insensitively — the spec
/// convention is uppercase but a case mismatch is not amplified into staleness).
pub fn validate(source_text: &str, document_md5: &str) -> FreshnessCheck {
    if document_md5.is_empty() {
        FreshnessCheck::MissingMd5
    } else {
        let actual = compute_hex(source_text);
        if actual.eq_ignore_ascii_case(document_md5) {
            FreshnessCheck::Fresh
        } else {
            FreshnessCheck::Stale {
                document_md5: document_md5.to_string(),
                source_md5: actual,
            }
        }
    }
}

/// Freshness of a raw [`SdbDocument`] against `source_text`.
pub fn validate_doc(source_text: &str, doc: &SdbDocument) -> FreshnessCheck {
    validate(source_text, &doc.md5)
}

/// Freshness of a [`NormalizedDocument`] against `source_text`.
pub fn validate_normalized(source_text: &str, doc: &NormalizedDocument) -> FreshnessCheck {
    validate(source_text, &doc.md5)
}

/// RFC 1321 MD5 over `msg`, returning the 16-byte digest.
fn md5_digest(msg: &[u8]) -> [u8; 16] {
    // Per-round left-rotate amounts.
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    // K[i] = floor(2^32 * abs(sin(i + 1))).
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    // Pad: 0x80, then zeros to 56 mod 64, then the original bit length (LE u64).
    let mut data = msg.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in data.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            m[i] = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | (!b & d), i)
            } else if i < 32 {
                ((d & b) | (!d & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | !d), (7 * i) % 16)
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_md5_vectors() {
        assert_eq!(compute_hex(""), "D41D8CD98F00B204E9800998ECF8427E");
        assert_eq!(compute_hex("hello"), "5D41402ABC4B2A76B9719D911017C592");
        assert_eq!(
            compute_hex("The quick brown fox jumps over the lazy dog"),
            "9E107D9D372BB6826BD81D3542A419D6"
        );
    }
}
