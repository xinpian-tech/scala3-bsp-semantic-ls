package pkgc

class Widget(val size: Int):
  def area: Int = size * size

object Widget:
  def square(n: Int): Widget = new Widget(n)
