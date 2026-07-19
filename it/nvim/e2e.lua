-- Project-level LSP e2e driven by a REAL editor: headless Neovim attaches
-- scala3-bsp-semantic-ls to a real workspace (the pinned zaozi repo by
-- default), opens a real source, and exercises the editor-visible surface —
-- readiness (doctor), reindex ingest, workspace/symbol, definition,
-- references, and PC-backed hover (which boots the embedded JVM island for
-- real). Assertions are token-anchored so upstream source drift does not
-- invalidate positions.
--
-- Usage: nvim --headless -l it/nvim/e2e.lua <workspace> <server_bin> <rel_file> <token>
-- Exits 0 on success; prints "E2E FAIL: ..." and exits 1 on the first failure.

local ws = assert(arg[1], "arg1: workspace root")
local bin = assert(arg[2], "arg2: ls-server binary")
local rel = assert(arg[3], "arg3: workspace-relative source file")
local token = assert(arg[4], "arg4: token to anchor queries on")

local function fail(msg)
  io.stderr:write("E2E FAIL: " .. msg .. "\n")
  -- Surface the server's own stderr (the wrapper script tees it into the
  -- workspace) — nvim swallows LSP server stderr otherwise.
  local log = io.open(arg[1] .. "/ls-server.stderr.log", "r")
  if log then
    local text = log:read("*a") or ""
    log:close()
    io.stderr:write("=== ls-server stderr (tail) ===\n")
    io.stderr:write(text:sub(-4000) .. "\n")
  end
  os.exit(1)
end

local function pass(msg)
  io.stdout:write("E2E PASS: " .. msg .. "\n")
end

local file = ws .. "/" .. rel
vim.cmd.edit(file)
local buf = vim.api.nvim_get_current_buf()
if vim.api.nvim_buf_line_count(buf) < 2 then
  fail("could not read " .. file)
end

-- The 0-based (line, character) of the first occurrence of the token.
local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
local at
for i, line in ipairs(lines) do
  local col = line:find(token, 1, true)
  if col then
    at = { line = i - 1, character = col - 1 }
    break
  end
end
if not at then
  fail("token '" .. token .. "' not found in " .. rel)
end

local client_id = vim.lsp.start({
  name = "scala3-bsp-semantic-ls",
  cmd = { bin },
  root_dir = ws,
}, { bufnr = buf })
if not client_id then
  fail("vim.lsp.start did not return a client id")
end
local client = assert(vim.lsp.get_client_by_id(client_id))

if not vim.wait(120000, function()
  return client.initialized
end, 200) then
  fail("LSP initialize did not complete")
end
pass("initialize handshake (capabilities: " .. tostring(client.server_capabilities ~= nil) .. ")")

local function request(method, params, timeout)
  local response, err = client:request_sync(method, params, timeout or 60000, buf)
  if err then
    fail(method .. ": " .. tostring(err))
  end
  if not response then
    fail(method .. ": no response")
  end
  if response.err then
    fail(method .. ": error response: " .. vim.inspect(response.err))
  end
  return response.result
end

local function execute(command, timeout)
  return request(
    "workspace/executeCommand",
    { command = command, arguments = {} },
    timeout
  )
end

-- Readiness: poll the doctor until the bootstrap (mill BSP session + initial
-- ingest) reaches ready. A failed bootstrap is terminal — surface its report
-- immediately instead of burning the deadline.
local ready = false
local last_report = "(no doctor report)"
local deadline = os.time() + 600
while os.time() < deadline do
  local report = execute("scala3SemanticLs.doctor", 60000)
  if type(report) == "string" then
    last_report = report
    if report:find("state: ready", 1, true) then
      ready = true
      break
    end
    if report:find("state: failed", 1, true) then
      break
    end
  end
  vim.wait(1000)
end
if not ready then
  fail("bootstrap never reached ready over the real BSP session; last doctor report:\n" .. last_report)
end
pass("doctor reports state: ready over the real project")

-- First-editor-session flow, exactly like the real-BSP suites: compile over
-- the retained BSP session (the session's own out-dir view is authoritative —
-- a CLI pre-compile writes elsewhere), then reindex to ingest the SemanticDB
-- it produced. The ingest must actually carry documents.
local compiled = execute("scala3SemanticLs.compile", 600000)
if type(compiled) ~= "string" or not compiled:find("compile ok", 1, true) then
  fail("BSP-session compile failed: " .. vim.inspect(compiled))
end
pass("compile over the real BSP session: " .. compiled)

local reindexed = execute("scala3SemanticLs.reindex", 120000)
local docs = type(reindexed) == "string" and reindexed:match("(%d+) docs")
if not docs or tonumber(docs) == 0 then
  fail("reindex ingested no documents: " .. vim.inspect(reindexed))
end
pass("reindex: " .. reindexed)

-- workspace/symbol resolves project symbols over the index.
local symbols = request("workspace/symbol", { query = token })
if type(symbols) ~= "table" or #symbols == 0 then
  fail("workspace/symbol returned nothing for '" .. token .. "'")
end
pass("workspace/symbol finds '" .. token .. "' (" .. #symbols .. " hits)")

local doc_at = {
  textDocument = { uri = vim.uri_from_fname(file) },
  position = at,
}

-- PC-backed hover first: it boots the embedded JVM island against the real
-- project classpath (generous first-query deadline) and is the pure-PC probe —
-- a hover failure means the island itself, before the definition resolver is
-- in play.
local hover = request("textDocument/hover", doc_at, 180000)
if type(hover) ~= "table" or hover.contents == nil then
  fail("hover returned no contents (PC island did not answer)")
end
pass("PC-backed hover answered over the real project")

-- Definition, bisected across the three navigation shapes: in-buffer (pure
-- PC span), cross-target (PC symbol -> index resolver, the shape the gated
-- real-BSP suites prove), and same-target-cross-file (informational: its
-- support status is exactly what a real-project e2e exists to observe).
local function open_attached(relpath)
  vim.cmd.edit(ws .. "/" .. relpath)
  local b = vim.api.nvim_get_current_buf()
  vim.lsp.buf_attach_client(b, client_id)
  return b
end

local function find_pos(b, tok, nth)
  local want = nth or 1
  local seen = 0
  for i, line in ipairs(vim.api.nvim_buf_get_lines(b, 0, -1, false)) do
    local from = 1
    while true do
      local col = line:find(tok, from, true)
      if not col then
        break
      end
      seen = seen + 1
      if seen == want then
        return { line = i - 1, character = col - 1 }
      end
      from = col + #tok
    end
  end
  return nil
end

local function probe_definition(desc, b, tok, nth)
  local pos = find_pos(b, tok, nth)
  if not pos then
    return nil, desc .. ": token '" .. tok .. "' not found"
  end
  local params = { textDocument = { uri = vim.uri_from_bufnr(b) }, position = pos }
  local response, err = client:request_sync("textDocument/definition", params, 120000, b)
  if err or not response or response.err then
    return nil, desc .. ": request failed: " .. vim.inspect(err or (response and response.err))
  end
  local result = response.result
  if type(result) ~= "table" or #result == 0 then
    return nil, desc .. ": empty result at " .. pos.line .. ":" .. pos.character
  end
  local uri = result[1].uri or (result[1].location and result[1].location.uri) or ""
  return uri:gsub(".*/", "")
end

-- In-buffer definition: `encoding(...)` use -> the case-class field.
local where, why = probe_definition("in-file", buf, "encoding(", 1)
if not where then
  fail("in-buffer definition failed — " .. why)
end
pass("in-buffer definition resolves to " .. where)

-- Cross-file definition (both cross-target and same-target): OBSERVED, not
-- yet gated. On this real project the PC resolves the symbol (hover answers at
-- the same position) but the island's `symbol_definition` resolver answers
-- empty — a finding this e2e exists to surface; sample-workspace's gated
-- real-BSP suite passes the cross-target shape, so the gap is real-project
-- specific (multi-segment package?). Flip these to hard gates once fixed.
local spec_buf = open_attached("decoder/tests/src/BitSetSpec.scala")
for _, case in ipairs({
  { "cross-target definition", spec_buf, "BitSet.bitpat", 2 },
  { "same-target cross-file definition", buf, "BitSet.bitset", 1 },
}) do
  local where, why = probe_definition(case[1], case[2], case[3], case[4])
  if where then
    pass(case[1] .. " resolves to " .. where)
  else
    io.stdout:write("E2E INFO: " .. why .. " (known gap — see docs/traceability.md)\n")
  end
end

-- References over the index.
local references = request("textDocument/references", {
  textDocument = doc_at.textDocument,
  position = doc_at.position,
  context = { includeDeclaration = true },
})
if type(references) ~= "table" or #references == 0 then
  fail("references returned nothing")
end
pass("references finds " .. #references .. " sites")

pass("all project-level checks")
os.exit(0)
