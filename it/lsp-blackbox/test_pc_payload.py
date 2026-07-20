"""The payload-backed PC methods over the real binary: inlayHint,
selectionRange, and foldingRange round-trip through lsprotocol's independent
typed model and answer their graceful empty/null fallbacks.

These methods are PC-backed, and a blackbox session NEVER opens the documents
it queries, so the `withPcBuffer` gate answers each method's fallback WITHOUT
booting the embedded JVM island (the island stays cold for the whole session —
asserted via the typed cold pcPluginStatus after the queries ran). The
positive, island-answered shapes are pinned elsewhere: hermetically by the
JVM-free wire suite `crates/ls-server/tests/pc_wire.rs` over the fake PC, and
against the real island by the editor e2e `it/nvim/e2e.lua`.
"""

import pytest
from lsprotocol import types

from conftest import CORE, PC_PLUGIN_STATUS, await_ready, execute, source_uri


async def test_inlay_hint_on_an_unopened_buffer_is_the_empty_list(client):
    """A semanticdb-owned URI with no open buffer takes the `withPcBuffer`
    fallback: the empty hint list, never an error and never a JVM boot."""
    await await_ready(client)
    result = await client.text_document_inlay_hint_async(
        types.InlayHintParams(
            text_document=types.TextDocumentIdentifier(uri=source_uri(CORE)),
            range=types.Range(
                start=types.Position(line=0, character=0),
                end=types.Position(line=10, character=0),
            ),
        )
    )
    # The wire answer is `[]`, not null (lsprotocol surfaces it as an empty
    # sequence).
    assert result is not None
    assert list(result) == []


async def test_inlay_hint_on_a_no_semanticdb_source_is_a_typed_error(client):
    """inlayHint follows the hover/completion discipline: `requireSemanticdb`
    gates BEFORE the buffer fallback, so a source owned by a target compiled
    without SemanticDB is a hard error, not an empty list."""
    await await_ready(client)
    nosdb_uri = (client.workspace / "nosdb" / "NoSdb.scala").as_uri()
    with pytest.raises(Exception) as excinfo:
        await client.text_document_inlay_hint_async(
            types.InlayHintParams(
                text_document=types.TextDocumentIdentifier(uri=nosdb_uri),
                range=types.Range(
                    start=types.Position(line=0, character=0),
                    end=types.Position(line=1, character=0),
                ),
            )
        )
    assert "has no SemanticDB output" in str(excinfo.value)


async def test_selection_range_on_an_unopened_buffer_is_null(client):
    """selectionRange has NO semanticdb gate (pure syntax), only the open-buffer
    gate — and its fallback is null, never an array whose length mismatches the
    request's positions (the spec ties result[i] to positions[i])."""
    await await_ready(client)
    result = await client.text_document_selection_range_async(
        types.SelectionRangeParams(
            text_document=types.TextDocumentIdentifier(uri=source_uri(CORE)),
            positions=[types.Position(line=2, character=6)],
        )
    )
    assert result is None


async def test_folding_range_on_an_unopened_buffer_is_the_empty_list(client):
    """The positive foldingRange blackbox probe (the method used to be the
    METHOD-NOT-FOUND example in test_robustness.py): the request round-trips
    as a real handler and answers the graceful empty list for a buffer the PC
    mirror does not hold — no error, no METHOD_NOT_FOUND, no JVM."""
    await await_ready(client)
    result = await client.text_document_folding_range_async(
        types.FoldingRangeParams(
            text_document=types.TextDocumentIdentifier(uri=source_uri(CORE))
        )
    )
    # The wire answer is `[]`, not null (lsprotocol surfaces it as an empty
    # sequence).
    assert result is not None
    assert list(result) == []


async def test_the_payload_fallbacks_never_boot_the_island(client):
    """Drive all three methods, then assert the island is STILL cold: the
    gate fallbacks answered without a PC query, so the blackbox session keeps
    its zero-JVM guarantee."""
    await await_ready(client)
    doc = types.TextDocumentIdentifier(uri=source_uri(CORE))
    await client.text_document_inlay_hint_async(
        types.InlayHintParams(
            text_document=doc,
            range=types.Range(
                start=types.Position(line=0, character=0),
                end=types.Position(line=5, character=0),
            ),
        )
    )
    await client.text_document_selection_range_async(
        types.SelectionRangeParams(
            text_document=doc, positions=[types.Position(line=0, character=0)]
        )
    )
    await client.text_document_folding_range_async(
        types.FoldingRangeParams(text_document=doc)
    )
    status = await execute(client, PC_PLUGIN_STATUS)
    assert "PC island not booted (cold)" in status
