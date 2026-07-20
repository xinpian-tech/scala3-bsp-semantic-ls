//! `textDocument/formatting` over the scalafmt COMMAND LINE.
//!
//! The server never links scalafmt-core: formatting spawns an external
//! `scalafmt` binary (`--stdin --config <ws>/.scalafmt.conf --non-interactive`,
//! cwd = the workspace root) over the OPEN buffer text and folds the output
//! into MINIMAL `lsp_types::TextEdit`s — the `dissimilar` diff (the dtolnay
//! crate rust-analyzer uses for its formatting diffs) walked into
//! replace/delete/insert edits addressing the ORIGINAL text, positions in
//! UTF-16 via `line-index` ([`minimal_edits`]). Formatting that changes
//! nothing is the empty edit list. The request's LSP `options` (tab size,
//! insert-spaces, …) are deliberately ignored: `.scalafmt.conf` is the single
//! authority on style, exactly as scalafmt users expect.
//!
//! Binary resolution ([`resolve_scalafmt`]) mirrors the
//! [`crate::pc::resolve_java_home`] precedence — config > env > nix-baked:
//! the workspace config's `scalafmt` key wins, then `LS_SCALAFMT`, then the
//! first executable `scalafmt` on `PATH`. The packaged wrapper bakes the nix
//! default as `--set-default LS_SCALAFMT <store scalafmt>/bin/scalafmt`
//! (`nix/package.nix`), which by construction applies only when the caller's
//! environment does not set the variable — so under the packaged wrapper the
//! `PATH` tier is effectively the wrapper-bypassed/dev-shell tier, the same
//! shape as the baked `JAVA_HOME`.
//!
//! Config discovery is the workspace ROOT only ([`scalafmt_conf`]): scalafmt
//! itself owns nested-config semantics (`project.*` includes/excludes,
//! `fileOverride`), so the server hands it the one root config instead of
//! re-implementing a nearest-conf walk. A missing root config is a typed
//! error — scalafmt requires a pinned `version` and refuses to run without
//! one, so the server refuses first with a message that says why.
//!
//! Offline stance: the spawn exports `COURSIER_MODE=offline` into the child,
//! so the scalafmt-dynamic CLI can never download a `.scalafmt.conf`-pinned
//! core version from Maven behind the editor's back — the nix-shipped
//! scalafmt is ONE fixed version, and a workspace pinning any other fails
//! fast with the typed non-zero-exit error whose stderr tail names the
//! unresolvable artifact (documented in `docs/deployment.md`).
//!
//! Range formatting is deliberately NOT advertised: the CLI's hidden
//! `--range from=to` option is experimental and skips lines inside multi-line
//! ranges (probed on the nix scalafmt 3.11.1: `--range 3=4` leaves line 4
//! untouched where `--range 4=4` alone formats it), and a partially formatted
//! selection is worse than not offering the provider.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The per-request scalafmt deadline: the child is killed past it and the
/// request fails typed. An explicit format request blocking the loop for this
/// long is the same class as a PC cold boot; ten seconds bounds the stall
/// while leaving headroom for the scalafmt CLI's own JVM start.
pub(crate) const SCALAFMT_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of scalafmt's stderr a non-zero exit carries into the typed
/// error: the last [`STDERR_TAIL_LINES`] noise-filtered lines, capped at
/// [`STDERR_TAIL_BYTES`].
const STDERR_TAIL_LINES: usize = 10;
const STDERR_TAIL_BYTES: usize = 2000;

/// The workspace-level `scalafmt` binary override, read from the optional
/// `.scala3-bsp-semantic-ls/config.json` at the workspace root (the same file
/// and discipline as the PC island's `javaHome`). Absent file, unparseable
/// JSON, or a missing/non-string `scalafmt` key all resolve to `None` — the
/// config tier simply does not apply.
fn workspace_config_scalafmt(workspace_root: &Path) -> Option<PathBuf> {
    let text =
        std::fs::read_to_string(workspace_root.join(".scala3-bsp-semantic-ls/config.json")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("scalafmt")?.as_str().map(PathBuf::from)
}

/// Resolves the scalafmt binary by the [`crate::pc::resolve_java_home`]
/// precedence — workspace config `scalafmt`, then `LS_SCALAFMT`, then the
/// first executable `scalafmt` on `PATH`. The nix-baked default arrives as
/// the wrapper's `--set-default LS_SCALAFMT` (see the module doc). `None`
/// when no tier resolves.
pub(crate) fn resolve_scalafmt(
    workspace_root: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    if let Some(path) = workspace_config_scalafmt(workspace_root) {
        return Some(path);
    }
    if let Some(path) = env("LS_SCALAFMT") {
        return Some(PathBuf::from(path));
    }
    scalafmt_on_path(env)
}

/// The first `PATH` directory holding an executable regular file named
/// `scalafmt`. Empty `PATH` entries (the historical "cwd" spelling) are
/// skipped rather than resolved against the process cwd.
fn scalafmt_on_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let path = env("PATH")?;
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join("scalafmt"))
        .find(|candidate| is_executable(candidate))
}

/// An executable regular file (any execute bit; the server does not model
/// per-user permission checks beyond what `PATH` lookup means).
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The workspace's `.scalafmt.conf` — the workspace ROOT only, by decision
/// (see the module doc: scalafmt owns nested-config semantics). `None` when
/// the root file does not exist.
pub(crate) fn scalafmt_conf(workspace_root: &Path) -> Option<PathBuf> {
    let conf = workspace_root.join(".scalafmt.conf");
    conf.is_file().then_some(conf)
}

/// Runs `<bin> --stdin --config <conf> --non-interactive` with cwd =
/// `workspace_root`, writing `text` to stdin and returning the formatted
/// buffer text (scalafmt prints stdout only when the result DIFFERS from the
/// input, so silent stdout with a clean exit answers `text` unchanged).
/// `COURSIER_MODE=offline` is exported into the child (the module-doc
/// offline stance). Past `timeout` the child is killed and the error is
/// typed; a non-zero exit is a typed error carrying the stderr tail (e.g. a
/// parse error's `stdin.scala:N: error:` lines, or the version-mismatch
/// resolution failure naming the unresolvable scalafmt-core artifact).
///
/// The helper threads only feed stdin and drain the two output pipes so a
/// full pipe can never deadlock the child (scalafmt reads all of stdin before
/// writing); they are joined — or the child killed — before this function
/// returns, so the request itself stays synchronous and cancellable-by-queue
/// like every other handler.
pub(crate) fn run_scalafmt(
    bin: &Path,
    conf: &Path,
    workspace_root: &Path,
    text: &str,
    timeout: Duration,
) -> Result<String, String> {
    let mut child = Command::new(bin)
        .arg("--stdin")
        .arg("--config")
        .arg(conf)
        .arg("--non-interactive")
        .current_dir(workspace_root)
        .env("COURSIER_MODE", "offline")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn scalafmt ({}): {error}", bin.display()))?;
    let mut stdin = child.stdin.take().expect("piped scalafmt stdin");
    let input = text.to_string();
    // Dropping stdin at the end of the closure closes the pipe — scalafmt's
    // EOF signal to start formatting.
    let stdin_writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().expect("piped scalafmt stdout");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let mut stderr = child.stderr.take().expect("piped scalafmt stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // Kill + reap; the closed pipes EOF the reader threads, so the
                // joins below cannot hang.
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdin_writer.join();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!(
                    "scalafmt timed out after {timeout:?} and was killed"
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(format!("failed to wait for scalafmt: {error}")),
        }
    };
    let _ = stdin_writer.join();
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "scalafmt failed ({status}): {}",
            stderr_tail(&String::from_utf8_lossy(&stderr))
        ));
    }
    // `--stdin` prints the formatted text only when it DIFFERS from the
    // input; an already-formatted buffer yields silent stdout (probed on the
    // nix scalafmt 3.11.1). Empty stdout is unambiguous — a real formatted
    // result is never the empty string (even empty input formats to "\n") —
    // so it means "unchanged".
    if stdout.is_empty() {
        return Ok(text.to_string());
    }
    String::from_utf8(stdout).map_err(|_| "scalafmt produced non-UTF-8 output".to_string())
}

/// The stderr tail a failed scalafmt run carries into its typed error: JVM
/// launcher noise (`Picked up …`), coursier progress lines, and Java stack
/// frames are dropped, then the LAST [`STDERR_TAIL_LINES`] remaining lines are
/// kept (front-truncated to [`STDERR_TAIL_BYTES`]) — enough for a parse
/// error's message lines or the offline resolution failure naming the
/// mismatched scalafmt-core version, without a whole stack trace in an editor
/// popup.
fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !line.starts_with("Picked up "))
        .filter(|line| !line.trim_start().starts_with("at "))
        .filter(|line| !line.starts_with("Downloading ") && !line.starts_with("Downloaded "))
        .collect();
    let start = lines.len().saturating_sub(STDERR_TAIL_LINES);
    let mut tail = lines[start..].join("\n");
    if tail.len() > STDERR_TAIL_BYTES {
        let mut cut = tail.len() - STDERR_TAIL_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = format!("…{}", &tail[cut..]);
    }
    if tail.is_empty() {
        "(no stderr)".to_string()
    } else {
        tail
    }
}

/// Folds the `dissimilar` diff of `original` -> `formatted` into minimal
/// `TextEdit`s (the rust-analyzer diff→edits fold): a `Delete` immediately
/// followed by an `Insert` is one replace edit, a lone `Delete`/`Insert` is a
/// delete/insert edit, and `Equal` runs only advance the cursor. Ranges
/// address the ORIGINAL text, positions in UTF-16 code units via `line-index`
/// (the encoding the server advertises); the chunks walk the original left to
/// right, so the edit list is ascending and non-overlapping by construction.
/// An identical `formatted` is the empty list.
pub(crate) fn minimal_edits(original: &str, formatted: &str) -> Vec<lsp_types::TextEdit> {
    use dissimilar::Chunk;

    let index = line_index::LineIndex::new(original);
    let position = |offset: usize| -> lsp_types::Position {
        let line_col = index.line_col(line_index::TextSize::new(offset as u32));
        // Chunk boundaries are `&str` slice boundaries of `original`, so the
        // offset always names a valid char position with a wide equivalent.
        let wide = index
            .to_wide(line_index::WideEncoding::Utf16, line_col)
            .expect("diff chunk offsets lie on char boundaries of the original text");
        lsp_types::Position::new(wide.line, wide.col)
    };
    let edit = |start: usize, end: usize, new_text: &str| lsp_types::TextEdit {
        range: lsp_types::Range::new(position(start), position(end)),
        new_text: new_text.to_string(),
    };

    let mut edits = Vec::new();
    let mut pos = 0usize;
    let mut chunks = dissimilar::diff(original, formatted).into_iter().peekable();
    while let Some(chunk) = chunks.next() {
        if let (Chunk::Delete(deleted), Some(Chunk::Insert(inserted))) =
            (chunk, chunks.peek().copied())
        {
            chunks.next();
            edits.push(edit(pos, pos + deleted.len(), inserted));
            pos += deleted.len();
            continue;
        }
        match chunk {
            Chunk::Equal(text) => pos += text.len(),
            Chunk::Delete(deleted) => {
                edits.push(edit(pos, pos + deleted.len(), ""));
                pos += deleted.len();
            }
            Chunk::Insert(inserted) => edits.push(edit(pos, pos, inserted)),
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::documents::{apply_content_changes, ContentChange};
    use crate::protocol::{Position, Range};

    // --- resolution precedence (injected env) -----------------------------

    fn write_config(root: &Path, json: &str) {
        let dir = root.join(".scala3-bsp-semantic-ls");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), json).unwrap();
    }

    /// A directory holding an executable `scalafmt` stub, for the PATH tier.
    fn executable_scalafmt_dir(root: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("scalafmt");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[test]
    fn resolution_prefers_the_workspace_config_over_env_and_path() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), r#"{"scalafmt": "/from/config/scalafmt"}"#);
        let exec_dir = executable_scalafmt_dir(dir.path(), "on-path");
        let exec_dir_str = exec_dir.to_str().unwrap().to_string();
        let env = move |key: &str| match key {
            "LS_SCALAFMT" => Some("/from/env/scalafmt".to_string()),
            "PATH" => Some(exec_dir_str.clone()),
            _ => None,
        };
        assert_eq!(
            resolve_scalafmt(dir.path(), &env),
            Some(PathBuf::from("/from/config/scalafmt"))
        );
    }

    #[test]
    fn resolution_prefers_ls_scalafmt_over_path_without_a_config() {
        let dir = tempfile::tempdir().unwrap();
        let exec_dir = executable_scalafmt_dir(dir.path(), "on-path");
        let exec_dir_str = exec_dir.to_str().unwrap().to_string();
        let env = move |key: &str| match key {
            "LS_SCALAFMT" => Some("/from/env/scalafmt".to_string()),
            "PATH" => Some(exec_dir_str.clone()),
            _ => None,
        };
        assert_eq!(
            resolve_scalafmt(dir.path(), &env),
            Some(PathBuf::from("/from/env/scalafmt"))
        );
    }

    // The PATH tier takes the FIRST directory with an EXECUTABLE scalafmt:
    // a directory whose scalafmt lacks the execute bit is skipped, and empty
    // PATH entries never resolve against the cwd.
    #[test]
    fn the_path_tier_finds_the_first_executable_scalafmt() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let non_exec_dir = dir.path().join("non-exec");
        std::fs::create_dir_all(&non_exec_dir).unwrap();
        let non_exec = non_exec_dir.join("scalafmt");
        std::fs::write(&non_exec, "not a program").unwrap();
        std::fs::set_permissions(&non_exec, std::fs::Permissions::from_mode(0o644)).unwrap();
        let exec_dir = executable_scalafmt_dir(dir.path(), "exec");
        let path = format!("::{}:{}", non_exec_dir.display(), exec_dir.display());
        let env = move |key: &str| (key == "PATH").then(|| path.clone());
        assert_eq!(
            resolve_scalafmt(dir.path(), &env),
            Some(exec_dir.join("scalafmt"))
        );
    }

    // An unparseable config (or one without the key) falls through to the env
    // tier — the config tier simply does not apply, like `javaHome`.
    #[test]
    fn a_config_without_the_key_falls_through_and_no_tier_is_none() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), r#"{"javaHome": "/jdk"}"#);
        let env = |key: &str| (key == "LS_SCALAFMT").then(|| "/from/env/scalafmt".to_string());
        assert_eq!(
            resolve_scalafmt(dir.path(), &env),
            Some(PathBuf::from("/from/env/scalafmt"))
        );
        write_config(dir.path(), "not json");
        assert_eq!(
            resolve_scalafmt(dir.path(), &env),
            Some(PathBuf::from("/from/env/scalafmt"))
        );
        let empty = |_: &str| None;
        assert_eq!(resolve_scalafmt(dir.path(), &empty), None);
    }

    // --- config discovery --------------------------------------------------

    #[test]
    fn scalafmt_conf_is_the_workspace_root_file_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(scalafmt_conf(dir.path()), None);
        std::fs::write(dir.path().join(".scalafmt.conf"), "version = \"3.9.8\"\n").unwrap();
        assert_eq!(
            scalafmt_conf(dir.path()),
            Some(dir.path().join(".scalafmt.conf"))
        );
        // A nested conf is scalafmt's business, not discovery's.
        let nested = dir.path().join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(".scalafmt.conf"), "version = \"3.9.8\"\n").unwrap();
        assert_eq!(
            scalafmt_conf(&nested).unwrap(),
            nested.join(".scalafmt.conf")
        );
    }

    // --- the runner over fake binaries -------------------------------------

    /// Writes an executable fake scalafmt script and returns its path.
    fn fake_binary(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("fake-scalafmt");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// [`run_scalafmt`] with a retry on `ETXTBSY`: a CONCURRENT test's
    /// fork can hold a just-written script's write fd across its exec window,
    /// making our exec fail with "Text file busy". Test-only — production
    /// spawns pre-existing binaries, never files it just wrote.
    fn run_fake(
        bin: &Path,
        conf: &Path,
        workspace_root: &Path,
        text: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        for _ in 0..50 {
            match run_scalafmt(bin, conf, workspace_root, text, timeout) {
                Err(error) if error.contains("Text file busy") => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                outcome => return outcome,
            }
        }
        run_scalafmt(bin, conf, workspace_root, text, timeout)
    }

    fn conf_in(dir: &Path) -> PathBuf {
        let conf = dir.join(".scalafmt.conf");
        std::fs::write(&conf, "version = \"3.9.8\"\n").unwrap();
        conf
    }

    #[test]
    fn the_runner_pipes_stdin_to_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_binary(dir.path(), "#!/bin/sh\ncat\nprintf 'extra\\n'\n");
        let conf = conf_in(dir.path());
        let out = run_fake(&bin, &conf, dir.path(), "object A\n", SCALAFMT_TIMEOUT).unwrap();
        assert_eq!(out, "object A\nextra\n");
    }

    // scalafmt's --stdin convention: silent stdout on an already-formatted
    // input means "unchanged" — the runner answers the input text, so the
    // diff downstream is empty.
    #[test]
    fn silent_stdout_with_a_clean_exit_means_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_binary(dir.path(), "#!/bin/sh\ncat > /dev/null\nexit 0\n");
        let conf = conf_in(dir.path());
        let out = run_fake(&bin, &conf, dir.path(), "object A\n", SCALAFMT_TIMEOUT).unwrap();
        assert_eq!(out, "object A\n");
    }

    #[test]
    fn a_wedged_binary_is_killed_at_the_deadline_with_a_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_binary(dir.path(), "#!/bin/sh\nexec sleep 30\n");
        let conf = conf_in(dir.path());
        let started = Instant::now();
        let error = run_fake(
            &bin,
            &conf,
            dir.path(),
            "object A\n",
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the kill must not wait out the sleep"
        );
    }

    #[test]
    fn a_non_zero_exit_surfaces_the_stderr_tail() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_binary(
            dir.path(),
            "#!/bin/sh\necho 'stdin.scala:3: error: illegal start of definition' >&2\nexit 2\n",
        );
        let conf = conf_in(dir.path());
        let error = run_fake(&bin, &conf, dir.path(), "object A\n", SCALAFMT_TIMEOUT).unwrap_err();
        assert!(error.contains("scalafmt failed"), "{error}");
        assert!(
            error.contains("stdin.scala:3: error: illegal start of definition"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_binary_is_a_typed_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let conf = conf_in(dir.path());
        let error = run_scalafmt(
            &dir.path().join("absent-scalafmt"),
            &conf,
            dir.path(),
            "object A\n",
            SCALAFMT_TIMEOUT,
        )
        .unwrap_err();
        assert!(error.contains("failed to spawn scalafmt"), "{error}");
    }

    // The offline stance rides the spawn env: the child sees
    // COURSIER_MODE=offline, so scalafmt-dynamic can never download another
    // core version.
    #[test]
    fn the_spawn_exports_coursier_offline_mode() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_binary(dir.path(), "#!/bin/sh\nprintf '%s' \"$COURSIER_MODE\"\n");
        let conf = conf_in(dir.path());
        let out = run_fake(&bin, &conf, dir.path(), "", SCALAFMT_TIMEOUT).unwrap();
        assert_eq!(out, "offline");
    }

    // The tail filter over the REAL failure shapes: the JVM `Picked up` noise
    // and stack frames drop, the offline resolution error keeps the line
    // naming the mismatched scalafmt-core version, and a parse error's
    // message lines survive intact.
    #[test]
    fn stderr_tail_keeps_the_version_mismatch_and_drops_jvm_noise() {
        let trace = "Picked up JAVA_TOOL_OPTIONS: -Dhttp.proxyHost=proxy\n\
             org.scalafmt.cli.FailedToFormat: /ws/.scalafmt.conf\n\
             Caused by: coursierapi.error.MultipleResolutionError: Error downloading org.scala-lang:scala-reflect:2.13.16\n\
             \u{20}\u{20}not found: /home/user/.ivy2/local/org.scala-lang/scala-reflect/2.13.16/ivys/ivy.xml\n\
             Error downloading org.scalameta:scalafmt-core_2.13:3.9.7\n\
             \u{20}\u{20}not found: /cache/https/repo1.maven.org/maven2/org/scalameta/scalafmt-core_2.13/3.9.7/scalafmt-core_2.13-3.9.7.pom\n\
             \tat coursierapi.error.MultipleResolutionError.of(MultipleResolutionError.java:28)\n\
             \tat org.scalafmt.cli.Cli$.run(Cli.scala:68)\n";
        let tail = stderr_tail(trace);
        assert!(
            tail.contains("Error downloading org.scalameta:scalafmt-core_2.13:3.9.7"),
            "{tail}"
        );
        assert!(!tail.contains("Picked up"), "{tail}");
        assert!(!tail.contains("\tat "), "{tail}");

        let parse = "Picked up JAVA_TOOL_OPTIONS: x\n\
             stdin.scala:3: error: [dialect scala3] illegal start of simple expression\n\
             }\n\
             ^\n";
        let tail = stderr_tail(parse);
        assert!(tail.starts_with("stdin.scala:3: error:"), "{tail}");

        assert_eq!(stderr_tail(""), "(no stderr)");
    }

    #[test]
    fn stderr_tail_caps_lines_and_bytes_keeping_the_end() {
        let many: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let tail = stderr_tail(&many);
        assert!(tail.starts_with("line 30"), "{tail}");
        assert!(tail.ends_with("line 39"), "{tail}");

        let huge = format!("{}\nthe end", "x".repeat(3000));
        let tail = stderr_tail(&huge);
        assert!(tail.len() <= STDERR_TAIL_BYTES + '…'.len_utf8());
        assert!(tail.starts_with('…'), "{tail}");
        assert!(tail.ends_with("the end"), "{tail}");
    }

    // --- the diff → edits fold ---------------------------------------------

    /// Applies original-addressed, ascending, non-overlapping edits the way an
    /// LSP client does: bottom-up, each through the document store's UTF-16
    /// range application — so the fold is verified against the same position
    /// arithmetic the rest of the server uses.
    fn apply(original: &str, edits: &[lsp_types::TextEdit]) -> String {
        let mut text = original.to_string();
        for edit in edits.iter().rev() {
            let change = ContentChange {
                range: Some(Range {
                    start: Position {
                        line: edit.range.start.line,
                        character: edit.range.start.character,
                    },
                    end: Position {
                        line: edit.range.end.line,
                        character: edit.range.end.character,
                    },
                }),
                text: edit.new_text.clone(),
            };
            text = apply_content_changes(&text, &[change]);
        }
        text
    }

    #[test]
    fn an_identical_result_is_the_empty_edit_list() {
        assert_eq!(minimal_edits("object A\n", "object A\n"), vec![]);
        assert_eq!(minimal_edits("", ""), vec![]);
    }

    // Two separated hunks produce two edits with exact UTF-16 ranges — the
    // untouched middle line is never part of any edit.
    #[test]
    fn multi_hunk_changes_produce_minimal_separated_edits() {
        let original = "object   A {\n  val ok = 1\n  def f( x:Int )=x\n}\n";
        let formatted = "object A {\n  val ok = 1\n  def f(x: Int) = x\n}\n";
        let edits = minimal_edits(original, formatted);
        assert!(
            edits.len() >= 2,
            "two separated hunks must not collapse into one whole-file edit: {edits:?}"
        );
        assert!(
            edits
                .iter()
                .all(|edit| edit.range.start.line != 1 && edit.range.end.line != 1),
            "the already-formatted line must stay untouched: {edits:?}"
        );
        // The first hunk: the doubled space after `object`.
        assert_eq!(edits[0].range.start.line, 0);
        assert_eq!(apply(original, &edits), formatted);
    }

    // UTF-16 columns: an astral char (2 UTF-16 units, 4 UTF-8 bytes) before
    // the edit shifts the reported character by its WIDE width, not its byte
    // width.
    #[test]
    fn edits_after_an_astral_char_use_utf16_columns() {
        let original = "val 𝕏x=1\n";
        let formatted = "val 𝕏x = 1\n";
        let edits = minimal_edits(original, formatted);
        assert_eq!(edits.len(), 1, "{edits:?}");
        // `val ` = 4 units, `𝕏` = 2 units, `x` = 1 unit → the change starts at 7.
        assert_eq!(edits[0].range.start.line, 0);
        assert_eq!(edits[0].range.start.character, 7);
        assert_eq!(apply(original, &edits), formatted);
    }

    // CRLF originals keep correct lines/columns (line-index treats `\r\n` as
    // the terminator; the `\r` bytes stay inside the diff's byte arithmetic).
    #[test]
    fn crlf_originals_map_to_correct_positions() {
        let original = "object A {\r\n  val x=1\r\n}\r\n";
        let formatted = "object A {\r\n  val x = 1\r\n}\r\n";
        let edits = minimal_edits(original, formatted);
        assert!(!edits.is_empty());
        assert!(
            edits.iter().all(|edit| edit.range.start.line == 1),
            "only the middle line changed: {edits:?}"
        );
        assert_eq!(apply(original, &edits), formatted);
    }

    // A whole-file rewrite still round-trips through the fold (the edits may
    // legitimately span everything — the guarantee is correctness, and
    // minimality where content is shared).
    #[test]
    fn a_whole_file_change_still_applies_to_the_formatted_text() {
        let original = "class   Old {\n}\n";
        let formatted = "object New:\n  def fresh: Int = 42\n";
        let edits = minimal_edits(original, formatted);
        assert!(!edits.is_empty());
        assert_eq!(apply(original, &edits), formatted);
    }

    // A pure insertion and a pure deletion each produce the exact single edit.
    #[test]
    fn pure_insert_and_pure_delete_fold_to_single_edits() {
        let original = "object A {\n}\n";
        let with_body = "object A {\n  val x = 1\n}\n";
        let inserts = minimal_edits(original, with_body);
        assert_eq!(inserts.len(), 1, "{inserts:?}");
        assert_eq!(inserts[0].range.start, inserts[0].range.end);
        assert_eq!(apply(original, &inserts), with_body);

        let deletes = minimal_edits(with_body, original);
        assert_eq!(deletes.len(), 1, "{deletes:?}");
        assert_eq!(deletes[0].new_text, "");
        assert_eq!(apply(with_body, &deletes), original);
    }
}
