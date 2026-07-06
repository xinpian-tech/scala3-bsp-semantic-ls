//! Command-line argument parsing. `--version` and `--doctor [dir]` are handled
//! before the server starts; empty arguments start the stdio server; anything
//! else is a usage error rather than a silent start. The former PC-backend
//! selection flags (`--in-process-pc`/`--forked-pc`) and the AOT-training entry
//! point are gone, so they now parse as unknown arguments.

use std::path::{Path, PathBuf};

/// The action selected by the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliAction {
    /// Print the server name and version, then exit.
    Version,
    /// Print the offline doctor report for `dir`, then exit. `dir` is the raw
    /// argument (or `.`); the entry point resolves it to an absolute, normalized
    /// path with [`resolve_doctor_dir`] before rendering the report.
    Doctor { dir: PathBuf },
    /// Start the LSP server over stdio.
    Serve,
    /// Print a usage error and exit non-zero.
    Usage { message: String },
}

/// Resolves a `--doctor` directory the way the entry point does: made absolute
/// against `cwd` when relative, then lexically collapsed — matching the Scala
/// `Path.of(dir).toAbsolutePath.normalize`. `cwd` is the process working
/// directory (passed explicitly so the resolution stays pure and testable).
pub fn resolve_doctor_dir(dir: &Path, cwd: &Path) -> PathBuf {
    let absolute = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        cwd.join(dir)
    };
    ls_index_model::uri::normalize(&absolute)
}

/// Parses the process arguments (excluding the program name) into a [`CliAction`].
pub fn parse_args(args: &[String]) -> CliAction {
    // `--version` takes precedence over everything else.
    if args.iter().any(|a| a == "--version") {
        return CliAction::Version;
    }
    // `--doctor` optionally takes the workspace directory as the next argument,
    // defaulting to the current directory.
    if let Some(i) = args.iter().position(|a| a == "--doctor") {
        let dir = args
            .get(i + 1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        return CliAction::Doctor { dir };
    }
    // With no action flag, only an empty argument list starts the server; any
    // leftover argument is unrecognized and rejected (never a silent start).
    if args.is_empty() {
        CliAction::Serve
    } else {
        CliAction::Usage {
            message: format!("unknown arguments: {}", args.join(" ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // Ports ls.core.Main argument handling, with the removed PC-backend flags
    // and the unknown-argument rejection.
    #[test]
    fn version_flag_selects_version() {
        assert_eq!(parse_args(&args(&["--version"])), CliAction::Version);
    }

    #[test]
    fn version_wins_over_other_arguments() {
        assert_eq!(
            parse_args(&args(&["--doctor", "--version"])),
            CliAction::Version
        );
        assert_eq!(
            parse_args(&args(&["--version", "--anything"])),
            CliAction::Version
        );
    }

    #[test]
    fn doctor_without_a_dir_defaults_to_the_current_directory() {
        assert_eq!(
            parse_args(&args(&["--doctor"])),
            CliAction::Doctor {
                dir: PathBuf::from(".")
            }
        );
    }

    #[test]
    fn doctor_takes_the_following_directory_argument() {
        assert_eq!(
            parse_args(&args(&["--doctor", "/tmp/ws"])),
            CliAction::Doctor {
                dir: PathBuf::from("/tmp/ws")
            }
        );
    }

    #[test]
    fn no_arguments_starts_the_server() {
        assert_eq!(parse_args(&[]), CliAction::Serve);
    }

    #[test]
    fn removed_pc_backend_flags_are_now_usage_errors() {
        assert!(matches!(
            parse_args(&args(&["--forked-pc"])),
            CliAction::Usage { .. }
        ));
        assert!(matches!(
            parse_args(&args(&["--in-process-pc"])),
            CliAction::Usage { .. }
        ));
        assert!(matches!(
            parse_args(&args(&["--aot-train", "/x"])),
            CliAction::Usage { .. }
        ));
    }

    #[test]
    fn an_unknown_flag_is_a_usage_error_not_a_silent_serve() {
        match parse_args(&args(&["--bogus"])) {
            CliAction::Usage { message } => assert!(message.contains("--bogus"), "{message}"),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    // Mirrors Main.scala computing the doctor root as
    // Path.of(dir).toAbsolutePath.normalize.
    #[test]
    fn doctor_dir_is_absolutized_against_the_cwd_and_normalized() {
        let cwd = Path::new("/home/u/ws");
        assert_eq!(
            resolve_doctor_dir(Path::new("."), cwd),
            PathBuf::from("/home/u/ws")
        );
        assert_eq!(
            resolve_doctor_dir(Path::new("../other/./sub"), cwd),
            PathBuf::from("/home/u/other/sub")
        );
    }

    #[test]
    fn doctor_dir_absolute_input_is_only_normalized() {
        assert_eq!(
            resolve_doctor_dir(Path::new("/srv/a/../b"), Path::new("/ignored")),
            PathBuf::from("/srv/b")
        );
    }
}
