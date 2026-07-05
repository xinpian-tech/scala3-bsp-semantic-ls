package pkga

object Inlines:
  inline def twice(x: Int): Int = x + x
  val here: Int = twice(1)
