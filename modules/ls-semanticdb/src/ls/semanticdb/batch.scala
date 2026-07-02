package ls.semanticdb

import ls.index.{NormalizedDocument, RenameProfile, SymbolKey}

/** The deterministic result of one SemanticDB ingest batch — everything
  * downstream ingestion (SQLite interning + mmap postings, wave 2) consumes:
  *
  *   - `documents`: normalized TextDocuments in input order;
  *   - `groups.refGroups` / `groups.renameGroups`: exact alias groups as
  *     `Vector[Set[SymbolKey]]` with symbol -> group-index maps. Group
  *     indices are batch-local ordinals; wave 2 assigns persistent
  *     ref_group_id / rename_group_id when interning;
  *   - `renameProfiles`: one [[ls.index.RenameProfile]] per rename group,
  *     aligned with `groups.renameGroups`.
  *
  * Everything is immutable and order-deterministic so repeated ingests of the
  * same corpus produce identical output (plan Phase 3 acceptance).
  */
final case class SemanticBatch(
    documents: Vector[NormalizedDocument],
    groups: AliasGroups,
    renameProfiles: Vector[RenameProfile]
):
  def refGroupOf(key: SymbolKey): Option[Int] = groups.refGroupOf(key)
  def renameGroupOf(key: SymbolKey): Option[Int] = groups.renameGroupOf(key)

  def renameProfileOf(key: SymbolKey): Option[RenameProfile] =
    groups.renameGroupOf(key).map(renameProfiles(_))

object SemanticBatch:
  /** Builds groups and rename profiles for a batch of normalized documents.
    * `docFacts` carries per-uri generated/readonly/dependency knowledge;
    * missing uris are treated as plain editable workspace sources.
    */
  def assemble(
      documents: Vector[NormalizedDocument],
      docFacts: Map[String, DocFacts] = Map.empty
  ): SemanticBatch =
    val groups = AliasGroupBuilder.build(documents)
    val profiles = RenameProfileBuilder.build(documents, groups, docFacts)
    SemanticBatch(documents, groups, profiles)
