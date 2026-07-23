//! The process-wide log sink behind the `log` facade — the ONE place log lines
//! are formatted and written. Library crates (`ls-bsp`, `ls-jvm`) and every
//! module here speak only the facade macros with an area target
//! (`serve`|`boot`|`bsp`|`bsp-err`|`pc`|`index`|`watch`|`fmt`); this module
//! renders each record as
//!
//! ```text
//! [+SSS.mmm LEVEL area] message
//! ```
//!
//! where `+SSS.mmm` is the monotonic elapsed time since process start — the
//! analysis axis a stuck user reads to see where the lifecycle stalled.
//!
//! stderr is the log channel by design (stdout carries only protocol frames).
//! `LS_LOG` selects the level (`error|warn|info|debug`, default `info`);
//! `LS_LOG_FILE=<path>` ADDITIONALLY appends every line to a file — the escape
//! hatch for editors (e.g. nvim) that swallow LSP server stderr. The file sink
//! is write-through and best-effort: after one warning, file errors are
//! ignored so logging can never take the server down.
//!
//! One startup banner line is always printed — even under `LS_LOG=error` — so
//! a captured stderr/log file always identifies the process: version, pid,
//! wallclock UTC start, argv mode, and the effective `LS_LOG` level + file.
//!
//! Deliberately not `env_logger`: the whole need is one line format, one env
//! level, and one optional file tee — a dependency would bring filters,
//! styles, and regex parsing this server must never grow.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Metadata, Record};

use crate::capabilities::{SERVER_NAME, SERVER_VERSION};

/// The level `LS_LOG` selects; anything unrecognized (and an unset variable)
/// is the default `info` — a misspelt value must degrade to the useful
/// default, never to silence.
pub(crate) fn parse_level(value: Option<&str>) -> LevelFilter {
    match value.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("error") => LevelFilter::Error,
        Some(v) if v.eq_ignore_ascii_case("warn") => LevelFilter::Warn,
        Some(v) if v.eq_ignore_ascii_case("debug") => LevelFilter::Debug,
        _ => LevelFilter::Info,
    }
}

/// The stderr (+ optional file) sink. Constructed once by [`init`]; also
/// constructed directly by unit tests, which exercise the formatting, level
/// filtering, and file tee without touching the process-global logger.
pub(crate) struct LogSink {
    /// Process start — the zero of the `+SSS.mmm` elapsed axis.
    start: Instant,
    level: LevelFilter,
    /// The optional `LS_LOG_FILE` tee (append, write-through).
    file: Mutex<Option<File>>,
    /// Set after the first file-write failure so the warning prints once and
    /// later failures are ignored silently.
    file_warned: AtomicBool,
}

impl LogSink {
    pub(crate) fn new(level: LevelFilter, file_path: Option<&Path>) -> LogSink {
        let file = file_path.and_then(|path| {
            match OpenOptions::new().create(true).append(true).open(path) {
                Ok(file) => Some(file),
                Err(error) => {
                    eprintln!(
                        "{SERVER_NAME}: cannot open LS_LOG_FILE {}: {error} — \
                         logging to stderr only",
                        path.display()
                    );
                    None
                }
            }
        });
        LogSink {
            start: Instant::now(),
            level,
            file: Mutex::new(file),
            file_warned: AtomicBool::new(false),
        }
    }

    /// Renders one record as the canonical line (no trailing newline).
    pub(crate) fn line_for(&self, record: &Record) -> String {
        format_line(
            self.start.elapsed(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        )
    }

    /// Writes one already-formatted line to stderr AND (write-through) to the
    /// `LS_LOG_FILE` tee. Errors on either sink can never propagate: stderr
    /// failures are ignored (there is nowhere left to report), file failures
    /// warn once and then go silent.
    pub(crate) fn write_line(&self, line: &str) {
        {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{line}");
        }
        let mut guard = self
            .file
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(file) = guard.as_mut() {
            if writeln!(file, "{line}").is_err() && !self.file_warned.swap(true, Ordering::SeqCst) {
                eprintln!("{SERVER_NAME}: writing LS_LOG_FILE failed; further file-log errors are ignored");
            }
        }
    }

    /// The level-proof banner: composed and written outside the facade so it
    /// prints even under `LS_LOG=error`.
    pub(crate) fn emit_banner(&self, mode: &str, file_path: Option<&Path>) {
        self.write_line(&banner_line(SystemTime::now(), mode, self.level, file_path));
    }
}

impl log::Log for LogSink {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.write_line(&self.line_for(record));
        }
    }

    fn flush(&self) {}
}

/// `[+SSS.mmm LEVEL area] message` — the one line shape every sink writes.
/// The elapsed stamp is seconds.milliseconds since process start.
pub(crate) fn format_line(elapsed: Duration, level: Level, area: &str, message: &str) -> String {
    format!(
        "[+{}.{:03} {level} {area}] {message}",
        elapsed.as_secs(),
        elapsed.subsec_millis(),
    )
}

/// The startup banner line (always printed, at the `+0.000`-ish start of the
/// stream): server identity, pid, wallclock UTC start (so the monotonic
/// elapsed stamps can be correlated with real time), the argv mode, and the
/// effective log configuration.
pub(crate) fn banner_line(
    started_at: SystemTime,
    mode: &str,
    level: LevelFilter,
    file: Option<&Path>,
) -> String {
    format!(
        "[+0.000 INFO serve] {SERVER_NAME} {SERVER_VERSION} pid={} started {} mode={mode} \
         LS_LOG={} LS_LOG_FILE={}",
        std::process::id(),
        utc_rfc3339(started_at),
        level.as_str().to_ascii_lowercase(),
        file.map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unset)".to_string()),
    )
}

/// `SystemTime` -> `YYYY-MM-DDTHH:MM:SSZ` without a chrono dependency: the
/// standard days-to-civil conversion (Howard Hinnant's `civil_from_days`).
fn utc_rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year_of_era = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 {
        year_of_era + 1
    } else {
        year_of_era
    };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Installs the process-wide sink from the `LS_LOG`/`LS_LOG_FILE` environment
/// and prints the banner. Called once from `main` before any subsystem runs;
/// idempotent for tests (a second install keeps the first logger and only
/// re-prints a banner).
pub fn init(mode: &str) {
    let level = parse_level(std::env::var("LS_LOG").ok().as_deref());
    let file_path = std::env::var("LS_LOG_FILE").ok().map(PathBuf::from);
    let sink = LogSink::new(level, file_path.as_deref());
    sink.emit_banner(mode, file_path.as_deref());
    log::set_max_level(level);
    // A second init (only reachable from tests — main runs once) keeps the
    // installed logger; the boxed sink is dropped harmlessly.
    let _ = log::set_boxed_logger(Box::new(sink));
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log;

    fn record<'a>(level: Level, target: &'a str, args: std::fmt::Arguments<'a>) -> Record<'a> {
        Record::builder()
            .level(level)
            .target(target)
            .args(args)
            .build()
    }

    #[test]
    fn ls_log_parses_the_four_levels_and_defaults_to_info() {
        assert_eq!(parse_level(Some("error")), LevelFilter::Error);
        assert_eq!(parse_level(Some("WARN")), LevelFilter::Warn);
        assert_eq!(parse_level(Some("info")), LevelFilter::Info);
        assert_eq!(parse_level(Some("debug")), LevelFilter::Debug);
        assert_eq!(parse_level(None), LevelFilter::Info);
        // Unrecognized degrades to the useful default, never to silence.
        assert_eq!(parse_level(Some("trace")), LevelFilter::Info);
        assert_eq!(parse_level(Some("banana")), LevelFilter::Info);
    }

    #[test]
    fn the_line_format_is_elapsed_level_area_message() {
        let line = format_line(
            Duration::from_millis(12_345),
            Level::Warn,
            "boot",
            "still waiting",
        );
        assert_eq!(line, "[+12.345 WARN boot] still waiting");
        let line = format_line(Duration::from_millis(7), Level::Info, "serve", "x");
        assert_eq!(line, "[+0.007 INFO serve] x");
    }

    #[test]
    fn the_sink_filters_by_level_and_keeps_the_area_target() {
        let sink = LogSink::new(LevelFilter::Warn, None);
        assert!(sink.enabled(&Metadata::builder().level(Level::Error).build()));
        assert!(sink.enabled(&Metadata::builder().level(Level::Warn).build()));
        assert!(!sink.enabled(&Metadata::builder().level(Level::Info).build()));
        assert!(!sink.enabled(&Metadata::builder().level(Level::Debug).build()));

        // The record's target is the area between level and message.
        let line = sink.line_for(&record(Level::Warn, "bsp-err", format_args!("boom")));
        assert!(line.contains(" WARN bsp-err] boom"), "{line}");
        assert!(line.starts_with("[+"), "{line}");
    }

    #[test]
    fn every_line_is_duplicated_into_the_ls_log_file_tee() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ls.log");
        let sink = LogSink::new(LevelFilter::Info, Some(&path));
        sink.log(&record(Level::Info, "index", format_args!("reindex done")));
        // Below the level threshold: neither sink receives it.
        sink.log(&record(Level::Debug, "serve", format_args!("hidden")));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("INFO index] reindex done"), "{text}");
        assert!(!text.contains("hidden"), "{text}");
    }

    #[test]
    fn the_banner_always_prints_even_at_error_level() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ls.log");
        let sink = LogSink::new(LevelFilter::Error, Some(&path));
        sink.emit_banner("serve", Some(&path));
        // An info record is filtered at LS_LOG=error; the banner is not.
        sink.log(&record(Level::Info, "serve", format_args!("filtered out")));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(SERVER_NAME), "{text}");
        assert!(
            text.contains(&format!("pid={}", std::process::id())),
            "{text}"
        );
        assert!(text.contains("mode=serve"), "{text}");
        assert!(text.contains("LS_LOG=error"), "{text}");
        assert!(!text.contains("filtered out"), "{text}");
    }

    #[test]
    fn the_banner_names_version_mode_wallclock_and_log_config() {
        let line = banner_line(
            UNIX_EPOCH,
            "doctor",
            LevelFilter::Debug,
            Some(Path::new("/tmp/x.log")),
        );
        assert!(line.contains(SERVER_VERSION), "{line}");
        assert!(line.contains("started 1970-01-01T00:00:00Z"), "{line}");
        assert!(line.contains("mode=doctor"), "{line}");
        assert!(line.contains("LS_LOG=debug"), "{line}");
        assert!(line.contains("LS_LOG_FILE=/tmp/x.log"), "{line}");
        let unset = banner_line(UNIX_EPOCH, "serve", LevelFilter::Info, None);
        assert!(unset.contains("LS_LOG_FILE=(unset)"), "{unset}");
    }

    #[test]
    fn utc_rendering_matches_known_instants() {
        assert_eq!(utc_rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // 2026-07-01T12:30:05Z == 1782909005 (leap-day arithmetic exercised).
        let t = UNIX_EPOCH + Duration::from_secs(1_782_909_005);
        assert_eq!(utc_rfc3339(t), "2026-07-01T12:30:05Z");
    }

    #[test]
    fn an_unopenable_log_file_degrades_to_stderr_only() {
        let sink = LogSink::new(
            LevelFilter::Info,
            Some(Path::new("/nonexistent-dir/nope/ls.log")),
        );
        // No panic, no error propagation — the record just goes to stderr.
        sink.log(&record(Level::Info, "serve", format_args!("still logging")));
    }
}
