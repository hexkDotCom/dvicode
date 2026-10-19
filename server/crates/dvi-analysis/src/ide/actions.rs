//! Quick fixes for problems: "Did you mean", "Import package" and "Declare variable".

use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, ParsedUnit, RefId, RefKind, Sep};
use dvi_syntax::{Diag, FileId, Span};
use rustc_hash::FxHashSet;
use smol_str::SmolStr;

/// A fix for one problem: text edits (an empty span inserts).
#[derive(Clone, Debug)]
pub struct QuickFix {
    pub title: String,
    pub diag: Diag,
    pub edits: Vec<(Span, String)>,
    pub preferred: bool,
}

const MAX_SUGGESTIONS: usize = 3;

impl Analysis {
    /// Quick fixes for the problems of `file` that overlap the byte range `start..=end`.
    pub fn quick_fixes(&self, file: FileId, start: u32, end: u32) -> Vec<QuickFix> {
        let mut out = Vec::new();
        for d in self.file_diagnostics(file) {
            if d.span.start > end || d.span.end < start {
                continue;
            }
            if !matches!(d.code, "UNDECLARED_IDENTIFIER" | "NON_EXISTING_TYPE") {
                continue;
            }
            let Some((unit, rid)) = self.ref_starting_at(file, d.span) else { continue };
            self.fixes_for_unknown_name(unit, rid, &d, &mut out);
        }
        out
    }

    /// The reference chain whose first segment is written at `span`, in the unit that
    /// reports the problems of `file`.
    fn ref_starting_at(&self, file: FileId, span: Span) -> Option<(UnitId, RefId)> {
        for uid in self.units_for(file) {
            let u = self.unit(uid);
            let Some(pos) = u.positions.get(&file) else { continue };
            let i = pos.refs.partition_point(|(s, _, _)| (s.start, s.end) < (span.start, span.end));
            for (s, rid, seg) in &pos.refs[i..] {
                if *s != span {
                    break;
                }
                if *seg == 0 {
                    return Some((uid, *rid));
                }
            }
        }
        None
    }

    fn fixes_for_unknown_name(&self, unit: UnitId, rid: RefId, d: &Diag, out: &mut Vec<QuickFix>) {
        let u = self.unit(unit);
        let chain = u.parsed.reference(rid);
        let seg = &chain.segs[0];
        let name = seg.name.as_str();
        let scoped = chain.segs.get(1).is_some_and(|s| s.sep == Sep::Scope);
        let want = if d.code == "NON_EXISTING_TYPE" || chain.kind == RefKind::Type {
            Want::Type
        } else if scoped {
            Want::Scope
        } else {
            Want::Value
        };
        let first = out.len();
        // Did you mean: visible names that are close to the unknown one.
        for c in self.similar_names(unit, chain.scope, name, want) {
            out.push(QuickFix { title: format!("Did you mean '{c}'?"), diag: d.clone(), edits: vec![(d.span, c)], preferred: false });
        }
        if out.len() == first + 1 {
            out[first].preferred = true;
        }
        // Import package: packages that declare the name.
        for pkg in self.packages_declaring(name, want) {
            if let Some(edit) = self.import_edit(unit, chain.scope, d.span, &pkg) {
                out.push(QuickFix { title: format!("Import package '{pkg}' ({pkg}::*)"), diag: d.clone(), edits: vec![edit], preferred: false });
            }
        }
        // Declare variable.
        if want == Want::Value && chain.segs.len() == 1 && !seg.called && chain.kind == RefKind::Expr {
            if let Some(edit) = self.declare_edit(unit, chain.scope, name) {
                out.push(QuickFix { title: format!("Declare variable '{name}'"), diag: d.clone(), edits: vec![edit], preferred: false });
            }
        }
    }

    /// Declarations visible from `scope` (nearest first) plus global names.
    fn visible_decls(&self, unit: UnitId, scope: DeclId) -> Vec<DeclRef> {
        let u = self.unit(unit);
        let mut out = Vec::new();
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            let sr = DeclRef { unit, decl: s };
            match d.kind {
                DeclKind::Class | DeclKind::InterfaceClass => out.extend(self.members_list(sr)),
                _ if d.has(dflags::OUT_OF_BLOCK) => {
                    out.extend(self.scope_members(sr));
                    if let Some(c) = self.owner_class(sr) {
                        out.extend(self.members_list(c));
                    }
                }
                _ => out.extend(self.scope_members(sr)),
            }
            for imp in &d.imports {
                if let Some(p) = self.global.packages.get(&imp.pkg).and_then(|v| v.first()) {
                    for m in self.scope_members(*p) {
                        if imp.item.as_ref().map_or(true, |i| *i == self.decl(m).name) {
                            out.push(m);
                        }
                    }
                }
            }
            cur = d.parent;
        }
        for v in self.global.unit_items.values().chain(self.global.packages.values()).chain(self.global.definitions.values()) {
            out.extend(v.first().copied());
        }
        if let Some(std) = self.global.std_pkg {
            out.extend(self.scope_members(std));
        }
        out
    }

    /// Up to three visible names of the wanted kind within a small edit distance of `name`.
    fn similar_names(&self, unit: UnitId, scope: DeclId, name: &str, want: Want) -> Vec<String> {
        let max = match name.len() {
            0..=2 => return Vec::new(),
            3..=5 => 1,
            6..=9 => 2,
            _ => 3,
        };
        let mut seen: FxHashSet<SmolStr> = FxHashSet::default();
        let mut cands: Vec<(usize, String)> = Vec::new();
        for r in self.visible_decls(unit, scope) {
            let d = self.decl(r);
            if d.name.is_empty() || d.name == name || d.has(dflags::OUT_OF_BLOCK) || !want.accepts(d.kind) || !seen.insert(d.name.clone()) {
                continue;
            }
            let dist = edit_distance(&name.to_ascii_lowercase(), &d.name.to_ascii_lowercase());
            if dist <= max && dist < d.name.len() {
                cands.push((dist, d.name.to_string()));
            }
        }
        cands.sort();
        cands.into_iter().take(MAX_SUGGESTIONS).map(|(_, n)| n).collect()
    }

    /// Packages (other than `std`) that declare `name` directly.
    fn packages_declaring(&self, name: &str, want: Want) -> Vec<String> {
        let mut out: Vec<String> = self
            .global
            .packages
            .iter()
            .filter_map(|(pname, v)| {
                let p = *v.first()?;
                if self.unit(p.unit).kind == UnitKind::Builtin {
                    return None;
                }
                let m = self.member_of(p, name)?;
                (want.accepts(self.decl(m).kind) && !self.decl(m).has(dflags::OUT_OF_BLOCK)).then(|| pname.to_string())
            })
            .collect();
        out.sort();
        out
    }

    /// `import pkg::*;` right after the header of the design element or package around
    /// `scope`, or before a compilation-unit level class/function.
    fn import_edit(&self, unit: UnitId, scope: DeclId, at: Span, pkg: &str) -> Option<(Span, String)> {
        let u = self.unit(unit);
        let mut top = scope;
        while let Some(p) = u.parsed.decl(top).parent {
            if p == ParsedUnit::ROOT {
                break;
            }
            top = p;
        }
        if top == ParsedUnit::ROOT {
            let ls = self.line_start(at.file, at.start)?;
            let (ind, nl) = (self.indent_at(at.file, ls), self.newline(at.file));
            return Some((Span::new(at.file, ls, ls), format!("{ind}import {pkg}::*;{nl}")));
        }
        let d = u.parsed.decl(top);
        match d.kind {
            DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Package | DeclKind::Checker => {
                let semi = self.header_end(DeclRef { unit, decl: top })?;
                let (ind, nl) = (self.indent_at(semi.file, self.line_start(semi.file, d.span.start)?), self.newline(semi.file));
                Some((Span::new(semi.file, semi.end, semi.end), format!("{nl}{ind}  import {pkg}::*;")))
            }
            _ => {
                let ls = self.line_start(d.span.file, d.span.start)?;
                let (ind, nl) = (self.indent_at(d.span.file, ls), self.newline(d.span.file));
                Some((Span::new(d.span.file, ls, ls), format!("{ind}import {pkg}::*;{nl}")))
            }
        }
    }

    /// A declaration of `name` at the top of the enclosing function, task, class or design
    /// element.
    fn declare_edit(&self, unit: UnitId, scope: DeclId, name: &str) -> Option<(Span, String)> {
        let u = self.unit(unit);
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            let ty = match d.kind {
                DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Checker => "logic",
                DeclKind::Function | DeclKind::Task | DeclKind::Class | DeclKind::Package => "int",
                _ => {
                    cur = d.parent;
                    continue;
                }
            };
            if d.has(dflags::FROM_MACRO) {
                return None;
            }
            let semi = self.header_end(DeclRef { unit, decl: s })?;
            let (ind, nl) = (self.indent_at(semi.file, self.line_start(semi.file, d.span.start)?), self.newline(semi.file));
            return Some((Span::new(semi.file, semi.end, semi.end), format!("{nl}{ind}  {ty} {name};")));
        }
        None
    }

    /// The `;` that ends the header of a declaration (`module m (...);`, `function f();`).
    fn header_end(&self, r: DeclRef) -> Option<Span> {
        let d = self.decl(r);
        let toks = self.file_tokens(d.span.file)?;
        let lo = toks.partition_point(|t| t.span.start < d.span.start);
        let mut depth = 0i32;
        for t in &toks[lo..] {
            if t.span.start > d.span.end {
                break;
            }
            if t.is_punct("(") || t.is_punct("[") || t.is_punct("{") {
                depth += 1;
            } else if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
                depth -= 1;
            } else if depth == 0 && t.is_punct(";") {
                return Some(t.span);
            }
        }
        None
    }

    fn line_start(&self, file: FileId, off: u32) -> Option<u32> {
        let text = self.file_text(file)?;
        let before = text.get(..off as usize)?;
        Some(before.rfind('\n').map_or(0, |i| i as u32 + 1))
    }

    /// The line ending the file uses, so that an inserted line does not mix CRLF and LF.
    fn newline(&self, file: FileId) -> &'static str {
        match self.file_text(file) {
            Some(t) if t.contains("\r\n") => "\r\n",
            _ => "\n",
        }
    }

    /// Leading whitespace of the line starting at `line_start`.
    fn indent_at(&self, file: FileId, line_start: u32) -> String {
        let Some(text) = self.file_text(file) else { return String::new() };
        text[line_start as usize..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    /// A data type.
    Type,
    /// The `X` of `X::y`: a class, typedef or package.
    Scope,
    /// A variable, parameter, function, ...
    Value,
}

impl Want {
    fn accepts(self, k: DeclKind) -> bool {
        match self {
            Want::Type => k.is_type() || k == DeclKind::Interface,
            Want::Scope => k.is_type() || k == DeclKind::Package,
            Want::Value => !(k.is_type() || k.is_definition() || matches!(k, DeclKind::Package | DeclKind::Unit | DeclKind::Generate | DeclKind::Block | DeclKind::Constraint)),
        }
    }
}

/// Optimal string alignment distance (Levenshtein with transpositions).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

#[cfg(test)]
mod tests {
    use super::edit_distance;

    #[test]
    fn distances() {
        assert_eq!(edit_distance("state_e", "stat_e"), 1);
        assert_eq!(edit_distance("addr", "adrd"), 1);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("", "ab"), 2);
    }
}
