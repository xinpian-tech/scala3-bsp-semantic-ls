package pkga

case class Item(id: Int)

object MakeItems:
  val i1 = Item(1)
  val i2 = Item.apply(2)
  val i3 = new Item(3)
