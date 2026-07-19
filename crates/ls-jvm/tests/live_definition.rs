//! Live cross-file `symbol_definition` + `search_methods` integration: boots
//! the PRODUCTION island with REAL snapshot-backed resolvers installed, and
//! proves both index callbacks over the REAL embedded-JVM boundary:
//!   * forward-closure pruning is asserted directly on the same orchestrator the
//!     island calls (a buffer in `app` reaches `core`'s definition, never the
//!     disconnected `dup` target's duplicate of the same symbol string);
//!   * the live presentation compiler, asked for the definition of a cross-file
//!     library symbol (`List`), routes through `SymbolSearch.definition` → the
//!     Scala `PcHostDefinitionResolver` → the Rust `symbol_definition` slot →
//!     the installed resolver, and the resolver's location comes back as the PC
//!     definition result — the full round-trip across FFM;
//!   * a REAL member completion (`s.myEx` on a `String` receiver) discovers a
//!     workspace extension method ONLY reachable through `SymbolSearch.
//!     searchMethods` → the Rust `search_methods` slot → the installed search
//!     resolver, whose canned hit names an extension method defined in a second
//!     source on the target's source path (mirroring how the dotty PC tests
//!     structure extension discovery through `TestingWorkspaceSearch`), and the
//!     extension item comes back in the completion list;
//!   * a REAL exhaustive-`match` completion over a cross-file sealed type
//!     (`java.nio.file.AccessMode` — a Java enum, the sealed shape whose
//!     children carry no compiler source positions) consults `SymbolSearch.
//!     definitionSourceToplevels` → the `definition_source_toplevels` slot →
//!     the installed resolver with the sealed parent's SemanticDB symbol, and
//!     the generated case order follows the resolver's (scrambled) list;
//!   * a REAL `pc_diagnostics` payload query (ABI v2 `PcPayloadQueryFn` slot)
//!     over a type-error buffer decodes a non-stub diagnostic — a v2 payload
//!     op proven end-to-end live.
//!
//! Env-gated exactly like the live sweep (`LS_LIBJVM` + `PC_HOST_AGENT_JAR` +
//! `LS_PC_TARGET_CLASSPATH`); skips cleanly when unset. A separate test binary
//! because only one JVM can boot per process — which is also why the search-
//! methods completion leg runs inside the same `#[test]` as the definition leg
//! rather than as a second test racing it for the one island.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ls_engine::{QueryOrchestrator, TargetSpec, WorkspaceTargets};
use ls_index_model::{Loc, Span};
use ls_jvm::backend::VtableBackend;
use ls_jvm::watchdog::{PayloadQueryKind, PcRequest, QueryKind, Supervisor};
use ls_jvm::{
    boot_island, install_definition_source_toplevels_resolver, install_search_methods_resolver,
    install_symbol_definition_resolver, IslandConfig,
};
use ls_pc_abi::payloads::{
    origin, CompletionEdit, CompletionList, DefinitionResult, Location, MethodHit,
    MethodHitsResult, PcDiagnosticsResult, Rng, TargetConfig, ToplevelsResult, UriParams,
};
use ls_semanticdb::{md5, SdbDocument, SdbOccurrence, SdbRange, SdbSymbolInfo};
use ls_store::Store;

const TARGET_ID: &str = "def-target";
const BUFFER_URI: &str = "file:///live/def/SearchBuffer.scala";
// A buffer referencing `List`, a scala-library symbol NOT defined in the buffer:
// go-to-definition on it must fall through to `SymbolSearch.definition`.
const SOURCE: &str = "object SearchBuffer:\n  val xs = List(1, 2)\n";
// What the resolver answers for the library symbol, so the RESPONSE leg of the
// boundary is exercised (the exact SemanticDB string for `List` is a compiler
// internal; the synthetic index proves the request/pruning legs separately).
const SENTINEL_URI: &str = "file:///resolved-by-index/Elsewhere.scala";
// The symbol defined in the synthetic two-target index (forward-closure proof).
const PROBE_SYMBOL: &str = "pkg/Probe#";

// ---- the search_methods completion leg -------------------------------------
// A second PC target whose buffer completes `s.myEx` on a String receiver; the
// extension method is NOT in scope, so the completion can only surface it
// through `SymbolSearch.searchMethods` → the `search_methods` vtable slot.
const EXT_TARGET_ID: &str = "ext-target";
const EXT_BUFFER_URI: &str = "file:///live/ext/Use.scala";
const EXT_BUFFER_SOURCE: &str = "object X:\n  val s = \"\"\n  val r = s.myEx\n";
// The second source the canned hit points at (on the target's source path, the
// island-side analogue of dotty's `TestingWorkspaceSearch` indexing the case
// source): a package-object extension method on String.
const EXT_SOURCE: &str =
    "package livex\n\nobject enrichments:\n  extension (s: String)\n    def myExt: Int = s.length\n";
const EXT_SYMBOL: &str = "livex/enrichments.myExt().";

// ---- the definition_source_toplevels completion leg -------------------------
// A buffer completing `matc` after a `java.nio.file.AccessMode` receiver: the
// exhaustive-match contributor sorts the sealed children through
// `SymbolSearch.definitionSourceToplevels` → the `definition_source_toplevels`
// vtable slot → the installed resolver, whose (deliberately scrambled) order
// must drive the generated case order. A JAVA enum is the one cross-file
// sealed shape whose children carry NO compiler source positions (Scala
// children — tasty or Scala-2 pickles alike — carry positions, and the sorter
// then never consults the search seam), so this is the shape that actually
// exercises the slot.
const MATCH_BUFFER_URI: &str = "file:///live/match/MatchBuffer.scala";
const MATCH_BUFFER_SOURCE: &str =
    "import java.nio.file.AccessMode\nobject MatchBuffer:\n  (??? : AccessMode) matc\n";
const ACCESS_MODE_SYMBOL: &str = "java/nio/file/AccessMode#";

// ---- the v2 payload-query leg -----------------------------------------------
// A buffer with a type error, pushed through the `pc_diagnostics` payload-query
// slot: a non-stub decoded diagnostic proves a v2 payload op end-to-end live.
const DIAG_BUFFER_URI: &str = "file:///live/diag/DiagBuffer.scala";
const DIAG_BUFFER_SOURCE: &str = "object DiagBuffer:\n  val broken: Int = \"nope\"\n";

// ---- minimal SemanticDB protobuf encoder (subset the ingest parser reads) ----

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

fn encode_doc(d: &SdbDocument) -> Vec<u8> {
    let mut w = ProtoWriter::default();
    w.message_field(1, |dw| {
        dw.varint_field(1, d.schema as i64 as u64);
        dw.string_field(2, &d.uri);
        dw.string_field(3, &d.text);
        dw.string_field(11, &d.md5);
        dw.varint_field(10, d.language_code as i64 as u64);
        for s in &d.symbols {
            dw.message_field(5, |sw| {
                sw.string_field(1, &s.symbol);
                sw.varint_field(3, s.kind_code as i64 as u64);
                sw.int32_field(4, s.properties);
                sw.string_field(5, &s.display_name);
            });
        }
        for o in &d.occurrences {
            dw.message_field(6, |ow| {
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
    w.out
}

const REFERENCE: i32 = 1;
const DEFINITION: i32 = 2;
const KIND_CLASS: i32 = 13;

/// Writes one doc's source + `.semanticdb` under a target's roots.
fn write_doc(
    targetroot: &Path,
    sourceroot: &Path,
    uri: &str,
    source: &str,
    symbols: Vec<SdbSymbolInfo>,
    occurrences: Vec<SdbOccurrence>,
) {
    let src = sourceroot.join(uri);
    fs::create_dir_all(src.parent().unwrap()).unwrap();
    fs::write(&src, source).unwrap();
    let doc = SdbDocument {
        schema: 4,
        uri: uri.to_string(),
        text: String::new(),
        md5: md5::compute_hex(source),
        language_code: 1,
        symbols,
        occurrences,
    };
    let file = targetroot
        .join("META-INF/semanticdb")
        .join(format!("{uri}.semanticdb"));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, encode_doc(&doc)).unwrap();
}

fn class_sym(symbol: &str, display: &str) -> SdbSymbolInfo {
    SdbSymbolInfo {
        symbol: symbol.to_string(),
        kind_code: KIND_CLASS,
        properties: 0,
        display_name: display.to_string(),
        overridden_symbols: Vec::new(),
    }
}

fn occ(sl: i32, sc: i32, el: i32, ec: i32, symbol: &str, role: i32) -> SdbOccurrence {
    SdbOccurrence {
        range: Some(SdbRange {
            start_line: sl,
            start_character: sc,
            end_line: el,
            end_character: ec,
        }),
        symbol: symbol.to_string(),
        role_code: role,
    }
}

/// `core` defines `pkg/Probe#`; `app` depends on `core` and references it; `dup`
/// is disconnected and ALSO defines `pkg/Probe#`.
fn build_orchestrator(root: &Path) -> (QueryOrchestrator, PathBuf, PathBuf) {
    let sub = |name: &str| {
        let p = root.join(name);
        fs::create_dir_all(&p).unwrap();
        p
    };
    let (core_t, core_s) = (sub("coretarget"), sub("coresrc"));
    let (app_t, app_s) = (sub("apptarget"), sub("appsrc"));
    let (dup_t, dup_s) = (sub("duptarget"), sub("dupsrc"));

    write_doc(
        &core_t,
        &core_s,
        "c/Probe.scala",
        "class Probe\n",
        vec![class_sym(PROBE_SYMBOL, "Probe")],
        vec![occ(0, 6, 0, 11, PROBE_SYMBOL, DEFINITION)],
    );
    write_doc(
        &app_t,
        &app_s,
        "a/App.scala",
        "val a = new Probe\n",
        vec![],
        vec![occ(0, 12, 0, 17, PROBE_SYMBOL, REFERENCE)],
    );
    write_doc(
        &dup_t,
        &dup_s,
        "d/Probe.scala",
        "class Probe\n",
        vec![class_sym(PROBE_SYMBOL, "Probe")],
        vec![occ(0, 6, 0, 11, PROBE_SYMBOL, DEFINITION)],
    );

    let ws = WorkspaceTargets::new(vec![
        TargetSpec::new("core", core_t, core_s.clone()),
        TargetSpec::new("app", app_t, app_s.clone()).with_deps(vec!["core".to_string()]),
        TargetSpec::new("dup", dup_t, dup_s.clone()),
    ]);
    let store = Store::open(&root.join("store")).expect("open store");
    let orch = QueryOrchestrator::with_defaults(store);
    orch.ingest(Arc::new(ws)).expect("ingest");
    (orch, core_s, app_s)
}

fn file_uri(sourceroot: &Path, rel: &str) -> String {
    format!(
        "file://{}/{}",
        sourceroot.to_str().unwrap().trim_end_matches('/'),
        rel
    )
}

fn to_abi_location(loc: Loc) -> Location {
    Location {
        uri: loc.uri,
        range: Rng {
            start_line: loc.span.start_line,
            start_character: loc.span.start_char,
            end_line: loc.span.end_line,
            end_character: loc.span.end_char,
        },
        origin: origin::WORKSPACE,
    }
}

struct Env {
    libjvm: PathBuf,
    agent_jar: PathBuf,
    classpath: Vec<String>,
}

fn env() -> Option<Env> {
    Some(Env {
        libjvm: PathBuf::from(std::env::var_os("LS_LIBJVM")?),
        agent_jar: PathBuf::from(std::env::var_os("PC_HOST_AGENT_JAR")?),
        classpath: std::env::var("LS_PC_TARGET_CLASSPATH")
            .ok()?
            .split(':')
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

#[test]
fn live_symbol_definition_prunes_and_round_trips_over_the_boundary() {
    let Some(env) = env() else {
        eprintln!(
            "live_definition: skipping — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run the live definition test"
        );
        return;
    };

    let workspace = std::env::temp_dir().join(format!("ls-live-def-{}", std::process::id()));
    fs::create_dir_all(&workspace).expect("create workspace root");
    let (orch, core_s, app_s) = build_orchestrator(&workspace);
    let orch = Arc::new(orch);

    // Forward-closure pruning, asserted directly on the SAME orchestrator the
    // island calls: from `app` (which depends on `core`) go-to-definition of
    // `pkg/Probe#` reaches core's definition only, never the disconnected `dup`.
    let from_app = file_uri(&app_s, "a/App.scala");
    let pruned = orch.symbol_definition(PROBE_SYMBOL, &from_app);
    assert_eq!(pruned.len(), 1, "app sees exactly the visible (core) def");
    assert_eq!(pruned[0].uri, file_uri(&core_s, "c/Probe.scala"));

    // Install the real snapshot-backed resolver. The live PC drives the library
    // symbol `List` (absent from the synthetic index), so the snapshot answer is
    // empty and we return a sentinel to also exercise the response leg.
    let recorded = Arc::new(Mutex::new(None::<(String, String)>));
    let recorded_cb = recorded.clone();
    let orch_cb = orch.clone();
    install_symbol_definition_resolver(Box::new(move |symbol: &str, from_uri: &str| {
        *recorded_cb.lock().unwrap() = Some((symbol.to_string(), from_uri.to_string()));
        let mut locs = orch_cb.symbol_definition(symbol, from_uri);
        if locs.is_empty() {
            locs.push(Loc::new(SENTINEL_URI.to_string(), Span::new(3, 2, 3, 7)));
        }
        ls_pc_abi::payloads::LocationsResult {
            locations: locs.into_iter().map(to_abi_location).collect(),
        }
    }));

    // The extension source the search resolver's canned hit points at, written
    // under a source dir the ext target hands the PC as its source path.
    let ext_src_dir = workspace.join("extsrc");
    fs::create_dir_all(ext_src_dir.join("livex")).expect("create ext source dir");
    let ext_source_path = ext_src_dir.join("livex/Enrichments.scala");
    fs::write(&ext_source_path, EXT_SOURCE).expect("write ext source");
    let ext_source_uri = format!("file://{}", ext_source_path.to_str().unwrap());

    // Install the search_methods resolver (the second index callback) before
    // boot, next to symbol_definition: it records the PC's query/target and
    // answers the canned extension-method hit (`myExt` at its real name range
    // in the second source).
    let searched = Arc::new(Mutex::new(None::<(String, String)>));
    let searched_cb = searched.clone();
    let hit_uri = ext_source_uri.clone();
    install_search_methods_resolver(Box::new(move |query: &str, target: &str| {
        *searched_cb.lock().unwrap() = Some((query.to_string(), target.to_string()));
        MethodHitsResult {
            hits: vec![MethodHit {
                uri: hit_uri.clone(),
                symbol: EXT_SYMBOL.to_string(),
                kind: 3,
                range: Rng {
                    start_line: 4,
                    start_character: 8,
                    end_line: 4,
                    end_character: 13,
                },
            }],
        }
    }));

    // Install the definition_source_toplevels resolver (the third index
    // callback) before boot: it records the PC's (symbol, uri) downcall and
    // answers a canned toplevels list for the enum whose order — WRITE before
    // READ before EXECUTE, a scramble of the declaration order — must drive
    // the exhaustive-match case order.
    let toplevels_seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let toplevels_cb = toplevels_seen.clone();
    install_definition_source_toplevels_resolver(Box::new(move |symbol: &str, uri: &str| {
        toplevels_cb
            .lock()
            .unwrap()
            .push((symbol.to_string(), uri.to_string()));
        if symbol == ACCESS_MODE_SYMBOL {
            ToplevelsResult {
                symbols: vec![
                    "java/nio/file/AccessMode#WRITE.".to_string(),
                    "java/nio/file/AccessMode#READ.".to_string(),
                    "java/nio/file/AccessMode#EXECUTE.".to_string(),
                ],
            }
        } else {
            ToplevelsResult {
                symbols: Vec::new(),
            }
        }
    }));

    let config = IslandConfig {
        libjvm: &env.libjvm,
        agent_jar: &env.agent_jar,
        extra_classpath: &[],
        workspace_root: Some(&workspace),
        extra_jvm_options: &[],
        rendezvous_timeout: Duration::from_secs(30),
        max_abandoned_generations: 4,
        request_deadline: Duration::from_secs(30),
        cancel_grace: Duration::from_millis(500),
    };
    let mut sup: Supervisor<VtableBackend> =
        boot_island(&config).expect("the production island boots");

    sup.request(PcRequest::RegisterTarget {
        id: TARGET_ID.to_string(),
        config: TargetConfig {
            bsp_id: TARGET_ID.to_string(),
            scala_version: "3.8.4".to_string(),
            classpath: env.classpath.clone(),
            scalac_options: vec![],
            source_dirs: vec![],
        },
    })
    .expect("register_target");
    sup.request(PcRequest::DidOpen {
        target_id: TARGET_ID.to_string(),
        uri: BUFFER_URI.to_string(),
        text: SOURCE.to_string(),
    })
    .expect("did_open");

    // Definition on `List` (line 1, just after `  val xs = Li`) — a cross-file
    // symbol the PC resolves through SymbolSearch.definition → our resolver.
    let allocs_before = ls_pc_abi::memory::live_allocations();
    let reply = sup
        .request(PcRequest::Query {
            kind: QueryKind::Definition,
            uri: BUFFER_URI.to_string(),
            line: 1,
            character: "  val xs = Li".len() as u32,
        })
        .expect("definition query");
    let result = DefinitionResult::decode(&reply).expect("decode definition");

    // The island resolver freed the Rust-owned symbol_definition response buffer
    // across the boundary (and the Rust backend freed the query response), so no
    // allocation leaks after the round-trip — the resolver's `finally`-free path.
    assert_eq!(
        ls_pc_abi::memory::live_allocations(),
        allocs_before,
        "the symbol_definition response buffer must be freed by the island resolver"
    );

    // The resolver's location came back through the PC across the boundary.
    assert!(
        result.locations.iter().any(|l| l.uri == SENTINEL_URI),
        "resolver location must reach the PC definition result over the boundary: {result:?}"
    );
    // The PC consulted the resolver with the SemanticDB symbol for `List` and the
    // originating buffer uri — the downcall carried the exact arguments.
    let (symbol, from_uri) = recorded
        .lock()
        .unwrap()
        .clone()
        .expect("the PC consulted the resolver for the cross-file symbol");
    assert!(
        symbol.starts_with("scala/") && symbol.contains("List"),
        "unexpected cross-file symbol: {symbol}"
    );
    assert_eq!(from_uri, BUFFER_URI);

    // ---- search_methods: a REAL completion discovers a workspace extension
    // method only reachable through the `search_methods` slot. ----
    sup.request(PcRequest::RegisterTarget {
        id: EXT_TARGET_ID.to_string(),
        config: TargetConfig {
            bsp_id: EXT_TARGET_ID.to_string(),
            scala_version: "3.8.4".to_string(),
            classpath: env.classpath.clone(),
            scalac_options: vec![],
            source_dirs: vec![ext_src_dir.to_str().unwrap().to_string()],
        },
    })
    .expect("register ext target");
    sup.request(PcRequest::DidOpen {
        target_id: EXT_TARGET_ID.to_string(),
        uri: EXT_BUFFER_URI.to_string(),
        text: EXT_BUFFER_SOURCE.to_string(),
    })
    .expect("did_open ext buffer");

    // Completion after `s.myEx` (line 2): the receiver is a String and `myExt`
    // is not in scope, so the item can only come from searchMethods.
    let allocs_before = ls_pc_abi::memory::live_allocations();
    let reply = sup
        .request(PcRequest::Query {
            kind: QueryKind::Completion,
            uri: EXT_BUFFER_URI.to_string(),
            line: 2,
            character: "  val r = s.myEx".len() as u32,
        })
        .expect("completion query");
    let list = CompletionList::decode(&reply).expect("decode completion list");
    assert_eq!(
        ls_pc_abi::memory::live_allocations(),
        allocs_before,
        "the search_methods response buffer must be freed by the island resolver"
    );

    // The PC consulted the search resolver with the member query and the
    // requesting PC target id — the downcall carried the exact arguments.
    let (query, target) = searched
        .lock()
        .unwrap()
        .clone()
        .expect("the PC consulted the search_methods resolver for the member completion");
    assert_eq!(query, "myEx");
    assert_eq!(target, EXT_TARGET_ID);

    // The canned hit's extension method came back as a completion item: the
    // compiler resolved the SemanticDB symbol from the second source and kept
    // it applicable to the String receiver.
    let labels: Vec<&str> = list.items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.iter().any(|l| l.starts_with("myExt")),
        "the workspace extension method must reach the completion list: {labels:?}"
    );

    // ---- definition_source_toplevels: a REAL exhaustive-match completion
    // over a cross-file sealed type orders its cases by the resolver's list. ----
    sup.request(PcRequest::DidOpen {
        target_id: TARGET_ID.to_string(),
        uri: MATCH_BUFFER_URI.to_string(),
        text: MATCH_BUFFER_SOURCE.to_string(),
    })
    .expect("did_open match buffer");

    // Completion at `matc` (line 2): the match-keyword contributor computes the
    // exhaustive completion for `AccessMode`; the enum constants come from a
    // Java classfile without source positions, so their order can only come
    // from the toplevels resolver.
    let reply = sup
        .request(PcRequest::Query {
            kind: QueryKind::Completion,
            uri: MATCH_BUFFER_URI.to_string(),
            line: 2,
            character: "  (??? : AccessMode) matc".len() as u32,
        })
        .expect("match completion query");
    let list = CompletionList::decode(&reply).expect("decode match completion list");

    // The PC consulted the resolver with the sealed parent's SemanticDB symbol
    // and the requesting buffer's uri — the downcall carried the arguments.
    let consulted = toplevels_seen.lock().unwrap().clone();
    assert!(
        consulted
            .iter()
            .any(|(symbol, uri)| symbol == ACCESS_MODE_SYMBOL && uri == MATCH_BUFFER_URI),
        "the PC must consult the toplevels resolver for {ACCESS_MODE_SYMBOL}: {consulted:?}"
    );

    // The exhaustive item's generated cases follow the resolver's list order —
    // WRITE, READ, EXECUTE — a scramble of the declaration order (READ, WRITE,
    // EXECUTE), so only the resolver can explain it.
    let exhaustive = list
        .items
        .iter()
        .find(|i| i.label.contains("exhaustive"))
        .unwrap_or_else(|| panic!("no exhaustive match item among {} items", list.items.len()));
    let new_text = match (&exhaustive.text_edit, &exhaustive.insert_text) {
        (Some(CompletionEdit::Plain(edit)), _) => edit.new_text.clone(),
        (Some(CompletionEdit::InsertReplace(edit)), _) => edit.new_text.clone(),
        (None, Some(text)) => text.clone(),
        (None, None) => panic!("exhaustive item carries no edit text"),
    };
    let write_at = new_text
        .find("WRITE")
        .expect("case WRITE in the exhaustive match");
    let read_at = new_text
        .find("READ")
        .expect("case READ in the exhaustive match");
    let execute_at = new_text
        .find("EXECUTE")
        .expect("case EXECUTE in the exhaustive match");
    assert!(
        write_at < read_at && read_at < execute_at,
        "case order must follow the resolver's list (WRITE, READ, EXECUTE): {new_text}"
    );

    // ---- v2 payload query: a REAL pc_diagnostics round-trip through the
    // payload-query vtable slot answers a non-stub decoded diagnostic. ----
    sup.request(PcRequest::DidOpen {
        target_id: TARGET_ID.to_string(),
        uri: DIAG_BUFFER_URI.to_string(),
        text: DIAG_BUFFER_SOURCE.to_string(),
    })
    .expect("did_open diag buffer");

    let params = UriParams {
        uri: DIAG_BUFFER_URI.to_string(),
    }
    .encode()
    .expect("encode pc_diagnostics params");
    let allocs_before = ls_pc_abi::memory::live_allocations();
    let reply = sup
        .request(PcRequest::PayloadQuery {
            kind: PayloadQueryKind::PcDiagnostics,
            params,
        })
        .expect("pc_diagnostics payload query");
    assert_eq!(
        ls_pc_abi::memory::live_allocations(),
        allocs_before,
        "the pc_diagnostics response buffer must be freed after decode"
    );
    let diags = PcDiagnosticsResult::decode(&reply).expect("decode pc_diagnostics");
    assert!(
        !diags.diagnostics.is_empty(),
        "the type error must surface as a live PC diagnostic"
    );
    let diag = &diags.diagnostics[0];
    assert_eq!(diag.severity, 1, "expected an Error severity: {diag:?}");
    assert_eq!(
        diag.range.start_line, 1,
        "the error is on the val line: {diag:?}"
    );
    assert!(
        !diag.message.is_empty(),
        "expected a rendered message: {diag:?}"
    );
}
