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
pub fn scan(a: &Analysis, unit: UnitId, file: FileId, tokens: &[Token]) -> Waivers {
    let Some(text) = a.file_text(file) else { return Waivers::default() };
    let inactive: Vec<Span> = a.unit(unit).inactive.iter().filter(|s| s.file == file).copied().collect();
    let mut items = Vec::new();
    let mut prev_end = 0usize;
    for i in 0..=tokens.len() {
        let gap_end = tokens.get(i).map_or(text.len(), |t| t.span.start as usize).min(text.len());
        if gap_end > prev_end {
            comments_in(&text, prev_end, gap_end, &mut |s, e| {
                if inactive.iter().any(|x| x.start <= s && s < x.end) {
                    return;
                }
                if let Some(w) = parse_comment(&text[s as usize..e as usize], Span::new(file, s, e)) {
                    items.push((w, i));
                }
            });
        }
        if let Some(t) = tokens.get(i) {
            prev_end = prev_end.max(t.span.end as usize);
        }
    }
    let items = items
        .into_iter()
        .map(|(mut w, next)| {
            if w.kind == WaiverKind::Guide {
                w.covers = coverage(tokens, next);
            }
            w
        })
        .collect();
    Waivers { items }
}

/// Calls `f(start, end)` for each `//` comment (without its newline) in `text[from..to]`, which
/// holds no token: only whitespace and comments.
fn comments_in(text: &str, from: usize, to: usize, f: &mut dyn FnMut(u32, u32)) {
    let b = text.as_bytes();
    let mut i = from;
    while i + 1 < to {
        if b[i] == b'/' && b[i + 1] == b'/' {
            let e = text[i..to].find('\n').map_or(to, |n| i + n);
            let e = if e > i && b[e - 1] == b'\r' { e - 1 } else { e };
            f(i as u32, e as u32);
            i = e;
        } else if b[i] == b'/' && b[i + 1] == b'*' {
            i = text[i + 2..to].find("*/").map_or(to, |n| i + 2 + n + 2);
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
    // A statement: as the statement walker sees it.
    if let Some(end) = stmt::statement_end(t, k).filter(|e| *e > k) {
        if !matches!(x.text.as_str(), "class" | "virtual" | "module" | "interface" | "package" | "program" | "function" | "task" | "covergroup") {
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
    let w = &facts.waivers.items;
    if w.is_empty() {
        return;
    }
    for f in findings.iter_mut() {
        let Some(meta) = super::rules::meta(f.rule) else { continue };
        // Findings about waivers themselves are not waived.
        if meta.id.starts_with("waiver-") || meta.id.starts_with("suppression-") {
            continue;
        }
        let at = f.span.start;
        for x in w {
            let hit = match x.kind {
                WaiverKind::Guide => x.covers.is_some_and(|(s, e)| s <= at && at < e) && x.ids.iter().any(|i| names_rule(i, meta)),
                WaiverKind::DisableFile => x.ids.iter().any(|i| names_rule(i, meta)),
                WaiverKind::Disable => {
                    x.span.end <= at
                        && x.ids.iter().any(|i| names_rule(i, meta))
                        && !w.iter().any(|y| y.kind == WaiverKind::Enable && y.span.start > x.span.end && y.span.end <= at && y.ids.iter().any(|i| x.ids.contains(i)))
                }
                WaiverKind::Enable => false,
            };
            if hit {
                f.suppressed = Some(match x.kind {
                    WaiverKind::Guide => format!("{} waived: {}", x.ids.join(", "), x.reason),
                    _ => format!("dvicode-lint: disable={} {}", x.ids.join(","), x.reason).trim_end().to_string(),
                });
                break;
            }
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
    }
}
