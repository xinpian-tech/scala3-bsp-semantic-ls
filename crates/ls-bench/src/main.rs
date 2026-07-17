//! `ls-bench` CLI.
//!
//! ```text
//!   --smoke   small corpus, finishes in seconds (CI gate; the default)
//!   --tiny    minimal corpus (harness self-check)
//!   --full    bigger corpus for real measurements
//! ```
//!
//! Exits non-zero on any ground-truth inconsistency: a benchmark that answers
//! wrongly measures nothing.

use std::process::ExitCode;

use ls_bench::BenchConfig;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = if args.iter().any(|a| a == "--full") {
        BenchConfig::full()
    } else if args.iter().any(|a| a == "--tiny") {
        BenchConfig::tiny()
    } else if args.is_empty() || args.iter().any(|a| a == "--smoke") {
        BenchConfig::smoke()
    } else {
        eprintln!("usage: ls-bench [--smoke | --tiny | --full]");
        return ExitCode::FAILURE;
    };
    match ls_bench::run(&cfg) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(inconsistency) => {
            eprintln!("ls-bench: ground-truth inconsistency: {inconsistency}");
            ExitCode::FAILURE
        }
    }
}
