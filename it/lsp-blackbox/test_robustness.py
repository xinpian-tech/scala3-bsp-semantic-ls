"""Protocol robustness through the real binary: implemented-but-absent LSP
methods answer a typed error (never a silent empty result), unknown
executeCommand identifiers are typed unknown-command errors, the advertised
`pcPluginStatus` over a never-booted (cold) island answers the typed cold
status rather than booting the JVM, and `$/cancelRequest` (bogus or raced) is
absorbed without ever wedging the session.
"""

import pytest
from lsprotocol import types
from pygls.exceptions import JsonRpcRequestCancelled

from conftest import CORE, DOCTOR, PC_PLUGIN_STATUS, await_ready, execute, source_uri


async def test_an_unimplemented_method_is_a_typed_error(client):
    # `textDocument/documentColor` is the still-unimplemented probe
    # (foldingRange graduated to a real handler — its positive blackbox test
    # lives in test_pc_payload.py).
    await await_ready(client)
    with pytest.raises(Exception) as excinfo:
        await client.text_document_document_color_async(
            types.DocumentColorParams(
                text_document=types.TextDocumentIdentifier(uri=source_uri(CORE))
            )
        )
    # The wire answer is the METHOD_NOT_FOUND (-32601) typed error from the
    # dispatch layer, surfaced by pygls as JsonRpcMethodNotFound.
    assert "unhandled request: textDocument/documentColor" in str(excinfo.value)
    assert type(excinfo.value).__name__ == "JsonRpcMethodNotFound"


async def test_an_unknown_command_is_a_typed_error(client):
    await await_ready(client)
    with pytest.raises(Exception) as excinfo:
        await execute(client, "scala3SemanticLs.nonesuch")
    assert "unknown command 'scala3SemanticLs.nonesuch'" in str(excinfo.value)


async def test_a_cancel_for_a_bogus_id_leaves_the_server_serviceable(client):
    """A `$/cancelRequest` for an id the server never saw (number or string
    shape) is inert: it is intercepted off the wire, never answered, and the
    follow-up request is served normally."""
    await await_ready(client)
    client.protocol.notify(types.CANCEL_REQUEST, types.CancelParams(id=424242))
    client.protocol.notify(
        types.CANCEL_REQUEST, types.CancelParams(id="no-such-request")
    )
    report = await execute(client, DOCTOR)
    assert "state: ready" in report


async def test_a_cancel_raced_against_a_live_request_yields_exactly_one_reply(client):
    """A cancel racing its own request may land before the request's turn
    (answered with RequestCancelled, -32800) or after it already ran (answered
    with the result) — both are spec-legal. Either way exactly one reply
    arrives and the session stays fully serviceable."""
    await await_ready(client)
    params = types.ExecuteCommandParams(command=DOCTOR, arguments=[])
    future = client.protocol.send_request_async(
        types.WORKSPACE_EXECUTE_COMMAND, params, msg_id="cancel-race-1"
    )
    client.protocol.notify(
        types.CANCEL_REQUEST, types.CancelParams(id="cancel-race-1")
    )
    try:
        result = await future
        assert "state:" in result  # the request ran to completion
    except JsonRpcRequestCancelled:
        pass  # cancelled before its turn — the other legal outcome
    report = await execute(client, DOCTOR)
    assert "state: ready" in report


async def test_pc_plugin_status_ready_but_cold_is_the_typed_cold_answer(client):
    """A blackbox session never issues a PC query, so the island never boots;
    the advertised pcPluginStatus command answers the typed cold status (a
    success string, not an error) instead of booting the JVM to inspect it."""
    await await_ready(client)
    result = await execute(client, PC_PLUGIN_STATUS)
    assert "pc plugin status unavailable" in result
    assert "PC island not booted (cold)" in result
    assert "reported after the first PC query boots it" in result
