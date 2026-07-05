//! Tiny protobuf wire-format ENCODER, test-only — a port of the Scala
//! `ProtoTestWriter` / `ProtoTestEncoder`. Round-trips SemanticDB messages
//! through the production decoder, including unknown fields of every wire type.

#![allow(dead_code)]

use ls_semanticdb::{SdbDocument, SdbOccurrence, SdbRange, SdbSymbolInfo};

#[derive(Default)]
pub struct ProtoTestWriter {
    out: Vec<u8>,
}

impl ProtoTestWriter {
    pub fn new() -> Self {
        ProtoTestWriter::default()
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.out.clone()
    }

    pub fn write_raw_varint(&mut self, value: u64) -> &mut Self {
        let mut v = value;
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.out.push(b);
                break;
            }
            self.out.push(b | 0x80);
        }
        self
    }

    pub fn write_tag(&mut self, field: u32, wire_type: u32) -> &mut Self {
        self.write_raw_varint(((field as u64) << 3) | wire_type as u64)
    }

    pub fn varint_field(&mut self, field: u32, value: u64) -> &mut Self {
        self.write_tag(field, 0);
        self.write_raw_varint(value)
    }

    /// Negative int32 values sign-extend to 10-byte varints, per proto spec.
    pub fn int32_field(&mut self, field: u32, value: i32) -> &mut Self {
        self.varint_field(field, value as i64 as u64)
    }

    pub fn fixed64_field(&mut self, field: u32, value: u64) -> &mut Self {
        self.write_tag(field, 1);
        for i in 0..8 {
            self.out.push(((value >> (8 * i)) & 0xff) as u8);
        }
        self
    }

    pub fn fixed32_field(&mut self, field: u32, value: u32) -> &mut Self {
        self.write_tag(field, 5);
        for i in 0..4 {
            self.out.push(((value >> (8 * i)) & 0xff) as u8);
        }
        self
    }

    pub fn bytes_field(&mut self, field: u32, data: &[u8]) -> &mut Self {
        self.write_tag(field, 2);
        self.write_raw_varint(data.len() as u64);
        self.out.extend_from_slice(data);
        self
    }

    pub fn string_field(&mut self, field: u32, value: &str) -> &mut Self {
        self.bytes_field(field, value.as_bytes())
    }

    pub fn message_field(
        &mut self,
        field: u32,
        build: impl FnOnce(&mut ProtoTestWriter),
    ) -> &mut Self {
        let mut nested = ProtoTestWriter::new();
        build(&mut nested);
        let b = nested.bytes();
        self.bytes_field(field, &b)
    }

    /// Legacy group encoding (wire types 3/4); the decoder must skip it as an
    /// unknown field.
    pub fn group_field(
        &mut self,
        field: u32,
        build: impl FnOnce(&mut ProtoTestWriter),
    ) -> &mut Self {
        self.write_tag(field, 3);
        build(self);
        self.write_tag(field, 4)
    }
}

fn write_range(w: &mut ProtoTestWriter, r: &SdbRange) {
    w.int32_field(1, r.start_line);
    w.int32_field(2, r.start_character);
    w.int32_field(3, r.end_line);
    w.int32_field(4, r.end_character);
}

fn write_occurrence(w: &mut ProtoTestWriter, o: &SdbOccurrence, noise: bool) {
    if let Some(r) = &o.range {
        w.message_field(1, |mw| write_range(mw, r));
    }
    if noise {
        w.fixed32_field(90, 0xdead_beef);
    }
    w.string_field(2, &o.symbol);
    if noise {
        w.group_field(91, |gw| {
            gw.varint_field(1, 7);
        });
    }
    w.varint_field(3, o.role_code as i64 as u64);
}

fn write_symbol_info(w: &mut ProtoTestWriter, s: &SdbSymbolInfo, noise: bool) {
    w.string_field(1, &s.symbol);
    if noise {
        // signature (17) and access (18) are real fields we intentionally skip
        w.message_field(17, |mw| {
            mw.message_field(2, |m2| {
                m2.message_field(3, |m3| {
                    m3.string_field(2, "scala/Int#");
                });
            });
        });
        w.message_field(18, |mw| {
            mw.message_field(7, |_| {});
        });
    }
    w.varint_field(3, s.kind_code as i64 as u64);
    w.int32_field(4, s.properties);
    w.string_field(5, &s.display_name);
    for o in &s.overridden_symbols {
        w.string_field(19, o);
    }
    if noise {
        w.varint_field(16, 1); // language
    }
}

fn write_document(w: &mut ProtoTestWriter, d: &SdbDocument, noise: bool) {
    w.varint_field(1, d.schema as i64 as u64);
    w.string_field(2, &d.uri);
    w.string_field(3, &d.text);
    if noise {
        // Diagnostic payload (field 7): must be skipped without breaking.
        w.message_field(7, |dw| {
            dw.message_field(1, |mw| write_range(mw, &range(1, 2, 3, 4)));
            dw.varint_field(2, 2);
            dw.string_field(3, "unused diagnostic");
        });
    }
    w.string_field(11, &d.md5);
    w.varint_field(10, d.language_code as i64 as u64);
    for s in &d.symbols {
        w.message_field(5, |mw| write_symbol_info(mw, s, noise));
    }
    for o in &d.occurrences {
        w.message_field(6, |mw| write_occurrence(mw, o, noise));
    }
    if noise {
        // Synthetic payload (12), build_target (13), plus unknown fields of
        // every wire type.
        w.message_field(12, |sw| {
            sw.message_field(1, |mw| write_range(mw, &range(0, 0, 0, 1)));
        });
        w.string_field(13, "build-target");
        w.varint_field(98, i64::MAX as u64);
        w.fixed64_field(99, i64::MIN as u64);
        w.varint_field(100, (-1i64) as u64); // 10-byte varint
    }
}

/// Encodes the SemanticDB message subset (and optional unknown-field noise) with
/// the field numbers of scalameta semanticdb.proto.
pub fn encode(docs: &[SdbDocument], noise: bool) -> Vec<u8> {
    let mut w = ProtoTestWriter::new();
    if noise {
        w.varint_field(2, 42); // unknown field in TextDocuments
    }
    for d in docs {
        w.message_field(1, |mw| write_document(mw, d, noise));
    }
    if noise {
        w.bytes_field(55, &[1, 2, 3]);
    }
    w.bytes()
}

// ---- small constructors mirroring the Scala test fixtures ----

pub fn range(sl: i32, sc: i32, el: i32, ec: i32) -> SdbRange {
    SdbRange {
        start_line: sl,
        start_character: sc,
        end_line: el,
        end_character: ec,
    }
}

pub fn sym(
    symbol: &str,
    kind: i32,
    props: i32,
    display: &str,
    overridden: &[&str],
) -> SdbSymbolInfo {
    SdbSymbolInfo {
        symbol: symbol.into(),
        kind_code: kind,
        properties: props,
        display_name: display.into(),
        overridden_symbols: overridden.iter().map(|s| s.to_string()).collect(),
    }
}

pub fn occ(range: Option<SdbRange>, symbol: &str, role: i32) -> SdbOccurrence {
    SdbOccurrence {
        range,
        symbol: symbol.into(),
        role_code: role,
    }
}

pub fn doc(
    schema: i32,
    uri: &str,
    text: &str,
    md5: &str,
    language: i32,
    symbols: Vec<SdbSymbolInfo>,
    occurrences: Vec<SdbOccurrence>,
) -> SdbDocument {
    SdbDocument {
        schema,
        uri: uri.into(),
        text: text.into(),
        md5: md5.into(),
        language_code: language,
        symbols,
        occurrences,
    }
}
