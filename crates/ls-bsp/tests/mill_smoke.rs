//! Live mill BSP smoke: drive the real mill build server built from
//! `it/sample-workspace` through the `ls-bsp` client — discovery, launch,
//! handshake, project-model load, the core requests, and a forced compile
//! diagnostic.
//!
//! Gated on `LS_BSP_MILL_SMOKE=1` (mirroring the Scala `LS_REAL_BSP_IT` real-BSP
//! gate) so ordinary `cargo test` and the hermetic Nix check skip it — mill
//! needs a JVM/toolchain those sandboxes forbid. Run it via
//! `scripts/it-mill-smoke.sh` under `nix develop`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ls_bsp::protocol::PublishDiagnosticsParams;
use ls_bsp::{
    BspClientHandlers, BspCompileOutcome, BspDiscovery, BspSession, BspSessionConfig,
    ProjectModelLoader,
};

fn repo_root() -> PathBuf {
    if let Ok(root) = std::env::var("LS_REPO_ROOT") {
        return PathBuf::from(root);
    }
    // crates/ls-bsp -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

#[test]
fn mill_bsp_live_smoke() {
    if std::env::var("LS_BSP_MILL_SMOKE").is_err() {
        eprintln!("mill_bsp_live_smoke: skipped (set LS_BSP_MILL_SMOKE=1 to run)");
        return;
    }

    let sample = repo_root().join("it").join("sample-workspace");
    assert!(sample.is_dir(), "sample workspace not found at {sample:?}");

    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("sample-workspace");
    copy_dir(&sample, &ws);

    // Generate the .bsp/mill-bsp.json connection file with real mill.
    let install = Command::new("mill")
        .args(["--no-daemon", "mill.bsp.BSP/install"])
        .current_dir(&ws)
        .status()
        .expect("run mill BSP/install");
    assert!(install.success(), "mill BSP/install failed");

    let details = BspDiscovery::required(&ws)
        .expect("mill-bsp connection file")
        .details;
    assert_eq!(details.name, "mill-bsp");

    let diagnostics: Arc<Mutex<Vec<PublishDiagnosticsParams>>> = Arc::new(Mutex::new(Vec::new()));
    let handlers = BspClientHandlers::new().on_diagnostics({
        let q = Arc::clone(&diagnostics);
        move |d| q.lock().unwrap().push(d)
    });

    let config = BspSessionConfig {
        request_timeout: Duration::from_secs(240),
        shutdown_timeout: Duration::from_secs(20),
        ..BspSessionConfig::default()
    };
    let session =
        BspSession::launch(ws.clone(), &details, handlers, config).expect("launch mill bsp");

    let init = session.initialize().expect("initialize");
    eprintln!("server: {} {}", init.display_name, init.version);
    eprintln!("capabilities: {:?}", session.server_capabilities());

    // Load the project model from the real server.
    let model = ProjectModelLoader::load(&session).expect("load model");
    for t in &model.targets {
        eprintln!(
            "target id={} display={} indexable={} deps={:?} sdb={:?} sources={}",
            t.bsp_id,
            t.display_name,
            t.indexable(),
            t.direct_deps,
            t.semanticdb_root,
            t.sources.len()
        );
    }

    // Identify a/b/c by the module directory their sources live under: robust to
    // mill's target-id scheme and any extra (mill-build) targets.
    let owns = |name: &str| {
        let module_src = ws.join(name).join("src");
        move |t: &&ls_bsp::BspTarget| t.sources.iter().any(|s| s.starts_with(&module_src))
    };
    let a = model.targets.iter().find(owns("a")).expect("target a");
    let b = model.targets.iter().find(owns("b")).expect("target b");
    let c = model.targets.iter().find(owns("c")).expect("target c");

    // a and b build with -Xsemanticdb; c deliberately does not.
    assert!(a.indexable(), "target a should produce SemanticDB");
    assert!(b.indexable(), "target b should produce SemanticDB");
    assert!(!c.indexable(), "target c must NOT produce SemanticDB");
    assert!(
        b.direct_deps.contains(&a.bsp_id),
        "b should depend on a: {:?}",
        b.direct_deps
    );

    let (a_id, b_id, c_id) = (a.bsp_id.clone(), b.bsp_id.clone(), c.bsp_id.clone());
    let ids = vec![a_id.clone(), b_id.clone(), c_id.clone()];

    // The core requests against the real server.
    let targets = session
        .workspace_build_targets()
        .expect("workspace/buildTargets");
    assert!(targets.len() >= 3, "expected at least a/b/c targets");
    assert!(!session
        .build_target_sources(&ids)
        .expect("sources")
        .is_empty());
    assert!(!session
        .build_target_scalac_options(&ids)
        .expect("scalacOptions")
        .is_empty());

    // Force a deterministic diagnostic: give a's Greeting an ill-typed body,
    // then compile a and expect a failure with forwarded diagnostics.
    let greeting = ws.join("a").join("src").join("pkga").join("Greeting.scala");
    std::fs::write(
        &greeting,
        "package pkga\n\nclass Greeting(val name: String):\n  def message: String = 42\n\nobject Greeting:\n  def default: Greeting = new Greeting(\"world\")\n",
    )
    .unwrap();

    let outcome = session
        .compile(
            std::slice::from_ref(&a_id),
            Some("smoke-origin".to_string()),
        )
        .expect("compile a");
    eprintln!("compile outcome: {outcome:?}");
    assert!(
        matches!(outcome, BspCompileOutcome::Failed { .. }),
        "expected a failed compile, got {outcome:?}"
    );

    // Diagnostics arrive as build/publishDiagnostics while the compile runs;
    // allow a brief window in case any trail the response.
    let deadline = Instant::now() + Duration::from_secs(10);
    let error_diag = loop {
        if let Some(d) = diagnostics
            .lock()
            .unwrap()
            .iter()
            .find(|d| !d.diagnostics.is_empty())
            .cloned()
        {
            break Some(d);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let error_diag = error_diag.expect("a publishDiagnostics with at least one entry");
    eprintln!("error diagnostic: {error_diag:?}");
    assert!(
        error_diag.text_document.uri.ends_with("Greeting.scala"),
        "diagnostic should target Greeting.scala, got {}",
        error_diag.text_document.uri
    );
    assert!(
        error_diag.build_target.is_some(),
        "diagnostic should carry a buildTarget"
    );
    assert_eq!(
        error_diag.origin_id,
        Some("smoke-origin".to_string()),
        "publishDiagnostics should echo the compile originId"
    );
    assert!(
        error_diag.diagnostics.iter().any(|d| d.severity == Some(1)),
        "expected an error-severity diagnostic, got {:?}",
        error_diag.diagnostics
    );

    session.shutdown();
    assert!(session.is_closed());
}
