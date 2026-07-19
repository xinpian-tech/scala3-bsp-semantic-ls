//! The advertised server capabilities and `initialize` result. The set is
//! exactly the server's implemented surface: incremental text sync (with the
//! explicit `utf-16` position encoding, the LSP default the server assumes
//! everywhere); completion (trigger `.`, resolve); hover; signature help
//! (triggers `(`, `,`); definition;
//! type-definition; references; rename (prepare); document highlight; workspace
//! symbol; and the execute-command set. `semanticTokens` and `inlayHint` are
//! deliberately absent.

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
/// spelling; the absence of `semanticTokensProvider`/`inlayHintProvider` is
/// intentional (they are not implemented).
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
    pub execute_command_provider: ExecuteCommandOptions,
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
        assert!(json.contains("\"executeCommandProvider\""), "{json}");
        assert!(json.contains("\"completionProvider\""), "{json}");
        assert!(json.contains("\"resolveProvider\":true"), "{json}");
        assert!(json.contains("\"hoverProvider\":true"), "{json}");
        assert!(json.contains("\"signatureHelpProvider\""), "{json}");
        assert!(json.contains("\"definitionProvider\":true"), "{json}");
        assert!(json.contains("\"typeDefinitionProvider\":true"), "{json}");
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

    #[test]
    fn semantic_tokens_and_inlay_hint_are_absent() {
        let json = initialize_json();
        assert!(!json.contains("semanticTokensProvider"), "{json}");
        assert!(!json.contains("inlayHintProvider"), "{json}");
    }
}
