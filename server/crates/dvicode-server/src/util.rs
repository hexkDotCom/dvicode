//! Conversions between analysis results and LSP types.

use dvi_analysis::Analysis;
use dvi_syntax::{Diag, LineIndex, Severity};
use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, TextDocumentContentChangeEvent};

pub fn to_lsp_diag(a: &Analysis, d: &Diag) -> Option<Diagnostic> {
    let range = a.to_range(d.span)?;
    Some(Diagnostic {
        range,
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
            Severity::Info => DiagnosticSeverity::INFORMATION,
            Severity::Hint => DiagnosticSeverity::HINT,
        }),
        code: Some(NumberOrString::String(d.code.to_string())),
        code_description: None,
        source: Some("dvicode".into()),
        message: d.message.strip_prefix(&format!("{}: ", d.code)).unwrap_or(&d.message).to_string(),
        related_information: None,
        tags: None,
        data: None,
    })
}

/// Applies LSP content changes (UTF-16 positions) to a text.
pub fn apply_changes(text: &str, changes: Vec<TextDocumentContentChangeEvent>) -> String {
    let mut text = text.to_string();
    for c in changes {
        match c.range {
            None => text = c.text,
            Some(r) => {
                let li = LineIndex::new(&text);
                let s = li.offset(&text, r.start.line, r.start.character) as usize;
                let e = li.offset(&text, r.end.line, r.end.character) as usize;
                let (s, e) = (s.min(text.len()), e.min(text.len()).max(s.min(text.len())));
                text.replace_range(s..e, &c.text);
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};

    #[test]
    fn incremental_edits() {
        let t = "module a;\nendmodule\n";
        let ch = vec![TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(0, 7), Position::new(0, 8))),
            range_length: None,
            text: "top".into(),
        }];
        assert_eq!(apply_changes(t, ch), "module top;\nendmodule\n");
    }
}
