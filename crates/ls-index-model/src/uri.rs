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

/// Inverse of [`path_to_uri`], accepting the spellings `Path.of(URI.create(…))`
/// does: the empty-authority form `file:///abs` and the single-slash `file:/abs`
/// (no authority). A NON-empty authority (`file://host/abs`) is rejected, because
/// `Path.of(URI)` — which the retained `Uris.toPath`/`WorkspaceUris.toSdbUri`
/// call — throws on an authority component; dropping the host instead would let a
/// `file://host/…` URI be answered as if it named the local `/…` path. Non-file
/// schemes and pathless file URIs are also rejected.
pub fn uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let rest = uri
        .strip_prefix("file:")
        .ok_or_else(|| format!("not a file uri: {uri}"))?;
    let path_part = if let Some(after_authority) = rest.strip_prefix("//") {
        // Authority form. The authority is the text before the first `/`; only an
        // EMPTY authority (`file:///…`, first `/` at index 0) is accepted, and the
        // path is the remainder from that `/`. A non-empty authority is rejected.
        match after_authority.find('/') {
            Some(0) => after_authority,
            Some(_) => return Err(format!("file uri has a non-empty authority: {uri}")),
            None => return Err(format!("file uri has no path: {uri}")),
        }
    } else if rest.starts_with('/') {
        // Single-slash form `file:/abs`: the path is the remainder verbatim.
        rest
    } else {
        return Err(format!("file uri has no path: {uri}"));
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

/// Canonicalizes a `file://` URI string the way `ls.core.Uris.normalize` does:
/// URI -> path -> lexical [`normalize`] -> URI, so equivalent spellings key the
/// same document. A URI that does not parse as a file URI is returned unchanged
/// (mirroring the Scala `catch` that falls back to the raw input).
///
/// A `file://host/…` URI with a non-empty authority is left unchanged: Java's
/// `Path.of(URI)` (which `Uris.normalize` calls) rejects an authority component,
/// so the Scala normalizer returns the input verbatim rather than silently
/// dropping the host. Keying the document under the raw spelling keeps it
/// distinct from the authority-less form, as it is in the Scala server.
pub fn normalize_uri(uri: &str) -> String {
    if has_nonempty_authority(uri) {
        return uri.to_string();
    }
    match uri_to_path(uri) {
        Ok(path) => path_to_uri(&normalize(&path)),
        Err(_) => uri.to_string(),
    }
}

/// Whether `uri` is a `file://<authority>/…` URI with a non-empty authority.
fn has_nonempty_authority(uri: &str) -> bool {
    uri.strip_prefix("file://")
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|authority| !authority.is_empty())
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
    fn decodes_percent_escapes_and_rejects_a_non_empty_authority() {
        assert_eq!(
            uri_to_path("file:///a/b%20c.scala").unwrap(),
            PathBuf::from("/a/b c.scala")
        );
        // A non-empty authority is rejected: `Path.of(URI)` throws on it, so the
        // retained `Uris.toPath` never maps `file://host/...` to a local path.
        assert!(uri_to_path("file://host/a/b.scala").is_err());
    }

    #[test]
    fn rejects_non_file_and_pathless_uris() {
        assert!(uri_to_path("https://example/x").is_err());
        assert!(uri_to_path("file://").is_err());
        assert!(uri_to_path("file:").is_err());
        assert!(uri_to_path("file://host").is_err());
    }

    #[test]
    fn accepts_single_slash_file_uri_and_canonicalizes() {
        // `file:/abs` (no authority) resolves the same absolute path as the
        // three-slash `file:///abs`, matching `Path.of(URI.create(...))`.
        assert_eq!(
            uri_to_path("file:/tmp/a.scala").unwrap(),
            PathBuf::from("/tmp/a.scala")
        );
        assert_eq!(
            uri_to_path("file:/tmp/a.scala").unwrap(),
            uri_to_path("file:///tmp/a.scala").unwrap()
        );
        // Decode + normalize + re-encode canonicalizes a single-slash `..`
        // spelling to the three-slash form.
        let p = normalize(&uri_to_path("file:/tmp/a/../a/B.scala").unwrap());
        assert_eq!(path_to_uri(&p), "file:///tmp/a/B.scala");
    }

    #[test]
    fn normalize_collapses_dot_and_dotdot() {
        assert_eq!(
            normalize(Path::new("/tmp/a/./b/../c")),
            PathBuf::from("/tmp/a/c")
        );
        assert_eq!(normalize(Path::new("/tmp/a/b")), PathBuf::from("/tmp/a/b"));
    }

    // Mirrors ls.core.Uris.normalize: canonicalize a file uri, pass a
    // non-file uri through unchanged.
    #[test]
    fn normalize_uri_canonicalizes_a_file_uri_and_passes_others_through() {
        assert_eq!(
            normalize_uri("file:///ws/a/../b/c.scala"),
            "file:///ws/b/c.scala"
        );
        assert_eq!(normalize_uri("untitled:Untitled-1"), "untitled:Untitled-1");
    }

    // Java Path.of(URI) rejects an authority, so Uris.normalize leaves a
    // file://host/... uri unchanged rather than dropping the host.
    #[test]
    fn normalize_uri_leaves_a_non_empty_authority_unchanged() {
        assert_eq!(
            normalize_uri("file://host/ws/a.scala"),
            "file://host/ws/a.scala"
        );
    }
}
