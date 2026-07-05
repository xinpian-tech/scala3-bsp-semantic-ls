//! `ls-semanticdb` — SemanticDB ingest for the Rust language server.
//!
//! A behavior-preserving port of the Scala `ls.semanticdb` package: a
//! zero-dependency protobuf reader ([`parser`]/`wire`) decodes the SemanticDB
//! `TextDocuments` subset ([`model`]); [`md5`] proves per-document freshness;
//! [`symbols`] parses the SemanticDB symbol grammar; [`normalize`] lowers a raw
//! document into the shared [`ls_index_model::NormalizedDocument`]; and
//! [`groups`]/[`profile`]/[`batch`] build exact alias groups and rename-safety
//! profiles for one ingest batch.
//!
//! Every numeric contract and grouping rule is ported verbatim so this crate is
//! a faithful foundation for the engine layer built on it.

// Pure, safe model + decode logic — no FFI.
#![forbid(unsafe_code)]

mod error;
mod wire;

pub mod batch;
pub mod groups;
pub mod md5;
pub mod model;
pub mod normalize;
pub mod parser;
pub mod profile;
pub mod symbols;

pub use batch::SemanticBatch;
pub use error::{SemanticdbError, SemanticdbResult};
pub use groups::AliasGroups;
pub use md5::FreshnessCheck;
pub use model::{SdbDocument, SdbDocuments, SdbOccurrence, SdbRange, SdbSymbolInfo};
pub use normalize::normalize;
pub use parser::{parse_file, parse_text_documents};
pub use profile::DocFacts;
