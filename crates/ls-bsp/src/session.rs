//! One live connection to a BSP server. Every request is bounded by
//! `config.request_timeout` and mapped to a typed [`BspError`]. `launch` starts
//! a server process and connects over its stdio; `connect` runs over arbitrary
//! streams (an in-process server in tests, or a socket).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::client::{dispatch_notification, BspClientHandlers};
use crate::discovery::BspConnectionDetails;
use crate::errors::BspError;
use crate::jsonrpc::{JsonRpcClient, Notification, RpcCallError};
use crate::model::BspProjectModel;
use crate::protocol::*;
use crate::uri::path_to_uri;

/// Client identity and the two timeouts governing a session.
#[derive(Clone, Debug)]
pub struct BspSessionConfig {
    pub client_name: String,
    pub client_version: String,
    pub request_timeout: Duration,
    pub shutdown_timeout: Duration,
    /// The waiting-heartbeat interval for the bootstrap-handshake requests
    /// ([`HANDSHAKE_HEARTBEAT_METHODS`]): while such a request has no response
    /// after this interval, one "still waiting for <method>" line is logged per
    /// elapsed interval — the breadcrumb that distinguishes a busy build server
    /// (compiling its build script, or blocked on another mill/sbt holding the
    /// workspace lock) from a wedged one. Tests shrink it to observe the line
    /// quickly; production keeps the 10s default.
    pub handshake_heartbeat: Duration,
}

impl Default for BspSessionConfig {
    fn default() -> Self {
        BspSessionConfig {
            client_name: "scala3-bsp-semantic-ls".to_string(),
            client_version: "0.1.0".to_string(),
            request_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(5),
            handshake_heartbeat: Duration::from_secs(10),
        }
    }
}

/// The bootstrap-handshake requests that get the waiting heartbeat: the ones a
/// restarted server blocks on while the build server starts up (or another
/// build tool holds the workspace lock). `buildTarget/compile` is deliberately
/// NOT here — a long compile is normal, its progress is visible through the
/// forwarded `build/logMessage` lines, and its own begin/end lines bound it.
const HANDSHAKE_HEARTBEAT_METHODS: [&str; 4] = [
    "build/initialize",
    "workspace/buildTargets",
    "buildTarget/sources",
    "buildTarget/scalacOptions",
];

/// Typed result of `buildTarget/compile`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BspCompileOutcome {
    Ok {
        origin_id: Option<String>,
    },
    Failed {
        status_code: i32,
        origin_id: Option<String>,
    },
}

impl BspCompileOutcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, BspCompileOutcome::Ok { .. })
    }
}

pub struct BspSession {
    pub workspace_root: PathBuf,
    process: Mutex<Option<Child>>,
    rpc: JsonRpcClient,
    config: BspSessionConfig,
    initialize_result: Mutex<Option<InitializeBuildResult>>,
    closed: AtomicBool,
}

impl BspSession {
    /// Launches the server process described by a connection file (argv as
    /// given, cwd = workspace root) and connects over its stdio.
    pub fn launch(
        workspace_root: PathBuf,
        details: &BspConnectionDetails,
        handlers: BspClientHandlers,
        config: BspSessionConfig,
    ) -> Result<BspSession, BspError> {
        let name = if details.name.is_empty() {
            "<unnamed>".to_string()
        } else {
            details.name.clone()
        };
        if details.argv.is_empty() {
            return Err(BspError::LaunchFailed {
                server: name,
                detail: "connection file has empty argv".to_string(),
            });
        }
        let mut command = Command::new(&details.argv[0]);
        command
            .args(&details.argv[1..])
            .current_dir(&workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| BspError::LaunchFailed {
            server: name.clone(),
            detail: e.to_string(),
        })?;
        log::info!(
            target: "bsp",
            "launched build server '{name}' (pid {}), cwd {}",
            child.id(),
            workspace_root.display(),
        );
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let stderr = child.stderr.take().expect("piped stderr");
        pump_stderr(stderr, handlers.clone());
        Ok(Self::make(
            workspace_root,
            Some(child),
            Box::new(stdout),
            Box::new(stdin),
            handlers,
            config,
        ))
    }

    /// Connects over arbitrary streams. `input` carries server -> client
    /// messages, `output` client -> server.
    pub fn connect(
        workspace_root: PathBuf,
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
        handlers: BspClientHandlers,
        config: BspSessionConfig,
    ) -> BspSession {
        Self::make(workspace_root, None, input, output, handlers, config)
    }

    fn make(
        workspace_root: PathBuf,
        process: Option<Child>,
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
        handlers: BspClientHandlers,
        config: BspSessionConfig,
    ) -> BspSession {
        let dispatch_handlers = handlers.clone();
        let on_notification: Box<dyn Fn(Notification) + Send> =
            Box::new(move |note| dispatch_notification(&dispatch_handlers, note));
        let rpc = JsonRpcClient::start(input, output, on_notification);
        BspSession {
            workspace_root,
            process: Mutex::new(process),
            rpc,
            config,
            initialize_result: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Alive-flag of the launched server process; None for stream-connected
    /// sessions.
    pub fn server_process_alive(&self) -> Option<bool> {
        let mut guard = self.process.lock().unwrap();
        guard
            .as_mut()
            .map(|child| matches!(child.try_wait(), Ok(None)))
    }

    /// Capabilities from build/initialize; None before [`initialize`] ran.
    pub fn server_capabilities(&self) -> Option<BuildServerCapabilities> {
        self.initialize_result
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.capabilities.clone())
    }

    /// build/initialize (languageIds = ["scala"]) then the build/initialized
    /// notification.
    pub fn initialize(&self) -> Result<InitializeBuildResult, BspError> {
        let params = InitializeBuildParams {
            display_name: self.config.client_name.clone(),
            version: self.config.client_version.clone(),
            bsp_version: PROTOCOL_VERSION.to_string(),
            root_uri: path_to_uri(&self.workspace_root),
            capabilities: BuildClientCapabilities {
                language_ids: vec!["scala".to_string()],
            },
        };
        let result: InitializeBuildResult = self.request("build/initialize", to_value(params))?;
        log::info!(
            target: "bsp",
            "build/initialize ok: server '{}' {} (bsp {})",
            result.display_name,
            result.version,
            result.bsp_version,
        );
        *self.initialize_result.lock().unwrap() = Some(result.clone());
        self.notify("build/initialized", Value::Null)?;
        Ok(result)
    }

    pub fn workspace_build_targets(&self) -> Result<Vec<BuildTarget>, BspError> {
        let result: WorkspaceBuildTargetsResult =
            self.request("workspace/buildTargets", Value::Null)?;
        Ok(result.targets)
    }

    pub fn build_target_sources(&self, bsp_ids: &[String]) -> Result<Vec<SourcesItem>, BspError> {
        let params = SourcesParams {
            targets: ids_of(bsp_ids),
        };
        let result: SourcesResult = self.request("buildTarget/sources", to_value(params))?;
        Ok(result.items)
    }

    pub fn build_target_scalac_options(
        &self,
        bsp_ids: &[String],
    ) -> Result<Vec<ScalacOptionsItem>, BspError> {
        let params = ScalacOptionsParams {
            targets: ids_of(bsp_ids),
        };
        let result: ScalacOptionsResult =
            self.request("buildTarget/scalacOptions", to_value(params))?;
        Ok(result.items)
    }

    /// buildTarget/compile. Diagnostics and messages arrive through the client
    /// handlers while the request is in flight; the status code is typed.
    pub fn compile(
        &self,
        bsp_ids: &[String],
        origin_id: Option<String>,
    ) -> Result<BspCompileOutcome, BspError> {
        log::info!(
            target: "bsp",
            "buildTarget/compile started ({} target(s): {:?})",
            bsp_ids.len(),
            bsp_ids,
        );
        let started = Instant::now();
        let params = CompileParams {
            targets: ids_of(bsp_ids),
            origin_id,
        };
        let result: Result<CompileResult, BspError> =
            self.request("buildTarget/compile", to_value(params));
        let elapsed = started.elapsed().as_secs_f64();
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                log::warn!(
                    target: "bsp",
                    "buildTarget/compile failed after {elapsed:.1}s: {error}"
                );
                return Err(error);
            }
        };
        match result.status_code {
            Some(STATUS_OK) => {
                log::info!(
                    target: "bsp",
                    "buildTarget/compile finished: statusCode 1 (OK) in {elapsed:.1}s"
                );
                Ok(BspCompileOutcome::Ok {
                    origin_id: result.origin_id,
                })
            }
            Some(other) => {
                log::warn!(
                    target: "bsp",
                    "buildTarget/compile finished: statusCode {other} in {elapsed:.1}s"
                );
                Ok(BspCompileOutcome::Failed {
                    status_code: other,
                    origin_id: result.origin_id,
                })
            }
            None => Err(BspError::InvalidResponse {
                method: "buildTarget/compile".to_string(),
                detail: "missing statusCode".to_string(),
            }),
        }
    }

    /// Raw buildTarget/inverseSources call, regardless of capabilities.
    pub fn server_inverse_sources(&self, uri: &str) -> Result<Vec<String>, BspError> {
        let params = InverseSourcesParams {
            text_document: TextDocumentIdentifier {
                uri: uri.to_string(),
            },
        };
        let result: InverseSourcesResult =
            self.request("buildTarget/inverseSources", to_value(params))?;
        Ok(result.targets.into_iter().map(|t| t.uri).collect())
    }

    /// Uses the server when it advertises inverseSourcesProvider, otherwise
    /// falls back to the local uri -> target map of the project model.
    pub fn inverse_sources(
        &self,
        uri: &str,
        model: &BspProjectModel,
    ) -> Result<Vec<String>, BspError> {
        let advertised = self
            .server_capabilities()
            .and_then(|caps| caps.inverse_sources_provider)
            .unwrap_or(false);
        if advertised {
            self.server_inverse_sources(uri)
        } else {
            Ok(model.uri_to_target.get(uri).cloned().into_iter().collect())
        }
    }

    /// buildTarget/dependencySources when advertised. Best-effort: not
    /// advertised or a failed request yields None and never crashes the caller.
    pub fn dependency_sources(&self, bsp_ids: &[String]) -> Option<Vec<DependencySourcesItem>> {
        self.capability_gated(
            bsp_ids,
            |caps| caps.dependency_sources_provider,
            || {
                let params = DependencySourcesParams {
                    targets: ids_of(bsp_ids),
                };
                let result: DependencySourcesResult =
                    self.request("buildTarget/dependencySources", to_value(params))?;
                Ok(result.items)
            },
        )
    }

    /// buildTarget/outputPaths when advertised; best-effort like
    /// [`dependency_sources`].
    pub fn output_paths(&self, bsp_ids: &[String]) -> Option<Vec<OutputPathsItem>> {
        self.capability_gated(
            bsp_ids,
            |caps| caps.output_paths_provider,
            || {
                let params = OutputPathsParams {
                    targets: ids_of(bsp_ids),
                };
                let result: OutputPathsResult =
                    self.request("buildTarget/outputPaths", to_value(params))?;
                Ok(result.items)
            },
        )
    }

    fn capability_gated<T>(
        &self,
        bsp_ids: &[String],
        provider: impl Fn(&BuildServerCapabilities) -> Option<bool>,
        call: impl FnOnce() -> Result<Vec<T>, BspError>,
    ) -> Option<Vec<T>> {
        let advertised = self
            .server_capabilities()
            .as_ref()
            .and_then(&provider)
            .unwrap_or(false);
        if bsp_ids.is_empty() || !advertised {
            None
        } else {
            call().ok()
        }
    }

    /// Graceful buildShutdown + build/exit, then stream/process teardown. Each
    /// step is best-effort and bounded by `config.shutdown_timeout`.
    pub fn shutdown(&self) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        log::info!(
            target: "bsp",
            "session shutdown: sending build/shutdown (bounded {:?}) then build/exit",
            self.config.shutdown_timeout,
        );
        let _ = self.request_bounded("build/shutdown", Value::Null, self.config.shutdown_timeout);
        let _ = self.notify("build/exit", Value::Null);
        self.close();
    }

    /// Hard teardown: marks the session closed and terminates the server
    /// process (waiting `config.shutdown_timeout`, then SIGKILL).
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        // Keep the (now terminated) child in place so `server_process_alive`
        // still reports it after shutdown.
        let mut guard = self.process.lock().unwrap();
        if let Some(child) = guard.as_mut() {
            self.terminate(child);
        }
    }

    fn terminate(&self, child: &mut Child) {
        if wait_quietly(child, self.config.shutdown_timeout) {
            log::info!(target: "bsp", "build server process exited cleanly");
            return;
        }
        // Rust's std only exposes SIGKILL, so the SIGTERM rung of the ladder
        // collapses into the forcible one.
        log::warn!(
            target: "bsp",
            "build server did not exit within {:?} — killing it (SIGKILL)",
            self.config.shutdown_timeout,
        );
        let _ = child.kill();
        wait_quietly(child, Duration::from_millis(1000));
    }

    fn request<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, BspError> {
        let value = self.request_bounded(method, params, self.config.request_timeout)?;
        serde_json::from_value(value).map_err(|e| BspError::InvalidResponse {
            method: method.to_string(),
            detail: e.to_string(),
        })
    }

    fn request_bounded(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BspError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(BspError::SessionClosed {
                method: method.to_string(),
            });
        }
        // The bootstrap-handshake requests get the waiting heartbeat; the rest
        // keep the single bounded wait.
        let heartbeat = HANDSHAKE_HEARTBEAT_METHODS
            .contains(&method)
            .then_some(self.config.handshake_heartbeat);
        self.rpc
            .request(method, params, timeout, heartbeat)
            .map_err(|e| match e {
                RpcCallError::Timeout => BspError::RequestTimeout {
                    method: method.to_string(),
                    timeout_millis: timeout.as_millis() as u64,
                },
                RpcCallError::Failed(detail) => BspError::RequestFailed {
                    method: method.to_string(),
                    detail,
                },
            })
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), BspError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(BspError::SessionClosed {
                method: method.to_string(),
            });
        }
        self.rpc.notify(method, params).map_err(|e| match e {
            RpcCallError::Failed(detail) => BspError::RequestFailed {
                method: method.to_string(),
                detail,
            },
            RpcCallError::Timeout => BspError::RequestTimeout {
                method: method.to_string(),
                timeout_millis: 0,
            },
        })
    }
}

fn ids_of(bsp_ids: &[String]) -> Vec<BuildTargetIdentifier> {
    bsp_ids
        .iter()
        .map(|uri| BuildTargetIdentifier { uri: uri.clone() })
        .collect()
}

fn to_value<T: serde::Serialize>(params: T) -> Value {
    serde_json::to_value(params).unwrap_or(Value::Null)
}

/// Polls the child for up to `timeout`; true if it exited within the window.
fn wait_quietly(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
}

/// Pumps the launched build server's stderr: every line is re-emitted on the
/// log stream under the `bsp-err` area (mill/sbt print their startup and
/// build-script progress here — exactly what a user staring at a silent
/// bootstrap needs to see) and then forwarded to the optional
/// `on_server_stderr` handler (tests capture it there).
fn pump_stderr(stderr: impl Read + Send + 'static, handlers: BspClientHandlers) {
    thread::Builder::new()
        .name("bsp-server-stderr".to_string())
        .spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        log::info!(target: "bsp-err", "{line}");
                        handlers.emit_stderr(line);
                    }
                    Err(_) => break,
                }
            }
        })
        .ok();
}
