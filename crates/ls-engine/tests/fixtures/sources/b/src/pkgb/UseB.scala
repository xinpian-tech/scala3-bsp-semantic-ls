package pkgb

import pkga.*

object UseB:
  val core: Core = Core.make("b")
  val item: Item = Item(42)
  def greetAll(g: Greeter): String = g.greet("b")
  val color: Color = Color.Green
  def loud(c: Core): String = c.shout
  val g2: Core = pkga.defaultCore
  val s: String = shared.SharedThing.tag
  val twiced: Int = pkga.Inlines.twice(21)
  val named: Named = Named("b")
  val theTitle: String = named.title
  val topUse: Int = pkga.topHelper(pkga.topConst)
