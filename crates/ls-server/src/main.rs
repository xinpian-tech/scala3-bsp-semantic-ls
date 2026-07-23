//! The `scala3-bsp-semantic-ls` stdio LSP server binary — the production entry
//! point. Ports `ls.core.Main.main`: `--version` and `--doctor [dir]` print and
//! exit; otherwise the JSON-RPC server runs over stdin/stdout with the
//! production [`CoreHandlers`] and the live-BSP [`IndexBootstrap`].
//!
//! stdout is reserved for the protocol: diagnostics and log lines go to stderr,
//! so the JSON-RPC stream is never corrupted.

use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use ls_bsp::protocol::PublishDiagnosticsParams as BspPublishDiagnosticsParams;
use ls_server::doctor::DoctorReport;
use ls_server::{
    dump_report, parse_args, resolve_doctor_dir, serve, CliAction, CoreHandlers, DiagnosticRouter,
    IndexBootstrap, LiveBspModelSource, OutputSink, PcDiagnosticsLayer, PublishDiagnosticsParams,
    ServerCore, SERVER_NAME, SERVER_VERSION,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = parse_args(&args);
    // The log sink (stderr + optional LS_LOG_FILE tee) is installed before any
    // subsystem runs, and its banner line always prints — so a captured stderr
    // stream identifies the process even when everything after it goes wrong.
    ls_server::logging::init(mode_name(&action));
    match action {
        CliAction::Version => {
            println!("{}", version_line());
            ExitCode::SUCCESS
        }
        CliAction::Doctor { dir, json } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            println!(
                "{}",
                offline_doctor_report(&resolve_doctor_dir(&dir, &cwd), json)
            );
            ExitCode::SUCCESS
        }
        CliAction::Dump { dir } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            print!("{}", dump_report(&resolve_doctor_dir(&dir, &cwd)));
            ExitCode::SUCCESS
        }
        CliAction::Serve => {
            serve_stdio();
            ExitCode::SUCCESS
        }
        CliAction::Usage { message } => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// `--version`: the server name and version, matching the Scala
/// `println(s"${ScalaLs.ServerName} ${ScalaLs.ServerVersion}")`.
fn version_line() -> String {
    format!("{SERVER_NAME} {SERVER_VERSION}")
}

/// The argv mode named in the log banner.
fn mode_name(action: &CliAction) -> &'static str {
    match action {
        CliAction::Version => "version",
        CliAction::Doctor { .. } => "doctor",
        CliAction::Dump { .. } => "dump",
        CliAction::Serve => "serve",
        CliAction::Usage { .. } => "usage",
    }
}

/// `--doctor [dir]`: the offline report over the typed [`DoctorReport`] model.
/// Pre-bootstrap the build server and presentation compiler are not connected, so
/// the live `BSP`/`SemanticDB`/`PC`/`PC Plugins` sections render `unavailable`;
/// `Runtime`/`Nix`/`Store` are gathered from the host, the workspace files, and
/// the read-only on-disk store. `--json` returns the structured object. Boots no
/// JVM.
fn offline_doctor_report(root: &Path, json: bool) -> String {
    let report = DoctorReport::offline(root);
    let header = format!(
        "{SERVER_NAME} {SERVER_VERSION}\n\
         state: offline (--doctor): build server and presentation compiler not connected\n\
         workspace: {}",
        root.display()
    );
    if json {
        report.render_json().to_string()
    } else {
        format!("{header}\n\n{}", report.render_text())
    }
}

/// Runs the stdio JSON-RPC server: the live-BSP bootstrap on `initialized`, the
/// production handlers for ready requests. stdout carries only protocol frames.
/// stdin is wrapped unlocked: `serve`'s scoped reader thread needs a `Send`
/// reader, and a `StdinLock` guard is not `Send` (nothing else reads stdin, so
/// the lock buys nothing here).
fn serve_stdio() {
    let mut reader = BufReader::new(io::stdin());
    // The output sink is shared: the message loop writes request responses and the
    // BSP session's reader thread writes `textDocument/publishDiagnostics` through
    // the one lock, so a diagnostic reaches the editor immediately even while the
    // loop is parked reading the next request. `io::Stdout` (not a held lock) is
    // Send + Sync so the sink can be shared with the reader thread.
    let sink = Arc::new(OutputSink::new(io::stdout()));
    let mut core = ServerCore::new();
    // The build server's `buildTarget/didChange` (delivered on the session reader
    // thread) sets the loop's reload flag; the loop drains it and reloads the model.
    let reload_flag = core.reload_flag();
    let on_build_targets_changed: Arc<dyn Fn() + Send + Sync> =
        Arc::new(move || reload_flag.store(true, Ordering::SeqCst));
    // The build server's `build/publishDiagnostics` (delivered on the session
    // reader thread) is routed through one `DiagnosticRouter` (per-URI merge across
    // targets, per-target reset, clear-once suppression); each accepted LSP publish
    // then flows through the shared PC-diagnostics merge layer — which records the
    // BSP set, drops the URI's live-typing overlay, and writes the merged
    // `textDocument/publishDiagnostics` to the sink. The SAME layer is handed to
    // the bootstrap, so the ready bundle's debounced live-typing pull publishes
    // into the same merged stream (BSP primary, PC-tagged overlay second).
    let router = Arc::new(Mutex::new(DiagnosticRouter::new()));
    let pc_diagnostics = PcDiagnosticsLayer::new({
        let sink = Arc::clone(&sink);
        Arc::new(move |publish: &PublishDiagnosticsParams| {
            let _ = sink.publish_diagnostics(publish);
        })
    });
    let on_diagnostics: Arc<dyn Fn(BspPublishDiagnosticsParams) + Send + Sync> = {
        let router = Arc::clone(&router);
        let layer = Arc::clone(&pc_diagnostics);
        Arc::new(move |params| {
            if let Some(publish) = router.lock().unwrap().accept(&params) {
                layer.bsp_published(publish);
            }
        })
    };
    let bootstrap = IndexBootstrap::new(LiveBspModelSource::new(
        on_build_targets_changed,
        on_diagnostics,
    ))
    .with_pc_diagnostics(pc_diagnostics);
    if let Err(error) = serve(&mut reader, &sink, &mut core, &CoreHandlers, bootstrap) {
        // The two error endings a stuck user must be able to tell apart: the
        // write side broke (the editor process died while we answered) vs a
        // stdin read error (the clean-EOF and exit endings are logged inside
        // `serve` and return Ok).
        if error.kind() == io::ErrorKind::BrokenPipe {
            log::error!(target: "serve", "output pipe broken — client died: {error}");
        } else {
            log::error!(target: "serve", "serve loop ended on an I/O error: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_is_name_and_version() {
        assert_eq!(version_line(), "scala3-bsp-semantic-ls 0.1.0");
    }

    #[test]
    fn offline_doctor_report_names_the_workspace_state_and_store() {
        let report = offline_doctor_report(Path::new("/ws/x"), false);
        assert!(report.contains("scala3-bsp-semantic-ls"));
        assert!(report.contains("workspace: /ws/x"));
        assert!(report.contains("offline"));
        // The Store section renders offline (here /ws/x has no store).
        assert!(report.contains("Store:"), "{report}");
        assert!(
            report.contains("no store at this workspace root"),
            "{report}"
        );
        // All seven headings render, live-only ones as `unavailable`.
        assert!(report.contains("Runtime:"), "{report}");
        assert!(report.contains("BSP:\n  unavailable:"), "{report}");
    }

    #[test]
    fn offline_doctor_json_is_a_structured_object_with_a_store_key() {
        let json = offline_doctor_report(Path::new("/ws/x"), true);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(value.get("store").is_some());
        assert!(value.get("sqlite").is_none());
        assert_eq!(value["bsp"]["unavailable"], "no BSP connection");
    }
}
