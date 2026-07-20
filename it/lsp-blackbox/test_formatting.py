"""`textDocument/formatting` over real stdio: the scalafmt-CLI handler.

The round trip runs against a REAL scalafmt binary (the dev shell / flake
check provides one on PATH; `LS_SCALAFMT` overrides) and writes the workspace
`.scalafmt.conf` pinning exactly that binary's version — scalafmt refuses,
and under the server's offline stance cannot download, any other. The typed
errors (not-open, no-config) fire before binary resolution, so they run even
without scalafmt installed. The per-test workspace factory keeps the conf
OUT of every other suite's workspace: the missing-config error is itself a
test case here.
"""

import os
import shutil
import subprocess

import pytest
from lsprotocol import types

from conftest import await_ready


def scalafmt_binary() -> str | None:
    return os.environ.get("LS_SCALAFMT") or shutil.which("scalafmt")


def scalafmt_version(binary: str) -> str:
    out = subprocess.run(
        [binary, "--version"], capture_output=True, text=True, check=True
    )
    # `scalafmt 3.9.8` on stdout (JVM noise goes to stderr).
    return out.stdout.split()[-1]


# Line 0 is already formatted (a minimal diff must leave it untouched);
# lines 1-2 need edits.
MISFORMATTED = "object Fmt {\n  def  f( x:Int ) : Int   =x+1\n  val   y=2\n}\n"


def open_doc(client, uri: str, text: str):
    client.text_document_did_open(
        types.DidOpenTextDocumentParams(
            text_document=types.TextDocumentItem(
                uri=uri, language_id="scala", version=1, text=text
            )
        )
    )


async def request_formatting(client, uri: str):
    return await client.text_document_formatting_async(
        types.DocumentFormattingParams(
            text_document=types.TextDocumentIdentifier(uri=uri),
            options=types.FormattingOptions(tab_size=2, insert_spaces=True),
        )
    )


def apply_edits(text: str, edits) -> str:
    """Bottom-up application of original-addressed edits (ASCII fixture, so
    UTF-16 characters == bytes within a line)."""

    def offset(position: types.Position) -> int:
        lines = text.splitlines(keepends=True)
        return sum(len(line) for line in lines[: position.line]) + position.character

    for edit in sorted(edits, key=lambda e: offset(e.range.start), reverse=True):
        start, end = offset(edit.range.start), offset(edit.range.end)
        text = text[:start] + edit.new_text + text[end:]
    return text


async def test_formatting_round_trips_minimal_edits_and_is_idempotent(client):
    binary = scalafmt_binary()
    if binary is None:
        pytest.skip("no scalafmt binary via LS_SCALAFMT or PATH")
    await await_ready(client)
    version = scalafmt_version(binary)
    (client.workspace / ".scalafmt.conf").write_text(
        f'version = "{version}"\nrunner.dialect = scala3\n'
    )
    uri = (client.workspace / "Fmt.scala").as_uri()
    open_doc(client, uri, MISFORMATTED)

    edits = await request_formatting(client, uri)
    assert edits, "the misformatted buffer must yield edits"
    # Minimal, not whole-file: the already-formatted line 0 stays untouched.
    assert all(edit.range.start.line >= 1 for edit in edits), edits

    applied = apply_edits(MISFORMATTED, edits)
    assert "def f(x: Int): Int = x + 1" in applied
    assert "val y = 2" in applied

    # Idempotence: the applied text formats to the empty edit list (the wire
    # answer is `[]`; lsprotocol surfaces the empty sequence).
    client.text_document_did_change(
        types.DidChangeTextDocumentParams(
            text_document=types.VersionedTextDocumentIdentifier(uri=uri, version=2),
            content_changes=[
                types.TextDocumentContentChangeWholeDocument(text=applied)
            ],
        )
    )
    second = await request_formatting(client, uri)
    assert not second, second


async def test_formatting_a_not_open_file_is_a_typed_error(client):
    """Formatting serves the OPEN buffer only: a file the document store does
    not hold is a typed error, never a silent disk-file format."""
    await await_ready(client)
    uri = (client.workspace / "NeverOpened.scala").as_uri()
    with pytest.raises(Exception) as excinfo:
        await request_formatting(client, uri)
    assert "is not open" in str(excinfo.value)


async def test_formatting_without_a_scalafmt_conf_is_a_typed_error(client):
    """No root `.scalafmt.conf` → the typed no-config error (scalafmt requires
    a pinned version). Fires before binary resolution, so this runs without
    scalafmt installed — and pins that the shared workspace factory ships no
    conf by default."""
    await await_ready(client)
    assert not (client.workspace / ".scalafmt.conf").exists()
    uri = (client.workspace / "Open.scala").as_uri()
    open_doc(client, uri, "object   Open\n")
    with pytest.raises(Exception) as excinfo:
        await request_formatting(client, uri)
    assert "no .scalafmt.conf in the workspace (scalafmt requires a pinned version)" in str(
        excinfo.value
    )
