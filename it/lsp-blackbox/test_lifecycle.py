"""Lifecycle over real stdio: initialize capabilities, readiness, doctor.

The capability assertions mirror `crates/ls-server/src/capabilities.rs` — the
advertised set is exactly the implemented surface (inlayHint / selectionRange /
foldingRange / semanticTokens included). Here the payload additionally
round-trips through lsprotocol's INDEPENDENT typed model, so a serialization
drift in the serde layer (hand-rolled or lsp-types-bridged) fails loudly in a
client this repo did not write. The semantic-tokens LEGEND itself is pinned in
test_semantic_tokens.py.
"""

from lsprotocol import types

from conftest import (
    COMPILE,
    DOCTOR,
    PC_PLUGIN_STATUS,
    REINDEX,
    await_ready,
    execute,
)


async def test_initialize_advertises_the_exact_capability_surface(client):
    result = client.init_result
    caps = result.capabilities

    assert caps.text_document_sync == types.TextDocumentSyncKind.Incremental
    assert caps.position_encoding == types.PositionEncodingKind.Utf16
    assert caps.completion_provider.resolve_provider is True
    assert list(caps.completion_provider.trigger_characters) == ["."]
    assert caps.hover_provider is True
    assert list(caps.signature_help_provider.trigger_characters) == ["(", ","]
    assert caps.definition_provider is True
    assert caps.type_definition_provider is True
    assert caps.references_provider is True
    assert caps.rename_provider.prepare_provider is True
    assert caps.document_highlight_provider is True
    assert caps.workspace_symbol_provider is True
    # The index-backed navigation pair: plain booleans. documentSymbol always
    # answers the NESTED DocumentSymbol shape (hierarchical support is not
    # negotiated); implementation is the override-family query.
    assert caps.document_symbol_provider is True
    assert caps.implementation_provider is True
    # Index-backed call hierarchy (usage-hierarchy semantics): a plain boolean.
    assert caps.call_hierarchy_provider is True
    # The payload-backed providers: inlay hints without resolve (every hint
    # ships complete), selection range and folding range as plain booleans.
    assert caps.inlay_hint_provider is not None
    assert caps.inlay_hint_provider.resolve_provider is False
    assert caps.selection_range_provider is True
    assert caps.folding_range_provider is True
    # formatting: the scalafmt-CLI handler as a plain boolean; range and
    # on-type formatting are deliberately NOT advertised (the CLI's hidden
    # `--range` skips lines inside multi-line ranges).
    assert caps.document_formatting_provider is True
    assert caps.document_range_formatting_provider is None
    assert caps.document_on_type_formatting_provider is None
    # codeAction: exactly the four assembly kinds, resolve OFF (every action
    # carries its WorkspaceEdit inline — there is no codeAction/resolve).
    assert caps.code_action_provider is not None
    assert list(caps.code_action_provider.code_action_kinds) == [
        types.CodeActionKind.QuickFix,
        types.CodeActionKind.RefactorRewrite,
        types.CodeActionKind.RefactorExtract,
        types.CodeActionKind.RefactorInline,
    ]
    assert caps.code_action_provider.resolve_provider is False
    assert list(caps.execute_command_provider.commands) == [
        DOCTOR,
        REINDEX,
        COMPILE,
        PC_PLUGIN_STATUS,
    ]

    # semanticTokens: range as a plain boolean, full as {delta: true} — the
    # server answers textDocument/semanticTokens/full/delta and /full
    # responses carry the resultId it deltas against (the exact legend lists
    # are pinned in test_semantic_tokens.py).
    assert caps.semantic_tokens_provider is not None
    assert caps.semantic_tokens_provider.full == types.SemanticTokensFullDelta(
        delta=True
    )
    assert caps.semantic_tokens_provider.range is True


async def test_initialize_reports_the_server_identity(client):
    info = client.init_result.server_info
    assert info.name == "scala3-bsp-semantic-ls"
    assert info.version == "0.1.0"


async def test_bootstrap_reaches_ready_over_the_fake_bsp(client):
    report = await await_ready(client)
    assert "state: ready" in report


async def test_doctor_flags_the_semanticdb_less_target(client):
    await await_ready(client)
    report = await execute(client, DOCTOR)
    assert "fixture-nosdb" in report
    assert "without SemanticDB" in report


async def test_doctor_reports_the_cold_pc_plugins_reason(client):
    """A blackbox session never issues a PC query, so the island stays cold and
    the doctor's `PC Plugins` section carries the typed cold reason — gathered
    without booting the JVM."""
    await await_ready(client)
    report = await execute(client, DOCTOR)
    assert "PC Plugins:" in report
    assert "unavailable: PC island not booted (cold)" in report
    assert "reported after the first PC query boots it" in report
