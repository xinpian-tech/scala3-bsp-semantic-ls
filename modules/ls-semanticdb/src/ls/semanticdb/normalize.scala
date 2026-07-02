package ls.semanticdb

import ls.index.{
  DocId,
  NormalizedDocument,
  Occurrence,
  Role,
  Span,
  SymKind,
  SymbolInfo,
  SymbolKey
}

/** Turns one raw [[SdbDocument]] into the shared [[NormalizedDocument]] model.
  *
  * Rules:
  *   - local symbols become `SymbolKey.local(sym, docId)` with the
  *     caller-supplied DocId (SemanticDB locals are only unique per document);
  *     everything else becomes `SymbolKey.global`.
  *   - ownerName/packageName are derived from the symbol string grammar.
  *   - kind codes map through `SymKind.fromCode`; property bits are kept
  *     verbatim.
  *   - occurrences without a range, with an empty symbol, or with an unknown
  *     role are dropped: they cannot be materialized as exact locations.
  */
object Normalizer:

  def normalize(doc: SdbDocument, docId: DocId): NormalizedDocument =
    def keyOf(sym: String): SymbolKey =
      if SymbolStrings.isLocal(sym) then SymbolKey.local(sym, docId)
      else SymbolKey.global(sym)

    val symbols = doc.symbols.map { s =>
      val display =
        if s.displayName.nonEmpty then s.displayName
        else SymbolStrings.displayName(s.symbol).getOrElse(s.symbol)
      SymbolInfo(
        key = keyOf(s.symbol),
        displayName = display,
        ownerName = SymbolStrings.ownerName(s.symbol),
        packageName = SymbolStrings.packageName(s.symbol),
        kind = SymKind.fromCode(s.kindCode),
        properties = s.properties,
        overriddenSymbols = s.overriddenSymbols.toList
      )
    }

    val occurrences = doc.occurrences.flatMap { o =>
      (o.range, roleOf(o.roleCode)) match
        case (Some(r), Some(role)) if o.symbol.nonEmpty =>
          Some(
            Occurrence(
              key = keyOf(o.symbol),
              span = Span(r.startLine, r.startCharacter, r.endLine, r.endCharacter),
              role = role
            )
          )
        case (_, _) => None
    }

    NormalizedDocument(
      uri = doc.uri,
      md5 = doc.md5,
      schemaVersion = doc.schema,
      language = SdbLanguage.name(doc.languageCode),
      symbols = symbols,
      occurrences = occurrences
    )

  private def roleOf(code: Int): Option[Role] = code match
    case SdbRole.Reference => Some(Role.Reference)
    case SdbRole.Definition => Some(Role.Definition)
    case _ => None
