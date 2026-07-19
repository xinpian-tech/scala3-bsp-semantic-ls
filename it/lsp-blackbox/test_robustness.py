"""Protocol robustness through the real binary: implemented-but-absent LSP
methods answer a typed error (never a silent empty result), unknown
executeCommand identifiers are typed unknown-command errors, and the advertised
`pcPluginStatus` over a never-booted (cold) island answers the typed cold
status rather than booting the JVM.
"""

import pytest
from lsprotocol import types

from conftest import CORE, PC_PLUGIN_STATUS, await_ready, execute, source_uri


async def test_an_unimplemented_method_is_a_typed_error(client):
    await await_ready(client)
    with pytest.raises(Exception) as excinfo:
        await client.text_document_folding_range_async(
            types.FoldingRangeParams(
                text_document=types.TextDocumentIdentifier(uri=source_uri(CORE))
            )
        )
    # The wire answer is the METHOD_NOT_FOUND (-32601) typed error from the
    # dispatch layer, surfaced by pygls as JsonRpcMethodNotFound.
    assert "unhandled request: textDocument/foldingRange" in str(excinfo.value)
    assert type(excinfo.value).__name__ == "JsonRpcMethodNotFound"


async def test_an_unknown_command_is_a_typed_error(client):
    await await_ready(client)
    with pytest.raises(Exception) as excinfo:
        await execute(client, "scala3SemanticLs.nonesuch")
    assert "unknown command 'scala3SemanticLs.nonesuch'" in str(excinfo.value)


async def test_pc_plugin_status_ready_but_cold_is_the_typed_cold_answer(client):
    """A blackbox session never issues a PC query, so the island never boots;
    the advertised pcPluginStatus command answers the typed cold status (a
    success string, not an error) instead of booting the JVM to inspect it."""
    await await_ready(client)
    result = await execute(client, PC_PLUGIN_STATUS)
    assert "pc plugin status unavailable" in result
    assert "PC island not booted (cold)" in result
    assert "reported after the first PC query boots it" in result
