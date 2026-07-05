package pkga

object Rendering:
  def render(using c: Core): String = c.ping
  val rendered: String = render(using defaultCore)
