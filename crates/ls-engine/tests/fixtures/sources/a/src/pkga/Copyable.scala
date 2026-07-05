package pkga

case class Copyable(id: Int)

object CopyableUse:
  val c = Copyable(1)
  val d = c.copy(id = 2)
