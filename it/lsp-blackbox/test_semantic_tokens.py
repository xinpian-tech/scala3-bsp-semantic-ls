"""Semantic tokens over the real binary: the advertised legend is EXACTLY the
PC-vendored `scala.meta.internal.pc.SemanticTokens` lists (asserted through
lsprotocol's independent typed model, with the cross-language golden anchors),
and both methods answer their graceful cold fallbacks.

The methods are PC-backed, and a blackbox session never opens the documents it
queries, so the `withPcBuffer` gate answers null WITHOUT booting the embedded
JVM island. The positive, island-answered token streams are pinned elsewhere:
hermetically by the JVM-free wire suite `crates/ls-server/tests/pc_wire.rs`
over the fake PC, and against the real island by the editor e2e
`it/nvim/e2e.lua`.
"""

import pytest
from lsprotocol import types

from conftest import CORE, PC_PLUGIN_STATUS, await_ready, execute, source_uri

# The PC-vendored legend (scala3-presentation-compiler 3.8.4), pinned verbatim
# on every side of the boundary: `legend::TOKEN_TYPES`/`TOKEN_MODIFIERS` in
# crates/ls-server/src/pc_lsp.rs and the island-side munit pin in
# modules/ls-pc/test/src/ls/pc/SemanticTokensLegendSuite.scala.
TOKEN_TYPES = [
    "namespace",
    "type",
    "class",
    "enum",
    "interface",
    "struct",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "event",
    "function",
    "method",
    "macro",
    "keyword",
    "modifier",
    "comment",
    "string",
    "number",
    "regexp",
    "operator",
    "decorator",
]

TOKEN_MODIFIERS = [
    "declaration",
    "definition",
    "readonly",
    "static",
    "deprecated",
    "abstract",
    "async",
    "modification",
    "documentation",
    "defaultLibrary",
]


async def test_the_legend_is_the_pc_vendored_token_lists(client):
    provider = client.init_result.capabilities.semantic_tokens_provider
    assert provider is not None
    assert list(provider.legend.token_types) == TOKEN_TYPES
    assert list(provider.legend.token_modifiers) == TOKEN_MODIFIERS
    # The golden anchors shared with the Rust and island pins.
    assert provider.legend.token_types[13] == "method"
    assert provider.legend.token_modifiers[0] == "declaration"
    assert provider.full is True
    assert provider.range is True


async def test_full_on_an_unopened_buffer_is_null(client):
    """A semanticdb-owned URI with no open buffer takes the `withPcBuffer`
    fallback: null (`SemanticTokens | null`), never an empty stream that would
    wipe client-side highlighting — and never a JVM boot."""
    await await_ready(client)
    result = await client.text_document_semantic_tokens_full_async(
        types.SemanticTokensParams(
            text_document=types.TextDocumentIdentifier(uri=source_uri(CORE))
        )
    )
    assert result is None


async def test_range_on_an_unopened_buffer_is_null(client):
    await await_ready(client)
    result = await client.text_document_semantic_tokens_range_async(
        types.SemanticTokensRangeParams(
            text_document=types.TextDocumentIdentifier(uri=source_uri(CORE)),
            range=types.Range(
                start=types.Position(line=0, character=0),
                end=types.Position(line=5, character=0),
            ),
        )
    )
    assert result is None


async def test_full_on_a_no_semanticdb_source_is_a_typed_error(client):
    """semanticTokens follows the hover discipline: `requireSemanticdb` gates
    BEFORE the buffer fallback, so a source owned by a target compiled without
    SemanticDB is a hard error, not null."""
    await await_ready(client)
    nosdb_uri = (client.workspace / "nosdb" / "NoSdb.scala").as_uri()
    with pytest.raises(Exception) as excinfo:
        await client.text_document_semantic_tokens_full_async(
            types.SemanticTokensParams(
                text_document=types.TextDocumentIdentifier(uri=nosdb_uri)
            )
        )
    assert "has no SemanticDB output" in str(excinfo.value)


async def test_the_token_fallbacks_never_boot_the_island(client):
    """Drive both methods, then assert the island is STILL cold — the gate
    fallbacks answered without a PC query."""
    await await_ready(client)
    doc = types.TextDocumentIdentifier(uri=source_uri(CORE))
    await client.text_document_semantic_tokens_full_async(
        types.SemanticTokensParams(text_document=doc)
    )
    await client.text_document_semantic_tokens_range_async(
        types.SemanticTokensRangeParams(
            text_document=doc,
            range=types.Range(
                start=types.Position(line=0, character=0),
                end=types.Position(line=1, character=0),
            ),
        )
    )
    status = await execute(client, PC_PLUGIN_STATUS)
    assert "PC island not booted (cold)" in status
