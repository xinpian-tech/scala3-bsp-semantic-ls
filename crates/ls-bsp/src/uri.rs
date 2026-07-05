//! Minimal `file://` URI <-> path conversion. The two directions are exact
//! inverses for absolute paths, which is all the BSP layer round-trips.

use std::path::{Path, PathBuf};

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/')
}

/// `/a/b c.scala` -> `file:///a/b%20c.scala`. Percent-encodes everything
/// outside the unreserved set (path separators are kept literal).
pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::from("file://");
    for &b in s.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// Inverse of [`path_to_uri`]. An optional authority component (`file://host/…`)
/// is dropped; the path starts at its first `/`.
pub fn uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| format!("not a file uri: {uri}"))?;
    let path_part = match rest.find('/') {
        Some(i) => &rest[i..],
        None => return Err(format!("file uri has no path: {uri}")),
    };
    let bytes = percent_decode(path_part)?;
    let s = String::from_utf8(bytes).map_err(|e| format!("bad utf8 in uri {uri}: {e}"))?;
    Ok(PathBuf::from(s))
}

fn percent_decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(format!("truncated percent-escape in {s}"));
            }
            let hi = hex_val(bytes[i + 1]).ok_or_else(|| format!("bad percent-escape in {s}"))?;
            let lo = hex_val(bytes[i + 2]).ok_or_else(|| format!("bad percent-escape in {s}"))?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
