//! The `scala3SemanticLs.doctor` report — a typed model rendered as fixed-order
//! sections in both text and JSON, a behavior-preserving port of the Scala
//! `ls.doctor.Doctor` + `ls.core.DoctorCommand`, adapted for the SQLite-removed
//! store: the Scala `SQLite` and `Postings` sections collapse into one `Store`
//! section (the immutable-segment manifest/state facts).
//!
//! The report is TOTAL — it renders in every server state (offline, pre-ready,
//! failed, ready). `Runtime`, `Nix`, and `Store` are always gathered (host +
//! filesystem + read-only store); the live-only `BSP`, `SemanticDB`, `PC`, and
//! `PC Plugins` sections render `unavailable: <reason>` when there is no ready
//! bundle. Gathering is NON-INVASIVE: it reads the host, the workspace files,
//! the on-disk store (`Store::open_readonly`), and the embedded island's STATIC
//! launch config + `/proc/self/maps` — it never boots the JVM, so an index-only
//! session inspects itself with zero JVM in the process.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The doctor list-rendering cap (the Scala `Doctor.ListCap`).
const LIST_CAP: usize = 20;

/// The full build-target inventory the live `BSP`/`SemanticDB` sections need,
/// distilled from the `BspProjectModel` at bootstrap (the model already holds
/// only Scala 3 targets; non-Scala-3 targets are filtered upstream). Retained in
/// the ready bundle because the ingest-facing `WorkspaceTargets` keeps only the
/// indexable (SemanticDB-emitting) targets — so counting/listing off it would
/// silently drop exactly the misconfigured targets the doctor must surface (the
/// Scala doctor reads `model.targets` + `model.unavailableTargets`).
#[derive(Default)]
pub struct DoctorTargets {
    /// The build server's display name from `build/initialize` (the Scala
    /// `server: <name>` doctor line), or `None` when no initialize result was
    /// captured (index-only injections). Retained here so it survives into the
    /// ready bundle the doctor reads.
    pub server_name: Option<String>,
    /// The build server's version from `build/initialize`.
    pub server_version: Option<String>,
    /// All Scala 3 target bspIds, sorted (the Scala `model.targets`).
    pub all_ids: Vec<String>,
    /// bspIds of targets without SemanticDB output, sorted (the Scala
    /// `model.unavailableTargets`) — the `-Xsemanticdb`-missing targets.
    pub unavailable_ids: Vec<String>,
    /// `(bspId, targetroot)` for each indexable target, sorted by bspId. The
    /// targetroot is the directory that CONTAINS `META-INF/semanticdb` (the real
    /// SemanticDB dir is resolved by the `SemanticDB` gather).
    pub indexable_roots: Vec<(String, PathBuf)>,
}

/// A section that may be unavailable, carrying the reason (the Scala
/// `SectionState`). Rendered as `unavailable: <reason>` in text and
/// `{"unavailable": "<reason>"}` in JSON.
pub enum SectionState<T> {
    Available(T),
    Unavailable(String),
}

/// `Runtime`: host + embedded-island launch-config facts, gathered without
/// booting the island.
pub struct RuntimeSection {
    pub java: String,
    pub native_access_enabled_for: Vec<String>,
    pub compact_object_headers: String,
    pub aot_cache: String,
}

/// `Nix`: workspace flake / ivy-lock facts (the Scala `NixSection`).
pub struct NixSection {
    pub flake_detected: bool,
    pub mill_ivy_fetcher_input: bool,
    pub ivy_lock_path: String,
    pub ivy_lock_exists: bool,
    pub lock_status: String,
}

/// `BSP`: build-server + target facts (the Scala `BspSection`).
pub struct BspSection {
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub target_count: usize,
    pub scala3_targets: Vec<String>,
    pub index_unavailable_targets: Vec<String>,
}

/// One SemanticDB targetroot fact.
pub struct SemanticdbRoot {
    pub bsp_id: String,
    pub semanticdb_root: String,
    pub exists: bool,
    pub semanticdb_file_count: usize,
}

/// `SemanticDB`: targetroot + freshness facts (the Scala `SemanticdbSection`).
pub struct SemanticdbSection {
    pub roots: Vec<SemanticdbRoot>,
    /// Doc freshness (fresh/stale/missing). `None` renders `unavailable: not
    /// computed yet`, matching the Scala `stats = None` gather.
    pub freshness: Option<Freshness>,
    pub generated_source_count: usize,
    pub stale_targets: Vec<String>,
}

pub struct Freshness {
    pub fresh: usize,
    pub stale: usize,
    pub missing: usize,
    pub uris: Vec<String>,
}

/// `Store`: the immutable-segment manifest/state facts (replacing the Scala
/// `SQLite` + `Postings` sections). `status` names the store state; `facts` are
/// the rendered fact lines (also the read-only `ls dump` body).
pub struct StoreSection {
    pub status: String,
    pub facts: Vec<String>,
}

/// `PC`: presentation-compiler worker status + target sets, gathered
/// non-invasively (worker status from `/proc/self/maps`, targets from config).
pub struct PcSection {
    pub worker_status: String,
    pub active_targets: Vec<String>,
    pub registered_targets: Vec<String>,
}

/// `PC Plugins`: the `pcPluginStatus` inspection is not ported. Always rendered
/// `unavailable` with the deferral reason, never omitted.
pub struct PcPluginsSection;

/// The whole doctor report: seven sections in fixed render order.
pub struct DoctorReport {
    pub runtime: RuntimeSection,
    pub nix: NixSection,
    pub bsp: SectionState<BspSection>,
    pub semanticdb: SectionState<SemanticdbSection>,
    pub store: StoreSection,
    pub pc: SectionState<PcSection>,
    pub pc_plugins: SectionState<PcPluginsSection>,
}

impl DoctorReport {
    /// The offline report: `Runtime`/`Nix`/`Store` gathered from the host,
    /// workspace, and on-disk store; every live-only section `unavailable`
    /// (the Scala `DoctorInput.offline`).
    pub fn offline(workspace_root: &Path) -> DoctorReport {
        DoctorReport {
            runtime: RuntimeSection::gather(),
            nix: NixSection::gather(workspace_root),
            bsp: SectionState::Unavailable("no BSP connection".to_string()),
            semanticdb: SectionState::Unavailable("no BSP connection".to_string()),
            store: StoreSection::gather(Some(workspace_root)),
            pc: SectionState::Unavailable("no BSP connection".to_string()),
            pc_plugins: SectionState::Unavailable(PcPluginsSection::DEFERRED.to_string()),
        }
    }

    /// The human-readable text layout: the seven headings with indented
    /// `key: value` lines, sections separated by a blank line (the Scala
    /// `Doctor.render`).
    pub fn render_text(&self) -> String {
        let sections = [
            section("Runtime", runtime_lines(&self.runtime)),
            section("Nix", nix_lines(&self.nix)),
            section_of("BSP", &self.bsp, bsp_lines),
            section_of("SemanticDB", &self.semanticdb, semanticdb_lines),
            section("Store", store_lines(&self.store)),
            section_of("PC", &self.pc, pc_lines),
            section_of("PC Plugins", &self.pc_plugins, |_| Vec::new()),
        ];
        sections.join("\n")
    }

    /// The structured JSON object. Section keys: `runtime`, `nix`, `bsp`,
    /// `semanticdb`, `store`, `pc`, `pcPlugins` — the `store` key replaces the
    /// old `sqlite`/`postings`. Unavailable sections encode
    /// `{"unavailable": "<reason>"}`.
    pub fn render_json(&self) -> Value {
        json!({
            "runtime": runtime_json(&self.runtime),
            "nix": nix_json(&self.nix),
            "bsp": state_json(&self.bsp, bsp_json),
            "semanticdb": state_json(&self.semanticdb, semanticdb_json),
            "store": store_json(&self.store),
            "pc": state_json(&self.pc, pc_json),
            "pcPlugins": state_json(&self.pc_plugins, |_| json!({})),
        })
    }
}

// --- text rendering -----------------------------------------------------------

/// A section: `name:` then each line indented two spaces, terminated with a
/// newline (the Scala `Doctor.section`).
fn section(name: &str, lines: Vec<String>) -> String {
    let mut out = format!("{name}:\n");
    for line in lines {
        out.push_str("  ");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn section_of<T>(name: &str, state: &SectionState<T>, f: impl Fn(&T) -> Vec<String>) -> String {
    let lines = match state {
        SectionState::Available(value) => f(value),
        SectionState::Unavailable(reason) => vec![format!("unavailable: {reason}")],
    };
    section(name, lines)
}

fn runtime_lines(r: &RuntimeSection) -> Vec<String> {
    let native_access = if r.native_access_enabled_for.is_empty() {
        "not enabled for any module".to_string()
    } else {
        format!("enabled for {}", r.native_access_enabled_for.join(", "))
    };
    vec![
        format!("Java: {}", r.java),
        format!("Native access: {native_access}"),
        format!("Compact Object Headers: {}", r.compact_object_headers),
        format!("AOT cache: {}", r.aot_cache),
    ]
}

fn nix_lines(n: &NixSection) -> Vec<String> {
    vec![
        format!("flake detected: {}", yes_no(n.flake_detected)),
        format!(
            "mill-ivy-fetcher input: {}",
            yes_no(n.mill_ivy_fetcher_input)
        ),
        format!(
            "ivy lock: {} ({})",
            n.ivy_lock_path,
            if n.ivy_lock_exists {
                "exists"
            } else {
                "missing"
            }
        ),
        format!("lock status: {}", n.lock_status),
    ]
}

fn bsp_lines(b: &BspSection) -> Vec<String> {
    let server = match (&b.server_name, &b.server_version) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name.clone(),
        (None, Some(version)) => format!("unknown server {version}"),
        (None, None) => "unknown (initialize result not provided)".to_string(),
    };
    // SemanticDB is mandatory; a target without it is an ERROR, not a tolerated
    // steady state.
    let coverage = if b.index_unavailable_targets.is_empty() {
        "SemanticDB coverage: all targets emit SemanticDB".to_string()
    } else {
        format!(
            "SemanticDB coverage: ERROR - {} target(s) without SemanticDB (recompile with -Xsemanticdb): {}",
            b.index_unavailable_targets.len(),
            b.index_unavailable_targets.join(", ")
        )
    };
    vec![
        format!("server: {server}"),
        format!("targets: {}", b.target_count),
        format!("Scala 3 targets: {}", count_and_list(&b.scala3_targets)),
        coverage,
    ]
}

fn semanticdb_lines(s: &SemanticdbSection) -> Vec<String> {
    let mut lines = vec![format!("semanticdb roots: {}", s.roots.len())];
    for root in &s.roots {
        let status = if root.exists {
            format!("exists, {} semanticdb files", root.semanticdb_file_count)
        } else {
            "missing".to_string()
        };
        lines.push(format!(
            "  {}: {} ({status})",
            root.bsp_id, root.semanticdb_root
        ));
    }
    match &s.freshness {
        None => lines.push("doc freshness: unavailable: not computed yet".to_string()),
        Some(f) => {
            lines.push(format!("fresh docs: {}", f.fresh));
            lines.push(format!("stale docs (md5 mismatch): {}", f.stale));
            lines.push(format!("missing docs: {}", f.missing));
            if !f.uris.is_empty() {
                lines.push(format!("stale/missing uris: {}", f.uris.join(", ")));
            }
        }
    }
    lines.push(format!(
        "generated source status: {}",
        s.generated_source_count
    ));
    lines.push(format!("stale targets: {}", none_or_list(&s.stale_targets)));
    lines
}

fn store_lines(s: &StoreSection) -> Vec<String> {
    s.facts.clone()
}

fn pc_lines(p: &PcSection) -> Vec<String> {
    vec![
        format!("worker status: {}", p.worker_status),
        format!("active targets: {}", none_or_list(&p.active_targets)),
        format!(
            "registered targets: {}",
            none_or_list(&p.registered_targets)
        ),
    ]
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn count_and_list(items: &[String]) -> String {
    if items.is_empty() {
        "0".to_string()
    } else {
        format!("{} ({})", items.len(), capped(items))
    }
}

fn none_or_list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        format!("{} ({})", items.len(), capped(items))
    }
}

fn capped(items: &[String]) -> String {
    if items.len() <= LIST_CAP {
        items.join(", ")
    } else {
        format!(
            "{}, ... (+{} more)",
            items[..LIST_CAP].join(", "),
            items.len() - LIST_CAP
        )
    }
}

// --- JSON rendering -----------------------------------------------------------

fn state_json<T>(state: &SectionState<T>, f: impl Fn(&T) -> Value) -> Value {
    match state {
        SectionState::Available(value) => f(value),
        SectionState::Unavailable(reason) => json!({ "unavailable": reason }),
    }
}

fn runtime_json(r: &RuntimeSection) -> Value {
    json!({
        "java": r.java,
        "nativeAccessEnabledFor": r.native_access_enabled_for,
        "compactObjectHeaders": r.compact_object_headers,
        "aotCache": r.aot_cache,
    })
}

fn nix_json(n: &NixSection) -> Value {
    json!({
        "flakeDetected": n.flake_detected,
        "millIvyFetcherInput": n.mill_ivy_fetcher_input,
        "ivyLockPath": n.ivy_lock_path,
        "ivyLockExists": n.ivy_lock_exists,
        "lockStatus": n.lock_status,
    })
}

fn bsp_json(b: &BspSection) -> Value {
    json!({
        "serverName": b.server_name,
        "serverVersion": b.server_version,
        "targetCount": b.target_count,
        "scala3Targets": b.scala3_targets,
        "indexUnavailableTargets": b.index_unavailable_targets,
    })
}

fn semanticdb_json(s: &SemanticdbSection) -> Value {
    let roots: Vec<Value> = s
        .roots
        .iter()
        .map(|r| {
            json!({
                "bspId": r.bsp_id,
                "semanticdbRoot": r.semanticdb_root,
                "exists": r.exists,
                "semanticdbFileCount": r.semanticdb_file_count,
            })
        })
        .collect();
    let freshness = match &s.freshness {
        None => Value::Null,
        Some(f) => json!({
            "fresh": f.fresh,
            "stale": f.stale,
            "missing": f.missing,
            "uris": f.uris,
        }),
    };
    json!({
        "roots": roots,
        "freshness": freshness,
        "generatedSourceCount": s.generated_source_count,
        "staleTargets": s.stale_targets,
    })
}

fn store_json(s: &StoreSection) -> Value {
    json!({ "status": s.status, "facts": s.facts })
}

fn pc_json(p: &PcSection) -> Value {
    json!({
        "workerStatus": p.worker_status,
        "activeTargets": p.active_targets,
        "registeredTargets": p.registered_targets,
    })
}

// --- gathering (non-invasive) -------------------------------------------------

impl RuntimeSection {
    /// Gathers Runtime facts from the host + the embedded island's STATIC launch
    /// policy (mirroring `ls_jvm::boot_options`) — never boots the island. The
    /// island is launched from `$JAVA_HOME` with `--enable-native-access=
    /// ALL-UNNAMED` and `-XX:+UseCompactObjectHeaders`, and no `-XX:AOTCache`.
    pub fn gather() -> RuntimeSection {
        RuntimeSection {
            java: java_version(),
            native_access_enabled_for: vec!["ALL-UNNAMED".to_string()],
            compact_object_headers: "enabled".to_string(),
            aot_cache: "missing (no -XX:AOTCache flag)".to_string(),
        }
    }
}

/// The configured Java version, read from `$JAVA_HOME/release` (a file read, no
/// process launched). `unavailable`/`unknown` when it cannot be determined.
fn java_version() -> String {
    let Some(home) = std::env::var_os("JAVA_HOME") else {
        return "unavailable: JAVA_HOME not set".to_string();
    };
    let release = Path::new(&home).join("release");
    match std::fs::read_to_string(&release) {
        Ok(content) => content
            .lines()
            .find_map(|line| line.strip_prefix("JAVA_VERSION="))
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_else(|| format!("unknown (no JAVA_VERSION in {})", release.display())),
        Err(_) => format!("unknown ({} unreadable)", release.display()),
    }
}

impl NixSection {
    /// The workspace-relative ivy lock path (the Scala `NixSection.IvyLockRelPath`).
    const IVY_LOCK_REL_PATH: &'static str = "nix/ivy-lock.nix";

    /// Gathers Nix workspace facts from the filesystem (the Scala
    /// `NixSection.gather`). Total; never runs `mif`.
    pub fn gather(workspace_root: &Path) -> NixSection {
        let flake_file = workspace_root.join("flake.nix");
        let flake_detected = flake_file.is_file();
        let mill_ivy_fetcher_input = flake_detected
            && std::fs::read_to_string(&flake_file)
                .map(|c| c.contains("mill-ivy-fetcher"))
                .unwrap_or(false);
        let lock_file = workspace_root.join(Self::IVY_LOCK_REL_PATH);
        let ivy_lock_exists = lock_file.is_file();
        NixSection {
            flake_detected,
            mill_ivy_fetcher_input,
            ivy_lock_path: Self::IVY_LOCK_REL_PATH.to_string(),
            ivy_lock_exists,
            lock_status: nix_lock_status(
                workspace_root,
                flake_detected,
                ivy_lock_exists,
                &lock_file,
            ),
        }
    }
}

/// The ivy-lock staleness heuristic (the Scala `NixSection.lockStatus`): a
/// missing lock is stale; otherwise a cheap mtime-vs-`build.mill` heuristic when
/// `mif` is on PATH, else `unknown` (CI owns the authoritative check). The doctor
/// NEVER runs `mif`.
fn nix_lock_status(
    workspace_root: &Path,
    flake_detected: bool,
    lock_exists: bool,
    lock_file: &Path,
) -> String {
    let rel = NixSection::IVY_LOCK_REL_PATH;
    if !flake_detected {
        return format!("unknown: no flake.nix under {}", workspace_root.display());
    }
    if !lock_exists {
        return format!("stale ({rel} does not exist; run `mif run -p . -o {rel}`)");
    }
    if !mif_runnable() {
        return "unknown: mif is not runnable from this process; \
                CI (scripts/check-ivy-lock.sh) owns the authoritative staleness check"
            .to_string();
    }
    let build_mill = workspace_root.join("build.mill");
    if !build_mill.is_file() {
        return "unknown: build.mill not found next to the lock".to_string();
    }
    match (mtime(lock_file), mtime(&build_mill)) {
        (Some(lock_t), Some(build_t)) if lock_t >= build_t => format!(
            "fresh (heuristic: {rel} is not older than build.mill; authoritative check runs in CI)"
        ),
        (Some(_), Some(_)) => {
            format!("stale (build.mill modified after {rel}; run `mif run -p . -o {rel}`)")
        }
        _ => "unknown: could not read modification times".to_string(),
    }
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Whether an executable `mif` is on PATH (the doctor deliberately never runs it).
fn mif_runnable() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join("mif");
        std::fs::metadata(&candidate)
            .map(|m| {
                use std::os::unix::fs::PermissionsExt;
                m.is_file() && (m.permissions().mode() & 0o111 != 0)
            })
            .unwrap_or(false)
    })
}

impl StoreSection {
    /// The `Store` section facts (manifest/segment/state), read strictly
    /// read-only from `<workspace_root>/.scala3-bsp-semantic-ls` — never boots a
    /// JVM, never creates or mutates the store.
    pub fn gather(workspace_root: Option<&Path>) -> StoreSection {
        let (status, facts) = crate::store_dump::store_facts(workspace_root);
        StoreSection { status, facts }
    }
}

impl PcPluginsSection {
    /// The deferral reason for the `PC Plugins` section: the `pcPluginStatus`
    /// command and its plugin-status inspection infrastructure are not ported.
    pub const DEFERRED: &'static str =
        "pcPluginStatus is not ported: plugin-status inspection is unavailable";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ready() -> DoctorReport {
        DoctorReport {
            runtime: RuntimeSection {
                java: "21.0.2".to_string(),
                native_access_enabled_for: vec!["ALL-UNNAMED".to_string()],
                compact_object_headers: "enabled".to_string(),
                aot_cache: "missing (no -XX:AOTCache flag)".to_string(),
            },
            nix: NixSection {
                flake_detected: true,
                mill_ivy_fetcher_input: true,
                ivy_lock_path: "nix/ivy-lock.nix".to_string(),
                ivy_lock_exists: true,
                lock_status: "fresh".to_string(),
            },
            bsp: SectionState::Available(BspSection {
                server_name: None,
                server_version: None,
                target_count: 2,
                scala3_targets: vec!["a".to_string(), "b".to_string()],
                index_unavailable_targets: Vec::new(),
            }),
            semanticdb: SectionState::Available(SemanticdbSection {
                roots: vec![SemanticdbRoot {
                    bsp_id: "a".to_string(),
                    semanticdb_root: "/ws/out-a".to_string(),
                    exists: true,
                    semanticdb_file_count: 3,
                }],
                freshness: None,
                generated_source_count: 0,
                stale_targets: Vec::new(),
            }),
            store: StoreSection {
                status: "active".to_string(),
                facts: vec!["segment: id 1".to_string()],
            },
            pc: SectionState::Available(PcSection {
                worker_status: "not booted (cold)".to_string(),
                active_targets: Vec::new(),
                registered_targets: vec!["a".to_string()],
            }),
            pc_plugins: SectionState::Unavailable(PcPluginsSection::DEFERRED.to_string()),
        }
    }

    #[test]
    fn text_renders_the_seven_sections_in_fixed_order() {
        let text = sample_ready().render_text();
        let headings: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with(' ') && l.ends_with(':'))
            .collect();
        assert_eq!(
            headings,
            vec![
                "Runtime:",
                "Nix:",
                "BSP:",
                "SemanticDB:",
                "Store:",
                "PC:",
                "PC Plugins:"
            ]
        );
    }

    #[test]
    fn text_renders_runtime_nix_and_pc_facts() {
        let text = sample_ready().render_text();
        assert!(text.contains("  Java: 21.0.2"), "{text}");
        assert!(
            text.contains("  Native access: enabled for ALL-UNNAMED"),
            "{text}"
        );
        assert!(text.contains("  Compact Object Headers: enabled"), "{text}");
        assert!(text.contains("  flake detected: yes"), "{text}");
        assert!(
            text.contains("  worker status: not booted (cold)"),
            "{text}"
        );
        assert!(text.contains("  registered targets: 1 (a)"), "{text}");
        assert!(text.contains("  active targets: none"), "{text}");
    }

    #[test]
    fn unavailable_sections_render_the_reason_not_omitted() {
        let root = std::path::Path::new("/nonexistent-doctor-root");
        let text = DoctorReport::offline(root).render_text();
        // All live-only sections present with `unavailable: ...`, never dropped.
        assert!(
            text.contains("BSP:\n  unavailable: no BSP connection"),
            "{text}"
        );
        assert!(
            text.contains("SemanticDB:\n  unavailable: no BSP connection"),
            "{text}"
        );
        assert!(
            text.contains("PC:\n  unavailable: no BSP connection"),
            "{text}"
        );
        assert!(text.contains("PC Plugins:\n  unavailable:"), "{text}");
        // Store still renders (its facts line for a missing root).
        assert!(text.contains("Store:"), "{text}");
    }

    #[test]
    fn json_has_the_store_key_and_no_sqlite_or_postings() {
        let value = sample_ready().render_json();
        assert!(value.get("store").is_some(), "store key present");
        assert!(value.get("sqlite").is_none(), "no sqlite key");
        assert!(value.get("postings").is_none(), "no postings key");
        // Fixed section keys present.
        for key in [
            "runtime",
            "nix",
            "bsp",
            "semanticdb",
            "store",
            "pc",
            "pcPlugins",
        ] {
            assert!(value.get(key).is_some(), "missing key {key}");
        }
        assert_eq!(value["store"]["status"], "active");
        assert_eq!(value["pc"]["registeredTargets"][0], "a");
    }

    #[test]
    fn json_unavailable_sections_encode_the_reason() {
        let root = std::path::Path::new("/nonexistent-doctor-root");
        let value = DoctorReport::offline(root).render_json();
        assert_eq!(value["bsp"]["unavailable"], "no BSP connection");
        assert!(value["pcPlugins"]["unavailable"].is_string());
        // A valid, serializable JSON object.
        assert!(serde_json::to_string(&value).is_ok());
    }
}
