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

use ls_server::{
    parse_args, resolve_doctor_dir, serve, CliAction, CoreHandlers, IndexBootstrap,
    LiveBspModelSource, PublishDiagnosticsParams, ServerCore, ServerHooks, SERVER_NAME,
    SERVER_VERSION,
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

/// `--doctor [dir]`: a minimal offline report. Pre-bootstrap the build server,
/// index store, and presentation compiler are not connected; the full Store/
/// Runtime section contract renders once the doctor module lands.
fn offline_doctor_report(root: &Path) -> String {
    format!(
        "{SERVER_NAME} {SERVER_VERSION}\n\
         workspace: {}\n\
         state: offline (--doctor): build server, index store, and presentation compiler not connected\n",
        root.display()
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
    let bootstrap = IndexBootstrap::new(LiveBspModelSource::new());
    // Diagnostics publishing and build-target-change draining attach with the
    // async lifecycle; the index bootstrap emits neither, so both are no-ops.
    let publish = |_p: PublishDiagnosticsParams| {};
    let on_changed = || {};
    let hooks = ServerHooks {
        publish_diagnostics: &publish,
        on_build_targets_changed: &on_changed,
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
    fn offline_doctor_report_names_the_workspace_and_state() {
        let report = offline_doctor_report(Path::new("/ws/x"));
        assert!(report.contains("scala3-bsp-semantic-ls"));
        assert!(report.contains("workspace: /ws/x"));
        assert!(report.contains("offline"));
    }
}
