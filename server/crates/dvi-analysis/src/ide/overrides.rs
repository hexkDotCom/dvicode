//! Overriding inherited methods: completion of a method's whole definition from the base's
//! declaration (`build_phase`, `run_phase`, `write`, `new`...), and an action that implements
//! the pure virtual methods a class still lacks.

use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::{dflags, DeclKind};
use dvi_syntax::lexer::lex;
use dvi_syntax::token::TokKind;
use dvi_syntax::{FileId, Span};
use rustc_hash::FxHashSet;
use smol_str::SmolStr;

/// A method a class may override.
#[derive(Clone, Debug)]
pub struct Overridable {
    /// The nearest declaration in the bases (a prototype or a definition).
    pub method: DeclRef,
    /// The class that declares it.
    pub owner: DeclRef,
    /// Pure virtual there (or in an interface class): a class that is not `virtual` must
    /// implement it.
    pub pure: bool,
}

/// An edit of the source with a title, for a code action that fixes no problem.
#[derive(Clone, Debug)]
pub struct Refactor {
    pub title: String,
    pub edits: Vec<(Span, String)>,
}

impl Analysis {
    /// The methods `class` inherits that it may override and does not declare itself: virtual
    /// ones (pure ones too) from its bases and the interface classes it implements, and the
    /// constructor of its base. The nearest declaration of each name.
    pub fn overridable_methods(&self, class: DeclRef) -> Vec<Overridable> {
        let own: FxHashSet<SmolStr> = self
            .scope_members(class)
            .into_iter()
            .map(|m| self.decl(m))
            .filter(|d| matches!(d.kind, DeclKind::Function | DeclKind::Task))
            .map(|d| d.name.clone())
            .collect();
        let mut seen = own;
        let mut visited = FxHashSet::default();
        let mut queue: Vec<DeclRef> = self.direct_bases(class);
        let mut out = Vec::new();
        let mut i = 0;
        while i < queue.len() && i < 64 {
            let c = queue[i];
            i += 1;
            if !visited.insert(self.decl_key(c)) {
                continue;
            }
            let interface = self.decl(c).kind == DeclKind::InterfaceClass;
            for m in self.scope_members(c) {
                let d = self.decl(m);
                if !matches!(d.kind, DeclKind::Function | DeclKind::Task) || d.has(dflags::LOCAL) || d.has(dflags::STATIC) {
                    continue;
                }
                let constructor = d.name == "new";
                if !(constructor || interface || d.has(dflags::VIRTUAL) || d.has(dflags::PURE)) {
                    continue;
                }
                if !seen.insert(d.name.clone()) {
                    continue;
                }
                out.push(Overridable { method: m, owner: c, pure: d.has(dflags::PURE) || interface && !constructor });
            }
            queue.extend(self.direct_bases(c));
        }
        out
    }

    /// The class a class extends and the interface classes it implements.
    fn direct_bases(&self, class: DeclRef) -> Vec<DeclRef> {
        let mut out: Vec<DeclRef> = self.class_base(class).into_iter().collect();
        let d = self.decl(class);
        for t in &d.implements {
            if let Some(rid) = t.ref_id {
                let n = self.unit(class.unit).parsed.reference(rid).segs.len();
                if let Some(c) = self.resolve_ref(class.unit, rid, n - 1).and_then(|c| self.follow_typedef(c)) {
                    if matches!(self.decl(c).kind, DeclKind::Class | DeclKind::InterfaceClass) {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// The definition overriding `m`, its body indented by `indent` (one level), with `$0` (a
    /// snippet's cursor) in the body when `snippet`. `None` when its header cannot be read.
    pub fn override_text(&self, m: DeclRef, indent: &str, snippet: bool) -> Option<String> {
        let header = self.method_header(m)?;
        let d = self.decl(m);
        let task = d.kind == DeclKind::Task;
        let u = self.unit(m.unit);
        let args: Vec<String> = d.children.iter().map(|c| u.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Argument).map(|c| c.name.to_string()).collect();
        let call = format!("super.{}({})", d.name, args.join(", "));
        // A call of the base's version: always in a constructor; else when that version does
        // something (an empty one, or a pure one, has nothing to add).
        let returns = !task && d.name != "new" && !header.contains(" void ");
        let body = if d.name == "new" {
            Some(format!("{call};"))
        } else if self.has_body(m) {
            Some(if returns { format!("return {call};") } else { format!("{call};") })
        } else {
            None
        };
        let esc = |s: &str| if snippet { s.replace('\\', "\\\\").replace('$', "\\$").replace('}', "\\}") } else { s.to_string() };
        let mut text = esc(&header);
        text.push('\n');
        if let Some(b) = body {
            text.push_str(indent);
            text.push_str(&esc(&b));
            text.push('\n');
        }
        if snippet {
            text.push_str(indent);
            text.push_str("$0\n");
        }
        text.push_str(if task { "endtask" } else { "endfunction" });
        Some(text)
    }

    /// The header of a method as written (`virtual function void build_phase(uvm_phase phase);`),
    /// on one line, without `extern` or `pure`.
    fn method_header(&self, m: DeclRef) -> Option<String> {
        let d = self.decl(m);
        let text = self.file_text(d.span.file)?;
        let src = text.get(d.span.start as usize..d.span.end as usize)?;
        // Up to the `;` that ends it, outside parentheses.
        let toks = lex(src, d.span.file);
        let mut depth = 0i32;
        let semi = toks.iter().find(|t| {
            if t.is_punct("(") {
                depth += 1;
            } else if t.is_punct(")") {
                depth -= 1;
            }
            depth == 0 && t.is_punct(";")
        })?;
        let words: Vec<String> = toks
            .iter()
            .take_while(|t| t.span.start <= semi.span.start)
            .filter(|t| !(t.kind == TokKind::Keyword && matches!(t.text.as_str(), "extern" | "pure")))
            .map(|t| t.text.to_string())
            .collect();
        if !words.iter().any(|w| w == "function" || w == "task") {
            return None;
        }
        // Rejoined with single spaces, none inside brackets or before `,;)` and after `(`.
        let mut out = String::new();
        for (i, w) in words.iter().enumerate() {
            let prev = if i == 0 { "" } else { words[i - 1].as_str() };
            let tight = i == 0 || matches!(w.as_str(), "," | ";" | ")" | "]" | "::" | "." | "[") || matches!(prev, "(" | "[" | "::" | "." | "#") || w == "(" && !matches!(prev, "," | "=");
            if !tight {
                out.push(' ');
            }
            out.push_str(w);
        }
        Some(out)
    }

    /// Whether the definition of `m` (its body outside the class, for a prototype) does
    /// anything: more than an empty body or a bare `return;`.
    fn has_body(&self, m: DeclRef) -> bool {
        let d = self.decl(m);
        if d.has(dflags::PURE) {
            return false;
        }
        let def = if d.has(dflags::PROTOTYPE) { self.implementation_of(m) } else { Some(m) };
        let Some(def) = def else { return false };
        let dd = self.decl(def);
        let Some(text) = self.file_text(dd.span.file) else { return false };
        let Some(src) = text.get(dd.span.start as usize..dd.span.end as usize) else { return false };
        let toks = lex(src, dd.span.file);
        let mut depth = 0i32;
        let Some(semi) = toks.iter().position(|t| {
            if t.is_punct("(") {
                depth += 1;
            } else if t.is_punct(")") {
                depth -= 1;
            }
            depth == 0 && t.is_punct(";")
        }) else {
            return false;
        };
        toks[semi + 1..]
            .iter()
            .filter(|t| !(t.kind == TokKind::Keyword && matches!(t.text.as_str(), "endfunction" | "endtask" | "return")))
            .filter(|t| !t.is_punct(";") && !t.is_punct(":"))
            // `endfunction : name`
            .filter(|t| t.text != d.name)
            .count()
            > 0
    }

    /// "Implement the pure virtual methods" for the class around `off`, when it lacks some:
    /// their definitions inserted before its `endclass`.
    pub fn implement_pure_methods(&self, file: FileId, off: u32) -> Option<Refactor> {
        let &uid = self.units_for(file).first()?;
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
        // Before the line of `endclass`, indented like the class's first member (or one level
        // deeper than `endclass`).
        let src = text.get(cd.span.start as usize..cd.span.end as usize)?;
        let end_kw = lex(src, file).into_iter().rev().find(|t| t.is_kw("endclass"))?;
        let end_at = cd.span.start as usize + end_kw.span.start as usize;
        let line_start = text[..end_at].rfind('\n').map_or(0, |i| i + 1);
        let class_indent: String = text[line_start..end_at].chars().take_while(|c| c.is_whitespace()).collect();
        if class_indent.len() != end_at - line_start {
            return None;
        }
        let member_indent = cd
            .children
            .iter()
            .map(|c| self.unit(class.unit).parsed.decl(*c).span)
            .find(|s| s.file == file && s.start > cd.span.start)
            .and_then(|s| {
                let ls = text[..s.start as usize].rfind('\n').map_or(0, |i| i + 1);
                let ind: String = text[ls..s.start as usize].chars().take_while(|c| c.is_whitespace()).collect();
                (ind.len() == s.start as usize - ls && ind.len() > class_indent.len()).then_some(ind)
            })
            .unwrap_or_else(|| format!("{class_indent}  "));
        let step = member_indent[class_indent.len()..].to_string();
        let mut insert = String::new();
        for o in &missing {
            let t = self.override_text(o.method, &step, false)?;
            insert.push('\n');
            for line in t.lines() {
                insert.push_str(&member_indent);
                insert.push_str(line);
                insert.push('\n');
            }
        }
        let names: Vec<&str> = missing.iter().map(|o| self.decl(o.method).name.as_str()).collect();
        let title = if missing.len() == 1 { format!("Implement pure virtual method {}", names[0]) } else { format!("Implement {} pure virtual methods ({})", missing.len(), names.join(", ")) };
        Some(Refactor { title, edits: vec![(Span::new(file, line_start as u32, line_start as u32), insert)] })
    }
}
