//! Process-launch behavior against deliberately unresponsive commands — port of
//! the Scala `BspLaunchTest`. No real build tool: only argv/cwd handling,
//! request timeouts, and bounded process termination.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ls_bsp::{BspClientHandlers, BspConnectionDetails, BspError, BspSession, BspSessionConfig};

fn details(argv: &[&str]) -> BspConnectionDetails {
    BspConnectionDetails {
        name: "unresponsive".to_string(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        version: "1.0.0".to_string(),
        bsp_version: "2.1.1".to_string(),
        languages: vec!["scala".to_string()],
    }
}

fn fast_config() -> BspSessionConfig {
    BspSessionConfig {
        request_timeout: Duration::from_millis(400),
        shutdown_timeout: Duration::from_millis(300),
        ..BspSessionConfig::default()
    }
}

#[test]
fn requests_against_an_unresponsive_server_time_out() {
    let ws = tempfile::tempdir().unwrap();
    let session = BspSession::launch(
        ws.path().to_path_buf(),
        &details(&["sleep", "30"]),
        BspClientHandlers::new(),
        fast_config(),
    )
    .unwrap();
    assert_eq!(session.server_process_alive(), Some(true));
    match session.initialize() {
        Err(BspError::RequestTimeout {
            method,
            timeout_millis,
        }) => {
            assert_eq!(method, "build/initialize");
            assert_eq!(timeout_millis, 400);
        }
        other => panic!("expected RequestTimeout, got {other:?}"),
    }
    session.shutdown();
    // Graceful shutdown timed out, so the process must have been terminated.
    assert_eq!(session.server_process_alive(), Some(false));
    assert!(session.is_closed());
}

#[test]
fn server_stderr_is_forwarded_to_the_handler() {
    let ws = tempfile::tempdir().unwrap();
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let handlers =
        BspClientHandlers::new().on_server_stderr(move |line| sink.lock().unwrap().push(line));
    let session = BspSession::launch(
        ws.path().to_path_buf(),
        &details(&["sh", "-c", "echo fake-stderr-line >&2; exec sleep 30"]),
        handlers,
        fast_config(),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_millis(3000);
    while lines.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        lines.lock().unwrap().first().map(String::as_str),
        Some("fake-stderr-line")
    );
    session.shutdown();
}

#[test]
fn launch_with_empty_argv_fails() {
    let ws = tempfile::tempdir().unwrap();
    let bad = BspConnectionDetails {
        name: "empty-argv".to_string(),
        argv: Vec::new(),
        version: "1.0.0".to_string(),
        bsp_version: "2.1.1".to_string(),
        languages: vec!["scala".to_string()],
    };
    match BspSession::launch(
        ws.path().to_path_buf(),
        &bad,
        BspClientHandlers::new(),
        fast_config(),
    ) {
        Err(BspError::LaunchFailed { server, .. }) => assert_eq!(server, "empty-argv"),
        Err(other) => panic!("expected LaunchFailed, got {other:?}"),
        Ok(_) => panic!("expected LaunchFailed, got a live session"),
    }
}

#[test]
fn launch_with_nonexistent_binary_fails() {
    let ws = tempfile::tempdir().unwrap();
    match BspSession::launch(
        ws.path().to_path_buf(),
        &details(&["/nonexistent/bsp-server-binary"]),
        BspClientHandlers::new(),
        fast_config(),
    ) {
        Err(BspError::LaunchFailed { server, detail }) => {
            assert_eq!(server, "unresponsive");
            assert!(!detail.is_empty());
        }
        Err(other) => panic!("expected LaunchFailed, got {other:?}"),
        Ok(_) => panic!("expected LaunchFailed, got a live session"),
    }
}
