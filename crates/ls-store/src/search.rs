//! Workspace-symbol search — a Rust port of `ls.sqlite.FuzzyRank` plus the
//! deterministic, bounded query layer that replaces FTS5 bm25 (DEC-4).
//!
//! [`normalize`] / [`initials`] / [`score`] mirror `FuzzyRank.scala` exactly.
//! [`SearchIndex`] is built once from a [`SegmentReader`]: it resolves each
//! `search.bin` row's symbol metadata (display / owner / package / kind /
//! def-doc) and indexes the display/owner/package tokens, so
//! [`SearchIndex::workspace_symbol_search`] can match multi-token queries across
//! those fields and rank by the fuzzy tiers, while
//! [`SearchIndex::workspace_symbol_name_exists`] answers exact-name membership.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::reader::SegmentReader;

// Score tiers (higher is better), verbatim from `FuzzyRank`.
const EXACT_BASE: i32 = 1_000_000;
const PREFIX_BASE: i32 = 100_000;
const SUBSEQ_BASE: i32 = 1_000;
const HUMP_BONUS: i32 = 1_000;

/// Upper bound on the single-token fuzzy candidate pull (`MetaStore.FuzzyCandidateCap`):
/// a large corpus never triggers an unbounded subsequence scan.
pub const FUZZY_CANDIDATE_CAP: usize = 5000;

/// Java `Character.toLowerCase(char)` is 1:1; take the first lowercase char so
/// the normalized string stays index-aligned with its hump flags.
fn lower1(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Lowercase, keep only letters/digits (drops separators/punctuation). CJK and
/// other letters are kept (`is_alphanumeric`), matching the FTS tokenizer.
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(lower1)
        .collect()
}

/// Normalized chars plus, per char, whether it starts a camel hump (first alnum,
/// a char after a separator, an uppercase after lowercase/digit, or a digit
/// after a letter). The two vectors are index-aligned.
fn normalized_with_humps(s: &str) -> (Vec<char>, Vec<bool>) {
    let mut nn = Vec::new();
    let mut humps = Vec::new();
    let (mut prev_alnum, mut prev_upper, mut prev_digit) = (false, false, false);
    for c in s.chars() {
        if c.is_alphanumeric() {
            let upper = c.is_uppercase();
            let digit = c.is_numeric();
            let hump = !prev_alnum || (upper && !prev_upper) || (digit && !prev_digit);
            nn.push(lower1(c));
            humps.push(hump);
            prev_alnum = true;
            prev_upper = upper;
            prev_digit = digit;
        } else {
            prev_alnum = false;
            prev_upper = false;
            prev_digit = false;
        }
    }
    (nn, humps)
}

/// Camel-hump initials, lowercased. e.g. `workspaceSymbol` -> `ws`.
pub fn initials(s: &str) -> String {
    let (nn, humps) = normalized_with_humps(s);
    nn.iter()
        .zip(humps)
        .filter_map(|(c, h)| h.then_some(*c))
        .collect()
}

/// Fuzzy score of `query` against `name`, or `None` when normalized `query` is
/// not even a subsequence of normalized `name` (or either normalizes to empty).
/// Exact and prefix are the top tiers; otherwise the score is dominated by the
/// number of query chars landing on camel-hump starts, minus name length.
pub fn score(query: &str, name: &str) -> Option<i32> {
    let nq: Vec<char> = normalize(query).chars().collect();
    if nq.is_empty() {
        return None;
    }
    let (nn, humps) = normalized_with_humps(name);
    if nn.is_empty() {
        return None;
    }
    let len = nn.len() as i32;
    if nn == nq {
        Some(EXACT_BASE - len)
    } else if nn.len() >= nq.len() && nn[..nq.len()] == nq[..] {
        Some(PREFIX_BASE - len)
    } else {
        max_hump_hits(&nq, &nn, &humps).map(|hits| SUBSEQ_BASE + hits * HUMP_BONUS - len)
    }
}

/// Max query chars matchable at camel-hump positions over any subsequence
/// embedding of `nq` in `nn`, or `None` when `nq` is not a subsequence. DP over
/// (query index, name index), matching `FuzzyRank.maxHumpHits`.
fn max_hump_hits(nq: &[char], nn: &[char], humps: &[bool]) -> Option<i32> {
    let n = nn.len();
    const NEG: i32 = i32::MIN / 4;
    let mut next = vec![0i32; n + 1]; // f(q, j) = 0: empty query fully matched
    for &qc in nq.iter().rev() {
        let mut cur = vec![NEG; n + 1]; // f(i, n) = NEG: query left, name exhausted
        for j in (0..n).rev() {
            let mut best = cur[j + 1]; // skip nn(j)
            if nn[j] == qc {
                let sub = next[j + 1];
                if sub > NEG {
                    let cand = i32::from(humps[j]) + sub;
                    if cand > best {
                        best = cand;
                    }
                }
            }
            cur[j] = best;
        }
        next = cur;
    }
    let res = next[0];
    (res > NEG).then_some(res)
}

/// A resolved workspace-symbol hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSymbolHit {
    pub symbol_ord: u32,
    pub display: String,
    pub owner: String,
    pub package_name: String,
    pub kind: i32,
    /// Defining document ordinal (`-1` = unknown).
    pub def_doc_ord: i32,
}

struct Row {
    symbol_ord: u32,
    display: String,
    owner: String,
    package_name: String,
    kind: i32,
    def_doc_ord: i32,
}

/// An in-memory workspace-symbol index built once from a segment.
pub struct SearchIndex {
    rows: Vec<Row>,
    /// Sorted `(field_token, row)` over display/owner/package tokens.
    tokens: Vec<(String, u32)>,
    /// Sorted `(normalize(display), row)` for the single-token fuzzy pull.
    norm_sorted: Vec<(String, u32)>,
    /// Sorted `(initials(display), row)` for the single-token fuzzy pull.
    init_sorted: Vec<(String, u32)>,
    /// Exact display-name membership.
    display_set: HashSet<String>,
    last_fuzzy_candidate_count: AtomicUsize,
}

/// Split `s` into lowercased maximal alphanumeric runs (the FTS tokenizer).
fn field_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            cur.push(lower1(c));
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Rows whose sorted key starts with `prefix`, inserted into `out`; stops once
/// `out` reaches `cap`.
fn fill_prefix(sorted: &[(String, u32)], prefix: &str, out: &mut HashSet<u32>, cap: usize) {
    let start = sorted.partition_point(|(k, _)| k.as_str() < prefix);
    for (k, r) in &sorted[start..] {
        if !k.starts_with(prefix) || out.len() >= cap {
            break;
        }
        out.insert(*r);
    }
}

impl SearchIndex {
    /// Build the index from an opened segment.
    pub fn build(reader: &SegmentReader) -> SearchIndex {
        let n = reader.search_row_count().max(0) as usize;
        let mut rows = Vec::with_capacity(n);
        let mut tokens = Vec::new();
        let mut norm_sorted = Vec::new();
        let mut init_sorted = Vec::new();
        let mut display_set = HashSet::new();
        for i in 0..n {
            let (_normalized, sym) = reader.search_row(i as i64);
            if sym < 0 || sym as usize >= reader.symbol_count() {
                continue;
            }
            let meta = reader.symbol_meta(sym as u32);
            let row = rows.len() as u32;
            for field in [&meta.display, &meta.owner, &meta.package_name] {
                for tok in field_tokens(field) {
                    tokens.push((tok, row));
                }
            }
            let norm = normalize(&meta.display);
            if !norm.is_empty() {
                norm_sorted.push((norm, row));
            }
            let init = initials(&meta.display);
            if !init.is_empty() {
                init_sorted.push((init, row));
            }
            if !meta.display.is_empty() {
                display_set.insert(meta.display.clone());
            }
            rows.push(Row {
                symbol_ord: sym as u32,
                display: meta.display,
                owner: meta.owner,
                package_name: meta.package_name,
                kind: meta.kind,
                def_doc_ord: meta.def_doc_ord,
            });
        }
        tokens.sort();
        norm_sorted.sort();
        init_sorted.sort();
        SearchIndex {
            rows,
            tokens,
            norm_sorted,
            init_sorted,
            display_set,
            last_fuzzy_candidate_count: AtomicUsize::new(0),
        }
    }

    /// Number of candidates pulled by the most recent single-token fuzzy
    /// fallback (never exceeds [`FUZZY_CANDIDATE_CAP`]). Test seam proving the
    /// pull stays bounded.
    pub fn last_fuzzy_candidate_count(&self) -> usize {
        self.last_fuzzy_candidate_count.load(Ordering::Relaxed)
    }

    /// Whether an active workspace symbol has this EXACT display name — a direct
    /// membership check, never a ranked-search proxy, so a name outside a search
    /// window is not missed. Empty name -> false.
    pub fn workspace_symbol_name_exists(&self, display_name: &str) -> bool {
        !display_name.is_empty() && self.display_set.contains(display_name)
    }

    /// Ranked workspace-symbol search. Whitespace/punctuation query tokens must
    /// each prefix-match some display/owner/package token (AND across query
    /// tokens); a single-token query additionally pulls bounded camel-hump /
    /// subsequence candidates. Results are ranked by the fuzzy tiers over the
    /// display name (owner/package-only matches ranked last), with a stable final
    /// tie-break by `symbol_ord`, then truncated to `limit`.
    pub fn workspace_symbol_search(&self, query: &str, limit: usize) -> Vec<WorkspaceSymbolHit> {
        self.last_fuzzy_candidate_count.store(0, Ordering::Relaxed);
        let qtokens = field_tokens(query);
        if qtokens.is_empty() || limit == 0 {
            return Vec::new();
        }

        // Prefix-AND across field tokens: the FTS-equivalent match set.
        let mut per_token = qtokens.iter().map(|t| {
            let mut set = HashSet::new();
            fill_prefix(&self.tokens, t, &mut set, usize::MAX);
            set
        });
        let mut prefix_and = per_token.next().unwrap();
        for set in per_token {
            prefix_and.retain(|r| set.contains(r));
        }

        // `cand[row] = score(query, display)`; prefix-AND matches are always in.
        let mut cand: HashMap<u32, Option<i32>> = HashMap::new();
        for &r in &prefix_and {
            cand.insert(r, score(query, &self.rows[r as usize].display));
        }

        // Single identifier token: bounded fuzzy fallback, kept only if the query
        // is a subsequence of the display (so the match set is not widened).
        if qtokens.len() == 1 {
            let first: String = qtokens[0].chars().next().unwrap().to_string();
            let mut fuzzy = HashSet::new();
            fill_prefix(&self.norm_sorted, &first, &mut fuzzy, FUZZY_CANDIDATE_CAP);
            fill_prefix(&self.init_sorted, &first, &mut fuzzy, FUZZY_CANDIDATE_CAP);
            self.last_fuzzy_candidate_count
                .store(fuzzy.len(), Ordering::Relaxed);
            for r in fuzzy {
                if let std::collections::hash_map::Entry::Vacant(e) = cand.entry(r) {
                    if let Some(s) = score(query, &self.rows[r as usize].display) {
                        e.insert(Some(s));
                    }
                }
            }
        }

        // Deterministic order: Some-scored before None, higher score first, then
        // a stable tie-break by symbol_ord.
        let mut ranked: Vec<(u32, Option<i32>)> = cand.into_iter().collect();
        ranked.sort_by_key(|&(r, s)| {
            (
                s.is_none(),
                std::cmp::Reverse(s.unwrap_or(i32::MIN)),
                self.rows[r as usize].symbol_ord,
            )
        });
        ranked
            .into_iter()
            .take(limit)
            .map(|(r, _)| self.hit(r))
            .collect()
    }

    fn hit(&self, row: u32) -> WorkspaceSymbolHit {
        let r = &self.rows[row as usize];
        WorkspaceSymbolHit {
            symbol_ord: r.symbol_ord,
            display: r.display.clone(),
            owner: r.owner.clone(),
            package_name: r.package_name.clone(),
            kind: r.kind,
            def_doc_ord: r.def_doc_ord,
        }
    }
}

impl SegmentReader {
    /// Build the in-memory workspace-symbol [`SearchIndex`] for this segment.
    pub fn build_search_index(&self) -> SearchIndex {
        SearchIndex::build(self)
    }
}
