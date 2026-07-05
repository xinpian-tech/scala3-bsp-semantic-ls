//! Owned mirrors of today's PC carriers with lossless flat encode/decode over
//! the [`crate::codec`] buffer format. The response carriers are the LSP4J
//! 0.24.0 result objects the worker returns (`CompletionList`/`CompletionItem`,
//! `Hover`, `SignatureHelp`, `Range`, `Location`), so the mirrors model the full
//! LSP4J field surface a plugin, presentation compiler, or resolve response can
//! populate — every optional scalar, every nullable list, and every `Either`
//! variant — not just the baseline subset. Genuinely opaque JSON fields
//! (`CompletionItem.data`, `Command.arguments`, `itemDefaults.data`) are carried
//! as opaque bytes the island reconstructs deterministically; the ABI never
//! interprets them as JSON. Nullable results carry a presence flag so `None` is
//! distinct from an empty value.

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

fn bad_tag(what: &str, tag: u32) -> AbiError {
    AbiError(format!("invalid {what} variant tag {tag}"))
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

fn write_opt_str_list(w: &mut Writer, list: &Option<Vec<String>>) {
    match list {
        Some(items) => {
            w.u32(1);
            write_str_list(w, items);
        }
        None => w.u32(0),
    }
}

fn read_opt_str_list(r: &mut Reader) -> Result<Option<Vec<String>>, AbiError> {
    if r.u32()? == 0 {
        Ok(None)
    } else {
        Ok(Some(read_str_list(r)?))
    }
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

/// An insert/replace completion edit (LSP4J `InsertReplaceEdit`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertReplaceEdit {
    pub new_text: String,
    pub insert: Rng,
    pub replace: Rng,
}

/// A completion item's edit: either a plain `TextEdit` or an `InsertReplaceEdit`
/// (LSP4J `Either<TextEdit, InsertReplaceEdit>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionEdit {
    Plain(TextEdit),
    InsertReplace(InsertReplaceEdit),
}

impl CompletionEdit {
    fn write(&self, w: &mut Writer) {
        match self {
            CompletionEdit::Plain(edit) => {
                w.u32(0);
                edit.write(w);
            }
            CompletionEdit::InsertReplace(edit) => {
                w.u32(1);
                w.str(&edit.new_text);
                edit.insert.write(w);
                edit.replace.write(w);
            }
        }
    }

    fn read(r: &mut Reader) -> Result<CompletionEdit, AbiError> {
        match r.u32()? {
            0 => Ok(CompletionEdit::Plain(TextEdit::read(r)?)),
            1 => {
                let new_text = r.str()?;
                let insert = Rng::read(r)?;
                let replace = Rng::read(r)?;
                Ok(CompletionEdit::InsertReplace(InsertReplaceEdit {
                    new_text,
                    insert,
                    replace,
                }))
            }
            tag => Err(bad_tag("completion edit", tag)),
        }
    }
}

/// Markup content (LSP4J `MarkupContent`): `kind` is the markup kind string
/// (`"plaintext"`/`"markdown"`) and `value` the body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkupContent {
    pub kind: String,
    pub value: String,
}

impl MarkupContent {
    fn write(&self, w: &mut Writer) {
        w.str(&self.kind);
        w.str(&self.value);
    }

    fn read(r: &mut Reader) -> Result<MarkupContent, AbiError> {
        let kind = r.str()?;
        let value = r.str()?;
        Ok(MarkupContent { kind, value })
    }
}

/// A documentation body (LSP4J `Either<String, MarkupContent>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Documentation {
    Plain(String),
    Markup(MarkupContent),
}

impl Documentation {
    fn write_opt(w: &mut Writer, doc: &Option<Documentation>) {
        match doc {
            None => w.u32(0),
            Some(Documentation::Plain(s)) => {
                w.u32(1);
                w.str(s);
            }
            Some(Documentation::Markup(m)) => {
                w.u32(2);
                m.write(w);
            }
        }
    }

    fn read_opt(r: &mut Reader) -> Result<Option<Documentation>, AbiError> {
        match r.u32()? {
            0 => Ok(None),
            1 => Ok(Some(Documentation::Plain(r.str()?))),
            2 => Ok(Some(Documentation::Markup(MarkupContent::read(r)?))),
            tag => Err(bad_tag("documentation", tag)),
        }
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

/// Additional label details (LSP4J `CompletionItemLabelDetails`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelDetails {
    pub detail: Option<String>,
    pub description: Option<String>,
}

/// A completion item's `command` (LSP4J `Command`). `arguments` is the opaque
/// serialized JSON argument array, carried verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub title: String,
    pub command: String,
    pub arguments: Option<Vec<u8>>,
}

/// One completion item — the full LSP4J 0.24.0 `CompletionItem` surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub label_details: Option<LabelDetails>,
    pub kind: Option<i32>,
    pub tags: Option<Vec<i32>>,
    pub detail: Option<String>,
    pub documentation: Option<Documentation>,
    pub deprecated: Option<bool>,
    pub preselect: Option<bool>,
    pub sort_text: Option<String>,
    pub filter_text: Option<String>,
    pub insert_text: Option<String>,
    pub insert_text_format: Option<i32>,
    pub insert_text_mode: Option<i32>,
    pub text_edit: Option<CompletionEdit>,
    pub additional_text_edits: Option<Vec<TextEdit>>,
    pub commit_characters: Option<Vec<String>>,
    pub command: Option<Command>,
    pub data: Option<Vec<u8>>,
}

impl CompletionItem {
    fn write(&self, w: &mut Writer) {
        w.str(&self.label);
        match &self.label_details {
            Some(details) => {
                w.u32(1);
                w.opt_str(details.detail.as_deref());
                w.opt_str(details.description.as_deref());
            }
            None => w.u32(0),
        }
        w.opt_i32(self.kind);
        match &self.tags {
            Some(tags) => {
                w.u32(1);
                w.u32(tags.len() as u32);
                for tag in tags {
                    w.i32(*tag);
                }
            }
            None => w.u32(0),
        }
        w.opt_str(self.detail.as_deref());
        Documentation::write_opt(w, &self.documentation);
        w.opt_bool(self.deprecated);
        w.opt_bool(self.preselect);
        w.opt_str(self.sort_text.as_deref());
        w.opt_str(self.filter_text.as_deref());
        w.opt_str(self.insert_text.as_deref());
        w.opt_i32(self.insert_text_format);
        w.opt_i32(self.insert_text_mode);
        match &self.text_edit {
            Some(edit) => {
                w.u32(1);
                edit.write(w);
            }
            None => w.u32(0),
        }
        match &self.additional_text_edits {
            Some(edits) => {
                w.u32(1);
                w.u32(edits.len() as u32);
                for edit in edits {
                    edit.write(w);
                }
            }
            None => w.u32(0),
        }
        write_opt_str_list(w, &self.commit_characters);
        match &self.command {
            Some(command) => {
                w.u32(1);
                w.str(&command.title);
                w.str(&command.command);
                w.opt_bytes(command.arguments.as_deref());
            }
            None => w.u32(0),
        }
        w.opt_bytes(self.data.as_deref());
    }

    fn read(r: &mut Reader) -> Result<CompletionItem, AbiError> {
        let label = r.str()?;
        let label_details = if r.u32()? != 0 {
            let detail = r.opt_str()?;
            let description = r.opt_str()?;
            Some(LabelDetails {
                detail,
                description,
            })
        } else {
            None
        };
        let kind = r.opt_i32()?;
        let tags = if r.u32()? != 0 {
            let n = r.count()?;
            let mut tags = Vec::with_capacity(n);
            for _ in 0..n {
                tags.push(r.i32()?);
            }
            Some(tags)
        } else {
            None
        };
        let detail = r.opt_str()?;
        let documentation = Documentation::read_opt(r)?;
        let deprecated = r.opt_bool()?;
        let preselect = r.opt_bool()?;
        let sort_text = r.opt_str()?;
        let filter_text = r.opt_str()?;
        let insert_text = r.opt_str()?;
        let insert_text_format = r.opt_i32()?;
        let insert_text_mode = r.opt_i32()?;
        let text_edit = if r.u32()? != 0 {
            Some(CompletionEdit::read(r)?)
        } else {
            None
        };
        let additional_text_edits = if r.u32()? != 0 {
            let n = r.count()?;
            let mut edits = Vec::with_capacity(n);
            for _ in 0..n {
                edits.push(TextEdit::read(r)?);
            }
            Some(edits)
        } else {
            None
        };
        let commit_characters = read_opt_str_list(r)?;
        let command = if r.u32()? != 0 {
            let title = r.str()?;
            let command = r.str()?;
            let arguments = r.opt_bytes()?;
            Some(Command {
                title,
                command,
                arguments,
            })
        } else {
            None
        };
        let data = r.opt_bytes()?;
        Ok(CompletionItem {
            label,
            label_details,
            kind,
            tags,
            detail,
            documentation,
            deprecated,
            preselect,
            sort_text,
            filter_text,
            insert_text,
            insert_text_format,
            insert_text_mode,
            text_edit,
            additional_text_edits,
            commit_characters,
            command,
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

/// A range or an insert/replace range pair (LSP4J completion-list
/// `itemDefaults.editRange` = `Either<Range, InsertReplaceRange>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditRange {
    Range(Rng),
    InsertReplace { insert: Rng, replace: Rng },
}

impl EditRange {
    fn write_opt(w: &mut Writer, edit_range: &Option<EditRange>) {
        match edit_range {
            None => w.u32(0),
            Some(EditRange::Range(range)) => {
                w.u32(1);
                range.write(w);
            }
            Some(EditRange::InsertReplace { insert, replace }) => {
                w.u32(2);
                insert.write(w);
                replace.write(w);
            }
        }
    }

    fn read_opt(r: &mut Reader) -> Result<Option<EditRange>, AbiError> {
        match r.u32()? {
            0 => Ok(None),
            1 => Ok(Some(EditRange::Range(Rng::read(r)?))),
            2 => {
                let insert = Rng::read(r)?;
                let replace = Rng::read(r)?;
                Ok(Some(EditRange::InsertReplace { insert, replace }))
            }
            tag => Err(bad_tag("edit range", tag)),
        }
    }
}

/// Completion-list defaults (LSP4J `CompletionItemDefaults`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItemDefaults {
    pub commit_characters: Option<Vec<String>>,
    pub edit_range: Option<EditRange>,
    pub insert_text_format: Option<i32>,
    pub insert_text_mode: Option<i32>,
    pub data: Option<Vec<u8>>,
}

impl CompletionItemDefaults {
    fn write(&self, w: &mut Writer) {
        write_opt_str_list(w, &self.commit_characters);
        EditRange::write_opt(w, &self.edit_range);
        w.opt_i32(self.insert_text_format);
        w.opt_i32(self.insert_text_mode);
        w.opt_bytes(self.data.as_deref());
    }

    fn read(r: &mut Reader) -> Result<CompletionItemDefaults, AbiError> {
        let commit_characters = read_opt_str_list(r)?;
        let edit_range = EditRange::read_opt(r)?;
        let insert_text_format = r.opt_i32()?;
        let insert_text_mode = r.opt_i32()?;
        let data = r.opt_bytes()?;
        Ok(CompletionItemDefaults {
            commit_characters,
            edit_range,
            insert_text_format,
            insert_text_mode,
            data,
        })
    }
}

/// A completion response list (LSP4J `CompletionList`). An empty `items` is a
/// real empty list (distinct from a null hover / prepare-rename).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionList {
    pub is_incomplete: bool,
    pub item_defaults: Option<CompletionItemDefaults>,
    pub items: Vec<CompletionItem>,
}

impl CompletionList {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.bool32(self.is_incomplete);
        match &self.item_defaults {
            Some(defaults) => {
                w.u32(1);
                defaults.write(&mut w);
            }
            None => w.u32(0),
        }
        w.u32(self.items.len() as u32);
        for item in &self.items {
            item.write(&mut w);
        }
        w.finish(KIND_COMPLETION_LIST)
    }

    pub fn decode(buf: &[u8]) -> Result<CompletionList, AbiError> {
        let mut r = Reader::new(buf, KIND_COMPLETION_LIST)?;
        let is_incomplete = r.bool32()?;
        let item_defaults = if r.u32()? != 0 {
            Some(CompletionItemDefaults::read(&mut r)?)
        } else {
            None
        };
        let count = r.count()?;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(CompletionItem::read(&mut r)?);
        }
        r.finish()?;
        Ok(CompletionList {
            is_incomplete,
            item_defaults,
            items,
        })
    }
}

// ---------------------------------------------------------------------------
// Hover (nullable).
// ---------------------------------------------------------------------------

/// One entry of a marked-string hover (LSP4J `Either<String, MarkedString>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkedStringItem {
    Plain(String),
    Marked { language: String, value: String },
}

/// Hover contents (LSP4J `Either<List<Either<String, MarkedString>>, MarkupContent>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HoverContents {
    Markup(MarkupContent),
    Marked(Vec<MarkedStringItem>),
}

impl HoverContents {
    fn write(&self, w: &mut Writer) {
        match self {
            HoverContents::Markup(markup) => {
                w.u32(0);
                markup.write(w);
            }
            HoverContents::Marked(items) => {
                w.u32(1);
                w.u32(items.len() as u32);
                for item in items {
                    match item {
                        MarkedStringItem::Plain(s) => {
                            w.u32(0);
                            w.str(s);
                        }
                        MarkedStringItem::Marked { language, value } => {
                            w.u32(1);
                            w.str(language);
                            w.str(value);
                        }
                    }
                }
            }
        }
    }

    fn read(r: &mut Reader) -> Result<HoverContents, AbiError> {
        match r.u32()? {
            0 => Ok(HoverContents::Markup(MarkupContent::read(r)?)),
            1 => {
                let n = r.count()?;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    let item = match r.u32()? {
                        0 => MarkedStringItem::Plain(r.str()?),
                        1 => {
                            let language = r.str()?;
                            let value = r.str()?;
                            MarkedStringItem::Marked { language, value }
                        }
                        tag => return Err(bad_tag("marked string", tag)),
                    };
                    items.push(item);
                }
                Ok(HoverContents::Marked(items))
            }
            tag => Err(bad_tag("hover contents", tag)),
        }
    }
}

/// Hover contents plus an optional range (LSP4J `Hover`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub contents: HoverContents,
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
                hover.contents.write(&mut w);
                Rng::write_opt(&mut w, &hover.range);
            }
            None => w.u32(0),
        }
        w.finish(KIND_HOVER)
    }

    pub fn decode(buf: &[u8]) -> Result<HoverResult, AbiError> {
        let mut r = Reader::new(buf, KIND_HOVER)?;
        let hover = if r.u32()? != 0 {
            let contents = HoverContents::read(&mut r)?;
            let range = Rng::read_opt(&mut r)?;
            Some(Hover { contents, range })
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

/// A parameter label: either the text or a `[start, end)` offset pair into the
/// signature label (LSP4J `Either<String, Tuple.Two<Integer, Integer>>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParameterLabel {
    Str(String),
    Offsets { start: u32, end: u32 },
}

impl ParameterLabel {
    fn write(&self, w: &mut Writer) {
        match self {
            ParameterLabel::Str(s) => {
                w.u32(0);
                w.str(s);
            }
            ParameterLabel::Offsets { start, end } => {
                w.u32(1);
                w.u32(*start);
                w.u32(*end);
            }
        }
    }

    fn read(r: &mut Reader) -> Result<ParameterLabel, AbiError> {
        match r.u32()? {
            0 => Ok(ParameterLabel::Str(r.str()?)),
            1 => {
                let start = r.u32()?;
                let end = r.u32()?;
                Ok(ParameterLabel::Offsets { start, end })
            }
            tag => Err(bad_tag("parameter label", tag)),
        }
    }
}

/// One parameter of a signature (LSP4J `ParameterInformation`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterInfo {
    pub label: ParameterLabel,
    pub documentation: Option<Documentation>,
}

/// One signature (LSP4J `SignatureInformation`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<Documentation>,
    pub parameters: Option<Vec<ParameterInfo>>,
    pub active_parameter: Option<i32>,
}

/// A signature-help response (LSP4J `SignatureHelp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureInfo>,
    pub active_signature: Option<i32>,
    pub active_parameter: Option<i32>,
}

impl SignatureHelp {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.signatures.len() as u32);
        for sig in &self.signatures {
            w.str(&sig.label);
            Documentation::write_opt(&mut w, &sig.documentation);
            match &sig.parameters {
                Some(params) => {
                    w.u32(1);
                    w.u32(params.len() as u32);
                    for param in params {
                        param.label.write(&mut w);
                        Documentation::write_opt(&mut w, &param.documentation);
                    }
                }
                None => w.u32(0),
            }
            w.opt_i32(sig.active_parameter);
        }
        w.opt_i32(self.active_signature);
        w.opt_i32(self.active_parameter);
        w.finish(KIND_SIGNATURE_HELP)
    }

    pub fn decode(buf: &[u8]) -> Result<SignatureHelp, AbiError> {
        let mut r = Reader::new(buf, KIND_SIGNATURE_HELP)?;
        let sig_count = r.count()?;
        let mut signatures = Vec::with_capacity(sig_count);
        for _ in 0..sig_count {
            let label = r.str()?;
            let documentation = Documentation::read_opt(&mut r)?;
            let parameters = if r.u32()? != 0 {
                let param_count = r.count()?;
                let mut params = Vec::with_capacity(param_count);
                for _ in 0..param_count {
                    let label = ParameterLabel::read(&mut r)?;
                    let documentation = Documentation::read_opt(&mut r)?;
                    params.push(ParameterInfo {
                        label,
                        documentation,
                    });
                }
                Some(params)
            } else {
                None
            };
            let active_parameter = r.opt_i32()?;
            signatures.push(SignatureInfo {
                label,
                documentation,
                parameters,
                active_parameter,
            });
        }
        let active_signature = r.opt_i32()?;
        let active_parameter = r.opt_i32()?;
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
