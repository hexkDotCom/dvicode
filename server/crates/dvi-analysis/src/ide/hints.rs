//! Inlay hints: the names of the ports, parameters and arguments that positional connections
//! and calls bind (`u (a, b)` shows `clk: a, rst_n: b`).

use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::{Decl, DeclKind, RefKind};
use dvi_syntax::lexer::lex;
use dvi_syntax::token::{TokKind, Token};
use dvi_syntax::FileId;

/// Which hints to give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HintKinds {
    /// Port names on positional port connections.
    pub ports: bool,
    /// Parameter names on positional parameter values (`#(8, 4)`).
    pub parameters: bool,
    /// Argument names on positional arguments of function and task calls.
    pub arguments: bool,
}

impl Default for HintKinds {
    fn default() -> HintKinds {
        HintKinds { ports: true, parameters: true, arguments: true }
    }
}

/// A hint: a name to show before the token at `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub offset: u32,
    pub label: String,
}

/// How much text before the range is read, for an instance or call that starts above it.
const LOOK_BACK: usize = 16 << 10;
/// And after it, for one that starts in it and ends below.
const LOOK_AHEAD: usize = 4 << 10;

impl Analysis {
    /// The hints for the part of `file` from `start` to `end`.
    pub fn inlay_hints(&self, file: FileId, start: u32, end: u32, kinds: HintKinds) -> Vec<Hint> {
        if !(kinds.ports || kinds.parameters || kinds.arguments) {
            return Vec::new();
        }
        let Some(text) = self.file_text(file) else { return Vec::new() };
        let Some(&uid) = self.units_for(file).first() else { return Vec::new() };
        let u = self.unit(uid);
        let Some(pos) = u.positions.get(&file) else { return Vec::new() };
        // A window of the text around the range, from the start of a line.
        let (s, e) = (start as usize, (end as usize).min(text.len()));
        let mut lo = s.saturating_sub(LOOK_BACK);
        while lo > 0 && !text.is_char_boundary(lo) {
            lo -= 1;
        }
        if lo > 0 {
            lo = text[lo..s.max(lo)].find('\n').map_or(s, |i| lo + i + 1);
        }
        let mut hi = (e + LOOK_AHEAD).min(text.len());
        while hi < text.len() && !text.is_char_boundary(hi) {
            hi += 1;
        }
        let mut toks = lex(&text[lo..hi], file);
        for t in &mut toks {
            t.span.start += lo as u32;
            t.span.end += lo as u32;
        }
        let mut out = Vec::new();
        let push = |offset: u32, label: &str, out: &mut Vec<Hint>| {
            if (start..=end).contains(&offset) {
                out.push(Hint { offset, label: format!("{label}:") });
            }
        };
        for i in 0..toks.len() {
            let t = &toks[i];
            if !matches!(t.kind, TokKind::Ident) {
                continue;
            }
            let lo_ref = pos.refs.partition_point(|(sp, _, _)| sp.start < t.span.start);
            for (sp, rid, seg) in pos.refs[lo_ref..].iter().take_while(|(sp, _, _)| sp.start == t.span.start) {
                if sp.end != t.span.end {
                    continue;
                }
                let chain = u.parsed.reference(*rid);
                if chain.kind == RefKind::Definition {
                    let Some(def) = self.resolve_ref(uid, *rid, *seg as usize) else { continue };
                    if !self.decl(def).kind.is_definition() {
                        continue;
                    }
                    let (params, ports) = self.header(def);
                    let mut j = i + 1;
                    // `#( ... )`
                    if toks.get(j).is_some_and(|t| t.is_punct("#")) && toks.get(j + 1).is_some_and(|t| t.is_punct("(")) {
                        let (items, after) = list(&toks, j + 1);
                        if kinds.parameters {
                            for (k, item) in items.iter().enumerate() {
                                if let Some(name) = positional(&toks, item).and(params.get(k)) {
                                    if !same_name(&toks, item, name) {
                                        push(toks[item.0].span.start, name, &mut out);
                                    }
                                }
                            }
                        }
                        j = after;
                    }
                    // Instances: `name [dims] ( ... )`, separated by commas.
                    while toks.get(j).is_some_and(|t| t.kind == TokKind::Ident) {
                        j += 1;
                        while toks.get(j).is_some_and(|t| t.is_punct("[")) {
                            j = skip(&toks, j);
                        }
                        if !toks.get(j).is_some_and(|t| t.is_punct("(")) {
                            break;
                        }
                        let (items, after) = list(&toks, j);
                        if kinds.ports {
                            for (k, item) in items.iter().enumerate() {
                                if let Some(name) = positional(&toks, item).and(ports.get(k)) {
                                    if !same_name(&toks, item, name) {
                                        push(toks[item.0].span.start, name, &mut out);
                                    }
                                }
                            }
                        }
                        j = after;
                        if toks.get(j).is_some_and(|t| t.is_punct(",")) {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                } else if kinds.arguments && toks.get(i + 1).is_some_and(|n| n.is_punct("(")) {
                    let Some(callee) = self.resolve_ref(uid, *rid, *seg as usize) else { continue };
                    if !matches!(self.decl(callee).kind, DeclKind::Function | DeclKind::Task) {
                        continue;
                    }
                    let args = self.arguments(callee);
                    let (items, _) = list(&toks, i + 1);
                    // One argument named as itself would only repeat it; and a call with one
                    // argument says enough.
                    if items.len() < 2 {
                        continue;
                    }
                    for (k, item) in items.iter().enumerate() {
                        if let Some(name) = positional(&toks, item).and(args.get(k)) {
                            if !same_name(&toks, item, name) {
                                push(toks[item.0].span.start, name, &mut out);
                            }
                        }
                    }
                }
                break;
            }
        }
        out.sort_by_key(|h| h.offset);
        out.dedup();
        out
    }

    /// The parameters (in order) and ports of a design element.
    fn header(&self, def: DeclRef) -> (Vec<String>, Vec<String>) {
        let u = self.unit(def.unit);
        let d = u.parsed.decl(def.decl);
        let kids = || d.children.iter().map(|c| u.parsed.decl(*c));
        let params = kids().filter(|c| matches!(c.kind, DeclKind::Parameter | DeclKind::TypeParam)).map(|c: &Decl| c.name.to_string()).collect();
        let ports = kids().filter(|c| c.kind == DeclKind::Port).map(|c| c.name.to_string()).collect();
        (params, ports)
    }

    /// The argument names of a function or task, in order.
    fn arguments(&self, sub: DeclRef) -> Vec<String> {
        let u = self.unit(sub.unit);
        u.parsed.decl(sub.decl).children.iter().map(|c| u.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Argument).map(|c| c.name.to_string()).collect()
    }
}

/// The items of the parenthesized list opening at `toks[open]` (`(`), as token index ranges
/// (`(first, end)`, end excluded; empty for an empty item), and the index after its `)`.
fn list(toks: &[Token], open: usize) -> (Vec<(usize, usize)>, usize) {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut first = open + 1;
    let mut i = open + 1;
    while i < toks.len() {
        let t = &toks[i];
        if t.kind == TokKind::Punct {
            match t.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" if depth > 0 => depth -= 1,
                ")" => {
                    if i > first || !items.is_empty() {
                        items.push((first, i));
                    }
                    return (items, i + 1);
                }
                "," if depth == 0 => {
                    items.push((first, i));
                    first = i + 1;
                }
                ";" => break,
                _ => {}
            }
        }
        i += 1;
    }
    // Not closed in the window: no items to name.
    (Vec::new(), i)
}

/// The index after the bracket group opening at `toks[open]`.
fn skip(toks: &[Token], open: usize) -> usize {
    let mut depth = 0usize;
    for (i, t) in toks.iter().enumerate().skip(open) {
        if t.is_punct("[") || t.is_punct("(") || t.is_punct("{") {
            depth += 1;
        } else if t.is_punct("]") || t.is_punct(")") || t.is_punct("}") {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return i + 1;
            }
        }
    }
    toks.len()
}

/// `Some(())` for a positional item that is not empty (a named one, `.p(x)` or `.*`, is not).
fn positional(toks: &[Token], (first, end): &(usize, usize)) -> Option<()> {
    if first >= end {
        return None;
    }
    let t = &toks[*first];
    (!(t.is_punct(".") || t.is_punct(".*"))).then_some(())
}

/// The item is just the name it binds to (`u (clk, rst_n)`): the hint would repeat it.
fn same_name(toks: &[Token], (first, end): &(usize, usize), name: &str) -> bool {
    end - first == 1 && toks[*first].kind == TokKind::Ident && toks[*first].text.eq_ignore_ascii_case(name)
}
