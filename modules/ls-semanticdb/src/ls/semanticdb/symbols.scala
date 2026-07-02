package ls.semanticdb

/** Helpers over the SemanticDB symbol grammar (scalameta spec, "Symbol"):
  *
  * {{{
  * Symbol       = GlobalSymbol | LocalSymbol
  * LocalSymbol  = "local" Number
  * GlobalSymbol = Owner Descriptor
  * Descriptor   = Name "."                  // term (object/val/field)
  *              | Name Disambiguator "."    // method, e.g. f(). f(+1).
  *              | Name "#"                  // type (class/trait/type)
  *              | Name "/"                  // package
  *              | "(" Name ")"              // parameter
  *              | "[" Name "]"              // type parameter
  * Name         = identifier | "`" anything "`"
  * }}}
  *
  * Parsing works backwards from the end of the string, so backticked names
  * containing descriptor characters are handled correctly.
  */
object SymbolStrings:

  final val RootPackage = "_root_/"
  final val EmptyPackage = "_empty_/"
  final val ConstructorName = "<init>"

  /** The last descriptor of a global symbol, carrying its decoded name. */
  enum Descriptor(val name: String):
    case Term(termName: String) extends Descriptor(termName)
    case Method(methodName: String, disambiguator: String) extends Descriptor(methodName)
    case Type(typeName: String) extends Descriptor(typeName)
    case Package(packageName: String) extends Descriptor(packageName)
    case Parameter(parameterName: String) extends Descriptor(parameterName)
    case TypeParameter(typeParameterName: String) extends Descriptor(typeParameterName)

  /** Local symbols are `local` + id and only meaningful inside one document. */
  def isLocal(symbol: String): Boolean =
    symbol.length > 5 && symbol.startsWith("local") &&
      !symbol.exists(isNameBoundary)

  def isGlobal(symbol: String): Boolean = symbol.nonEmpty && !isLocal(symbol)

  def isPackage(symbol: String): Boolean = symbol.nonEmpty && symbol.last == '/'

  private def isNameBoundary(c: Char): Boolean = c match
    case '/' | '.' | '#' | '(' | ')' | '[' | ']' | '`' => true
    case _ => false

  /** Encodes a display name into descriptor syntax, mirroring scalameta:
    * backticks unless the name is a plain Java-style identifier.
    */
  def encodeName(name: String): String =
    if name.isEmpty then "``"
    else
      val plain =
        Character.isJavaIdentifierStart(name.head) &&
          name.forall(Character.isJavaIdentifierPart)
      if plain then name else "`" + name + "`"

  /** Reads a (possibly backticked) name ending at `endIdx` inclusive.
    * Returns the start index of the name (including the opening backtick)
    * and the decoded name.
    */
  private def readNameBackwards(s: String, endIdx: Int): Option[(Int, String)] =
    if endIdx < 0 then None
    else if s(endIdx) == '`' then
      val open = s.lastIndexOf('`', endIdx - 1)
      if open < 0 then None
      else Some((open, s.substring(open + 1, endIdx)))
    else
      var i = endIdx
      while i >= 0 && !isNameBoundary(s(i)) do i -= 1
      if i == endIdx then None // empty name
      else Some((i + 1, s.substring(i + 1, endIdx + 1)))

  /** Splits a global symbol into (owner prefix, last descriptor).
    * None for local, empty, or malformed symbols.
    */
  def splitLast(symbol: String): Option[(String, Descriptor)] =
    if symbol.isEmpty || isLocal(symbol) then None
    else
      val n = symbol.length
      symbol.charAt(n - 1) match
        case '/' =>
          readNameBackwards(symbol, n - 2).map { (start, name) =>
            (symbol.substring(0, start), Descriptor.Package(name))
          }
        case '#' =>
          readNameBackwards(symbol, n - 2).map { (start, name) =>
            (symbol.substring(0, start), Descriptor.Type(name))
          }
        case '.' =>
          if n >= 2 && symbol.charAt(n - 2) == ')' then
            // Method: name , disambiguator "(...)" , "." — the disambiguator
            // never contains parens, so the nearest '(' closes it.
            val open = symbol.lastIndexOf('(', n - 2)
            if open < 0 then None
            else
              val disambiguator = symbol.substring(open, n - 1)
              readNameBackwards(symbol, open - 1).map { (start, name) =>
                (symbol.substring(0, start), Descriptor.Method(name, disambiguator))
              }
          else
            readNameBackwards(symbol, n - 2).map { (start, name) =>
              (symbol.substring(0, start), Descriptor.Term(name))
            }
        case ')' =>
          readNameBackwards(symbol, n - 2).flatMap { (start, name) =>
            if start > 0 && symbol.charAt(start - 1) == '(' then
              Some((symbol.substring(0, start - 1), Descriptor.Parameter(name)))
            else None
          }
        case ']' =>
          readNameBackwards(symbol, n - 2).flatMap { (start, name) =>
            if start > 0 && symbol.charAt(start - 1) == '[' then
              Some((symbol.substring(0, start - 1), Descriptor.TypeParameter(name)))
            else None
          }
        case _ => None

  def descriptorOf(symbol: String): Option[Descriptor] = splitLast(symbol).map(_._2)

  /** Decoded name of the last descriptor. None for locals (their display name
    * only exists in SymbolInformation) and malformed symbols.
    */
  def displayName(symbol: String): Option[String] = descriptorOf(symbol).map(_.name)

  /** Owner prefix, None when the symbol is top-level (owner is the root). */
  def owner(symbol: String): Option[String] =
    splitLast(symbol).map(_._1).filter(_.nonEmpty)

  /** All enclosing symbols from outermost to the symbol itself (inclusive).
    * For locals, just the local symbol.
    */
  def ownerChain(symbol: String): List[String] =
    if symbol.isEmpty then Nil
    else if isLocal(symbol) then symbol :: Nil
    else
      var acc: List[String] = Nil
      var cur = symbol
      var continue = true
      while continue && cur.nonEmpty do
        acc = cur :: acc
        splitLast(cur) match
          case Some((ownerPrefix, _)) => cur = ownerPrefix
          case None => continue = false
      acc

  /** Dotted package path of the strictly enclosing packages. None for locals,
    * the root package and the empty package.
    */
  def packageName(symbol: String): Option[String] =
    val enclosing = ownerChain(symbol).dropRight(1)
    val names = enclosing
      .takeWhile(isPackage)
      .flatMap(displayName)
      .filter(n => n != "_root_" && n != "_empty_")
    if names.isEmpty then None else Some(names.mkString("."))

  /** Display name of the nearest enclosing non-package declaration. None when
    * the symbol sits directly in a package, and for locals.
    */
  def ownerName(symbol: String): Option[String] =
    ownerChain(symbol)
      .dropRight(1)
      .reverse
      .collectFirst { case s if !isPackage(s) => s }
      .flatMap(displayName)

  /** Companion symbol per the grammar: `X#` <-> `X.`. Only meaningful when
    * the counterpart actually exists; existence is the caller's concern.
    */
  def companion(symbol: String): Option[String] =
    splitLast(symbol).flatMap {
      case (ownerPrefix, Descriptor.Type(name)) =>
        Some(ownerPrefix + encodeName(name) + ".")
      case (ownerPrefix, Descriptor.Term(name)) =>
        Some(ownerPrefix + encodeName(name) + "#")
      case _ => None
    }

  def isCompanionPair(a: String, b: String): Boolean =
    companion(a).contains(b)

  def isConstructor(symbol: String): Boolean =
    descriptorOf(symbol) match
      case Some(Descriptor.Method(name, _)) => name == ConstructorName
      case _ => false

  def isSetter(symbol: String): Boolean =
    descriptorOf(symbol) match
      case Some(Descriptor.Method(name, _)) => name.length > 2 && name.endsWith("_=")
      case _ => false

  /** For a setter `x_=(...)` the plain name `x` of its getter/field. */
  def setterTargetName(symbol: String): Option[String] =
    descriptorOf(symbol) match
      case Some(Descriptor.Method(name, _)) if name.length > 2 && name.endsWith("_=") =>
        Some(name.dropRight(2))
      case _ => None

  /** The outermost non-package enclosing symbol — the top-level class, trait
    * or object containing this symbol (the symbol itself when top-level).
    * None for packages and locals.
    */
  def enclosingTopLevel(symbol: String): Option[String] =
    ownerChain(symbol).find(s => !isPackage(s) && !isLocal(s))
