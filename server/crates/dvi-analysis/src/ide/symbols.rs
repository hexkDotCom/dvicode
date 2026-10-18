//! Outline and workspace symbols.

use dvi_syntax::SourceProvider;
use crate::world::{Analysis, DeclRef, UnitKind};
use dvi_syntax::ast::{dflags, Decl, DeclId, DeclKind};
use dvi_syntax::FileId;
use lsp_types::{DocumentSymbol, Location, SymbolInformation, SymbolKind};

pub fn symbol_kind(d: &Decl, in_class: bool) -> SymbolKind {
    match d.kind {
        DeclKind::Unit => SymbolKind::FILE,
        DeclKind::Module | DeclKind::Program | DeclKind::Checker | DeclKind::Primitive | DeclKind::Config => SymbolKind::MODULE,
        DeclKind::Interface => SymbolKind::INTERFACE,
        DeclKind::Package => SymbolKind::PACKAGE,
        DeclKind::Class => SymbolKind::CLASS,
        DeclKind::InterfaceClass => SymbolKind::INTERFACE,
        DeclKind::Function | DeclKind::Task => {
            if d.name == "new" {
                SymbolKind::CONSTRUCTOR
            } else if in_class {
                SymbolKind::METHOD
            } else {
                SymbolKind::FUNCTION
            }
        }
        DeclKind::Variable if in_class => SymbolKind::FIELD,
        DeclKind::Variable | DeclKind::Net | DeclKind::Genvar | DeclKind::Let => SymbolKind::VARIABLE,
        DeclKind::Port | DeclKind::Argument => SymbolKind::PROPERTY,
        DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam => SymbolKind::CONSTANT,
        DeclKind::Typedef => match d.ty.as_ref().and_then(|t| t.text.split_whitespace().next()) {
            Some("enum") => SymbolKind::ENUM,
            Some("struct" | "union") => SymbolKind::STRUCT,
            _ => SymbolKind::TYPE_PARAMETER,
        },
        DeclKind::StructType => SymbolKind::STRUCT,
        DeclKind::EnumType => SymbolKind::ENUM,
        DeclKind::EnumMember => SymbolKind::ENUM_MEMBER,
        DeclKind::Field => SymbolKind::FIELD,
        DeclKind::Constraint => SymbolKind::OBJECT,
        DeclKind::Covergroup => SymbolKind::CLASS,
        DeclKind::Coverpoint | DeclKind::Cross => SymbolKind::FIELD,
        DeclKind::Instance => SymbolKind::OBJECT,
        DeclKind::Generate | DeclKind::Block => SymbolKind::NAMESPACE,
        DeclKind::Modport | DeclKind::ClockingBlock => SymbolKind::INTERFACE,
        DeclKind::Property | DeclKind::Sequence => SymbolKind::EVENT,
    }
}

fn outline_visible(d: &Decl) -> bool {
    !d.name.is_empty()
        && !d.has(dflags::FROM_MACRO)
        && !matches!(d.kind, DeclKind::Block | DeclKind::Argument | DeclKind::StructType | DeclKind::EnumType)
}

/// Children shown under a declaration in the outline.
fn outline_children(d: &Decl) -> bool {
    !matches!(d.kind, DeclKind::Function | DeclKind::Task | DeclKind::Constraint | DeclKind::Property | DeclKind::Sequence)
}

impl Analysis {
    pub fn document_symbols(&self, file: FileId) -> Vec<DocumentSymbol> {
        let Some(&uid) = self.units_for(file).first() else { return Vec::new() };
        let u = self.unit(uid);
        let Some(tops) = u.parsed.file_tops.get(&file) else { return Vec::new() };
        let mut out = Vec::new();
        for &t in tops {
            self.outline_node(uid, t, file, &mut out, 0);
        }
        // Macros defined in this file.
        for m in &u.macro_defs {
            if m.name_span.file == file {
                if let Some(r) = self.to_range(m.name_span) {
                    #[allow(deprecated)]
                    out.push(DocumentSymbol {
                        name: format!("`{}", m.name),
                        detail: m.params.as_ref().map(|_| "macro".to_string()),
                        kind: SymbolKind::CONSTANT,
                        tags: None,
                        deprecated: None,
                        range: r,
                        selection_range: r,
                        children: None,
                    });
                }
            }
        }
        out.sort_by_key(|s| (s.range.start.line, s.range.start.character));
        out
    }

    fn outline_node(&self, uid: u32, id: DeclId, file: FileId, out: &mut Vec<DocumentSymbol>, depth: usize) {
        let u = self.unit(uid);
        let d = u.parsed.decl(id);
        if depth > 32 {
            return;
        }
        // Anonymous containers: show their contents in place.
        if !outline_visible(d) {
            if matches!(d.kind, DeclKind::StructType | DeclKind::EnumType) || (d.kind == DeclKind::Block && depth == 0) {
                for &c in &d.children {
                    self.outline_node(uid, c, file, out, depth + 1);
                }
            }
            return;
        }
        if d.name_span.file != file {
            return;
        }
        let in_class = d.parent.is_some_and(|p| matches!(u.parsed.decl(p).kind, DeclKind::Class | DeclKind::InterfaceClass));
        let mut children = Vec::new();
        if outline_children(d) {
            for &c in &d.children {
                self.outline_node(uid, c, file, &mut children, depth + 1);
            }
        }
        let Some(sel) = self.to_range(d.name_span) else { return };
        let full = if d.span.file == file { self.to_range(d.span).unwrap_or(sel) } else { sel };
        let full = if full.start <= sel.start && full.end >= sel.end { full } else { sel };
        let detail = self.outline_detail(d);
        #[allow(deprecated)]
        out.push(DocumentSymbol {
            name: d.name.to_string(),
            detail,
            kind: symbol_kind(d, in_class),
            tags: None,
            deprecated: None,
            range: full,
            selection_range: sel,
            children: if children.is_empty() { None } else { Some(children) },
        });
    }

    fn outline_detail(&self, d: &Decl) -> Option<String> {
        let s = match d.kind {
            DeclKind::Function | DeclKind::Task => {
                let ret = d.ty.as_ref().map(|t| t.text.to_string()).unwrap_or_default();
                if ret.is_empty() { None } else { Some(ret) }
            }
            DeclKind::Variable | DeclKind::Net | DeclKind::Port | DeclKind::Field | DeclKind::Instance | DeclKind::Parameter | DeclKind::LocalParam => {
                let mut s = String::new();
                if let Some(dir) = d.dir {
                    s.push_str(dir.as_str());
                    s.push(' ');
                }
                if let Some(t) = &d.ty {
                    s.push_str(&t.text);
                }
                (!s.trim().is_empty()).then(|| s.trim().to_string())
            }
            DeclKind::Class => d.extends.as_ref().map(|e| format!("extends {}", e.text)),
            DeclKind::EnumMember | DeclKind::Typedef => (!d.detail.is_empty()).then(|| d.detail.to_string()),
            _ => None,
        };
        s.map(|s| if s.len() > 80 { format!("{}...", &s[..s.char_indices().nth(77).map(|c| c.0).unwrap_or(s.len())]) } else { s })
    }

    /// Workspace symbols. The query may be `kind:pattern` (class:, module:, package:, macro:,
    /// interface:, function:, task:, typedef:, file:, extends:).
    pub fn workspace_symbols(&self, query: &str, limit: usize) -> Vec<SymbolInformation> {
        let (filter, pat) = match query.split_once(':') {
            Some((k, p)) if !k.contains(' ') && k.chars().all(|c| c.is_ascii_alphabetic() || c == '_') => (Some(k.to_ascii_lowercase()), p.trim()),
            _ => (None, query.trim()),
        };
        let pat_l = pat.to_ascii_lowercase();
        let mut out = Vec::new();
        let mut seen = rustc_hash::FxHashSet::default();
        let want = |k: &str| filter.as_deref().map_or(true, |f| f == k);
        let extends_filter = filter.as_deref() == Some("extends");
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
            }
            for (i, d) in u.parsed.decls.iter().enumerate() {
                if d.name.is_empty() || d.has(dflags::FROM_MACRO) || d.has(dflags::OUT_OF_BLOCK) {
                    continue;
                }
                let kind_name = match d.kind {
                    DeclKind::Class => "class",
                    DeclKind::InterfaceClass => "interface",
                    DeclKind::Module => "module",
                    DeclKind::Interface => "interface",
                    DeclKind::Program => "program",
                    DeclKind::Package => "package",
                    DeclKind::Checker => "checker",
                    DeclKind::Typedef => "typedef",
                    DeclKind::Covergroup => "covergroup",
                    DeclKind::Function | DeclKind::Task => {
                        // Only package/unit/class level subroutines.
                        let pk = d.parent.map(|p| u.parsed.decl(p).kind);
                        if !matches!(pk, Some(DeclKind::Package | DeclKind::Unit | DeclKind::Class | DeclKind::Module | DeclKind::Interface)) {
                            continue;
                        }
                        if d.kind == DeclKind::Function { "function" } else { "task" }
                    }
                    _ => continue,
                };
                if extends_filter {
                    let ok = matches!(d.kind, DeclKind::Class) && d.extends.as_ref().is_some_and(|e| fuzzy(&e.text.to_ascii_lowercase(), &pat_l));
                    if !ok {
                        continue;
                    }
                } else if !want(kind_name) && !(filter.as_deref() == Some("struct") && kind_name == "typedef") && !(filter.as_deref() == Some("enum") && kind_name == "typedef") {
                    continue;
                } else if !fuzzy(&d.name.to_ascii_lowercase(), &pat_l) {
                    continue;
                }
                let r = DeclRef { unit: u.id, decl: i as u32 };
                let key = self.decl_key(r);
                if !seen.insert(key) {
                    continue;
                }
                let Some(loc) = self.to_location(d.name_span) else { continue };
                let container = d.parent.and_then(|p| {
                    let pd = u.parsed.decl(p);
                    (pd.kind != DeclKind::Unit && !pd.name.is_empty()).then(|| pd.name.to_string())
                });
                let in_class = d.parent.is_some_and(|p| u.parsed.decl(p).kind == DeclKind::Class);
                #[allow(deprecated)]
                out.push(SymbolInformation { name: d.name.to_string(), kind: symbol_kind(d, in_class), tags: None, deprecated: None, location: loc, container_name: container });
                if out.len() >= limit {
                    return out;
                }
            }
        }
        if filter.is_none() || want("macro") {
            let mut names: Vec<_> = self.global.macros.iter().collect();
            names.sort_by(|a, b| a.0.cmp(b.0));
            for (n, defs) in names {
                if !fuzzy(&n.to_ascii_lowercase(), &pat_l) {
                    continue;
                }
                if let Some(def) = defs.first() {
                    if let Some(loc) = self.to_location(def.name_span) {
                        #[allow(deprecated)]
                        out.push(SymbolInformation { name: format!("`{n}"), kind: SymbolKind::CONSTANT, tags: None, deprecated: None, location: loc, container_name: Some("macro".into()) });
                    }
                }
                if out.len() >= limit {
                    return out;
                }
            }
        }
        if want("file") && filter.is_some() {
            for f in self.project_files() {
                let p = self.vfs.path(f);
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if fuzzy(&name.to_ascii_lowercase(), &pat_l) {
                    let loc = Location::new(super::url_of(&p), Default::default());
                    #[allow(deprecated)]
                    out.push(SymbolInformation { name, kind: SymbolKind::FILE, tags: None, deprecated: None, location: loc, container_name: None });
                }
            }
        }
        out
    }
}

/// Case-insensitive subsequence match (both arguments already lowercase).
pub fn fuzzy(s: &str, pat: &str) -> bool {
    if pat.is_empty() {
        return true;
    }
    let mut it = s.chars();
    pat.chars().all(|p| it.any(|c| c == p))
}
