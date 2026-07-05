package fix

object Use:
  val a: Person = Person("x")
  val b = new Person("y")
  val c = Person.apply("z")
  def show(p: Person): String = p.greet
