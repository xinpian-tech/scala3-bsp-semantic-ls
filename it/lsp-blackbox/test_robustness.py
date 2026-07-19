"""Protocol robustness through the real binary: implemented-but-absent LSP
methods answer a typed error (never a silent empty result), and unknown
executeCommand identifiers — including the deliberately un-advertised
`pcPluginStatus` — are typed unknown-command errors.
"""

import pytest
from lsprotocol import types

from conftest import CORE, await_ready, execute, source_uri


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
        await execute(client, "scala3SemanticLs.pcPluginStatus")
    assert "unknown command 'scala3SemanticLs.pcPluginStatus'" in str(excinfo.value)
