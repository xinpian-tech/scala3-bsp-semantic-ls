//! Typed failures surfaced to LSP as structured errors.
//!
//! There are no pretend-accurate fallbacks: when semantic truth is unavailable
//! the request fails with one of these. Messages mirror the Scala `LsError`
//! enum verbatim so operator-facing text stays stable across the rewrite.

use std::fmt;

/// A typed language-server failure.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LsError {
    /// A target produced no SemanticDB output.
    IndexUnavailable { target: String },
    /// A document's index is stale (md5 mismatch) and could not be refreshed.
    StaleIndex { uri: String },
    /// `buildTarget/compile` failed; rename requires a fresh successful compile.
    CompileFailed { target: String },
    /// Rename was rejected; carries one message per reason.
    RenameRejected { reasons: Vec<String> },
    /// The symbol is provided by a PC-only plugin and is absent from SemanticDB.
    PcOnlySymbol,
    /// No symbol occurrence exists at the cursor.
    NoSymbolAtCursor {
        uri: String,
        line: u32,
        character: u32,
    },
    /// The URI is not part of any indexed build target.
    NotIndexed { uri: String },
    /// The URI has no SemanticDB output.
    NoSemanticdb { uri: String },
}

impl fmt::Display for LsError {
    /// Renders the operator-facing message, byte-for-byte compatible with the
    /// Scala `LsError.message`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LsError::IndexUnavailable { target } => write!(
                f,
                "target {target} has no SemanticDB output; workspace symbol, references and rename are disabled for it"
            ),
            LsError::StaleIndex { uri } => {
                write!(f, "index for {uri} is stale (md5 mismatch) and could not be refreshed")
            }
            LsError::CompileFailed { target } => write!(
                f,
                "buildTarget/compile failed for {target}; rename requires a fresh successful compile"
            ),
            LsError::RenameRejected { reasons } => {
                f.write_str("rename rejected:")?;
                for r in reasons {
                    write!(f, "\n  - {r}")?;
                }
                Ok(())
            }
            LsError::PcOnlySymbol => f.write_str(
                "This symbol is provided by a PC-only plugin and is not present in fresh SemanticDB. \
Workspace-wide references and cross-file rename are unavailable for this symbol.",
            ),
            LsError::NoSymbolAtCursor {
                uri,
                line,
                character,
            } => write!(f, "no symbol occurrence at {uri}:{line}:{character}"),
            LsError::NotIndexed { uri } => {
                write!(f, "{uri} is not part of any indexed build target")
            }
            LsError::NoSemanticdb { uri } => write!(
                f,
                "{uri} has no SemanticDB output; every source must be compiled with -Xsemanticdb"
            ),
        }
    }
}

impl LsError {
    /// The operator-facing message, byte-for-byte compatible with the Scala
    /// `LsError.message`. Delegates to the [`Display`](fmt::Display) impl.
    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl std::error::Error for LsError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_match_scala_exactly() {
        assert_eq!(
            LsError::IndexUnavailable { target: "T".into() }.message(),
            "target T has no SemanticDB output; workspace symbol, references and rename are disabled for it"
        );
        assert_eq!(
            LsError::StaleIndex {
                uri: "file:///a.scala".into()
            }
            .message(),
            "index for file:///a.scala is stale (md5 mismatch) and could not be refreshed"
        );
        assert_eq!(
            LsError::CompileFailed { target: "T".into() }.message(),
            "buildTarget/compile failed for T; rename requires a fresh successful compile"
        );
        assert_eq!(
            LsError::NoSymbolAtCursor {
                uri: "file:///a.scala".into(),
                line: 3,
                character: 7
            }
            .message(),
            "no symbol occurrence at file:///a.scala:3:7"
        );
        assert_eq!(
            LsError::NotIndexed {
                uri: "file:///a.scala".into()
            }
            .message(),
            "file:///a.scala is not part of any indexed build target"
        );
        assert_eq!(
            LsError::NoSemanticdb { uri: "file:///a.scala".into() }.message(),
            "file:///a.scala has no SemanticDB output; every source must be compiled with -Xsemanticdb"
        );
        assert_eq!(
            LsError::PcOnlySymbol.message(),
            "This symbol is provided by a PC-only plugin and is not present in fresh SemanticDB. \
Workspace-wide references and cross-file rename are unavailable for this symbol."
        );
    }

    #[test]
    fn rename_rejected_joins_reasons_with_bullets() {
        assert_eq!(
            LsError::RenameRejected { reasons: vec![] }.message(),
            "rename rejected:"
        );
        assert_eq!(
            LsError::RenameRejected {
                reasons: vec!["a".into(), "b".into()]
            }
            .message(),
            "rename rejected:\n  - a\n  - b"
        );
    }

    #[test]
    fn display_renders_message() {
        let e = LsError::NotIndexed {
            uri: "file:///x".into(),
        };
        assert_eq!(
            e.to_string(),
            "file:///x is not part of any indexed build target"
        );
        assert_eq!(e.message(), e.to_string());
    }
}
