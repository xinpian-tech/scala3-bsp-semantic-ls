//! Live embedded-island integration: boots the PRODUCTION island
//! (`boot_island`) with the real PC-host assembly against a real JVM and drives
//! the PC lifecycle + real queries through the 15-slot vtable to a live Scala
//! presentation compiler. This exercises the production boundary end-to-end —
//! boot + registration, the flat-codec round-trip, and loaned-thread dispatch
//! serialization — against a real compiler, not the spike echo op or the
//! Scala-only unit seam.
//!
//! It is env-gated so a bare `cargo test` (no JVM present) still passes:
//!   LS_LIBJVM              = <jdk>/lib/server/libjvm.so
//!   PC_HOST_AGENT_JAR      = the pcHost assembly jar (self-contained premain)
//!   LS_PC_TARGET_CLASSPATH = ':'-separated jars for the registered target's
//!                            classpath (the Scala standard library)
//! When any is absent the test logs a skip and returns (still green), exactly
//! as the boundary spike tests skip without `LS_LIBJVM`/`SPIKE_AGENT_JAR`.

use std::path::PathBuf;
use std::time::Duration;

use ls_jvm::watchdog::{PcRequest, QueryKind};
use ls_jvm::{boot_island, IslandConfig};
use ls_pc_abi::payloads::{CompletionList, HoverResult, TargetConfig};

struct Harness {
    libjvm: PathBuf,
    agent_jar: PathBuf,
    target_classpath: Vec<String>,
}

/// Reads the boot env; `None` (→ skip) when the live-JVM inputs are absent, so
/// a cold `cargo test` run without a JVM still passes.
fn harness() -> Option<Harness> {
    let libjvm = PathBuf::from(std::env::var_os("LS_LIBJVM")?);
    let agent_jar = PathBuf::from(std::env::var_os("PC_HOST_AGENT_JAR")?);
    let target_classpath = std::env::var("LS_PC_TARGET_CLASSPATH")
        .ok()?
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    Some(Harness {
        libjvm,
        agent_jar,
        target_classpath,
    })
}

#[test]
fn live_island_answers_real_pc_queries_over_the_production_vtable() {
    let Some(harness) = harness() else {
        eprintln!(
            "live_boundary: skipping — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run the live embedded-JVM boundary test"
        );
        return;
    };

    let config = IslandConfig {
        libjvm: &harness.libjvm,
        agent_jar: &harness.agent_jar,
        extra_classpath: &[],
        workspace_root: None,
        rendezvous_timeout: Duration::from_secs(30),
        max_abandoned_generations: 4,
        // Generous: the first query pays the one-time compiler warm-up.
        request_deadline: Duration::from_secs(120),
        cancel_grace: Duration::from_millis(200),
    };
    let mut supervisor = boot_island(&config).expect("the production island boots");

    // A registered target carrying the Scala standard library, exactly as the
    // BSP layer supplies a real target's classpath.
    let target_id = "live-target".to_string();
    supervisor
        .request(PcRequest::RegisterTarget {
            id: target_id.clone(),
            config: TargetConfig {
                bsp_id: target_id.clone(),
                scala_version: "3.8.4".to_string(),
                classpath: harness.target_classpath.clone(),
                scalac_options: vec![],
                source_dirs: vec![],
            },
        })
        .expect("register_target");

    // An open buffer whose final line dots off a `List[Int]`.
    let uri = "file:///live/Main.scala".to_string();
    let source = "object Main:\n  val xs = List(1, 2, 3)\n  val ys = xs.\n";
    supervisor
        .request(PcRequest::DidOpen {
            target_id: target_id.clone(),
            uri: uri.clone(),
            text: source.to_string(),
        })
        .expect("did_open");

    // Completion right after the `.` on line 2 → members of `List[Int]` from the
    // live compiler, proving register → open → query round-trips through the
    // flat codec and the loaned dispatch lane.
    let dot_line = "  val ys = xs.";
    let completion = supervisor
        .request(PcRequest::Query {
            kind: QueryKind::Completion,
            uri: uri.clone(),
            line: 2,
            character: dot_line.chars().count() as u32,
        })
        .expect("completion query");
    let list = CompletionList::decode(&completion).expect("decode completion list");
    let labels: Vec<&str> = list.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.iter().any(|label| label.starts_with("map")),
        "expected a `map` member completion on List[Int] from the live compiler; \
         got {} items: {:?}",
        labels.len(),
        labels
    );

    // A second op type over the same boundary: hover on the `List` application.
    // The response must decode (a live compiler returns markup, but hover is
    // nullable, so we require only a well-formed decode here — hover content is
    // asserted by the Scala-side query suites).
    let list_col = "  val xs = Li"; // a column inside the `List` token on line 1
    let hover = supervisor
        .request(PcRequest::Query {
            kind: QueryKind::Hover,
            uri,
            line: 1,
            character: list_col.chars().count() as u32,
        })
        .expect("hover query");
    HoverResult::decode(&hover).expect("decode hover result");
}
