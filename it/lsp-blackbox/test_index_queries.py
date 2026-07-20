"""Index-backed queries over the real stdio wire: workspace/symbol, references,
documentHighlight, prepareRename, rename, the hard NoSemanticdb error, and the
PC-only dirty-buffer surface. Black-box twins of the in-process scenarios in
`crates/ls-server/tests/fake_bsp_e2e.rs`, driven through the real binary.
"""

import pytest
from lsprotocol import types

from conftest import (
    CORE,
    ITEM,
    await_ready,
    doc_position,
    source_text,
    source_uri,
)


async def test_workspace_symbol_resolves_over_the_fixture_model(client):
    await await_ready(client)
    symbols = await client.workspace_symbol_async(types.WorkspaceSymbolParams(query="Core"))
    assert symbols, "workspace/symbol returned nothing for Core"
    core = next((s for s in symbols if s.name == "Core"), None)
    assert core is not None, f"no Core symbol in {[s.name for s in symbols]}"
    assert core.location.uri.endswith("a/src/pkga/Core.scala")


async def test_references_highlight_and_prepare_rename(client):
    await await_ready(client)
    at = doc_position(CORE, "Core")

    references = await client.text_document_references_async(
        types.ReferenceParams(
            **at, context=types.ReferenceContext(include_declaration=True)
        )
    )
    assert references, "expected references for Core"

    highlights = await client.text_document_document_highlight_async(
        types.DocumentHighlightParams(**at)
    )
    assert highlights, "expected highlights for Core"

    span = await client.text_document_prepare_rename_async(
        types.PrepareRenameParams(**at)
    )
    assert span is not None, "prepareRename must return the symbol span"
    assert span.start.line == at["position"].line


async def test_document_symbol_outlines_core_as_a_nested_tree(client):
    """The index-backed outline through lsprotocol's typed model: nested
    DocumentSymbol nodes (never the flat SymbolInformation shape), the known
    Core.scala declarations at the expected levels, and the documented
    range == selectionRange limitation (the index knows only name spans)."""
    await await_ready(client)
    symbols = await client.text_document_document_symbol_async(
        types.DocumentSymbolParams(
            text_document=types.TextDocumentIdentifier(uri=source_uri(CORE))
        )
    )
    assert symbols, "documentSymbol returned nothing for Core.scala"
    assert all(isinstance(s, types.DocumentSymbol) for s in symbols), (
        "the server must send the nested DocumentSymbol shape"
    )
    roots = {s.name for s in symbols}
    assert {"Core", "Greeter", "Color"} <= roots, f"unexpected roots: {roots}"
    core_class = next(s for s in symbols if s.kind == types.SymbolKind.Class and s.name == "Core")
    assert {c.name for c in core_class.children} == {"label", "ping"}
    color = next(s for s in symbols if s.name == "Color")
    assert {c.name for c in color.children} == {"Red", "Green", "Blue"}, (
        "enum cases nest under the enum class node"
    )
    for symbol in symbols:
        assert symbol.range == symbol.selection_range, (
            f"{symbol.name}: the index carries only name spans"
        )


async def test_implementation_resolves_the_override_family(client):
    """textDocument/implementation on the `Greeter#greet` declaration answers
    the overrider's def site in Impl.scala (the index override-family query);
    the leaf overrider itself answers the honest empty."""
    await await_ready(client)
    at = doc_position(CORE, "greet")
    locations = await client.text_document_implementation_async(
        types.ImplementationParams(**at)
    )
    assert locations, "expected the Greeter#greet overrider"
    assert any(loc.uri.endswith("a/src/pkga/Impl.scala") for loc in locations), (
        f"expected Impl.scala in {[loc.uri for loc in locations]}"
    )

    leaf = await client.text_document_implementation_async(
        types.ImplementationParams(**doc_position("a/src/pkga/Impl.scala", "greet"))
    )
    assert not leaf, f"a leaf override answers empty, got {leaf}"


async def test_a_no_semanticdb_source_is_a_hard_error(client):
    await await_ready(client)
    uri = (client.workspace / "nosdb" / "NoSdb.scala").as_uri()
    at = {
        "text_document": types.TextDocumentIdentifier(uri=uri),
        "position": types.Position(line=0, character=6),
    }
    with pytest.raises(Exception) as excinfo:
        await client.text_document_references_async(
            types.ReferenceParams(
                **at, context=types.ReferenceContext(include_declaration=True)
            )
        )
    assert "has no SemanticDB output" in str(excinfo.value)


async def test_rename_returns_a_cross_file_workspace_edit(client):
    await await_ready(client)
    at = doc_position(ITEM, "Item")
    edit = await client.text_document_rename_async(
        types.RenameParams(**at, new_name="Renamed")
    )
    assert edit is not None and edit.changes, "rename produced no WorkspaceEdit"
    all_edits = [e for edits in edit.changes.values() for e in edits]
    assert any(e.new_text == "Renamed" for e in all_edits)


async def test_rename_surfaces_a_bsp_compile_failure(client_failing_compile):
    client = client_failing_compile
    await await_ready(client)
    at = doc_position(ITEM, "Item")
    with pytest.raises(Exception) as excinfo:
        await client.text_document_rename_async(
            types.RenameParams(**at, new_name="Renamed")
        )
    assert "buildTarget/compile failed" in str(excinfo.value)


async def test_a_ranged_did_change_dirties_the_buffer_and_rename_rejects_it(client):
    """Incremental sync end to end: didOpen with the on-disk text, then a RANGED
    didChange — the server's buffer diverges from disk exactly as edited, so
    rename answers the typed dirty-buffer rejection; a second ranged edit that
    reverts the first restores byte-equality with disk and rename succeeds
    (an off-by-one in the server-side edit application would keep it dirty)."""
    await await_ready(client)
    uri = source_uri(ITEM)
    client.text_document_did_open(
        types.DidOpenTextDocumentParams(
            text_document=types.TextDocumentItem(
                uri=uri, language_id="scala", version=1, text=source_text(ITEM)
            )
        )
    )
    at = doc_position(ITEM, "Item")

    def ranged_change(version, start, end, text):
        client.text_document_did_change(
            types.DidChangeTextDocumentParams(
                text_document=types.VersionedTextDocumentIdentifier(
                    uri=uri, version=version
                ),
                content_changes=[
                    types.TextDocumentContentChangePartial(
                        range=types.Range(start=start, end=end), text=text
                    )
                ],
            )
        )

    # Insert a line at the top: the buffer now diverges from disk.
    ranged_change(
        2,
        types.Position(line=0, character=0),
        types.Position(line=0, character=0),
        "// dirty\n",
    )
    with pytest.raises(Exception) as excinfo:
        await client.text_document_rename_async(
            types.RenameParams(**at, new_name="Renamed")
        )
    assert "has unsaved changes" in str(excinfo.value)

    # Delete exactly the inserted line: the buffer is byte-identical to disk
    # again, so the same rename now yields a WorkspaceEdit.
    ranged_change(
        3,
        types.Position(line=0, character=0),
        types.Position(line=1, character=0),
        "",
    )
    edit = await client.text_document_rename_async(
        types.RenameParams(**at, new_name="Renamed")
    )
    assert edit is not None and edit.changes, "clean-again buffer must rename"


async def test_an_unsaved_top_level_symbol_is_pc_only(client):
    await await_ready(client)
    uri = source_uri(CORE)
    text = "package pkga\n\nobject GhostWidget:\n  def z = 1\n"
    client.text_document_did_open(
        types.DidOpenTextDocumentParams(
            text_document=types.TextDocumentItem(
                uri=uri, language_id="scala", version=1, text=text
            )
        )
    )

    symbols = await client.workspace_symbol_async(
        types.WorkspaceSymbolParams(query="GhostWidget")
    )
    ghost = next((s for s in symbols or [] if s.name == "GhostWidget"), None)
    assert ghost is not None, "GhostWidget not surfaced by workspace/symbol"
    assert ghost.container_name == "unsaved buffer (PC-only)"

    at = {
        "text_document": types.TextDocumentIdentifier(uri=uri),
        "position": types.Position(line=2, character=10),
    }
    with pytest.raises(Exception) as excinfo:
        await client.text_document_references_async(
            types.ReferenceParams(
                **at, context=types.ReferenceContext(include_declaration=True)
            )
        )
    assert "PC-only plugin" in str(excinfo.value)
