//! The advertised server capabilities and `initialize` result. The set is
//! exactly the server's implemented surface: full text sync; completion (trigger
//! `.`, resolve); hover; signature help (triggers `(`, `,`); definition;
//! type-definition; references; rename (prepare); document highlight; workspace
//! symbol; and the execute-command set. `semanticTokens` and `inlayHint` are
//! deliberately absent.

use serde::Serialize;

/// The server's advertised identity.
pub const SERVER_NAME: &str = "scala3-bsp-semantic-ls";
pub const SERVER_VERSION: &str = "0.1.0";

/// The executeCommand identifiers the server advertises and handles.
pub mod commands {
    pub const DOCTOR: &str = "scala3SemanticLs.doctor";
    pub const REINDEX: &str = "scala3SemanticLs.reindex";
    pub const COMPILE: &str = "scala3SemanticLs.compile";
    pub const PC_PLUGIN_STATUS: &str = "scala3SemanticLs.pcPluginStatus";

    /// Every advertised command, in the order the client sees them.
    pub fn all() -> Vec<String> {
        vec![
            DOCTOR.to_string(),
            REINDEX.to_string(),
            COMPILE.to_string(),
            PC_PLUGIN_STATUS.to_string(),
        ]
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
    /// `1` == full document sync (`TextDocumentSyncKind.Full`).
    pub text_document_sync: u32,
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
        text_document_sync: 1,
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
    fn registers_full_text_document_sync() {
        assert!(initialize_json().contains("\"textDocumentSync\":1"));
    }

    #[test]
    fn lists_exactly_the_four_execute_command_commands() {
        let json = initialize_json();
        for command in commands::all() {
            assert!(json.contains(&format!("\"{command}\"")), "{json}");
        }
        assert_eq!(commands::all().len(), 4);
    }

    #[test]
    fn semantic_tokens_and_inlay_hint_are_absent() {
        let json = initialize_json();
        assert!(!json.contains("semanticTokensProvider"), "{json}");
        assert!(!json.contains("inlayHintProvider"), "{json}");
    }
}
