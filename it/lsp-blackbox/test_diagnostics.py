"""Compile-diagnostics forwarding over real stdio: a scripted fake-BSP compile
publishes an error on Core.scala; the server routes it to the editor as an LSP
`textDocument/publishDiagnostics`; a second (clean-reset) compile clears it.

Plus the live-typing (PC) diagnostics NON-flow of a blackbox session: a
`didChange` arms the debounced `pc_diagnostics` pull, but the pull never boots
the embedded JVM — so with the island cold it is skipped entirely: no crash,
no publish, no boot. The positive PC-tagged publish flow is proven by the
JVM-free wire suite (`crates/ls-server/tests/pc_wire.rs`, fake PC behind a
canned diagnostic) and against the real island by the nvim e2e.
"""

import asyncio

from lsprotocol import types

from conftest import (
    COMPILE,
    CORE,
    PC_PLUGIN_STATUS,
    await_diagnostics,
    await_ready,
    execute,
    source_text,
    source_uri,
)


async def test_compile_diagnostics_publish_then_clear(client_diags):
    client = client_diags
    await await_ready(client)

    # First scripted compile: one error on Core.scala reaches the client.
    result = await execute(client, COMPILE)
    assert str(result).startswith("compile ok"), f"compile failed: {result}"
    uri, diags = await await_diagnostics(
        client, "a/src/pkga/Core.scala", lambda d: len(d) == 1
    )
    assert diags[0].message == "value unused"
    assert diags[0].severity == 1

    # Second scripted compile: the clean reset clears the published file.
    result = await execute(client, COMPILE)
    assert str(result).startswith("compile ok"), f"compile failed: {result}"
    await await_diagnostics(client, "a/src/pkga/Core.scala", lambda d: len(d) == 0)


async def test_typing_on_a_cold_island_publishes_no_pc_diagnostics(client):
    """Open a corpus buffer and make a ranged edit (a dirty buffer — the exact
    trigger of the debounced live-typing pull). The blackbox island never
    boots, so the pull is SKIPPED: the session must neither crash nor publish
    a "scala3-pc (typing)" diagnostic, and the island must stay cold."""
    await await_ready(client)
    uri = source_uri(CORE)
    client.text_document_did_open(
        types.DidOpenTextDocumentParams(
            text_document=types.TextDocumentItem(
                uri=uri,
                language_id="scala",
                version=1,
                text=source_text(CORE),
            )
        )
    )
    # A ranged didChange (incremental sync) that dirties the buffer.
    client.text_document_did_change(
        types.DidChangeTextDocumentParams(
            text_document=types.VersionedTextDocumentIdentifier(uri=uri, version=2),
            content_changes=[
                types.TextDocumentContentChangePartial(
                    range=types.Range(
                        start=types.Position(line=0, character=0),
                        end=types.Position(line=0, character=0),
                    ),
                    text="// typing\n",
                )
            ],
        )
    )
    # Give the 300ms debounce window (plus margin) time to elapse; the skipped
    # pull publishes nothing for the edited URI.
    await asyncio.sleep(1.5)
    pc_tagged = [
        d
        for published_uri, diags in client.diagnostics.items()
        if published_uri == uri
        for d in diags
        if d.source == "scala3-pc (typing)"
    ]
    assert pc_tagged == [], f"a cold island must not publish typing diagnostics: {pc_tagged}"
    # No crash: the server still answers, and the island is still cold (the
    # didChange-armed pull never booted the JVM).
    status = await execute(client, PC_PLUGIN_STATUS)
    assert "PC island not booted (cold)" in status
