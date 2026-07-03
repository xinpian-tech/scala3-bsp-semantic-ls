package ls.core

import java.nio.file.Path

import ls.doctor.{
  BspSection,
  Doctor,
  DoctorInput,
  NixSection,
  PcPluginsSection,
  PcSection,
  PostingsSection,
  RuntimeSection,
  SectionState,
  SemanticdbSection,
  SqliteSection
}
import ls.pc.PcPluginStatusReport

/** `scala3SemanticLs.doctor` command: gathers a [[ls.doctor.DoctorInput]]
  * from the live [[WorkspaceState]] and renders the plan-19 report. Total —
  * a pre-bootstrap or failed server still produces a report (Runtime + Nix
  * plus `unavailable` sections) so the doctor is exactly the tool that works
  * when nothing else does.
  */
object DoctorCommand:

  def report(workspaceRoot: Option[Path], state: WorkspaceState): String =
    val header = s"state: ${state.statusLine}\n\n"
    state match
      case WorkspaceState.Ready(s) => header + Doctor.render(input(s)) + notesSection(s)
      case _ =>
        val root = workspaceRoot.getOrElse(Path.of(".").toAbsolutePath.normalize)
        header + Doctor.render(DoctorInput.offline(root))

  def input(s: CoreServices): DoctorInput =
    DoctorInput(
      runtime = RuntimeSection.gather(),
      nix = NixSection.gather(s.workspaceRoot),
      bsp = s.model match
        case Some(m) => BspSection.gather(m, s.serverInfo)
        case None => SectionState.Unavailable("no BSP connection"),
      // The MetaStore reads happen inside SemanticdbSection's failure boundary
      // (via the meta overload), so a store failure degrades this section to
      // `unavailable` instead of crashing the whole doctor report.
      semanticdb = s.model match
        case Some(m) => SemanticdbSection.fromModel(m, stats = None, s.meta)
        case None => SectionState.Unavailable("no BSP connection"),
      sqlite = SqliteSection.gather(s.meta),
      postings = PostingsSection.gather(s.meta, s.snapshots),
      pc = SectionState.attempt("PC") {
        PcSection.gather(s.pc.activeTargets, s.pc.registeredTargets, workerAlive = None)
      },
      pcPlugins = SectionState.attempt("PC Plugins") {
        PcPluginsSection.gather(s.pc.pluginStatus)
      }
    )

  private def notesSection(s: CoreServices): String =
    if s.notes.isEmpty then ""
    else s.notes.mkString("\nBootstrap:\n  ", "\n  ", "\n")

/** Rendering for `scala3SemanticLs.pcPluginStatus`: the PC-plugins slice of
  * the doctor as a standalone command result.
  */
object PcStatusRender:

  def render(report: PcPluginStatusReport): String =
    val compiler =
      if report.compilerPlugins.isEmpty then Vector("compiler plugins: none")
      else
        s"compiler plugins: ${report.compilerPlugins.length}" +:
          report.compilerPlugins.map { c =>
            val jars = if c.jars.isEmpty then "(no jars)" else c.jars.mkString(", ")
            s"  $jars: ${if c.loaded then "loaded" else c.detail}"
          }
    val service =
      if report.servicePlugins.isEmpty then Vector("service plugins: none")
      else
        s"service plugins: ${report.servicePlugins.length}" +:
          report.servicePlugins.map { p =>
            val status = if p.enabled then "enabled" else "disabled"
            val selfTest = if p.selfTestOk then "self-test ok" else p.selfTestDetail
            s"  ${p.id} (${p.source}): $status, $selfTest"
          }
    val disabled =
      if report.disabled.isEmpty then Vector("disabled plugins: none")
      else
        s"disabled plugins: ${report.disabled.length}" +:
          report.disabled.map(d => s"  ${d.id}: ${d.reason}")
    (compiler ++ service ++ disabled).mkString("\n")
