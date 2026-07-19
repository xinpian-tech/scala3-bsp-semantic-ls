"""Watched-files dynamic registration over real stdio.

A client advertising `workspace.didChangeWatchedFiles.dynamicRegistration`
receives the server's fire-and-forget `client/registerCapability` request with
exactly the three watcher globs (pytest-lsp's default client has no handler for
server-to-client registration requests, so `conftest.make_watching_client`
registers a recording one — see conftest), and a watched `config.json` event
round-trips without error, leaving the session fully serviceable. The
semanticdb-event -> background-reingest flow is proven in the in-process wire
suite (`crates/ls-server/tests/watched_files_wire.rs`), which can write into a
temp targetroot; here the black-box focus is the protocol round trip through an
independent client.
"""

import asyncio

from lsprotocol import types

from conftest import DOCTOR, await_ready, execute

EXPECTED_GLOBS = [
    "**/*.semanticdb",
    "**/.scala3-bsp-semantic-ls/config.json",
    "**/.bsp/*.json",
]


async def test_registration_arrives_with_the_three_watcher_globs(client_watching):
    await asyncio.wait_for(client_watching.registrations_received.wait(), 30)

    registrations = client_watching.registrations
    assert len(registrations) == 1, registrations
    registration = registrations[0]
    assert registration.method == "workspace/didChangeWatchedFiles"
    assert registration.id == "workspace/didChangeWatchedFiles"
    watchers = registration.register_options["watchers"]
    assert [w["globPattern"] for w in watchers] == EXPECTED_GLOBS


async def test_a_watched_config_event_round_trips_without_error(client_watching):
    await asyncio.wait_for(client_watching.registrations_received.wait(), 30)
    await await_ready(client_watching)

    config = client_watching.workspace / ".scala3-bsp-semantic-ls" / "config.json"
    client_watching.workspace_did_change_watched_files(
        types.DidChangeWatchedFilesParams(
            changes=[
                types.FileEvent(
                    uri=config.as_uri(), type=types.FileChangeType.Created
                )
            ]
        )
    )
    # The notification has no reply; prove acceptance by the session staying
    # fully serviceable behind it (the loop dispatches strictly in order).
    report = await execute(client_watching, DOCTOR)
    assert "state: ready" in report

    symbols = await client_watching.workspace_symbol_async(
        types.WorkspaceSymbolParams(query="Core")
    )
    assert any(s.name == "Core" for s in symbols)
