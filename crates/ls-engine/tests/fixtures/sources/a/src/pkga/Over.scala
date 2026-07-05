package pkga

object Over:
  def fmt(i: Int): String = i.toString
  def fmt(s: String): String = s
  val a = fmt(1)
  val b = fmt("x")
