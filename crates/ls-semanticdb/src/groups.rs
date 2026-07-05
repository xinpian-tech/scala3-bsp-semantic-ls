//! Exact alias groups for one ingest batch.
//!
//! `ref_groups` drive `textDocument/references`; `rename_groups` drive
//! cross-file rename. In the v1 policy the two partitions are identical, but they
//! are separate fields because they are expected to diverge (rename must stay
//! conservative while references can merge more). Group vectors are deterministic:
//! ordered by their minimal member under `(semantic_symbol, local_doc)` ordering.

use std::collections::{HashMap, HashSet};

use ls_index_model::{
    sym_props, unsafe_reason, NormalizedDocument, Role, SymKind, SymbolInfo, SymbolKey,
};

use crate::symbols::{self, Descriptor};

/// Exact alias groups plus the symbol -> group-index maps and the pure-semantic
/// unsafe mask per rename group.
#[derive(PartialEq, Eq)]
pub struct AliasGroups {
    pub ref_groups: Vec<HashSet<SymbolKey>>,
    pub ref_group_index: HashMap<SymbolKey, usize>,
    pub rename_groups: Vec<HashSet<SymbolKey>>,
    pub rename_group_index: HashMap<SymbolKey, usize>,
    /// Pure-semantic unsafe bits per rename group, computable without document
    /// facts (`OverrideFamily`, `UnsupportedSymbolFamily`, `OpaqueType`);
    /// doc-dependent bits are added by the rename-profile builder.
    pub rename_group_semantic_mask: Vec<u64>,
}

impl AliasGroups {
    pub fn ref_group_of(&self, key: &SymbolKey) -> Option<usize> {
        self.ref_group_index.get(key).copied()
    }
    pub fn rename_group_of(&self, key: &SymbolKey) -> Option<usize> {
        self.rename_group_index.get(key).copied()
    }
}

/// Sort key mirroring the Scala `(semanticSymbol, localDoc.fold(-1L)(_.value))`.
fn ord_val(k: &SymbolKey) -> i64 {
    k.local_doc.map(|d| d.value() as i64).unwrap_or(-1)
}

/// Builds EXACT alias groups by union-find over every `SymbolKey` seen in the
/// batch (symbols and occurrences), applying the v1 merge policy.
pub fn build(docs: &[NormalizedDocument]) -> AliasGroups {
    // 1. Key universe + first-wins SymbolInfo lookup.
    let mut key_set: HashSet<SymbolKey> = HashSet::new();
    let mut info_by_key: HashMap<SymbolKey, SymbolInfo> = HashMap::new();
    for doc in docs {
        for s in &doc.symbols {
            key_set.insert(s.key.clone());
            info_by_key
                .entry(s.key.clone())
                .or_insert_with(|| s.clone());
        }
        for o in &doc.occurrences {
            key_set.insert(o.key.clone());
        }
    }

    // Constructors imply their class symbol: synthesize the class key so `new X`
    // occurrences group with `X#` even when the class is otherwise absent.
    let mut implied: Vec<SymbolKey> = Vec::new();
    for key in &key_set {
        if !key.is_local() && symbols::is_constructor(&key.semantic_symbol) {
            if let Some((owner, _)) = symbols::split_last(&key.semantic_symbol) {
                if owner.ends_with('#') {
                    implied.push(SymbolKey::global(owner));
                }
            }
        }
    }
    key_set.extend(implied);

    let mut keys: Vec<SymbolKey> = key_set.into_iter().collect();
    keys.sort_by(|a, b| {
        a.semantic_symbol
            .cmp(&b.semantic_symbol)
            .then_with(|| ord_val(a).cmp(&ord_val(b)))
    });
    let index_of: HashMap<SymbolKey, usize> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), i))
        .collect();

    let mut uf = UnionFind::new(keys.len());

    let union_with_global = |uf: &mut UnionFind, from: usize, to_symbol: &str| {
        if let Some(&to) = index_of.get(&SymbolKey::global(to_symbol)) {
            uf.union(from, to);
        }
    };

    // 2. Merge policy over global keys.
    for (i, key) in keys.iter().enumerate() {
        if key.is_local() {
            continue;
        }
        let sym = &key.semantic_symbol;
        match symbols::split_last(sym) {
            Some((owner, Descriptor::Method(name, _))) => {
                if name == symbols::CONSTRUCTOR_NAME {
                    if owner.ends_with('#') {
                        union_with_global(&mut uf, i, &owner);
                    }
                } else if name == "apply" || name == "unapply" {
                    if owner.ends_with('.') {
                        if let Some(cls) = symbols::companion(&owner) {
                            if index_of.contains_key(&SymbolKey::global(&cls)) {
                                union_with_global(&mut uf, i, &cls);
                            }
                        }
                    }
                } else if name.chars().count() > 2 && name.ends_with("_=") {
                    let mut base_src = name.clone();
                    base_src.truncate(base_src.len() - 2);
                    let base = symbols::encode_name(&base_src);
                    union_with_global(&mut uf, i, &format!("{owner}{base}()."));
                    union_with_global(&mut uf, i, &format!("{owner}{base}."));
                }
            }
            Some((owner, Descriptor::Type(name))) => {
                union_with_global(
                    &mut uf,
                    i,
                    &format!("{owner}{}.", symbols::encode_name(&name)),
                );
            }
            _ => {}
        }
    }

    // 2b. Export forwarders. A Scala 3 `export B.m` synthesizes a forwarder
    // method `A.m` with NO definition occurrence, while the real `B.m` has one.
    // Union a definition-less Method into the UNIQUE definition-having method
    // that shares its descriptor under a different owner, and flag the group.
    let defined_keys: HashSet<SymbolKey> = docs
        .iter()
        .flat_map(|d| d.occurrences.iter())
        .filter(|o| o.role == Role::Definition)
        .map(|o| o.key.clone())
        .collect();

    // definition-having methods indexed by their descriptor (name + signature)
    let mut defined_method_by_descriptor: HashMap<String, Vec<SymbolKey>> = HashMap::new();
    for k in &keys {
        if k.is_local() || !defined_keys.contains(k) {
            continue;
        }
        if info_by_key.get(k).map(|i| i.kind) != Some(SymKind::Method) {
            continue;
        }
        if let Some((_, desc)) = descriptor_split(&k.semantic_symbol) {
            defined_method_by_descriptor
                .entry(desc)
                .or_default()
                .push(k.clone());
        }
    }

    let mut forwarder_keys: HashSet<SymbolKey> = HashSet::new();
    for (i, key) in keys.iter().enumerate() {
        if key.is_local() {
            continue;
        }
        let is_method = info_by_key.get(key).map(|i| i.kind) == Some(SymKind::Method);
        if !is_method || defined_keys.contains(key) {
            continue;
        }
        if let Some((owner, desc)) = descriptor_split(&key.semantic_symbol) {
            // Union ONLY when the descriptor maps to a UNIQUE definition-having
            // method under a different owner; skip on ambiguous or absent match.
            let mut candidates: Vec<SymbolKey> = defined_method_by_descriptor
                .get(&desc)
                .map(|v| v.as_slice())
                .unwrap_or(&[])
                .iter()
                .filter(|orig| {
                    descriptor_split(&orig.semantic_symbol)
                        .map(|(o, _)| o != owner)
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            candidates.dedup();
            if let [orig] = candidates.as_slice() {
                forwarder_keys.insert(key.clone());
                uf.union(i, index_of[orig]);
            }
        }
    }

    // 3. Assemble groups; iteration over sorted keys keeps output ordered by
    //    minimal member.
    let mut order: Vec<usize> = Vec::new();
    let mut buckets: HashMap<usize, Vec<SymbolKey>> = HashMap::new();
    for (i, key) in keys.iter().enumerate() {
        let root = uf.find(i);
        if !buckets.contains_key(&root) {
            order.push(root);
        }
        buckets.entry(root).or_default().push(key.clone());
    }
    let groups: Vec<HashSet<SymbolKey>> = order
        .iter()
        .map(|r| buckets[r].iter().cloned().collect())
        .collect();
    let mut group_index: HashMap<SymbolKey, usize> = HashMap::new();
    for (gi, g) in groups.iter().enumerate() {
        for k in g {
            group_index.insert(k.clone(), gi);
        }
    }

    // 4. OverrideFamily / UnsupportedSymbolFamily / OpaqueType semantic mask.
    let overridden_targets: HashSet<SymbolKey> = info_by_key
        .values()
        .flat_map(|i| i.overridden_symbols.iter())
        .map(SymbolKey::global)
        .collect();
    let semantic_mask: Vec<u64> = groups
        .iter()
        .map(|g| {
            let mut mask = 0u64;
            let override_flagged = g.iter().any(|k| {
                info_by_key
                    .get(k)
                    .map(|i| !i.overridden_symbols.is_empty())
                    .unwrap_or(false)
                    || overridden_targets.contains(k)
            });
            if override_flagged {
                mask |= unsafe_reason::OVERRIDE_FAMILY;
            }
            if g.iter().any(|k| forwarder_keys.contains(k)) {
                mask |= unsafe_reason::UNSUPPORTED_SYMBOL_FAMILY;
            }
            if g.iter().any(|k| {
                info_by_key
                    .get(k)
                    .map(|i| i.properties & sym_props::OPAQUE != 0)
                    .unwrap_or(false)
            }) {
                mask |= unsafe_reason::OPAQUE_TYPE;
            }
            mask
        })
        .collect();

    AliasGroups {
        ref_groups: groups.clone(),
        ref_group_index: group_index.clone(),
        rename_groups: groups,
        rename_group_index: group_index,
        rename_group_semantic_mask: semantic_mask,
    }
}

/// `splitLast(sym).map((owner, _) => (owner, sym.substring(owner.length)))` —
/// the owner prefix plus the raw descriptor suffix (name + signature).
fn descriptor_split(sym: &str) -> Option<(String, String)> {
    symbols::split_last(sym).map(|(owner, _)| {
        let owner_chars = owner.chars().count();
        let desc: String = sym.chars().skip(owner_chars).collect();
        (owner, desc)
    })
}

/// Classic union-find with path compression and union by rank.
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u32>,
}

impl UnionFind {
    fn new(size: usize) -> Self {
        UnionFind {
            parent: (0..size).collect(),
            rank: vec![0; size],
        }
    }

    fn find(&mut self, i: usize) -> usize {
        let mut root = i;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut cur = i;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                self.parent[rb] = ra;
                self.rank[ra] += 1;
            }
        }
    }
}
