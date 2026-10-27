//! Rules about the text and layout of a file (style guide FMT-2, FMT-3, FMT-6, FMT-7, DOC-6).

use crate::lint::{Cx, Level, ParamMeta, Rule, RuleMeta, Ruleset};
use dvi_syntax::{Span, TokKind};

pub struct AsciiOnly;
pub struct FinalNewline;
pub struct LineLength;
pub struct EndLabel;
pub struct EndComment;
pub struct TodoFormat;

static ASCII_ONLY: RuleMeta = RuleMeta {
    id: "ascii-only",
    guide: &["FMT-2"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "A line holds a character outside ASCII.",
    why: "SystemVerilog tools differ in how they read other encodings; a stray non-breaking space or typographic quote breaks some of them and is invisible in review.",
    params: &[],
    text_only: true,
};

static FINAL_NEWLINE: RuleMeta = RuleMeta {
    id: "final-newline",
    guide: &["FMT-2"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "The file does not end with exactly one newline.",
    why: "A missing final newline joins the last line with whatever follows it (an `include, a concatenated file list); extra blank lines are noise.",
    params: &[],
    text_only: true,
};

static LINE_LENGTH: RuleMeta = RuleMeta {
    id: "line-length",
    guide: &["FMT-3"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Info,
    subchecks: &[],
    summary: "A line is longer than the limit.",
    why: "Long lines are hard to read side by side in reviews and diffs.",
    params: &[
        ParamMeta { name: "max", doc: "the longest line allowed, in characters", min: 40, max: 1000, default: 120 },
        ParamMeta { name: "tab-width", doc: "the columns a tab counts for", min: 1, max: 16, default: 4 },
    ],
    text_only: true,
};

static END_LABEL: RuleMeta = RuleMeta {
    id: "end-label",
    guide: &["FMT-6"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "A class, function, task, module, interface, package, covergroup, property, sequence or clocking block ends without a label.",
    why: "`endclass : bus_driver` is checked by the compiler; a comment is not.",
    params: &[],
    text_only: false,
};

static END_COMMENT: RuleMeta = RuleMeta {
    id: "end-comment",
    guide: &["FMT-7"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "A comment follows `end`.",
    why: "End comments (`end // while (...)`) go stale when the code above changes; nothing checks them.",
    params: &[],
    text_only: true,
};

static TODO_FORMAT: RuleMeta = RuleMeta {
    id: "todo-format",
    guide: &["DOC-6"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "A TODO is not written `TODO:<owner>:<P1|P2|P3>: <text>`.",
    why: "A fixed form makes TODOs greppable and summarizable by owner and priority, and P1s can be checked before sign-off.",
    params: &[],
    text_only: true,
};

/// The keywords that close what FMT-6 lists.
const LABELED_ENDS: &[&str] = &["endclass", "endfunction", "endtask", "endmodule", "endinterface", "endpackage", "endgroup", "endproperty", "endsequence", "endclocking"];

impl Rule for AsciiOnly {
    fn meta(&self) -> &'static RuleMeta {
        &ASCII_ONLY
    }

    fn check(&self, cx: &mut Cx) {
        let Some(text) = cx.a.file_text(cx.file) else { return };
        let mut line_start = 0usize;
        let mut reported = false;
        for (i, c) in text.char_indices() {
            if c == '\n' {
                line_start = i + 1;
                reported = false;
                continue;
            }
            if !c.is_ascii() && !reported {
                if !cx.step(1) {
                    return;
                }
                reported = true;
                let _ = line_start;
                cx.report(None, Span::new(cx.file, i as u32, (i + c.len_utf8()) as u32), format!("Character U+{:04X} is not ASCII", c as u32), Vec::new());
            }
        }
    }
}

impl Rule for FinalNewline {
    fn meta(&self) -> &'static RuleMeta {
        &FINAL_NEWLINE
    }

    fn check(&self, cx: &mut Cx) {
        let Some(text) = cx.a.file_text(cx.file) else { return };
        if text.is_empty() {
            return;
        }
        let end = text.len() as u32;
        if !text.ends_with('\n') {
            cx.report(None, Span::new(cx.file, end, end), "The file does not end with a newline", Vec::new());
        } else if text.ends_with("\n\n") || text.ends_with("\n\r\n") {
            let body = text.trim_end_matches(['\n', '\r']);
            cx.report(None, Span::new(cx.file, body.len() as u32, end), "The file ends with blank lines", Vec::new());
        }
    }
}

impl Rule for LineLength {
    fn meta(&self) -> &'static RuleMeta {
        &LINE_LENGTH
    }

    fn check(&self, cx: &mut Cx) {
        let Some(text) = cx.a.file_text(cx.file) else { return };
        let (max, tab) = (cx.param("max") as usize, cx.param("tab-width") as usize);
        let mut start = 0usize;
        for line in text.split('\n') {
            if !cx.step(1) {
                return;
            }
            let l = line.strip_suffix('\r').unwrap_or(line);
            let mut cols = 0usize;
            for c in l.chars() {
                cols = if c == '\t' { (cols / tab + 1) * tab } else { cols + 1 };
            }
            if cols > max {
                cx.report(None, Span::new(cx.file, start as u32, (start + l.len()) as u32), format!("This line is {cols} characters long (the limit is {max})"), Vec::new());
            }
            start += line.len() + 1;
        }
    }
}

impl Rule for EndLabel {
    fn meta(&self) -> &'static RuleMeta {
        &END_LABEL
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        for (k, x) in t.iter().enumerate() {
            if x.kind != TokKind::Keyword || !LABELED_ENDS.contains(&x.text.as_str()) || facts.in_define(k) {
                continue;
            }
            if !cx.step(1) {
                return;
            }
            if !t.get(k + 1).is_some_and(|y| y.is_punct(":")) {
                cx.report(None, x.span, format!("`{}` has no label: write `{} : <name>`", x.text, x.text), Vec::new());
            }
        }
    }
}

impl Rule for EndComment {
    fn meta(&self) -> &'static RuleMeta {
        &END_COMMENT
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        let Some(text) = cx.a.file_text(cx.file) else { return };
        for c in &facts.comments {
            // On the line of the token before it, which is an `end`.
            let Some(prev) = c.next.checked_sub(1).and_then(|k| t.get(k)) else { continue };
            if !(prev.is_kw("end") || prev.is_kw("join") || prev.is_kw("join_any") || prev.is_kw("join_none") || prev.is_kw("endcase")) {
                continue;
            }
            if text[prev.span.end as usize..c.span.start as usize].contains('\n') {
                continue;
            }
            cx.report(None, c.span, format!("A comment after `{}`: remove it (end comments go stale)", prev.text), Vec::new());
        }
    }
}

impl Rule for TodoFormat {
    fn meta(&self) -> &'static RuleMeta {
        &TODO_FORMAT
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let Some(text) = cx.a.file_text(cx.file) else { return };
        for c in &facts.comments {
            let s = &text[c.span.start as usize..c.span.end as usize];
            let mut from = 0;
            while let Some(i) = s[from..].find("TODO") {
                let at = from + i;
                from = at + 4;
                // A word of its own (not TODOS, not MY_TODO).
                let before = s[..at].chars().next_back();
                let after = s[at + 4..].chars().next();
                if before.is_some_and(|b| b.is_ascii_alphanumeric() || b == '_') || after.is_some_and(|a| a.is_ascii_alphanumeric() || a == '_') {
                    continue;
                }
                if !well_formed(&s[at..]) {
                    let start = c.span.start + at as u32;
                    cx.report(None, Span::new(cx.file, start, start + 4), "Write TODOs as `TODO:<owner>:<P1|P2|P3>: <text>`", Vec::new());
                }
            }
        }
    }
}

/// `TODO:<owner>:<P1|P2|P3>: <text>`.
fn well_formed(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("TODO:") else { return false };
    let Some((owner, rest)) = rest.split_once(':') else { return false };
    if owner.is_empty() || !owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return false;
    }
    let Some((prio, text)) = rest.split_once(':') else { return false };
    matches!(prio, "P1" | "P2" | "P3") && text.starts_with(' ') && !text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_forms() {
        assert!(well_formed("TODO:rg:P1: handle concurrent interrupts"));
        assert!(!well_formed("TODO: handle it"));
        assert!(!well_formed("TODO:rg:P4: x"));
        assert!(!well_formed("TODO:rg:P1:x"));
        assert!(!well_formed("TODO:rg:P1: "));
    }
}
