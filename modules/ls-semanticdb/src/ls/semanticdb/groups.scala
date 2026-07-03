package ls.semanticdb

import ls.index.{NormalizedDocument, Role, SymbolKey, SymKind, SymProps, UnsafeReason}
import scala.collection.mutable

/** Exact alias groups for one ingest batch.
  *
  * `refGroups` drive `textDocument/references`; `renameGroups` drive
  * cross-file rename. In the v1 policy the two partitions are identical, but
  * they are separate fields because they are expected to diverge (rename must
  * stay conservative while references can merge more).
  *
  * Group vectors are deterministic: ordered by their minimal member under
  * (semanticSymbol, localDocId) ordering.
  */
final case class AliasGroups(
    refGroups: Vector[Set[SymbolKey]],
    refGroupIndex: Map[SymbolKey, Int],
    renameGroups: Vector[Set[SymbolKey]],
    renameGroupIndex: Map[SymbolKey, Int],
    /** Pure-semantic unsafe bits per rename group, computable without
      * document facts. Currently only [[UnsafeReason.OverrideFamily]];
      * doc-dependent bits (generated/readonly/external/dependency) are added
      * by [[RenameProfileBuilder]].
      */
    renameGroupSemanticMask: Vector[Long]
):
  def refGroupOf(key: SymbolKey): Option[Int] = refGroupIndex.get(key)
  def renameGroupOf(key: SymbolKey): Option[Int] = renameGroupIndex.get(key)

/** Builds EXACT alias groups by union-find over every SymbolKey seen in the
  * batch (symbols and occurrences).
  *
  * v1 merge policy (applied to BOTH ref and rename groups):
  *   - class/trait/enum `X#` merges with ALL its constructors
  *     `X#`<init>`(...)` (the class key is synthesized when only the
  *     constructor is present, e.g. a batch that only contains `new X`);
  *   - `X#` merges with its companion object `X.` when present in the batch;
  *   - companion `apply`/`unapply` methods (`X.apply(...).`,
  *     `X.unapply(...).`) merge into the class group when the class `X#` is
  *     present — their call-site token is the class name. Plain objects
  *     without a companion class do NOT merge with their apply methods;
  *   - a setter `x_=(...)` merges with its getter `x().` and with a val/var
  *     field term `x.` when those keys are present;
  *   - method overloads (same name, different disambiguator) stay in
  *     separate groups;
  *   - local symbols never merge.
  */
object AliasGroupBuilder:

  private val keyOrdering: Ordering[SymbolKey] =
    Ordering.by((k: SymbolKey) => (k.semanticSymbol, k.localDoc.fold(-1L)(_.value)))

  def build(docs: Vector[NormalizedDocument]): AliasGroups =
    // 1. Deterministic key universe + first-wins SymbolInfo lookup.
    val keySet = mutable.LinkedHashSet.empty[SymbolKey]
    val infoByKey = mutable.HashMap.empty[SymbolKey, ls.index.SymbolInfo]
    for doc <- docs do
      for s <- doc.symbols do
        keySet += s.key
        if !infoByKey.contains(s.key) then infoByKey.update(s.key, s)
      for o <- doc.occurrences do keySet += o.key

    // Constructors unambiguously imply their class symbol: synthesize the
    // class key so `new X` occurrences group with `X#` even when the class
    // is otherwise absent from this batch.
    val implied = mutable.LinkedHashSet.empty[SymbolKey]
    for key <- keySet if !key.isLocal do
      if SymbolStrings.isConstructor(key.semanticSymbol) then
        SymbolStrings.splitLast(key.semanticSymbol) match
          case Some((owner, _)) if owner.endsWith("#") =>
            implied += SymbolKey.global(owner)
          case Some(_) => ()
          case None => ()
    keySet ++= implied

    val keys = keySet.toVector.sorted(using keyOrdering)
    val indexOf: Map[SymbolKey, Int] = keys.iterator.zipWithIndex.toMap

    val uf = new UnionFind(keys.length)

    def unionWithGlobal(from: Int, toSymbol: String): Unit =
      indexOf.get(SymbolKey.global(toSymbol)) match
        case Some(to) => uf.union(from, to)
        case None => ()

    // 2. Apply the merge policy over global keys.
    for (key, i) <- keys.iterator.zipWithIndex if !key.isLocal do
      val sym = key.semanticSymbol
      SymbolStrings.splitLast(sym) match
        case Some((owner, SymbolStrings.Descriptor.Method(name, _))) =>
          if name == SymbolStrings.ConstructorName then
            if owner.endsWith("#") then unionWithGlobal(i, owner)
          else if name == "apply" || name == "unapply" then
            if owner.endsWith(".") then
              SymbolStrings.companion(owner) match
                case Some(cls) if indexOf.contains(SymbolKey.global(cls)) =>
                  unionWithGlobal(i, cls)
                case Some(_) => ()
                case None => ()
          else if name.length > 2 && name.endsWith("_=") then
            val base = SymbolStrings.encodeName(name.dropRight(2))
            unionWithGlobal(i, owner + base + "().")
            unionWithGlobal(i, owner + base + ".")
        case Some((owner, SymbolStrings.Descriptor.Type(name))) =>
          unionWithGlobal(i, owner + SymbolStrings.encodeName(name) + ".")
        case Some(_) => ()
        case None => ()

    // 2b. Export forwarders. Scala 3 `export B.m` synthesizes a forwarder
    //     method `A.m` in the exporting object: it carries a Method symbol but
    //     NO definition occurrence (no defining source token — the source shows
    //     `export B.m`), while the real `B.m` has one. So a Method symbol with
    //     no definition occurrence whose descriptor matches a definition-having
    //     Method under a different owner is a forwarder for that method. Union
    //     the forwarder into the original's group and remember it so the group
    //     is flagged UnsupportedSymbolFamily (exports are not safely renameable
    //     — a rename would have to touch the export clause and every forwarder).
    val definedKeys: Set[SymbolKey] =
      docs.iterator
        .flatMap(_.occurrences)
        .filter(_.role == Role.Definition)
        .map(_.key)
        .toSet
    def descriptorOf(sym: String): Option[(String, String)] =
      SymbolStrings.splitLast(sym).map((owner, _) => (owner, sym.substring(owner.length)))
    // definition-having methods indexed by their descriptor (name + signature)
    val definedMethodByDescriptor: Map[String, Vector[SymbolKey]] =
      keys.iterator
        .filter(k => !k.isLocal && definedKeys.contains(k))
        .filter(k => infoByKey.get(k).exists(_.kind == SymKind.Method))
        .flatMap(k => descriptorOf(k.semanticSymbol).map { case (owner, desc) => (desc, owner, k) })
        .toVector
        .groupMap(_._1)(t => t._3)
    val forwarderKeys = mutable.LinkedHashSet.empty[SymbolKey]
    for (key, i) <- keys.iterator.zipWithIndex if !key.isLocal do
      if infoByKey.get(key).exists(_.kind == SymKind.Method) && !definedKeys.contains(key) then
        descriptorOf(key.semanticSymbol) match
          case Some((owner, desc)) =>
            definedMethodByDescriptor.getOrElse(desc, Vector.empty).find { orig =>
              descriptorOf(orig.semanticSymbol).exists(_._1 != owner)
            } match
              case Some(orig) =>
                forwarderKeys += key
                uf.union(i, indexOf(orig))
              case None => ()
          case None => ()

    // 3. Assemble groups; iteration over sorted keys keeps output ordered by
    //    minimal member.
    val byRoot = mutable.LinkedHashMap.empty[Int, mutable.ArrayBuffer[SymbolKey]]
    for (key, i) <- keys.iterator.zipWithIndex do
      byRoot.getOrElseUpdate(uf.find(i), mutable.ArrayBuffer.empty[SymbolKey]) += key
    val groups: Vector[Set[SymbolKey]] = byRoot.valuesIterator.map(_.toSet).toVector
    val groupIndex: Map[SymbolKey, Int] =
      groups.iterator.zipWithIndex.flatMap((g, gi) => g.iterator.map(_ -> gi)).toMap

    // 4. OverrideFamily: a group is flagged when any member declares
    //    overridden symbols OR is itself overridden by another symbol in the
    //    batch (reverse map over overridden_symbols).
    val overriddenTargets: Set[SymbolKey] =
      infoByKey.valuesIterator
        .flatMap(_.overriddenSymbols.iterator)
        .map(SymbolKey.global)
        .toSet
    val semanticMask: Vector[Long] = groups.map { g =>
      var mask = 0L
      val overrideFlagged = g.exists { k =>
        infoByKey.get(k).exists(_.overriddenSymbols.nonEmpty) || overriddenTargets.contains(k)
      }
      if overrideFlagged then mask |= UnsafeReason.OverrideFamily
      if g.exists(forwarderKeys.contains) then mask |= UnsafeReason.UnsupportedSymbolFamily
      // Opaque types: rename is conservatively rejected (resolved policy). An
      // `opaque type T` merges with its companion and all uses, but renaming it
      // cannot be proven safe in v1, so the group is flagged unsafe outright.
      if g.exists(k => infoByKey.get(k).exists(i => (i.properties & SymProps.Opaque) != 0)) then
        mask |= UnsafeReason.OpaqueType
      mask
    }

    AliasGroups(
      refGroups = groups,
      refGroupIndex = groupIndex,
      renameGroups = groups,
      renameGroupIndex = groupIndex,
      renameGroupSemanticMask = semanticMask
    )

  /** Classic union-find with path compression and union by rank. */
  private final class UnionFind(size: Int):
    private val parent = Array.tabulate(size)(identity)
    private val rank = new Array[Int](size)

    def find(i: Int): Int =
      var root = i
      while parent(root) != root do root = parent(root)
      var cur = i
      while parent(cur) != root do
        val next = parent(cur)
        parent(cur) = root
        cur = next
      root

    def union(a: Int, b: Int): Unit =
      val ra = find(a)
      val rb = find(b)
      if ra != rb then
        if rank(ra) < rank(rb) then parent(ra) = rb
        else if rank(ra) > rank(rb) then parent(rb) = ra
        else
          parent(rb) = ra
          rank(ra) += 1
