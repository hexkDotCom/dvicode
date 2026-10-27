//! Waivers in the code, as the HEXK style guide writes them (§0):
//!
//! ```text
//! // LNG-10 waived: the constraint solver is not used in this smoke test
//! void'(item.randomize());
//! ```
//!
//! A waiver names a guide rule or a lint rule and covers the statement or declaration that
//! follows it (other comments and attributes may stand between). Tool suppressions
//! (`// dvicode-lint: disable=<rule> <reason>` ... `// dvicode-lint: enable=<rule>`,
//! `// dvicode-lint: disable-file=<rule> <reason>`) turn rules off for a region; they are not
//! the guide's waiver record.

use super::config::FileConfig;
use super::facts::FileFacts;
use super::Finding;
use crate::ide::stmt;
use crate::world::{Analysis, UnitId};
use dvi_syntax::{FileId, Span, TokKind, Token};

/// The waivers and suppressions of a file.
#[derive(Clone, Debug, Default)]
pub struct Waivers {
    pub items: Vec<Waiver>,
    /// Suppressed regions, one per ID: the ID, the byte range, and the comment that opened it.
    pub regions: Vec<(String, u32, u32, usize)>,
    /// `enable=` IDs with no `disable=` open for them: the comment and the ID.
    pub unmatched: Vec<(Span, String)>,
}

#[derive(Clone, Debug)]
pub struct Waiver {
    /// The comment.
    pub span: Span,
    pub kind: WaiverKind,
    /// The rules or guide rules it names, as written.
    pub ids: Vec<String>,
    pub reason: String,
    /// The byte range it covers (`None`: attached to nothing).
    pub covers: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaiverKind {
    /// `// <ID> waived: <reason>`.
    Guide,
    /// `// dvicode-lint: disable=...`: to the matching `enable` or the end of the file.
    Disable,
    /// `// dvicode-lint: enable=...`.
    Enable,
    /// `// dvicode-lint: disable-file=...`.
    DisableFile,
}

/// Reasons that are not reasons.
pub fn is_template_reason(r: &str) -> bool {
    let r = r.trim();
    r.is_empty() || r == "<reason>" || r.eq_ignore_ascii_case("todo") || r.eq_ignore_ascii_case("tbd")
}

/// Keywords that close a scope or a branch: a waiver right before one covers nothing.
const CLOSERS: &[&str] = &[
    "end", "else", "endcase", "join", "join_any", "join_none", "endfunction", "endtask", "endclass", "endmodule", "endinterface", "endpackage",
    "endprogram", "endgroup", "endproperty", "endsequence", "endgenerate", "endclocking", "endchecker", "endspecify",
];

/// Finds the waivers of `file` (`tokens`: its active tokens in `unit`). Comments are in the
/// gaps between tokens; a comment in code `ifdef` leaves out is dormant (not returned).
pub fn scan(a: &Analysis, file: FileId, tokens: &[Token], comments: &[Comment]) -> Waivers {
    let Some(text) = a.file_text(file) else { return Waivers::default() };
    let mut items = Vec::new();
    for c in comments.iter().filter(|c| c.line) {
        if let Some(w) = parse_comment(&text[c.span.start as usize..c.span.end as usize], c.span) {
            items.push((w, c.next));
        }
    }
    let items: Vec<Waiver> = items
        .into_iter()
        .map(|(mut w, next)| {
            if w.kind == WaiverKind::Guide {
                w.covers = coverage(tokens, next).map(|(s, e)| (from_line_start(&text, s), e));
            }
            w
        })
        .collect();
    // Suppressed regions, each ID on its own: `disable=a,b` then `enable=a` leaves `b` off.
    let mut regions = Vec::new();
    let mut unmatched = Vec::new();
    let mut open: Vec<(String, u32, usize)> = Vec::new();
    for (i, w) in items.iter().enumerate() {
        match w.kind {
            WaiverKind::Disable => {
                for id in &w.ids {
                    if !open.iter().any(|(o, _, _)| o == id) {
                        open.push((id.clone(), w.span.end, i));
                    }
                }
            }
            WaiverKind::Enable => {
                for id in &w.ids {
                    match open.iter().position(|(o, _, _)| o == id) {
                        Some(k) => {
                            let (id, start, from) = open.remove(k);
                            regions.push((id, start, w.span.start, from));
                        }
                        None => unmatched.push((w.span, id.clone())),
                    }
                }
            }
            WaiverKind::DisableFile => {
                for id in &w.ids {
                    regions.push((id.clone(), 0, u32::MAX, i));
                }
            }
            WaiverKind::Guide => {}
        }
    }
    for (id, start, from) in open {
        regions.push((id, start, u32::MAX, from));
    }
    Waivers { items, regions, unmatched }
}

/// `at`, or the start of its line when only blanks are before it there: a waiver covers the
/// indentation of its statement too (text rules report whole lines).
fn from_line_start(text: &str, at: u32) -> u32 {
    let ls = text[..at as usize].rfind('\n').map_or(0, |i| i + 1);
    if text[ls..at as usize].trim().is_empty() {
        ls as u32
    } else {
        at
    }
}

/// Where a guide waiver for a finding at `at` goes: the start of the finding's line, when a
/// waiver inserted there would cover the finding. `None` when it would not, or when a comment
/// there would change the code (inside a `` `define ``, after a line continuation).
pub fn waiver_place(text: &str, facts: &super::facts::FileFacts, at: u32) -> Option<u32> {
    let ls = text.get(..at as usize)?.rfind('\n').map_or(0, |i| i + 1);
    if ls > 0 && text[..ls - 1].trim_end_matches([' ', '\t', '\r']).ends_with('\\') {
        return None;
    }
    let t = &facts.tokens;
    let k = t.partition_point(|x| x.span.start < ls as u32);
    let x = t.get(k)?;
    if !text.get(ls..x.span.start as usize)?.trim().is_empty() || facts.in_define(k) {
        return None;
    }
    let (s, e) = coverage(t, k)?;
    (from_line_start(text, s) <= at && at < e).then_some(ls as u32)
}

/// A comment of the file (in active code).
#[derive(Clone, Copy, Debug)]
pub struct Comment {
    /// Without the newline of a `//` comment.
    pub span: Span,
    /// The index of the first token after it.
    pub next: usize,
    /// `//` (else `/* */`).
    pub line: bool,
}

/// The comments of `file` (`tokens`: its active tokens in `unit`). Comments are in the gaps
/// between the lexer's tokens, so strings and escaped names are never taken for comments; the
/// parts of a gap that `ifdef` leaves out are skipped.
pub fn comments(a: &Analysis, unit: UnitId, file: FileId, tokens: &[Token]) -> Vec<Comment> {
    let Some(text) = a.file_text(file) else { return Vec::new() };
    let inactive = super::facts::inactive(a, unit, file);
    let mut out = Vec::new();
    let mut prev_end = 0usize;
    let mut k = 0usize;
    for i in 0..=tokens.len() {
        let gap_end = tokens.get(i).map_or(text.len(), |t| t.span.start as usize).min(text.len());
        // The gap without what `ifdef` leaves out.
        let mut from = prev_end;
        while from < gap_end {
            while k < inactive.len() && (inactive[k].end as usize) <= from {
                k += 1;
            }
            let (to, skip) = match inactive.get(k) {
                Some(x) if (x.start as usize) < gap_end => ((x.start as usize).max(from), (x.end as usize).min(gap_end)),
                _ => (gap_end, gap_end),
            };
            comments_in(&text, from, to, &mut |s, e, line| out.push(Comment { span: Span::new(file, s, e), next: i, line }));
            from = skip.max(to);
        }
        if let Some(t) = tokens.get(i) {
            prev_end = prev_end.max(t.span.end as usize);
        }
    }
    out
}

/// Calls `f(start, end, line)` for each comment in `text[from..to]`, which holds no token:
/// only whitespace and comments (a `//` comment without its newline).
fn comments_in(text: &str, from: usize, to: usize, f: &mut dyn FnMut(u32, u32, bool)) {
    let b = text.as_bytes();
    let mut i = from;
    while i + 1 < to {
        if b[i] == b'/' && b[i + 1] == b'/' {
            let e = text[i..to].find('\n').map_or(to, |n| i + n);
            let e = if e > i && b[e - 1] == b'\r' { e - 1 } else { e };
            f(i as u32, e as u32, true);
            i = e;
        } else if b[i] == b'/' && b[i + 1] == b'*' {
            let e = text[i + 2..to].find("*/").map_or(to, |n| i + 2 + n + 2);
            f(i as u32, e as u32, false);
            i = e;
        } else {
            i += 1;
        }
    }
}

fn parse_comment(c: &str, span: Span) -> Option<Waiver> {
    let body = c.strip_prefix("//")?.trim();
    if let Some(rest) = body.strip_prefix("dvicode-lint:") {
        let rest = rest.trim_start();
        let (kind, rest) = if let Some(r) = rest.strip_prefix("disable-file=") {
            (WaiverKind::DisableFile, r)
        } else if let Some(r) = rest.strip_prefix("disable=") {
            (WaiverKind::Disable, r)
        } else if let Some(r) = rest.strip_prefix("enable=") {
            (WaiverKind::Enable, r)
        } else {
            return None;
        };
        let (ids, reason) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let ids = ids.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        return Some(Waiver { span, kind, ids, reason: reason.trim().to_string(), covers: None });
    }
    // `<ID>[, <ID>...] waived: <reason>`; the reason may be empty (then it is reported).
    let at = body.find(" waived:").or_else(|| body.find(" waived :"))?;
    let (head, tail) = body.split_at(at);
    let reason = tail.trim_start().strip_prefix("waived")?.trim_start().strip_prefix(':')?.trim();
    let ids: Vec<String> = head.split(',').map(str::trim).map(String::from).collect();
    if ids.is_empty() || !ids.iter().all(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) {
        return None;
    }
    Some(Waiver { span, kind: WaiverKind::Guide, ids, reason: reason.to_string(), covers: None })
}

/// What a guide waiver before `tokens[k]` covers: the statement or declaration starting there.
fn coverage(t: &[Token], k: usize) -> Option<(u32, u32)> {
    let (start, end) = coverage_from(t, k)?;
    // A case item (`0, 1: begin ... end`) or a labelled statement: the statement after the
    // label's `:`, when it reaches further than the first `;` (the colon of `?:` does not).
    let x = &t[k];
    if x.kind == TokKind::Directive {
        return Some((start, end));
    }
    let mut depth = 0i32;
    for j in k..t.len() {
        let y = &t[j];
        if y.kind != TokKind::Punct {
            continue;
        }
        match y.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            ";" if depth <= 0 => break,
            ":" if depth == 0 => {
                if let Some(e) = stmt::statement_end(t, j + 1).filter(|e| *e > j + 1) {
                    return Some((start, end.max(t[e - 1].span.end)));
                }
                break;
            }
            _ => {}
        }
    }
    Some((start, end))
}

fn coverage_from(t: &[Token], k: usize) -> Option<(u32, u32)> {
    let mut k = k;
    // Attributes between the waiver and the code.
    while t.get(k).is_some_and(|x| x.is_punct("(*")) {
        k = (k + 1..t.len()).find(|&j| t[j].is_punct("*)"))? + 1;
    }
    let x = t.get(k)?;
    if x.kind == TokKind::Keyword && CLOSERS.contains(&x.text.as_str()) || x.kind == TokKind::Directive && is_conditional(&x.text) {
        return None;
    }
    if x.kind == TokKind::Directive && x.text == "include" {
        return None;
    }
    let start = x.span.start;
    // A function or task: to its `endfunction`/`endtask` (a prototype: to its `;`).
    let mut q = k;
    let mut proto = false;
    while t.get(q).is_some_and(|y| matches!(y.text.as_str(), "virtual" | "static" | "protected" | "local" | "automatic" | "pure" | "extern")) {
        proto |= t[q].text == "pure" || t[q].text == "extern";
        q += 1;
    }
    if !proto && t.get(q).is_some_and(|y| y.is_kw("function") || y.is_kw("task")) {
        let end_kw = if t[q].is_kw("function") { "endfunction" } else { "endtask" };
        let e = (q..t.len()).find(|&j| t[j].is_kw(end_kw))?;
        return Some((start, t[e].span.end));
    }
    // A statement: as the statement walker sees it.
    if let Some(end) = stmt::statement_end(t, k).filter(|e| *e > k) {
        if !matches!(x.text.as_str(), "class" | "virtual" | "module" | "interface" | "package" | "program" | "function" | "task" | "covergroup" | "static" | "protected" | "local" | "pure" | "extern") {
            return Some((start, t[end - 1].span.end));
        }
    }
    // A declaration or a header: up to its `;` outside brackets; a macro call: its arguments.
    let mut depth = 0i32;
    let mut j = k;
    while let Some(y) = t.get(j) {
        if y.kind == TokKind::Punct {
            match y.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 && x.kind == TokKind::Directive && j > k && !t.get(j + 1).is_some_and(|z| z.is_punct(";")) {
                        return Some((start, y.span.end));
                    }
                }
                ";" if depth <= 0 => return Some((start, y.span.end)),
                _ => {}
            }
        }
        if depth == 0 && j > k && x.kind == TokKind::Directive && t[j - 1].kind == TokKind::Directive && !y.is_punct("(") && y.nl_before() {
            // An object-like macro alone on its line.
            return Some((start, t[j - 1].span.end));
        }
        j += 1;
    }
    Some((start, t.last()?.span.end))
}

fn is_conditional(d: &str) -> bool {
    matches!(d, "ifdef" | "ifndef" | "elsif" | "else" | "endif")
}

/// Whether a waiver ID names `rule` (its own ID or one of its guide rules).
pub fn names_rule(id: &str, rule: &super::RuleMeta) -> bool {
    id == rule.id || rule.guide.contains(&id)
}

/// Marks the findings that a waiver or a suppression covers.
pub fn apply(_a: &Analysis, _file: FileId, facts: &FileFacts, findings: &mut [Finding], _cfg: &FileConfig) {
    let w = &facts.waivers;
    if w.items.is_empty() {
        return;
    }
    for f in findings.iter_mut() {
        let Some(meta) = super::rules::meta(f.rule) else { continue };
        // Findings about waivers themselves are not waived.
        if meta.id.starts_with("waiver-") || meta.id.starts_with("suppression-") {
            continue;
        }
        let at = f.span.start;
        let guide = w.items.iter().find(|x| x.kind == WaiverKind::Guide && x.covers.is_some_and(|(s, e)| s <= at && at < e) && x.ids.iter().any(|i| names_rule(i, meta)));
        if let Some(x) = guide {
            f.suppressed = Some(format!("{} waived: {}", x.ids.join(", "), x.reason));
            continue;
        }
        if let Some((id, _, _, from)) = w.regions.iter().find(|(id, s, e, _)| *s <= at && at < *e && names_rule(id, meta)) {
            let x = &w.items[*from];
            let how = if x.kind == WaiverKind::DisableFile { "disable-file" } else { "disable" };
            f.suppressed = Some(format!("dvicode-lint: {how}={id} {}", x.reason).trim_end().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_forms() {
        let s = Span::new(0, 0, 0);
        let w = parse_comment("// LNG-10 waived: smoke test only", s).unwrap();
        assert_eq!((w.kind, w.ids.as_slice(), w.reason.as_str()), (WaiverKind::Guide, &["LNG-10".to_string()][..], "smoke test only"));
        let w = parse_comment("// LNG-10, randomize-result-discarded waived:", s).unwrap();
        assert_eq!(w.ids.len(), 2);
        assert!(is_template_reason(&w.reason));
        assert!(parse_comment("// this is waived: no ids here", s).is_none());
        assert!(parse_comment("// plain comment", s).is_none());
        let w = parse_comment("// dvicode-lint: disable=a,b legacy block", s).unwrap();
        assert_eq!((w.kind, w.ids.len(), w.reason.as_str()), (WaiverKind::Disable, 2, "legacy block"));
        let w = parse_comment("// dvicode-lint: enable=a", s).unwrap();
        assert_eq!(w.kind, WaiverKind::Enable);
        let w = parse_comment("// dvicode-lint: disable-file=trailing-whitespace generated", s).unwrap();
        assert_eq!(w.kind, WaiverKind::DisableFile);
    }

    fn lex(s: &str) -> Vec<Token> {
        dvi_syntax::lexer::lex(s, 0).into_iter().filter(|t| t.kind != TokKind::Eof).collect()
    }

    fn covered(src: &str, k: usize) -> Option<&str> {
        let t = lex(src);
        coverage(&t, k).map(|(s, e)| &src[s as usize..e as usize])
    }

    #[test]
    fn what_a_waiver_covers() {
        assert_eq!(covered("x.randomize(); y = 1;", 0), Some("x.randomize();"));
        assert_eq!(covered("if (a) x.randomize(); else y = 1; z = 2;", 0), Some("if (a) x.randomize(); else y = 1;"));
        assert_eq!(covered("(* keep *) x = 1; y = 2;", 0), Some("x = 1;"));
        assert_eq!(covered("class c extends b; int x; endclass", 0), Some("class c extends b;"));
        assert_eq!(covered("`uvm_component_utils(c)\nint x;", 0), Some("`uvm_component_utils(c)"));
        assert_eq!(covered("end", 0), None);
        assert_eq!(covered("`ifdef X\nx = 1;\n`endif", 0), None);
        assert_eq!(covered("virtual function void f(); x = 1; endfunction y = 2;", 0), Some("virtual function void f(); x = 1; endfunction"));
        assert_eq!(covered("extern function void f(); y = 2;", 0), Some("extern function void f();"));
        assert_eq!(covered("task t; #1; endtask", 0), Some("task t; #1; endtask"));
        assert_eq!(covered("0, 1: begin x(); y(); end\n2: z();", 0), Some("0, 1: begin x(); y(); end"));
        assert_eq!(covered("default: if (a) x(); else y(); z();", 0), Some("default: if (a) x(); else y();"));
        assert_eq!(covered("if (a) x = b ? c : d; else y = 1; z = 2;", 0), Some("if (a) x = b ? c : d; else y = 1;"));
        assert_eq!(covered("x = b ? c : d; y = 1;", 0), Some("x = b ? c : d;"));
        assert_eq!(covered("bit [3:0] x; y = 1;", 0), Some("bit [3:0] x;"));
    }
}
