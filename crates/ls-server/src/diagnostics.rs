//! The diagnostics router: BSP `build/publishDiagnostics` notifications arrive
//! per (file, build target) with per-target reset semantics; the router merges
//! all targets' diagnostics into a single per-file LSP publish, and suppresses a
//! clearing publish for a file that was never published non-empty.

use std::collections::{HashMap, HashSet};

use ls_bsp::protocol as bsp;

use crate::protocol::{Diagnostic, DiagnosticCode, Position, PublishDiagnosticsParams, Range};

/// Converts a BSP diagnostic to its LSP shape. BSP and LSP share the severity
/// integers (1 Error, 2 Warning, 3 Information, 4 Hint) and the same
/// `integer | string` code, so the code copies through and severity maps via
/// [`to_lsp_severity`].
pub fn to_lsp_diagnostic(d: &bsp::Diagnostic) -> Diagnostic {
    Diagnostic {
        range: d.range.as_ref().map(to_lsp_range).unwrap_or(ORIGIN_RANGE),
        severity: d.severity.map(to_lsp_severity),
        code: d.code.as_ref().map(to_lsp_code),
        source: d.source.clone(),
        message: d.message.clone(),
    }
}

/// BSP and LSP share the severity integers 1..=4 (Error/Warning/Information/
/// Hint). A present-but-unexpected value maps to Information (3), matching the
/// Scala converter's explicit default branch; a missing severity stays unset
/// (the `map` in [`to_lsp_diagnostic`] keeps `None`).
fn to_lsp_severity(bsp: i32) -> u32 {
    match bsp {
        1..=4 => bsp as u32,
        _ => 3,
    }
}

/// A `[0:0, 0:0)` fallback for a BSP diagnostic that carries no range (bsp4j
/// always supplies one; this keeps the LSP `range` non-null defensively).
const ORIGIN_RANGE: Range = Range {
    start: Position {
        line: 0,
        character: 0,
    },
    end: Position {
        line: 0,
        character: 0,
    },
};

fn to_lsp_range(r: &bsp::Range) -> Range {
    Range {
        start: to_lsp_position(&r.start),
        end: to_lsp_position(&r.end),
    }
}

// LSP positions are unsigned; a build server sends non-negative coordinates, so
// a stray negative clamps to 0 rather than emitting an out-of-range position.
fn to_lsp_position(p: &bsp::Position) -> Position {
    Position {
        line: u32::try_from(p.line).unwrap_or(0),
        character: u32::try_from(p.character).unwrap_or(0),
    }
}

fn to_lsp_code(c: &bsp::DiagnosticCode) -> DiagnosticCode {
    match c {
        bsp::DiagnosticCode::Integer(i) => DiagnosticCode::Integer(*i),
        bsp::DiagnosticCode::String(s) => DiagnosticCode::String(s.clone()),
    }
}

type UriMapper = Box<dyn Fn(&str) -> String + Send + Sync>;

/// Merges per-target BSP diagnostics into per-file LSP publishes.
pub struct DiagnosticRouter {
    /// `fileUri -> [(targetUri, diagnostics)]`, insertion-ordered by target so
    /// the merged publish is deterministic.
    by_uri: HashMap<String, Vec<(String, Vec<Diagnostic>)>>,
    /// Files for which a non-empty publish has been emitted, so a later clear is
    /// forwarded exactly once (and a never-published file stays silent).
    published_non_empty: HashSet<String>,
    to_file_uri: UriMapper,
}

impl DiagnosticRouter {
    pub fn new() -> DiagnosticRouter {
        DiagnosticRouter::with_uri_mapper(Box::new(|uri| uri.to_string()))
    }

    /// A router whose incoming build-server URIs are mapped to file URIs before
    /// publishing (e.g. resolving a build-server-relative URI to `file://`).
    pub fn with_uri_mapper(to_file_uri: UriMapper) -> DiagnosticRouter {
        DiagnosticRouter {
            by_uri: HashMap::new(),
            published_non_empty: HashSet::new(),
            to_file_uri,
        }
    }

    /// Accepts one BSP publish and returns the LSP publish to forward to the
    /// client, or `None` when the notification is suppressed (an empty publish
    /// for a file that was never published non-empty).
    pub fn accept(
        &mut self,
        params: &bsp::PublishDiagnosticsParams,
    ) -> Option<PublishDiagnosticsParams> {
        let file_uri = (self.to_file_uri)(&params.text_document.uri);
        let target = params
            .build_target
            .as_ref()
            .map(|t| t.uri.clone())
            .unwrap_or_default();
        let incoming: Vec<Diagnostic> = params.diagnostics.iter().map(to_lsp_diagnostic).collect();

        let union: Vec<Diagnostic> = {
            let per_target = self.by_uri.entry(file_uri.clone()).or_default();
            let pos = per_target.iter().position(|(t, _)| *t == target);
            if params.reset {
                match (pos, incoming.is_empty()) {
                    (Some(i), true) => {
                        per_target.remove(i);
                    }
                    (Some(i), false) => per_target[i].1 = incoming,
                    (None, true) => {}
                    (None, false) => per_target.push((target, incoming)),
                }
            } else {
                match pos {
                    Some(i) => per_target[i].1.extend(incoming),
                    None => per_target.push((target, incoming)),
                }
            }
            per_target
                .iter()
                .flat_map(|(_, ds)| ds.iter().cloned())
                .collect()
        };

        if !union.is_empty() {
            self.published_non_empty.insert(file_uri.clone());
            Some(PublishDiagnosticsParams {
                uri: file_uri,
                diagnostics: union,
            })
        } else {
            self.by_uri.remove(&file_uri);
            if self.published_non_empty.remove(&file_uri) {
                Some(PublishDiagnosticsParams {
                    uri: file_uri,
                    diagnostics: Vec::new(),
                })
            } else {
                None
            }
        }
    }
}

impl Default for DiagnosticRouter {
    fn default() -> Self {
        DiagnosticRouter::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(msg: &str, sev: i32) -> bsp::Diagnostic {
        bsp::Diagnostic {
            range: Some(bsp::Range {
                start: bsp::Position {
                    line: 1,
                    character: 2,
                },
                end: bsp::Position {
                    line: 1,
                    character: 7,
                },
            }),
            severity: Some(sev),
            code: None,
            source: None,
            message: msg.to_string(),
        }
    }

    fn publish(
        uri: &str,
        target: &str,
        reset: bool,
        diags: Vec<bsp::Diagnostic>,
    ) -> bsp::PublishDiagnosticsParams {
        bsp::PublishDiagnosticsParams {
            text_document: bsp::TextDocumentIdentifier {
                uri: uri.to_string(),
            },
            build_target: Some(bsp::BuildTargetIdentifier {
                uri: target.to_string(),
            }),
            diagnostics: diags,
            reset,
            origin_id: None,
        }
    }

    fn messages(p: &PublishDiagnosticsParams) -> Vec<String> {
        p.diagnostics.iter().map(|d| d.message.clone()).collect()
    }

    // Ports ls.core.DiagnosticRouterSuite.
    #[test]
    fn single_publish_reaches_the_sink_with_converted_content() {
        let mut router = DiagnosticRouter::new();
        let out = router
            .accept(&publish(
                "file:///a.scala",
                "t/a",
                true,
                vec![diag("boom", 1)],
            ))
            .expect("a non-empty publish");
        assert_eq!(out.uri, "file:///a.scala");
        assert_eq!(out.diagnostics.len(), 1);
        let d = &out.diagnostics[0];
        assert_eq!(d.message, "boom");
        assert_eq!(d.severity, Some(1));
        assert_eq!(d.range.start.line, 1);
        assert_eq!(d.range.start.character, 2);
        assert_eq!(d.range.end.character, 7);
    }

    #[test]
    fn two_targets_on_one_uri_merge_into_one_publish() {
        let mut router = DiagnosticRouter::new();
        router.accept(&publish(
            "file:///a.scala",
            "t/a",
            true,
            vec![diag("from-a", 1)],
        ));
        let out = router
            .accept(&publish(
                "file:///a.scala",
                "t/b",
                true,
                vec![diag("from-b", 2)],
            ))
            .unwrap();
        let msgs: std::collections::HashSet<String> = messages(&out).into_iter().collect();
        assert_eq!(
            msgs,
            ["from-a".to_string(), "from-b".to_string()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn reset_replaces_a_targets_diagnostics() {
        let mut router = DiagnosticRouter::new();
        router.accept(&publish(
            "file:///a.scala",
            "t/a",
            true,
            vec![diag("old", 1)],
        ));
        let out = router
            .accept(&publish(
                "file:///a.scala",
                "t/a",
                true,
                vec![diag("new", 1)],
            ))
            .unwrap();
        assert_eq!(messages(&out), vec!["new".to_string()]);
    }

    #[test]
    fn clearing_one_target_does_not_clear_a_sibling() {
        let mut router = DiagnosticRouter::new();
        router.accept(&publish(
            "file:///a.scala",
            "t/a",
            true,
            vec![diag("from-a", 1)],
        ));
        router.accept(&publish(
            "file:///a.scala",
            "t/b",
            true,
            vec![diag("from-b", 2)],
        ));
        let out = router
            .accept(&publish("file:///a.scala", "t/a", true, vec![]))
            .unwrap();
        assert_eq!(messages(&out), vec!["from-b".to_string()]);
    }

    #[test]
    fn clearing_the_only_target_publishes_an_empty_list() {
        let mut router = DiagnosticRouter::new();
        router.accept(&publish(
            "file:///a.scala",
            "t/a",
            true,
            vec![diag("boom", 1)],
        ));
        let out = router
            .accept(&publish("file:///a.scala", "t/a", true, vec![]))
            .expect("a clearing publish");
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn empty_publish_for_an_already_clean_uri_does_not_publish() {
        let mut router = DiagnosticRouter::new();
        let out = router.accept(&publish("file:///a.scala", "t/a", true, vec![]));
        assert!(out.is_none());
    }

    #[test]
    fn non_reset_diagnostics_accumulate_for_a_target() {
        let mut router = DiagnosticRouter::new();
        router.accept(&publish(
            "file:///a.scala",
            "t/a",
            true,
            vec![diag("one", 1)],
        ));
        let out = router
            .accept(&publish(
                "file:///a.scala",
                "t/a",
                false,
                vec![diag("two", 1)],
            ))
            .unwrap();
        assert_eq!(messages(&out), vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn to_file_uri_hook_is_applied_to_the_published_uri() {
        let mut router =
            DiagnosticRouter::with_uri_mapper(Box::new(|_| "file:///mapped.scala".to_string()));
        let out = router
            .accept(&publish("bsp:///whatever", "t/a", true, vec![diag("x", 1)]))
            .unwrap();
        assert_eq!(out.uri, "file:///mapped.scala");
    }

    // Ports ls.core.LspConvertDiagnosticSuite.
    #[test]
    fn converts_range_message_severity_and_code() {
        let d = bsp::Diagnostic {
            range: Some(bsp::Range {
                start: bsp::Position {
                    line: 3,
                    character: 4,
                },
                end: bsp::Position {
                    line: 5,
                    character: 6,
                },
            }),
            severity: Some(2),
            code: Some(bsp::DiagnosticCode::String("E123".to_string())),
            source: None,
            message: "hi".to_string(),
        };
        let out = to_lsp_diagnostic(&d);
        assert_eq!(out.message, "hi");
        assert_eq!(out.range.start.line, 3);
        assert_eq!(out.range.start.character, 4);
        assert_eq!(out.range.end.line, 5);
        assert_eq!(out.range.end.character, 6);
        assert_eq!(out.severity, Some(2));
        assert_eq!(out.code, Some(DiagnosticCode::String("E123".to_string())));
    }

    #[test]
    fn missing_severity_stays_none() {
        let d = bsp::Diagnostic {
            range: Some(bsp::Range {
                start: bsp::Position {
                    line: 0,
                    character: 0,
                },
                end: bsp::Position {
                    line: 0,
                    character: 1,
                },
            }),
            severity: None,
            code: None,
            source: None,
            message: "m".to_string(),
        };
        assert_eq!(to_lsp_diagnostic(&d).severity, None);
    }

    #[test]
    fn valid_severities_pass_through_and_out_of_range_maps_to_information() {
        let base = bsp::Range {
            start: bsp::Position {
                line: 0,
                character: 0,
            },
            end: bsp::Position {
                line: 0,
                character: 1,
            },
        };
        let with_sev = |sev: Option<i32>| bsp::Diagnostic {
            range: Some(base.clone()),
            severity: sev,
            code: None,
            source: None,
            message: "m".to_string(),
        };
        for sev in 1..=4 {
            assert_eq!(
                to_lsp_diagnostic(&with_sev(Some(sev))).severity,
                Some(sev as u32)
            );
        }
        // A present value outside the 1..=4 enum falls to Information (3), like
        // the Scala converter's default branch; a missing severity stays unset.
        assert_eq!(to_lsp_diagnostic(&with_sev(Some(0))).severity, Some(3));
        assert_eq!(to_lsp_diagnostic(&with_sev(Some(5))).severity, Some(3));
        assert_eq!(to_lsp_diagnostic(&with_sev(Some(-1))).severity, Some(3));
        assert_eq!(to_lsp_diagnostic(&with_sev(None)).severity, None);
    }
}
