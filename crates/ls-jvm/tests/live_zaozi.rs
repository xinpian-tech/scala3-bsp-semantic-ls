//! Live zaozi cross-file navigation integration: the retained zaozi PC-plugin
//! cross-file nav suite re-pointed at the PRODUCTION embedded-JVM boundary. It
//! boots the island with the zaozi compiler plugin loaded through a workspace
//! `pc-plugins.json` (the per-workspace plugin loader the island runs at boot)
//! AND a real snapshot-backed `symbol_definition` resolver installed, and proves
//! over the REAL vtable + a live compiler the full cross-file flow:
//!   * a zaozi Dynamic field access `io.a` on a `Referable[LibBundle]`, where
//!     `LibBundle` is a COMPILED dependency (classes on the target classpath, no
//!     source in the PC's view), resolves to the library's source `val a`. That
//!     requires BOTH the plugin (which steers `io.a` to the field symbol) AND the
//!     Rust `symbol_definition` callback (which the PC consults because the field
//!     lives in a compiled dependency) — with the requesting buffer's target
//!     forward-closure containing the library target;
//!   * a NON-zaozi `scala.Dynamic` access of the same shape is left unchanged
//!     (its `io.a` does NOT reach the in-buffer field) — the plugin is selective,
//!     so the steering is the plugin's doing, not default PC behavior.
//!
//! Env-gated like the other live tests (`LS_LIBJVM` + `PC_HOST_AGENT_JAR` +
//! `LS_PC_TARGET_CLASSPATH`) plus `ZAOZI_PCPLUGIN_JAR`; skips cleanly when unset.
//! A separate test binary because only one JVM can boot per process. The library
//! is compiled in-test with the compiler bundled in the PC-host assembly, so the
//! SemanticDB symbol the index holds and the symbol the plugin+PC produce come
//! from the same compiler and match by construction.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ls_engine::{QueryOrchestrator, TargetSpec, WorkspaceTargets};
use ls_index_model::uri::path_to_uri;
use ls_index_model::Loc;
use ls_jvm::backend::VtableBackend;
use ls_jvm::watchdog::{PcRequest, QueryKind, Supervisor};
use ls_jvm::{boot_island, install_symbol_definition_resolver, IslandConfig};
use ls_pc_abi::payloads::{origin, DefinitionResult, Location, Rng, TargetConfig};
use ls_store::Store;

const APP_TARGET_ID: &str = "zaozi-app";

/// The zaozi mini-API + a `LibBundle` whose `val a` is the cross-file go-to
/// destination. Compiled to classes (for the PC classpath) + real SemanticDB
/// (for the index) — the retained `ZaoziPcCrossFileSuite` library source.
const LIB_SOURCE: &str = "\
package me.jiuyang.zaozi.magic { trait DynamicSubfield }
package me.jiuyang.zaozi.reftpe {
  import scala.language.dynamics
  trait Referable[T] extends scala.Dynamic:
    transparent inline def selectDynamic(name: String): Any = referHelper(this, name)
  def referHelper(r: Any, name: String): Any = null
}
package sample {
  import me.jiuyang.zaozi.magic.DynamicSubfield
  class LibBundle extends DynamicSubfield:
    val a: Int = 0
    def normalMethod(): Int = 1
}
";

/// The open buffer: `io.a` is a zaozi Dynamic access whose field lives in the
/// COMPILED `LibBundle` (no source in the PC), so go-to must leave the buffer.
const USE_BUFFER: &str = "\
import me.jiuyang.zaozi.reftpe.*
import sample.LibBundle

object Use:
  val io: Referable[LibBundle] = null.asInstanceOf[Referable[LibBundle]]
  val io2: LibBundle = new LibBundle
  val x = io.a
  val y = io2.normalMethod()
";

/// A non-zaozi `scala.Dynamic` access of the same shape, self-contained (the
/// field is in-buffer) — the plugin must leave it alone (the negative control).
const ALIEN_BUFFER: &str = "\
package other {
  import scala.language.dynamics
  trait Widget[T] extends scala.Dynamic:
    transparent inline def selectDynamic(name: String): Any = widgetHelper(this, name)
  def widgetHelper(r: Any, name: String): Any = null
  class Panel:
    val a: Int = 0
  object Top:
    val io: Widget[Panel] = null.asInstanceOf[Widget[Panel]]
    val probe = io.a
}
";

const LIB_REL: &str = "sample/Lib.scala";
const USE_URI_REL: &str = "UseBuffer.scala";
const ALIEN_URI_REL: &str = "Alien.scala";

struct Env {
    libjvm: PathBuf,
    agent_jar: PathBuf,
    classpath: Vec<String>,
    plugin_jar: String,
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
        plugin_jar: std::env::var("ZAOZI_PCPLUGIN_JAR").ok()?,
    })
}

/// (line, character) of `marker` in `text`, offset into the marker.
fn cursor(text: &str, marker: &str, offset: u32) -> (u32, u32) {
    for (i, line) in text.split('\n').enumerate() {
        if let Some(idx) = line.find(marker) {
            return (i as u32, idx as u32 + offset);
        }
    }
    panic!("marker '{marker}' not found in fixture");
}

fn line_of(text: &str, marker: &str) -> u32 {
    cursor(text, marker, 0).0
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

/// Compile the library with the compiler bundled in the PC-host assembly,
/// emitting classes (for the PC target classpath) + SemanticDB (for the index)
/// under `libroot`, from source rooted at `libsrc`.
fn compile_library(env: &Env, libsrc: &Path, libroot: &Path) {
    let src_file = libsrc.join(LIB_REL);
    fs::create_dir_all(src_file.parent().unwrap()).expect("create lib src dir");
    fs::write(&src_file, LIB_SOURCE).expect("write lib source");
    let status = Command::new("java")
        .arg("-cp")
        .arg(&env.agent_jar)
        .arg("dotty.tools.dotc.Main")
        .arg("-d")
        .arg(libroot)
        .arg("-Xsemanticdb")
        .arg("-sourceroot")
        .arg(libsrc)
        .arg("-classpath")
        .arg(env.classpath.join(":"))
        .arg(&src_file)
        .status()
        .expect("spawn dotty compiler from the assembly");
    assert!(status.success(), "library compile failed: {status}");
    assert!(
        libroot.join("sample/LibBundle.class").is_file(),
        "expected compiled LibBundle.class under {}",
        libroot.display()
    );
}

/// The definition start lines returned for the cursor `(line, character)` in
/// `text` opened at `uri` under the app target.
fn definition_lines(
    sup: &mut Supervisor<VtableBackend>,
    uri: &str,
    text: &str,
    line: u32,
    character: u32,
) -> Vec<u32> {
    sup.request(PcRequest::DidOpen {
        target_id: APP_TARGET_ID.to_string(),
        uri: uri.to_string(),
        text: text.to_string(),
    })
    .expect("did_open");
    let reply = sup
        .request(PcRequest::Query {
            kind: QueryKind::Definition,
            uri: uri.to_string(),
            line,
            character,
        })
        .expect("definition query");
    DefinitionResult::decode(&reply)
        .expect("decode definition")
        .locations
        .iter()
        .map(|l| l.range.start_line)
        .collect()
}

#[test]
fn live_zaozi_cross_file_go_to_routes_through_plugin_and_symbol_definition() {
    let Some(env) = env() else {
        eprintln!(
            "live_zaozi: skipping — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH + ZAOZI_PCPLUGIN_JAR to run the live zaozi test"
        );
        return;
    };

    let root = std::env::temp_dir().join(format!("ls-live-zaozi-{}", std::process::id()));
    let libsrc = root.join("libsrc");
    let libroot = root.join("libout");
    let appsrc = root.join("appsrc");
    let approot = root.join("appout");
    for d in [&libsrc, &libroot, &appsrc, &approot] {
        fs::create_dir_all(d).expect("create dir");
    }

    // Compile the library: classes (PC classpath) + real SemanticDB (index).
    compile_library(&env, &libsrc, &libroot);

    // Index the library so `symbol_definition` can resolve the field's symbol to
    // the library SOURCE. `app` (the buffer's target) forward-depends on `lib`,
    // so the forward closure of the requesting buffer contains the library def.
    let ws = WorkspaceTargets::new(vec![
        TargetSpec::new("lib", libroot.clone(), libsrc.clone()),
        TargetSpec::new("app", approot.clone(), appsrc.clone()).with_deps(vec!["lib".to_string()]),
    ]);
    let store = Store::open(&root.join("store")).expect("open store");
    let orch = Arc::new(QueryOrchestrator::with_defaults(store));
    orch.ingest(Arc::new(ws))
        .expect("ingest library semanticdb");

    let recorded = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let recorded_cb = recorded.clone();
    let orch_cb = orch.clone();
    install_symbol_definition_resolver(Box::new(move |symbol: &str, from_uri: &str| {
        recorded_cb
            .lock()
            .unwrap()
            .push((symbol.to_string(), from_uri.to_string()));
        ls_pc_abi::payloads::LocationsResult {
            locations: orch_cb
                .symbol_definition(symbol, from_uri)
                .into_iter()
                .map(to_abi_location)
                .collect(),
        }
    }));

    // A workspace whose `pc-plugins.json` loads the zaozi compiler plugin through
    // the per-workspace plugin loader the island runs at boot.
    let config_dir = root.join(".scala3-bsp-semantic-ls");
    fs::create_dir_all(&config_dir).expect("create workspace config dir");
    let plugin_config = format!(
        "{{\"compilerPlugins\":[{{\"jars\":[\"{}\"],\"options\":[]}}],\"servicePluginJars\":[]}}",
        env.plugin_jar
    );
    fs::write(config_dir.join("pc-plugins.json"), plugin_config).expect("write pc-plugins.json");

    let config = IslandConfig {
        libjvm: &env.libjvm,
        agent_jar: &env.agent_jar,
        extra_classpath: &[],
        workspace_root: Some(&root),
        extra_jvm_options: &[],
        rendezvous_timeout: Duration::from_secs(30),
        max_abandoned_generations: 4,
        // Generous: the plugin adds a compile phase, and several live JVM checks
        // build in parallel under nix flake check.
        request_deadline: Duration::from_secs(120),
        cancel_grace: Duration::from_millis(500),
    };
    let mut sup: Supervisor<VtableBackend> =
        boot_island(&config).expect("the production island boots");

    // The PC target sees the compiled library on its classpath (LibBundle has no
    // source in the PC's view — so go-to on it must fall through to the resolver).
    let mut classpath = env.classpath.clone();
    classpath.push(libroot.to_str().unwrap().to_string());
    sup.request(PcRequest::RegisterTarget {
        id: APP_TARGET_ID.to_string(),
        config: TargetConfig {
            bsp_id: APP_TARGET_ID.to_string(),
            scala_version: "3.8.4".to_string(),
            classpath,
            scalac_options: vec![],
            source_dirs: vec![],
        },
    })
    .expect("register_target");

    // Cross-file: go-to on the zaozi Dynamic `io.a` reaches the library's source
    // `val a`. This proves BOTH the plugin (steers io.a → the field symbol) and
    // the Rust symbol_definition callback (resolves the compiled-dependency symbol
    // to the library source across the target forward closure).
    let use_uri = path_to_uri(&appsrc.join(USE_URI_REL));
    let (use_line, use_char) = cursor(USE_BUFFER, "io.a", 3);
    let use_lines = definition_lines(&mut sup, &use_uri, USE_BUFFER, use_line, use_char);
    let lib_uri = path_to_uri(&libsrc.join(LIB_REL));
    let val_a_line = line_of(LIB_SOURCE, "val a: Int = 0");
    assert!(
        use_lines.contains(&val_a_line),
        "cross-file go-to on the zaozi io.a should reach the library `val a` \
         (line {val_a_line}); got def lines {use_lines:?}"
    );
    // The definition location is the library SOURCE file (resolved by the index),
    // not a compiled classfile — proving the resolver, not the PC, produced it.
    {
        let reply = sup
            .request(PcRequest::Query {
                kind: QueryKind::Definition,
                uri: use_uri.clone(),
                line: use_line,
                character: use_char,
            })
            .expect("definition re-query");
        let result = DefinitionResult::decode(&reply).expect("decode definition");
        assert!(
            result.locations.iter().any(|l| l.uri == lib_uri),
            "the go-to location must be the library source uri {lib_uri}; got {result:?}"
        );
    }
    // The PC consulted the Rust resolver with the library field's symbol and the
    // originating buffer uri — the plugin steered it to the field, and the PC fell
    // through to symbol_definition for the compiled-dependency symbol.
    let consulted = recorded.lock().unwrap().clone();
    assert!(
        consulted
            .iter()
            .any(|(sym, from)| sym.contains("LibBundle") && sym.contains('a') && from == &use_uri),
        "the resolver must be consulted for the LibBundle field symbol from the buffer; \
         got {consulted:?}"
    );

    // Selectivity: a non-zaozi Dynamic access of the same shape is left unchanged —
    // its `io.a` does NOT reach the in-buffer field, so the steering above is the
    // plugin's doing (it only rewrites zaozi-shaped accesses), not default PC
    // behavior.
    let alien_uri = path_to_uri(&appsrc.join(ALIEN_URI_REL));
    let (alien_line, alien_char) = cursor(ALIEN_BUFFER, "io.a", 3);
    let alien_lines = definition_lines(&mut sup, &alien_uri, ALIEN_BUFFER, alien_line, alien_char);
    let alien_val_a_line = line_of(ALIEN_BUFFER, "val a: Int = 0");
    assert!(
        !alien_lines.contains(&alien_val_a_line),
        "a non-zaozi Dynamic access must be unchanged by the plugin; io.a unexpectedly \
         reached `val a` (line {alien_val_a_line}); got {alien_lines:?}"
    );
}
