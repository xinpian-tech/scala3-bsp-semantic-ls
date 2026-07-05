package fix

class Box:
  var value: Int = 0

object BoxUse:
  def bump(b: Box): Unit =
    val tmp = b.value + 1
    b.value = tmp
