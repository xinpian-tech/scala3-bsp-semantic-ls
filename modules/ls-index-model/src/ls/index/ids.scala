package ls.index

/** Persistent identifiers live in SQLite and survive snapshots.
  * Snapshot ordinals are dense ints valid only within one IndexSnapshot,
  * used for O(1) array lookup on the query hot path.
  */

opaque type SymbolId = Long
object SymbolId:
  def apply(v: Long): SymbolId = v
  extension (id: SymbolId) def value: Long = id

opaque type DocId = Long
object DocId:
  def apply(v: Long): DocId = v
  extension (id: DocId) def value: Long = id

opaque type TargetId = Long
object TargetId:
  def apply(v: Long): TargetId = v
  extension (id: TargetId) def value: Long = id

opaque type RefGroupId = Long
object RefGroupId:
  def apply(v: Long): RefGroupId = v
  extension (id: RefGroupId) def value: Long = id

opaque type RenameGroupId = Long
object RenameGroupId:
  def apply(v: Long): RenameGroupId = v
  extension (id: RenameGroupId) def value: Long = id

opaque type SymbolOrd = Int
object SymbolOrd:
  def apply(v: Int): SymbolOrd = v
  extension (o: SymbolOrd) def ord: Int = o

opaque type DocOrd = Int
object DocOrd:
  def apply(v: Int): DocOrd = v
  extension (o: DocOrd) def ord: Int = o

opaque type TargetOrd = Int
object TargetOrd:
  def apply(v: Int): TargetOrd = v
  extension (o: TargetOrd) def ord: Int = o

opaque type RefGroupOrd = Int
object RefGroupOrd:
  def apply(v: Int): RefGroupOrd = v
  extension (o: RefGroupOrd) def ord: Int = o

opaque type RenameGroupOrd = Int
object RenameGroupOrd:
  def apply(v: Int): RenameGroupOrd = v
  extension (o: RenameGroupOrd) def ord: Int = o
