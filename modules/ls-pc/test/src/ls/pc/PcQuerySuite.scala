package ls.pc

import java.nio.file.Files

import scala.concurrent.duration.*
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.MarkupContent

/** End-to-end queries against a real Scala 3 presentation compiler, sharing
  * one facade/PC instance across all tests (instance creation and the first
  * compile are the expensive parts).
  */
class PcQuerySuite extends munit.FunSuite:
  override def munitTimeout: Duration = 5.minutes

  import SharedPc.{facade, labels, openBuffer, targetId}

  private def hoverText(hover: org.eclipse.lsp4j.Hover): String =
    val contents = hover.getContents
    if contents == null then ""
    else if contents.isRight then contents.getRight.getValue
    else
      contents.getLeft.asScala
        .map(e => if e.isLeft then e.getLeft else e.getRight.getValue)
        .mkString("\n")

  test("completion on List receiver contains map/filter; marker plugin appends; thrower never fails the request"):
    val text = "object Main:\n  val xs = List(1)\n  val ys = xs.\n"
    val uri = openBuffer(text)
    // cursor right after `xs.` on line 2
    val list = facade.completion(uri, 2, "  val ys = xs.".length)
    val ls = labels(list)
    assert(ls.exists(_.startsWith("map")), s"missing map in: ${ls.take(30)}")
    assert(ls.exists(_.startsWith("filter")), s"missing filter in: ${ls.take(30)}")
    // (a) plugin-appended marker item is present
    assert(ls.contains(SharedPc.marker.marker), s"missing marker item in: ${ls.takeRight(5)}")
    // (c) the throwing plugin crashed during this request yet the request succeeded;
    //     it is now disabled with a recorded reason + throwable
    val report = facade.pluginStatus
    val disabled = report.disabled.find(_.id == "throwing-plugin")
    assert(disabled.isDefined, s"throwing-plugin not disabled: ${report.disabled}")
    assert(disabled.get.reason.contains("afterCompletion"), disabled.get.reason)
    assert(SharedPc.pluginManager.disabledCause("throwing-plugin").exists(_.getMessage.startsWith("boom")))
    assert(!SharedPc.pluginManager.enabledPluginIds.contains("throwing-plugin"))

  test("completionItemResolve round-trips an item through the PC"):
    val text = "object ResolveTest:\n  val xs = List(1)\n  val ys = xs.\n"
    val uri = openBuffer(text)
    val list = facade.completion(uri, 2, "  val ys = xs.".length)
    val mapItem = list.getItems.asScala.find(_.getLabel.startsWith("map")).get
    val resolved =
      facade.completionItemResolve(targetId, mapItem, "scala/collection/immutable/List#map().")
    assert(resolved != null)
    assertEquals(resolved.getLabel, mapItem.getLabel)

  test("dirty buffer: didChange(full text) updates completions"):
    val v1 = "object Dirty:\n  val alphaOne = 1\n  val z = alph\n"
    val uri = openBuffer(v1)
    val before = labels(facade.completion(uri, 2, "  val z = alph".length))
    assert(before.exists(_.startsWith("alphaOne")), s"expected alphaOne in: ${before.take(20)}")

    val v2 = "object Dirty:\n  val betaTwo = 1\n  val z = beta\n"
    facade.didChange(uri, v2)
    assertEquals(facade.bufferText(uri), Some(v2))
    val after = labels(facade.completion(uri, 2, "  val z = beta".length))
    assert(after.exists(_.startsWith("betaTwo")), s"expected betaTwo in: ${after.take(20)}")
    assert(!after.exists(_.startsWith("alphaOne")), s"stale alphaOne in: ${after.take(20)}")

  test("hover on a val shows its inferred type"):
    val text = "object HoverTest:\n  val xs = List(1)\n  val use = xs\n"
    val uri = openBuffer(text)
    val hover = facade.hover(uri, 2, "  val use = x".length) // on `xs` usage
    assert(hover.isDefined, "expected a hover")
    val rendered = hoverText(hover.get)
    assert(rendered.contains("List[Int]"), s"hover did not mention List[Int]: $rendered")

  test("UTF-16 offsets: emoji (surrogate pairs) before the cursor on the same line"):
    val line1 = "  val s = \"🚀🚀\"; val t = s."
    val text = s"object Emoji:\n$line1\n"
    val uri = openBuffer(text)
    // character counts UTF-16 code units: each emoji is 2 units; cursor at line end
    val list = facade.completion(uri, 1, line1.length)
    val ls = labels(list)
    assert(ls.exists(_.startsWith("length")), s"missing String member 'length' in: ${ls.take(30)}")
    assert(ls.exists(_.startsWith("substring")), s"missing String member 'substring' in: ${ls.take(30)}")

  test("signatureHelp inside a call"):
    val text = "object SigTest:\n  def bar(x: Int, y: String): Int = x\n  val z = bar(1, \"a\")\n"
    val uri = openBuffer(text)
    val help = facade.signatureHelp(uri, 2, "  val z = bar(1".length) // inside the args
    assert(help.getSignatures != null && !help.getSignatures.isEmpty, "expected signatures")
    val label = help.getSignatures.get(0).getLabel
    assert(label.contains("bar"), s"unexpected signature label: $label")
    assert(label.contains("x: Int"), s"unexpected signature label: $label")

  test("definition to a same-file definition returns the def's range; plugin additions are origin-marked"):
    val text = "object DefTest:\n  def foo(x: Int): Int = x\n  val y = foo(1)\n"
    val uri = openBuffer(text)
    val result = facade.definition(uri, 2, "  val y = fo".length) // on `foo` usage
    assert(result.locations.nonEmpty, "expected definition locations")

    val workspace = result.locations.filter(_.origin == DefinitionOrigin.Workspace)
    assert(workspace.nonEmpty, s"no workspace-origin location: ${result.locations}")
    val defLoc = workspace.head.location
    assertEquals(defLoc.getUri, uri)
    assertEquals(defLoc.getRange.getStart.getLine, 1)
    assertEquals(defLoc.getRange.getStart.getCharacter, "  def ".length)

    // the DefinitionAugmentPlugin appended two locations claiming Workspace origin;
    // the facade must re-mark them (plan 14.4: definition — yes, but mark the source)
    val synthetic = result.locations.filter(_.origin == DefinitionOrigin.Synthetic)
    assert(
      synthetic.exists(_.location.getUri == SharedPc.syntheticGenUri),
      s"synthetic-source hit not marked: ${result.locations}"
    )
    val plugin = result.locations.filter(_.origin == DefinitionOrigin.Plugin)
    assert(
      plugin.exists(_.location.getUri == SharedPc.foreignUri),
      s"plugin-added hit not marked: ${result.locations}"
    )
    assert(result.hasSyntheticHits)

  test("typeDefinition jumps to the type's same-file definition"):
    val text = "object TypeDefTest:\n  class Box\n  val b = new Box\n  def use = b\n"
    val uri = openBuffer(text)
    val result = facade.typeDefinition(uri, 3, "  def use = ".length) // on `b` usage
    val workspace = result.locations.filter(_.origin == DefinitionOrigin.Workspace)
    assert(workspace.nonEmpty, s"expected a workspace typeDefinition location: ${result.locations}")
    assertEquals(workspace.head.location.getUri, uri)
    assertEquals(workspace.head.location.getRange.getStart.getLine, 1)

  test("prepareRename on a local returns its range"):
    val text = "object RenTest:\n  def m: Int =\n    val local = 1\n    local + 1\n"
    val uri = openBuffer(text)
    val range = facade.prepareRename(uri, 3, "    lo".length) // on `local` usage
    assert(range.isDefined, "expected a prepareRename range")
    assertEquals(range.get.getStart.getLine, 3)
    assertEquals(range.get.getStart.getCharacter, "    ".length)
    assertEquals(range.get.getEnd.getCharacter, "    local".length)

  test("plugin patchOptions flag and compiler-plugin materialization reach the live instance"):
    // (b) the FlagOptionsPlugin adds -deprecation; the instance serving all the
    // requests above must have received it
    val instance = facade.workerManager.getOrCreate(SharedPc.targetConfig)
    assert(instance.effectiveOptions.contains(SharedPc.flag.flag), instance.effectiveOptions.toString)
    assert(instance.isLoaded || instance.scalaVersion.nonEmpty)

  test("plugin synthetic sources are materialized and appear on the source path"):
    val genDir = SharedPc.generatedSourcesRoot.resolve(targetId)
    val genFile = genDir.resolve("Gen.scala")
    assert(Files.exists(genFile), s"synthetic source not materialized: $genFile")
    assert(Files.readString(genFile).contains("LsPcGenerated"))
    val instance = facade.workerManager.getOrCreate(SharedPc.targetConfig)
    assert(instance.effectiveSourcePath.contains(genDir), instance.effectiveSourcePath.toString)
    assert(instance.syntheticUris.contains(SharedPc.syntheticGenUri))

  test("pc diagnostics flow through filterPcDiagnostics"):
    val text = "object DiagTest:\n  val x: Int = \"not an int\"\n"
    val uri = openBuffer(text)
    val before = SharedPc.diagProbe.invocations.get()
    val diags = facade.diagnostics(uri)
    assertEquals(SharedPc.diagProbe.invocations.get(), before + 1, "filter hook did not run")
    // the probe returns diagnostics unchanged, so the facade result equals what it saw
    assertEquals(diags, SharedPc.diagProbe.lastSeen)

  test("didClose drops the buffer; queries on it then fail fast"):
    val uri = openBuffer("object CloseMe\n")
    facade.didClose(uri)
    assert(!facade.openBuffers.contains(uri))
    intercept[IllegalStateException](facade.completion(uri, 0, 0))

  test("plugin status lists all registered service plugins for the doctor"):
    val report = facade.pluginStatus
    val ids = report.servicePlugins.map(_.id).toSet
    for expected <- Vector(
        "marker-completion",
        "flag-options",
        "synthetic-source",
        "definition-augment",
        "diagnostics-probe",
        "throwing-plugin"
      )
    do assert(ids.contains(expected), s"missing $expected in $ids")

  test("restartTarget disposes the instance; the next request lazily recreates it"):
    val before = facade.workerManager.getOrCreate(SharedPc.targetConfig)
    assert(facade.restartTarget(targetId))
    assert(!facade.activeTargets.contains(targetId))
    val uri = openBuffer("object AfterRestart:\n  val xs = List(1)\n  val ys = xs.\n")
    val ls = labels(facade.completion(uri, 2, "  val ys = xs.".length))
    assert(ls.exists(_.startsWith("map")), s"completion after restart broken: ${ls.take(20)}")
    val after = facade.workerManager.getOrCreate(SharedPc.targetConfig)
    assert(before ne after, "expected a fresh PcInstance after restartTarget")
