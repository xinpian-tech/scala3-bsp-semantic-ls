//! The advertised server capabilities and `initialize` result. The set is
//! exactly the server's implemented surface: incremental text sync (with the
//! explicit `utf-16` position encoding, the LSP default the server assumes
//! everywhere); completion (trigger `.`, resolve); hover; signature help
//! (triggers `(`, `,`); definition;
//! type-definition; references; rename (prepare); document highlight; workspace
//! symbol; document symbol (nested `DocumentSymbol` trees are ALWAYS sent —
//! the client capability `hierarchicalDocumentSymbolSupport` is deliberately
//! not modeled, keeping the narrow no-client-negotiation policy below; the
//! flat `SymbolInformation[]` fallback for pre-3.10 clients is skipped);
//! implementation (index-backed method override families);
//! inlay hint (no resolve — every hint ships complete, the
//! `lsp_types::InlayHintOptions` shape); code action (the four assembly kinds
//! — quickfix, refactor.rewrite, refactor.extract, refactor.inline — with no
//! resolve: every action carries its edit inline); selection range; folding
//! range;
//! semantic tokens (`full` + `range` over the PC-vendored legend, no
//! `full.delta` — delta requests are not implemented so they must not be
//! advertised); and the execute-command set.
//!
//! Semantic tokens are advertised UNCONDITIONALLY, without reading the
//! client's `textDocument.semanticTokens` capability: every mainstream client
//! (VS Code, Neovim 0.10+) sends the standard token capability, a client that
//! lacks it simply never issues the request, and the server keeps its narrow
//! typed-flag policy (see `watched_files_dynamic_registration`) instead of
//! growing a general client-capability negotiation model.

use serde::Serialize;

/// The server's advertised identity.
pub const SERVER_NAME: &str = "scala3-bsp-semantic-ls";
pub const SERVER_VERSION: &str = "0.1.0";

/// The executeCommand identifiers the server advertises and handles — the v1
/// set (`Commands.all`), `pcPluginStatus` included: it reports the embedded PC
/// island's plugin state, answering a typed "not booted (cold)" status while
/// the island is still cold (the inspection never boots the JVM).
pub mod commands {
    pub const DOCTOR: &str = "scala3SemanticLs.doctor";
    pub const REINDEX: &str = "scala3SemanticLs.reindex";
    pub const COMPILE: &str = "scala3SemanticLs.compile";
    pub const PC_PLUGIN_STATUS: &str = "scala3SemanticLs.pcPluginStatus";

    /// Every advertised command, in the order the client sees them (the Scala
    /// `Commands.all`).
    pub fn all() -> Vec<String> {
        vec![
            DOCTOR.to_string(),
            REINDEX.to_string(),
            COMPILE.to_string(),
            PC_PLUGIN_STATUS.to_string(),
        ]
    }
}

/// The glob patterns the server registers for client-side file watching
/// (`workspace/didChangeWatchedFiles` dynamic registration, sent after
/// `initialized` when the client advertised
/// `workspace.didChangeWatchedFiles.dynamicRegistration`). One source of truth:
/// the registration payload (`server::serve`) and the event filter
/// (`services::CoreHandlers::on_watched_files`) both read these, so the globs
/// the client watches and the globs the server reacts to cannot drift apart.
pub mod watch_globs {
    /// Freshly compiled SemanticDB output anywhere under the workspace — the
    /// out-of-editor reingest trigger (a build ran outside a didSave).
    pub const SEMANTICDB: &str = "**/*.semanticdb";
    /// The workspace configuration file; a change nudges the PC island to
    /// re-read it (`PcQueryService::on_config_changed`).
    pub const CONFIG: &str = "**/.scala3-bsp-semantic-ls/config.json";
    /// BSP connection files; a change is only logged (reconnecting a live
    /// session in place is out of scope — restart the server).
    pub const BSP: &str = "**/.bsp/*.json";

    /// The registered globs, in registration (and filter-index) order.
    pub fn all() -> [&'static str; 3] {
        [SEMANTICDB, CONFIG, BSP]
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionOptions {
    pub resolve_provider: bool,
    pub trigger_characters: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureHelpOptions {
    pub trigger_characters: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameOptions {
    pub prepare_provider: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandOptions {
    pub commands: Vec<String>,
}

/// The `ServerCapabilities` payload. Fields serialize to the LSP camelCase
/// spelling. The payload-backed providers added on the lsp-types edge
/// (`inlayHintProvider`/`selectionRangeProvider`/`foldingRangeProvider`/
/// `semanticTokensProvider`) use the upstream `lsp_types` options shape where
/// the capability carries options, and a plain `true` where it is boolean.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// `2` == incremental sync (`TextDocumentSyncKind.Incremental`): didChange
    /// carries ranged `contentChanges` the server folds into its buffer.
    pub text_document_sync: u32,
    /// `"utf-16"`, the LSP default, advertised explicitly (additive LSP 3.17
    /// field): every position the server reads or writes is in UTF-16 units.
    pub position_encoding: String,
    pub completion_provider: CompletionOptions,
    pub hover_provider: bool,
    pub signature_help_provider: SignatureHelpOptions,
    pub definition_provider: bool,
    pub type_definition_provider: bool,
    pub references_provider: bool,
    pub rename_provider: RenameOptions,
    pub document_highlight_provider: bool,
    pub workspace_symbol_provider: bool,
    /// Plain `true`. The server always answers the NESTED
    /// `DocumentSymbol[]` shape — `hierarchicalDocumentSymbolSupport` is not
    /// read (see the module doc), and the flat `SymbolInformation[]` fallback
    /// is deliberately not implemented.
    pub document_symbol_provider: bool,
    /// Plain `true` — index-backed `textDocument/implementation` over method
    /// override families.
    pub implementation_provider: bool,
    /// `{resolveProvider: false}` — every hint ships complete; there is no
    /// `inlayHint/resolve` handler, so lazy-resolve must not be advertised.
    pub inlay_hint_provider: lsp_types::InlayHintOptions,
    /// `{codeActionKinds: [quickfix, refactor.rewrite, refactor.extract,
    /// refactor.inline], resolveProvider: false}` — the kinds the assembly
    /// layer can produce (`services::assemble_code_actions`). Every action is
    /// literal with its `WorkspaceEdit` inline (eager resolution), so
    /// `codeAction/resolve` must not be advertised.
    pub code_action_provider: lsp_types::CodeActionOptions,
    pub selection_range_provider: bool,
    pub folding_range_provider: bool,
    /// `{legend, full: true, range: true}` — the legend is EXACTLY the
    /// PC-vendored `scala.meta.internal.pc.SemanticTokens` lists
    /// ([`crate::pc_lsp::legend`]), because the island's node type/modifier
    /// ints index those lists. `full` is the plain boolean — `full.delta` must
    /// NOT be advertised (no delta handler exists; `resultId` is never
    /// emitted).
    pub semantic_tokens_provider: lsp_types::SemanticTokensOptions,
    pub execute_command_provider: ExecuteCommandOptions,
}

/// The advertised semantic-tokens capability: the vendored legend, `full` and
/// `range` both plain `true`.
pub fn semantic_tokens_options() -> lsp_types::SemanticTokensOptions {
    // The golden anchors of the cross-language legend contract, re-checked at
    // the point the legend is advertised (debug builds; the release value is
    // the same constant the tests pin).
    debug_assert_eq!(
        crate::pc_lsp::legend::TOKEN_TYPES[crate::pc_lsp::legend::METHOD_TYPE_INDEX],
        "method"
    );
    debug_assert_eq!(
        crate::pc_lsp::legend::TOKEN_MODIFIERS[crate::pc_lsp::legend::DECLARATION_MODIFIER_INDEX],
        "declaration"
    );
    lsp_types::SemanticTokensOptions {
        work_done_progress_options: Default::default(),
        legend: lsp_types::SemanticTokensLegend {
            token_types: crate::pc_lsp::legend::TOKEN_TYPES
                .iter()
                .map(|&name| lsp_types::SemanticTokenType::new(name))
                .collect(),
            token_modifiers: crate::pc_lsp::legend::TOKEN_MODIFIERS
                .iter()
                .map(|&name| lsp_types::SemanticTokenModifier::new(name))
                .collect(),
        },
        range: Some(true),
        full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
    }
}

/// The advertised code-action capability: exactly the four kinds the assembly
/// produces, no resolve (every action ships its edit inline).
pub fn code_action_options() -> lsp_types::CodeActionOptions {
    lsp_types::CodeActionOptions {
        code_action_kinds: Some(vec![
            lsp_types::CodeActionKind::QUICKFIX,
            lsp_types::CodeActionKind::REFACTOR_REWRITE,
            lsp_types::CodeActionKind::REFACTOR_EXTRACT,
            lsp_types::CodeActionKind::REFACTOR_INLINE,
        ]),
        work_done_progress_options: Default::default(),
        resolve_provider: Some(false),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

/// Builds the exact capability set the server implements.
pub fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: 2,
        position_encoding: "utf-16".to_string(),
        completion_provider: CompletionOptions {
            resolve_provider: true,
            trigger_characters: vec![".".to_string()],
        },
        hover_provider: true,
        signature_help_provider: SignatureHelpOptions {
            trigger_characters: vec!["(".to_string(), ",".to_string()],
        },
        definition_provider: true,
        type_definition_provider: true,
        references_provider: true,
        rename_provider: RenameOptions {
            prepare_provider: true,
        },
        document_highlight_provider: true,
        workspace_symbol_provider: true,
        document_symbol_provider: true,
        implementation_provider: true,
        inlay_hint_provider: lsp_types::InlayHintOptions {
            work_done_progress_options: Default::default(),
            resolve_provider: Some(false),
        },
        code_action_provider: code_action_options(),
        selection_range_provider: true,
        folding_range_provider: true,
        semantic_tokens_provider: semantic_tokens_options(),
        execute_command_provider: ExecuteCommandOptions {
            commands: commands::all(),
        },
    }
}

/// The synchronous `initialize` result: the capability surface plus the server's
/// identity.
pub fn initialize_result() -> InitializeResult {
    InitializeResult {
        capabilities: server_capabilities(),
        server_info: ServerInfo {
            name: SERVER_NAME.to_string(),
            version: SERVER_VERSION.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialize_json() -> String {
        serde_json::to_string(&initialize_result()).unwrap()
    }

    // Ports ls.core.CapabilitiesSuite.
    #[test]
    fn advertises_the_core_providers_plus_completion_hover_signature() {
        let json = initialize_json();
        assert!(json.contains("\"workspaceSymbolProvider\":true"), "{json}");
        assert!(json.contains("\"referencesProvider\":true"), "{json}");
        assert!(
            json.contains("\"renameProvider\":{\"prepareProvider\":true}"),
            "{json}"
        );
        assert!(
            json.contains("\"documentHighlightProvider\":true"),
            "{json}"
        );
        // The two index-backed navigation providers: plain booleans (nested
        // DocumentSymbol trees are always sent; implementation is the
        // index-backed override-family query).
        assert!(json.contains("\"documentSymbolProvider\":true"), "{json}");
        assert!(json.contains("\"implementationProvider\":true"), "{json}");
        assert!(json.contains("\"executeCommandProvider\""), "{json}");
        assert!(json.contains("\"completionProvider\""), "{json}");
        assert!(json.contains("\"resolveProvider\":true"), "{json}");
        assert!(json.contains("\"hoverProvider\":true"), "{json}");
        assert!(json.contains("\"signatureHelpProvider\""), "{json}");
        assert!(json.contains("\"definitionProvider\":true"), "{json}");
        assert!(json.contains("\"typeDefinitionProvider\":true"), "{json}");
    }

    // The three payload-backed providers on the lsp-types edge: inlay hints
    // advertise the lsp_types::InlayHintOptions shape with resolve OFF (there
    // is no inlayHint/resolve handler), selection range and folding range are
    // plain booleans.
    #[test]
    fn advertises_the_payload_backed_providers() {
        let json = initialize_json();
        assert!(
            json.contains("\"inlayHintProvider\":{\"resolveProvider\":false}"),
            "{json}"
        );
        assert!(json.contains("\"selectionRangeProvider\":true"), "{json}");
        assert!(json.contains("\"foldingRangeProvider\":true"), "{json}");
    }

    // codeActionProvider: exactly the four assembly kinds, in order, with
    // resolve OFF — every action ships its `WorkspaceEdit` inline, so a
    // `codeAction/resolve` handler must not be implied.
    #[test]
    fn advertises_code_actions_with_the_four_assembly_kinds_and_no_resolve() {
        let value: serde_json::Value =
            serde_json::from_str(&initialize_json()).expect("initialize result is JSON");
        let provider = &value["capabilities"]["codeActionProvider"];
        assert_eq!(
            provider["codeActionKinds"],
            serde_json::json!([
                "quickfix",
                "refactor.rewrite",
                "refactor.extract",
                "refactor.inline"
            ]),
            "{provider}"
        );
        assert_eq!(
            provider["resolveProvider"],
            serde_json::json!(false),
            "{provider}"
        );
    }

    #[test]
    fn registers_incremental_text_document_sync() {
        assert!(initialize_json().contains("\"textDocumentSync\":2"));
    }

    #[test]
    fn advertises_the_utf16_position_encoding() {
        assert!(initialize_json().contains("\"positionEncoding\":\"utf-16\""));
    }

    #[test]
    fn lists_exactly_the_advertised_execute_command_commands() {
        let json = initialize_json();
        for command in commands::all() {
            assert!(json.contains(&format!("\"{command}\"")), "{json}");
        }
        assert_eq!(commands::all().len(), 4);
        // The presentation-compiler plugin-status command is advertised (and
        // routed — the server_surface routed-set probe pins the round trip).
        assert!(json.contains("scala3SemanticLs.pcPluginStatus"), "{json}");
    }

    // The registered watcher globs — one source of truth for the registration
    // payload and the event filter — pinned so neither side can drift.
    #[test]
    fn the_watch_globs_are_exactly_the_registered_three() {
        assert_eq!(
            watch_globs::all(),
            [
                "**/*.semanticdb",
                "**/.scala3-bsp-semantic-ls/config.json",
                "**/.bsp/*.json",
            ]
        );
    }

    // semanticTokensProvider: the PC-vendored legend verbatim (23 types, 10
    // modifiers, the golden anchors at their pinned indices), full and range
    // as plain `true` — and NO `full.delta` (no delta handler exists, so
    // advertising it would invite requests the server cannot answer).
    #[test]
    fn advertises_semantic_tokens_with_the_vendored_legend() {
        let value: serde_json::Value =
            serde_json::from_str(&initialize_json()).expect("initialize result is JSON");
        let provider = &value["capabilities"]["semanticTokensProvider"];
        assert_eq!(provider["full"], serde_json::json!(true), "{provider}");
        assert_eq!(provider["range"], serde_json::json!(true), "{provider}");
        let token_types: Vec<&str> = provider["legend"]["tokenTypes"]
            .as_array()
            .expect("tokenTypes array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let token_modifiers: Vec<&str> = provider["legend"]["tokenModifiers"]
            .as_array()
            .expect("tokenModifiers array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(token_types, crate::pc_lsp::legend::TOKEN_TYPES);
        assert_eq!(token_modifiers, crate::pc_lsp::legend::TOKEN_MODIFIERS);
        assert_eq!(
            token_types[crate::pc_lsp::legend::METHOD_TYPE_INDEX],
            "method"
        );
        assert_eq!(
            token_modifiers[crate::pc_lsp::legend::DECLARATION_MODIFIER_INDEX],
            "declaration"
        );
    }
}
