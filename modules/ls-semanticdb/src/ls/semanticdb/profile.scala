package ls.semanticdb

import ls.index.{NormalizedDocument, RenameProfile, Role, SymbolKey, UnsafeReason}

/** Facts about one document that only the ingest orchestration layer knows
  * (from BSP/build metadata), keyed by `TextDocument.uri`.
  */
final case class DocFacts(
    generated: Boolean,
    readonly: Boolean,
    isDependencySource: Boolean
):
  /** A rename edit may touch this document. */
  def editable: Boolean = !generated && !readonly && !isDependencySource

object DocFacts:
  /** Plain editable workspace source — the default when no facts are known. */
  val workspaceSource: DocFacts = DocFacts(generated = false, readonly = false, isDependencySource = false)

/** Computes one [[ls.index.RenameProfile]] per rename group (plan 13.2).
  *
  * Semantics:
  *   - isLocal: every member is a local symbol;
  *   - isExternal: no Definition-role occurrence of any member anywhere in
  *     the batch — the definition lives outside the workspace;
  *   - hasGenerated/hasReadonly: some occurrence lands in a generated /
  *     readonly document;
  *   - editableOccurrenceCount: occurrences in documents that are neither
  *     generated, readonly nor dependency sources;
  *   - unsafeReasonMask: the group's semantic mask (OverrideFamily) plus
  *     External / GeneratedOccurrence / ReadonlyOccurrence /
  *     DependencySource bits. Generated occurrences make a group unsafe by
  *     default per plan 13.1 ("no generated sources by default").
  */
object RenameProfileBuilder:

  def build(
      docs: Vector[NormalizedDocument],
      groups: AliasGroups,
      docFacts: Map[String, DocFacts]
  ): Vector[RenameProfile] =
    val n = groups.renameGroups.length
    val hasDefinition = new Array[Boolean](n)
    val hasGenerated = new Array[Boolean](n)
    val hasReadonly = new Array[Boolean](n)
    val hasDependency = new Array[Boolean](n)
    val editableCount = new Array[Int](n)

    for doc <- docs do
      val facts = docFacts.getOrElse(doc.uri, DocFacts.workspaceSource)
      for occ <- doc.occurrences do
        groups.renameGroupIndex.get(occ.key) match
          case Some(gi) =>
            occ.role match
              case Role.Definition => hasDefinition(gi) = true
              case Role.Reference => ()
            if facts.generated then hasGenerated(gi) = true
            if facts.readonly then hasReadonly(gi) = true
            if facts.isDependencySource then hasDependency(gi) = true
            if facts.editable then editableCount(gi) += 1
          case None => ()

    groups.renameGroups.iterator.zipWithIndex.map { (group, gi) =>
      val isLocal = group.nonEmpty && group.forall(_.isLocal)
      val isExternal = !hasDefinition(gi)
      val semanticMask = groups.renameGroupSemanticMask(gi)
      val hasOverrideFamily = (semanticMask & UnsafeReason.OverrideFamily) != 0L
      val hasCompanion = group.exists { k =>
        !k.isLocal && k.semanticSymbol.endsWith("#") &&
          SymbolStrings
            .companion(k.semanticSymbol)
            .exists(c => group.contains(SymbolKey.global(c)))
      }
      var mask = semanticMask
      if isExternal then mask |= UnsafeReason.External
      if hasGenerated(gi) then mask |= UnsafeReason.GeneratedOccurrence
      if hasReadonly(gi) then mask |= UnsafeReason.ReadonlyOccurrence
      if hasDependency(gi) then mask |= UnsafeReason.DependencySource
      RenameProfile(
        isLocal = isLocal,
        isExternal = isExternal,
        hasGeneratedOccurrences = hasGenerated(gi),
        hasReadonlyOccurrences = hasReadonly(gi),
        hasOverrideFamily = hasOverrideFamily,
        hasCompanion = hasCompanion,
        editableOccurrenceCount = editableCount(gi),
        unsafeReasonMask = mask
      )
    }.toVector
