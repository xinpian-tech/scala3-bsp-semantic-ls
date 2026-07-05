package pkga

class Counter:
  var value: Int = 0

object UseCounter:
  def bump(c: Counter): Int =
    val tmp = c.value + 1
    c.value = tmp
    tmp
