//! Real-BSP presentation-compiler rows: the position features
//! (hover/signatureHelp/definition) and dirty-buffer completion, driven over the
//! framed LSP wire against a REAL mill build server + the REAL embedded PC island.
//! A port of the PC half of the Scala `RealBspCoreTest`/`RealBspIntegrationTest`.
//!
//! This is its OWN integration-test binary (one process) that boots the embedded
//! JVM/island exactly once, because only one island can boot per process. All the
//! PC operations run against that single booted island.
//!
//! Gated on `LS_REAL_BSP_IT=1` (mill) AND the full PC env
//! (`LS_LIBJVM` + `PC_HOST_AGENT_JAR` + `LS_PC_TARGET_CLASSPATH`), like
//! `live_pc.rs`; skips cleanly when either is absent.

mod real_bsp_common;

use serde_json::{json, Value};

use real_bsp_common::*;

#[test]
fn real_bsp_presentation_compiler_position_features_and_completion() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    if !pc_enabled() {
        return skip(
            "real_bsp: skipping PC features — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run them",
        );
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    let greeting_text = source_text(&ws, GREETING);
    server.did_open(GREETING, &greeting_text);

    // hover (PC) answers on an indexed symbol.
    let (ml, mc) = position_of(&greeting_text, "message", 0);
    let hover = server.result(
        "textDocument/hover",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(ml, mc)}),
    );
    assert!(!hover.is_null(), "expected a non-null hover for message");

    // signatureHelp (PC) answers at the constructor call site.
    let (cl, cc) = position_of(&greeting_text, "new Greeting(", 0);
    let sig = server.result(
        "textDocument/signatureHelp",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(cl, cc + "new Greeting(".len() as u32)}),
    );
    let signatures = sig.get("signatures").and_then(Value::as_array);
    assert!(
        signatures.is_some_and(|s| !s.is_empty()),
        "expected a signature: {sig}"
    );

    // definition (PC) resolves the `Greeting` in `new Greeting("world")` (the 4th
    // whole occurrence) to its declaration in the same file.
    let (dl, dc) = position_of(&greeting_text, "Greeting", 3);
    let definition = server.result(
        "textDocument/definition",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(dl, dc)}),
    );
    let greeting_uri = server.file_uri(GREETING);
    let resolved = definition
        .as_array()
        .map(|locs| {
            locs.iter()
                .any(|l| l.get("uri").and_then(Value::as_str) == Some(greeting_uri.as_str()))
        })
        .unwrap_or(false);
    assert!(
        resolved,
        "expected the definition in Greeting.scala: {definition}"
    );
    server.did_close(GREETING);

    // A dirty buffer with a member-select the forked worker must complete against
    // the real classpath.
    let probe = "  val probe = greeting.mess";
    let dirty = format!("{}{probe}\n", source_text(&ws, CONSUMER));
    server.did_open(CONSUMER, &dirty);
    let line = dirty.lines().count() as u32 - 1;
    let character = probe.len() as u32;
    let completion = server.result(
        "textDocument/completion",
        json!({"textDocument": server.text_doc(CONSUMER), "position": position_json(line, character)}),
    );
    let items = completion
        .get("items")
        .or(Some(&completion))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        items.iter().any(|i| i
            .get("label")
            .and_then(Value::as_str)
            .is_some_and(|l| l.starts_with("message"))),
        "forked completion should offer message: {completion}"
    );
    server.did_close(CONSUMER);

    // The PC operations booted the embedded island in-process (the whole point of
    // isolating this binary), so libjvm is now mapped.
    assert!(
        ls_server::libjvm_mapped(),
        "the PC operations should have booted the embedded island"
    );

    server.shutdown();
}
