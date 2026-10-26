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
    // A line index, and the position up to which it still holds: lines before an edit keep
    // their starts. VS Code sends a multi-cursor edit's changes last first, so one index
    // serves them all instead of one scan of the document each.
    // The index, and the last line whose start it still holds (the line of the last change:
    // its start is unchanged, its end may not be).
    let mut index: Option<(LineIndex, u32)> = None;
    for c in changes {
        match c.range {
            None => {
                text = c.text;
                index = None;
            }
            Some(r) => {
                let holds = index.as_ref().is_some_and(|(_, upto)| r.end.line <= *upto);
                if !holds {
                    index = Some((LineIndex::new(&text), u32::MAX));
                }
                let Some((li, upto)) = index.as_mut() else { continue };
                let s = offset_in(&text, li, r.start.line, r.start.character);
                let e = offset_in(&text, li, r.end.line, r.end.character);
                let (s, e) = (s.min(text.len()), e.min(text.len()).max(s.min(text.len())));
                text.replace_range(s..e, &c.text);
                // Lines up to the edited one keep their starts; columns are read in the text.
                *upto = r.start.line;
            }
        }
    }
    text
}

/// The byte offset of a line and UTF-16 column: the line's start from `li`, its columns read
/// in `text` itself (a change on the line before this one moved its end); clamped to the line.
fn offset_in(text: &str, li: &LineIndex, line: u32, col: u32) -> usize {
    if line >= li.line_count() {
        return text.len();
    }
    let start = (li.line_start(line) as usize).min(text.len());
    let mut units = 0u32;
    for (i, ch) in text[start..].char_indices() {
        if ch == '\n' || units >= col {
            return start + i;
        }
        units += ch.len_utf16() as u32;
    }
    text.len()
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

    #[test]
    fn several_changes_apply_one_after_the_other_in_any_order() {
        let t = "a = 1;\nb = 2;\nc = 3;\n";
        let ch = |l: u32, c0: u32, c1: u32, s: &str| TextDocumentContentChangeEvent { range: Some(Range::new(Position::new(l, c0), Position::new(l, c1))), range_length: None, text: s.into() };
        // Last first (as a multi-cursor edit comes), with line breaks inserted.
        assert_eq!(apply_changes(t, vec![ch(2, 0, 1, "cc\nx"), ch(1, 0, 1, "bb"), ch(0, 4, 5, "10\n")]), "a = 10\n;\nbb = 2;\ncc\nx = 3;\n");
        // First first: each later range is in the text the earlier changes made.
        assert_eq!(apply_changes(t, vec![ch(0, 4, 5, "10\n"), ch(2, 0, 2, "bb"), ch(3, 0, 1, "cc")]), "a = 10\n;\nbb= 2;\ncc = 3;\n");
        // Two on one line, the right one first.
        assert_eq!(apply_changes(t, vec![ch(1, 4, 5, "20"), ch(1, 0, 1, "B")]), "a = 1;\nB = 20;\nc = 3;\n");        // Two on one line with a character beyond ASCII before them (columns in UTF-16).
        assert_eq!(apply_changes("// \u{e9}abc\n", vec![ch(0, 4, 7, ""), ch(0, 3, 4, "e")]), "// e\n");
    }
}
