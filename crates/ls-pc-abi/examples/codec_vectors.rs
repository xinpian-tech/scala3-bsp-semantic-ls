//! Regenerates the cross-language golden vectors for the ABI v2 payload-query
//! carriers (the block appended to
//! `modules/ls-pc-host/test/resources/codec-vectors.txt`). Run with
//! `cargo run -p ls-pc-abi --example codec_vectors`: the `name=hex` lines go to
//! stdout (paste them into the vectors file); the layout canary and ABI version
//! go to stderr (the constants `LayoutSuite` pins). The pre-v2 vectors in the
//! committed file are byte-stable and are not regenerated here.

use ls_pc_abi::payloads::{
    code_action_id, folding_kind, AutoImport, AutoImportParams, AutoImportsResult,
    CodeActionParams, CodeActionResult, FoldingRange, FoldingRangesResult, InlayHint,
    InlayHintParams, InlayHintsResult, InlayLabelPart, PcDiagnostic, PcDiagnosticsResult, Pos, Rng,
    SelectionRangeParams, SelectionRangesResult, SemanticNode, SemanticTokensResult, TextEdit,
    ToplevelsResult, UriParams,
};
use ls_pc_abi::{ABI_VERSION, LAYOUT_CANARY};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn emit(name: &str, bytes: Vec<u8>) {
    println!("{name}={}", hex(&bytes));
}

fn rng(a: u32, b: u32, c: u32, d: u32) -> Rng {
    Rng {
        start_line: a,
        start_character: b,
        end_line: c,
        end_character: d,
    }
}

fn pos(line: u32, character: u32) -> Pos {
    Pos { line, character }
}

fn main() {
    eprintln!("ABI_VERSION = {ABI_VERSION}");
    eprintln!("LAYOUT_CANARY = 0x{LAYOUT_CANARY:016x}");

    emit(
        "inlay_hint_params",
        InlayHintParams {
            uri: "file:///H.scala".to_string(),
            range: rng(0, 0, 20, 0),
            flags: 3,
        }
        .encode()
        .unwrap(),
    );
    emit(
        "inlay_hints",
        InlayHintsResult {
            hints: vec![InlayHint {
                position: pos(2, 10),
                label_parts: vec![
                    InlayLabelPart {
                        text: ": Int".to_string(),
                        location: Some(("file:///I.scala".to_string(), rng(1, 0, 1, 3))),
                        tooltip: Some("inferred type".to_string()),
                    },
                    InlayLabelPart {
                        text: "=>".to_string(),
                        location: None,
                        tooltip: None,
                    },
                ],
                kind: 1,
                padding_left: true,
                padding_right: false,
                text_edits: Some(vec![TextEdit {
                    range: rng(2, 10, 2, 10),
                    new_text: ": Int".to_string(),
                }]),
                data: Some(vec![1, 2, 3]),
            }],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "inlay_hints_empty",
        InlayHintsResult { hints: vec![] }.encode().unwrap(),
    );
    emit(
        "uri_params",
        UriParams {
            uri: "file:///U.scala".to_string(),
        }
        .encode()
        .unwrap(),
    );
    emit(
        "semantic_tokens",
        SemanticTokensResult {
            nodes: vec![
                SemanticNode {
                    start: 0,
                    end: 6,
                    token_type: 3,
                    token_modifier: 1,
                },
                SemanticNode {
                    start: 10,
                    end: 14,
                    token_type: 15,
                    token_modifier: 0,
                },
            ],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "semantic_tokens_empty",
        SemanticTokensResult { nodes: vec![] }.encode().unwrap(),
    );
    emit(
        "selection_range_params",
        SelectionRangeParams {
            uri: "file:///S.scala".to_string(),
            positions: vec![pos(1, 2), pos(3, 4)],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "selection_ranges",
        SelectionRangesResult {
            chains: vec![
                vec![rng(1, 2, 1, 4), rng(1, 0, 2, 0), rng(0, 0, 9, 0)],
                vec![],
            ],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "selection_ranges_empty",
        SelectionRangesResult { chains: vec![] }.encode().unwrap(),
    );
    emit(
        "code_action_params",
        CodeActionParams {
            uri: "file:///C.scala".to_string(),
            action: code_action_id::EXTRACT_METHOD,
            position: pos(5, 1),
            extraction_end: Some(pos(7, 2)),
            arg_indices: Some(vec![0, 2]),
        }
        .encode()
        .unwrap(),
    );
    emit(
        "code_action_params_bare",
        CodeActionParams {
            uri: "file:///C.scala".to_string(),
            action: code_action_id::INSERT_INFERRED_TYPE,
            position: pos(5, 1),
            extraction_end: None,
            arg_indices: None,
        }
        .encode()
        .unwrap(),
    );
    emit(
        "code_action_edits",
        CodeActionResult {
            edits: vec![TextEdit {
                range: rng(3, 0, 3, 0),
                new_text: ": Int".to_string(),
            }],
            refusal: None,
        }
        .encode()
        .unwrap(),
    );
    emit(
        "code_action_refusal",
        CodeActionResult {
            edits: vec![],
            refusal: Some("Cannot extract selection".to_string()),
        }
        .encode()
        .unwrap(),
    );
    emit(
        "auto_import_params",
        AutoImportParams {
            uri: "file:///A.scala".to_string(),
            position: pos(4, 9),
            name: "Future".to_string(),
            is_extension: false,
        }
        .encode()
        .unwrap(),
    );
    emit(
        "auto_imports",
        AutoImportsResult {
            imports: vec![AutoImport {
                package_name: "scala.concurrent".to_string(),
                edits: vec![TextEdit {
                    range: rng(0, 0, 0, 0),
                    new_text: "import scala.concurrent.Future\n".to_string(),
                }],
                symbol: Some("scala/concurrent/Future#".to_string()),
            }],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "auto_imports_empty",
        AutoImportsResult { imports: vec![] }.encode().unwrap(),
    );
    emit(
        "pc_diagnostics",
        PcDiagnosticsResult {
            diagnostics: vec![PcDiagnostic {
                range: rng(3, 0, 3, 5),
                severity: 1,
                code: "E007".to_string(),
                message: "not found: value x".to_string(),
            }],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "pc_diagnostics_empty",
        PcDiagnosticsResult {
            diagnostics: vec![],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "folding_ranges",
        FoldingRangesResult {
            ranges: vec![
                FoldingRange {
                    range: rng(0, 0, 5, 1),
                    kind: folding_kind::IMPORTS,
                },
                FoldingRange {
                    range: rng(6, 10, 9, 1),
                    kind: folding_kind::NONE,
                },
            ],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "folding_ranges_empty",
        FoldingRangesResult { ranges: vec![] }.encode().unwrap(),
    );
    emit(
        "toplevels",
        ToplevelsResult {
            symbols: vec!["a/b/Main.".to_string(), "a/b/Main#".to_string()],
        }
        .encode()
        .unwrap(),
    );
    emit(
        "toplevels_empty",
        ToplevelsResult { symbols: vec![] }.encode().unwrap(),
    );
}
