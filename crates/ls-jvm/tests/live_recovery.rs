//! Live embedded-island recovery integration: boots the PRODUCTION island with
//! a test-only fault hook (`-Dls.pc.host.testFault=busyCompletion`) so a real
//! dispatch-lane completion wedges non-cooperatively, and proves the Rust
//! watchdog's dispatch-generation recovery ladder over the REAL boundary:
//!   * a wedged request fails typed (`PcError::RequestTimeout`);
//!   * the watchdog loans a fresh dispatch generation via the real
//!     `spawn_dispatch` slot and replays the mirrored targets/buffers into it;
//!   * a later completion on the replayed buffer works without the editor
//!     re-registering or reopening it;
//!   * exceeding the abandoned-generation cap is island-fatal.
//!
//! Env-gated exactly like the live sweep (`LS_LIBJVM` + `PC_HOST_AGENT_JAR` +
//! `LS_PC_TARGET_CLASSPATH`); skips cleanly when unset. This is a separate test
//! binary from the sweep because only one JVM can boot per process and this one
//! needs the fault property set at boot.

use std::path::PathBuf;
use std::time::Duration;

use ls_jvm::backend::VtableBackend;
use ls_jvm::watchdog::{PcError, PcRequest, QueryKind, Supervisor};
use ls_jvm::{boot_island, IslandConfig};
use ls_pc_abi::payloads::{CompletionList, TargetConfig};

const TARGET_ID: &str = "recovery-target";
// The fault hook wedges a completion whose URI contains "wedge"; the normal
// buffer's URI does not, so completions on it run through to the compiler.
const NORMAL_URI: &str = "file:///live/recovery/Main.scala";
const WEDGE_URI: &str = "file:///live/recovery/Wedge.scala";
const SOURCE: &str = "object Main:\n  val xs = List(1, 2, 3)\n  val ys = xs.\n";

struct Env {
    libjvm: PathBuf,
    agent_jar: PathBuf,
    classpath: Vec<String>,
}

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

/// Run a completion at the `.` on line 2 of `SOURCE` for `uri`.
fn complete(sup: &mut Supervisor<VtableBackend>, uri: &str) -> Result<Vec<u8>, PcError> {
    sup.request(PcRequest::Query {
        kind: QueryKind::Completion,
        uri: uri.to_string(),
        line: 2,
        character: 14,
    })
}

/// Does the completion reply list a `map` member of `List[Int]`?
fn has_map(reply: &[u8]) -> bool {
    CompletionList::decode(reply)
        .expect("decode completion list")
        .items
        .iter()
        .any(|item| item.label.starts_with("map"))
}

#[test]
fn live_dispatch_wedge_recovers_via_a_fresh_generation_and_caps_at_fatal() {
    let Some(env) = env() else {
        eprintln!(
            "live_recovery: skipping — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run the live recovery test"
        );
        return;
    };

    let workspace = std::env::temp_dir().join(format!("ls-live-recovery-{}", std::process::id()));
    std::fs::create_dir_all(&workspace).expect("create workspace root");

    // Boot with the fault hook armed and a cap of 1 abandoned generation (so the
    // second non-cooperative wedge is fatal). The per-request deadline sits well
    // below the 60s wedge busy-loop (so a wedge still times out) yet high enough
    // that a cold first completion has slack when several live JVM checks build
    // in parallel under CI load.
    let fault = ["-Dls.pc.host.testFault=busyCompletion".to_string()];
    let config = IslandConfig {
        libjvm: &env.libjvm,
        agent_jar: &env.agent_jar,
        extra_classpath: &[],
        workspace_root: Some(&workspace),
        extra_jvm_options: &fault,
        rendezvous_timeout: Duration::from_secs(30),
        max_abandoned_generations: 1,
        request_deadline: Duration::from_secs(20),
        cancel_grace: Duration::from_millis(500),
    };
    let mut sup = boot_island(&config).expect("the production island boots");

    // Register the target and open both buffers (open never wedges; only a
    // completion on the wedge URI does).
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
    for uri in [NORMAL_URI, WEDGE_URI] {
        sup.request(PcRequest::DidOpen {
            target_id: TARGET_ID.to_string(),
            uri: uri.to_string(),
            text: SOURCE.to_string(),
        })
        .expect("did_open");
    }

    // A healthy completion warms generation 0.
    let healthy = complete(&mut sup, NORMAL_URI).expect("healthy completion");
    assert!(has_map(&healthy), "warm-up completion should list `map`");
    assert_eq!(sup.generation(), 0);

    // A non-cooperative wedge: the completion busy-loops on the dispatch lane,
    // ignoring the PC restart, so the request times out typed and the watchdog
    // must escalate to a fresh dispatch generation via the real spawn_dispatch.
    assert_eq!(complete(&mut sup, WEDGE_URI), Err(PcError::RequestTimeout));
    assert_eq!(
        sup.generation(),
        1,
        "a non-cooperative wedge must advance to a new dispatch generation"
    );
    assert!(!sup.is_fatal(), "one abandoned generation is under the cap");

    // The recovered generation serves completions on the replayed buffer with no
    // editor re-register / re-open: the real spawn_dispatch loaned a working
    // dispatch thread and Rust replayed the mirrored target + buffers into it.
    let recovered = complete(&mut sup, NORMAL_URI).expect("completion after recovery");
    assert!(
        has_map(&recovered),
        "completion must work on the recovered generation without reopening the buffer"
    );

    // A second non-cooperative wedge abandons another generation, exceeding the
    // cap of 1 → island-fatal (an orderly process exit, not accumulating wedged
    // dispatch threads).
    assert_eq!(complete(&mut sup, WEDGE_URI), Err(PcError::RequestTimeout));
    assert!(
        sup.is_fatal(),
        "exceeding the abandoned-generation cap must set the island fatal"
    );
}
