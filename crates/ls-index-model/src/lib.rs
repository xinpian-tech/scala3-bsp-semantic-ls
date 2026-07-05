//! `ls-index-model` — pure, dependency-free model primitives shared across the
//! Rust language-server crates.
//!
//! It carries: opaque id/ordinal newtypes ([`ids`]); zero-based line/character
//! positions and the columnar `(line << 12) | char` packing ([`text`]);
//! occurrence roles and per-occurrence flags ([`flags`]); SemanticDB symbol
//! identity, kinds, and property masks ([`symbol`]); the rename-safety reason
//! mask ([`unsafe_reason`]); an exact target-membership bitset ([`bitset`]);
//! and the typed error surface ([`error`]).
//!
//! Every numeric contract — SemanticDB kind/property codes, the packing layout,
//! the reason bit assignments, and the error messages — is ported verbatim from
//! the Scala `ls.index` package (`modules/ls-index-model`), so this crate is a
//! behavior-preserving foundation for the storage and engine layers built on it.

// This crate is pure, safe model logic — no FFI, no unsafe blocks.
#![forbid(unsafe_code)]

mod bitset;
mod error;
mod flags;
mod groups;
mod ids;
mod semantics;
mod symbol;
mod text;

pub mod unsafe_reason;

pub use bitset::TargetBitset;
pub use error::LsError;
pub use flags::{occ_flags, Role};
pub use groups::RenameProfile;
pub use ids::{
    DocId, DocOrd, RefGroupId, RefGroupOrd, RenameGroupId, RenameGroupOrd, SymbolId, SymbolOrd,
    TargetId, TargetOrd,
};
pub use semantics::{NormalizedDocument, Occurrence, SymbolInfo};
pub use symbol::{sym_props, SymKind, SymbolKey};
pub use text::{Loc, Pos, Span};
