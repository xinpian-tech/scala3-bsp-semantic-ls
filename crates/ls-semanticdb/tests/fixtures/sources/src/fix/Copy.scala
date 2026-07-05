package fix

case class Pt(x: Int, y: Int)

object CopyUse:
  val p = Pt(1, 2)
  val q = p.copy(x = 3)
