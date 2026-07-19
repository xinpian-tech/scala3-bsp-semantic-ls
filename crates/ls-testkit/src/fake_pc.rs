//! A scriptable, JVM-free [`PcQueryService`] fake.
//!
//! Answers deterministic, position-echoing results so wire suites can assert
//! the full PC-backed LSP surface (completion, `completionItem/resolve`, hover,
//! signature help, the definition family) through the REAL serve loop — inject
//! it with [`ls_server::IndexBootstrap::with_pc`] and
//! [`FakePcService::factory`]. Never boots a JVM; the buffer mirror and the
//! registered-target gate behave like the island's outer mirror.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use ls_pc_abi::payloads::TargetConfig;
use ls_server::bootstrap::PcServiceFactory;
use ls_server::pc::{PcDefLocation, PcDefOrigin, PcDefinition, PcSpan};
use ls_server::{PcLocation, PcQueryService, SymbolResolver};

/// The symbol every fake result carries; resolve gates key off `data.symbol`.
pub const FAKE_SYMBOL: &str = "fake/Symbol#";

#[derive(Default)]
pub struct FakePcService {
    /// uri -> (owning target, mirrored text).
    buffers: Mutex<HashMap<String, (String, String)>>,
    registered: Mutex<BTreeSet<String>>,
    /// Every trait call, in order — suites assert lifecycle routing over this.
    log: Mutex<Vec<String>>,
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
            move |_root, targets: Vec<TargetConfig>, _resolver: Box<SymbolResolver>| {
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

    fn record(&self, call: String) {
        self.log.lock().unwrap().push(call);
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

    fn reconfigure_targets(&self, targets: Vec<TargetConfig>) {
        self.record(format!("reconfigure_targets ({})", targets.len()));
        let mut registered = self.registered.lock().unwrap();
        registered.clear();
        registered.extend(targets.into_iter().map(|t| t.bsp_id));
    }
}
