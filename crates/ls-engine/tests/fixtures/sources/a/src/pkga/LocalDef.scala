package pkga

object LocalDefs:
  def countdown(n: Int): Int =
    def loop(m: Int): Int = if m <= 0 then 0 else m + loop(m - 1)
    loop(n)
