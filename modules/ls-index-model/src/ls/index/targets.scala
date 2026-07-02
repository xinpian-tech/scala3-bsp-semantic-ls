package ls.index

/** Exact membership set over snapshot target ordinals (a bitset, not a
  * probabilistic filter). Used for target-graph pruning and block skip.
  */
final class TargetBitset private (private val words: Array[Long], val size: Int):
  def contains(targetOrd: Int): Boolean =
    targetOrd >= 0 && targetOrd < size &&
      (words(targetOrd >>> 6) & (1L << (targetOrd & 63))) != 0

  def intersects(other: TargetBitset): Boolean =
    val n = math.min(words.length, other.words.length)
    var i = 0
    while i < n do
      if (words(i) & other.words(i)) != 0 then return true
      i += 1
    false

  /** Intersects against a raw word array (e.g. a block's target bitset read
    * straight out of an mmap segment).
    */
  def intersectsWords(otherWords: Array[Long]): Boolean =
    val n = math.min(words.length, otherWords.length)
    var i = 0
    while i < n do
      if (words(i) & otherWords(i)) != 0 then return true
      i += 1
    false

  def toWords: Array[Long] = words.clone()

  def cardinality: Int =
    var c = 0
    var i = 0
    while i < words.length do
      c += java.lang.Long.bitCount(words(i))
      i += 1
    c

object TargetBitset:
  def empty(size: Int): TargetBitset =
    new TargetBitset(new Array[Long]((size + 63) >>> 6), size)

  def of(size: Int, ords: Iterable[Int]): TargetBitset =
    val words = new Array[Long](math.max(1, (size + 63) >>> 6))
    for o <- ords do
      require(o >= 0 && o < size, s"target ordinal $o out of range [0,$size)")
      words(o >>> 6) |= 1L << (o & 63)
    new TargetBitset(words, size)

  def fromWords(size: Int, words: Array[Long]): TargetBitset =
    new TargetBitset(words, size)

  def all(size: Int): TargetBitset = of(size, 0 until size)

/** Build-target dependency graph from BSP, with reverse-closure queries.
  * Nodes are persistent target ids; per-snapshot pruning converts to
  * ordinals via the snapshot dictionary.
  */
final class TargetGraph(val targets: Vector[TargetId], edges: Map[TargetId, Set[TargetId]]):
  /** direct dependencies: target -> targets it depends on */
  def dependenciesOf(t: TargetId): Set[TargetId] = edges.getOrElse(t, Set.empty)

  private lazy val reverseEdges: Map[TargetId, Set[TargetId]] =
    val builder = collection.mutable.Map.empty[TargetId, Set[TargetId]]
    for
      (from, tos) <- edges
      to <- tos
    do builder.updateWith(to)(prev => Some(prev.getOrElse(Set.empty) + from))
    builder.toMap

  def dependentsOf(t: TargetId): Set[TargetId] =
    reverseEdges.getOrElse(t, Set.empty)

  /** T + all targets that transitively depend on T: the exact upper bound of
    * targets that can reference a symbol defined in T.
    */
  def reverseDependencyClosure(t: TargetId): Set[TargetId] =
    val seen = collection.mutable.Set(t)
    val queue = collection.mutable.Queue(t)
    while queue.nonEmpty do
      val cur = queue.dequeue()
      for dep <- dependentsOf(cur) if seen.add(dep) do queue.enqueue(dep)
    seen.toSet

object TargetGraph:
  val empty: TargetGraph = new TargetGraph(Vector.empty, Map.empty)
