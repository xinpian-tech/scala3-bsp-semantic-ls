//! A scriptable, JVM-free [`PcQueryService`] fake.
//!
//! Answers deterministic, position-echoing results so wire suites can assert
//! the full PC-backed LSP surface (completion, `completionItem/resolve`, hover,
//! signature help, the definition family) through the REAL serve loop — inject
//! it with [`ls_server::IndexBootstrap::with_pc`] and
//! [`FakePcService::factory`]. Never boots a JVM; the buffer mirror and the
//! registered-target gate behave like the island's outer mirror.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Barrier, Mutex};

use serde_json::{json, Value};

use ls_pc_abi::payloads::{
    AutoImport, CodeActionResult, FoldingRange, InlayHint, InlayLabelPart, PcDiagnostic, Pos, Rng,
    SemanticNode, TargetConfig, TextEdit,
};
use ls_server::bootstrap::PcServiceFactory;
use ls_server::pc::{
    PcCompilerPluginStatus, PcDefLocation, PcDefOrigin, PcDefinition, PcDisabledPlugin,
    PcPluginStatusReport, PcServicePluginStatus, PcSpan,
};
use ls_server::{
    PcLocation, PcQueryService, SearchMethodsResolver, SymbolResolver, ToplevelsResolver,
};

/// The symbol every fake result carries; resolve gates key off `data.symbol`.
pub const FAKE_SYMBOL: &str = "fake/Symbol#";

#[derive(Default)]
pub struct FakePcService {
    /// uri -> (owning target, mirrored text).
    buffers: Mutex<HashMap<String, (String, String)>>,
    registered: Mutex<BTreeSet<String>>,
    /// Every trait call, in order — suites assert lifecycle routing over this.
    log: Mutex<Vec<String>>,
    /// One-shot method gates ([`FakePcService::gate_method`]): the next call of
    /// a gated method logs itself, then blocks on the barrier (and the gate is
    /// consumed) — so a wire suite can hold one request in flight on the serve
    /// loop while later frames queue behind it. One-shot on purpose: a request
    /// that should have been cancelled but wrongly reaches the PC answers
    /// normally (a clean assertion failure) instead of deadlocking the suite.
    gates: Mutex<HashMap<String, Arc<Barrier>>>,
    /// A scripted `semantic_tokens` answer ([`FakePcService::script_semantic_tokens`]):
    /// when set, every `semantic_tokens` call answers it instead of the default
    /// canned node set — so a delta wire suite can serve two DIFFERENT streams
    /// across consecutive requests. Persistent until re-scripted.
    scripted_semantic_tokens: Mutex<Option<Vec<SemanticNode>>>,
}

impl FakePcService {
    pub fn new() -> Arc<FakePcService> {
        Arc::new(FakePcService::default())
    }

    /// A [`PcServiceFactory`] that registers the bootstrap's PC target configs
    /// on `this` and hands the same shared fake to the ready services — so the
    /// suite keeps a handle for assertions while the server uses it live.
    pub fn factory(this: Arc<FakePcService>) -> PcServiceFactory {
        Arc::new(
            move |_root,
                  targets: Vec<TargetConfig>,
                  _resolver: Box<SymbolResolver>,
                  _search_resolver: Box<SearchMethodsResolver>,
                  _toplevels_resolver: Box<ToplevelsResolver>| {
                let mut registered = this.registered.lock().unwrap();
                registered.clear();
                registered.extend(targets.iter().map(|t| t.bsp_id.clone()));
                drop(registered);
                Arc::clone(&this) as Arc<dyn PcQueryService>
            },
        )
    }

    pub fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn mirrored_text(&self, uri: &str) -> Option<String> {
        self.buffers
            .lock()
            .unwrap()
            .get(uri)
            .map(|(_, text)| text.clone())
    }

    /// Arm a one-shot gate on `method` (a call-log method name, e.g.
    /// `"completion"`): the next such call blocks on the barrier AFTER logging
    /// itself, so the suite can poll [`FakePcService::calls`] to learn the call
    /// is in flight, do its work, then release via `gate.wait()`.
    pub fn gate_method(&self, method: &str, gate: Arc<Barrier>) {
        self.gates.lock().unwrap().insert(method.to_string(), gate);
    }

    /// Script the `semantic_tokens` answer: every subsequent call returns
    /// `nodes` (for any uri) until re-scripted. The delta wire suite scripts
    /// one set before `/full` and another before `/full/delta`, so the server
    /// has a genuine two-stream diff to answer.
    pub fn script_semantic_tokens(&self, nodes: Vec<SemanticNode>) {
        *self.scripted_semantic_tokens.lock().unwrap() = Some(nodes);
    }

    fn record(&self, call: String) {
        self.log.lock().unwrap().push(call.clone());
        let method = call.split(' ').next().unwrap_or_default().to_string();
        let gate = self.gates.lock().unwrap().remove(&method);
        if let Some(gate) = gate {
            gate.wait();
        }
    }
}

impl PcQueryService for FakePcService {
    fn did_open(&self, target_id: &str, uri: &str, text: &str) {
        self.record(format!("did_open {target_id} {uri}"));
        self.buffers
            .lock()
            .unwrap()
            .insert(uri.to_string(), (target_id.to_string(), text.to_string()));
    }

    fn did_change(&self, uri: &str, text: &str) {
        self.record(format!("did_change {uri}"));
        if let Some(buffer) = self.buffers.lock().unwrap().get_mut(uri) {
            buffer.1 = text.to_string();
        }
    }

    fn did_close(&self, uri: &str) {
        self.record(format!("did_close {uri}"));
        self.buffers.lock().unwrap().remove(uri);
    }

    fn is_open(&self, uri: &str) -> bool {
        self.buffers.lock().unwrap().contains_key(uri)
    }

    fn definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation> {
        self.record(format!("definition {uri}:{line}:{character}"));
        vec![PcLocation {
            uri: uri.to_string(),
            start_line: line,
            start_character: character,
            end_line: line,
            end_character: character + 3,
        }]
    }

    fn type_definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation> {
        self.record(format!("type_definition {uri}:{line}:{character}"));
        vec![PcLocation {
            uri: uri.to_string(),
            start_line: line,
            start_character: character,
            end_line: line,
            end_character: character + 7,
        }]
    }

    fn completion(&self, uri: &str, line: u32, character: u32) -> Value {
        self.record(format!("completion {uri}:{line}:{character}"));
        json!({
            "isIncomplete": false,
            "items": [{
                "label": format!("fakeItem@{line}:{character}"),
                "kind": 3,
                "data": { "symbol": FAKE_SYMBOL },
            }],
        })
    }

    fn hover(&self, uri: &str, line: u32, character: u32) -> Value {
        self.record(format!("hover {uri}:{line}:{character}"));
        json!({
            "contents": {
                "kind": "markdown",
                "value": format!("fake hover at {line}:{character}"),
            }
        })
    }

    fn signature_help(&self, uri: &str, line: u32, character: u32) -> Value {
        self.record(format!("signature_help {uri}:{line}:{character}"));
        json!({
            "signatures": [{ "label": format!("fakeSig(line: {line}, character: {character})") }],
            "activeSignature": 0,
            "activeParameter": 0,
        })
    }

    fn prepare_rename(&self, uri: &str, line: u32, character: u32) -> Option<PcSpan> {
        self.record(format!("prepare_rename {uri}:{line}:{character}"));
        Some(PcSpan {
            start_line: line,
            start_character: character,
            end_line: line,
            end_character: character + 4,
        })
    }

    fn definition_result(&self, uri: &str, line: u32, character: u32) -> PcDefinition {
        self.record(format!("definition_result {uri}:{line}:{character}"));
        PcDefinition {
            symbol: FAKE_SYMBOL.to_string(),
            locations: vec![PcDefLocation {
                uri: uri.to_string(),
                span: PcSpan {
                    start_line: line,
                    start_character: character,
                    end_line: line,
                    end_character: character + 3,
                },
                origin: PcDefOrigin::Workspace,
            }],
        }
    }

    fn is_registered(&self, target_id: &str) -> bool {
        self.registered.lock().unwrap().contains(target_id)
    }

    fn registered_targets(&self) -> Vec<String> {
        self.registered.lock().unwrap().iter().cloned().collect()
    }

    fn resolve_completion_item(&self, target_id: &str, symbol: &str, item: &Value) -> Value {
        self.record(format!("resolve {target_id} {symbol}"));
        let mut enriched = item.clone();
        if let Some(obj) = enriched.as_object_mut() {
            obj.insert(
                "detail".to_string(),
                Value::String(format!("resolved by fake PC ({target_id}, {symbol})")),
            );
        }
        enriched
    }

    fn on_config_changed(&self) {
        // Recorded so a wire suite can assert the watched-config nudge reached
        // the PC seam (the default impl is a no-op).
        self.record("on_config_changed".to_string());
    }

    fn reconfigure_targets(&self, targets: Vec<TargetConfig>) {
        self.record(format!("reconfigure_targets ({})", targets.len()));
        let mut registered = self.registered.lock().unwrap();
        registered.clear();
        registered.extend(targets.into_iter().map(|t| t.bsp_id));
    }

    // --- ABI v2 payload-query ops: deterministic, position/uri-echoing canned
    // --- answers + call logging, so the wire suites are ready for the feature
    // --- task's LSP mapping without a JVM.

    fn inlay_hints(&self, uri: &str, range: Rng, flags: u32) -> Vec<InlayHint> {
        self.record(format!(
            "inlay_hints {uri}:{}:{}-{}:{} flags={flags}",
            range.start_line, range.start_character, range.end_line, range.end_character
        ));
        vec![InlayHint {
            position: Pos {
                line: range.start_line,
                character: range.start_character + 2,
            },
            label_parts: vec![InlayLabelPart {
                text: ": Int".to_string(),
                location: Some((uri.to_string(), range.clone())),
                tooltip: Some("fake inferred type".to_string()),
            }],
            kind: 1,
            padding_left: true,
            padding_right: false,
            text_edits: None,
            // Canonical JSON bytes, exactly like the real island (which writes
            // the lsp4j hint's `data` as gson JSON) — so the wire suite pins
            // the verbatim JSON pass-through on the LSP edge.
            data: Some(format!("{{\"symbol\":\"{FAKE_SYMBOL}\"}}").into_bytes()),
        }]
    }

    /// Offset nodes shaped for the pc_wire `DIRTY` buffer ("package pkga\n\n
    /// class Core…"): two line-0 tokens, one on line 2 (so the wire snapshot
    /// pins a cross-line delta), and one `-1` unclassified node (the dotty
    /// `makeNode` fallthrough) the encoder must drop. A scripted answer
    /// ([`FakePcService::script_semantic_tokens`]) replaces the canned set.
    fn semantic_tokens(&self, uri: &str) -> Vec<SemanticNode> {
        self.record(format!("semantic_tokens {uri}"));
        if let Some(scripted) = self.scripted_semantic_tokens.lock().unwrap().clone() {
            return scripted;
        }
        vec![
            SemanticNode {
                start: 0,
                end: 4,
                token_type: 3,
                token_modifier: 1,
            },
            SemanticNode {
                start: 8,
                end: 12,
                token_type: 15,
                token_modifier: 0,
            },
            SemanticNode {
                start: 20,
                end: 24,
                token_type: 2,
                token_modifier: 2,
            },
            SemanticNode {
                start: 25,
                end: 27,
                token_type: -1,
                token_modifier: 0,
            },
        ]
    }

    fn selection_range(&self, uri: &str, positions: &[Pos]) -> Vec<Vec<Rng>> {
        self.record(format!("selection_range {uri} ({})", positions.len()));
        positions
            .iter()
            .map(|pos| {
                vec![
                    Rng {
                        start_line: pos.line,
                        start_character: pos.character,
                        end_line: pos.line,
                        end_character: pos.character + 2,
                    },
                    Rng {
                        start_line: pos.line,
                        start_character: 0,
                        end_line: pos.line + 1,
                        end_character: 0,
                    },
                ]
            })
            .collect()
    }

    /// Canned per-action-id answers shaped for the code-action ASSEMBLY layer:
    /// `InsertInferredType` answers a `": Int"` insert at the probe position
    /// (assembled into the "Insert type annotation" action), `InlineValue`
    /// answers the refusal-as-data case (the assembly must DROP it), and every
    /// other id answers empty (dropped too) — so a wire suite pins both the
    /// included and the eagerly-dropped halves against one canned session.
    fn code_action(
        &self,
        uri: &str,
        action: i32,
        position: Pos,
        _extraction_end: Option<Pos>,
        _arg_indices: Option<Vec<i32>>,
    ) -> CodeActionResult {
        self.record(format!(
            "code_action {uri}:{}:{} action={action}",
            position.line, position.character
        ));
        match action {
            ls_pc_abi::payloads::code_action_id::INSERT_INFERRED_TYPE => CodeActionResult {
                edits: vec![TextEdit {
                    range: Rng {
                        start_line: position.line,
                        start_character: position.character,
                        end_line: position.line,
                        end_character: position.character,
                    },
                    new_text: ": Int".to_string(),
                }],
                refusal: None,
            },
            ls_pc_abi::payloads::code_action_id::INLINE_VALUE => CodeActionResult {
                edits: Vec::new(),
                refusal: Some("fake refusal: cannot inline here".to_string()),
            },
            _ => CodeActionResult::default(),
        }
    }

    fn auto_imports(
        &self,
        uri: &str,
        position: Pos,
        name: &str,
        is_extension: bool,
    ) -> Vec<AutoImport> {
        self.record(format!(
            "auto_imports {uri}:{}:{} {name} ext={is_extension}",
            position.line, position.character
        ));
        vec![AutoImport {
            package_name: "fake.pkg".to_string(),
            edits: vec![TextEdit {
                range: Rng::default(),
                new_text: format!("import fake.pkg.{name}\n"),
            }],
            symbol: Some(format!("fake/pkg/{name}#")),
        }]
    }

    fn pc_diagnostics(&self, uri: &str) -> Vec<PcDiagnostic> {
        self.record(format!("pc_diagnostics {uri}"));
        vec![PcDiagnostic {
            range: Rng {
                start_line: 0,
                start_character: 0,
                end_line: 0,
                end_character: 4,
            },
            severity: 2,
            code: "FAKE1".to_string(),
            message: format!("fake diagnostic for {uri}"),
        }]
    }

    fn folding_range(&self, uri: &str) -> Vec<FoldingRange> {
        self.record(format!("folding_range {uri}"));
        vec![FoldingRange {
            range: Rng {
                start_line: 0,
                start_character: 0,
                end_line: 3,
                end_character: 1,
            },
            kind: 2,
        }]
    }

    /// A canned plugin-status report, so the `pcPluginStatus` executeCommand
    /// round-trip (and the doctor's `PC Plugins` section) is wire-testable
    /// without a JVM. Deterministic: one loaded compiler plugin, one enabled
    /// service plugin passing its self-test, one disabled plugin.
    fn plugin_status(&self) -> Option<PcPluginStatusReport> {
        self.record("plugin_status".to_string());
        Some(PcPluginStatusReport {
            compiler_plugins: vec![PcCompilerPluginStatus {
                jars: vec!["/plugins/fake-plugin.jar".to_string()],
                options: vec!["-P:fake:on".to_string()],
                loaded: true,
                detail: "ok".to_string(),
            }],
            service_plugins: vec![PcServicePluginStatus {
                id: "fake.nav".to_string(),
                source: "builtin".to_string(),
                enabled: true,
                self_test_ok: true,
                self_test_detail: "ok".to_string(),
            }],
            disabled: vec![PcDisabledPlugin {
                id: "fake.disabled".to_string(),
                reason: "disabled by config".to_string(),
            }],
        })
    }
}
