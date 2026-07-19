"""Compile-diagnostics forwarding over real stdio: a scripted fake-BSP compile
publishes an error on Core.scala; the server routes it to the editor as an LSP
`textDocument/publishDiagnostics`; a second (clean-reset) compile clears it.
"""

from conftest import COMPILE, await_diagnostics, await_ready, execute


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
