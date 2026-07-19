#!/usr/bin/env python3
"""A scriptable fake BSP build server over stdio.

The Python twin of `FakeBuildServer` in `crates/ls-server/tests/fake_bsp_e2e.rs`,
for the black-box pytest-lsp suite: the server under test discovers this process
through a `.bsp/*.json` connection file and speaks real BSP to it over stdio
(Content-Length framed JSON-RPC), so BSP discovery, launch, the session
handshake, model load, compile, and diagnostics forwarding all run through
production code in the REAL `ls-server` binary.

It advertises the committed `ls-engine` SemanticDB fixture corpus: targets
`fixture-a`/`fixture-b`/`fixture-c` point `-semanticdb-target` at the committed
`out-*` targetroots, plus one `fixture-nosdb` target compiled WITHOUT
`-Xsemanticdb` (the hard-error case). Its reaction to `buildTarget/compile`
(status, published diagnostics, a `buildTarget/didChange` reload) is scriptable
through a JSON file passed as `--script`; with no script every compile succeeds.

Script schema (all fields optional):

    {
      "initialTargets": ["fixture-a", "fixture-b", "fixture-nosdb"],
      "compiles": [
        {
          "status": 1,
          "diagnostics": [ <build/publishDiagnostics params> ],
          "reloadToDefault": false
        }
      ]
    }

`compiles` entries are consumed one per `buildTarget/compile`; when the queue is
empty a compile is a plain success. Inside the script the placeholders
`${SOURCES_URI}` and `${WS_URI}` expand to the file URIs of the fixture sources
root and the workspace root. Stdlib only — the spawning server knows nothing of
the test venv.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import deque
from pathlib import Path

DEFAULT_TARGET_NAMES = ["fixture-a", "fixture-b", "fixture-c", "fixture-nosdb"]
TARGET_DEPS = {"fixture-b": ["fixture-a"]}


def read_message(stream):
    """Reads one Content-Length framed message; None on clean EOF."""
    content_length = None
    while True:
        line = stream.readline()
        if not line:
            return None
        text = line.decode("ascii", "replace").rstrip("\r\n")
        if not text:
            break
        key, _, value = text.partition(":")
        if key.strip().lower() == "content-length":
            content_length = int(value.strip())
    if content_length is None:
        raise ValueError("missing Content-Length header")
    body = stream.read(content_length)
    if len(body) != content_length:
        return None
    return json.loads(body)


def write_message(stream, msg):
    body = json.dumps(msg).encode("utf-8")
    stream.write(b"Content-Length: %d\r\n\r\n" % len(body))
    stream.write(body)
    stream.flush()


def target_id(name):
    return f"bsp://workspace/{name}"


def build_target(name):
    deps = TARGET_DEPS.get(name, [])
    return {
        "id": {"uri": target_id(name)},
        "displayName": name,
        "tags": [],
        "languageIds": ["scala"],
        "dependencies": [{"uri": target_id(d)} for d in deps],
        "capabilities": {"canCompile": True},
        "dataKind": "scala",
        "data": {
            "scalaOrganization": "org.scala-lang",
            "scalaVersion": "3.3.1",
            "scalaBinaryVersion": "3",
            "platform": 1,
            "jars": [],
        },
    }


class FakeBuildServer:
    def __init__(self, fixtures_root: Path, workspace: Path, script: dict):
        self.fixtures_root = fixtures_root
        self.sources_root = fixtures_root / "sources"
        self.nosdb_source = workspace / "nosdb" / "NoSdb.scala"
        names = script.get("initialTargets") or DEFAULT_TARGET_NAMES
        self.targets = [build_target(n) for n in names]
        self.compiles = deque(script.get("compiles", []))

    # --- request handlers -----------------------------------------------------

    def initialize_result(self):
        return {
            "displayName": "fake-bsp-server",
            "version": "0.0.1",
            "bspVersion": "2.1.0",
            "capabilities": {"compileProvider": {"languageIds": ["scala"]}},
        }

    def source_item(self, name):
        def dir_item(rel):
            return {
                "uri": (self.sources_root / rel).as_uri(),
                "kind": 2,
                "generated": False,
            }

        if name == "fixture-a":
            sources = [dir_item("a"), dir_item("shared"), dir_item("dep")]
        elif name == "fixture-b":
            sources = [dir_item("b")]
        elif name == "fixture-c":
            sources = [dir_item("c")]
        elif name == "fixture-nosdb":
            sources = [
                {"uri": self.nosdb_source.as_uri(), "kind": 1, "generated": False}
            ]
        else:
            sources = []
        return {"target": {"uri": target_id(name)}, "sources": sources}

    def scalac_option_item(self, name):
        if name in ("fixture-a", "fixture-b", "fixture-c"):
            out = {"fixture-a": "out-a", "fixture-b": "out-b", "fixture-c": "out-c"}
            options = [
                "-Xsemanticdb",
                f"-semanticdb-target:{self.fixtures_root / out[name]}",
                "-sourceroot",
                str(self.sources_root),
            ]
        else:
            options = ["-deprecation"]
        return {
            "target": {"uri": target_id(name)},
            "options": options,
            "classpath": [],
            "classDirectory": (self.fixtures_root / "out" / name).as_uri(),
        }

    def requested_names(self, params):
        names = []
        for target in (params or {}).get("targets", []):
            uri = target.get("uri", "")
            names.append(uri.removeprefix("bsp://workspace/"))
        return names

    def compile(self, writer, params):
        script = (
            self.compiles.popleft()
            if self.compiles
            else {"status": 1, "diagnostics": []}
        )
        for diagnostic in script.get("diagnostics", []):
            notify(writer, "build/publishDiagnostics", diagnostic)
        if script.get("reloadToDefault"):
            self.targets = [build_target(n) for n in DEFAULT_TARGET_NAMES]
            notify(writer, "buildTarget/didChange", {"changes": []})
        return {"statusCode": script.get("status", 1)}

    # --- dispatch -------------------------------------------------------------

    def handle(self, msg, writer):
        """Returns False to stop serving (on build/exit)."""
        method = msg.get("method", "")
        msg_id = msg.get("id")
        params = msg.get("params") or {}
        if method == "build/initialize":
            reply(writer, msg_id, self.initialize_result())
        elif method == "build/initialized":
            pass
        elif method == "build/shutdown":
            reply(writer, msg_id, None)
        elif method == "build/exit":
            return False
        elif method == "workspace/buildTargets":
            reply(writer, msg_id, {"targets": self.targets})
        elif method == "buildTarget/sources":
            items = [self.source_item(n) for n in self.requested_names(params)]
            reply(writer, msg_id, {"items": items})
        elif method == "buildTarget/scalacOptions":
            items = [self.scalac_option_item(n) for n in self.requested_names(params)]
            reply(writer, msg_id, {"items": items})
        elif method == "buildTarget/compile":
            reply(writer, msg_id, self.compile(writer, params))
        elif msg_id is not None:
            error(writer, msg_id, -32601, f"method not found: {method}")
        return True


def reply(writer, msg_id, result):
    if msg_id is not None:
        write_message(writer, {"jsonrpc": "2.0", "id": msg_id, "result": result})


def error(writer, msg_id, code, message):
    write_message(
        writer,
        {"jsonrpc": "2.0", "id": msg_id, "error": {"code": code, "message": message}},
    )


def notify(writer, method, params):
    write_message(writer, {"jsonrpc": "2.0", "method": method, "params": params})


def load_script(path: Path | None, sources_root: Path, workspace: Path) -> dict:
    if path is None:
        return {}
    text = path.read_text()
    text = text.replace("${SOURCES_URI}", sources_root.as_uri())
    text = text.replace("${WS_URI}", workspace.as_uri())
    return json.loads(text)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures-root", required=True, type=Path)
    parser.add_argument("--workspace", required=True, type=Path)
    parser.add_argument("--script", type=Path, default=None)
    args = parser.parse_args()

    script = load_script(args.script, args.fixtures_root / "sources", args.workspace)
    server = FakeBuildServer(args.fixtures_root, args.workspace, script)

    reader = sys.stdin.buffer
    writer = sys.stdout.buffer
    while True:
        msg = read_message(reader)
        if msg is None or not server.handle(msg, writer):
            break


if __name__ == "__main__":
    main()
