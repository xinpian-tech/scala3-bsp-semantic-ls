package pkga

class Core(val label: String):
  def ping: String = "c " + label

object Core:
  def make(l: String): Core = new Core(l)

object UseC:
  val core: Core = Core.make("c")
