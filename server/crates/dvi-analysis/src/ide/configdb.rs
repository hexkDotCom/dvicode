//! `uvm_config_db`: which `set()` calls can provide a `get()`, and which `get()` calls a `set()`
//! can reach, matched as UVM 1.2 matches them.
//!
//! - Field: the same literal, case-sensitive (UVM 1.2 looks fields up exactly: a set of `"*"`
//!   provides no field `"cfg"`).
//! - Type: the same declaration (or the same built-in type); a derived class does not satisfy
//!   a get of its base. Differing parameter values are "unknown", not "different".
//! - Scope: the set's `{context}.{inst}` is a glob (`*`, `?`, `+`, anchored) matched against
//!   the get's `{context}.{inst}`; a context is placed through the verification hierarchy of
//!   the configured test (`this` in a component class), or is the root (`null`,
//!   `uvm_root::get()`, `uvm_top`). Anything else is "unknown", never "no match".
//!
//! Nothing here claims which set wins at run time: during build a set from a context higher in
//! the hierarchy takes precedence, which is all that is said.

use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::DeclKind;
use dvi_syntax::{FileId, Span, TokKind, Token};
use lsp_types::{CompletionItem, CompletionItemKind, Location};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    Get,
    Set,
}

/// The type a call is specialized with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CfgType {
    /// A declaration (class, interface, typedef followed) and the text of its parameter values
    /// (`#(8)`), or of its modport for a virtual interface.
    Decl((u32, u32), String),
    /// A built-in type, its text normalized (`bit [7:0]`).
    Builtin(String),
    /// Not resolved.
    Unknown,
}

/// The context argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ctx {
    /// `this` (the component whose method makes the call).
    This,
    /// `null`, `uvm_root::get()`, `uvm_top`: the root, whose name is empty.
    Root,
    /// Anything else (a handle, `get_parent()`, an argument).
    Unknown,
}

/// One `uvm_config_db #(T)::set/get(...)` call.
#[derive(Clone, Debug)]
pub struct ConfigCall {
    pub kind: CallKind,
    pub unit: UnitId,
    /// The `get`/`set` name.
    pub method: Span,
    /// From `uvm_config_db` to the closing `)`.
    pub call: Span,
    pub ty_text: String,
    pub ty: CfgType,
    pub ctx: Ctx,
    pub ctx_text: String,
    /// The `inst_name` and `field_name` literals (decoded), `None` when computed.
    pub inst: Option<String>,
    pub field: Option<String>,
    /// The field argument (a string literal, quotes included).
    pub field_span: Option<Span>,
    pub value_text: String,
    /// The class whose method makes it (for `this`), and that method's name.
    pub class: Option<DeclRef>,
    pub in_method: String,
}

/// Every call of the project, by field.
#[derive(Default)]
pub struct ConfigIndex {
    pub calls: Vec<ConfigCall>,
    by_field: FxHashMap<String, Vec<usize>>,
    /// Calls whose field is computed: candidates for any field.
    unknown_field: Vec<usize>,
    by_file: FxHashMap<FileId, Vec<usize>>,
}

/// Where the hierarchy places component classes, per test.
#[derive(Default)]
pub struct Places {
    /// (test, class key → (path, path known)).
    pub tests: Vec<(String, FxHashMap<(u32, u32), Vec<(String, bool)>>)>,
    /// A test's hierarchy is cut somewhere, or a class has more places than are kept: a place
    /// may be missing, so "no match" is not certain.
    pub incomplete: bool,
}

/// Places kept per class and test.
const MAX_PLACES: usize = 256;

/// How one dimension of a match stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Match,
    Mismatch,
    Unknown,
}

/// A set and a get compared.
#[derive(Clone, Debug)]
pub struct Pairing {
    pub field: Status,
    pub ty: Status,
    pub scope: Status,
    /// The lookup name and the scope pattern that matched (or a reason).
    pub scope_text: String,
    /// The set runs during build and its context's depth is known: UVM gives it precedence
    /// `1000 - depth` there.
    pub precedence: Option<u32>,
}

impl Pairing {
    /// Nothing known against it.
    pub fn candidate(&self) -> bool {
        self.field != Status::Mismatch && self.ty != Status::Mismatch && self.scope != Status::Mismatch
    }
}

/// UVM's glob (`uvm_glob_to_re`): `*` any text (dots too), `?` one character, `+` one or
/// more, the rest literal, the whole string.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Positions of the pattern that can stand at each text position (no recursion).
    let mut cur = vec![false; p.len() + 1];
    cur[0] = true;
    let close = |v: &mut Vec<bool>| {
        for i in 0..p.len() {
            if v[i] && p[i] == '*' {
                v[i + 1] = true;
            }
        }
    };
    close(&mut cur);
    for &c in &t {
        let mut next = vec![false; p.len() + 1];
        for i in 0..p.len() {
            if !cur[i] {
                continue;
            }
            match p[i] {
                '*' => next[i] = true,
                '+' => {
                    next[i] = true;
                    next[i + 1] = true;
                }
                '?' => next[i + 1] = true,
                x if x == c => next[i + 1] = true,
                _ => {}
            }
        }
        close(&mut next);
        cur = next;
    }
    cur[p.len()]
}

/// `{context}.{inst}` as UVM forms it: the context's name alone when `inst` is empty, `inst`
/// alone when the context is the root.
pub fn scope_of(context: &str, inst: &str) -> String {
    match (context.is_empty(), inst.is_empty()) {
        (_, true) => context.to_string(),
        (true, false) => inst.to_string(),
        _ => format!("{context}.{inst}"),
    }
}

/// A string literal's value, its escapes decoded (`\n`, `\t`, `\\`, `\"`, `\v`, `\f`, `\a`,
/// octal `\ddd`, hex `\xhh`, a line continuation); `None` for an escape it does not know.
fn literal(t: &Token) -> Option<String> {
    if t.kind != TokKind::String {
        return None;
    }
    let s = t.text.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            'v' => out.push('\u{0b}'),
            'f' => out.push('\u{0c}'),
            'a' => out.push('\u{07}'),
            '\n' => {}
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
            }
            d @ '0'..='7' => {
                let mut v = d.to_digit(8)?;
                for _ in 0..2 {
                    match it.peek().and_then(|x| x.to_digit(8)) {
                        Some(x) => {
                            v = v * 8 + x;
                            it.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(v)?);
            }
            'x' => {
                let (mut v, mut n) = (0, 0);
                while n < 2 {
                    match it.peek().and_then(|x| x.to_digit(16)) {
                        Some(x) => {
                            v = v * 16 + x;
                            it.next();
                            n += 1;
                        }
                        None => break,
                    }
                }
                if n == 0 {
                    return None;
                }
                out.push(char::from_u32(v)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// A built-in type's text as compared: no spaces, default signedness dropped (`int signed`
/// is `int`, `bit unsigned [7:0]` is `bit[7:0]`).
fn builtin_text(ty: &[Token]) -> String {
    let mut w: Vec<&str> = ty.iter().map(|t| t.text.as_str()).collect();
    if w.len() >= 2 && ["byte", "shortint", "int", "longint", "integer"].contains(&w[0]) && w[1] == "signed" {
        w.remove(1);
    }
    if w.len() >= 2 && ["bit", "logic", "reg"].contains(&w[0]) && w[1] == "unsigned" {
        w.remove(1);
    }
    w.concat()
}

/// The argument tokens of a call's items by name (`cntxt`, `inst_name`, `field_name`,
/// `value`): positional ones in that order, named ones by name.
fn arguments<'t>(toks: &'t [Token], items: &[(usize, usize)]) -> [Option<&'t [Token]>; 4] {
    const NAMES: [&str; 4] = ["cntxt", "inst_name", "field_name", "value"];
    let mut out: [Option<&[Token]>; 4] = [None; 4];
    for (k, &(a, b)) in items.iter().enumerate() {
        let item = &toks[a..b];
        if let [dot, name, open, inner @ .., close] = item {
            if dot.is_punct(".") && open.is_punct("(") && close.is_punct(")") {
                if let Some(i) = NAMES.iter().position(|n| *n == name.text.as_str()) {
                    out[i] = Some(inner);
                }
                continue;
            }
        }
        if k < 4 {
            out[k] = Some(item);
        }
    }
    out
}

impl Analysis {
    /// The `uvm_config_db` calls of the project (made once per snapshot).
    pub fn config_index(&self) -> Arc<ConfigIndex> {
        if let Some(i) = self.config_calls.get() {
            return i.clone();
        }
        let idx = Arc::new(self.build_config_index());
        // A cancelled build is partial: used for this request only.
        if !self.cancelled() {
            let _ = self.config_calls.set(idx.clone());
        }
        idx
    }

    fn build_config_index(&self) -> ConfigIndex {
        let mut idx = ConfigIndex::default();
        let mut files: FxHashSet<FileId> = FxHashSet::default();
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin || u.library {
                continue;
            }
            if self.cancelled() {
                break;
            }
            for &f in &u.files {
                // Each file once, in the unit it is read in (the chosen one, if any).
                if self.chosen_unit(f).is_some_and(|c| c != u.id) || !files.insert(f) {
                    continue;
                }
                if !self.file_text(f).is_some_and(|t| t.contains("uvm_config_db")) {
                    continue;
                }
                self.index_file(u.id, f, &mut idx);
            }
        }
        for (i, c) in idx.calls.iter().enumerate() {
            match &c.field {
                Some(f) => idx.by_field.entry(f.clone()).or_default().push(i),
                None => idx.unknown_field.push(i),
            }
            idx.by_file.entry(c.method.file).or_default().push(i);
        }
        idx
    }

    /// The calls that may use field `f` (`None`: every call).
    fn calls_of_field(idx: &ConfigIndex, f: Option<&str>) -> Vec<usize> {
        match f {
            Some(f) => idx.by_field.get(f).into_iter().flatten().chain(idx.unknown_field.iter()).copied().collect(),
            None => (0..idx.calls.len()).collect(),
        }
    }

    fn index_file(&self, unit: UnitId, f: FileId, idx: &mut ConfigIndex) {
        let Some(toks) = self.file_tokens(f) else { return };
        let u = self.unit(unit);
        let inactive: Vec<Span> = u.inactive.iter().filter(|s| s.file == f).copied().collect();
        // The methods of the file, by start: the innermost one around a call is found by search.
        let mut methods: Vec<(Span, dvi_syntax::ast::DeclId)> =
            u.parsed.decls.iter().enumerate().filter(|(_, d)| matches!(d.kind, DeclKind::Function | DeclKind::Task) && d.span.file == f).map(|(i, d)| (d.span, i as dvi_syntax::ast::DeclId)).collect();
        methods.sort_by_key(|m| m.0.start);
        let mut i = 0;
        while i < toks.len() {
            if toks[i].text != "uvm_config_db" || toks[i].kind != TokKind::Ident {
                i += 1;
                continue;
            }
            let start = i;
            let mut j = i + 1;
            let mut ty: &[Token] = &[];
            if toks.get(j).is_some_and(|t| t.is_punct("#")) && toks.get(j + 1).is_some_and(|t| t.is_punct("(")) {
                let end = super::hints::skip(&toks, j + 1);
                ty = &toks[j + 2..end.saturating_sub(1).max(j + 2)];
                j = end;
            }
            i = j.max(i + 1);
            let kind = match (toks.get(j), toks.get(j + 1), toks.get(j + 2)) {
                (Some(c), Some(m), Some(o)) if c.is_punct("::") && o.is_punct("(") && m.text == "set" => CallKind::Set,
                (Some(c), Some(m), Some(o)) if c.is_punct("::") && o.is_punct("(") && m.text == "get" => CallKind::Get,
                _ => continue,
            };
            let (items, after) = super::hints::list(&toks, j + 2);
            let [ctx, inst, field, value] = arguments(&toks, &items);
            let text = |t: Option<&[Token]>| t.map(super::views::tokens_text).unwrap_or_default();
            let ctx_t = ctx.unwrap_or(&[]);
            let ctx_kind = match ctx_t {
                [t] if t.is_kw("this") => Ctx::This,
                [t] if t.is_kw("null") || t.text == "uvm_top" => Ctx::Root,
                [a, c, g, o, cl] if a.text == "uvm_root" && c.is_punct("::") && (g.text == "get" || g.text == "get_inst") && o.is_punct("(") && cl.is_punct(")") => Ctx::Root,
                _ => Ctx::Unknown,
            };
            let lit = |t: Option<&[Token]>| match t {
                Some([s]) => literal(s),
                _ => None,
            };
            let method = toks[j + 1].span;
            if inactive.iter().any(|s| s.start <= method.start && method.end <= s.end) {
                i = after.max(i);
                continue;
            }
            let (class, in_method) = self.enclosing_class_and_method(unit, &methods, method.start);
            let call_end = toks.get(after.saturating_sub(1)).map_or(method, |t| t.span);
            idx.calls.push(ConfigCall {
                kind,
                unit,
                method,
                call: Span::new(f, toks[start].span.start, call_end.end),
                ty_text: super::views::tokens_text(ty),
                ty: self.config_type(unit, ty),
                ctx: ctx_kind,
                ctx_text: text(ctx),
                inst: lit(inst),
                field: lit(field),
                field_span: match field {
                    Some([s]) if s.kind == TokKind::String => Some(s.span),
                    _ => None,
                },
                value_text: text(value),
                class,
                in_method,
            });
            i = after.max(i);
        }
    }

    /// The class (component) and the method a position is in, from the file's methods by start.
    fn enclosing_class_and_method(&self, unit: UnitId, methods: &[(Span, dvi_syntax::ast::DeclId)], off: u32) -> (Option<DeclRef>, String) {
        let u = self.unit(unit);
        let k = methods.partition_point(|m| m.0.start <= off);
        let Some(&(_, m)) = methods[..k].iter().rev().take(64).find(|m| m.0.end >= off) else { return (None, String::new()) };
        let method = u.parsed.decl(m).name.to_string();
        let mut d = u.parsed.decl(m).parent;
        while let Some(id) = d {
            let decl = u.parsed.decl(id);
            if decl.kind == DeclKind::Class {
                return (Some(DeclRef { unit, decl: id }), method);
            }
            if decl.kind.is_definition() {
                break;
            }
            d = decl.parent;
        }
        // An out-of-block body (`function C::build_phase`): its class through its owner.
        (self.owner_class(DeclRef { unit, decl: m }), method)
    }

    /// The type `uvm_config_db #(...)` is specialized with.
    fn config_type(&self, unit: UnitId, ty: &[Token]) -> CfgType {
        if ty.is_empty() {
            return CfgType::Unknown;
        }
        // `virtual [interface] name[.modport]`
        if ty[0].is_kw("virtual") {
            let mut k = 1;
            if ty.get(k).is_some_and(|t| t.is_kw("interface")) {
                k += 1;
            }
            // `p::bus_if`: the last name of the chain.
            while ty.get(k).is_some_and(|t| t.kind == TokKind::Ident) && ty.get(k + 1).is_some_and(|t| t.is_punct("::")) {
                k += 2;
            }
            let Some(name) = ty.get(k).filter(|t| t.kind == TokKind::Ident) else { return CfgType::Unknown };
            let rest = builtin_text(&ty[k + 1..]);
            return match self.resolve_at_span(unit, name.span).and_then(|d| self.follow_typedef(d)) {
                Some(d) if self.decl(d).kind != DeclKind::TypeParam => CfgType::Decl(self.decl_key(self.canonical(d)), rest),
                _ => CfgType::Unknown,
            };
        }
        if ty.iter().all(|t| matches!(t.kind, TokKind::Keyword | TokKind::Number) || t.is_punct("[") || t.is_punct("]") || t.is_punct(":") || t.is_punct("-")) {
            return CfgType::Builtin(builtin_text(ty));
        }
        // `[pkg::]name [#(...)]`
        let hash = ty.iter().position(|t| t.is_punct("#")).unwrap_or(ty.len());
        let Some(name) = ty[..hash].iter().rev().find(|t| t.kind == TokKind::Ident) else { return CfgType::Unknown };
        let params = builtin_text(&ty[hash..]);
        match self.resolve_at_span(unit, name.span).and_then(|d| self.follow_typedef(d)) {
            // A type parameter is whatever the specialization makes it.
            Some(d) if self.decl(d).kind != DeclKind::TypeParam => CfgType::Decl(self.decl_key(self.canonical(d)), params),
            _ => CfgType::Unknown,
        }
    }

    /// Where the configured test's hierarchy (every test's, when none is configured) places
    /// each component class.
    pub fn config_places(&self) -> Arc<Places> {
        if let Some(p) = self.config_places_cache.get() {
            return p.clone();
        }
        let p = Arc::new(self.build_places());
        if !self.cancelled() {
            let _ = self.config_places_cache.set(p.clone());
        }
        p
    }

    fn build_places(&self) -> Places {
        let h = self.verification_hierarchy(None, 200_000);
        let h = if h.roots.is_empty() && h.test.is_none() { self.verification_hierarchy(Some("*"), 200_000) } else { h };
        let mut out = Places::default();
        for root in &h.roots {
            let mut map: FxHashMap<(u32, u32), Vec<(String, bool)>> = FxHashMap::default();
            // A node's path is a place of its class and of every base class (a base's method
            // runs with `this` being the node).
            fn walk(a: &Analysis, n: &super::views::VerifNode, map: &mut FxHashMap<(u32, u32), Vec<(String, bool)>>, incomplete: &mut bool) {
                *incomplete |= n.truncated;
                let mut class = n.class_decl;
                let mut steps = 0;
                while let Some(c) = class {
                    let v = map.entry(a.decl_key(a.canonical(c))).or_default();
                    if v.len() < MAX_PLACES {
                        v.push((n.path.clone(), n.path_known && n.count.is_none()));
                    } else {
                        *incomplete = true;
                    }
                    steps += 1;
                    if steps > 32 {
                        break;
                    }
                    class = a.class_base(c);
                }
                for c in &n.children {
                    walk(a, c, map, incomplete);
                }
            }
            walk(self, root, &mut map, &mut out.incomplete);
            out.tests.push((root.class.clone(), map));
        }
        out
    }

    /// Compares set `s` with get `g`.
    pub fn config_pair(&self, s: &ConfigCall, g: &ConfigCall) -> Pairing {
        let field = match (&s.field, &g.field) {
            (Some(a), Some(b)) if a == b => Status::Match,
            (Some(_), Some(_)) => Status::Mismatch,
            _ => Status::Unknown,
        };
        let ty = match (&s.ty, &g.ty) {
            (CfgType::Decl(a, pa), CfgType::Decl(b, pb)) if a == b => {
                if pa == pb {
                    Status::Match
                } else {
                    Status::Unknown
                }
            }
            (CfgType::Builtin(a), CfgType::Builtin(b)) if a == b => Status::Match,
            (CfgType::Unknown, _) | (_, CfgType::Unknown) => Status::Unknown,
            _ => Status::Mismatch,
        };
        let places = self.config_places();
        let mut scope = Status::Mismatch;
        let mut scope_text = String::new();
        let mut placed = false;
        let (mut g_any, mut s_any) = (false, false);
        let mut precedence = None;
        for (test, map) in &places.tests {
            let names = |c: &ConfigCall| -> Option<Vec<(String, bool)>> {
                match c.ctx {
                    Ctx::Root => Some(vec![(String::new(), true)]),
                    Ctx::This => c.class.and_then(|k| map.get(&self.decl_key(self.canonical(k)))).cloned(),
                    Ctx::Unknown => None,
                }
            };
            let (gn, sn) = (names(g), names(s));
            g_any |= gn.is_some();
            s_any |= sn.is_some();
            let (Some(gn), Some(sn)) = (gn, sn) else {
                // A context that is not placed: unknown, unless the class is not in this test.
                let absent = |c: &ConfigCall| c.ctx == Ctx::This && c.class.is_some_and(|k| !map.contains_key(&self.decl_key(self.canonical(k))));
                if !absent(g) && !absent(s) && scope != Status::Match {
                    scope = Status::Unknown;
                    scope_text = "context not resolved".into();
                }
                continue;
            };
            placed = true;
            for (gp, gk) in &gn {
                for (sp, sk) in &sn {
                    let (Some(gi), Some(si)) = (&g.inst, &s.inst) else {
                        if scope != Status::Match {
                            scope = Status::Unknown;
                            scope_text = "a computed instance name".into();
                        }
                        continue;
                    };
                    let lookup = scope_of(gp, gi);
                    let pattern = scope_of(sp, si);
                    if !gk || !sk || (pattern.starts_with('/') && pattern.ends_with('/') && pattern.len() > 1) {
                        if scope != Status::Match {
                            scope = Status::Unknown;
                            scope_text = if pattern.starts_with('/') { format!("regular expression {pattern}") } else { format!("path not known exactly ({lookup})") };
                        }
                        continue;
                    }
                    if glob_match(&pattern, &lookup) {
                        if scope != Status::Match {
                            scope = Status::Match;
                            scope_text = format!("{lookup} matches {pattern}{}", if places.tests.len() > 1 { format!(" (test {test})") } else { String::new() });
                            if s.in_method == "build_phase" {
                                precedence = Some(1000u32.saturating_sub(if sp.is_empty() { 0 } else { sp.split('.').count() as u32 }));
                            }
                        }
                    } else if scope == Status::Mismatch && scope_text.is_empty() {
                        scope_text = format!("{lookup} does not match {pattern}");
                    }
                }
            }
        }
        if !placed && scope == Status::Mismatch {
            // Both placed, never in the same test: they cannot meet.
            if g_any && s_any && !places.incomplete {
                scope_text = "not in the same test".into();
            } else {
                scope = Status::Unknown;
                if scope_text.is_empty() {
                    scope_text = "not placed in the hierarchy of a test".into();
                }
            }
        }
        if scope == Status::Mismatch && places.incomplete {
            scope = Status::Unknown;
            scope_text = format!("{scope_text} (the hierarchy is not complete)");
        }
        Pairing { field, ty, scope, scope_text, precedence }
    }

    /// Whether `off` can be in a config_db call: a `get`/`set` name or a string, in a file that
    /// names `uvm_config_db` (checked before the index is built).
    fn maybe_config_at(&self, file: FileId, off: u32) -> bool {
        if !self.file_text(file).is_some_and(|t| t.contains("uvm_config_db")) {
            return false;
        }
        let Some(toks) = self.file_tokens(file) else { return false };
        let k = toks.partition_point(|t| t.span.end < off);
        toks.get(k).is_some_and(|t| t.span.start <= off && (t.kind == TokKind::String || t.text == "get" || t.text == "set"))
    }

    /// The config_db call whose `get`/`set` name or field string is at `off`.
    fn config_call_at(&self, idx: &ConfigIndex, file: FileId, off: u32) -> Option<usize> {
        idx.by_file.get(&file)?.iter().copied().find(|&i| {
            let c = &idx.calls[i];
            (c.method.start <= off && off <= c.method.end) || c.field_span.is_some_and(|s| s.start <= off && off <= s.end)
        })
    }

    /// Hover on a config_db `get`/`set`: the candidate providers or consumers.
    pub fn config_hover(&self, file: FileId, off: u32) -> Option<(String, Span)> {
        if !self.maybe_config_at(file, off) {
            return None;
        }
        let idx = self.config_index();
        let i = self.config_call_at(&idx, file, off)?;
        let c = &idx.calls[i];
        let what = if c.kind == CallKind::Get { "get" } else { "set" };
        let field = c.field.as_deref().map_or_else(|| "a computed field".to_string(), |f| format!("`\"{f}\"`"));
        let mut md = format!("**uvm_config_db #({}) {what}** {field}", if c.ty_text.is_empty() { "?" } else { &c.ty_text });
        let others = Self::calls_of_field(&idx, c.field.as_deref());
        let mut good = Vec::new();
        let mut near = Vec::new();
        for j in others.into_iter().filter(|&j| idx.calls[j].kind != c.kind) {
            let o = &idx.calls[j];
            let (s, g) = if c.kind == CallKind::Get { (o, c) } else { (c, o) };
            let p = self.config_pair(s, g);
            if p.candidate() {
                good.push((j, p));
            } else if p.field == Status::Match {
                near.push((j, p));
            }
        }
        let status = |x: Status| match x {
            Status::Match => "matches",
            Status::Mismatch => "differs",
            Status::Unknown => "not known",
        };
        let line = |j: usize, p: &Pairing| -> String {
            let o = &idx.calls[j];
            let at = self.to_location(o.call).map(|l| format!("{}:{}", self.path(o.call.file).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), l.range.start.line + 1)).unwrap_or_default();
            let mut s = format!("- {at} `{}`", self.span_text(o.call));
            if p.ty != Status::Match {
                s.push_str(&format!(" — type {} (`{}`)", status(p.ty), o.ty_text));
            }
            if !p.scope_text.is_empty() {
                s.push_str(&format!(" — scope {}: {}", status(p.scope), p.scope_text));
            }
            if let Some(pr) = p.precedence {
                s.push_str(&format!(" — during build, precedence {pr}"));
            }
            s
        };
        let (them, none) = if c.kind == CallKind::Get { ("Candidate providers", "No indexed `set` provides it") } else { ("Candidate consumers", "No indexed `get` reads it") };
        if good.is_empty() {
            md.push_str(&format!("\n\n{none} in this build configuration."));
        } else {
            md.push_str(&format!("\n\n**{them}** ({})", good.len()));
            for (j, p) in good.iter().take(20) {
                md.push('\n');
                md.push_str(&line(*j, p));
            }
            if good.len() > 20 {
                md.push_str(&format!("\n- and {} more (Find References on the field lists them)", good.len() - 20));
            }
        }
        if !near.is_empty() {
            md.push_str("\n\n**Same field, not matching**");
            for (j, p) in near.iter().take(10) {
                md.push('\n');
                md.push_str(&line(*j, p));
            }
        }
        if c.kind == CallKind::Get {
            md.push_str("\n\n*A value may also come from `uvm_resource_db`, `set_config_*` or the command line, which are not indexed.*");
        }
        let span = if c.method.start <= off && off <= c.method.end { c.method } else { c.field_span.unwrap_or(c.method) };
        Some((md, span))
    }

    /// The source text of a span, on one line, shortened.
    fn span_text(&self, s: Span) -> String {
        let Some(t) = self.file_text(s.file) else { return String::new() };
        let one = t.get(s.start as usize..s.end as usize).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
        if one.chars().count() > 120 {
            format!("{}…", one.chars().take(119).collect::<String>())
        } else {
            one
        }
    }

    /// Go to Definition on a get's field string: the candidate sets (every set of that field
    /// when none is a candidate).
    pub fn config_definition(&self, file: FileId, off: u32) -> Option<Vec<Location>> {
        if !self.maybe_config_at(file, off) {
            return None;
        }
        let idx = self.config_index();
        let i = self.config_call_at(&idx, file, off)?;
        let c = &idx.calls[i];
        if c.kind != CallKind::Get || !c.field_span.is_some_and(|s| s.start <= off && off <= s.end) {
            return None;
        }
        c.field.as_ref()?;
        let sets: Vec<usize> = Self::calls_of_field(&idx, c.field.as_deref()).into_iter().filter(|&j| idx.calls[j].kind == CallKind::Set).collect();
        let good: Vec<usize> = sets.iter().copied().filter(|&j| self.config_pair(&idx.calls[j], c).candidate()).collect();
        let pick = if good.is_empty() { sets } else { good };
        Some(pick.into_iter().filter_map(|j| self.to_location(idx.calls[j].field_span.unwrap_or(idx.calls[j].call))).collect())
    }

    /// Find References on a field string: every get and set of that literal field.
    pub fn config_references(&self, file: FileId, off: u32) -> Option<Vec<Location>> {
        if !self.maybe_config_at(file, off) {
            return None;
        }
        let idx = self.config_index();
        let i = self.config_call_at(&idx, file, off)?;
        let c = &idx.calls[i];
        if !c.field_span.is_some_and(|s| s.start <= off && off <= s.end) {
            return None;
        }
        let all = idx.by_field.get(c.field.as_ref()?)?;
        Some(all.iter().filter_map(|&j| self.to_location(idx.calls[j].field_span.unwrap_or(idx.calls[j].call))).collect())
    }

    /// Completion inside the field string of a `uvm_config_db` call written on the cursor's
    /// line (`line`, up to the cursor): the fields set or got with the same type first, then the
    /// others. The cursor must be in a string that is the field argument itself (third, or
    /// `.field_name(`), not one nested in it.
    pub fn config_field_completions(&self, line: &str) -> Option<Vec<CompletionItem>> {
        let at = line.rfind("uvm_config_db")?;
        let rest: Vec<char> = line[at + "uvm_config_db".len()..].chars().collect();
        let mut k = 0;
        let skip_ws = |k: &mut usize| {
            while rest.get(*k).is_some_and(|c| c.is_whitespace()) {
                *k += 1;
            }
        };
        skip_ws(&mut k);
        // `#( type )`, balanced (`p::cfg`, `#(8)` inside).
        let mut ty = String::new();
        if rest.get(k) == Some(&'#') {
            k += 1;
            skip_ws(&mut k);
            if rest.get(k) != Some(&'(') {
                return None;
            }
            let mut depth = 0;
            loop {
                let c = *rest.get(k)?;
                k += 1;
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                if depth > 0 && !(depth == 1 && c == '(') {
                    ty.push(c);
                }
            }
        }
        skip_ws(&mut k);
        if rest.get(k) != Some(&':') || rest.get(k + 1) != Some(&':') {
            return None;
        }
        k += 2;
        skip_ws(&mut k);
        let name: String = rest[k..].iter().take_while(|c| c.is_alphanumeric() || **c == '_').collect();
        if name != "get" && name != "set" {
            return None;
        }
        k += name.len();
        skip_ws(&mut k);
        if rest.get(k) != Some(&'(') {
            return None;
        }
        k += 1;
        // The arguments: position at depth 1, named ones, and where the open string starts.
        let (mut depth, mut arg, mut named) = (1i32, 0usize, None::<String>);
        let mut arg_start = k;
        let mut i = k;
        while i < rest.len() {
            let c = rest[i];
            match c {
                '"' => {
                    // A string: closed later on the line, or open at the cursor.
                    let mut j = i + 1;
                    let mut closed = false;
                    while j < rest.len() {
                        if rest[j] == '\\' {
                            j += 2;
                            continue;
                        }
                        if rest[j] == '"' {
                            closed = true;
                            break;
                        }
                        j += 1;
                    }
                    if !closed {
                        // The open string must be the whole argument so far.
                        let before: String = rest[arg_start..i].iter().collect();
                        let lead = before.trim();
                        let field_named = named.as_deref() == Some("field_name") && depth == 2 && lead.is_empty();
                        let field_pos = named.is_none() && depth == 1 && arg == 2 && lead.is_empty();
                        if !(field_named || field_pos) {
                            return None;
                        }
                        break;
                    }
                    i = j + 1;
                    continue;
                }
                '(' | '[' | '{' => {
                    depth += 1;
                    // `.field_name(`: a named argument opens.
                    if c == '(' && depth == 2 {
                        let before: String = rest[arg_start..i].iter().collect();
                        let b = before.trim();
                        if let Some(n) = b.strip_prefix('.') {
                            named = Some(n.trim().to_string());
                            arg_start = i + 1;
                        }
                    }
                }
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth <= 0 {
                        return None;
                    }
                }
                ',' if depth == 1 => {
                    arg += 1;
                    named = None;
                    arg_start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        if i >= rest.len() {
            return None;
        }
        let idx = self.config_index();
        let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let ty = norm(&ty);
        let mut seen: FxHashMap<String, bool> = FxHashMap::default();
        for c in &idx.calls {
            if let Some(f) = &c.field {
                let same = norm(&c.ty_text) == ty;
                let e = seen.entry(f.clone()).or_insert(false);
                *e |= same;
            }
        }
        let mut items: Vec<(bool, String)> = seen.into_iter().map(|(f, same)| (same, f)).collect();
        items.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        Some(
            items
                .into_iter()
                .take(200)
                .enumerate()
                .map(|(k, (same, f))| CompletionItem {
                    label: f.clone(),
                    kind: Some(CompletionItemKind::VALUE),
                    detail: Some(if same { format!("uvm_config_db #({ty}) field") } else { "uvm_config_db field".to_string() }),
                    sort_text: Some(format!("{k:04}")),
                    ..Default::default()
                })
                .collect(),
        )
    }
}
