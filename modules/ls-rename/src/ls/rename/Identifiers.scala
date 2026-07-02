package ls.rename

/** Scala 3 identifier validation for rename targets. A new name is either a
  * plain identifier (used verbatim), or anything backtick-quotable (wrapped
  * in backticks — keywords, spaces, operators mixed with letters, ...), or
  * rejected (empty, contains a backtick or a line break).
  */
object ScalaIdentifiers:

  val keywords: Set[String] = Set(
    "abstract", "case", "catch", "class", "def", "do", "else", "enum",
    "export", "extends", "false", "final", "finally", "for", "given", "if",
    "implicit", "import", "lazy", "match", "new", "null", "object",
    "override", "package", "private", "protected", "return", "sealed",
    "super", "then", "throw", "trait", "true", "try", "type", "val", "var",
    "while", "with", "yield", "_", ":", "=", "<-", "=>", "<:", ">:", "#",
    "@", "=>>", "?=>"
  )

  private def isOpChar(c: Char): Boolean =
    c match
      case '!' | '#' | '%' | '&' | '*' | '+' | '-' | '/' | ':' | '<' | '=' |
          '>' | '?' | '@' | '\\' | '^' | '|' | '~' =>
        true
      case _ =>
        val t = Character.getType(c)
        t == Character.MATH_SYMBOL || t == Character.OTHER_SYMBOL

  private def isIdStart(c: Char): Boolean =
    c == '_' || c == '$' || Character.isUnicodeIdentifierStart(c)

  private def isIdPart(c: Char): Boolean =
    c == '$' || Character.isUnicodeIdentifierPart(c)

  /** Plain identifier per the Scala lexical syntax (simplified): an
    * alphanumeric identifier, optionally `_`-joined with a trailing operator
    * part, or a pure operator identifier.
    */
  def isPlainIdentifier(name: String): Boolean =
    if name.isEmpty then false
    else if name.forall(isOpChar) then true
    else if !isIdStart(name.head) then false
    else
      // letters/digits, optionally ending in '_' + opchars
      val underscore = name.lastIndexOf('_')
      val (alnum, op) =
        if underscore >= 0 && underscore < name.length - 1 &&
          name.substring(underscore + 1).forall(isOpChar) &&
          name.substring(0, underscore + 1).forall(isIdPart)
        then (name.substring(0, underscore + 1), name.substring(underscore + 1))
        else (name, "")
      alnum.forall(isIdPart) && (op.isEmpty || op.forall(isOpChar))

  /** The token to write into source for `name`: verbatim for plain
    * non-keyword identifiers, backtick-quoted when the name demands it,
    * Left(message) when the name cannot be a Scala identifier at all.
    */
  def encode(name: String): Either[String, String] =
    if name.isEmpty then Left("new name must not be empty")
    else if name.exists(c => c == '`' || c == '\n' || c == '\r') then
      Left(s"'$name' is not a valid Scala identifier")
    else if isPlainIdentifier(name) && !keywords.contains(name) then Right(name)
    else Right("`" + name + "`")
