//! Real-BSP dispatch-generation recovery: proves the embedded PC island recovers
//! from a NON-cooperative dispatch wedge over the real-BSP LSP server path (a
//! retargeted port of the Scala forked-worker-kill scenario to the Rust
//! dispatch-generation model). Boots a real mill workspace through the production
//! `LiveBspModelSource`, warms the presentation compiler with a normal completion,
//! wedges a completion on a `wedge`-marked buffer (the Java test fault hook busy-
//! loops the dispatch lane), and asserts:
//!   * the wedged request does not resolve (it times out);
//!   * a later completion on the NORMAL buffer works again WITHOUT the client
//!     re-opening or re-registering — the watchdog loaned a fresh dispatch
//!     generation and replayed the mirrored buffers into it;
//!   * the server stays up and the island stays booted (one abandoned generation
//!     is under the cap), i.e. the language server recovered rather than dying.
//!
//! Its OWN integration-test binary (one process, one island boot) armed with the
//! fault: gated on `LS_REAL_BSP_IT=1` (mill) + the full PC env + `LS_PC_TEST_FAULT`
//! (which `IslandPcService::boot` reads to arm `-Dls.pc.host.testFault` and tighten
//! the request deadline). Skips cleanly when any is absent. The lower-level
//! dispatch-generation ladder — including the abandoned-generation fatal cap — is
//! proven at the island boundary by `ls-jvm`'s `live_recovery` suite; this test
//! proves the recovery over the real mill-loaded server path.

mod real_bsp_common;

use serde_json::{json, Value};

use real_bsp_common::*;

const NORMAL: &str = "a/src/pkga/Normalcy.scala";
const WEDGE: &str = "a/src/pkga/Wedge.scala";
// On-disk (compilable) sources so module a compiles; the dirty completion buffers
// append a `val ys = xs.` member-select the PC resolves. The wedge buffer's uri
// carries the `wedge` marker the fault hook keys off (case-insensitive).
const NORMAL_SRC: &str = "package pkga\n\nobject Normalcy:\n  val xs = List(1, 2, 3)\n";
const WEDGE_SRC: &str = "package pkga\n\nobject Wedge:\n  val xs = List(1, 2, 3)\n";

fn dirty(src: &str) -> String {
    format!("{src}  val ys = xs.\n")
}

/// A completion at the `.` of the appended `val ys = xs.` member-select.
fn complete(server: &mut RealServer, rel: &str, text: &str) -> Value {
    let (line, col) = position_of(text, "xs.", 0);
    let params = json!({
        "textDocument": server.text_doc(rel),
        "position": position_json(line, col + "xs.".len() as u32),
    });
    server.request("textDocument/completion", params)
}

/// Does the completion response list a member with the given label prefix?
fn lists(response: &Value, prefix: &str) -> bool {
    response
        .get("result")
        .and_then(|r| r.get("items").or(Some(r)))
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().any(|i| {
                i.get("label")
                    .and_then(Value::as_str)
                    .is_some_and(|l| l.starts_with(prefix))
            })
        })
        .unwrap_or(false)
}

#[test]
fn real_bsp_forked_pc_recovers_from_a_dispatch_wedge() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    if !pc_enabled() {
        return skip(
            "real_bsp: skipping PC recovery — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run it",
        );
    }
    if std::env::var_os("LS_PC_TEST_FAULT").is_none() {
        return skip(
            "real_bsp: skipping PC recovery — set LS_PC_TEST_FAULT=busyCompletion to arm the \
             dispatch-wedge fault hook",
        );
    }
    let (tmp, ws) = prepare_workspace(&[(NORMAL, NORMAL_SRC), (WEDGE, WEDGE_SRC)], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    let normal_dirty = dirty(NORMAL_SRC);
    let wedge_dirty = dirty(WEDGE_SRC);
    server.did_open(NORMAL, &normal_dirty);
    server.did_open(WEDGE, &wedge_dirty);

    // Warm the presentation compiler: a healthy completion on the normal buffer
    // lists `List`'s `map` member (generation 0).
    assert!(
        lists(&complete(&mut server, NORMAL, &normal_dirty), "map"),
        "the warm-up completion should list map"
    );

    // A non-cooperative wedge on the `wedge`-marked buffer: the fault hook busy-
    // loops the dispatch lane, so the request times out and the watchdog escalates
    // to a fresh dispatch generation. However it resolves over the wire (typed
    // error or empty), the wedged completion must not list the member.
    let wedged = complete(&mut server, WEDGE, &wedge_dirty);
    assert!(
        !lists(&wedged, "map"),
        "the wedged completion must not resolve: {wedged}"
    );

    // Recovery: a completion on the NORMAL buffer works again with no client
    // re-open / re-register — the watchdog replayed the mirrored target + buffers
    // into the loaned dispatch generation.
    assert!(
        lists(&complete(&mut server, NORMAL, &normal_dirty), "map"),
        "completion must work after the dispatch-generation recovery, without reopening"
    );

    // The server survived (one abandoned generation is under the cap): the doctor
    // still answers ready and the island is still booted, not dead.
    let doctor = server.execute_command(DOCTOR);
    assert!(
        doctor.contains("state: ready"),
        "the server should still be ready after recovery:\n{doctor}"
    );
    assert!(
        ls_server::libjvm_mapped(),
        "the embedded island should still be booted after recovery"
    );

    server.did_close(NORMAL);
    server.did_close(WEDGE);
    server.shutdown();
}
