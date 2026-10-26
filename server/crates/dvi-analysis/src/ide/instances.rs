//! Instance repair: the port and parameter connections of an instance, read from its source,
//! with rewrites (connect missing ports, expand `.*`, use named connections), the problems
//! a connection list shows for sure (a port connected twice, ordered and named items mixed,
//! more ordered items than ports), and completion of port and parameter names.
//!
//! A definition's ports are taken in header order only when that order is certain: no macro or
//! `ifdef` in the header, nothing encrypted, and every slot a plain name that is one of its
//! port declarations. Otherwise nothing that depends on the order is offered or reported.

use super::hints::list;
use super::overrides::Refactor;
use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, DeclId, DeclKind};
use dvi_syntax::{Diag, FileId, Span, TokKind, Token};
use lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat};
use rustc_hash::FxHashSet;
use std::sync::Arc;

/// One port (or parameter) slot of a definition's header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    /// `None` for a slot that is not a plain name (`.ext(a)`, `{a, b}`).
    pub name: Option<String>,
    /// Written with a default value (`input int w = 8`, `parameter W = 8`).
    pub default: bool,
    /// An interface port (it cannot be left unconnected).
    pub interface: bool,
}

/// The slots of a definition, and whether their order and names are certain.
#[derive(Clone, Debug, Default)]
pub struct Header {
    pub slots: Vec<Slot>,
    pub known: bool,
}

/// What an item of a connection list is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// `.p(expr)`, `.p()` (`open`: the index of its `(`), or `.p` (no `open`).
    Named { name: String, name_tok: usize, open: Option<usize> },
    DotStar,
    Ordered,
    /// Nothing between two commas.
    Empty,
}

/// An item of a connection list: token indexes `first..end`, `body` after its attributes.
#[derive(Clone, Debug)]
pub struct ConnItem {
    pub kind: ItemKind,
    pub first: usize,
    pub end: usize,
    pub body: usize,
}

/// A connection list: its `(` and `)` token indexes and its items.
#[derive(Clone, Debug)]
pub struct ConnList {
    pub open: usize,
    pub close: usize,
    pub items: Vec<ConnItem>,
    /// A macro is used in it: its expansion may add or change items.
    pub macros: bool,
}

/// The items of the list opening at `toks[open]`.
pub fn conn_list(toks: &[Token], open: usize) -> Option<ConnList> {
    let (raw, after) = list(toks, open);
    let close = after.checked_sub(1)?;
    if !toks.get(close).is_some_and(|t| t.is_punct(")")) {
        return None;
    }
    let mut macros = false;
    let mut items = Vec::new();
    for (first, end) in raw {
        macros |= toks[first..end].iter().any(|t| t.kind == TokKind::Directive);
        // Attributes `(* ... *)` before the item.
        let mut body = first;
        while body < end && toks[body].is_punct("(*") {
            while body < end && !toks[body].is_punct("*)") {
                body += 1;
            }
            body += 1;
        }
        let item = &toks[body.min(end)..end];
        let kind = match item {
            [] => ItemKind::Empty,
            [s] if s.is_punct(".*") => ItemKind::DotStar,
            [d, n, rest @ ..] if d.is_punct(".") && n.kind == TokKind::Ident => {
                let open = (!rest.is_empty()).then_some(body + 2).filter(|&o| toks[o].is_punct("("));
                if !rest.is_empty() && open.is_none() {
                    ItemKind::Ordered
                } else {
                    ItemKind::Named { name: n.text.to_string(), name_tok: body + 1, open }
                }
            }
            _ => ItemKind::Ordered,
        };
        items.push(ConnItem { kind, first, end, body: body.min(end) });
    }
    Some(ConnList { open, close, items, macros })
}

/// A name as it must be written: `\p.q ` (escaped, with its terminating space) when it is not
/// a plain identifier.
pub fn ident(n: &str) -> String {
    let mut c = n.chars();
    let plain = c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_' || x == '$');
    // A keyword as a name is escaped; `new`, a method's name in calls (`super.new(...)`), is not.
    if plain && (n == "new" || !dvi_syntax::token::is_keyword(n)) {
        n.to_string()
    } else {
        format!("\\{n} ")
    }
}

/// The last name outside brackets and before a default value in a header slot, and whether
/// it has a default.
fn slot_name(item: &[Token]) -> (Option<String>, bool) {
    if item.first().is_some_and(|t| t.is_punct(".") || t.is_punct("{")) {
        return (None, false);
    }
    let mut depth = 0i32;
    let mut name = None;
    for t in item {
        if t.is_punct("[") || t.is_punct("(") {
            depth += 1;
        } else if t.is_punct("]") || t.is_punct(")") {
            depth -= 1;
        } else if t.is_punct("=") && depth == 0 {
            return (name, true);
        } else if depth == 0 && t.kind == TokKind::Ident {
            name = Some(t.text.to_string());
        }
    }
    (name, false)
}

impl Analysis {
    /// The index of the token at `span` in `toks`.
    fn token_at(toks: &[Token], span: Span) -> Option<usize> {
        let k = toks.partition_point(|t| t.span.start < span.start);
        toks.get(k).is_some_and(|t| t.span == span).then_some(k)
    }

    /// After a definition's name: past `import ...;` lists, the index of its `#` (or of its port
    /// list's `(`).
    fn after_definition_name(toks: &[Token], name: usize) -> usize {
        let mut j = name + 1;
        while toks.get(j).is_some_and(|t| t.is_kw("import")) {
            while toks.get(j).is_some_and(|t| !t.is_punct(";")) {
                j += 1;
            }
            j += 1;
        }
        j
    }

    /// A definition's ports in header order (ANSI or not).
    pub fn port_header(&self, def: DeclRef) -> Header {
        let d = self.decl(def);
        let Some(toks) = self.file_tokens(d.name_span.file) else { return Header::default() };
        let Some(k) = Self::token_at(&toks, d.name_span) else { return Header::default() };
        let mut j = Self::after_definition_name(&toks, k);
        if toks.get(j).is_some_and(|t| t.is_punct("#")) && toks.get(j + 1).is_some_and(|t| t.is_punct("(")) {
            j = super::hints::skip(&toks, j + 1);
        }
        let ports: Vec<DeclRef> = self.unit(def.unit).parsed.decl(def.decl).children.iter().map(|c| DeclRef { unit: def.unit, decl: *c }).filter(|c| self.decl(*c).kind == DeclKind::Port).collect();
        if !toks.get(j).is_some_and(|t| t.is_punct("(")) {
            // No port list: no ports (`module m;`).
            return Header { slots: Vec::new(), known: ports.is_empty() && !d.has(dflags::ENCRYPTED) };
        }
        let Some(l) = conn_list(&toks, j) else { return Header::default() };
        let mut known = !l.macros && !d.has(dflags::ENCRYPTED) && !self.has_inactive_between(def.unit, d.name_span.file, toks[j].span.start, toks[l.close].span.end);
        let by_name: rustc_hash::FxHashMap<&str, DeclRef> = ports.iter().map(|p| (self.decl(*p).name.as_str(), *p)).collect();
        let mut slots: Vec<Slot> = Vec::new();
        for it in &l.items {
            let item = &toks[it.body..it.end];
            let (mut name, default) = slot_name(item);
            // A slot with no keyword and one name: a non-ANSI port reference (`a`, `a[0]`), or
            // an ANSI port taking the slot before it's direction and type (`input a, b`).
            let idents = item.iter().filter(|t| t.kind == TokKind::Ident).count();
            let bare = !item.iter().any(|t| t.kind == TokKind::Keyword) && idents == 1;
            if bare && item.iter().any(|t| t.is_punct("[")) {
                // `a[0]`: a part of a port, which has no name to connect by.
                name = None;
                known = false;
            }
            let port = name.as_deref().and_then(|n| by_name.get(n).copied());
            if name.is_some() && port.is_none() {
                known = false;
            }
            let interface = port.and_then(|p| self.type_of(p)).is_some_and(|t| self.decl(t).kind == DeclKind::Interface)
                || item.iter().any(|t| t.is_kw("interface"))
                || (bare && slots.last().is_some_and(|s| s.interface));
            slots.push(Slot { name, default, interface });
        }
        if slots.len() != ports.len() && !(slots.len() == 1 && l.items[0].kind == ItemKind::Empty && ports.is_empty()) {
            known = false;
        }
        if slots.len() == 1 && l.items[0].kind == ItemKind::Empty {
            slots.clear();
        }
        Header { slots, known }
    }

    /// A definition's overridable parameters in order: its `#(...)` header (empty for `#()`),
    /// else its body's `parameter`s.
    pub fn param_header(&self, def: DeclRef) -> Header {
        let d = self.decl(def);
        let Some(toks) = self.file_tokens(d.name_span.file) else { return Header::default() };
        let Some(k) = Self::token_at(&toks, d.name_span) else { return Header::default() };
        let j = Self::after_definition_name(&toks, k);
        let params: Vec<DeclRef> = self
            .unit(def.unit)
            .parsed
            .decl(def.decl)
            .children
            .iter()
            .map(|c| DeclRef { unit: def.unit, decl: *c })
            .filter(|c| matches!(self.decl(*c).kind, DeclKind::Parameter | DeclKind::TypeParam) && !self.decl(*c).has(dflags::LOCAL_PARAM))
            .collect();
        if !(toks.get(j).is_some_and(|t| t.is_punct("#")) && toks.get(j + 1).is_some_and(|t| t.is_punct("("))) {
            let slots = params.iter().map(|p| Slot { name: Some(self.decl(*p).name.to_string()), default: !self.decl(*p).detail.is_empty(), interface: false }).collect();
            return Header { slots, known: !d.has(dflags::ENCRYPTED) };
        }
        let Some(l) = conn_list(&toks, j + 1) else { return Header::default() };
        let mut known = !l.macros && !d.has(dflags::ENCRYPTED) && !self.has_inactive_between(def.unit, d.name_span.file, toks[j].span.start, toks[l.close].span.end);
        let mut slots = Vec::new();
        let declared: rustc_hash::FxHashSet<&str> = params.iter().map(|p| self.decl(*p).name.as_str()).collect();
        for it in &l.items {
            let item = &toks[it.body..it.end];
            if item.iter().any(|t| t.is_kw("localparam")) {
                known = false;
            }
            let (name, default) = slot_name(item);
            if name.as_ref().is_none_or(|n| !declared.contains(n.as_str())) {
                known = false;
            }
            slots.push(Slot { name, default, interface: false });
        }
        if slots.len() == 1 && l.items[0].kind == ItemKind::Empty {
            slots.clear();
            known = true;
        }
        Header { slots, known }
    }

    /// Whether code between two offsets of a file is left out by `ifdef` in `unit`.
    fn has_inactive_between(&self, unit: UnitId, file: FileId, start: u32, end: u32) -> bool {
        self.unit(unit).inactive.iter().any(|s| s.file == file && s.start < end && s.end > start)
    }

    /// The instance (not a bound one) of `file` around `off`, in the file's first unit.
    fn instance_at(&self, file: FileId, off: u32) -> Option<DeclRef> {
        let uid = self.context_unit(file)?;
        let p = &self.unit(uid).parsed;
        // Instances of one statement share its start; the one whose name or list holds `off`.
        let toks = self.file_tokens(file)?;
        let mut best: Option<DeclRef> = None;
        for (i, d) in p.decls.iter().enumerate() {
            if d.kind != DeclKind::Instance || d.has(dflags::BOUND) || d.span.file != file || off < d.span.start || off > d.span.end {
                continue;
            }
            let r = DeclRef { unit: uid, decl: i as DeclId };
            let Some(k) = Self::token_at(&toks, d.name_span) else { continue };
            let mut j = k + 1;
            while toks.get(j).is_some_and(|t| t.is_punct("[")) {
                j = super::hints::skip(&toks, j);
            }
            let end = if toks.get(j).is_some_and(|t| t.is_punct("(")) { toks.get(super::hints::skip(&toks, j).saturating_sub(1)).map_or(d.span.end, |t| t.span.end) } else { d.name_span.end };
            if d.name_span.start <= off && off <= end {
                return Some(r);
            }
            best.get_or_insert(r);
        }
        best
    }

    /// An instance's port list (from its name) and its statement's `#(...)` list, with the
    /// definition.
    pub(crate) fn instance_lists(&self, inst: DeclRef) -> Option<(Arc<Vec<Token>>, DeclRef, Option<ConnList>, Option<ConnList>)> {
        let d = self.decl(inst);
        let def = self.type_of(inst).filter(|t| self.decl(*t).kind.is_definition())?;
        let toks = self.file_tokens(d.name_span.file)?;
        let k = Self::token_at(&toks, d.name_span)?;
        let mut j = k + 1;
        while toks.get(j).is_some_and(|t| t.is_punct("[")) {
            j = super::hints::skip(&toks, j);
        }
        let ports = toks.get(j).is_some_and(|t| t.is_punct("(")).then(|| conn_list(&toks, j)).flatten();
        // A primitive's `#(...)` is a delay, not parameters.
        if self.decl(def).kind == DeclKind::Primitive {
            return Some((toks, def, ports, None));
        }
        // The statement's parameters: `type [#(...)]` at its start.
        let s = toks.partition_point(|t| t.span.start < d.span.start);
        let mut p = s;
        while toks.get(p).is_some_and(|t| t.kind == TokKind::Ident) && toks.get(p + 1).is_some_and(|t| t.is_punct("::")) {
            p += 2;
        }
        let params = (toks.get(p + 1).is_some_and(|t| t.is_punct("#")) && toks.get(p + 2).is_some_and(|t| t.is_punct("(")) && p + 2 < k).then(|| conn_list(&toks, p + 2)).flatten();
        Some((toks, def, ports, params))
    }

    /// The source text between two tokens (both included).
    fn source_of(&self, toks: &[Token], first: usize, last: usize) -> Option<String> {
        let (a, b) = (toks.get(first)?.span, toks.get(last)?.span);
        if a.file != b.file {
            return None;
        }
        let text = self.file_text(a.file)?;
        let mut out = text.get(a.start as usize..b.end as usize)?.to_string();
        // An escaped identifier at the end needs its terminating whitespace.
        if text.get(b.start as usize..).is_some_and(|t| t.starts_with('\\')) {
            out.push(' ');
        }
        Some(out)
    }

    /// How a list lays out its items: on lines of their own (with that indent), or on one line.
    fn layout(&self, toks: &[Token], l: &ConnList) -> Option<String> {
        let firsts: Vec<&Token> = l.items.iter().filter(|i| i.first < i.end).map(|i| &toks[i.first]).collect();
        if firsts.is_empty() || !firsts.iter().all(|t| t.nl_before()) {
            return None;
        }
        let t = firsts[0];
        let text = self.file_text(t.span.file)?;
        let line_start = text[..t.span.start as usize].rfind('\n').map_or(0, |i| i + 1);
        Some(text[line_start..t.span.start as usize].chars().take_while(|c| c.is_whitespace()).collect())
    }

    /// The separator before a new item: a new line with the list's indent, or a space.
    fn separator(&self, toks: &[Token], l: &ConnList) -> String {
        let nl = self.file_text(toks[l.open].span.file).is_some_and(|t| t.contains("\r\n"));
        match self.layout(toks, l) {
            Some(indent) => format!(",{}{indent}", if nl { "\r\n" } else { "\n" }),
            None => ", ".to_string(),
        }
    }

    /// Whether a declaration named `name` is visible where instance `inst` is (a signal or an
    /// interface instance to connect).
    fn visible_signal(&self, inst: DeclRef, name: &str) -> bool {
        self.visible(inst, name).is_some_and(|d| matches!(self.decl(d).kind, DeclKind::Variable | DeclKind::Net | DeclKind::Port | DeclKind::Parameter | DeclKind::LocalParam))
    }

    /// The declaration named `name` where instance `inst` is.
    fn visible(&self, inst: DeclRef, name: &str) -> Option<DeclRef> {
        let parent = self.decl(inst).parent.unwrap_or(0);
        self.lookup(inst.unit, parent, name)
    }

    /// Whether `name` where `inst` is names an interface instance or an interface port.
    fn visible_interface(&self, inst: DeclRef, name: &str) -> bool {
        self.visible(inst, name).is_some_and(|d| matches!(self.decl(d).kind, DeclKind::Instance | DeclKind::Port) && self.type_of(d).is_some_and(|t| self.decl(t).kind == DeclKind::Interface))
    }

    /// Whether the port `port` of `def` and the declaration `name` where `inst` is have the
    /// same type for certain (what `.*` requires): the same written type with no name in it, or
    /// the same declared type (a typedef, a class, an interface).
    fn same_type_for_star(&self, def: DeclRef, port: &str, inst: DeclRef, name: &str) -> bool {
        let Some(p) = self.member_of(def, port).filter(|p| self.decl(*p).kind == DeclKind::Port) else { return false };
        let Some(v) = self.visible(inst, name) else { return false };
        let (Some(pt), Some(vt)) = (self.decl(p).ty.as_ref(), self.decl(v).ty.as_ref()) else { return false };
        let norm = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        let named = |t: &str| dvi_syntax::lexer::lex(t, 0).iter().any(|x| x.kind == TokKind::Ident);
        if !named(&pt.text) && !named(&vt.text) {
            return norm(&pt.text) == norm(&vt.text);
        }
        // A declared type: both resolve to it, with nothing else written (no range on a typedef).
        let simple = |t: &str| dvi_syntax::lexer::lex(t, 0).iter().filter(|x| !x.is_punct("::")).count() <= 3 && !t.contains('[');
        simple(&pt.text) && simple(&vt.text) && match (self.type_of(p), self.type_of(v)) {
            (Some(a), Some(b)) => self.decl_key(self.canonical(a)) == self.decl_key(self.canonical(b)),
            _ => false,
        }
    }

    /// The rewrites offered on the instance around `off`.
    pub fn instance_actions(&self, file: FileId, off: u32) -> Vec<Refactor> {
        let mut out = Vec::new();
        let Some(inst) = self.instance_at(file, off) else { return out };
        let Some((toks, def, ports, params)) = self.instance_lists(inst) else { return out };
        let name = self.decl(inst).name.to_string();
        if let Some(l) = ports.as_ref().filter(|l| !l.macros) {
            let header = self.port_header(def);
            let named: rustc_hash::FxHashSet<&str> = l.items.iter().filter_map(|i| if let ItemKind::Named { name, .. } = &i.kind { Some(name.as_str()) } else { None }).collect();
            let star = l.items.iter().position(|i| i.kind == ItemKind::DotStar);
            // `u(,,)`: empty slots of an ordered list; `u()`: an empty list.
            let ordered = l.items.iter().any(|i| i.kind == ItemKind::Ordered) || (l.items.len() > 1 && l.items.iter().all(|i| i.kind == ItemKind::Empty));
            let empty_list = l.items.len() == 1 && l.items[0].kind == ItemKind::Empty;
            // Connect missing ports (a named or empty list).
            if !ordered && star.is_none() && header.known {
                let mut left = Vec::new();
                let mut add = Vec::new();
                for s in &header.slots {
                    let Some(n) = &s.name else { continue };
                    if named.contains(&n.as_str()) {
                        continue;
                    }
                    let w = ident(n);
                    if s.default {
                        left.push("default value");
                    } else if s.interface {
                        if self.visible_interface(inst, n) {
                            add.push(format!(".{w}({w})"));
                        } else {
                            left.push("interface");
                        }
                    } else if self.visible_signal(inst, n) {
                        add.push(format!(".{w}({w})"));
                    } else {
                        add.push(format!(".{w}()"));
                    }
                }
                if !add.is_empty() {
                    let sep = self.separator(&toks, l);
                    let edit = match l.items.iter().rev().find(|i| i.first < i.end) {
                        Some(last) if !empty_list => {
                            let at = toks[last.end - 1].span;
                            (Span::new(at.file, at.end, at.end), add.iter().map(|a| format!("{sep}{a}")).collect::<String>())
                        }
                        _ => {
                            let at = toks[l.open].span;
                            (Span::new(at.file, at.end, at.end), add.join(", "))
                        }
                    };
                    left.sort();
                    left.dedup();
                    let note = if left.is_empty() { String::new() } else { format!(" (some left out: {})", left.join(", ")) };
                    out.push(Refactor { title: format!("Connect {} missing port{} of {name}{note}", add.len(), if add.len() == 1 { "" } else { "s" }), edits: vec![edit] });
                }
            }
            // Expand `.*`.
            if let (Some(k), true) = (star, header.known && !ordered) {
                let mut add = Vec::new();
                let mut ok = true;
                for s in &header.slots {
                    let Some(n) = &s.name else {
                        ok = false;
                        break;
                    };
                    if named.contains(&n.as_str()) {
                        continue;
                    }
                    let w = ident(n);
                    let visible = if s.interface { self.visible_interface(inst, n) } else { self.visible_signal(inst, n) };
                    if visible && self.same_type_for_star(def, n, inst, n) {
                        add.push(format!(".{w}({w})"));
                    } else if visible || !s.default {
                        // `.*` cannot connect it, or a type it cannot prove: not rewritten.
                        ok = false;
                        break;
                    }
                }
                if ok {
                    let it = &l.items[k];
                    let star_span = toks[it.body].span;
                    let edit = if add.is_empty() {
                        // Nothing to write: `.*` goes with one comma.
                        let prev = l.items[..k].iter().rev().find(|i| i.first < i.end);
                        let next = l.items[k + 1..].iter().find(|i| i.first < i.end);
                        match (prev, next) {
                            (Some(p), _) => (Span::new(star_span.file, toks[p.end - 1].span.end, star_span.end), String::new()),
                            (None, Some(n)) => (Span::new(star_span.file, toks[it.first].span.start, toks[n.first].span.start), String::new()),
                            (None, None) => (Span::new(star_span.file, toks[it.first].span.start, star_span.end), String::new()),
                        }
                    } else {
                        let sep = self.separator(&toks, l);
                        (star_span, add.join(&sep))
                    };
                    out.push(Refactor { title: format!("Expand .* of {name}"), edits: vec![edit] });
                }
            }
            // Ordered connections to named ones.
            if ordered && star.is_none() && named.is_empty() && header.known && l.items.len() <= header.slots.len() {
                if let Some(edits) = self.named_edits(&toks, l, &header) {
                    out.push(Refactor { title: format!("Use named port connections for {name}"), edits });
                }
            }
        }
        // Ordered parameter values to named ones (one edit for the statement).
        if let Some(l) = params.as_ref().filter(|l| !l.macros) {
            let header = self.param_header(def);
            let ordered = l.items.iter().any(|i| i.kind == ItemKind::Ordered);
            let named = l.items.iter().any(|i| matches!(i.kind, ItemKind::Named { .. } | ItemKind::DotStar));
            if ordered && !named && header.known && l.items.len() <= header.slots.len() {
                if let Some(edits) = self.named_edits(&toks, l, &header) {
                    out.push(Refactor { title: format!("Use named parameter values for {}", self.decl(def).name), edits });
                }
            }
        }
        out
    }

    /// `.slot(item)` for each ordered item (`.slot()` for an empty one), from the source.
    fn named_edits(&self, toks: &[Token], l: &ConnList, header: &Header) -> Option<Vec<(Span, String)>> {
        let mut edits = Vec::new();
        for (k, it) in l.items.iter().enumerate() {
            let slot = header.slots.get(k)?.name.clone()?;
            if it.body >= it.end {
                // An empty item keeps a port's default; `.p()` would not.
                if header.slots[k].default {
                    return None;
                }
                // An empty item: `.p()` just before what follows it (`,` or `)`).
                let at = toks.get(it.end).or_else(|| toks.get(it.first))?.span;
                edits.push((Span::new(at.file, at.start, at.start), format!(".{}()", ident(&slot))));
                continue;
            }
            let text = self.source_of(toks, it.body, it.end - 1)?;
            let a = toks[it.body].span;
            let b = toks[it.end - 1].span;
            edits.push((Span::new(a.file, a.start, b.end), format!(".{}({text})", ident(&slot))));
        }
        Some(edits)
    }

    /// The problems of the instance connections of `file` (in its first unit) that are certain.
    pub fn instance_problems(&self, file: FileId) -> Vec<Diag> {
        let mut out = Vec::new();
        let Some(uid) = self.context_unit(file) else { return out };
        let p = &self.unit(uid).parsed;
        let mut seen: FxHashSet<u32> = FxHashSet::default();
        let mut headers: rustc_hash::FxHashMap<((u32, u32), bool), Arc<Header>> = rustc_hash::FxHashMap::default();
        for (i, d) in p.decls.iter().enumerate() {
            if d.kind != DeclKind::Instance || d.has(dflags::BOUND) || d.span.file != file {
                continue;
            }
            if self.cancelled() {
                break;
            }
            let inst = DeclRef { unit: uid, decl: i as DeclId };
            let Some((toks, def, ports, params)) = self.instance_lists(inst) else { continue };
            for (l, header, what) in [(ports, None, "port"), (params, Some(()), "parameter")] {
                let Some(l) = l else { continue };
                if l.macros || !seen.insert(toks[l.open].span.start) {
                    continue;
                }
                let key = (self.decl_key(self.canonical(def)), header.is_some());
                let header = headers.entry(key).or_insert_with(|| Arc::new(if key.1 { self.param_header(def) } else { self.port_header(def) })).clone();
                self.list_problems(&toks, &l, &header, what, &mut out);
            }
        }
        out
    }

    fn list_problems(&self, toks: &[Token], l: &ConnList, header: &Header, what: &str, out: &mut Vec<Diag>) {
        let mut names: FxHashSet<&str> = FxHashSet::default();
        let mut stars = 0;
        let mut ordered = 0;
        let mut empty = 0;
        let mut named = 0;
        for (k, it) in l.items.iter().enumerate() {
            match &it.kind {
                ItemKind::Named { name, name_tok, .. } => {
                    named += 1;
                    if !names.insert(name.as_str()) {
                        out.push(Diag::error(toks[*name_tok].span, "DUPLICATE_CONNECTION", format!("The {what} '{name}' is connected more than once")));
                    }
                }
                ItemKind::DotStar => {
                    stars += 1;
                    named += 1;
                    if stars > 1 {
                        out.push(Diag::error(toks[it.body].span, "DUPLICATE_CONNECTION", "'.*' is written more than once"));
                    }
                }
                ItemKind::Ordered => ordered += 1,
                ItemKind::Empty => {
                    // An empty item is an ordered slot (`u(a, , b)`) in an ordered list; a lone
                    // `()` is none, and a trailing comma after named items is no connection.
                    if l.items.len() > 1 || k > 0 {
                        empty += 1;
                    }
                }
            }
        }
        if named == 0 {
            ordered += empty;
        }
        let first = toks[l.open].span;
        if ordered > 0 && named > 0 {
            let at = l.items.iter().find(|i| matches!(i.kind, ItemKind::Named { .. } | ItemKind::DotStar)).map_or(first, |i| toks[i.body].span);
            out.push(Diag::error(at, "MIXED_CONNECTIONS", format!("Ordered and named {what} connections cannot be mixed in one list")));
        } else if ordered > header.slots.len() && header.known {
            let it = l.items.iter().filter(|i| i.kind != ItemKind::DotStar).nth(header.slots.len());
            let at = it.map_or(first, |i| if i.first < i.end { toks[i.first].span } else { first });
            out.push(Diag::error(at, "TOO_MANY_CONNECTIONS", format!("{ordered} ordered {what} connections, but only {} {what}s", header.slots.len())));
        }
    }

    /// Completion of port or parameter names at the name position of a connection item
    /// (`.cl|` in `u (.clk(c), .cl|`): `line` is the cursor's line up to the cursor, which
    /// starts at `line_start` in this snapshot's text.
    pub fn connection_completions(&self, file: FileId, line_start: u32, line: &str) -> Option<Vec<CompletionItem>> {
        // `.name` being typed, after `(` or `,` (or at the line's start in a list).
        let trimmed = line.trim_end();
        if trimmed.len() != line.len() {
            return None;
        }
        let prefix: String = line.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<Vec<_>>().into_iter().rev().collect();
        let before_prefix = &line[..line.len() - prefix.len()];
        let before_dot = before_prefix.strip_suffix('.')?.trim_end();
        let lead_ok = before_dot.ends_with('(') || before_dot.ends_with(',') || before_dot.is_empty();
        if !lead_ok || before_dot.ends_with(".(") {
            return None;
        }
        // The open `(` the item is in: on this line, or before it in the file.
        let line_toks: Vec<Token> = dvi_syntax::lexer::lex(before_dot, file)
            .into_iter()
            .map(|mut t| {
                t.span = Span::new(file, t.span.start + line_start, t.span.end + line_start);
                t
            })
            .collect();
        let toks = self.file_tokens(file)?;
        // The search goes back 20,000 tokens at most: a window of the file before the line.
        let k = toks.partition_point(|t| t.span.end <= line_start);
        let file_before: Vec<&Token> = toks[k.saturating_sub(40_000)..k].iter().collect();
        let all: Vec<&Token> = file_before.iter().copied().chain(line_toks.iter()).collect();
        let mut depth = 0i32;
        let mut open = None;
        for (i, t) in all.iter().enumerate().rev() {
            if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
                depth += 1;
            } else if t.is_punct("(") || t.is_punct("[") || t.is_punct("{") {
                if depth == 0 {
                    open = Some(i);
                    break;
                }
                depth -= 1;
            } else if t.is_punct(";") || all.len() - i > 20_000 {
                return None;
            }
        }
        let open = open?;
        if !all[open].is_punct("(") || open == 0 {
            return None;
        }
        // `#(` of a definition (parameters), or `name [dims] (` of an instance (ports).
        let (def_tok, params) = if all[open - 1].is_punct("#") {
            (open.checked_sub(2)?, true)
        } else {
            let mut j = open - 1;
            while all[j].is_punct("]") {
                let mut d = 0;
                loop {
                    if all[j].is_punct("]") {
                        d += 1;
                    } else if all[j].is_punct("[") {
                        d -= 1;
                        if d == 0 {
                            break;
                        }
                    }
                    j = j.checked_sub(1)?;
                }
                j = j.checked_sub(1)?;
            }
            if all[j].kind != TokKind::Ident {
                return None;
            }
            // The instance name; the definition before it (with its `#(...)`), past the earlier
            // instances of the statement (`m u0(...), u1(`).
            let mut k = j.checked_sub(1)?;
            while all[k].is_punct(",") {
                k = k.checked_sub(1)?;
                if !all[k].is_punct(")") {
                    return None;
                }
                let mut d = 0;
                loop {
                    if all[k].is_punct(")") {
                        d += 1;
                    } else if all[k].is_punct("(") {
                        d -= 1;
                        if d == 0 {
                            break;
                        }
                    }
                    k = k.checked_sub(1)?;
                }
                k = k.checked_sub(1)?;
                while all[k].is_punct("]") {
                    let mut d = 0;
                    loop {
                        if all[k].is_punct("]") {
                            d += 1;
                        } else if all[k].is_punct("[") {
                            d -= 1;
                            if d == 0 {
                                break;
                            }
                        }
                        k = k.checked_sub(1)?;
                    }
                    k = k.checked_sub(1)?;
                }
                if all[k].kind != TokKind::Ident {
                    return None;
                }
                k = k.checked_sub(1)?;
            }
            if all[k].is_punct(")") {
                let mut d = 0;
                loop {
                    if all[k].is_punct(")") {
                        d += 1;
                    } else if all[k].is_punct("(") {
                        d -= 1;
                        if d == 0 {
                            break;
                        }
                    }
                    k = k.checked_sub(1)?;
                }
                k = k.checked_sub(2)?;
            }
            (k, false)
        };
        let def_name = all.get(def_tok).filter(|t| t.kind == TokKind::Ident)?;
        let uid = self.context_unit(file)?;
        let scope = self.scope_at(uid, file, line_start);
        let def = self.resolve_at_span(uid, def_name.span).or_else(|| self.lookup(uid, scope, &def_name.text)).or_else(|| self.lookup_design(&def_name.text))?;
        if !self.decl(def).kind.is_definition() {
            return None;
        }
        // Names already in the list (from the file's tokens of it, and this line's).
        let mut used: FxHashSet<String> = FxHashSet::default();
        for w in all[open..].windows(2) {
            if w[0].is_punct(".") && w[1].kind == TokKind::Ident {
                used.insert(w[1].text.to_string());
            }
        }
        let header = if params { self.param_header(def) } else { self.port_header(def) };
        let kids: Vec<DeclRef> = self.unit(def.unit).parsed.decl(def.decl).children.iter().map(|c| DeclRef { unit: def.unit, decl: *c }).collect();
        let mut items: Vec<CompletionItem> = Vec::new();
        let names: Vec<String> = if header.slots.iter().any(|s| s.name.is_some()) {
            header.slots.iter().filter_map(|s| s.name.clone()).collect()
        } else {
            kids.iter().filter(|c| if params { matches!(self.decl(**c).kind, DeclKind::Parameter | DeclKind::TypeParam) && !self.decl(**c).has(dflags::LOCAL_PARAM) } else { self.decl(**c).kind == DeclKind::Port }).map(|c| self.decl(*c).name.to_string()).collect()
        };
        // The first child of each name.
        let mut by_name: rustc_hash::FxHashMap<&str, DeclRef> = rustc_hash::FxHashMap::default();
        for c in &kids {
            by_name.entry(self.decl(*c).name.as_str()).or_insert(*c);
        }
        // As many as other completions give at most.
        for (k, n) in names.iter().enumerate().take(3000) {
            let d = by_name.get(n.as_str()).map(|c| self.decl(*c));
            let connected = used.contains(n);
            let detail = d.map(|d| d.detail.to_string()).filter(|s| !s.is_empty());
            items.push(CompletionItem {
                label: n.clone(),
                kind: Some(if params { CompletionItemKind::CONSTANT } else { CompletionItemKind::FIELD }),
                detail: Some(match (&detail, connected) {
                    (Some(t), true) => format!("{t} (connected)"),
                    (Some(t), false) => t.clone(),
                    (None, true) => "connected".to_string(),
                    (None, false) => if params { "parameter".to_string() } else { "port".to_string() },
                }),
                insert_text: Some(format!("{}($1)", ident(n).replace('$', "\\$"))),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                sort_text: Some(format!("{}{k:04}", if connected { 1 } else { 0 })),
                ..Default::default()
            });
        }
        Some(items)
    }

    /// A design element by name (the first one), for a definition not resolved at a position.
    fn lookup_design(&self, name: &str) -> Option<DeclRef> {
        for u in self.units.iter().flatten() {
            for (i, d) in u.parsed.decls.iter().enumerate() {
                if d.kind.is_definition() && d.name == name && d.parent == Some(0) {
                    return Some(DeclRef { unit: u.id, decl: i as DeclId });
                }
            }
        }
        None
    }
}
