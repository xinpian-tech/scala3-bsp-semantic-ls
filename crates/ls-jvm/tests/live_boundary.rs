//! Live embedded-island integration: boots the PRODUCTION island
//! (`boot_island`) with the real PC-host assembly against a real JVM and drives
//! the full PC operation surface through the 22-slot vtable to a **live Scala
//! presentation compiler**. This exercises the production boundary end-to-end —
//! boot + registration, the flat-codec round-trip for every payload, and
//! loaned-thread dispatch/control — against a real compiler, not the spike echo
//! op or the Scala-only unit seam.
//!
//! It is env-gated so a bare `cargo test` (no JVM present) still passes:
//!   LS_LIBJVM              = <jdk>/lib/server/libjvm.so
//!   PC_HOST_AGENT_JAR      = the pcHost assembly jar (self-contained premain)
//!   LS_PC_TARGET_CLASSPATH = ':'-separated jars for the registered target's
//!                            classpath (the Scala standard library)
//! When any is absent the test logs a skip and returns (still green), exactly
//! as the boundary spike tests skip without `LS_LIBJVM`/`SPIKE_AGENT_JAR`.
//!
//! `spawn_dispatch` (the recovery-generation slot) is the one vtable op not
//! driven here: it fires only on a non-cooperative dispatch wedge, which needs a
//! fault hook inside the Java host; that live recovery scenario is a separate
//! slice. The pure-Rust generation ladder is covered by the `ls-jvm` unit tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ls_jvm::backend::VtableBackend;
use ls_jvm::watchdog::{PcRequest, QueryKind, Supervisor};
use ls_jvm::{boot_island, IslandConfig};
use ls_pc_abi::payloads::{
    CompletionItem, CompletionList, DefinitionResult, HoverContents, HoverResult, PluginStatus,
    PrepareRenameResult, SignatureHelp, TargetConfig,
};

const TARGET_ID: &str = "live-target";
const URI: &str = "file:///live/demo/Main.scala";

// The buffer the query ops run against (line numbers are 0-based):
//   0  package demo
//   1
//   2  class Box(val n: Int)
//   3
//   4  object Main:
//   5    def greet(name: String): String = "hi " + name
//   6    val boxed: Box = Box(1)
//   7    val used = boxed.n
//   8    val msg = greet("world")
//   9    def renamed: Int =
//  10      val local = 1
//  11      local + 1
//  12    val xs = List(1, 2, 3)
//  13    val ys = xs.
const RICH_SOURCE: &str = concat!(
    "package demo\n",
    "\n",
    "class Box(val n: Int)\n",
    "\n",
    "object Main:\n",
    "  def greet(name: String): String = \"hi \" + name\n",
    "  val boxed: Box = Box(1)\n",
    "  val used = boxed.n\n",
    "  val msg = greet(\"world\")\n",
    "  def renamed: Int =\n",
    "    val local = 1\n",
    "    local + 1\n",
    "  val xs = List(1, 2, 3)\n",
    "  val ys = xs.\n",
);

// A different buffer used to prove did_change took effect: completion now dots
// off a `String`, so its members (e.g. `length`) replace the `List` members.
//   0  object Main:
//   1    val s = "hello"
//   2    val t = s.
const CHANGED_SOURCE: &str = "object Main:\n  val s = \"hello\"\n  val t = s.\n";

struct Env {
    libjvm: PathBuf,
    agent_jar: PathBuf,
    classpath: Vec<String>,
}

/// Reads the boot env; `None` (→ skip) when the live-JVM inputs are absent, so a
/// cold `cargo test` run without a JVM still passes.
fn env() -> Option<Env> {
    let libjvm = PathBuf::from(std::env::var_os("LS_LIBJVM")?);
    let agent_jar = PathBuf::from(std::env::var_os("PC_HOST_AGENT_JAR")?);
    let classpath = std::env::var("LS_PC_TARGET_CLASSPATH")
        .ok()?
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    Some(Env {
        libjvm,
        agent_jar,
        classpath,
    })
}

/// A booted production island with one registered target, plus helpers to drive
/// each PC op through the real vtable. Boot happens once; the whole sweep runs
/// sequentially against this fixture (only one JVM can exist per process).
struct LiveIsland {
    supervisor: Supervisor<VtableBackend>,
    _workspace: PathBuf,
}

impl LiveIsland {
    fn boot(env: &Env, workspace: &Path) -> LiveIsland {
        let config = IslandConfig {
            libjvm: &env.libjvm,
            agent_jar: &env.agent_jar,
            extra_classpath: &[],
            workspace_root: Some(workspace),
            extra_jvm_options: &[],
            rendezvous_timeout: Duration::from_secs(30),
            max_abandoned_generations: 4,
            // Generous: the first query pays the one-time compiler warm-up.
            request_deadline: Duration::from_secs(120),
            cancel_grace: Duration::from_millis(200),
        };
        let mut supervisor = boot_island(&config).expect("the production island boots");
        supervisor
            .request(PcRequest::RegisterTarget {
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
        LiveIsland {
            supervisor,
            _workspace: workspace.to_path_buf(),
        }
    }

    fn open(&mut self, text: &str) {
        self.supervisor
            .request(PcRequest::DidOpen {
                target_id: TARGET_ID.to_string(),
                uri: URI.to_string(),
                text: text.to_string(),
            })
            .expect("did_open");
    }

    fn change(&mut self, text: &str) {
        self.supervisor
            .request(PcRequest::DidChange {
                uri: URI.to_string(),
                text: text.to_string(),
            })
            .expect("did_change");
    }

    fn close(&mut self) {
        self.supervisor
            .request(PcRequest::DidClose {
                uri: URI.to_string(),
            })
            .expect("did_close");
    }

    fn query(&mut self, kind: QueryKind, line: u32, character: u32) -> Vec<u8> {
        self.supervisor
            .request(PcRequest::Query {
                kind,
                uri: URI.to_string(),
                line,
                character,
            })
            .expect("query")
    }

    fn completion(&mut self, line: u32, character: u32) -> CompletionList {
        CompletionList::decode(&self.query(QueryKind::Completion, line, character))
            .expect("decode completion list")
    }

    fn hover(&mut self, line: u32, character: u32) -> HoverResult {
        HoverResult::decode(&self.query(QueryKind::Hover, line, character)).expect("decode hover")
    }

    fn signature_help(&mut self, line: u32, character: u32) -> SignatureHelp {
        SignatureHelp::decode(&self.query(QueryKind::SignatureHelp, line, character))
            .expect("decode signature help")
    }

    fn definition(&mut self, line: u32, character: u32) -> DefinitionResult {
        DefinitionResult::decode(&self.query(QueryKind::Definition, line, character))
            .expect("decode definition")
    }

    fn type_definition(&mut self, line: u32, character: u32) -> DefinitionResult {
        DefinitionResult::decode(&self.query(QueryKind::TypeDefinition, line, character))
            .expect("decode type definition")
    }

    fn prepare_rename(&mut self, line: u32, character: u32) -> PrepareRenameResult {
        PrepareRenameResult::decode(&self.query(QueryKind::PrepareRename, line, character))
            .expect("decode prepare rename")
    }

    fn resolve(&mut self, symbol: &str, item: &CompletionItem) -> CompletionItem {
        let bytes = self
            .supervisor
            .request(PcRequest::Resolve {
                target_id: TARGET_ID.to_string(),
                symbol: symbol.to_string(),
                item: item.encode().unwrap(),
            })
            .expect("completion_resolve");
        CompletionItem::decode(&bytes).expect("decode resolved item")
    }

    fn plugin_status(&self) -> PluginStatus {
        PluginStatus::decode(&self.supervisor.plugin_status().expect("plugin_status"))
            .expect("decode plugin status")
    }

    fn restart_instances(&mut self) -> i32 {
        self.supervisor.restart_instances()
    }

    fn shutdown(&self) -> i32 {
        self.supervisor.shutdown()
    }
}

/// The markup/plain text of a hover response, or empty when it is null.
fn hover_text(hover: &HoverResult) -> String {
    match &hover.0 {
        Some(hover) => match &hover.contents {
            HoverContents::Markup(markup) => markup.value.clone(),
            HoverContents::Marked(items) => format!("{items:?}"),
        },
        None => String::new(),
    }
}

/// The resolution symbol a completion item carries in its `data` (mtags puts the
/// SemanticDB symbol in `CompletionItem.data.symbol`), if present.
fn resolution_symbol(item: &CompletionItem) -> Option<String> {
    item.data
        .as_deref()
        .and_then(|data| json_str_field(data, "symbol"))
}

/// Extract a JSON string field's value from opaque `data` bytes (the completion
/// item's `data` is canonical JSON carrying the resolution symbol).
fn json_str_field(data: &[u8], field: &str) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    let key = format!("\"{field}\"");
    let after_key = text.find(&key)? + key.len();
    let after_colon = text[after_key..].find(':')? + after_key + 1;
    let open = text[after_colon..].find('"')? + after_colon + 1;
    let close = text[open..].find('"')? + open;
    Some(text[open..close].to_string())
}

#[test]
fn live_island_answers_the_full_pc_operation_surface() {
    let Some(env) = env() else {
        eprintln!(
            "live_boundary: skipping — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run the live embedded-JVM boundary test"
        );
        return;
    };

    // A real (if empty) workspace root, as the BSP layer would supply: the
    // island loads any `pc-plugins.json` under it (there is none here).
    let workspace = std::env::temp_dir().join(format!("ls-live-boundary-{}", std::process::id()));
    std::fs::create_dir_all(&workspace).expect("create workspace root");

    let mut island = LiveIsland::boot(&env, &workspace);
    island.open(RICH_SOURCE);

    // completion: members of `List[Int]` after `xs.` on line 13.
    let completion = island.completion(13, 14);
    let labels: Vec<&str> = completion
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert!(
        labels.iter().any(|label| label.starts_with("map")),
        "expected a `map` member on List[Int]; got {} items: {labels:?}",
        labels.len()
    );

    // completion_resolve: pick a completion item that carries a non-empty
    // resolution `symbol` in its `data`, resolve it, and require a real
    // enrichment (the PC fills in detail/documentation on resolve) in addition
    // to label stability — not just an unchanged echo.
    let resolvable = completion
        .items
        .iter()
        .find(|item| resolution_symbol(item).is_some_and(|symbol| !symbol.is_empty()))
        .expect("a completion item carrying a non-empty resolution symbol in its data");
    let symbol = resolution_symbol(resolvable).expect("resolution symbol");
    let resolved = island.resolve(&symbol, resolvable);
    assert_eq!(
        resolved.label, resolvable.label,
        "completion_resolve must return the same item"
    );
    let enriched = resolved.detail.as_deref().is_some_and(|d| !d.is_empty())
        || resolved.documentation.is_some();
    assert!(
        enriched,
        "completion_resolve must enrich the item with detail/documentation; got {resolved:?}"
    );

    // hover: on the `greet` application (line 8) → markup from the live compiler.
    let hover = island.hover(8, 14);
    let hover_markup = hover_text(&hover);
    assert!(
        !hover_markup.is_empty(),
        "expected non-empty hover markup on `greet`, got a null hover"
    );

    // signature_help: inside `greet(` on line 8 → a signature naming the param.
    let signature = island.signature_help(8, 18);
    assert!(
        !signature.signatures.is_empty(),
        "expected a signature for `greet(`"
    );
    assert!(
        signature.signatures[0].label.contains("name"),
        "expected the `greet` signature to name its `name` parameter; got {:?}",
        signature.signatures[0].label
    );

    // definition: on the `greet` use (line 8) → its declaration on line 5.
    let definition = island.definition(8, 14);
    assert!(
        definition
            .locations
            .iter()
            .any(|loc| loc.range.start_line == 5),
        "expected `greet`'s definition at line 5; got {:?}",
        definition.locations
    );

    // type_definition: on `boxed` (line 7) → its type `class Box` on line 2.
    let type_def = island.type_definition(7, 15);
    assert!(
        type_def
            .locations
            .iter()
            .any(|loc| loc.range.start_line == 2),
        "expected `boxed`'s type definition at the `class Box` line 2; got {:?}",
        type_def.locations
    );

    // prepare_rename: on the `local` use inside `renamed` (line 11) → a non-null
    // range; the PC offers rename ranges for method-local bindings.
    let rename = island.prepare_rename(11, 6);
    assert!(
        rename.0.is_some(),
        "expected a prepare-rename range on the method-local `local`"
    );

    // restart_instances + replay: the doctor restart tier recreates the PC
    // instances; the registered target and open buffer survive, so the same
    // completion works with no re-register / re-open by the editor.
    assert_eq!(island.restart_instances(), 0, "restart_instances status");
    let after_restart = island.completion(13, 14);
    assert!(
        after_restart
            .items
            .iter()
            .any(|item| item.label.starts_with("map")),
        "the completion must still work after restart_instances without reopening the buffer"
    );

    // did_change: swap the buffer to dot off a `String`; completion now offers
    // String members (e.g. `length`), proving the change reached the compiler.
    island.change(CHANGED_SOURCE);
    let changed = island.completion(2, 12);
    let changed_labels: Vec<&str> = changed.items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        changed_labels
            .iter()
            .any(|label| label.starts_with("length")),
        "expected a `length` member on String after did_change; got {changed_labels:?}"
    );

    // did_close: closes the buffer without error.
    island.close();

    // plugin_status: a decoded report; with no workspace `pc-plugins.json` there
    // are no configured compiler plugins.
    let status = island.plugin_status();
    assert!(
        status.compiler_plugins.is_empty(),
        "expected no configured compiler plugins for a config-less workspace"
    );

    // shutdown: orderly PC teardown (driven last).
    assert_eq!(island.shutdown(), 0, "shutdown status");
}
