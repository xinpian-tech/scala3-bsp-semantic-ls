"""Lifecycle over real stdio: initialize capabilities, readiness, doctor.

The capability assertions mirror `crates/ls-server/src/capabilities.rs` — the
advertised set is exactly the implemented surface, and `semanticTokens` /
`inlayHint` are deliberately absent. Here the payload additionally round-trips
through lsprotocol's INDEPENDENT typed model, so a serialization drift in the
hand-rolled serde layer fails loudly in a client this repo did not write.
"""

from lsprotocol import types

from conftest import COMPILE, DOCTOR, REINDEX, await_ready, execute


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
    assert list(caps.execute_command_provider.commands) == [DOCTOR, REINDEX, COMPILE]

    # Deliberately absent: not implemented, so never advertised.
    assert caps.semantic_tokens_provider is None
    assert caps.inlay_hint_provider is None


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
