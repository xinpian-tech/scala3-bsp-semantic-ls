package fix

object Over:
  def f(i: Int): Int = i
  def f(s: String): String = s
  val x = f(1)
  val y = f("s")
