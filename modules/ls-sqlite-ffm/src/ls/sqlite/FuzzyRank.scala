package ls.sqlite

/** In-memory camel-hump / subsequence ranker for the workspace-symbol fuzzy
  * fallback (plan §11, schema v2). The normalized name and camel-hump initials
  * are stored in the `workspace_symbol_fuzzy` sidecar so the bounded candidate
  * pull can index-filter by first character; the final ranking runs here, over
  * each candidate's display name, and is NOT FTS5 trigram.
  */
private[sqlite] object FuzzyRank:

  // Score tiers (higher is better). Length is subtracted as a "tightness" proxy
  // so a shorter name ranks above a longer one at the same tier / hump count.
  private inline val ExactBase = 1_000_000
  private inline val PrefixBase = 100_000
  private inline val SubseqBase = 1_000
  private inline val HumpBonus = 1_000

  /** Lowercase, keep only letters/digits (drops separators/punctuation). CJK and
    * other BMP letters are kept (`isLetterOrDigit`), matching the FTS tokenizer.
    */
  def normalize(s: String): String =
    val sb = new StringBuilder(s.length)
    var i = 0
    while i < s.length do
      val c = s.charAt(i)
      if Character.isLetterOrDigit(c) then sb.append(Character.toLowerCase(c))
      i += 1
    sb.toString

  /** Camel-hump initials, lowercased: the first alnum char, a char after a
    * separator, an uppercase char after a lowercase/digit, and a digit after a
    * letter each start a new hump. e.g. `workspaceSymbol` -> `ws`.
    */
  def initials(s: String): String =
    val (nn, humps) = normalizedWithHumps(s)
    val sb = new StringBuilder
    var i = 0
    while i < nn.length do
      if humps(i) then sb.append(nn.charAt(i))
      i += 1
    sb.toString

  /** Fuzzy score of `query` against a candidate `name`, or None when `query`
    * (normalized) is not even a subsequence of `name` (normalized). Exact and
    * prefix matches are the top tiers; otherwise the score is dominated by the
    * number of query characters that land on camel-hump starts.
    */
  def score(query: String, name: String): Option[Int] =
    val nq = normalize(query)
    if nq.isEmpty then None
    else
      val (nn, humps) = normalizedWithHumps(name)
      if nn.isEmpty then None
      else if nn == nq then Some(ExactBase - nn.length)
      else if nn.startsWith(nq) then Some(PrefixBase - nn.length)
      else
        maxHumpHits(nq, nn, humps) match
          case None => None
          case Some(hits) => Some(SubseqBase + hits * HumpBonus - nn.length)

  /** Normalized name plus, for each normalized character, whether it starts a
    * camel hump. The two arrays are index-aligned.
    */
  private def normalizedWithHumps(s: String): (String, Array[Boolean]) =
    val sb = new StringBuilder(s.length)
    val humps = new scala.collection.mutable.ArrayBuffer[Boolean](s.length)
    var prevAlnum = false
    var prevUpper = false
    var prevDigit = false
    var i = 0
    while i < s.length do
      val c = s.charAt(i)
      if Character.isLetterOrDigit(c) then
        val upper = Character.isUpperCase(c)
        val digit = Character.isDigit(c)
        val hump = !prevAlnum || (upper && !prevUpper) || (digit && !prevDigit)
        sb.append(Character.toLowerCase(c))
        humps += hump
        prevAlnum = true
        prevUpper = upper
        prevDigit = digit
      else
        prevAlnum = false
        prevUpper = false
        prevDigit = false
      i += 1
    (sb.toString, humps.toArray)

  /** Max number of query chars matchable at camel-hump positions over any valid
    * subsequence embedding of `nq` in `nn`, or None when `nq` is not a
    * subsequence. DP over (query index, name index).
    */
  private def maxHumpHits(nq: String, nn: String, humps: Array[Boolean]): Option[Int] =
    val q = nq.length
    val n = nn.length
    val NEG = Int.MinValue / 4
    // next(j) = f(i+1, j); f(q, j) = 0 (empty query fully matched).
    var next = Array.fill(n + 1)(0)
    var i = q - 1
    while i >= 0 do
      val cur = Array.fill(n + 1)(NEG) // f(i, n) = NEG: query left, name exhausted
      val qc = nq.charAt(i)
      var j = n - 1
      while j >= 0 do
        var best = cur(j + 1) // skip nn(j)
        if nn.charAt(j) == qc then
          val sub = next(j + 1)
          if sub > NEG then
            val cand = (if humps(j) then 1 else 0) + sub
            if cand > best then best = cand
        cur(j) = best
        j -= 1
      next = cur
      i -= 1
    val res = next(0)
    if res <= NEG then None else Some(res)
