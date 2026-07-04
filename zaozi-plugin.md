# zaozi-pcplugin — Presentation-Compiler Go-To & Hover for zaozi's `Dynamic` Bundle-Field Macro

## Goal Description

Add a new Mill build target `zaoziPcplugin`: a Scala 3 `StandardPlugin` compiler plugin that, loaded into this project's presentation compiler (PC) via the existing `pc-plugins.json` `compilerPlugins` mechanism, makes the PC resolve `textDocument/definition` (go-to) **and** `textDocument/hover` on zaozi's dynamic bundle-field access `io.a` to the real `val a = Aligned(...)` field declaration.

Today `io.a` goes through `Referable extends scala.Dynamic` → `transparent inline selectDynamic("a")` → macro `me.jiuyang.zaozi.magic.macros.referableSelectDynamic` → `getRefViaFieldValName(io.refer, "a")`, so the field name survives only as a string literal. In the PC the typed node at `.a` is `Inlined(call = io.selectDynamic("a"), …)`, and dotty's `PcDefinitionProvider` returns `call.symbol` (the framework method `selectDynamic`) — so navigation never reaches the field. The plugin adds one `PluginPhase` (after `typer`, before `SetRootTree`) that **structurally rewrites** `Inlined.call` to a typed reference to the resolved field symbol, so the PC's definition/hover providers resolve to the field.

The pinned zaozi workspace is bumped to Scala 3.8.4 so this project's 3.8.4 PC can typecheck zaozi buffers (today the zaozi integration runs with `--skip-pc` because of 3.7.x/3.8.x skew). SemanticDB-based find-usages for the macro is already handled and is out of scope. The plugin is shipped inside the packaged language server and validated against zaozi's own utest testbench (`zaozi/tests/src/BundleSpec.scala`).

## Acceptance Criteria

Following TDD philosophy, each criterion includes positive and negative tests for deterministic verification.

- AC-1: The `zaoziPcplugin` module builds a well-formed compiler-plugin jar, and that jar is shipped inside the Nix package, without breaking the offline build.
  - Positive Tests (expected to PASS):
    - `mill zaoziPcplugin.jar` produces `out/zaoziPcplugin/jar.dest/out.jar`; its jar root contains `plugin.properties` with `pluginClass=ls.zaozi.pcplugin.ZaoziPcDefinitionPlugin` and the compiled plugin classes.
    - `nix build .#default` (offline) succeeds and installs the plugin jar under `share/scala3-bsp-semantic-ls/` alongside `default-plugin-schema.json`; `nix flake check` and `./scripts/check-ivy-lock.sh` pass (the latter with an empty diff, since `scala3-compiler:3.8.4` is already locked).
    - `nix develop -c mill __.compile && mill __.test` remains green (no regression to existing modules).
  - Negative Tests (expected to FAIL / be rejected when working correctly):
    - The shipped jar does NOT contain `scala3-compiler`/dotty classes (a plain `jar`, never `assembly`); a check for embedded `dotty.tools.*` classes finds none.
    - Building the module introduces NO new artifact into `nix/ivy-lock.nix` (regenerating the lock yields a byte-identical file); a spurious added dependency would make `check-ivy-lock.sh` fail.

- AC-2: The plugin is accepted and its phase runs inside the PC, in both in-process and forked backends.
  - Positive Tests:
    - With `-Xplugin:<zaoziPcplugin.jar>` on a PC target's compiler options (via `PcTargetConfig.scalacOptions` or `pc-plugins.json` `compilerPlugins`), a live `definition`/`hover` request compiles without error and the phase executes — proven by the observable rewrite effect in AC-3 (not by log/stderr noise, and not by doctor status).
    - The plugin works under the forked PC worker (a forked-worker smoke test loads the built jar via `--plugin-config` and gets the AC-3 rewrite result), matching the production/shipped default backend.
  - Negative Tests:
    - A `pc-plugins.json` whose `servicePluginJars`/`compilerPlugins` reference a **missing** jar is dropped by `PcPluginManager` (`Files.exists` filter) → no `-Xplugin` is passed → the PC behaves exactly as with no plugin (request still returns).
    - Note (documented limitation, not a passing test): a **present-but-malformed** compiler-plugin jar IS passed to dotty as `-Xplugin` and is NOT guarded like a `PcServicePlugin` hook — so this plan does not claim "malformed jar still returns". Acceptance does not depend on that behavior.

- AC-3: The plugin steers PC go-to for a dynamic field access to the field declaration (core rewrite).
  - Positive Tests:
    - Unit (non-macro fixture): a single-buffer `Referable[MyBundle] extends scala.Dynamic` with `transparent inline def selectDynamic(name) = helper(this, name)` and `class MyBundle { val a = … }`; with the plugin `-Xplugin`'d, `facade.definition` on `io.a` returns a location whose range covers `val a` in the buffer.
    - Unit (macro-expanded fixture): a fixture (compiled in a prior unit via `dotty.tools.dotc.Main.process`) whose `Inlined.expansion` is `getRefViaFieldValName(...)("a")` also resolves `definition` on `io.a` to the field.
    - Baseline: the SAME fixture WITHOUT the plugin resolves `definition` on `io.a` to `selectDynamic` (not `val a`) — proving the plugin is the cause.
  - Negative Tests:
    - A non-zaozi `scala.Dynamic` `selectDynamic("a")` (receiver not a `Referable[T]` with `T <: DynamicSubfield`) is left UNCHANGED — `definition` is identical with and without the plugin.
    - A dynamic access to a non-existent field, a non-literal name expression, or a malformed receiver degrades to identity — no rewrite, no thrown exception, request still returns.

- AC-4: Field-access-form coverage (first cut): direct, nested, optional, and all `Referable` receiver kinds.
  - Positive Tests:
    - `io.a` (direct), nested `io.f.g` (cursor on each segment resolves to its own field declaration), and optional fields via `getOptionRefViaFieldValName` all resolve `definition` to the field.
    - The receiver may be any `Referable` subtype (e.g. `Interface`, `Writable`, `Wire`, `Reg`, `Node`, `Const`); a fixture exercising at least one non-`Interface` receiver resolves correctly.
  - Negative Tests:
    - Index/slice `applyDynamic` forms (`vec(i)`, `bits(hi, lo)`) are explicitly left as identity for the first cut — `definition` returns the unmodified PC result (never a wrong/garbage location). This is a documented scope boundary (see Pending User Decisions DEC-3, resolved).

- AC-5: The plugin steers PC hover on the dynamic field access to describe the field.
  - Positive Tests:
    - Unit: with the plugin, `facade.hover` on `io.a` returns hover content derived from the `val a` field symbol (its type/signature), not from `selectDynamic`.
    - Baseline: without the plugin, hover on `io.a` reflects `selectDynamic`/`Any` (the framework method), demonstrating the difference.
  - Negative Tests:
    - Hover on a non-zaozi dynamic access is unchanged by the plugin.
    - Hover on an unresolved field degrades to identity (no crash; returns whatever the PC produced).

- AC-6: zaozi is bumped to Scala 3.8.4, and PC go-to + hover on the dynamic access work end-to-end on the real testbench.
  - Positive Tests:
    - `nix/patches/zaozi-semanticdb.patch` bumps zaozi `val scala` to `3.8.4` (keeping `-Xsemanticdb -sourceroot`); zaozi `mill __.compile` succeeds in its own nix env.
    - `scripts/it-zaozi.sh` (or `scripts/it-zaozi-pc.sh`), with `--skip-pc` removed and `<zaozi-ws>/.scala3-bsp-semantic-ls/pc-plugins.json` configuring the plugin, drives a headless PC probe that opens `zaozi/tests/src/BundleSpec.scala`, **scans the source text** to locate the `io.a` use site and the `val a = Aligned(...)` declaration, calls `textDocument/definition` and `textDocument/hover` on the use site, and asserts the definition range covers the `val a` declaration and the hover describes the field. Positions are text-scanned, never hard-coded line numbers.
  - Negative Tests:
    - With NO plugin configured, the same probe resolves `definition` on `io.a` to `selectDynamic` (baseline) — the probe distinguishes success from baseline.
    - If zaozi cannot compile under 3.8.4, the run fails loudly with a clear message (the situation is surfaced/reported, never silently skipped or reported as success).

## Path Boundaries

Path boundaries define the acceptable range of implementation quality and choices.

### Upper Bound (Maximum Acceptable Scope)
A shipped `zaoziPcplugin` jar (in the Nix package `share/`); a `StandardPlugin` with one guarded `PluginPhase` that rewrites `Inlined.call` for `selectDynamic`/`getRefViaFieldValName` on all `Referable` receivers, covering direct, nested, and optional field access; PC go-to AND hover both steered; unit tests with a non-macro single-buffer fixture and a macro-expanded fixture plus negatives; a forked-worker smoke test; the zaozi bump to 3.8.4 and an end-to-end `it-zaozi` PC probe (text-scanned positions) on `BundleSpec.scala`; and docs. `applyDynamic` index/slice navigation may be added but is not required.

### Lower Bound (Minimum Acceptable Scope)
The `zaoziPcplugin` jar builds and is shipped; the plugin rewrite makes `definition` on a direct `io.a` resolve to the field, proven by a unit fixture; hover on `io.a` describes the field; the wiring gate (plugin runs in the PC) and the negative-safety tests (non-zaozi Dynamic unchanged; unresolved/malformed → identity) pass. The zaozi-3.8.4 end-to-end (AC-6) is gated on the bump succeeding; if the bump is infeasible after reasonable effort, that is surfaced explicitly rather than worked around.

### Allowed Choices
- Can use: a plain `ScalaModule` (explicitly NOT `LsModule`) with re-set `moduleDir` (→ `modules/ls-zaozi-pcplugin`), `scalaVersion = Deps.scalaVer` (3.8.4), JDK-25 `javaHome`, `mvnDeps = Seq(Deps.scala3Compiler)`, and a plain `jar`; a `StandardPlugin` + one `MiniPhase`/`PluginPhase`; a structural `Inlined.call` rewrite to `tpd.ref(fieldSym)`; the in-process PC path for the zaozi integration probe (the forked path is covered by a separate smoke test); either `PcTargetConfig.scalacOptions` or `pc-plugins.json` `compilerPlugins` to inject `-Xplugin` in tests.
- Cannot use: `LsModule` for this module; a `ResearchPlugin` (nightly-gated; silently dropped on stable 3.8.4); tree attachments to carry the rewrite (wiped by `InteractiveDriver` cleanup); an `assembly`/fat jar that bundles `scala3-compiler`; doctor "compiler plugins loaded" status as proof the phase ran (it only means the jar exists); hard-coded zaozi source line numbers in the integration probe; any new unlocked dependency (would require an `nix/ivy-lock.nix` regeneration).

> **Note on Deterministic Design**: The mechanism is fixed by user decision (a compiler plugin inside the PC; a minimal non-`LsModule` module; a zaozi bump to 3.8.4). Within that, the phase anchor (`runsAfter=typer`, `runsBefore=SetRootTree`) and the structural `Inlined.call` rewrite are dictated by dotty 3.8.4 internals, so those choices are effectively fixed.

## Feasibility Hints and Suggestions

> **Note**: This section is for reference and understanding only. These are conceptual suggestions, not prescriptive requirements.

### Conceptual Approach
The dotty presentation compiler runs `-Xplugin` `StandardPlugin` phases (plugin insertion happens in `Run.addPluginPhases`, above the interactive compiler's truncated plan `[parser, typer, SetRootTree, cookComments]`). `PcDefinitionProvider`/hover derive their target from the `.symbol` of the typed node at the cursor; for `Inlined(call, …)` that is `call.symbol` (today `selectDynamic`). A `PluginPhase` after `typer` sees the already-expanded `Inlined` node (`transparent inline` forces expansion in typer) and can structurally rewrite `Inlined.call` to a typed `tpd.ref(fieldSym)` with the original span; that rewrite survives `InteractiveDriver` cleanup (unlike attachments, which are wiped). Conceptual phase body:

```
transformInlined(tree):
  try:
    (qual, fieldName) = matchDynamicAccess(tree.call)      // selectDynamic("a") | getRefViaFieldValName(...)("a")
    bundleT           = qual.tpe.baseType(Referable).typeArgs.head   // require T <: DynamicSubfield
    fieldSym          = bundleT.member(termName(fieldName)).symbol   // mirror the macro's field lookup
    if fieldSym.exists && !fieldSym.is(Synthetic-only):
        return cpy.Inlined(tree)(call = tpd.ref(fieldSym).withSpan(tree.call.span))
    else return tree
  catch NonFatal => return tree                            // never break the PC
```

Loading is pure config: `pc-plugins.json` `compilerPlugins` → `PcPluginManager.compilerPluginOptions` (= `-Xplugin:<jar>`) → `PcWorkerManager` appends to the target's scalac options → `ScalaPresentationCompiler.newInstance`; the forked worker receives the config path via `--plugin-config`.

For the unit test, the cheapest primary fixture is a NON-macro `transparent inline def selectDynamic(name) = helper(this, name)` in a single open buffer (produces the same `Inlined(call = selectDynamic("a"))` node with no separate compilation, `val a` in-buffer so go-to lands same-file). A full-macro fixture compiled via `dotty.tools.dotc.Main.process` into a classes dir on the PC classpath covers the `getRefViaFieldValName` expansion. Use an isolated `PcFacade`/`PcPluginManager` per test, not the shared `SharedPc` singleton.

### Relevant References
- `modules/ls-pc/src/ls/pc/PcWorkerManager.scala` — appends `compilerPluginOptions` to the PC target's scalac options; where `-Xplugin` reaches `newInstance`.
- `modules/ls-pc/src/ls/pc/PcPluginManager.scala` — `compilerPluginOptions`, `CompilerPluginStatus` (jar-existence only; not phase-run proof).
- `modules/ls-pc/src/ls/pc/PcFacade.scala` — `definition`/`hover` request path (the compiler-plugin rewrite steers these, distinct from the `PcServicePlugin` after-hooks).
- `modules/ls-pc/test/src/ls/pc/{PcTestHarness,PcQuerySuite,PcWorkerManagerSuite,CompilerPluginConfigSuite}.scala` — PC test harness, `definition(uri,line,char)` drive, existing `-Xplugin` option plumbing.
- `modules/ls-semanticdb/test/src/ls/semanticdb/ScalacIntegrationSuite.scala` — in-process `dotty.tools.dotc.Main.process` fixture-compilation pattern.
- `modules/ls-core/src/ls/core/AotTrain.scala` — headless in-process `ScalaLs` driver; where a text-scanned `definition`/`hover` zaozi probe is added; note it boots `Bootstrap.Config`'s in-process PC default.
- `build.mill` — `LsModule` trait, `Deps.scalaVer` (3.8.4), `Deps.scala3Compiler`; module/jar/resources conventions.
- `nix/package.nix` — buildPhase/installPhase (add `zaoziPcplugin.jar` build + copy to `share/`); `nix/ivy-lock.nix` already contains `scala3-compiler` 3.8.4.
- `nix/patches/zaozi-semanticdb.patch`, `scripts/it-zaozi.sh` — zaozi patch (bump to 3.8.4) and the integration driver.
- `zaozi/tests/src/BundleSpec.scala` (pinned zaozi) — testbench probe: `BundleSpecIO.a`, `io.a` uses, nested `io.f.g`, optional `io.k`.

## Dependencies and Sequence

### Milestones
1. Plugin module and PC wiring gate.
   - Phase A: add the `zaoziPcplugin` module (`build.mill`, `resources/plugin.properties`) and a no-op `PluginPhase(runsAfter=typer, runsBefore=SetRootTree)`; build `zaoziPcplugin.jar`.
   - Phase B: unit test proving the plugin loads and the phase runs inside the PC on a live `definition` request (real jar via `-Xplugin`), in-process; then a forked-worker smoke variant.
2. The rewrite: go-to + hover on a fixture.
   - Phase A: implement `transformInlined` with the guarded match + `Inlined.call` rewrite; cover direct/nested/optional and all `Referable` receivers; `applyDynamic` index/slice left as identity.
   - Phase B: unit tests — non-macro single-buffer fixture (primary), macro-expanded fixture (defensive), hover, and negatives (non-zaozi Dynamic, missing field, malformed → identity).
3. zaozi bump + end-to-end (gated).
   - Phase A: bump zaozi to 3.8.4 in the nix patch; verify `mill __.compile` on zaozi in its own nix env (risk gate — blocks the rest if it fails).
   - Phase B: wire `it-zaozi.sh` (build jar, write `pc-plugins.json`, drop `--skip-pc`) and add the text-scanned `definition`/`hover` probe to `AotTrain` (or a small driver); assert on `BundleSpec.scala`.
4. Packaging + docs.
   - Phase A: `nix/package.nix` builds and ships `zaoziPcplugin.jar` under `share/`; confirm offline `nix build`/`nix flake check`/`check-ivy-lock`.
   - Phase B: document the plugin and the `compilerPlugins` config in `docs/plugin-spi.md`/`docs/deployment.md`, including the doctor-status caveat.

Dependencies: Milestone 2 depends on Milestone 1 (a loading plugin). Milestone 3 Phase B depends on 3 Phase A (the bump) and Milestone 2 (a working rewrite). Milestone 4 packaging depends on Milestone 1 (a buildable jar) and can proceed in parallel with 3.

## Task Breakdown

Each task must include exactly one routing tag:
- `coding`: implemented by Claude
- `analyze`: executed via Codex (`/humanize:ask-codex`)

| Task ID | Description | Target AC | Tag (`coding`/`analyze`) | Depends On |
|---------|-------------|-----------|----------------------------|------------|
| task1 | Add `object zaoziPcplugin extends ScalaModule` (moduleDir → `modules/ls-zaozi-pcplugin`, `scalaVersion=3.8.4`, JDK-25 `javaHome`, `mvnDeps=Seq(Deps.scala3Compiler)`, plain `jar`) + `resources/plugin.properties`; explicit `object test extends ScalaTests with TestModule.Munit` with `pc` dep | AC-1 | coding | - |
| task2 | `ZaoziPcDefinitionPlugin extends StandardPlugin` + no-op `PluginPhase(runsAfter=typer, runsBefore=SetRootTree)`; unit test (isolated `PcFacade`) proving it loads/runs in the PC via `-Xplugin` on a live request; forked-worker smoke variant | AC-2 | coding | task1 |
| task3 | Implement `transformInlined`: match `selectDynamic`/`getRefViaFieldValName` on `Referable[T<:DynamicSubfield]`, resolve `fieldSym` from `T`, rewrite `Inlined.call → tpd.ref(fieldSym)`; cover nested/optional/all receiver kinds; guarded identity fallback; `applyDynamic` index/slice = identity | AC-3, AC-4 | coding | task2 |
| task4 | Unit tests: non-macro single-buffer fixture (primary go-to), macro-expanded fixture via `Main.process` (defensive), hover assertions, and negatives (non-zaozi Dynamic unchanged; missing field / non-literal / malformed → identity) | AC-3, AC-4, AC-5 | coding | task3 |
| task5 | Bump zaozi to 3.8.4 in `nix/patches/zaozi-semanticdb.patch`; verify zaozi `mill __.compile` in its own nix env; report any 3.7.4→3.8.4 source blockers | AC-6 | analyze | task1 |
| task6 | Wire `scripts/it-zaozi.sh` (build `zaoziPcplugin.jar`, write `pc-plugins.json`, drop `--skip-pc`); add a text-scanned `definition`+`hover` probe on `BundleSpec.scala` to `AotTrain` (or a small headless driver) | AC-6 | coding | task3, task5 |
| task7 | `nix/package.nix`: build `zaoziPcplugin.jar` and install it under `share/scala3-bsp-semantic-ls/`; confirm offline `nix build .#default`, `nix flake check`, `./scripts/check-ivy-lock.sh` | AC-1 | coding | task1 |
| task8 | Docs: `docs/plugin-spi.md`/`docs/deployment.md` — the plugin, the `compilerPlugins` config, and the doctor-status caveat (jar-existence ≠ phase ran) | AC-1, AC-2 | coding | task3 |

## Claude-Codex Deliberation

### Agreements
- The module is a plain `ScalaModule` (not `LsModule`) with explicit `moduleDir`/`scalaVersion`/`javaHome`, `mvnDeps = Seq(Deps.scala3Compiler)`, and a plain `jar` (never `assembly`).
- `scala3-compiler:3.8.4` is already in `nix/ivy-lock.nix`, so no lock regeneration is needed; the ivy-lock gate passes with a no-op diff.
- The PC loading path (`PcPluginManager.compilerPluginOptions` → `PcWorkerManager` → `newInstance`; forked via `--plugin-config`) is correct, and acceptance is based on the actual `definition`/`hover` result, not doctor status.
- The dotty 3.8.4 technique is sound: `-Xplugin` StandardPlugin phases run in the interactive PC; anchor `runsAfter=typer`/`runsBefore=SetRootTree`; structurally rewrite `Inlined.call` (attachments are wiped).

### Resolved Disagreements
- Malformed-jar safety (Codex): `PcPluginManager` only `Files.exists`-checks, so a present-but-malformed compiler-plugin jar IS passed to dotty and is NOT guarded like a service plugin. Resolution: AC-2's negative distinguishes a MISSING jar (dropped → PC unaffected) from a malformed one, and the plan does not claim malformed-jar resilience; the phase-run proof is AC-3's rewrite effect, not a log or doctor status.
- Proving the feature end-to-end (Codex): dropping `--skip-pc` alone is insufficient — `AotTrain.strict` only runs a generic completion probe. Resolution: task6 adds a dedicated text-scanned `definition`+`hover` probe on `BundleSpec.scala`.
- PC backend mode (Codex): `--aot-train` boots `Bootstrap.Config`'s in-process PC default. Resolution: the zaozi integration proves in-process PC explicitly, and a separate forked-worker smoke test (task2) covers the forked/production default.
- Test module + isolation (Codex): a plain `ScalaModule` does not inherit `LsTests`. Resolution: task1 defines an explicit `object test extends ScalaTests with TestModule.Munit` with a `pc` dependency, and tests use an isolated `PcFacade`/`PcPluginManager` per test (not the shared `SharedPc` singleton, whose service plugins mutate results).
- Position determinism (Codex): don't hard-code zaozi line numbers (they shift on the 3.8.4 bump). Resolution: the integration probe scans `BundleSpec.scala` text to locate the `io.a` use and the `val a` declaration.

### Convergence Status
- Final Status: `converged`

## Pending User Decisions

- DEC-1: Ship the plugin jar with the packaged LS, or keep it a repo-local integration/test artifact only?
  - Claude Position: Repo-local only (the plugin is zaozi-specific; keep the shipped LS zaozi-agnostic).
  - Codex Position: N/A - open question (Codex flagged it as needing a decision).
  - Tradeoff Summary: Shipping in `nix/package.nix` distributes the jar under `share/` (users point `pc-plugins.json` at it) but couples the general server to a zaozi-specific plugin and adds offline-build wiring; repo-local keeps the package lean.
  - Decision Status: Ship in the package (user decision). `nix/package.nix` builds `zaoziPcplugin.jar` and installs it under `share/scala3-bsp-semantic-ls/` (task7); AC-1 reflects this.

- DEC-2: Definition-only, or also steer hover?
  - Claude Position: Definition-only for the first cut; hover as a follow-up.
  - Codex Position: N/A - open question.
  - Tradeoff Summary: The same `Inlined.call` rewrite makes hover resolve the field largely for free; including it now adds hover assertions/tests but broadens value in one pass.
  - Decision Status: Include hover now (user decision). AC-5 and task4 cover hover.

- DEC-3: Which dynamic access forms in the first cut — field access only, or also `applyDynamic` index/slice and wrapper receivers?
  - Claude Position: Field access on all `Referable` receivers (incl. wrappers); `applyDynamic` index/slice out of first cut.
  - Codex Position: N/A - open question.
  - Tradeoff Summary: Field access covers the primary navigation need with a single `Inlined.call` matcher; `applyDynamic` (`vec(i)`, `bits(hi,lo)`) needs additional matchers on the apply expansions.
  - Decision Status: Field access incl. wrappers (user decision); `applyDynamic` index/slice left as identity (AC-4).

## Implementation Notes

### Code Style Requirements
- Implementation code and comments must NOT contain plan-specific terminology such as "AC-", "Milestone", "Step", "Phase" (as a workflow marker), "task1", or similar plan/workflow markers. (Note: dotty's own compiler-phase concept and the `PluginPhase`/`phaseName` API are legitimate domain terms and are fine.)
- These plan markers are for this document only, not for the resulting codebase.
- Use descriptive, domain-appropriate naming in code (e.g. `ZaoziPcDefinitionPlugin`, `phaseName = "zaozi-pc-dynamic-nav"`).

### Operational Notes
- The rewrite phase must be strictly guarded (`try`/`NonFatal` → identity) so it never throws inside the PC; a throwing compiler plugin is not disabled by the service-plugin guard path and could otherwise destabilize interactive requests (forked mode survives but may respawn-replay into the same failure).
- Do not rely on the doctor "compiler plugins loaded: N of M" line as proof the phase ran — it reflects jar existence only. Prove the effect via the `definition`/`hover` result.
- The zaozi 3.8.4 compile gate (task5) is load-bearing: run it before investing in the integration-script rewiring; if zaozi cannot compile under 3.8.4, surface it rather than silently changing mechanism.

--- Original Design Draft Start ---

# Plan: `zaozi-pcplugin` — PC go-to for zaozi's `Dynamic` bundle-field macro

## Context

zaozi bundle fields are read dynamically: `io.a` compiles (via
`Referable extends scala.Dynamic` → `transparent inline selectDynamic("a")` →
macro `me.jiuyang.zaozi.magic.macros.referableSelectDynamic`) to
`getRefViaFieldValName(io.refer, "a")`, where the field name survives only as a
**string literal**. In the presentation compiler the typed node at `.a` is
`Inlined(call = io.selectDynamic("a"), …)`, and dotty's `PcDefinitionProvider`
resolves it to the framework method `selectDynamic` — so **editor go-to on `io.a`
never reaches the `val a = Aligned(...)` declaration**.

SemanticDB-side navigation (find-usages via the index) is **already handled** and
is out of scope here. This task delivers the **PC** side: a scalac compiler plugin,
built as a new target **`zaozi-pcplugin`**, loaded into our presentation compiler,
that makes the PC resolve `io.a` go-to (and hover) to the real field declaration.
Validated against zaozi's own utest testbench (`zaozi/tests/src/BundleSpec.scala`).

**Decisions (user):** implement as a **compiler plugin inside the PC** (not a
build-time SemanticDB enhancer, not an index change); the module is **minimal — a
plain plugin module, NOT `LsModule`**; **bump zaozi to Scala 3.8.4** so our 3.8.4
PC can actually compile zaozi buffers (today it's `--skip-pc`'d for 3.7.x skew).

## Confirmed feasibility (dotty 3.8.4 internals)

- **The PC runs `-Xplugin` `StandardPlugin` phases.** Plugin insertion is in
  `Run.addPluginPhases` (`dotc/Run.scala:356-359`), above the interactive
  compiler's truncated plan `parser, typer, SetRootTree, cookComments`
  (`dotc/interactive/InteractiveCompiler.scala:15-20`). `-Xplugin` reaches the PC
  unfiltered (`dotty/tools/pc/ScalaPresentationCompiler.scala` strips only
  `-print-tasty`). **Phase must anchor on `runsAfter=Set("typer")`,
  `runsBefore=Set("SetRootTree")`** — `posttyper`/`inlining` are absent in the
  interactive plan and get mis-scheduled (`plugins/Plugins.scala:246`).
- **Go-to target = the `.symbol` of the typed node at the cursor**
  (`PcDefinitionProvider.scala:44-51,90-94`; `MetalsInteractive.enclosingSymbols`).
  For `Inlined(call,…)` it returns `call.symbol` (`MetalsInteractive.scala:171-172`)
  → today `selectDynamic`.
- **A plugin phase after typer can steer it by STRUCTURAL rewrite, not attachment.**
  `transparent inline` already expanded the tree during typer, so the phase sees the
  `Inlined`/`getRefViaFieldValName(...)("a")` node. Rewriting `Inlined.call` to
  `tpd.ref(fieldSym).withSpan(call.span)` makes `enclosingSymbols` return `fieldSym`,
  and `PcDefinitionProvider.locationsForSymbol` (`:131-148`) yields the `val a`
  location (same file via `namePos`/`findTreesMatching`, other file via the index).
  **Attachments are wiped** by `InteractiveDriver` cleanup / `removeAllAttachments`
  (`InteractiveDriver.scala:308`) before the provider reads the tree — do not use them.
- **No codegen cost:** the PC stops at `CookComments`; the rewrite only affects the
  interactive tree the definition/hover providers read.

## How the plugin reaches the PC (already wired)

`pc-plugins.json` `compilerPlugins` → `PcPluginManager.compilerPluginOptions` (=
`-Xplugin:<jar>`) → appended to each target's PC scalac options in
`PcWorkerManager.scala:170` → passed to `ScalaPresentationCompiler.newInstance`. So
loading is purely config: drop `zaozi-pcplugin.jar` into the workspace's
`.scala3-bsp-semantic-ls/pc-plugins.json`. (Forked PC is the default; the config
path is forwarded to the worker via `--plugin-config`.)

## Design

### Build target `zaozi-pcplugin` (minimal module, NOT `LsModule`)

In `build.mill`, a plain `ScalaModule` (no `LsModule` conventions, no server
moduleDeps):

```scala
object zaoziPcplugin extends ScalaModule {
  def moduleDir  = mill.api.BuildCtx.workspaceRoot / "modules" / "ls-zaozi-pcplugin"
  def scalaVersion   = Deps.scalaVer                    // 3.8.4 == our PC
  def compileMvnDeps = Seq(Deps.scala3Compiler)         // provided: dotty internals, NOT packaged
  // plain `jar` (inherited) — must NOT be an assembly; plugin.properties from resources/
}
```

Jar contents = the plugin classes + `resources/plugin.properties`
(`pluginClass=ls.zaozi.pcplugin.ZaoziPcDefinitionPlugin`). Nothing bundled.

### The plugin: `StandardPlugin` + one `PluginPhase`

`ls.zaozi.pcplugin.ZaoziPcDefinitionPlugin extends StandardPlugin` (no nightly gate,
unlike PR #91's `ResearchPlugin`), returning one `PluginPhase extends MiniPhase`:

- `phaseName = "zaozi-pc-dynamic-nav"`, `runsAfter = Set("typer")`,
  `runsBefore = Set("SetRootTree")`.
- `override def transformInlined(tree: Inlined)`: match `tree.call` of the zaozi
  dynamic shape — `qual.selectDynamic(Literal("name"))` on a `qual: Referable[T]`
  where `T <: me.jiuyang.zaozi.magic.DynamicSubfield` (also handle the expanded
  `getRefViaFieldValName(...)("name")` form defensively). Resolve the field symbol
  the same way the macro does: from `qual.tpe.baseType(Referable).typeArgs.head`
  (the bundle type `T`), `fieldSym = T.member(termName("name")).symbol` (fall back to
  `T.classSymbol.info.decls` lookup). If found & real, return
  `cpy.Inlined(tree)(call = tpd.ref(fieldSym).withSpan(tree.call.span), …)`; else
  leave the tree unchanged. Guard everything in try/`NonFatal` → identity (never
  break the PC).
- Optional: also cover `applyDynamic`/`applyDynamicNamed` (`io.vec(i)`, `io.bits`)
  and `getOptionRefViaFieldValName` (optional fields) once the base case works.

This is zaozi-API-specific (keys on `Referable`/`DynamicSubfield`/`getRefViaFieldValName`),
so it only ever rewrites zaozi dynamic accesses and is inert elsewhere.

### Wire zaozi + validate

- Extend `nix/patches/zaozi-semanticdb.patch` (or a second patch) to zaozi's
  `build.mill` `ZaoziScalaModule`: bump `val scala = "3.7.4"` → `"3.8.4"` (keep the
  existing `-Xsemanticdb -sourceroot`). No `-Xplugin` in zaozi's build — the plugin
  runs in OUR PC, not zaozi's compile.
- `scripts/it-zaozi.sh` (or a new `scripts/it-zaozi-pc.sh`): build `mill
  zaoziPcplugin.jar`; write `<zaozi-ws>/.scala3-bsp-semantic-ls/pc-plugins.json` =
  `{"compilerPlugins":[{"jars":["<abs plugin.jar>"]}]}`; boot our server against
  zaozi over real Mill BSP **without `--skip-pc`**.
- Add a headless PC go-to probe (extend `AotTrain` or a small driver): open
  `zaozi/tests/src/BundleSpec.scala`, call `docs.definition(...)` with the cursor on
  `io.a` at line 63 (and `io.f.g`), assert the returned location is `val a` at
  BundleSpec.scala **line 39** (and `SimpleBundle.g` line 19-20). Also assert
  `doctor` shows the compiler plugin loaded (`PcPluginManager` compiler-plugin
  status). Non-empty + correct file/line.

## Files

**New:**
- `build.mill` — add `object zaoziPcplugin extends ScalaModule` (above).
- `modules/ls-zaozi-pcplugin/src/ls/zaozi/pcplugin/ZaoziPcDefinitionPlugin.scala`
  (StandardPlugin + the PluginPhase + field-resolution helpers).
- `modules/ls-zaozi-pcplugin/resources/plugin.properties`.
- `modules/ls-zaozi-pcplugin/test/...` — unit test: run the dotty PC (via the
  existing `ls-pc` test harness / `scala3-presentation-compiler`) on a tiny
  `Referable`+`Dynamic` fixture with the plugin `-Xplugin`'d, assert `definition` on
  the dynamic access resolves to the field (isolates the plugin from the heavy
  zaozi/CIRCT build).

**Modified:**
- `nix/patches/zaozi-semanticdb.patch` (bump zaozi Scala to 3.8.4).
- `scripts/it-zaozi.sh` (build+configure the plugin; drop `--skip-pc`; PC go-to asserts).
- `modules/ls-core/.../AotTrain.scala` (add the PC go-to probe on the dynamic access).
- `docs/plugin-spi.md` / `docs/deployment.md` (document the plugin + `compilerPlugins` config).

## Implementation phases (with risk gates)

1. **Module + empty plugin loads in the PC.** New module compiles vs 3.8.4
   scala3-compiler; a no-op `PluginPhase(runsAfter=typer, runsBefore=SetRootTree)`.
   **Gate:** an `ls-pc` unit test shows the phase runs in the PC (e.g. a log/side
   effect during a PC `definition` call) — confirms the wiring the feasibility
   agent described.
2. **Rewrite → PC go-to works on a fixture.** Implement `transformInlined`;
   unit-test on a minimal `Referable[Bundle]` + `io.a` fixture: `definition` returns
   the field. **Gate:** green on the fixture (proves the technique end-to-end,
   cheaply, before touching zaozi).
3. **Bump zaozi to 3.8.4.** **Risk gate:** `mill __.compile` on zaozi @ 3.8.4 in its
   own nix env must pass (3.7.4→3.8.4 may need small source fixes). If zaozi cannot
   compile under 3.8.4 after reasonable effort → stop and report (this blocks the
   PC-on-zaozi approach; see Risks).
4. **End-to-end on the real testbench** via `it-zaozi.sh`: PC go-to on `BundleSpec`
   `io.a` → `val a` (line 39), plugin visible in doctor.
5. Docs + a scoped (non-default-CI) note.

## Verification

- `nix develop -c mill zaoziPcplugin.compile zaoziPcplugin.test` (fixture: PC go-to
  on a dynamic access resolves to the field).
- `nix develop -c mill __.compile && mill __.test` (no regressions).
- `nix develop -c ./scripts/it-zaozi.sh`: zaozi @ 3.8.4 builds; our PC (with the
  plugin via `pc-plugins.json`) answers `definition` on `io.a` → `val a` line 39;
  doctor reports the compiler plugin loaded; green line printed.

## Risks & fallbacks

- **zaozi may not compile under 3.8.4** (a 3.7.4 macro/FFM codebase). Only provable
  by building it (phase-3 gate). This is the load-bearing risk: the PC can only run
  the plugin on zaozi if the 3.8.4 PC can typecheck zaozi buffers. If infeasible,
  the PC-plugin path is blocked — fallbacks would be a macro-side change (emit a
  real field reference in `selectDynamic`'s expansion) or an index-backed definition
  handler; both are out of the chosen scope, so I'd surface it rather than silently
  switch.
- **Field resolution must mirror the macro.** The plugin re-does
  `T.member(termName(name))`; if a field isn't statically exposed on the bundle
  refinement, resolution fails for that case (degrade to identity, log). Cover the
  common `Aligned/Flipped` + optional forms; exotic shapes may need the macro's exact
  logic.
- **PC must load zaozi's classpath** (native FFM/CIRCT via BSP). If the PC can't boot
  a zaozi target at all (independent of the plugin), go-to can't run — caught at
  phase 4.
- **`StandardPlugin` phase scheduling after `typer`** — validated in principle by the
  feasibility trace; confirm concretely at phase 1 before building the rewrite logic.

--- Original Design Draft End ---
