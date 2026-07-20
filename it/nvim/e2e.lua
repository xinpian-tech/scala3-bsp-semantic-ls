-- Project-level LSP e2e driven by a REAL editor: headless Neovim attaches
-- scala3-bsp-semantic-ls to a real workspace (the pinned zaozi repo by
-- default), opens a real source, and exercises the editor-visible surface —
-- readiness (doctor), reindex ingest, workspace/symbol, definition,
-- references, PC-backed hover (which boots the embedded JVM island for
-- real), the payload probes (foldingRange/selectionRange/inlayHint,
-- semanticTokens/full), the code-action assembly (an "Insert type
-- annotation" action with its inline edit at a real un-annotated val), and
-- the live-typing PC diagnostics flow (edit -> "scala3-pc (typing)" publish
-- -> revert -> clear). Assertions are token-anchored so upstream source
-- drift does not invalidate positions.
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

-- Watched-files probe setup. Neovim on Linux does NOT advertise
-- `workspace.didChangeWatchedFiles.dynamicRegistration` by default (0.10+
-- disables it because the Linux watch backends are too limited — see
-- runtime protocol.lua), so the server would send no registration; force the
-- capability on and wrap the `client/registerCapability` handler to record the
-- watched-files registration before delegating to Neovim's REAL default
-- handler (which parses the globs via vim.glob.to_lpeg and starts its
-- watchfunc) — so acceptance below is nvim's own, not a stub's. Event FIRING is
-- not asserted here: headless-Linux watch backends poll unreliably; the
-- event -> reingest flow is proven by the in-process wire suite.
local capabilities = vim.lsp.protocol.make_client_capabilities()
capabilities.workspace.didChangeWatchedFiles.dynamicRegistration = true
local watched_registrations = {}
local default_register_handler = vim.lsp.handlers["client/registerCapability"]

-- Record every publishDiagnostics (in arrival order) before delegating to
-- nvim's REAL default handler, so the live-typing diagnostics probe can await
-- the PC-tagged publish and its clear without polling editor-side state.
local recorded_publishes = {}
local default_publish_handler = vim.lsp.handlers["textDocument/publishDiagnostics"]

local client_id = vim.lsp.start({
  name = "scala3-bsp-semantic-ls",
  cmd = { bin },
  root_dir = ws,
  capabilities = capabilities,
  handlers = {
    ["client/registerCapability"] = function(err, params, ctx)
      for _, reg in ipairs((params and params.registrations) or {}) do
        if reg.method == "workspace/didChangeWatchedFiles" then
          table.insert(watched_registrations, reg)
        end
      end
      return default_register_handler(err, params, ctx)
    end,
    ["textDocument/publishDiagnostics"] = function(err, params, ctx)
      table.insert(recorded_publishes, params)
      if default_publish_handler then
        return default_publish_handler(err, params, ctx)
      end
    end,
  },
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

-- The server registers its watched-files globs right after `initialized`
-- (fire-and-forget client/registerCapability); nvim's default handler accepted
-- it above, so the registration must be observable with the three globs.
if not vim.wait(30000, function()
  return #watched_registrations > 0
end, 200) then
  fail("no workspace/didChangeWatchedFiles registration arrived after initialized")
end
local watchers = (watched_registrations[1].registerOptions or {}).watchers or {}
local globs = {}
for _, watcher in ipairs(watchers) do
  globs[watcher.globPattern] = true
end
for _, expected in ipairs({
  "**/*.semanticdb",
  "**/.scala3-bsp-semantic-ls/config.json",
  "**/.bsp/*.json",
}) do
  if not globs[expected] then
    fail("watched-files registration is missing glob '" .. expected .. "': " .. vim.inspect(watchers))
  end
end
pass("didChangeWatchedFiles registration accepted by nvim (3 globs)")

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

-- Payload-backed probes over the now-booted island (nvim's client opened the
-- buffer, so the PC mirror holds it and the withPcBuffer gate passes).

-- foldingRange: the parser-only provider must fold a real source file. Kind
-- facts: every range is well-formed (startLine <= endLine) and any kind is one
-- of the three LSP kind strings; a kind-less range is the plain code fold.
local folds = request("textDocument/foldingRange", { textDocument = doc_at.textDocument }, 120000)
if type(folds) ~= "table" or #folds == 0 then
  fail("foldingRange returned no ranges over the real buffer")
end
local fold_kinds = { comment = true, imports = true, region = true }
local kinded = 0
for _, fold in ipairs(folds) do
  if type(fold.startLine) ~= "number" or type(fold.endLine) ~= "number" or fold.endLine < fold.startLine then
    fail("foldingRange returned a malformed range: " .. vim.inspect(fold))
  end
  if fold.kind ~= nil then
    if not fold_kinds[fold.kind] then
      fail("foldingRange returned an unknown kind: " .. vim.inspect(fold))
    end
    kinded = kinded + 1
  end
end
pass("foldingRange finds " .. #folds .. " ranges (" .. kinded .. " kinded)")

-- selectionRange at the anchor: one linked chain whose innermost range
-- contains the cursor and whose parents widen (each parent contains its
-- child) — at least one parent level for a token nested in real code.
local sel = request("textDocument/selectionRange", {
  textDocument = doc_at.textDocument,
  positions = { at },
}, 120000)
if type(sel) ~= "table" or #sel ~= 1 then
  fail("selectionRange did not return exactly one chain: " .. vim.inspect(sel))
end
local function pos_le(a, b) -- a <= b in (line, character) order
  return a.line < b.line or (a.line == b.line and a.character <= b.character)
end
local node = sel[1]
if not (pos_le(node.range.start, at) and pos_le(at, node.range["end"])) then
  fail("selectionRange innermost range does not contain the anchor: " .. vim.inspect(node.range))
end
local depth = 0
while node.parent do
  local outer = node.parent
  if not (pos_le(outer.range.start, node.range.start) and pos_le(node.range["end"], outer.range["end"])) then
    fail("selectionRange parent does not contain its child: " .. vim.inspect(outer.range))
  end
  node = outer
  depth = depth + 1
end
if depth < 1 then
  fail("selectionRange chain has no parent level at the anchor")
end
pass("selectionRange chain widens over " .. (depth + 1) .. " levels at the anchor")

-- inlayHint: the request must round-trip cleanly over the whole file. Hints
-- may legitimately be empty depending on the server's default category flags
-- and the file's content — assert no error and the list shape only.
local hints = request("textDocument/inlayHint", {
  textDocument = doc_at.textDocument,
  range = {
    start = { line = 0, character = 0 },
    ["end"] = { line = vim.api.nvim_buf_line_count(buf), character = 0 },
  },
}, 120000)
if type(hints) ~= "table" then
  fail("inlayHint did not return a list: " .. vim.inspect(hints))
end
for _, hint in ipairs(hints) do
  if hint.position == nil or hint.label == nil then
    fail("inlayHint returned a malformed hint: " .. vim.inspect(hint))
  end
end
pass("inlayHint round-trips (" .. #hints .. " hints)")

-- semanticTokens/full over the real buffer: the island's whole-buffer node
-- walk, offset-converted and delta-encoded server-side. The stream must be
-- non-empty (a real source file names plenty of symbols) and well-formed
-- (five words per token); every token type/modifier index must be in legend
-- range, so the advertised legend and the emitted stream cannot drift apart.
local tokens = request("textDocument/semanticTokens/full", {
  textDocument = doc_at.textDocument,
}, 120000)
if type(tokens) ~= "table" or type(tokens.data) ~= "table" then
  fail("semanticTokens/full did not return a data array: " .. vim.inspect(tokens))
end
if #tokens.data == 0 then
  fail("semanticTokens/full returned an empty stream over a real buffer")
end
if #tokens.data % 5 ~= 0 then
  fail("semanticTokens/full data length " .. #tokens.data .. " is not a multiple of 5")
end
local legend = (client.server_capabilities.semanticTokensProvider or {}).legend or {}
local n_types = #(legend.tokenTypes or {})
local n_mods = #(legend.tokenModifiers or {})
if n_types ~= 23 or n_mods ~= 10 then
  fail("advertised legend is not the pinned 23/10 lists: " .. n_types .. "/" .. n_mods)
end
for i = 1, #tokens.data, 5 do
  local token_type = tokens.data[i + 3]
  local modifiers = tokens.data[i + 4]
  if token_type >= n_types then
    fail("token type index " .. token_type .. " outside the advertised legend")
  end
  if modifiers >= 2 ^ n_mods then
    fail("token modifier bitset " .. modifiers .. " outside the advertised legend")
  end
end
pass("semanticTokens/full streams " .. (#tokens.data / 5) .. " tokens within the legend")

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

-- Cross-file definition, both shapes, HARD gates: cross-target (tests -> main
-- module) and same-target cross-file. Under the mill layout every target
-- shares one `-sourceroot`, so these prove the doc-row target attribution in
-- `QueryOrchestrator::symbol_definition` (the shared-sourceroot regression).
local spec_buf = open_attached("decoder/tests/src/BitSetSpec.scala")
for _, case in ipairs({
  { "cross-target definition", spec_buf, "BitSet.bitpat", 2 },
  { "same-target cross-file definition", buf, "BitSet.bitset", 1 },
}) do
  local where, why = probe_definition(case[1], case[2], case[3], case[4])
  if not where then
    fail(why)
  end
  pass(case[1] .. " resolves to " .. where)
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

-- codeAction at a `val` with an inferable (un-annotated) type: the assembly
-- layer must offer an "Insert type annotation" action whose INLINE edit
-- (eager resolution — no executeCommand round trip) inserts a `": "` type
-- ascription. The action only has to arrive well-formed; applying it is not
-- required. The main buffer holds only destructuring vals, so the probe opens
-- a sibling real source with a plain `val name = ...`.
local ca_buf = open_attached("decoder/src/BitSet.scala")
local val_at
for i, line in ipairs(vim.api.nvim_buf_get_lines(ca_buf, 0, -1, false)) do
  local s = line:find("val [%w_]+ =")
  if s then
    val_at = { line = i - 1, character = s + 3 } -- 0-based start of the name
    break
  end
end
if not val_at then
  fail("no un-annotated `val name =` found in decoder/src/BitSet.scala for the codeAction probe")
end
local ca_response, ca_err = client:request_sync("textDocument/codeAction", {
  textDocument = { uri = vim.uri_from_bufnr(ca_buf) },
  range = { start = val_at, ["end"] = val_at },
  context = { diagnostics = {} },
}, 120000, ca_buf)
if ca_err or not ca_response or ca_response.err then
  fail("codeAction failed: " .. vim.inspect(ca_err or (ca_response and ca_response.err)))
end
local actions = ca_response.result
if type(actions) ~= "table" then
  fail("codeAction did not return a list: " .. vim.inspect(actions))
end
local insert_type
local titles = {}
for _, action in ipairs(actions) do
  table.insert(titles, action.title)
  if action.title == "Insert type annotation" then
    insert_type = action
  end
end
if not insert_type then
  fail(
    "no 'Insert type annotation' action at the val (line "
      .. val_at.line
      .. ", col "
      .. val_at.character
      .. "); offered: "
      .. vim.inspect(titles)
  )
end
if insert_type.kind ~= "refactor.rewrite" then
  fail("Insert type annotation has kind " .. tostring(insert_type.kind))
end
local has_ascription = false
for _, edits in pairs((insert_type.edit or {}).changes or {}) do
  for _, edit in ipairs(edits) do
    if edit.newText and edit.newText:find(": ", 1, true) then
      has_ascription = true
    end
  end
end
if not has_ascription then
  fail("the inline edit carries no ': ' ascription: " .. vim.inspect(insert_type.edit))
end
pass(
  "codeAction offers 'Insert type annotation' with an inline ': ' edit ("
    .. #actions
    .. " actions: "
    .. table.concat(titles, ", ")
    .. ")"
)

-- Live-typing (PC) diagnostics: edit the real buffer to introduce a type
-- error; nvim's incremental didChange arms the server's debounced
-- pc_diagnostics pull (the island is already booted by the probes above), and
-- a publishDiagnostics tagged "scala3-pc (typing)" must arrive for this
-- buffer. Reverting the edit must then clear the PC-tagged diagnostics.
local main_uri = vim.uri_from_bufnr(buf)

local function pc_tagged_count(params)
  local n = 0
  for _, d in ipairs(params.diagnostics or {}) do
    if d.source == "scala3-pc (typing)" then
      n = n + 1
    end
  end
  return n
end

-- The LATEST recorded publish for the buffer (publish replaces per URI).
local function last_publish_for(uri)
  for i = #recorded_publishes, 1, -1 do
    if recorded_publishes[i].uri == uri then
      return recorded_publishes[i]
    end
  end
  return nil
end

local probe_line = 'val __ls_e2e_probe: Int = ""'
local end_line = vim.api.nvim_buf_line_count(buf)
vim.api.nvim_buf_set_lines(buf, end_line, end_line, false, { probe_line })

if not vim.wait(120000, function()
  local p = last_publish_for(main_uri)
  return p ~= nil and pc_tagged_count(p) > 0
end, 500) then
  fail(
    "no PC-tagged typing diagnostic arrived after introducing a type error; last publish: "
      .. vim.inspect(last_publish_for(main_uri))
  )
end
local publish = last_publish_for(main_uri)
local tagged
for _, d in ipairs(publish.diagnostics) do
  if d.source == "scala3-pc (typing)" then
    tagged = d
  end
end
if tagged.range.start.line ~= end_line then
  fail(
    "the typing diagnostic does not point at the probe line "
      .. end_line
      .. ": "
      .. vim.inspect(tagged.range)
  )
end
pass('live-typing diagnostics: "' .. (tagged.message:gsub("\n.*", "")) .. '" tagged scala3-pc (typing)')

-- Revert the edit: the next debounced pull sees a buffer whose PC diagnostics
-- are gone, and the overlay publish clears the tag.
vim.api.nvim_buf_set_lines(buf, end_line, end_line + 1, false, {})
if not vim.wait(120000, function()
  local p = last_publish_for(main_uri)
  return p ~= nil and pc_tagged_count(p) == 0
end, 500) then
  fail(
    "the PC-tagged diagnostics never cleared after reverting; last publish: "
      .. vim.inspect(last_publish_for(main_uri))
  )
end
pass("live-typing diagnostics clear on revert")

pass("all project-level checks")
os.exit(0)
