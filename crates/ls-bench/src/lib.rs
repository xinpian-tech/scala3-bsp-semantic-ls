//! Benchmark harness over the real storage layer.
//!
//! Generates a synthetic multi-target SemanticDB corpus on disk (sources plus
//! `.semanticdb` files), runs the production full-generation ingest, and times
//! the production read paths: the references group fan-out over the snapshot,
//! workspace-symbol search, and doc-postings symbol-at-cursor. Every measured
//! operation is cross-checked against generated ground truth and any
//! inconsistency fails the run — a benchmark that answers wrongly measures
//! nothing.
//!
//! Modes: `smoke` (small corpus, the CI gate), `tiny` (harness self-check, run
//! by the crate's own test), `full` (bigger corpus for real measurements).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ls_engine::{QueryOrchestrator, ReferencesEngine, TargetSpec, WorkspaceTargets};
use ls_semanticdb::{md5, SdbDocument, SdbOccurrence, SdbRange, SdbSymbolInfo};
use ls_store::Store;

/// Corpus shape: `targets` build targets in a linear dependency chain (target
/// `t` depends on `t-1`), each with `docs` documents of `methods` method
/// definitions plus `probe_refs` references to the probe symbol defined in
/// target 0, doc 0.
#[derive(Clone, Copy, Debug)]
pub struct BenchConfig {
    pub name: &'static str,
    pub targets: usize,
    pub docs: usize,
    pub methods: usize,
    pub probe_refs: usize,
    pub query_iters: usize,
}

impl BenchConfig {
    pub fn tiny() -> Self {
        BenchConfig {
            name: "tiny",
            targets: 2,
            docs: 3,
            methods: 4,
            probe_refs: 2,
            query_iters: 5,
        }
    }
    pub fn smoke() -> Self {
        BenchConfig {
            name: "smoke",
            targets: 3,
            docs: 20,
            methods: 10,
            probe_refs: 5,
            query_iters: 50,
        }
    }
    pub fn full() -> Self {
        BenchConfig {
            name: "full",
            targets: 4,
            docs: 150,
            methods: 20,
            probe_refs: 10,
            query_iters: 200,
        }
    }
}

// ---- protobuf encoder (subset of scalameta semanticdb.proto) ----

#[derive(Default)]
struct ProtoWriter {
    out: Vec<u8>,
}

impl ProtoWriter {
    fn raw_varint(&mut self, value: u64) {
        let mut v = value;
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.out.push(b);
                break;
            }
            self.out.push(b | 0x80);
        }
    }
    fn tag(&mut self, field: u32, wire: u32) {
        self.raw_varint(((field as u64) << 3) | wire as u64);
    }
    fn varint_field(&mut self, field: u32, value: u64) {
        self.tag(field, 0);
        self.raw_varint(value);
    }
    fn int32_field(&mut self, field: u32, value: i32) {
        self.varint_field(field, value as i64 as u64);
    }
    fn bytes_field(&mut self, field: u32, data: &[u8]) {
        self.tag(field, 2);
        self.raw_varint(data.len() as u64);
        self.out.extend_from_slice(data);
    }
    fn string_field(&mut self, field: u32, value: &str) {
        self.bytes_field(field, value.as_bytes());
    }
    fn message_field(&mut self, field: u32, build: impl FnOnce(&mut ProtoWriter)) {
        let mut nested = ProtoWriter::default();
        build(&mut nested);
        self.bytes_field(field, &nested.out);
    }
}

fn encode(docs: &[SdbDocument]) -> Vec<u8> {
    let mut w = ProtoWriter::default();
    for d in docs {
        w.message_field(1, |mw| {
            mw.varint_field(1, d.schema as i64 as u64);
            mw.string_field(2, &d.uri);
            mw.string_field(3, &d.text);
            mw.string_field(11, &d.md5);
            mw.varint_field(10, d.language_code as i64 as u64);
            for s in &d.symbols {
                mw.message_field(5, |sw| {
                    sw.string_field(1, &s.symbol);
                    sw.varint_field(3, s.kind_code as i64 as u64);
                    sw.int32_field(4, s.properties);
                    sw.string_field(5, &s.display_name);
                    for o in &s.overridden_symbols {
                        sw.string_field(19, o);
                    }
                });
            }
            for o in &d.occurrences {
                mw.message_field(6, |ow| {
                    if let Some(r) = &o.range {
                        ow.message_field(1, |rw| {
                            rw.int32_field(1, r.start_line);
                            rw.int32_field(2, r.start_character);
                            rw.int32_field(3, r.end_line);
                            rw.int32_field(4, r.end_character);
                        });
                    }
                    ow.string_field(2, &o.symbol);
                    ow.varint_field(3, o.role_code as i64 as u64);
                });
            }
        });
    }
    w.out
}

const REFERENCE: i32 = 1;
const DEFINITION: i32 = 2;
const KIND_METHOD: i32 = 3;
const KIND_OBJECT: i32 = 12;

// ---- corpus generation ----

fn owner_name(target: usize, doc: usize) -> String {
    format!("Owner{target}x{doc}")
}

fn method_name(target: usize, doc: usize, method: usize) -> String {
    format!("bm{method}d{doc}t{target}")
}

fn method_symbol(target: usize, doc: usize, method: usize) -> String {
    format!(
        "bench/{}.{}().",
        owner_name(target, doc),
        method_name(target, doc, method)
    )
}

fn probe_name() -> String {
    method_name(0, 0, 0)
}

fn probe_symbol() -> String {
    method_symbol(0, 0, 0)
}

fn doc_uri(target: usize, doc: usize) -> String {
    format!("t{target}/Doc{doc}.scala")
}

/// One generated document: the owner-object definition line, `methods` method
/// definition lines, then `probe_refs` reference lines naming the probe symbol
/// (skipped in the probe's own defining document). Every line holds exactly one
/// token at column 0, so occurrence spans and rename-token checks line up by
/// construction.
fn generate_doc(cfg: &BenchConfig, target: usize, doc: usize) -> (String, SdbDocument) {
    let mut source = String::new();
    let mut symbols: Vec<SdbSymbolInfo> = Vec::new();
    let mut occurrences: Vec<SdbOccurrence> = Vec::new();
    let mut line: i32 = 0;

    let mut push_line = |source: &mut String, token: &str| -> i32 {
        let l = line;
        writeln!(source, "{token}").unwrap();
        line += 1;
        l
    };

    let owner = owner_name(target, doc);
    let owner_symbol = format!("bench/{owner}.");
    let l = push_line(&mut source, &owner);
    symbols.push(SdbSymbolInfo {
        symbol: owner_symbol.clone(),
        kind_code: KIND_OBJECT,
        properties: 0,
        display_name: owner.clone(),
        overridden_symbols: Vec::new(),
    });
    occurrences.push(occurrence(l, &owner, &owner_symbol, DEFINITION));

    for m in 0..cfg.methods {
        let name = method_name(target, doc, m);
        let symbol = method_symbol(target, doc, m);
        let l = push_line(&mut source, &name);
        symbols.push(SdbSymbolInfo {
            symbol: symbol.clone(),
            kind_code: KIND_METHOD,
            properties: 0,
            display_name: name.clone(),
            overridden_symbols: Vec::new(),
        });
        occurrences.push(occurrence(l, &name, &symbol, DEFINITION));
    }

    if !(target == 0 && doc == 0) {
        for _ in 0..cfg.probe_refs {
            let name = probe_name();
            let l = push_line(&mut source, &name);
            occurrences.push(occurrence(l, &name, &probe_symbol(), REFERENCE));
        }
    }

    let uri = doc_uri(target, doc);
    let sdb = SdbDocument {
        schema: 4,
        uri: uri.clone(),
        text: String::new(),
        md5: md5::compute_hex(&source),
        language_code: 1,
        symbols,
        occurrences,
    };
    (source, sdb)
}

fn occurrence(line: i32, token: &str, symbol: &str, role: i32) -> SdbOccurrence {
    SdbOccurrence {
        range: Some(SdbRange {
            start_line: line,
            start_character: 0,
            end_line: line,
            end_character: token.len() as i32,
        }),
        symbol: symbol.into(),
        role_code: role,
    }
}

/// Materializes the corpus under `root` and returns the workspace target set
/// (target `t` depends on `t-1`, so the probe target's reverse closure spans
/// the whole workspace).
fn generate_corpus(cfg: &BenchConfig, root: &Path) -> WorkspaceTargets {
    let mut specs = Vec::new();
    for t in 0..cfg.targets {
        let targetroot = root.join(format!("target{t}"));
        let sourceroot = root.join(format!("src{t}"));
        for d in 0..cfg.docs {
            let (source, sdb) = generate_doc(cfg, t, d);
            let src_path = sourceroot.join(&sdb.uri);
            fs::create_dir_all(src_path.parent().unwrap()).unwrap();
            fs::write(&src_path, &source).unwrap();
            let sdb_path = targetroot
                .join("META-INF/semanticdb")
                .join(format!("{}.semanticdb", sdb.uri));
            fs::create_dir_all(sdb_path.parent().unwrap()).unwrap();
            fs::write(&sdb_path, encode(std::slice::from_ref(&sdb))).unwrap();
        }
        let mut spec = TargetSpec::new(format!("bench://t{t}"), targetroot, sourceroot);
        if t > 0 {
            spec = spec.with_deps(vec![format!("bench://t{}", t - 1)]);
        }
        specs.push(spec);
    }
    WorkspaceTargets::new(specs)
}

// ---- measurement ----

struct Timed {
    label: &'static str,
    iters: usize,
    total_ms: f64,
}

impl Timed {
    fn row(&self) -> String {
        format!(
            "  {:<28} {:>8.3} ms/op   ({} iters, {:.1} ms total)",
            self.label,
            self.total_ms / self.iters.max(1) as f64,
            self.iters,
            self.total_ms
        )
    }
}

fn time<T>(label: &'static str, iters: usize, mut op: impl FnMut(usize) -> T) -> (Timed, T) {
    let t0 = Instant::now();
    let mut last = op(0);
    for i in 1..iters {
        last = op(i);
    }
    let total_ms = t0.elapsed().as_secs_f64() * 1e3;
    (
        Timed {
            label,
            iters,
            total_ms,
        },
        last,
    )
}

/// A self-cleaning scratch directory for the generated corpus and store.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("ls-bench-{}-{}", std::process::id(), n));
        fs::create_dir_all(&path).unwrap();
        ScratchDir { path }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn check(cond: bool, what: impl FnOnce() -> String) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(what())
    }
}

/// Runs one benchmark pass: generate → ingest → measure → cross-check. Returns
/// the printable report, or the first ground-truth inconsistency as `Err`.
pub fn run(cfg: &BenchConfig) -> Result<String, String> {
    let scratch = ScratchDir::new();
    let ws = Arc::new(generate_corpus(cfg, &scratch.path));

    let store = Store::open(&scratch.path.join("store")).map_err(|e| e.to_string())?;
    let orch = QueryOrchestrator::with_defaults(store);

    let t0 = Instant::now();
    let report = orch.ingest(Arc::clone(&ws)).map_err(|e| e.to_string())?;
    let ingest_ms = t0.elapsed().as_secs_f64() * 1e3;

    // Ground truth: corpus counts are exact by construction.
    let expected_docs = cfg.targets * cfg.docs;
    let expected_symbols = expected_docs * (cfg.methods + 1);
    check(report.docs_indexed == expected_docs, || {
        format!(
            "ingest indexed {} docs, corpus has {expected_docs}",
            report.docs_indexed
        )
    })?;
    check(report.symbol_count == expected_symbols, || {
        format!(
            "ingest saw {} symbols, corpus defines {expected_symbols}",
            report.symbol_count
        )
    })?;
    check(
        report.docs_stale == 0 && report.parse_errors.is_empty(),
        || {
            format!(
                "generated corpus must ingest clean (stale={}, parse_errors={})",
                report.docs_stale,
                report.parse_errors.len()
            )
        },
    )?;

    // References: every non-probe doc holds `probe_refs` references, plus the
    // probe's own definition (includeDeclaration). The cursor sits on a probe
    // reference in the LAST target, so the fan-out crosses the whole dependency
    // chain back to the defining target.
    let expected_refs = (expected_docs - 1) * cfg.probe_refs + 1;
    let probe_uri = doc_uri(cfg.targets - 1, cfg.docs - 1);
    let probe_line = (cfg.methods + 1) as u32;
    let engine = ReferencesEngine::new(&orch);
    let (refs_timed, refs_result) = time("references (probe fan-out)", cfg.query_iters, |_| {
        engine.references(&probe_uri, probe_line, 0, true)
    });
    let refs = refs_result.map_err(|e| format!("references failed: {e:?}"))?;
    check(refs.locations().len() == expected_refs, || {
        format!(
            "references returned {} locations, ground truth is {expected_refs}",
            refs.locations().len()
        )
    })?;

    // Symbol-at-cursor over doc postings: rotate through method-definition
    // lines across the corpus.
    let (cursor_timed, cursor_result) = time("symbol-at-cursor", cfg.query_iters, |i| {
        let t = i % cfg.targets;
        let d = i % cfg.docs;
        let m = i % cfg.methods;
        let uri = doc_uri(t, d);
        orch.symbol_at_cursor(&uri, (m + 1) as u32, 0)
            .map(|c| (c.semantic_symbol, t, d, m))
    });
    let (cursor_symbol, t, d, m) =
        cursor_result.map_err(|e| format!("symbol_at_cursor failed: {e:?}"))?;
    check(cursor_symbol == method_symbol(t, d, m), || {
        format!(
            "cursor resolved '{cursor_symbol}', ground truth is '{}'",
            method_symbol(t, d, m)
        )
    })?;

    // Workspace-symbol search: exact query must rank its symbol first; a prefix
    // query must contain it; a never-generated name must not exist.
    let exact = probe_name();
    let (search_timed, search_hits) = time("workspace-symbol search", cfg.query_iters, |i| {
        let m = i % cfg.methods;
        let _ = orch.workspace_symbol(&method_name(0, 0, m), 50);
        orch.workspace_symbol(&exact, 50)
    });
    check(
        search_hits.first().map(|h| h.display.as_str()) == Some(exact.as_str()),
        || format!("exact query '{exact}' did not rank its symbol first"),
    )?;
    let prefix: String = exact.chars().take(exact.len() - 2).collect();
    let prefix_hits = orch.workspace_symbol(&prefix, 200);
    check(prefix_hits.iter().any(|h| h.display == exact), || {
        format!("prefix query '{prefix}' did not surface '{exact}'")
    })?;
    check(orch.workspace_symbol_name_exists(&exact), || {
        format!("name-membership query missed '{exact}'")
    })?;
    check(
        !orch.workspace_symbol_name_exists("neverGeneratedName"),
        || "name-membership query invented a symbol".to_string(),
    )?;

    let mut out = String::new();
    writeln!(
        out,
        "ls-bench {}: {} targets x {} docs x {} methods ({} docs, {} symbols, {} probe refs)",
        cfg.name,
        cfg.targets,
        cfg.docs,
        cfg.methods,
        expected_docs,
        expected_symbols,
        expected_refs
    )
    .unwrap();
    writeln!(
        out,
        "  {:<28} {:>8.3} ms      (single full-generation pass, segment {})",
        "ingest (full generation)", ingest_ms, report.segment_id
    )
    .unwrap();
    for t in [&refs_timed, &cursor_timed, &search_timed] {
        writeln!(out, "{}", t.row()).unwrap();
    }
    writeln!(out, "  ground truth: all cross-checks passed").unwrap();
    Ok(out)
}
