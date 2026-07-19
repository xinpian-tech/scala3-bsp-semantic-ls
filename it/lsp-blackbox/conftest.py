"""Shared fixtures for the black-box pytest-lsp suite.

Each client fixture spawns the REAL `ls-server` binary (env `LS_SERVER_BIN`,
falling back to `target/debug/ls-server`) as a subprocess and talks to it over
real stdio — exactly like an editor. The workspace handed to `initialize` is a
fresh temp dir carrying a `.bsp/fake-bsp.json` that points at `fake_bsp.py`, so
the server's own BSP discovery spawns the scriptable fake build server, which
advertises the committed `ls-engine` SemanticDB fixture corpus.

Three sessions: `client` (plain, every compile succeeds), `client_diags`
(first compile publishes one error on Core.scala, second publishes a clean
reset), `client_failing_compile` (the next compile fails with statusCode 2).
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path

import pytest
import pytest_lsp
from lsprotocol import types
from pytest_lsp import ClientServerConfig, LanguageClient

TESTS_DIR = Path(__file__).resolve().parent
REPO_ROOT = TESTS_DIR.parents[1]
FIXTURES_ROOT = REPO_ROOT / "crates" / "ls-engine" / "tests" / "fixtures"
SOURCES_ROOT = FIXTURES_ROOT / "sources"
FAKE_BSP = TESTS_DIR / "fake_bsp.py"

SERVER_BIN = os.environ.get(
    "LS_SERVER_BIN", str(REPO_ROOT / "target" / "debug" / "ls-server")
)

DOCTOR = "scala3SemanticLs.doctor"
COMPILE = "scala3SemanticLs.compile"
REINDEX = "scala3SemanticLs.reindex"
PC_PLUGIN_STATUS = "scala3SemanticLs.pcPluginStatus"

CORE = "a/src/pkga/Core.scala"
ITEM = "a/src/pkga/Item.scala"

CORE_URI_TPL = "${SOURCES_URI}/a/src/pkga/Core.scala"


def pytest_collection_modifyitems(config, items):
    if not Path(SERVER_BIN).exists():
        skip = pytest.mark.skip(
            reason=f"ls-server binary not found at {SERVER_BIN}; "
            "run `cargo build -p ls-server` or set LS_SERVER_BIN"
        )
        for item in items:
            item.add_marker(skip)


# --- workspace assembly -------------------------------------------------------


def bsp_error(message: str, code: str) -> dict:
    return {
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 4},
        },
        "severity": 1,
        "code": code,
        "source": "sc",
        "message": message,
    }


DIAG_SCRIPT = {
    "compiles": [
        {
            "status": 1,
            "diagnostics": [
                {
                    "textDocument": {"uri": CORE_URI_TPL},
                    "buildTarget": {"uri": "bsp://workspace/fixture-a"},
                    "reset": True,
                    "diagnostics": [bsp_error("value unused", "unused-value")],
                }
            ],
        },
        {
            "status": 1,
            "diagnostics": [
                {
                    "textDocument": {"uri": CORE_URI_TPL},
                    "buildTarget": {"uri": "bsp://workspace/fixture-a"},
                    "reset": True,
                    "diagnostics": [],
                }
            ],
        },
    ]
}

FAILING_COMPILE_SCRIPT = {"compiles": [{"status": 2, "diagnostics": []}]}


def make_workspace(script: dict | None) -> Path:
    """A fresh workspace: the nosdb source plus the fake-BSP connection file."""
    ws = Path(tempfile.mkdtemp(prefix="lsp-blackbox-"))
    nosdb = ws / "nosdb" / "NoSdb.scala"
    nosdb.parent.mkdir(parents=True)
    nosdb.write_text("class NoSdb\n")

    argv = [
        sys.executable,
        str(FAKE_BSP),
        "--fixtures-root",
        str(FIXTURES_ROOT),
        "--workspace",
        str(ws),
    ]
    if script is not None:
        script_path = ws / "fake-bsp-script.json"
        script_path.write_text(json.dumps(script))
        argv += ["--script", str(script_path)]

    bsp_dir = ws / ".bsp"
    bsp_dir.mkdir()
    (bsp_dir / "fake-bsp.json").write_text(
        json.dumps(
            {
                "name": "fake-bsp",
                "argv": argv,
                "version": "0.0.1",
                "bspVersion": "2.1.0",
                "languages": ["scala"],
            }
        )
    )
    return ws


# --- session helpers ----------------------------------------------------------


async def _start(lsp_client: LanguageClient, script: dict | None = None) -> Path:
    ws = make_workspace(script)
    lsp_client.init_result = await lsp_client.initialize_session(
        types.InitializeParams(
            process_id=None,
            root_uri=ws.as_uri(),
            capabilities=types.ClientCapabilities(),
        )
    )
    lsp_client.workspace = ws
    return ws


async def _stop(lsp_client: LanguageClient, ws: Path):
    await lsp_client.shutdown_session()
    shutil.rmtree(ws, ignore_errors=True)


async def execute(client: LanguageClient, command: str):
    return await client.workspace_execute_command_async(
        types.ExecuteCommandParams(command=command, arguments=[])
    )


async def await_ready(client: LanguageClient, timeout: float = 60.0) -> str:
    """Poll the doctor until the bootstrap reaches Ready; the fixture corpus is
    pre-compiled so ready arrives as soon as the fake-BSP model is ingested."""
    deadline = time.monotonic() + timeout
    while True:
        report = await execute(client, DOCTOR)
        if "state: ready" in report:
            return report
        assert time.monotonic() < deadline, f"never reached ready:\n{report}"
        await asyncio.sleep(0.1)


async def await_diagnostics(
    client: LanguageClient, uri_suffix: str, pred, timeout: float = 30.0
):
    """Wait until the captured publishDiagnostics for a URI satisfy `pred`."""
    deadline = time.monotonic() + timeout
    while True:
        for uri, diags in client.diagnostics.items():
            if uri.endswith(uri_suffix) and pred(diags):
                return uri, diags
        assert (
            time.monotonic() < deadline
        ), f"timeout awaiting diagnostics for {uri_suffix}: {client.diagnostics}"
        await asyncio.sleep(0.1)


def source_uri(rel: str) -> str:
    return (SOURCES_ROOT / rel).as_uri()


def source_text(rel: str) -> str:
    return (SOURCES_ROOT / rel).read_text()


def position_of(rel: str, token: str, nth: int = 0) -> types.Position:
    """The 0-based position of the nth occurrence of `token` in a fixture source."""
    seen = 0
    for line_no, line in enumerate(source_text(rel).splitlines()):
        start = 0
        while (col := line.find(token, start)) != -1:
            if seen == nth:
                return types.Position(line=line_no, character=col)
            seen += 1
            start = col + len(token)
    raise AssertionError(f"token {token!r} occurrence {nth} not found in {rel}")


def doc_position(rel: str, token: str, nth: int = 0) -> dict:
    return {
        "text_document": types.TextDocumentIdentifier(uri=source_uri(rel)),
        "position": position_of(rel, token, nth),
    }


# --- client fixtures ----------------------------------------------------------


@pytest_lsp.fixture(config=ClientServerConfig(server_command=[SERVER_BIN]))
async def client(lsp_client: LanguageClient):
    ws = await _start(lsp_client)
    yield
    await _stop(lsp_client, ws)


@pytest_lsp.fixture(config=ClientServerConfig(server_command=[SERVER_BIN]))
async def client_diags(lsp_client: LanguageClient):
    ws = await _start(lsp_client, DIAG_SCRIPT)
    yield
    await _stop(lsp_client, ws)


@pytest_lsp.fixture(config=ClientServerConfig(server_command=[SERVER_BIN]))
async def client_failing_compile(lsp_client: LanguageClient):
    ws = await _start(lsp_client, FAILING_COMPILE_SCRIPT)
    yield
    await _stop(lsp_client, ws)
