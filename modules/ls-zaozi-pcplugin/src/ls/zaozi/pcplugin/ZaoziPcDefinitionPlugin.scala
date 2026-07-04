package ls.zaozi.pcplugin

import scala.util.control.NonFatal

import dotty.tools.dotc.ast.tpd.*
import dotty.tools.dotc.core.Constants.Constant
import dotty.tools.dotc.core.Contexts.Context
import dotty.tools.dotc.core.Decorators.*
import dotty.tools.dotc.core.Names.termName
import dotty.tools.dotc.core.Symbols.*
import dotty.tools.dotc.core.Types.*
import dotty.tools.dotc.plugins.{PluginPhase, StandardPlugin}

/** Presentation-compiler plugin that makes go-to-definition and hover on a
  * zaozi dynamic bundle-field access resolve to the real field declaration.
  *
  * A `Referable[T]` (`scala.Dynamic`) access `io.a` is a
  * `transparent inline selectDynamic("a")` whose expansion drops the field name
  * to a runtime string; the retained pre-inlining call `io.selectDynamic("a")`
  * carries only the framework method symbol, so the compiler resolves `io.a` to
  * `selectDynamic` rather than `val a`. This plugin runs after `typer` (the
  * inline expansion already happened there) and structurally rewrites the
  * `Inlined.call` to a typed reference to the resolved field symbol, so the
  * interactive symbol-at-cursor lookup returns the field.
  *
  * It keys strictly on the zaozi API (receiver derives from
  * `me.jiuyang.zaozi.reftpe.Referable`, field owner from
  * `me.jiuyang.zaozi.magic.DynamicSubfield`), so it is inert everywhere else,
  * and every step is guarded so it can never fail an interactive request.
  */
class ZaoziPcDefinitionPlugin extends StandardPlugin:
  val name: String = "zaozi-pc-nav"
  override val description: String =
    "Resolve go-to and hover on zaozi Dynamic bundle-field accesses to the field declaration."

  override def initialize(options: List[String])(using Context): List[PluginPhase] =
    List(new ZaoziPcNavPhase)

class ZaoziPcNavPhase extends PluginPhase:
  import ZaoziPcNavPhase.*

  val phaseName: String = ZaoziPcNavPhase.name

  // Anchor on phases the interactive presentation compiler actually schedules
  // (it runs only parser, typer, SetRootTree, cookComments); posttyper/inlining
  // are absent there and would mis-schedule this phase.
  override val runsAfter: Set[String] = Set("typer")
  override val runsBefore: Set[String] = Set("SetRootTree")

  override def transformInlined(tree: Inlined)(using Context): Tree =
    try rewriteDynamicNav(tree)
    catch case NonFatal(_) => tree

  private def rewriteDynamicNav(tree: Inlined)(using Context): Tree =
    dynamicFieldAccess(tree.call) match
      case Some((qual, fieldName)) =>
        resolveFieldSymbol(qual, fieldName) match
          case Some(fieldSym) =>
            cpy.Inlined(tree)(ref(fieldSym).withSpan(tree.call.span), tree.bindings, tree.expansion)
          case None => tree
      case None => tree

  /** `(receiver, fieldName)` of a retained `qual.selectDynamic("field")` call. */
  private def dynamicFieldAccess(call: Tree)(using Context): Option[(Tree, String)] =
    call match
      case Apply(Select(qual, sel), List(Literal(Constant(field: String))))
          if sel.toString == "selectDynamic" =>
        Some((qual, field))
      case _ => None

  /** Resolve `field` to its declaration in the bundle type of a
    * `Referable[T <: DynamicSubfield]` receiver, or `None` if the receiver is
    * not a zaozi referable or the field is not a real member.
    */
  private def resolveFieldSymbol(qual: Tree, fieldName: String)(using Context): Option[Symbol] =
    val receiverType = qual.tpe.widen
    receiverType.baseClasses.find(_.fullName.toString == ReferableName).flatMap { referable =>
      receiverType.baseType(referable).argInfos.headOption.flatMap { bundleType =>
        if bundleType.baseClasses.exists(_.fullName.toString == DynamicSubfieldName) then
          val sym = bundleType.member(termName(fieldName)).symbol
          if sym.exists && sym.isTerm then Some(sym) else None
        else None
      }
    }

object ZaoziPcNavPhase:
  val name: String = "zaozi-pc-dynamic-nav"
  private val ReferableName = "me.jiuyang.zaozi.reftpe.Referable"
  private val DynamicSubfieldName = "me.jiuyang.zaozi.magic.DynamicSubfield"
