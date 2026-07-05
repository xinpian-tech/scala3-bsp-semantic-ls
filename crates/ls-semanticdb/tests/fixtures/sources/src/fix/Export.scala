package fix

object Impl:
  def work(x: Int): Int = x

object Api:
  export Impl.work

object ExportUse:
  val use = Api.work(1)
