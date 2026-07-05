//! Owned mirrors of today's PC carriers with lossless flat encode/decode over
//! the [`crate::codec`] buffer format. Request payloads (target config, did_open,
//! did_change, position, resolve) and response payloads (completion list/item,
//! hover, signature help, definition, prepare-rename, plugin status, locations)
//! each round-trip through their `encode`/`decode` pair. Nullable results carry
//! a presence flag so `None` is distinct from an empty value.

use crate::codec::{AbiError, Reader, Writer};

// Payload kinds (the envelope tag; a decode against the wrong kind is rejected).
const KIND_TARGET_CONFIG: u32 = 1;
const KIND_DID_OPEN: u32 = 2;
const KIND_DID_CHANGE: u32 = 3;
const KIND_POSITION: u32 = 4;
const KIND_RESOLVE_PARAMS: u32 = 5;
const KIND_COMPLETION_LIST: u32 = 6;
const KIND_COMPLETION_ITEM: u32 = 7;
const KIND_HOVER: u32 = 8;
const KIND_SIGNATURE_HELP: u32 = 9;
const KIND_DEFINITION: u32 = 10;
const KIND_PREPARE_RENAME: u32 = 11;
const KIND_PLUGIN_STATUS: u32 = 12;
const KIND_LOCATIONS: u32 = 13;

/// `DefinitionOrigin` ordinals (mirrors the Scala `enum DefinitionOrigin`).
pub mod origin {
    pub const WORKSPACE: u32 = 0;
    pub const SYNTHETIC: u32 = 1;
    pub const PLUGIN: u32 = 2;
}

fn write_str_list(w: &mut Writer, list: &[String]) {
    w.u32(list.len() as u32);
    for s in list {
        w.str(s);
    }
}

fn read_str_list(r: &mut Reader) -> Result<Vec<String>, AbiError> {
    let n = r.count()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(r.str()?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Shared value types.
// ---------------------------------------------------------------------------

/// A zero-based `[start, end)` range (UTF-16 positions, as LSP).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rng {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

impl Rng {
    fn write(&self, w: &mut Writer) {
        w.range(
            self.start_line,
            self.start_character,
            self.end_line,
            self.end_character,
        );
    }

    fn read(r: &mut Reader) -> Result<Rng, AbiError> {
        let (start_line, start_character, end_line, end_character) = r.range()?;
        Ok(Rng {
            start_line,
            start_character,
            end_line,
            end_character,
        })
    }

    fn write_opt(w: &mut Writer, range: &Option<Rng>) {
        match range {
            Some(rng) => {
                w.u32(1);
                rng.write(w);
            }
            None => {
                w.u32(0);
                w.range(0, 0, 0, 0);
            }
        }
    }

    fn read_opt(r: &mut Reader) -> Result<Option<Rng>, AbiError> {
        let present = r.u32()?;
        let rng = Rng::read(r)?;
        Ok(if present == 0 { None } else { Some(rng) })
    }
}

/// A text edit (a range plus its replacement text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Rng,
    pub new_text: String,
}

impl TextEdit {
    fn write(&self, w: &mut Writer) {
        self.range.write(w);
        w.str(&self.new_text);
    }

    fn read(r: &mut Reader) -> Result<TextEdit, AbiError> {
        let range = Rng::read(r)?;
        let new_text = r.str()?;
        Ok(TextEdit { range, new_text })
    }
}

/// A definition/reference location plus its `DefinitionOrigin` ordinal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub uri: String,
    pub range: Rng,
    pub origin: u32,
}

impl Location {
    fn write(&self, w: &mut Writer) {
        w.str(&self.uri);
        self.range.write(w);
        w.u32(self.origin);
    }

    fn read(r: &mut Reader) -> Result<Location, AbiError> {
        let uri = r.str()?;
        let range = Rng::read(r)?;
        let origin = r.u32()?;
        Ok(Location { uri, range, origin })
    }
}

// ---------------------------------------------------------------------------
// Requests.
// ---------------------------------------------------------------------------

/// `register_target` payload (mirrors `PcWorkerTargetParams`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetConfig {
    pub bsp_id: String,
    pub scala_version: String,
    pub classpath: Vec<String>,
    pub scalac_options: Vec<String>,
    pub source_dirs: Vec<String>,
}

impl TargetConfig {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.bsp_id);
        w.str(&self.scala_version);
        write_str_list(&mut w, &self.classpath);
        write_str_list(&mut w, &self.scalac_options);
        write_str_list(&mut w, &self.source_dirs);
        w.finish(KIND_TARGET_CONFIG)
    }

    pub fn decode(buf: &[u8]) -> Result<TargetConfig, AbiError> {
        let mut r = Reader::new(buf, KIND_TARGET_CONFIG)?;
        let bsp_id = r.str()?;
        let scala_version = r.str()?;
        let classpath = read_str_list(&mut r)?;
        let scalac_options = read_str_list(&mut r)?;
        let source_dirs = read_str_list(&mut r)?;
        r.finish()?;
        Ok(TargetConfig {
            bsp_id,
            scala_version,
            classpath,
            scalac_options,
            source_dirs,
        })
    }
}

/// `did_open` payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DidOpenParams {
    pub target_id: String,
    pub uri: String,
    pub text: String,
}

impl DidOpenParams {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.target_id);
        w.str(&self.uri);
        w.str(&self.text);
        w.finish(KIND_DID_OPEN)
    }

    pub fn decode(buf: &[u8]) -> Result<DidOpenParams, AbiError> {
        let mut r = Reader::new(buf, KIND_DID_OPEN)?;
        let target_id = r.str()?;
        let uri = r.str()?;
        let text = r.str()?;
        r.finish()?;
        Ok(DidOpenParams {
            target_id,
            uri,
            text,
        })
    }
}

/// `did_change` payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DidChangeParams {
    pub uri: String,
    pub text: String,
}

impl DidChangeParams {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.uri);
        w.str(&self.text);
        w.finish(KIND_DID_CHANGE)
    }

    pub fn decode(buf: &[u8]) -> Result<DidChangeParams, AbiError> {
        let mut r = Reader::new(buf, KIND_DID_CHANGE)?;
        let uri = r.str()?;
        let text = r.str()?;
        r.finish()?;
        Ok(DidChangeParams { uri, text })
    }
}

/// A position query's params (uri + line/character), for the ops that carry
/// them as an encoded payload rather than scalar arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionParams {
    pub uri: String,
    pub line: u32,
    pub character: u32,
}

impl PositionParams {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.uri);
        w.u32(self.line);
        w.u32(self.character);
        w.finish(KIND_POSITION)
    }

    pub fn decode(buf: &[u8]) -> Result<PositionParams, AbiError> {
        let mut r = Reader::new(buf, KIND_POSITION)?;
        let uri = r.str()?;
        let line = r.u32()?;
        let character = r.u32()?;
        r.finish()?;
        Ok(PositionParams {
            uri,
            line,
            character,
        })
    }
}

/// `completion_resolve` params (mirrors `PcWorkerResolveParams`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveParams {
    pub target_id: String,
    pub symbol: String,
    pub item: CompletionItem,
}

impl ResolveParams {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.target_id);
        w.str(&self.symbol);
        self.item.write(&mut w);
        w.finish(KIND_RESOLVE_PARAMS)
    }

    pub fn decode(buf: &[u8]) -> Result<ResolveParams, AbiError> {
        let mut r = Reader::new(buf, KIND_RESOLVE_PARAMS)?;
        let target_id = r.str()?;
        let symbol = r.str()?;
        let item = CompletionItem::read(&mut r)?;
        r.finish()?;
        Ok(ResolveParams {
            target_id,
            symbol,
            item,
        })
    }
}

// ---------------------------------------------------------------------------
// Completion.
// ---------------------------------------------------------------------------

/// One completion item (the fields today's Scala PC populates).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: i32,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub sort_text: Option<String>,
    pub filter_text: Option<String>,
    pub insert_text: Option<String>,
    pub insert_text_format: i32,
    pub text_edit: Option<TextEdit>,
    pub additional_text_edits: Vec<TextEdit>,
    pub commit_characters: Vec<String>,
    pub data: Option<Vec<u8>>,
}

impl CompletionItem {
    fn write(&self, w: &mut Writer) {
        w.str(&self.label);
        w.i32(self.kind);
        w.opt_str(self.detail.as_deref());
        w.opt_str(self.documentation.as_deref());
        w.opt_str(self.sort_text.as_deref());
        w.opt_str(self.filter_text.as_deref());
        w.opt_str(self.insert_text.as_deref());
        w.i32(self.insert_text_format);
        match &self.text_edit {
            Some(edit) => {
                w.u32(1);
                edit.write(w);
            }
            None => w.u32(0),
        }
        w.u32(self.additional_text_edits.len() as u32);
        for edit in &self.additional_text_edits {
            edit.write(w);
        }
        write_str_list(w, &self.commit_characters);
        w.opt_bytes(self.data.as_deref());
    }

    fn read(r: &mut Reader) -> Result<CompletionItem, AbiError> {
        let label = r.str()?;
        let kind = r.i32()?;
        let detail = r.opt_str()?;
        let documentation = r.opt_str()?;
        let sort_text = r.opt_str()?;
        let filter_text = r.opt_str()?;
        let insert_text = r.opt_str()?;
        let insert_text_format = r.i32()?;
        let text_edit = if r.u32()? != 0 {
            Some(TextEdit::read(r)?)
        } else {
            None
        };
        let edit_count = r.count()?;
        let mut additional_text_edits = Vec::with_capacity(edit_count);
        for _ in 0..edit_count {
            additional_text_edits.push(TextEdit::read(r)?);
        }
        let commit_characters = read_str_list(r)?;
        let data = r.opt_bytes()?;
        Ok(CompletionItem {
            label,
            kind,
            detail,
            documentation,
            sort_text,
            filter_text,
            insert_text,
            insert_text_format,
            text_edit,
            additional_text_edits,
            commit_characters,
            data,
        })
    }

    /// `completion_resolve` response (a single enriched item).
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        self.write(&mut w);
        w.finish(KIND_COMPLETION_ITEM)
    }

    pub fn decode(buf: &[u8]) -> Result<CompletionItem, AbiError> {
        let mut r = Reader::new(buf, KIND_COMPLETION_ITEM)?;
        let item = CompletionItem::read(&mut r)?;
        r.finish()?;
        Ok(item)
    }
}

/// A completion response list. An empty `items` is a real empty list (distinct
/// from a null hover / prepare-rename).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionList {
    pub is_incomplete: bool,
    pub items: Vec<CompletionItem>,
}

impl CompletionList {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.bool32(self.is_incomplete);
        w.u32(self.items.len() as u32);
        for item in &self.items {
            item.write(&mut w);
        }
        w.finish(KIND_COMPLETION_LIST)
    }

    pub fn decode(buf: &[u8]) -> Result<CompletionList, AbiError> {
        let mut r = Reader::new(buf, KIND_COMPLETION_LIST)?;
        let is_incomplete = r.bool32()?;
        let count = r.count()?;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(CompletionItem::read(&mut r)?);
        }
        r.finish()?;
        Ok(CompletionList {
            is_incomplete,
            items,
        })
    }
}

// ---------------------------------------------------------------------------
// Hover (nullable).
// ---------------------------------------------------------------------------

/// Hover markup plus an optional range. `kind` mirrors the LSP markup kind
/// (`0` plaintext, `1` markdown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub contents: String,
    pub kind: i32,
    pub range: Option<Rng>,
}

/// A hover response: `None` is a null hover (the PC has nothing at the point),
/// distinct from a present hover with empty contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoverResult(pub Option<Hover>);

impl HoverResult {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match &self.0 {
            Some(hover) => {
                w.u32(1);
                w.str(&hover.contents);
                w.i32(hover.kind);
                Rng::write_opt(&mut w, &hover.range);
            }
            None => w.u32(0),
        }
        w.finish(KIND_HOVER)
    }

    pub fn decode(buf: &[u8]) -> Result<HoverResult, AbiError> {
        let mut r = Reader::new(buf, KIND_HOVER)?;
        let hover = if r.u32()? != 0 {
            let contents = r.str()?;
            let kind = r.i32()?;
            let range = Rng::read_opt(&mut r)?;
            Some(Hover {
                contents,
                kind,
                range,
            })
        } else {
            None
        };
        r.finish()?;
        Ok(HoverResult(hover))
    }
}

// ---------------------------------------------------------------------------
// Signature help.
// ---------------------------------------------------------------------------

/// One parameter of a signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterInfo {
    pub label: String,
    pub documentation: Option<String>,
}

/// One signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<ParameterInfo>,
}

/// A signature-help response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureInfo>,
    pub active_signature: i32,
    pub active_parameter: i32,
}

impl SignatureHelp {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.signatures.len() as u32);
        for sig in &self.signatures {
            w.str(&sig.label);
            w.opt_str(sig.documentation.as_deref());
            w.u32(sig.parameters.len() as u32);
            for param in &sig.parameters {
                w.str(&param.label);
                w.opt_str(param.documentation.as_deref());
            }
        }
        w.i32(self.active_signature);
        w.i32(self.active_parameter);
        w.finish(KIND_SIGNATURE_HELP)
    }

    pub fn decode(buf: &[u8]) -> Result<SignatureHelp, AbiError> {
        let mut r = Reader::new(buf, KIND_SIGNATURE_HELP)?;
        let sig_count = r.count()?;
        let mut signatures = Vec::with_capacity(sig_count);
        for _ in 0..sig_count {
            let label = r.str()?;
            let documentation = r.opt_str()?;
            let param_count = r.count()?;
            let mut parameters = Vec::with_capacity(param_count);
            for _ in 0..param_count {
                let plabel = r.str()?;
                let pdoc = r.opt_str()?;
                parameters.push(ParameterInfo {
                    label: plabel,
                    documentation: pdoc,
                });
            }
            signatures.push(SignatureInfo {
                label,
                documentation,
                parameters,
            });
        }
        let active_signature = r.i32()?;
        let active_parameter = r.i32()?;
        r.finish()?;
        Ok(SignatureHelp {
            signatures,
            active_signature,
            active_parameter,
        })
    }
}

// ---------------------------------------------------------------------------
// Definition / locations.
// ---------------------------------------------------------------------------

/// A definition / type-definition response: the resolved symbol plus its
/// origin-tagged locations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionResult {
    pub symbol: String,
    pub locations: Vec<Location>,
}

impl DefinitionResult {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.str(&self.symbol);
        w.u32(self.locations.len() as u32);
        for loc in &self.locations {
            loc.write(&mut w);
        }
        w.finish(KIND_DEFINITION)
    }

    pub fn decode(buf: &[u8]) -> Result<DefinitionResult, AbiError> {
        let mut r = Reader::new(buf, KIND_DEFINITION)?;
        let symbol = r.str()?;
        let count = r.count()?;
        let mut locations = Vec::with_capacity(count);
        for _ in 0..count {
            locations.push(Location::read(&mut r)?);
        }
        r.finish()?;
        Ok(DefinitionResult { symbol, locations })
    }
}

/// The `symbol_definition` callback response: locations only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationsResult {
    pub locations: Vec<Location>,
}

impl LocationsResult {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.locations.len() as u32);
        for loc in &self.locations {
            loc.write(&mut w);
        }
        w.finish(KIND_LOCATIONS)
    }

    pub fn decode(buf: &[u8]) -> Result<LocationsResult, AbiError> {
        let mut r = Reader::new(buf, KIND_LOCATIONS)?;
        let count = r.count()?;
        let mut locations = Vec::with_capacity(count);
        for _ in 0..count {
            locations.push(Location::read(&mut r)?);
        }
        r.finish()?;
        Ok(LocationsResult { locations })
    }
}

// ---------------------------------------------------------------------------
// Prepare-rename (nullable).
// ---------------------------------------------------------------------------

/// A prepare-rename response: `None` when the symbol is not PC-renameable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrepareRenameResult(pub Option<Rng>);

impl PrepareRenameResult {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        Rng::write_opt(&mut w, &self.0);
        w.finish(KIND_PREPARE_RENAME)
    }

    pub fn decode(buf: &[u8]) -> Result<PrepareRenameResult, AbiError> {
        let mut r = Reader::new(buf, KIND_PREPARE_RENAME)?;
        let range = Rng::read_opt(&mut r)?;
        r.finish()?;
        Ok(PrepareRenameResult(range))
    }
}

// ---------------------------------------------------------------------------
// Plugin status.
// ---------------------------------------------------------------------------

/// One compiler plugin's status (mirrors `PcWorkerCompilerPlugin`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerPlugin {
    pub jars: Vec<String>,
    pub options: Vec<String>,
    pub loaded: bool,
    pub detail: String,
}

/// One service plugin's status (mirrors `PcWorkerServicePlugin`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServicePlugin {
    pub id: String,
    pub source: String,
    pub enabled: bool,
    pub self_test_ok: bool,
    pub self_test_detail: String,
}

/// A disabled plugin (mirrors `PcWorkerDisabledPlugin`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisabledPlugin {
    pub id: String,
    pub reason: String,
}

/// The full plugin-status report (mirrors `PcWorkerPluginStatus`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStatus {
    pub compiler_plugins: Vec<CompilerPlugin>,
    pub service_plugins: Vec<ServicePlugin>,
    pub disabled: Vec<DisabledPlugin>,
}

impl PluginStatus {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.compiler_plugins.len() as u32);
        for plugin in &self.compiler_plugins {
            write_str_list(&mut w, &plugin.jars);
            write_str_list(&mut w, &plugin.options);
            w.bool32(plugin.loaded);
            w.str(&plugin.detail);
        }
        w.u32(self.service_plugins.len() as u32);
        for plugin in &self.service_plugins {
            w.str(&plugin.id);
            w.str(&plugin.source);
            w.bool32(plugin.enabled);
            w.bool32(plugin.self_test_ok);
            w.str(&plugin.self_test_detail);
        }
        w.u32(self.disabled.len() as u32);
        for plugin in &self.disabled {
            w.str(&plugin.id);
            w.str(&plugin.reason);
        }
        w.finish(KIND_PLUGIN_STATUS)
    }

    pub fn decode(buf: &[u8]) -> Result<PluginStatus, AbiError> {
        let mut r = Reader::new(buf, KIND_PLUGIN_STATUS)?;
        let compiler_count = r.count()?;
        let mut compiler_plugins = Vec::with_capacity(compiler_count);
        for _ in 0..compiler_count {
            let jars = read_str_list(&mut r)?;
            let options = read_str_list(&mut r)?;
            let loaded = r.bool32()?;
            let detail = r.str()?;
            compiler_plugins.push(CompilerPlugin {
                jars,
                options,
                loaded,
                detail,
            });
        }
        let service_count = r.count()?;
        let mut service_plugins = Vec::with_capacity(service_count);
        for _ in 0..service_count {
            let id = r.str()?;
            let source = r.str()?;
            let enabled = r.bool32()?;
            let self_test_ok = r.bool32()?;
            let self_test_detail = r.str()?;
            service_plugins.push(ServicePlugin {
                id,
                source,
                enabled,
                self_test_ok,
                self_test_detail,
            });
        }
        let disabled_count = r.count()?;
        let mut disabled = Vec::with_capacity(disabled_count);
        for _ in 0..disabled_count {
            let id = r.str()?;
            let reason = r.str()?;
            disabled.push(DisabledPlugin { id, reason });
        }
        r.finish()?;
        Ok(PluginStatus {
            compiler_plugins,
            service_plugins,
            disabled,
        })
    }
}
