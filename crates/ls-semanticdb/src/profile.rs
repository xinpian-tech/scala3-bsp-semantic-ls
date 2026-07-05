//! Per-rename-group rename-safety profiles (plan 13.2).
//!
//! Semantics:
//!   - `is_local`: every member is a local symbol;
//!   - `is_external`: no Definition-role occurrence of any member in the batch —
//!     the definition lives outside the workspace;
//!   - `has_generated`/`has_readonly`: some occurrence lands in a generated /
//!     readonly document;
//!   - `editable_occurrence_count`: occurrences in documents that are neither
//!     generated, readonly, nor dependency sources;
//!   - `unsafe_reason_mask`: the group's semantic mask plus External /
//!     GeneratedOccurrence / ReadonlyOccurrence / DependencySource bits.

use std::collections::{HashMap, HashSet};

use ls_index_model::{unsafe_reason, NormalizedDocument, RenameProfile, Role, SymbolKey};

use crate::groups::AliasGroups;
use crate::symbols;

/// Facts about one document that only the ingest orchestration layer knows (from
/// BSP/build metadata), keyed by `TextDocument.uri`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DocFacts {
    pub generated: bool,
    pub readonly: bool,
    pub is_dependency_source: bool,
}

impl DocFacts {
    /// A rename edit may touch this document.
    pub fn editable(&self) -> bool {
        !self.generated && !self.readonly && !self.is_dependency_source
    }

    /// Plain editable workspace source — the default when no facts are known.
    pub fn workspace_source() -> Self {
        DocFacts {
            generated: false,
            readonly: false,
            is_dependency_source: false,
        }
    }
}

/// Computes one [`RenameProfile`] per rename group, aligned with
/// `groups.rename_groups`.
pub fn build_profiles(
    docs: &[NormalizedDocument],
    groups: &AliasGroups,
    doc_facts: &HashMap<String, DocFacts>,
) -> Vec<RenameProfile> {
    let n = groups.rename_groups.len();
    let mut has_definition = vec![false; n];
    let mut has_generated = vec![false; n];
    let mut has_readonly = vec![false; n];
    let mut has_dependency = vec![false; n];
    let mut editable_count = vec![0u32; n];
    // Global keys carrying a Definition occurrence anywhere — tells a synthesized
    // member of a workspace type (owner defined here) from a truly external one.
    let mut defined_global_keys: HashSet<SymbolKey> = HashSet::new();

    for doc in docs {
        let facts = doc_facts
            .get(&doc.uri)
            .copied()
            .unwrap_or_else(DocFacts::workspace_source);
        for occ in &doc.occurrences {
            if occ.role == Role::Definition && !occ.key.is_local() {
                defined_global_keys.insert(occ.key.clone());
            }
            if let Some(&gi) = groups.rename_group_index.get(&occ.key) {
                if occ.role == Role::Definition {
                    has_definition[gi] = true;
                }
                if facts.generated {
                    has_generated[gi] = true;
                }
                if facts.readonly {
                    has_readonly[gi] = true;
                }
                if facts.is_dependency_source {
                    has_dependency[gi] = true;
                }
                if facts.editable() {
                    editable_count[gi] += 1;
                }
            }
        }
    }

    let owner_defined_in_workspace = |group: &HashSet<SymbolKey>| -> bool {
        group.iter().any(|k| {
            !k.is_local()
                && symbols::split_last(&k.semantic_symbol)
                    .map(|(owner, _)| defined_global_keys.contains(&SymbolKey::global(&owner)))
                    .unwrap_or(false)
        })
    };

    groups
        .rename_groups
        .iter()
        .enumerate()
        .map(|(gi, group)| {
            let is_local = !group.is_empty() && group.iter().all(|k| k.is_local());
            // No definition of its own: synthetic-only when a synthesized member
            // of a workspace type (owner defined here); truly external otherwise.
            let synthetic_only =
                !is_local && !has_definition[gi] && owner_defined_in_workspace(group);
            let is_external = !has_definition[gi] && !synthetic_only;
            let semantic_mask = groups.rename_group_semantic_mask[gi];
            let has_override_family = semantic_mask & unsafe_reason::OVERRIDE_FAMILY != 0;
            let has_companion = group.iter().any(|k| {
                !k.is_local()
                    && k.semantic_symbol.ends_with('#')
                    && symbols::companion(&k.semantic_symbol)
                        .map(|c| group.contains(&SymbolKey::global(&c)))
                        .unwrap_or(false)
            });
            let mut mask = semantic_mask;
            if is_external {
                mask |= unsafe_reason::EXTERNAL;
            }
            if synthetic_only {
                mask |= unsafe_reason::SYNTHETIC_ONLY;
            }
            if has_generated[gi] {
                mask |= unsafe_reason::GENERATED_OCCURRENCE;
            }
            if has_readonly[gi] {
                mask |= unsafe_reason::READONLY_OCCURRENCE;
            }
            if has_dependency[gi] {
                mask |= unsafe_reason::DEPENDENCY_SOURCE;
            }
            RenameProfile {
                is_local,
                is_external,
                has_generated_occurrences: has_generated[gi],
                has_readonly_occurrences: has_readonly[gi],
                has_override_family,
                has_companion,
                editable_occurrence_count: editable_count[gi],
                unsafe_reason_mask: mask,
            }
        })
        .collect()
}
