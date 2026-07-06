//! The presentation-compiler query seam and its production implementation over
//! the embedded in-process JVM island.
//!
//! [`PcQueryService`] is the narrow interface the ready path calls (the Scala
//! `CoreServices.pc` facade surface, restricted here to the document lifecycle
//! and the definition family). [`IslandPcService`] is the production
//! implementation: the document notifications keep a Rust-side mirror of the
//! open buffers in sync as they arrive, and it lazily boots the `ls-jvm` island
//! on the FIRST presentation-compiler QUERY (so an index-only session that opens
//! buffers but never issues a PC query keeps a zero-JVM process). On boot it
//! registers the workspace's PC targets, replays the mirrored open buffers into
//! the fresh island, and thereafter dispatches every notification and query over
//! the flat `#[repr(C)]` boundary. Cross-file go-to-definition falls through the
//! presentation compiler to the installed `symbol_definition` resolver, which
//! answers from the global index.
//!
//! [`pc_options`] strips the SemanticDB-generation flags from a target's scalac
//! options exactly as the Scala `Bootstrap.pcOptions` does, so the presentation
//! compiler runs without re-emitting SemanticDB.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use ls_jvm::backend::VtableBackend;
use ls_jvm::watchdog::{PcRequest, QueryKind, Supervisor};
use ls_jvm::{boot_island, install_symbol_definition_resolver, IslandConfig};
use ls_pc_abi::payloads::{DefinitionResult, LocationsResult, TargetConfig};

/// A resolved definition location, in the LSP coordinate system (zero-based
/// lines, UTF-16 characters, end-exclusive). The seam's own type so the trait
/// and its fakes do not depend on the ABI carrier crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcLocation {
    pub uri: String,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// The presentation-compiler capability the ready services own. The document
/// lifecycle (`did_open`/`did_change`/`did_close`) mirrors the editor's open-
/// buffer state into the presentation compiler as the notifications arrive, and
/// the queries run against that mirrored state — the Scala `CoreServices.pc`
/// facade surface (the document notifications plus the definition family). A
/// query is served only for a buffer the mirror holds ([`PcQueryService::is_open`],
/// the `withPcBuffer` gate); the definition family answers empty when the
/// presentation compiler yields nothing.
pub trait PcQueryService: Send + Sync {
    /// Mirror a newly opened buffer (owned by `target_id`) into the presentation
    /// compiler. Never boots the island (an index-only session that opens buffers
    /// but issues no PC query keeps a zero-JVM process).
    fn did_open(&self, target_id: &str, uri: &str, text: &str);

    /// Update a mirrored buffer's text. Never boots the island.
    fn did_change(&self, uri: &str, text: &str);

    /// Drop a buffer from the mirror. Never boots the island.
    fn did_close(&self, uri: &str);

    /// Whether the mirror currently holds an open buffer for `uri` — the Scala
    /// `pc.bufferText(uri).isDefined`, the `withPcBuffer` precondition.
    fn is_open(&self, uri: &str) -> bool;

    /// Go-to-definition of the symbol at `(line, character)` in the mirrored
    /// buffer `uri`. Empty when the presentation compiler yields nothing.
    fn definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation>;

    /// Go-to-type-definition, otherwise identical to [`PcQueryService::definition`].
    fn type_definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation>;
}

/// The `symbol_definition` resolver the island calls when the presentation
/// compiler has no in-buffer source position for a cross-file symbol. Answers
/// from the global index (`QueryOrchestrator::symbol_definition`).
pub type SymbolResolver = dyn Fn(&str, &str) -> LocationsResult + Send + Sync;

/// Strips the SemanticDB-generation flags from a target's scalac options so the
/// presentation compiler does not re-emit SemanticDB. Removes `-Xsemanticdb`,
/// `-Ysemanticdb`, and both the two-token (`-semanticdb-target <v>`) and colon
/// (`-semanticdb-target:<v>`) forms of `-semanticdb-target`/`-sourceroot`. A
/// behavior-preserving port of `Bootstrap.pcOptions`.
pub fn pc_options(scalac_options: &[String]) -> Vec<String> {
    const TWO_TOKEN: [&str; 2] = ["-semanticdb-target", "-sourceroot"];
    let mut out = Vec::new();
    let mut i = 0;
    while i < scalac_options.len() {
        let opt = &scalac_options[i];
        if opt == "-Xsemanticdb" || opt == "-Ysemanticdb" {
            // Drop the single-token generation flags.
        } else if TWO_TOKEN.contains(&opt.as_str()) && i + 1 < scalac_options.len() {
            // Drop the flag and skip its separate value token.
            i += 1;
        } else if TWO_TOKEN.iter().any(|f| opt.starts_with(&format!("{f}:"))) {
            // Drop the colon form (value fused onto the flag).
        } else {
            out.push(opt.clone());
        }
        i += 1;
    }
    out
}

/// The lazily-booted embedded PC island. Constructed with the workspace's PC
/// target registrations and the index-backed `symbol_definition` resolver, but
/// the JVM is not started until the first PC request.
pub struct IslandPcService {
    state: Mutex<IslandState>,
}

struct IslandState {
    workspace_root: PathBuf,
    /// The PC target registrations, replayed into the island on boot.
    targets: Vec<TargetConfig>,
    /// The `symbol_definition` resolver, installed into the island's global slot
    /// once, at boot; taken then.
    resolver: Option<Box<SymbolResolver>>,
    /// The mirrored open buffers (`uri -> (owning target, text)`), replayed into
    /// the island on boot and kept in sync on every query.
    buffers: BTreeMap<String, Buffered>,
    /// `None` until the first PC request boots the island.
    supervisor: Option<Supervisor<VtableBackend>>,
    /// A recorded boot failure, so a broken environment is reported once and the
    /// service then degrades to empty rather than re-attempting a boot per request.
    boot_error: Option<String>,
}

struct Buffered {
    target_id: String,
    text: String,
}

/// A generous per-request deadline: it only bounds a *wedged* request (a healthy
/// query returns well within it), and the first query after a cold boot pays the
/// presentation compiler's class-load + init under `nix flake check` parallelism,
/// so it is sized like the live sweep rather than the 15s production budget.
const REQUEST_DEADLINE: Duration = Duration::from_secs(120);
/// The premain registration deadline, sized for a cold JVM boot under parallel
/// live checks.
const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(60);

impl IslandPcService {
    /// Build the service from the workspace's PC target registrations and the
    /// `symbol_definition` resolver. Does not boot the JVM.
    pub fn new(
        workspace_root: PathBuf,
        targets: Vec<TargetConfig>,
        resolver: Box<SymbolResolver>,
    ) -> IslandPcService {
        IslandPcService {
            state: Mutex::new(IslandState {
                workspace_root,
                targets,
                resolver: Some(resolver),
                buffers: BTreeMap::new(),
                supervisor: None,
                boot_error: None,
            }),
        }
    }

    /// Whether the embedded JVM island has booted. A document notification must
    /// never boot it — only a query does.
    #[cfg(test)]
    fn is_booted(&self) -> bool {
        self.state
            .lock()
            .expect("pc island state mutex")
            .supervisor
            .is_some()
    }

    /// Ensures the island is booted (booting + replaying the mirrored buffers on
    /// the first query), dispatches the query against the already-mirrored buffer,
    /// and decodes the locations. Any boundary/decode failure degrades to an empty
    /// result, matching the Scala PC methods' empty/null fallback when the
    /// compiler yields nothing.
    fn query(&self, kind: QueryKind, uri: &str, line: u32, character: u32) -> Vec<PcLocation> {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;

        // The buffer is already mirrored by the document notifications; the query
        // is the first thing that boots the island (so an index-only session that
        // only opens buffers keeps a zero-JVM process).
        if state.supervisor.is_none() && !boot(state) {
            return Vec::new();
        }
        let Some(sup) = state.supervisor.as_mut() else {
            return Vec::new();
        };
        let reply = match sup.request(PcRequest::Query {
            kind,
            uri: uri.to_string(),
            line,
            character,
        }) {
            Ok(reply) => reply,
            Err(_) => return Vec::new(),
        };
        match DefinitionResult::decode(&reply) {
            Ok(result) => result.locations.into_iter().map(pc_location_of).collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl PcQueryService for IslandPcService {
    fn did_open(&self, target_id: &str, uri: &str, text: &str) {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        state.buffers.insert(
            uri.to_string(),
            Buffered {
                target_id: target_id.to_string(),
                text: text.to_string(),
            },
        );
        // Forward to a running island; a not-yet-booted island stays cold and
        // picks the buffer up from the mirror on its boot replay.
        if let Some(sup) = state.supervisor.as_mut() {
            let _ = sup.request(PcRequest::DidOpen {
                target_id: target_id.to_string(),
                uri: uri.to_string(),
                text: text.to_string(),
            });
        }
    }

    fn did_change(&self, uri: &str, text: &str) {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        // Update the mirror in place (keeping the owning target); the handler only
        // calls `did_change` for a buffer the mirror already holds.
        let Some(buffered) = state.buffers.get_mut(uri) else {
            return;
        };
        buffered.text = text.to_string();
        if let Some(sup) = state.supervisor.as_mut() {
            let _ = sup.request(PcRequest::DidChange {
                uri: uri.to_string(),
                text: text.to_string(),
            });
        }
    }

    fn did_close(&self, uri: &str) {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        state.buffers.remove(uri);
        // Forwarding a close for an unknown buffer is harmless (the island ignores
        // it), matching the Scala `s.pc.didClose(uri)` unconditional forward; a
        // cold island simply drops it from the mirror.
        if let Some(sup) = state.supervisor.as_mut() {
            let _ = sup.request(PcRequest::DidClose {
                uri: uri.to_string(),
            });
        }
    }

    fn is_open(&self, uri: &str) -> bool {
        self.state
            .lock()
            .expect("pc island state mutex")
            .buffers
            .contains_key(uri)
    }

    fn definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation> {
        self.query(QueryKind::Definition, uri, line, character)
    }

    fn type_definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation> {
        self.query(QueryKind::TypeDefinition, uri, line, character)
    }
}

/// Boots the island: installs the resolver (once), reads the JVM environment,
/// boots, registers the targets, and replays the mirrored buffers. Records a
/// boot failure so a broken environment does not re-attempt per request.
/// Returns whether the supervisor is now available.
fn boot(state: &mut IslandState) -> bool {
    if state.boot_error.is_some() {
        return false;
    }
    let (Some(libjvm), Some(agent_jar)) = (
        std::env::var_os("LS_LIBJVM").map(PathBuf::from),
        std::env::var_os("PC_HOST_AGENT_JAR").map(PathBuf::from),
    ) else {
        state.boot_error =
            Some("LS_LIBJVM and PC_HOST_AGENT_JAR must be set to boot the PC island".to_string());
        return false;
    };
    // The resolver slot is global and set-once; a second install (e.g. a second
    // workspace in the process) is ignored, which is correct — one server, one
    // process. Installed before boot so the premain sees it.
    if let Some(resolver) = state.resolver.take() {
        install_symbol_definition_resolver(resolver);
    }
    let config = IslandConfig {
        libjvm: &libjvm,
        agent_jar: &agent_jar,
        extra_classpath: &[],
        workspace_root: Some(&state.workspace_root),
        extra_jvm_options: &[],
        rendezvous_timeout: RENDEZVOUS_TIMEOUT,
        max_abandoned_generations: 4,
        request_deadline: REQUEST_DEADLINE,
        cancel_grace: Duration::from_millis(500),
    };
    let mut sup = match boot_island(&config) {
        Ok(sup) => sup,
        Err(error) => {
            state.boot_error = Some(error.to_string());
            return false;
        }
    };
    for target in &state.targets {
        let _ = sup.request(PcRequest::RegisterTarget {
            id: target.bsp_id.clone(),
            config: target.clone(),
        });
    }
    for (uri, buffered) in &state.buffers {
        let _ = sup.request(PcRequest::DidOpen {
            target_id: buffered.target_id.clone(),
            uri: uri.clone(),
            text: buffered.text.clone(),
        });
    }
    state.supervisor = Some(sup);
    true
}

/// ABI location carrier -> the seam's [`PcLocation`].
fn pc_location_of(loc: ls_pc_abi::payloads::Location) -> PcLocation {
    PcLocation {
        uri: loc.uri,
        start_line: loc.range.start_line,
        start_character: loc.range.start_character,
        end_line: loc.range.end_line,
        end_character: loc.range.end_character,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ports Bootstrap.pcOptions: strips the single-token generation flags, both
    // forms of the two-token flags, and keeps everything else in order.
    #[test]
    fn pc_options_strips_semanticdb_flags_in_every_form() {
        let options = vec![
            "-deprecation".to_string(),
            "-Xsemanticdb".to_string(),
            "-Ysemanticdb".to_string(),
            "-semanticdb-target".to_string(),
            "/out/meta".to_string(),
            "-sourceroot".to_string(),
            "/ws".to_string(),
            "-semanticdb-target:/out/meta2".to_string(),
            "-sourceroot:/ws2".to_string(),
            "-feature".to_string(),
        ];
        assert_eq!(
            pc_options(&options),
            vec!["-deprecation".to_string(), "-feature".to_string()]
        );
    }

    #[test]
    fn pc_options_keeps_a_two_token_flag_with_no_value_token() {
        // A trailing two-token flag with no following value is not treated as a
        // value-skip (mirrors the `i + 1 < length` guard); it is kept as-is.
        let options = vec!["-deprecation".to_string(), "-sourceroot".to_string()];
        assert_eq!(pc_options(&options), options);
    }

    #[test]
    fn pc_options_is_identity_without_semanticdb_flags() {
        let options = vec!["-deprecation".to_string(), "-explain".to_string()];
        assert_eq!(pc_options(&options), options);
    }

    fn empty_resolver() -> Box<SymbolResolver> {
        Box::new(|_symbol, _from_uri| LocationsResult {
            locations: Vec::new(),
        })
    }

    // The document notifications keep the open-buffer mirror in sync WITHOUT
    // booting the JVM island: an index-only session that only opens/edits/closes
    // buffers (never issuing a PC query) keeps a zero-JVM process. A change/close
    // for a buffer the mirror never held is a no-op, not a panic (replay-safe).
    #[test]
    fn document_notifications_mirror_without_booting_the_island() {
        let pc = IslandPcService::new(PathBuf::from("/ws"), Vec::new(), empty_resolver());
        let a = "file:///ws/a.scala";

        assert!(!pc.is_open(a));
        pc.did_open("t", a, "v1");
        assert!(pc.is_open(a));
        pc.did_change(a, "v2");
        assert!(pc.is_open(a));
        pc.did_close(a);
        assert!(!pc.is_open(a));

        // A change/close for a never-opened buffer is a harmless no-op.
        pc.did_change("file:///ws/ghost.scala", "x");
        pc.did_close("file:///ws/ghost.scala");
        assert!(!pc.is_open("file:///ws/ghost.scala"));

        assert!(
            !pc.is_booted(),
            "a document notification must never boot the embedded JVM island"
        );
    }
}
