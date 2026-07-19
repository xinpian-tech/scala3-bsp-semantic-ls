//! Watched-files dynamic registration over the framed wire, JVM-free: a client
//! that advertises `workspace.didChangeWatchedFiles.dynamicRegistration` drives
//! the REAL serve loop + REAL `IndexBootstrap` (fake BSP corpus, fake PC), and
//! the suite proves the full fire-and-forget round trip — the
//! `client/registerCapability` request with the three watcher globs, the
//! client's reply consumed cleanly — and the event reactions end-to-end: a NEW
//! symbol's `.semanticdb` dropped into the (writable temp copy of the)
//! targetroot becomes searchable through `workspace/symbol` after the watched
//! event triggers the debounced background reingest, and a watched
//! `config.json` event reaches `PcQueryService::on_config_changed`. The
//! save-driven-reingest geometry of `fake_bsp_e2e.rs`/`real_bsp_e2e.rs`, keyed
//! off the client watcher instead of `didSave`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ls_bsp::uri::path_to_uri;
use ls_server::IndexBootstrap;
use ls_testkit::client::WireClient;
use ls_testkit::fake_bsp::FakeBsp;
use ls_testkit::fake_pc::FakePcService;
use ls_testkit::fixtures::{build_target, copy_corpus_dir, fixtures_root};

/// The client capabilities advertising watched-files dynamic registration.
fn watching_capabilities() -> Value {
    json!({ "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } } })
}

/// Boot the production serve loop over the fake BSP corpus with ONE indexable
/// target (`fixture-a`) whose targetroot is a WRITABLE temp copy of the
/// committed `out-a`, so the suite can drop new SemanticDB into it. Returns the
/// client and the shared fake-PC handle.
fn boot_with_writable_targetroot() -> (WireClient, Arc<FakePcService>) {
    let pc = FakePcService::new();
    let pc_for_factory = Arc::clone(&pc);
    let client = WireClient::boot_in_process_with(move |parts| {
        let (fake, source) =
            FakeBsp::start(Arc::clone(&parts.reload_flag), Arc::clone(&parts.sink));
        let ws = fake.workspace_root.clone();
        copy_corpus_dir("out-a", &ws.join("out-a"));
        fake.server.set_targetroot_base(ws.clone());
        // Only fixture-a: the out-c corpus (holding `UseC`) stays un-ingested
        // until the suite copies its semanticdb into the writable targetroot.
        fake.server
            .set_targets(vec![build_target("fixture-a", &[])]);
        let bootstrap = IndexBootstrap::with_pc(source, FakePcService::factory(pc_for_factory));
        (ws, fake, bootstrap)
    });
    (client, pc)
}

fn symbol_names(result: &Value) -> Vec<String> {
    result
        .as_array()
        .expect("symbol array")
        .iter()
        .filter_map(|s| s["name"].as_str().map(str::to_string))
        .collect()
}

// The full watched-files flow over the wire: registration round trip, then a
// new `.semanticdb` dropped on disk + the watched event -> the debounced
// background reingest makes the new symbol searchable; a config.json event
// nudges the PC config seam; a .bsp event leaves the session serviceable.
#[test]
fn a_watched_semanticdb_event_drives_a_background_reingest_over_the_wire() {
    let (mut client, pc) = boot_with_writable_targetroot();
    client.initialize_with_capabilities(watching_capabilities());

    // The fire-and-forget registration: a server-side string id and the three
    // watcher globs; reply OK (the loop consumes it without an error frame).
    let registration = client.await_server_request("client/registerCapability");
    assert_eq!(registration["id"], "ls-server/1", "{registration}");
    let request = &registration["params"]["registrations"][0];
    assert_eq!(request["method"], "workspace/didChangeWatchedFiles");
    assert_eq!(
        request["registerOptions"]["watchers"],
        json!([
            { "globPattern": "**/*.semanticdb" },
            { "globPattern": "**/.scala3-bsp-semantic-ls/config.json" },
            { "globPattern": "**/.bsp/*.json" },
        ]),
        "{registration}"
    );
    client.respond(registration["id"].clone(), Value::Null);
    client.await_ready();

    // The new symbol is absent before the event: out-c never bootstrapped.
    let before = client.result("workspace/symbol", json!({ "query": "UseC" }));
    assert!(
        !symbol_names(&before).contains(&"UseC".to_string()),
        "UseC must be absent before the watched reingest: {before}"
    );

    // A build outside the editor: the out-c corpus document (defining `UseC`,
    // whose source lives under the SAME shared sourceroot) lands in the
    // writable targetroot, then the client watcher reports it.
    let rel = "META-INF/semanticdb/c/src/pkga/CopyCore.scala.semanticdb";
    let new_sdb = client.workspace().join("out-a").join(rel);
    std::fs::create_dir_all(new_sdb.parent().unwrap()).unwrap();
    std::fs::copy(fixtures_root().join("out-c").join(rel), &new_sdb).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": path_to_uri(&new_sdb), "type": 1 }] }),
    );

    // The reingest is debounced + background: poll until the symbol appears.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let symbols = client.result("workspace/symbol", json!({ "query": "UseC" }));
        if symbol_names(&symbols).contains(&"UseC".to_string()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the watched semanticdb event never made UseC searchable: {symbols}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // A watched config.json event reaches the PC config seam.
    let config = client
        .workspace()
        .join(".scala3-bsp-semantic-ls/config.json");
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": path_to_uri(&config), "type": 1 }] }),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pc.calls().iter().any(|c| c == "on_config_changed") {
        assert!(
            Instant::now() < deadline,
            "the watched config event never reached on_config_changed: {:?}",
            pc.calls()
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // A watched .bsp/*.json event only logs; the session stays serviceable.
    let bsp = client.workspace().join(".bsp/fake-bsp.json");
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": path_to_uri(&bsp), "type": 2 }] }),
    );
    let after = client.result("workspace/symbol", json!({ "query": "Core" }));
    assert!(
        !symbol_names(&after).is_empty(),
        "the session must stay serviceable after a .bsp event: {after}"
    );
    client.shutdown();
}
