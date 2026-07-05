package fix

case class Person(name: String):
  def greet: String = "hi " + name

object Person:
  def default: Person = new Person("d")
