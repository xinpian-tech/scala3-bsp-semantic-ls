package pkga

class Secretive:
  private val state: Int = 1
  private def helper(n: Int): Int = n + state
  def use: Int = helper(state)
