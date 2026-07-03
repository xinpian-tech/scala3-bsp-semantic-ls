package pkgb

import pkga.Greeting

object Consumer:
  val greeting: Greeting = Greeting.default
  val text: String = greeting.message
