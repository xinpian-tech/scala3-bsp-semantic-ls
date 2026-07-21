package ls.pc.navtest

import scala.util.control.NonFatal

import dotty.tools.dotc.ast.tpd.*
import dotty.tools.dotc.core.Constants.Constant
import dotty.tools.dotc.core.Contexts.Context
import dotty.tools.dotc.core.Flags
import dotty.tools.dotc.core.Names.termName
import dotty.tools.dotc.core.Symbols.*
import dotty.tools.dotc.core.Types.*
import dotty.tools.dotc.plugins.{PluginPhase, StandardPlugin}

/** TEST-FIXTURE presentation-compiler plugin proving the `pc-plugins.json`
  * `compilerPlugins` loading path end-to-end. It is NOT shipped in the package;
  * the live boundary test `crates/ls-jvm/tests/live_pcplugin.rs` (flake check
  * `pc-plugin-load`) loads this jar into the embedded island through a
  * workspace `pc-plugins.json` and observes the steering below over the real
  * vtable.
  *
  * The plugin defines its OWN marker shape (no third-party API knowledge): a
  * receiver deriving from the fixture trait `lstest.navfixture.NavProbe[T]`
  * (a `scala.Dynamic` whose `transparent inline selectDynamic` a test buffer
  * declares under that exact name). A dynamic access `io.a` on such a receiver
  * retains the pre-inlining call `io.selectDynamic("a")`, which carries only
  * the framework method symbol — so vanilla go-to lands on `selectDynamic`.
  * The single guarded phase rewrites that `Inlined.call` to a typed reference
  * to the real field `T.a`, so the interactive symbol-at-cursor lookup returns
  * the field declaration. Any non-marker `scala.Dynamic` of the same shape is
  * left unchanged, so the observed steering is unambiguously this plugin's
  * doing — the same island path a real navigation plugin (e.g. zaozi's
  * in-build `zaozi-compiler-plugin`) uses.
  */
class NavTestPcPlugin extends StandardPlugin:
  val name: String = "pc-navtest"
  override val description: String =
    "Test fixture: steer go-to on the navtest fixture Dynamic field access to the field declaration."

  override def initialize(options: List[String])(using Context): List[PluginPhase] =
    List(new NavTestPhase)

class NavTestPhase extends PluginPhase:
  val phaseName: String = NavTestPhase.name

  // Anchor on phases the interactive presentation compiler actually schedules
  // (it runs only parser, typer, SetRootTree, cookComments).
  override val runsAfter: Set[String] = Set("typer")
  override val runsBefore: Set[String] = Set("SetRootTree")

  override def transformInlined(tree: Inlined)(using Context): Tree =
    try
      tree.call match
        case Apply(Select(qual, sel), List(Literal(Constant(field: String))))
            if sel.toString == "selectDynamic" =>
          steeredRef(qual.tpe.widen, field) match
            case Some(fieldRef) =>
              cpy.Inlined(tree)(fieldRef.withSpan(tree.call.span), tree.bindings, tree.expansion)
            case None => tree
        case _ => tree
    catch case NonFatal(_) => tree

  /** A typed `ref` to `fieldName` on the `T` of a `NavProbe[T]` receiver;
    * None when the receiver is not the fixture marker trait or the field does
    * not resolve to a real, non-synthetic term member (identity — the phase
    * must never fail an interactive request).
    */
  private def steeredRef(receiver: Type, fieldName: String)(using Context): Option[Tree] =
    receiver.baseClasses
      .find(_.fullName.toString == NavTestPhase.ProbeName)
      .flatMap(probe => receiver.baseType(probe).argInfos.headOption)
      .flatMap { target =>
        val sym = target.member(termName(fieldName)).symbol
        Option.when(sym.exists && sym.isTerm && !sym.is(Flags.Synthetic))(ref(sym))
      }

object NavTestPhase:
  val name: String = "pc-navtest-dynamic-nav"

  /** The fixture marker trait the plugin keys on — owned by this plugin, not a
    * third-party API. Test buffers opt in by declaring a Dynamic receiver
    * under exactly this fully-qualified name.
    */
  val ProbeName: String = "lstest.navfixture.NavProbe"
