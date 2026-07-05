package pkga

class LoudGreeter extends Greeter:
  def greet(name: String): String = "HI " + name

object UseA:
  val core: Core = Core.make("a")
  val loud: String = core.shout
  val g2: Core = defaultCore
