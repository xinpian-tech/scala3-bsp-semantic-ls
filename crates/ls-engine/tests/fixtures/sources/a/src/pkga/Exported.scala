package pkga

object OriginalOwner:
  def exported(n: Int): Int = n + 1

object ForwarderOwner:
  export OriginalOwner.exported

object ExportedUse:
  val r = ForwarderOwner.exported(3)
