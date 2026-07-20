"""Call hierarchy over the real stdio wire (usage-hierarchy semantics), through
lsprotocol's INDEPENDENT typed models — a serialization drift in the server's
lsp-types-bridged layer fails loudly in a client this repo did not write.

prepare on the `make` definition answers the definition-side item; its raw
SemanticDB symbol round-trips through `data`; incomingCalls resolves the callers
— INCLUDING the disconnected target-C caller in CopyCore.scala, because call
hierarchy (unlike references) does not prune the reference-visibility closure.
"""

from lsprotocol import types

from conftest import CORE, await_ready, position_of, source_uri


async def test_prepare_then_incoming_over_core(client):
    await await_ready(client)

    # prepare on the `make` definition (`object Core: def make(...)`).
    items = await client.text_document_prepare_call_hierarchy_async(
        types.CallHierarchyPrepareParams(
            text_document=types.TextDocumentIdentifier(uri=source_uri(CORE)),
            position=position_of(CORE, "make", 0),
        )
    )
    assert items, "prepareCallHierarchy returned no item for make"
    assert len(items) == 1
    item = items[0]
    assert item.name == "make"
    assert item.kind == types.SymbolKind.Method
    # The index knows definition NAME spans only: range == selectionRange.
    assert item.range == item.selection_range
    # The raw SemanticDB symbol round-trips through `data`.
    assert item.data["symbol"] == "pkga/Core.make()."

    # incomingCalls: the callers grouped by enclosing definition, including the
    # disconnected target-C caller (no closure pruning).
    incoming = await client.call_hierarchy_incoming_calls_async(
        types.CallHierarchyIncomingCallsParams(item=item)
    )
    assert incoming, "no incoming calls for make"
    names = {call.from_.name for call in incoming}
    assert names == {"core", "defaultCore"}, names
    uris = {call.from_.uri for call in incoming}
    assert any(u.endswith("c/src/pkga/CopyCore.scala") for u in uris), (
        f"the disconnected target-C caller must appear (no closure pruning): {uris}"
    )
    for call in incoming:
        assert call.from_ranges, f"caller {call.from_.name} has no fromRanges"


async def test_outgoing_resolves_the_body_targets(client):
    await await_ready(client)
    items = await client.text_document_prepare_call_hierarchy_async(
        types.CallHierarchyPrepareParams(
            text_document=types.TextDocumentIdentifier(uri=source_uri(CORE)),
            position=position_of(CORE, "make", 0),
        )
    )
    assert items
    # `make`'s body constructs the Core class: outgoing answers the Core callee.
    outgoing = await client.call_hierarchy_outgoing_calls_async(
        types.CallHierarchyOutgoingCallsParams(item=items[0])
    )
    assert [call.to.name for call in outgoing] == ["Core"], (
        f"make calls Core: {[c.to.name for c in outgoing]}"
    )
