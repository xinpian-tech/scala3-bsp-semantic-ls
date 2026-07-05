package fix

trait Animal:
  def sound: String

class Dog extends Animal:
  def sound: String = "woof"
