package ls.pc

import scala.meta.internal.pc.SemanticTokens

/** The cross-language semantic-tokens legend parity pin.
  *
  * The island's `Node.tokenType`/`tokenModifier` ints are INDICES into the
  * PC-vendored `scala.meta.internal.pc.SemanticTokens.TokenTypes` /
  * `TokenModifiers` lists (scala3-presentation-compiler 3.8.4). The Rust
  * server advertises those lists verbatim as its `semanticTokensProvider`
  * legend, pinned as constants in `crates/ls-server/src/pc_lsp.rs`
  * (`legend::TOKEN_TYPES` / `legend::TOKEN_MODIFIERS`). This suite prints and
  * asserts the island-side lists against the SAME pinned values — plus the
  * shared golden anchors — so a PC upgrade that changes the legend breaks BOTH
  * builds instead of silently mis-coloring every token.
  */
class SemanticTokensLegendSuite extends munit.FunSuite:

  /** Must equal `legend::TOKEN_TYPES` in `crates/ls-server/src/pc_lsp.rs`. */
  private val pinnedTokenTypes = List(
    "namespace",
    "type",
    "class",
    "enum",
    "interface",
    "struct",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "event",
    "function",
    "method",
    "macro",
    "keyword",
    "modifier",
    "comment",
    "string",
    "number",
    "regexp",
    "operator",
    "decorator"
  )

  /** Must equal `legend::TOKEN_MODIFIERS` in `crates/ls-server/src/pc_lsp.rs`. */
  private val pinnedTokenModifiers = List(
    "declaration",
    "definition",
    "readonly",
    "static",
    "deprecated",
    "abstract",
    "async",
    "modification",
    "documentation",
    "defaultLibrary"
  )

  test("the vendored TokenTypes list is exactly the pinned Rust legend"):
    println(s"scala.meta.internal.pc.SemanticTokens.TokenTypes = ${SemanticTokens.TokenTypes.mkString("[", ", ", "]")}")
    assertEquals(SemanticTokens.TokenTypes, pinnedTokenTypes)
    assertEquals(SemanticTokens.TokenTypes.length, 23)

  test("the vendored TokenModifiers list is exactly the pinned Rust legend"):
    println(s"scala.meta.internal.pc.SemanticTokens.TokenModifiers = ${SemanticTokens.TokenModifiers.mkString("[", ", ", "]")}")
    assertEquals(SemanticTokens.TokenModifiers, pinnedTokenModifiers)
    assertEquals(SemanticTokens.TokenModifiers.length, 10)

  test("golden anchors: 'method' is type index 13, 'declaration' is modifier bit 0"):
    // The same anchors are pinned Rust-side as `legend::METHOD_TYPE_INDEX` /
    // `legend::DECLARATION_MODIFIER_INDEX` (and re-checked when the capability
    // is built), so an insertion that shifts indices fails on both sides.
    assertEquals(SemanticTokens.TokenTypes.indexOf("method"), 13)
    assertEquals(SemanticTokens.TokenModifiers.indexOf("declaration"), 0)
