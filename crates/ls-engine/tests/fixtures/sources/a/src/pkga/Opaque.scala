package pkga

object Ids:
  opaque type UserId = Long
  object UserId:
    def wrap(l: Long): UserId = l
  val sample: UserId = UserId.wrap(7L)
