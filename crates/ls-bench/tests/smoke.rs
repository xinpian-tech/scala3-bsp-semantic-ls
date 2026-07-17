//! The bench harness self-check (mirrors the Scala `BenchSuite`): the tiny
//! corpus must generate, ingest, measure, and pass every ground-truth
//! cross-check. This is the CI gate that keeps the harness itself honest.

use ls_bench::BenchConfig;

#[test]
fn tiny_corpus_runs_and_all_ground_truth_checks_pass() {
    let report = ls_bench::run(&BenchConfig::tiny()).expect("tiny bench pass");
    assert!(
        report.contains("ground truth: all cross-checks passed"),
        "{report}"
    );
    assert!(report.contains("ingest (full generation)"), "{report}");
    assert!(report.contains("references (probe fan-out)"), "{report}");
}

#[test]
fn smoke_corpus_runs_and_all_ground_truth_checks_pass() {
    let report = ls_bench::run(&BenchConfig::smoke()).expect("smoke bench pass");
    assert!(
        report.contains("ground truth: all cross-checks passed"),
        "{report}"
    );
}
