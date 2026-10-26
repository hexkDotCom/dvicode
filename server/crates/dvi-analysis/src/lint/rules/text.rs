//! Rules about the text of a file.

use crate::lint::{Cx, Level, Rule, RuleMeta, Ruleset};
use dvi_syntax::{Span, TokKind};

pub struct TrailingWhitespace;

static TRAILING_WHITESPACE: RuleMeta = RuleMeta {
    id: "trailing-whitespace",
    guide: &["FMT-2"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Info,
    subchecks: &[],
    summary: "A line ends in spaces or tabs.",
    why: "Trailing whitespace is invisible noise in diffs and reviews.",
    params: &[],
};

impl Rule for TrailingWhitespace {
    fn meta(&self) -> &'static RuleMeta {
        &TRAILING_WHITESPACE
    }

    fn check(&self, cx: &mut Cx) {
        let Some(text) = cx.a.file_text(cx.file) else { return };
        let b = text.as_bytes();
        // Whitespace inside a string token is the string's (a string cannot hold a raw newline,
        // but a line continuation can end one line of it).
        let strings: Vec<(u32, u32)> = cx.facts.tokens.iter().filter(|t| t.kind == TokKind::String).map(|t| (t.span.start, t.span.end)).collect();
        let mut line_start = 0usize;
        let mut si = 0usize;
        for i in 0..=b.len() {
            if i < b.len() && b[i] != b'\n' {
                continue;
            }
            if !cx.step(1) {
                return;
            }
            let mut end = i;
            if end > line_start && b[end - 1] == b'\r' {
                end -= 1;
            }
            let mut ws = end;
            while ws > line_start && matches!(b[ws - 1], b' ' | b'\t') {
                ws -= 1;
            }
            if ws < end {
                while si < strings.len() && strings[si].1 <= ws as u32 {
                    si += 1;
                }
                let in_string = si < strings.len() && strings[si].0 <= ws as u32;
                if !in_string {
                    cx.report(None, Span::new(cx.file, ws as u32, end as u32), "Trailing whitespace", Vec::new());
                }
            }
            line_start = i + 1;
        }
    }
}
