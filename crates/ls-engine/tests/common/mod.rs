//! Test harness: a tiny SemanticDB protobuf encoder plus a fixture builder that
//! materializes `.semanticdb` files + matching sources on disk, so the engines
//! can be driven over fully controlled workspaces (fresh / stale / multi-target
//! / shared-source), the Rust analogue of the Scala `FixtureWorkspace`.

#![allow(dead_code)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ls_engine::{CompileOutcome, CompileService, DirtyBufferOverlay, OverlayHit};
use ls_index_model::{Loc, Role, Span};
use ls_semanticdb::{md5, SdbDocument, SdbOccurrence, SdbRange, SdbSymbolInfo};

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

fn write_range(w: &mut ProtoWriter, r: &SdbRange) {
    w.int32_field(1, r.start_line);
    w.int32_field(2, r.start_character);
    w.int32_field(3, r.end_line);
    w.int32_field(4, r.end_character);
}

fn write_symbol(w: &mut ProtoWriter, s: &SdbSymbolInfo) {
    w.string_field(1, &s.symbol);
    w.varint_field(3, s.kind_code as i64 as u64);
    w.int32_field(4, s.properties);
    w.string_field(5, &s.display_name);
    for o in &s.overridden_symbols {
        w.string_field(19, o);
    }
}

fn write_occurrence(w: &mut ProtoWriter, o: &SdbOccurrence) {
    if let Some(r) = &o.range {
        w.message_field(1, |mw| write_range(mw, r));
    }
    w.string_field(2, &o.symbol);
    w.varint_field(3, o.role_code as i64 as u64);
}

fn write_document(w: &mut ProtoWriter, d: &SdbDocument) {
    w.varint_field(1, d.schema as i64 as u64);
    w.string_field(2, &d.uri);
    w.string_field(3, &d.text);
    w.string_field(11, &d.md5);
    w.varint_field(10, d.language_code as i64 as u64);
    for s in &d.symbols {
        w.message_field(5, |mw| write_symbol(mw, s));
    }
    for o in &d.occurrences {
        w.message_field(6, |mw| write_occurrence(mw, o));
    }
}

fn encode(docs: &[SdbDocument]) -> Vec<u8> {
    let mut w = ProtoWriter::default();
    for d in docs {
        w.message_field(1, |mw| write_document(mw, d));
    }
    w.out
}

// ---- constructors ----

pub fn rng(sl: i32, sc: i32, el: i32, ec: i32) -> SdbRange {
    SdbRange {
        start_line: sl,
        start_character: sc,
        end_line: el,
        end_character: ec,
    }
}

pub fn sym(symbol: &str, kind: i32, props: i32, display: &str) -> SdbSymbolInfo {
    SdbSymbolInfo {
        symbol: symbol.into(),
        kind_code: kind,
        properties: props,
        display_name: display.into(),
        overridden_symbols: Vec::new(),
    }
}

pub fn occ(range: SdbRange, symbol: &str, role: i32) -> SdbOccurrence {
    SdbOccurrence {
        range: Some(range),
        symbol: symbol.into(),
        role_code: role,
    }
}

pub const REFERENCE: i32 = 1;
pub const DEFINITION: i32 = 2;
pub const KIND_METHOD: i32 = 3;
pub const KIND_CLASS: i32 = 13;

// ---- fixture builder ----

pub struct DocFixture {
    pub uri: String,
    pub source: String,
    pub md5_override: Option<String>,
    pub symbols: Vec<SdbSymbolInfo>,
    pub occurrences: Vec<SdbOccurrence>,
}

impl DocFixture {
    pub fn new(uri: &str, source: &str) -> Self {
        DocFixture {
            uri: uri.into(),
            source: source.into(),
            md5_override: None,
            symbols: Vec::new(),
            occurrences: Vec::new(),
        }
    }
    pub fn symbol(mut self, s: SdbSymbolInfo) -> Self {
        self.symbols.push(s);
        self
    }
    pub fn occurrence(mut self, o: SdbOccurrence) -> Self {
        self.occurrences.push(o);
        self
    }
}

/// Writes one doc's `.semanticdb` + source file under an existing target root.
pub fn write_doc(targetroot: &Path, sourceroot: &Path, d: &DocFixture) {
    let src = sourceroot.join(&d.uri);
    fs::create_dir_all(src.parent().unwrap()).unwrap();
    fs::write(&src, &d.source).unwrap();

    let md5v = d
        .md5_override
        .clone()
        .unwrap_or_else(|| md5::compute_hex(&d.source));
    let sdb = SdbDocument {
        schema: 4,
        uri: d.uri.clone(),
        text: String::new(),
        md5: md5v,
        language_code: 1,
        symbols: d.symbols.clone(),
        occurrences: d.occurrences.clone(),
    };
    let bytes = encode(&[sdb]);
    let file = targetroot
        .join("META-INF/semanticdb")
        .join(format!("{}.semanticdb", d.uri));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, bytes).unwrap();
}

/// A self-cleaning temp directory.
pub struct TempDir {
    pub path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("ls-engine-{}-{}-{}", tag, std::process::id(), n));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
    pub fn sub(&self, name: &str) -> PathBuf {
        let p = self.path.join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ---- compile stubs ----

pub struct OkCompiler;
impl CompileService for OkCompiler {
    fn compile(&self, _targets: &[String]) -> CompileOutcome {
        CompileOutcome::Ok
    }
}

pub struct FailCompiler;
impl CompileService for FailCompiler {
    fn compile(&self, _targets: &[String]) -> CompileOutcome {
        CompileOutcome::Failed {
            reason: "compile failed".into(),
        }
    }
}

/// A compiler that records the exact target list it was asked to compile.
#[derive(Default)]
pub struct RecordingCompiler {
    seen: std::sync::Mutex<Option<Vec<String>>>,
}

impl RecordingCompiler {
    pub fn recorded(&self) -> Option<Vec<String>> {
        self.seen.lock().unwrap().clone()
    }
}

impl CompileService for RecordingCompiler {
    fn compile(&self, targets: &[String]) -> CompileOutcome {
        *self.seen.lock().unwrap() = Some(targets.to_vec());
        CompileOutcome::Ok
    }
}

/// Writes a corrupt `.semanticdb` (a length-delimited field whose declared
/// length exceeds the buffer), for malformed-ingest tests.
pub fn write_corrupt(targetroot: &Path, uri: &str) {
    let file = targetroot
        .join("META-INF/semanticdb")
        .join(format!("{uri}.semanticdb"));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    // tag=field1/wire2, then a length varint of 127 with no payload → truncated.
    fs::write(&file, [0x0A, 0x7F]).unwrap();
}

/// Writes only a source file (no `.semanticdb`), for no-SemanticDB tests.
pub fn write_source_only(sourceroot: &Path, uri: &str, source: &str) {
    let src = sourceroot.join(uri);
    fs::create_dir_all(src.parent().unwrap()).unwrap();
    fs::write(&src, source).unwrap();
}

// ---- test overlay ----

pub struct TestOverlay {
    pub dirty: HashSet<String>,
    pub hit: Option<OverlayHit>,
    pub occurrences: Vec<(String, Loc)>,
    pub contributes: bool,
}

impl TestOverlay {
    pub fn dirty(uri: &str, hit: Option<OverlayHit>) -> Self {
        let mut dirty = HashSet::new();
        dirty.insert(uri.to_string());
        TestOverlay {
            dirty,
            hit,
            occurrences: Vec::new(),
            contributes: false,
        }
    }
}

impl DirtyBufferOverlay for TestOverlay {
    fn is_dirty(&self, uri: &str) -> bool {
        self.dirty.contains(uri)
    }
    fn symbol_at(&self, uri: &str, _line: u32, _character: u32) -> Option<OverlayHit> {
        if self.dirty.contains(uri) {
            self.hit.clone()
        } else {
            None
        }
    }
    fn occurrences_of(&self, semantic_symbol: &str) -> Option<Vec<Loc>> {
        let hits: Vec<Loc> = self
            .occurrences
            .iter()
            .filter(|(s, _)| s == semantic_symbol)
            .map(|(_, l)| l.clone())
            .collect();
        if hits.is_empty() {
            None
        } else {
            Some(hits)
        }
    }
    fn contributes_occurrences(&self) -> bool {
        self.contributes
    }
}

pub fn overlay_hit(symbol: &str, span: Span, role: Role, pc_only: bool) -> OverlayHit {
    OverlayHit {
        semantic_symbol: symbol.into(),
        span,
        role,
        pc_only,
    }
}
