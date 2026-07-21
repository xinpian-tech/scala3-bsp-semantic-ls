//! Live `pc-plugins.json` compiler-plugin loading integration: boots the
//! PRODUCTION embedded island with a GENERIC test-fixture scalac plugin (the
//! mill `pcNavTestPlugin` module) loaded through a workspace `pc-plugins.json`
//! — the per-workspace plugin loader the island runs at boot — and proves the
//! whole product mechanism over the REAL vtable + a live compiler:
//!
//!   * the island's `plugin_status` control op reports the configured jar as
//!     LOADED (the config was read, the jar reached `-Xplugin`);
//!   * the plugin's steering is OBSERVABLE through the vtable: go-to on the
//!     fixture Dynamic access `io.a` (a `lstest.navfixture.NavProbe[T]`
//!     receiver — the marker shape the fixture plugin itself defines) resolves
//!     to the real `val a` declaration, landing on the exact NAME range;
//!   * a NON-marker `scala.Dynamic` access of the same shape is left unchanged
//!     (its `io.a` does NOT reach the in-buffer field) — the plugin is
//!     selective, so the steering is the loaded plugin's doing, not default PC
//!     behavior;
//!   * the `hover` vtable op round-trips with the plugin loaded; and
//!   * a missing fixture field (`io.notAField`) does not error or wedge the
//!     island — a normal steered query still succeeds afterwards.
//!
//! This proves `pc-plugins.json` `compilerPlugins` as a product feature with a
//! fixture this repo owns (no third-party API knowledge). Real-world
//! navigation plugins ride the same path — or, like zaozi's in-build
//! `zaozi-compiler-plugin`, arrive through the build's own `-Xplugin`
//! scalacOptions; both feed the identical island compiler.
//!
//! Env-gated like the other live tests (`LS_LIBJVM` + `PC_HOST_AGENT_JAR` +
//! `LS_PC_TARGET_CLASSPATH`) plus `LS_PC_NAVTEST_JAR` (the mill-built fixture
//! plugin jar); skips cleanly when unset. A separate test binary because only
//! one JVM can boot per process.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use ls_jvm::backend::VtableBackend;
use ls_jvm::watchdog::{PcRequest, QueryKind, Supervisor};
use ls_jvm::{boot_island, IslandConfig};
use ls_pc_abi::payloads::{DefinitionResult, HoverResult, Location, PluginStatus, TargetConfig};

const TARGET_ID: &str = "navtest-app";

/// The fixture buffer: the marker API the plugin keys on
/// (`lstest.navfixture.NavProbe`, declared in-buffer under exactly the name
/// the plugin owns) plus a `Payload` whose `val a` is the steering target.
/// `io.a` desugars to `transparent inline selectDynamic("a")`, so without the
/// plugin go-to lands on `selectDynamic`, never on `val a`.
const USE_BUFFER: &str = "\
package lstest.navfixture {
  import scala.language.dynamics
  trait NavProbe[T] extends scala.Dynamic:
    transparent inline def selectDynamic(name: String): Any = navHelper(this, name)
  def navHelper(r: Any, name: String): Any = null
}
package sample {
  import lstest.navfixture.NavProbe
  class Payload:
    val a: Int = 0
  object Use:
    val io: NavProbe[Payload] = null.asInstanceOf[NavProbe[Payload]]
    val steered = io.a
    val missing = io.notAField
}
";

/// A non-marker `scala.Dynamic` access of the same shape, self-contained (the
/// field is in-buffer) — the plugin must leave it alone (the negative control
/// proving the steering above is the plugin's, not default PC behavior).
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

const USE_URI: &str = "file:///navtest/UseBuffer.scala";
const ALIEN_URI: &str = "file:///navtest/Alien.scala";

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
        plugin_jar: std::env::var("LS_PC_NAVTEST_JAR").ok()?,
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

/// The definition locations returned for the cursor `(line, character)` in
/// `text` opened at `uri` under the fixture target.
fn definition_locations(
    sup: &mut Supervisor<VtableBackend>,
    uri: &str,
    text: &str,
    line: u32,
    character: u32,
) -> Vec<Location> {
    sup.request(PcRequest::DidOpen {
        target_id: TARGET_ID.to_string(),
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
}

/// The definition start lines for the cursor (a projection of
/// [`definition_locations`]).
fn definition_lines(
    sup: &mut Supervisor<VtableBackend>,
    uri: &str,
    text: &str,
    line: u32,
    character: u32,
) -> Vec<u32> {
    definition_locations(sup, uri, text, line, character)
        .iter()
        .map(|l| l.range.start_line)
        .collect()
}

#[test]
fn live_pc_plugins_json_loads_the_fixture_plugin_and_steers_definition() {
    let Some(env) = env() else {
        eprintln!(
            "live_pcplugin: skipping — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH + LS_PC_NAVTEST_JAR to run the live plugin-load test"
        );
        return;
    };

    // A workspace whose `pc-plugins.json` loads the fixture compiler plugin
    // through the per-workspace plugin loader the island runs at boot.
    let root = std::env::temp_dir().join(format!("ls-live-pcplugin-{}", std::process::id()));
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

    // The plugin arrives ONLY through pc-plugins.json: the registered target
    // carries no scalacOptions (no -Xplugin of its own).
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

    // Loading proof: the island's plugin-status report carries the configured
    // jar as LOADED — pc-plugins.json was read and the jar reached -Xplugin.
    let status = PluginStatus::decode(&sup.plugin_status().expect("plugin_status control op"))
        .expect("decode plugin status");
    assert!(
        status
            .compiler_plugins
            .iter()
            .any(|p| p.loaded && p.jars.iter().any(|j| j == &env.plugin_jar)),
        "the fixture jar {} must be reported loaded via pc-plugins.json; got {status:?}",
        env.plugin_jar
    );

    // Steering proof over the vtable: go-to on the fixture Dynamic `io.a`
    // reaches the in-buffer `val a` declaration. Without the plugin the PC
    // resolves `io.a` to `selectDynamic` (the alien control below pins that),
    // so this is the loaded plugin acting inside the island compiler.
    let (use_line, use_char) = cursor(USE_BUFFER, "io.a", 3);
    let val_a_line = line_of(USE_BUFFER, "val a: Int = 0");
    let use_lines = definition_lines(&mut sup, USE_URI, USE_BUFFER, use_line, use_char);
    assert!(
        use_lines.contains(&val_a_line),
        "go-to on the fixture io.a should reach `val a` (line {val_a_line}); \
         got def lines {use_lines:?}"
    );

    // Exact definition range: the steered go-to lands on the NAME span of
    // `val a` (start..end characters), not merely the right line.
    let a_defs = definition_locations(&mut sup, USE_URI, USE_BUFFER, use_line, use_char);
    let val_a_text = USE_BUFFER
        .lines()
        .find(|l| l.contains("val a: Int"))
        .expect("fixture has `val a`");
    let a_col = val_a_text.find("val a").unwrap() as u32 + 4; // the `a` after "val "
    assert!(
        a_defs.iter().any(|l| l.range.start_line == val_a_line
            && l.range.start_character == a_col
            && l.range.end_character == a_col + 1),
        "the steered io.a definition must span the exact `a` name (line {val_a_line}, \
         col {a_col}..{}); got {a_defs:?}",
        a_col + 1
    );

    // Hover over the vtable: the hover op round-trips for the fixture Dynamic
    // access with the plugin loaded — a second production vtable op end-to-end.
    {
        let reply = sup
            .request(PcRequest::Query {
                kind: QueryKind::Hover,
                uri: USE_URI.to_string(),
                line: use_line,
                character: use_char,
            })
            .expect("hover query");
        HoverResult::decode(&reply).expect("hover round-trips over the vtable");
    }

    // Missing-field no-crash: go-to on a non-existent fixture field must not
    // error or wedge the island (the plugin's steering is guarded/total), and a
    // steered query must still succeed afterwards.
    let (miss_line, miss_char) = cursor(USE_BUFFER, "notAField", 0);
    let _ = definition_locations(&mut sup, USE_URI, USE_BUFFER, miss_line, miss_char);
    let after = definition_lines(&mut sup, USE_URI, USE_BUFFER, use_line, use_char);
    assert!(
        after.contains(&val_a_line),
        "the island must still steer io.a after a missing-field query; got {after:?}"
    );

    // Selectivity: a non-marker Dynamic access of the same shape is left
    // unchanged — its `io.a` does NOT reach the in-buffer field, so the
    // steering above is the plugin's doing (it only rewrites the marker
    // shape), not default PC behavior.
    let (alien_line, alien_char) = cursor(ALIEN_BUFFER, "io.a", 3);
    let alien_lines = definition_lines(&mut sup, ALIEN_URI, ALIEN_BUFFER, alien_line, alien_char);
    let alien_val_a_line = line_of(ALIEN_BUFFER, "val a: Int = 0");
    assert!(
        !alien_lines.contains(&alien_val_a_line),
        "a non-marker Dynamic access must be unchanged by the plugin; io.a unexpectedly \
         reached `val a` (line {alien_val_a_line}); got {alien_lines:?}"
    );
}
