package pkga

class Greeting(val name: String):
  def message: String = "hello " + name

object Greeting:
  def default: Greeting = new Greeting("world")
