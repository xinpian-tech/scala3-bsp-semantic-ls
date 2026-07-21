# PC Plugin SPI

> Normative contract for the Presentation Compiler (PC) worker and its plugin system.
> Derived from `plan.md` sections 1.4, 4.3, 14, and 23. Implemented by the `pc` Mill
> module (`modules/ls-pc`). See `docs/architecture.md` for how the PC worker fits into
> the overall system.

## 1. PC contract (plan 4.3)

PC **is for**:

```text
completion
completionItem/resolve
hover
signature help
definition / typeDefinition
dirty buffer symbol-at-cursor overlay
prepareRename
PC-only plugin effects
PC diagnostics, optional and secondary
```

PC is **never for**:

```text
persistent indexing
global references truth
cross-file rename truth
index store writes (segments, manifest, workspace-state)
```

A crashing user plugin must never corrupt the main index or take down the main LS.
The presentation compiler runs in the embedded in-process JVM island: a plugin
crash is contained at the hook boundary (the plugin is disabled with a recorded
reason and the request completes as identity), and a wedged compiler is recovered
by the watchdog's escalation ladder (cancel → instance restart → a fresh loaned
dispatch generation with targets/buffers replayed). A JVM-fatal fault ends the
process, which the editor restarts against the crash-safe on-disk store — the
island never writes the persistent index, so no fault can corrupt it.

Plugin boundary (plan 1.4): this project does **not** manage the SemanticDB compiler
plugin — that belongs to the build tool / BSP server / scalac configuration and the
user must manage it in the real build. This project only manages **PC plugins**,
which run inside the PC island, affect only PC request results, and must not write
the index store and must not change workspace-wide semantic truth.

## 2. PC compiler plugins (plan 14.2)

Users may configure PC-only Scala 3 **compiler** plugins using standard scalac
plugin options:

```text
-Xplugin:/path/to/plugin.jar
-P:plugin:key:value
```

Scope rules:

- These options are injected **only** into the PC worker's compiler instances. They
  never reach the build server and never affect SemanticDB generation.
- Purpose: simulate the editing-time effect of macros / frameworks / compiler
  plugins on PC; insert compiler passes into PC; repair PC typecheck / completion /
  hover experience for plugin-heavy codebases.

Operational requirements (plan Phase 9):

- **Self-test**: on load, each configured compiler plugin is exercised against a
  trivial compilation unit inside the PC worker before it is enabled for user
  requests.
- **Fail policy**: a plugin that fails to load, fails its self-test, or throws during
  a request is disabled (not retried in a loop); the failure is recorded and surfaced
  through the Doctor report (`PC Plugins: compiler plugins loaded / self-test results /
  disabled plugins`). Plugin failure degrades PC features; it never degrades index
  correctness.

### 2.1 Configuration file and the `pc-plugin-load` proof

Compiler plugins are configured per workspace in
`<workspaceRoot>/.scala3-bsp-semantic-ls/pc-plugins.json` under `compilerPlugins`:

```json
{
  "compilerPlugins": [
    { "jars": ["/abs/path/to/plugin.jar"], "options": ["myPlugin:key:value"] }
  ]
}
```

Each `jars` entry becomes a `-Xplugin:<jar>` option and each `options` entry a
`-P:<option>` on the island's compiler instances (the workspace `pc-plugins.json`
is read by the island host at target registration).

The mechanism is proven end-to-end by a GENERIC test fixture this repo owns: the
`pcNavTestPlugin` Mill module (`modules/ls-pc-navtestplugin`, built standalone as
`.#pc-navtest-plugin-jar`; never shipped in the package). It is a Scala 3
`StandardPlugin` that runs after `typer` in the presentation compiler and rewrites
the `Inlined.call` of a `scala.Dynamic` field access (`io.a`) on its own marker
trait (`lstest.navfixture.NavProbe[T]`) to a typed reference to the resolved field
symbol — inert on every other shape. The live test
`crates/ls-jvm/tests/live_pcplugin.rs` (flake check `pc-plugin-load`) loads that
jar into the embedded island through a workspace `pc-plugins.json` and observes
the steered go-to (and the non-marker negative control) over the real vtable.

Real-world navigation plugins ride the same path. A project whose BUILD already
carries its tooling plugin — as zaozi does with its in-build
`zaozi-compiler-plugin`, whose interactive navigation phase reaches the island
via the build's `-Xplugin` scalacOptions over `buildTarget/scalacOptions` —
needs no `pc-plugins.json` at all; the config file is for plugins that are not
part of the build.

> **Doctor-status caveat.** The manager validates a configured compiler plugin by jar
> existence only, so the Doctor line `compiler plugins loaded: N of M` reports how many
> configured jars exist — **not** that the presentation compiler actually loaded the
> plugin class or ran its phase. To confirm a compiler plugin is working, exercise the
> PC feature it affects (e.g. the steered go-to the `pc-plugin-load` check drives), not
> the doctor status.

## 3. PC service plugin SPI (plan 14.3)

The project defines a stable service SPI. The trait, exactly as specified:

```scala
trait PcServicePlugin:
  def id: String
  def initialize(ctx: PcPluginInitContext): Unit = ()
  def patchOptions(ctx: PcTargetContext, options: Vector[String]): Vector[String] = options
  def patchSourcePath(ctx: PcTargetContext, sourcePath: Vector[Path]): Vector[Path] = sourcePath
  def syntheticSources(ctx: PcTargetContext): Vector[VirtualSource] = Vector.empty
  def beforeRequest(req: PcRequest): PcRequest = req
  def afterCompletion(req: PcRequest, result: CompletionList): CompletionList = result
  def afterHover(req: PcRequest, result: Option[Hover]): Option[Hover] = result
  def afterDefinition(req: PcRequest, result: DefinitionResult): DefinitionResult = result
  def filterPcDiagnostics(req: PcRequest, diagnostics: Vector[Diagnostic]): Vector[Diagnostic] = diagnostics
```

Every hook has a no-op default, so a plugin overrides only what it needs.

Per-hook contract:

| Hook | When it runs | Contract |
|---|---|---|
| `id` | always | Stable, unique plugin identifier. Used for configuration, doctor reporting, and disabling. |
| `initialize(ctx)` | once, at plugin load in the PC worker | One-time setup. Throwing here fails the plugin's self-test and disables it. |
| `patchOptions(ctx, options)` | when a PC compiler instance is (re)created for a target | May add/remove PC compiler options (including `-Xplugin`/`-P:` options) for that target. Affects the PC compiler only, never the build. |
| `patchSourcePath(ctx, sourcePath)` | when a PC compiler instance is (re)created for a target | May extend/modify the source path visible to PC for that target. |
| `syntheticSources(ctx)` | when a PC compiler instance is (re)created, and on plugin-driven invalidation | Contributes virtual sources (`VirtualSource`) visible only to PC. Materialized under `.scala3-bsp-semantic-ls/pc/generated-sources/`. Symbols defined only here are **PC-only symbols** (section 5). |
| `beforeRequest(req)` | before each PC request | May rewrite the request (e.g. adjust position, inject context). Must be pure with respect to persistent state. |
| `afterCompletion(req, result)` | after each completion request | May filter, reorder, or augment completion items. |
| `afterHover(req, result)` | after each hover request | May replace or enrich the hover, or suppress it with `None`. |
| `afterDefinition(req, result)` | after each definition request | May redirect definitions, including into synthetic sources; such results are marked as plugin-provided. |
| `filterPcDiagnostics(req, diagnostics)` | after PC produces diagnostics | May drop or rewrite **PC** diagnostics only. Build diagnostics (from BSP) are the primary diagnostics and are untouchable. |

Hook context/carrier types (`PcPluginInitContext`, `PcTargetContext`, `VirtualSource`,
`PcRequest`, `DefinitionResult`) are defined in `modules/ls-pc`; `CompletionList`,
`Hover`, `Diagnostic` are LSP model types. The same fail policy as compiler plugins
applies: a hook that throws disables the plugin for subsequent requests and reports it
via Doctor.

## 4. Permission matrix (plan 14.4)

| Capability | Can a PC plugin affect it? | Notes |
|---|---:|---|
| completion | yes | editing-time feature |
| hover | yes | may enrich explanations |
| signature help | yes | may improve DSL/macro experience |
| definition | yes, but source-marked | may jump into synthetic sources |
| PC diagnostics | yes | build diagnostics remain the primary diagnostics |
| dirty buffer references overlay | limited | never written to the persistent index |
| workspace symbol | **no** | comes only from the SemanticDB index |
| whole-repo references | **no** | comes only from SemanticDB/postings |
| cross-file rename | **no** | PC contributes only `prepareRename` |
| index store writes | **forbidden** | persistent index writes come only from scalac SemanticDB |

## 5. PC-only symbols (plan 14.5)

A symbol that exists only in plugin-provided synthetic sources or the dirty-buffer
overlay — i.e. not present in fresh scalac-generated SemanticDB — is a **PC-only
symbol**. Rules:

```text
completion / hover / definition may work.
workspace references are not promised.
cross-file rename is rejected.
```

The rejection is mandatory and uses this exact message (encoded as
`ls.index.LsError.PcOnlySymbol` in `modules/ls-index-model`, and as unsafe-reason bit
`ls.index.UnsafeReason.PcOnly` in rename profiles):

```text
This symbol is provided by a PC-only plugin and is not present in fresh SemanticDB.
Workspace-wide references and cross-file rename are unavailable for this symbol.
```

PC-only symbols surfaced in workspace-symbol dirty-buffer overlays must be labeled as
PC-only and are never written to the persistent index store.

## 6. Boundary statements (plan 23)

These are the load-bearing boundaries; any plugin-system change must preserve them:

```text
SemanticDB plugin belongs to build/scalac, not this LS.
PC plugin belongs to this LS, but cannot write persistent index.
```

And from the system-wide principles:

```text
Scala 3 PC provides interactive editing.
PC plugins improve PC only.
scalac SemanticDB provides semantic facts.
```

Any PC plugin capability whose effect would survive the PC worker process — a file in
the index store — a postings segment, the manifest, workspace state, a
workspace-wide answer — is out of
contract and must be rejected in code review.
