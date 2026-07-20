//! `textDocument/formatting` over the framed wire: the REAL serve loop + REAL
//! `IndexBootstrap` (fake BSP corpus, fake PC — the island is irrelevant:
//! formatting never touches it), shelling out to a REAL scalafmt CLI from the
//! dev shell for the round trip. The round-trip case skips cleanly when no
//! scalafmt is resolvable (`LS_SCALAFMT` / `PATH`), exactly like the live PC
//! suites skip without their boot env — the typed-error cases need no
//! binary and always run.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use ls_server::IndexBootstrap;
use ls_testkit::client::WireClient;
use ls_testkit::fake_bsp::FakeBsp;
use ls_testkit::fake_pc::FakePcService;

/// Boot the production serve loop over the fake BSP corpus (the pc_wire boot;
/// the fake PC only satisfies the bootstrap seam).
fn boot() -> WireClient {
    let pc = FakePcService::new();
    WireClient::boot_in_process_with(move |parts| {
        let (fake, source) =
            FakeBsp::start(Arc::clone(&parts.reload_flag), Arc::clone(&parts.sink));
        let pc_diagnostics = Arc::clone(&source.pc_diagnostics);
        let bootstrap = IndexBootstrap::with_pc(source, FakePcService::factory(pc))
            .with_pc_diagnostics(pc_diagnostics);
        (fake.workspace_root.clone(), fake, bootstrap)
    })
}

/// The real scalafmt the server would resolve without a workspace config:
/// `LS_SCALAFMT`, else the first executable on `PATH`. `None` skips the
/// round-trip case (the hermetic `rust-test` flake check has neither).
fn real_scalafmt() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LS_SCALAFMT") {
        return Some(PathBuf::from(path));
    }
    let path = std::env::var("PATH").ok()?;
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join("scalafmt"))
        .find(|candidate| candidate.is_file())
}

/// `scalafmt --version` → the version string, so the fixture conf pins
/// exactly the binary the server will spawn (scalafmt refuses — and, offline,
/// cannot download — any other version).
fn scalafmt_version(bin: &Path) -> String {
    let output = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .expect("run scalafmt --version");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .last()
        .expect("scalafmt --version prints `scalafmt <version>`")
        .to_string()
}

// Deliberately misformatted: line 0 is ALREADY formatted (so a minimal diff
// leaves it untouched), lines 1-2 need edits.
const MISFORMATTED: &str = "object Fmt {\n  def  f( x:Int ) : Int   =x+1\n  val   y=2\n}\n";

// The full round trip against the real binary: minimal (not whole-file)
// edits over a misformatted OPEN buffer, applied client-side and proven
// idempotent — the second format over the applied text answers [].
#[test]
fn formatting_round_trips_minimal_edits_against_a_real_scalafmt() {
    let Some(bin) = real_scalafmt() else {
        eprintln!("formatting_wire: skipping (no scalafmt via LS_SCALAFMT or PATH)");
        return;
    };
    let version = scalafmt_version(&bin);
    let mut client = boot();
    client.initialize();
    client.await_ready();
    std::fs::write(
        client.workspace().join(".scalafmt.conf"),
        format!("version = \"{version}\"\nrunner.dialect = scala3\n"),
    )
    .unwrap();

    let uri = client.file_uri("Fmt.scala");
    client.did_open_uri(&uri, MISFORMATTED);
    let result = client.result(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }),
    );
    let edits = result.as_array().expect("a TextEdit array").clone();
    assert!(
        !edits.is_empty(),
        "the misformatted buffer must yield edits"
    );
    // Minimal, not whole-file: the already-formatted first line is untouched.
    assert!(
        edits
            .iter()
            .all(|edit| edit["range"]["start"]["line"].as_u64().unwrap() >= 1),
        "no edit may touch the already-formatted line 0: {edits:?}"
    );

    // Apply client-side (ASCII fixture: characters == bytes per line),
    // bottom-up so original-addressed ranges stay valid.
    let applied = apply_edits(MISFORMATTED, &edits);
    assert!(applied.contains("def f(x: Int): Int = x + 1"), "{applied}");
    assert!(applied.contains("val y = 2"), "{applied}");

    // Idempotence: the applied text formats to the empty edit list.
    client.did_change_uri(&uri, &applied);
    let second = client.result(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }),
    );
    assert_eq!(second, json!([]), "a formatted buffer must answer []");
    client.shutdown();
}

// A file the document store does not hold is the typed "not open" error —
// formatting serves the OPEN buffer only, never a disk file behind the
// editor's back. No scalafmt needed: the gate fires before resolution.
#[test]
fn formatting_a_not_open_file_is_a_typed_error() {
    let mut client = boot();
    client.initialize();
    client.await_ready();
    let uri = client.file_uri("NeverOpened.scala");
    let message = client.error_message(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2 } }),
    );
    assert!(message.contains("is not open"), "{message}");
    client.shutdown();
}

// An open buffer in a workspace without a root `.scalafmt.conf` is the typed
// no-config error (scalafmt requires a pinned version) — again before any
// binary resolution, so the case runs without scalafmt installed.
#[test]
fn formatting_without_a_scalafmt_conf_is_a_typed_error() {
    let mut client = boot();
    client.initialize();
    client.await_ready();
    let uri = client.file_uri("Open.scala");
    client.did_open_uri(&uri, "object   Open\n");
    let message = client.error_message(
        "textDocument/formatting",
        json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2 } }),
    );
    assert_eq!(
        message,
        "no .scalafmt.conf in the workspace (scalafmt requires a pinned version)"
    );
    client.shutdown();
}

/// Applies original-addressed, ascending TextEdits bottom-up over an ASCII
/// document (character == byte within a line — true for the fixture).
fn apply_edits(original: &str, edits: &[Value]) -> String {
    let mut text = original.to_string();
    for edit in edits.iter().rev() {
        let start = offset_of(&text, &edit["range"]["start"]);
        let end = offset_of(&text, &edit["range"]["end"]);
        let new_text = edit["newText"].as_str().unwrap();
        text.replace_range(start..end, new_text);
    }
    text
}

fn offset_of(text: &str, position: &Value) -> usize {
    let line = position["line"].as_u64().unwrap() as usize;
    let character = position["character"].as_u64().unwrap() as usize;
    let line_start: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    line_start + character
}
