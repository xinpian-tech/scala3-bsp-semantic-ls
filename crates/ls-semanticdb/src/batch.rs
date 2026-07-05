//! The deterministic result of one SemanticDB ingest batch.
//!
//! Everything is immutable and order-deterministic so repeated ingests of the
//! same corpus produce identical output.

use std::collections::HashMap;

use ls_index_model::{NormalizedDocument, RenameProfile, SymbolKey};

use crate::groups::{self, AliasGroups};
use crate::profile::{self, DocFacts};

/// The result of one ingest batch: normalized documents (input order), exact
/// alias groups, and one [`RenameProfile`] per rename group.
pub struct SemanticBatch {
    pub documents: Vec<NormalizedDocument>,
    pub groups: AliasGroups,
    pub rename_profiles: Vec<RenameProfile>,
}

impl SemanticBatch {
    pub fn ref_group_of(&self, key: &SymbolKey) -> Option<usize> {
        self.groups.ref_group_of(key)
    }

    pub fn rename_group_of(&self, key: &SymbolKey) -> Option<usize> {
        self.groups.rename_group_of(key)
    }

    pub fn rename_profile_of(&self, key: &SymbolKey) -> Option<&RenameProfile> {
        self.groups
            .rename_group_of(key)
            .map(|gi| &self.rename_profiles[gi])
    }

    /// Builds groups and rename profiles for a batch, treating every uri as a
    /// plain editable workspace source.
    pub fn assemble(documents: Vec<NormalizedDocument>) -> SemanticBatch {
        Self::assemble_with_facts(documents, &HashMap::new())
    }

    /// Builds groups and rename profiles; `doc_facts` carries per-uri
    /// generated/readonly/dependency knowledge (missing uris default to plain
    /// editable workspace sources).
    pub fn assemble_with_facts(
        documents: Vec<NormalizedDocument>,
        doc_facts: &HashMap<String, DocFacts>,
    ) -> SemanticBatch {
        let groups = groups::build(&documents);
        let rename_profiles = profile::build_profiles(&documents, &groups, doc_facts);
        SemanticBatch {
            documents,
            groups,
            rename_profiles,
        }
    }
}
