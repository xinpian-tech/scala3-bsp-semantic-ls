package pkga

class Core(val label: String):
  def ping: String = "core " + label

object Core:
  def make(l: String): Core = new Core(l)

trait Greeter:
  def greet(name: String): String

enum Color:
  case Red, Green, Blue

extension (c: Core) def shout: String = c.ping.toUpperCase

given defaultCore: Core = Core.make("given")
