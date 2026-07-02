package ls.core

import java.util.concurrent.TimeUnit

import org.eclipse.lsp4j.InitializeParams
import org.eclipse.lsp4j.jsonrpc.json.MessageJsonHandler

class CapabilitiesSuite extends munit.FunSuite:

  private def initializeJson(): String =
    val server = new ScalaLs(ScalaLs.Config(exitProcessOnExit = false))
    val result = server.initialize(new InitializeParams()).get(10, TimeUnit.SECONDS)
    new MessageJsonHandler(java.util.Collections.emptyMap()).getGson.toJson(result)

  test("initialize result advertises the five core providers plus completion/hover/signature"):
    val json = initializeJson()
    assert(json.contains("\"workspaceSymbolProvider\":true"), json)
    assert(json.contains("\"referencesProvider\":true"), json)
    assert(json.contains("\"renameProvider\":{\"prepareProvider\":true}"), json)
    assert(json.contains("\"documentHighlightProvider\":true"), json)
    assert(json.contains("\"executeCommandProvider\""), json)
    assert(json.contains("\"completionProvider\""), json)
    assert(json.contains("\"resolveProvider\":true"), json)
    assert(json.contains("\"hoverProvider\":true"), json)
    assert(json.contains("\"signatureHelpProvider\""), json)
    assert(json.contains("\"definitionProvider\":true"), json)
    assert(json.contains("\"typeDefinitionProvider\":true"), json)

  test("initialize result registers full text document sync"):
    val json = initializeJson()
    assert(json.contains("\"textDocumentSync\":1"), json)

  test("initialize result lists exactly the four executeCommand commands"):
    val json = initializeJson()
    for command <- ScalaLs.Commands.all do assert(json.contains(s"\"$command\""), json)
    assertEquals(ScalaLs.Commands.all.length, 4)

  test("semanticTokens and inlayHint are deliberately absent (plan: later)"):
    val json = initializeJson()
    assert(!json.contains("semanticTokensProvider"), json)
    assert(!json.contains("inlayHintProvider"), json)
