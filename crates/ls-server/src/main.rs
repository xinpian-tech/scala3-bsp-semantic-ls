//! The `scala3-bsp-semantic-ls` stdio LSP server binary — the production entry
//! point. Ports `ls.core.Main.main`: `--version` and `--doctor [dir]` print and
//! exit; otherwise the JSON-RPC server runs over stdin/stdout with the
//! production [`CoreHandlers`] and the live-BSP [`IndexBootstrap`].
//!
//! stdout is reserved for the protocol: diagnostics and log lines go to stderr,
//! so the JSON-RPC stream is never corrupted.

use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use ls_server::{
    dump_report, parse_args, resolve_doctor_dir, serve, store_section, CliAction, CoreHandlers,
    IndexBootstrap, LiveBspModelSource, PublishDiagnosticsParams, ServerCore, ServerHooks,
    SERVER_NAME, SERVER_VERSION,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        CliAction::Version => {
            println!("{}", version_line());
            ExitCode::SUCCESS
        }
        CliAction::Doctor { dir } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            println!("{}", offline_doctor_report(&resolve_doctor_dir(&dir, &cwd)));
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

/// `--doctor [dir]`: the offline report. Pre-bootstrap the build server and
/// presentation compiler are not connected, but the on-disk index store under the
/// workspace root is inspected directly, so the `Store` section renders the same
/// manifest/segment/state facts the live doctor shows. The remaining
/// `DoctorCommand` sections (Runtime host facts + live island status,
/// BSP/SemanticDB/PC/Nix) are gathered as they are ported.
fn offline_doctor_report(root: &Path) -> String {
    format!(
        "{SERVER_NAME} {SERVER_VERSION}\n\
         state: offline (--doctor): build server and presentation compiler not connected\n\
         workspace: {}\n\n\
         {}",
        root.display(),
        store_section(Some(root)),
    )
}

/// Runs the stdio JSON-RPC server: the live-BSP bootstrap on `initialized`, the
/// production handlers for ready requests. stdin/stdout are locked for the
/// process lifetime; stdout carries only protocol frames.
fn serve_stdio() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut core = ServerCore::new();
    // The build server's `buildTarget/didChange` (delivered on the session reader
    // thread) sets the loop's reload flag; the loop drains it and reloads the model.
    let reload_flag = core.reload_flag();
    let on_build_targets_changed: Arc<dyn Fn() + Send + Sync> =
        Arc::new(move || reload_flag.store(true, Ordering::SeqCst));
    let bootstrap = IndexBootstrap::new(LiveBspModelSource::new(on_build_targets_changed));
    // Diagnostics publishing attaches with the diagnostics router; the index
    // bootstrap emits none, so it is a no-op.
    let publish = |_p: PublishDiagnosticsParams| {};
    let hooks = ServerHooks {
        publish_diagnostics: &publish,
    };
    if let Err(error) = serve(
        &mut reader,
        &mut writer,
        &mut core,
        &CoreHandlers,
        &bootstrap,
        &hooks,
    ) {
        eprintln!("{SERVER_NAME}: serve loop ended: {error}");
    }
    let _ = writer.flush();
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
        let report = offline_doctor_report(Path::new("/ws/x"));
        assert!(report.contains("scala3-bsp-semantic-ls"));
        assert!(report.contains("workspace: /ws/x"));
        assert!(report.contains("offline"));
        // The Store section renders offline (here /ws/x has no store).
        assert!(report.contains("Store:"), "{report}");
        assert!(
            report.contains("no store at this workspace root"),
            "{report}"
        );
    }
}
