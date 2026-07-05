//! `file://` URI <-> path conversion shared across the Rust server crates.
//!
//! The two directions are exact inverses for absolute paths (all the BSP layer
//! and the PC go-to-definition callback round-trip). [`normalize`] collapses
//! `.`/`..` lexically so equivalent spellings compare equal, mirroring the
//! retained `ls.core.Uris` semantics (`Path.toUri` / `URI -> Path` +
//! `.normalize()`).

use std::path::{Component, Path, PathBuf};

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

/// Lexically normalize a path: collapse `.` and `..` components without touching
/// the filesystem (no symlink resolution), so equivalent spellings of the same
/// absolute path compare equal. Matches Java `Path.normalize`.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_path_with_spaces_and_reserved_bytes() {
        let p = Path::new("/tmp/ws with space/A B.scala");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///tmp/ws%20with%20space/A%20B.scala");
        assert_eq!(uri_to_path(&uri).unwrap(), p);
    }

    #[test]
    fn decodes_percent_escapes_and_drops_authority() {
        assert_eq!(
            uri_to_path("file:///a/b%20c.scala").unwrap(),
            PathBuf::from("/a/b c.scala")
        );
        // an authority component is dropped; the path starts at its first `/`.
        assert_eq!(
            uri_to_path("file://host/a/b.scala").unwrap(),
            PathBuf::from("/a/b.scala")
        );
    }

    #[test]
    fn rejects_non_file_and_pathless_uris() {
        assert!(uri_to_path("https://example/x").is_err());
        assert!(uri_to_path("file://").is_err());
    }

    #[test]
    fn normalize_collapses_dot_and_dotdot() {
        assert_eq!(
            normalize(Path::new("/tmp/a/./b/../c")),
            PathBuf::from("/tmp/a/c")
        );
        assert_eq!(normalize(Path::new("/tmp/a/b")), PathBuf::from("/tmp/a/b"));
    }
}
