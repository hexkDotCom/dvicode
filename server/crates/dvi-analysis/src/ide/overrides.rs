//! Overriding inherited methods: completion of a method's whole definition from the base's
//! declaration (`build_phase`, `run_phase`, `write`, `new`...), and an action that implements
//! the pure virtual methods a class still lacks.

use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::{dflags, DeclKind};
use dvi_syntax::lexer::lex;
use dvi_syntax::token::{TokKind, Token};
use dvi_syntax::{FileId, Span};
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

/// Names of a base class's parameters and what they are in the derived class
/// (`uvm_subscriber #(my_item)`: `T` → `my_item`).
pub type Substitution = FxHashMap<String, String>;

/// A method a class may override.
#[derive(Clone, Debug)]
pub struct Overridable {
    /// The method's name (`new` for an implicit constructor).
    pub name: SmolStr,
    /// The nearest declaration in the bases (a prototype or a definition; the base class
    /// itself for an implicit constructor).
    pub method: DeclRef,
    /// The class that declares it.
    pub owner: DeclRef,
    /// Pure virtual there (or in an interface class) and defined by no class between: a
    /// class that is not `virtual` must implement it.
    pub pure: bool,
    /// The owner's parameters as the class sees them.
    pub subst: Substitution,
    /// The class has no explicit constructor to copy: `function new();`.
    pub implicit_new: bool,
}

/// An edit of the source with a title, for a code action that fixes no problem.
#[derive(Clone, Debug)]
pub struct Refactor {
    pub title: String,
    pub edits: Vec<(Span, String)>,
}

impl Analysis {
    /// The methods `class` inherits that it may override and does not declare itself, nearest
    /// declaration first: virtual ones of its base classes (virtual in any of them), then
    /// those of the interface classes it implements that no base class defines, and the
    /// constructor of its immediate base.
    pub fn overridable_methods(&self, class: DeclRef) -> Vec<Overridable> {
        let methods = |c: DeclRef| -> Vec<DeclRef> {
            self.scope_members(c).into_iter().filter(|m| matches!(self.decl(*m).kind, DeclKind::Function | DeclKind::Task)).collect()
        };
        let own: FxHashSet<SmolStr> = methods(class).into_iter().map(|m| self.decl(m).name.clone()).collect();
        // The base classes, nearest first, each with its parameters as `class` sees them.
        let mut chain: Vec<(DeclRef, Substitution)> = Vec::new();
        let mut visited = FxHashSet::default();
        let mut cur = class;
        let mut subst = Substitution::default();
        while let Some(base) = self.class_base(cur) {
            if !visited.insert(self.decl_key(base)) || chain.len() > 32 {
                break;
            }
            let ext = self.decl(cur).extends.as_ref().map(|t| t.text.to_string()).unwrap_or_default();
            subst = self.specialize(base, &ext, &subst);
            chain.push((base, subst.clone()));
            cur = base;
        }
        // Which names are virtual in some base (a method stays virtual where an override
        // leaves out the keyword), and the nearest declaration of each.
        let virtual_names: FxHashSet<SmolStr> = chain
            .iter()
            .flat_map(|(c, _)| methods(*c))
            .filter(|m| self.decl(*m).has(dflags::VIRTUAL) || self.decl(*m).has(dflags::PURE))
            .map(|m| self.decl(m).name.clone())
            .collect();
        let mut seen = own.clone();
        let mut out = Vec::new();
        for (c, subst) in &chain {
            for m in methods(*c) {
                let d = self.decl(m);
                if d.name == "new" {
                    continue;
                }
                if !seen.insert(d.name.clone()) {
                    continue;
                }
                if d.has(dflags::LOCAL) || d.has(dflags::STATIC) || !virtual_names.contains(&d.name) {
                    continue;
                }
                out.push(Overridable { name: d.name.clone(), method: m, owner: *c, pure: d.has(dflags::PURE), subst: subst.clone(), implicit_new: false });
            }
        }
        // The immediate base's constructor (or its implicit one).
        if !own.contains("new") {
            if let Some((base, subst)) = chain.first() {
                match methods(*base).into_iter().find(|m| self.decl(*m).name == "new") {
                    Some(m) => out.push(Overridable { name: SmolStr::new_static("new"), method: m, owner: *base, pure: false, subst: subst.clone(), implicit_new: false }),
                    None => out.push(Overridable { name: SmolStr::new_static("new"), method: *base, owner: *base, pure: false, subst: subst.clone(), implicit_new: true }),
                }
            }
        }
        // Interface classes of the class and of its bases: what no base class defines.
        let mut interfaces: Vec<(DeclRef, Substitution)> = Vec::new();
        let mut seen_if = FxHashSet::default();
        let mut todo: Vec<(DeclRef, Substitution)> = std::iter::once((class, Substitution::default())).chain(chain.iter().cloned()).collect();
        while let Some((c, s)) = todo.pop() {
            for t in &self.decl(c).implements {
                let Some(rid) = t.ref_id else { continue };
                let n = self.unit(c.unit).parsed.reference(rid).segs.len();
                let Some(ic) = self.resolve_ref(c.unit, rid, n - 1).and_then(|x| self.follow_typedef(x)) else { continue };
                if self.decl(ic).kind != DeclKind::InterfaceClass || !seen_if.insert(self.decl_key(ic)) {
                    continue;
                }
                let sub = self.specialize(ic, &t.text, &s);
                interfaces.push((ic, sub.clone()));
                todo.push((ic, sub));
            }
            // An interface class extends others with `extends`.
            if self.decl(c).kind == DeclKind::InterfaceClass {
                if let Some(t) = &self.decl(c).extends {
                    if let Some(rid) = t.ref_id {
                        let n = self.unit(c.unit).parsed.reference(rid).segs.len();
                        if let Some(ic) = self.resolve_ref(c.unit, rid, n - 1).and_then(|x| self.follow_typedef(x)) {
                            if self.decl(ic).kind == DeclKind::InterfaceClass && seen_if.insert(self.decl_key(ic)) {
                                let sub = self.specialize(ic, &t.text, &s);
                                interfaces.push((ic, sub.clone()));
                                todo.push((ic, sub));
                            }
                        }
                    }
                }
            }
        }
        for (ic, subst) in interfaces {
            for m in methods(ic) {
                let d = self.decl(m);
                if seen.insert(d.name.clone()) {
                    out.push(Overridable { name: d.name.clone(), method: m, owner: ic, pure: true, subst: subst.clone(), implicit_new: false });
                }
            }
        }
        out
    }

    /// The parameters of `base` as a class that names it as `written` (`uvm_driver #(req_t)`)
    /// sees them, where that class's own parameters are `outer`.
    fn specialize(&self, base: DeclRef, written: &str, outer: &Substitution) -> Substitution {
        let u = self.unit(base.unit);
        let d = u.parsed.decl(base.decl);
        let slots: Vec<(String, String)> = d
            .children
            .iter()
            .map(|c| u.parsed.decl(*c))
            .filter(|c| matches!(c.kind, DeclKind::Parameter | DeclKind::TypeParam) && c.has(dflags::PARAM_PORT) && !c.has(dflags::LOCAL_PARAM))
            .map(|c| (c.name.to_string(), c.detail.to_string()))
            .collect();
        let args = specialization_args(written);
        let mut out = Substitution::default();
        let mut k = 0;
        for (name, default) in &slots {
            let named = args.iter().find(|(n, _)| n.as_deref() == Some(name.as_str())).map(|(_, v)| v.clone());
            let positional = args.iter().filter(|(n, _)| n.is_none()).nth(k).map(|(_, v)| v.clone());
            k += 1;
            let value = match named.or(positional) {
                // Written in the derived class: in its own terms.
                Some(v) => substitute_text(&v, outer),
                // The default, in the base's terms (its earlier parameters as now known).
                None if !default.is_empty() => substitute_text(default, &out),
                None => name.clone(),
            };
            out.insert(name.clone(), value);
        }
        out
    }

    /// The definition overriding `o`, its body indented by `indent` (one level), lines ending
    /// with `nl`, with `$0` (a snippet's cursor) in the body when `snippet`. `None` when its
    /// header cannot be read.
    /// `into`: the class it goes in (names it cannot see are qualified).
    pub fn override_text(&self, o: &Overridable, into: DeclRef, indent: &str, nl: &str, snippet: bool) -> Option<String> {
        let esc = |s: &str| if snippet { s.replace('\\', "\\\\").replace('$', "\\$").replace('}', "\\}") } else { s.to_string() };
        let cursor = if snippet { format!("{indent}$0{nl}") } else { String::new() };
        if o.implicit_new {
            return Some(format!("function new();{nl}{indent}super.new();{nl}{cursor}endfunction"));
        }
        let (header, returns) = self.method_header(o.method, &o.subst, into)?;
        let d = self.decl(o.method);
        let task = d.kind == DeclKind::Task;
        let u = self.unit(o.method.unit);
        // Names as written (escaped ones with their terminating space).
        let args: Vec<String> = d.children.iter().map(|c| u.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Argument).map(|c| super::instances::ident(&c.name)).collect();
        let call = format!("super.{}({})", super::instances::ident(&d.name), args.join(", "));
        // A call of the base's version: always in a constructor; else unless that version is
        // known to do nothing (pure, or an empty body).
        let delegate = d.name == "new" || !o.pure && self.has_body(o.method) != Some(false);
        let mut text = esc(&header);
        text.push_str(nl);
        match (delegate, returns) {
            (true, true) => {
                // The new code before the base's result is returned.
                text.push_str(&cursor);
                text.push_str(&format!("{indent}{}{nl}", esc(&format!("return {call};"))));
            }
            (true, false) => {
                text.push_str(&format!("{indent}{}{nl}", esc(&format!("{call};"))));
                text.push_str(&cursor);
            }
            (false, _) => text.push_str(&cursor),
        }
        text.push_str(if task { "endtask" } else { "endfunction" });
        Some(text)
    }

    /// The header of a method as written (`virtual function void build_phase(uvm_phase phase);`),
    /// on one line, without `extern` or `pure`, with the owner's parameters replaced as in
    /// `subst`; and whether it returns a value.
    fn method_header(&self, m: DeclRef, subst: &Substitution, into: DeclRef) -> Option<(String, bool)> {
        let d = self.decl(m);
        let text = self.file_text(d.span.file)?;
        let src = text.get(d.span.start as usize..d.span.end as usize)?;
        let toks = lex(src, d.span.file);
        let semi = header_end(&toks)?;
        let toks = &toks[..=semi];
        let f = toks.iter().position(|t| t.is_kw("function") || t.is_kw("task"))?;
        // Arguments declared in the body (a non-ANSI header): not copied, so not offered.
        let has_args = self.unit(m.unit).parsed.decl(m.decl).children.iter().any(|c| self.unit(m.unit).parsed.decl(*c).kind == DeclKind::Argument);
        if has_args && !toks[f..].iter().any(|t| t.is_punct("(")) {
            return None;
        }
        // `function [automatic|static] void name(...)`
        let returns = toks[f].is_kw("function") && d.name != "new" && !toks[f + 1..].iter().find(|t| !t.is_kw("automatic") && !t.is_kw("static")).is_some_and(|t| t.is_kw("void"));
        // The source spelling of each token (backticks, escaped names), one space where the
        // source had space or a comment between two tokens.
        let mut out = String::new();
        let mut prev_end: Option<u32> = None;
        // A formal named as a class parameter is that formal, not the parameter.
        let formals: FxHashSet<SmolStr> = {
            let p = &self.unit(m.unit).parsed;
            p.decl(m.decl).children.iter().map(|c| p.decl(*c)).filter(|c| c.kind == DeclKind::Argument).map(|c| c.name.clone()).collect()
        };
        for t in toks {
            if t.kind == TokKind::Keyword && matches!(t.text.as_str(), "extern" | "pure") {
                continue;
            }
            let spelled = &src[t.span.start as usize..t.span.end as usize];
            if let Some(e) = prev_end {
                if t.span.start > e && !out.is_empty() {
                    out.push(' ');
                }
            }
            match subst.get(t.text.as_str()).filter(|_| t.kind == TokKind::Ident && !formals.contains(t.text.as_str())) {
                Some(v) => out.push_str(v),
                None => {
                    // A package's name the class it goes in does not see: qualified.
                    if let Some(p) = self.package_qualifier(m, d.span.start, t, &toks, into) {
                        out.push_str(&p);
                        out.push_str("::");
                    }
                    out.push_str(spelled);
                }
            }
            // An escaped name ends at white space (the one after it in the source counts).
            if spelled.starts_with('\\') {
                out.push(' ');
                prev_end = Some(t.span.end + 1);
                continue;
            }
            prev_end = Some(t.span.end);
        }
        Some((out, returns))
    }

    /// For a name in method `m`'s header (`t`, offsets from `base`): the package to qualify it
    /// with, when it is a package's type or constant that class `into` does not see by that
    /// name.
    fn package_qualifier(&self, m: DeclRef, base: u32, t: &dvi_syntax::Token, toks: &[dvi_syntax::Token], into: DeclRef) -> Option<String> {
        if t.kind != TokKind::Ident {
            return None;
        }
        let k = toks.iter().position(|x| std::ptr::eq(x, t))?;
        if k > 0 && toks[k - 1].is_punct("::") {
            return None;
        }
        let file = self.decl(m).span.file;
        let at = dvi_syntax::Span::new(file, base + t.span.start, base + t.span.end);
        let r = self.resolve_at_span(m.unit, at)?;
        let rd = self.decl(r);
        let parent = rd.parent.map(|p| DeclRef { unit: r.unit, decl: p })?;
        if self.decl(parent).kind != DeclKind::Package {
            return None;
        }
        let key = self.decl_key(self.canonical(r));
        let seen = self.lookup(into.unit, into.decl, &t.text).map(|x| self.decl_key(self.canonical(x)));
        (seen != Some(key)).then(|| self.decl(parent).name.to_string())
    }

    /// Whether the definition of `m` (its body outside the class, for a prototype) does
    /// anything more than an empty body or a bare `return;`; `None` when it cannot be found.
    fn has_body(&self, m: DeclRef) -> Option<bool> {
        let d = self.decl(m);
        if d.has(dflags::PURE) {
            return Some(false);
        }
        let def = if d.has(dflags::PROTOTYPE) { self.implementation_of(m)? } else { m };
        let dd = self.decl(def);
        let text = self.file_text(dd.span.file)?;
        let src = text.get(dd.span.start as usize..dd.span.end as usize)?;
        let toks = lex(src, dd.span.file);
        let semi = header_end(&toks)?;
        Some(
            toks[semi + 1..]
                .iter()
                .filter(|t| !(t.kind == TokKind::Keyword && matches!(t.text.as_str(), "endfunction" | "endtask" | "return")))
                .filter(|t| !t.is_punct(";") && !t.is_punct(":"))
                // `endfunction : name`
                .any(|t| t.text != d.name),
        )
    }

    /// "Implement the pure virtual methods" for the class around `off`, when it lacks some:
    /// their definitions inserted before its `endclass`.
    pub fn implement_pure_methods(&self, file: FileId, off: u32) -> Option<Refactor> {
        let uid = self.context_unit(file)?;
        let scope = self.scope_at(uid, file, off);
        let class = self.enclosing_class(uid, scope)?;
        let cd = self.decl(class);
        if cd.span.file != file || cd.kind != DeclKind::Class {
            return None;
        }
        let missing: Vec<Overridable> = self.overridable_methods(class).into_iter().filter(|o| o.pure).collect();
        if missing.is_empty() {
            return None;
        }
        let text = self.file_text(file)?;
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        // The class's own `endclass`: the last token of its text, or before its `: name`.
        let src = text.get(cd.span.start as usize..cd.span.end as usize)?;
        let toks = lex(src, file);
        let end_i = toks.iter().rposition(|t| t.is_kw("endclass"))?;
        let rest = &toks[end_i + 1..];
        if !(rest.is_empty() || rest.len() == 2 && rest[0].is_punct(":") && rest[1].kind == TokKind::Ident) {
            return None;
        }
        let end_at = cd.span.start as usize + toks[end_i].span.start as usize;
        let line_start = text[..end_at].rfind('\n').map_or(0, |i| i + 1);
        let before = &text[line_start..end_at];
        let alone = before.chars().all(char::is_whitespace);
        let class_line = text[..cd.span.start as usize].rfind('\n').map_or(0, |i| i + 1);
        let class_indent: String = text[class_line..].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        // Indented like the class's members (not its header parameters), else one level more.
        let member_indent = cd
            .children
            .iter()
            .map(|c| self.unit(class.unit).parsed.decl(*c))
            .filter(|c| !c.has(dflags::PARAM_PORT) && c.span.file == file && c.span.start > cd.span.start)
            .find_map(|c| {
                let ls = text[..c.span.start as usize].rfind('\n').map_or(0, |i| i + 1);
                let ind = &text[ls..c.span.start as usize];
                (ls > class_line && ind.chars().all(|ch| ch == ' ' || ch == '\t') && ind.len() > class_indent.len()).then(|| ind.to_string())
            })
            .unwrap_or_else(|| format!("{class_indent}  "));
        let step = member_indent.strip_prefix(class_indent.as_str()).unwrap_or("  ").to_string();
        let mut insert = String::new();
        if !alone {
            insert.push_str(nl);
        }
        for o in &missing {
            let t = self.override_text(o, class, &step, "\n", false)?;
            insert.push_str(nl);
            for line in t.lines() {
                insert.push_str(&member_indent);
                insert.push_str(line);
                insert.push_str(nl);
            }
        }
        if !alone {
            insert.push_str(&class_indent);
        }
        let at = if alone { line_start } else { end_at } as u32;
        let names: Vec<&str> = missing.iter().map(|o| o.name.as_str()).collect();
        let title = if missing.len() == 1 { format!("Implement pure virtual method {}", names[0]) } else { format!("Implement {} pure virtual methods ({})", missing.len(), names.join(", ")) };
        Some(Refactor { title, edits: vec![(Span::new(file, at, at), insert)] })
    }
}

/// The index of the `;` that ends a method's header, outside parentheses.
fn header_end(toks: &[Token]) -> Option<usize> {
    let mut depth = 0i32;
    toks.iter().position(|t| {
        if t.is_punct("(") {
            depth += 1;
        } else if t.is_punct(")") {
            depth -= 1;
        }
        depth == 0 && t.is_punct(";")
    })
}

/// The values of a specialization `name #(a, .P(b))`: `(None, "a")`, `(Some("P"), "b")`.
fn specialization_args(written: &str) -> Vec<(Option<String>, String)> {
    // `#(`, or `#` and `(` with space between.
    let Some(body) = written.match_indices('#').find_map(|(at, _)| written[at + 1..].trim_start().strip_prefix('(')) else { return Vec::new() };
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut item = String::new();
    let (mut in_str, mut esc) = (false, false);
    for ch in body.chars() {
        if in_str {
            item.push(ch);
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => depth -= 1,
            ')' => break,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut item));
                continue;
            }
            _ => {}
        }
        item.push(ch);
    }
    if !item.trim().is_empty() {
        out.push(item);
    }
    out.into_iter()
        .map(|s| {
            let s = s.trim().to_string();
            match s.strip_prefix('.').and_then(|r| r.split_once('(')) {
                Some((n, v)) => {
                    let v = v.trim_end();
                    (Some(n.trim().to_string()), v.strip_suffix(')').unwrap_or(v).trim().to_string())
                }
                None => (None, s),
            }
        })
        .collect()
}

/// `text` with its identifiers replaced as in `subst`.
fn substitute_text(text: &str, subst: &Substitution) -> String {
    if subst.is_empty() {
        return text.to_string();
    }
    let toks = lex(text, 0);
    let mut out = String::new();
    let mut last = 0usize;
    for t in &toks {
        if t.kind == TokKind::Ident {
            if let Some(v) = subst.get(t.text.as_str()) {
                out.push_str(&text[last..t.span.start as usize]);
                out.push_str(v);
                last = t.span.end as usize;
            }
        }
    }
    out.push_str(&text[last..]);
    out
}
