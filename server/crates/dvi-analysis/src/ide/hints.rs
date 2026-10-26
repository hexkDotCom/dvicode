//! Inlay hints: the names of the ports, parameters and arguments that positional connections
//! and calls bind (`u (a, b)` shows `clk: a, rst_n: b`).
//!
//! Hints are given only where the binding is certain from the source: no hint in a list that
//! a macro call is part of (its expansion can add items), in a list that mixes named and
//! positional items, or for a design element whose port slots are not plain names.

use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::{dflags, DeclKind, RefKind};
use dvi_syntax::token::{TokKind, Token};
use dvi_syntax::FileId;
use rustc_hash::FxHashMap;
use std::sync::Arc;

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

/// Tokens before the range looked at for a list that starts above it (a long port list).
const LOOK_BACK: usize = 20_000;

/// The names a list's items bind, in order; an empty name keeps a slot that gets no hint (an
/// unnamed DPI argument).
pub(crate) type Slots = Arc<Vec<String>>;

impl Analysis {
    /// The hints for the part of `file` from `start` to `end`.
    pub fn inlay_hints(&self, file: FileId, start: u32, end: u32, kinds: HintKinds) -> Vec<Hint> {
        if !(kinds.ports || kinds.parameters || kinds.arguments) {
            return Vec::new();
        }
        let Some(toks) = self.file_tokens(file) else { return Vec::new() };
        let Some(uid) = self.context_unit(file) else { return Vec::new() };
        let u = self.unit(uid);
        let Some(pos) = u.positions.get(&file) else { return Vec::new() };
        let first = toks.partition_point(|t| t.span.end < start).saturating_sub(LOOK_BACK);
        let last = toks.partition_point(|t| t.span.start <= end);
        let mut out = Vec::new();
        let mut cache: FxHashMap<DeclRef, (Slots, Slots)> = FxHashMap::default();
        let mut args_cache: FxHashMap<DeclRef, Slots> = FxHashMap::default();
        let push = |items: &[(usize, usize)], names: &[String], out: &mut Vec<Hint>| {
            // Not when a macro call may add items, nor when named and positional ones mix.
            let macro_in = items.iter().any(|&(a, b)| toks[a..b].iter().any(|t| t.kind == TokKind::Directive));
            let named = items.iter().any(|&(a, b)| a < b && (toks[a].is_punct(".") || toks[a].is_punct(".*")));
            if macro_in || named {
                return;
            }
            for (k, &(a, b)) in items.iter().enumerate() {
                let Some(name) = names.get(k) else { break };
                if a >= b || name.is_empty() {
                    continue;
                }
                let at = toks[a].span.start;
                // An item that is just the name it binds would only repeat it.
                let same = b - a == 1 && toks[a].kind == TokKind::Ident && toks[a].text == name.as_str();
                if !same && (start..=end).contains(&at) {
                    out.push(Hint { offset: at, label: format!("{name}:") });
                }
            }
        };
        for i in first..last.min(toks.len()) {
            if i % 4096 == 0 && self.cancelled() {
                return Vec::new();
            }
            let t = &toks[i];
            let ident = t.kind == TokKind::Ident || t.is_kw("new");
            if !ident {
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
                    let (params, ports) = cache.entry(def).or_insert_with(|| self.header_slots(def)).clone();
                    let mut j = i + 1;
                    if toks.get(j).is_some_and(|t| t.is_punct("#")) && toks.get(j + 1).is_some_and(|t| t.is_punct("(")) {
                        let (items, after) = list(&toks, j + 1);
                        if kinds.parameters {
                            push(&items, &params, &mut out);
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
                            push(&items, &ports, &mut out);
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
                    let args = args_cache.entry(callee).or_insert_with(|| self.argument_slots(callee)).clone();
                    let (items, _) = list(&toks, i + 1);
                    // A call with one argument says enough.
                    if items.len() >= 2 {
                        push(&items, &args, &mut out);
                    }
                }
                break;
            }
        }
        out.sort_by_key(|h| h.offset);
        out.dedup();
        out
    }

    /// The parameters positional values set, in order, and the port slots of a design element.
    /// Parameters: those of its `#(...)` header that are not local, or without a header, those
    /// of its body. Ports: none when a port slot is not a plain name (`module m({a, b}, c)`).
    pub(crate) fn header_slots(&self, def: DeclRef) -> (Slots, Slots) {
        let u = self.unit(def.unit);
        let d = u.parsed.decl(def.decl);
        let kids: Vec<_> = d.children.iter().map(|c| u.parsed.decl(*c)).collect();
        let is_param = |c: &dvi_syntax::ast::Decl| matches!(c.kind, DeclKind::Parameter | DeclKind::TypeParam) && !c.has(dflags::LOCAL_PARAM);
        let header = kids.iter().any(|c| c.has(dflags::PARAM_PORT));
        let params = kids.iter().filter(|c| is_param(c) && (!header || c.has(dflags::PARAM_PORT))).map(|c| c.name.to_string()).collect();
        // The header ends at the first `;` of the design element: a port declared only after
        // it was not a plain name in the header.
        // From the file's tokens (kept), not by lexing the whole body again.
        let header_end = self.file_tokens(d.span.file).and_then(|toks| {
            let lo = toks.partition_point(|t| t.span.start < d.span.start);
            first_semicolon(toks[lo..].iter().take_while(|t| t.span.start < d.span.end))
        });
        let ports: Vec<&&dvi_syntax::ast::Decl> = kids.iter().filter(|c| c.kind == DeclKind::Port).collect();
        let plain = header_end.is_some_and(|e| ports.iter().all(|p| p.name_span.file == d.span.file && p.name_span.start < e));
        let ports = if plain { ports.iter().map(|c| c.name.to_string()).collect() } else { Vec::new() };
        (Arc::new(params), Arc::new(ports))
    }

    /// The argument slots of a function or task, in order.
    fn argument_slots(&self, sub: DeclRef) -> Slots {
        let u = self.unit(sub.unit);
        Arc::new(u.parsed.decl(sub.decl).children.iter().map(|c| u.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Argument).map(|c| c.name.to_string()).collect())
    }
}

/// Where the first `;` outside parentheses among `toks` starts.
fn first_semicolon<'t>(toks: impl Iterator<Item = &'t Token>) -> Option<u32> {
    let mut depth = 0i32;
    toks.into_iter()
        .find(|t| {
            if t.is_punct("(") {
                depth += 1;
            } else if t.is_punct(")") {
                depth -= 1;
            }
            depth == 0 && t.is_punct(";")
        })
        .map(|t| t.span.start)
}

/// The items of the parenthesized list opening at `toks[open]` (`(`), as token index ranges
/// (`(first, end)`, end excluded; empty for an empty item), and the index after its `)`.
/// Attributes `(* ... *)` and nested groups (an inline constraint's `{ a; b; }`) are inside
/// items; a `;` outside them ends the search (an unclosed list names nothing).
pub(crate) fn list(toks: &[Token], open: usize) -> (Vec<(usize, usize)>, usize) {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut first = open + 1;
    let mut i = open + 1;
    while i < toks.len() {
        let t = &toks[i];
        if t.kind == TokKind::Punct {
            match t.text.as_str() {
                "(" | "[" | "{" | "(*" => depth += 1,
                ")" | "]" | "}" | "*)" if depth > 0 => depth -= 1,
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
                ";" if depth == 0 => break,
                _ => {}
            }
        }
        i += 1;
    }
    (Vec::new(), i)
}

/// The index after the bracket group opening at `toks[open]`.
pub(crate) fn skip(toks: &[Token], open: usize) -> usize {
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
