//! Navigation: definition, implementation, references, highlights, rename, type hierarchy.

use dvi_syntax::SourceProvider;
use super::{Hit, Target};
use crate::world::{Analysis, DeclRef, UnitKind};
use dvi_syntax::ast::{dflags, DeclId, DeclKind};
use dvi_syntax::{FileId, Span};
use lsp_types::{Location, SymbolKind, TypeHierarchyItem, WorkspaceEdit};
use rustc_hash::{FxHashMap, FxHashSet};

impl Analysis {
    pub fn goto_definition(&self, file: FileId, off: u32) -> Vec<Location> {
        let Some(t) = self.target_at(file, off) else { return Vec::new() };
        match t {
            Target::Decl(r) => {
                let mut v: Vec<Location> = self.decl_location(r).into_iter().collect();
                // From a prototype, also offer the body.
                if let Some(i) = self.implementation_of(r) {
                    v.extend(self.decl_location(i));
                }
                v
            }
            Target::Macro(m) => self.to_location(m.name_span).into_iter().collect(),
            Target::File(f) => vec![Location::new(super::url_of(&self.vfs.path(f)), Default::default())],
        }
    }

    pub fn goto_type_definition(&self, file: FileId, off: u32) -> Vec<Location> {
        let Some(Target::Decl(r)) = self.target_at(file, off) else { return Vec::new() };
        let ty = match self.decl(r).kind {
            DeclKind::Typedef => self.follow_typedef(r),
            _ => self.type_of(r),
        };
        let Some(ty) = ty else { return Vec::new() };
        // Inline struct/enum bodies: go to the typedef that names them, if any.
        let ty = match self.decl(ty).kind {
            DeclKind::StructType | DeclKind::EnumType => {
                let p = self.decl(ty).parent.map(|p| DeclRef { unit: ty.unit, decl: p });
                p.filter(|p| self.decl(*p).kind == DeclKind::Typedef).unwrap_or(ty)
            }
            _ => ty,
        };
        self.decl_location(ty).into_iter().collect()
    }

    /// Bodies of extern methods and overrides of virtual methods; subclasses of a class.
    pub fn goto_implementation(&self, file: FileId, off: u32) -> Vec<Location> {
        let Some(Target::Decl(r)) = self.target_at(file, off) else { return Vec::new() };
        let r = self.canonical(r);
        let d = self.decl(r);
        let mut out = Vec::new();
        match d.kind {
            DeclKind::Function | DeclKind::Task | DeclKind::Constraint => {
                if let Some(i) = self.implementation_of(r) {
                    out.extend(self.decl_location(i));
                }
                if let Some(class) = d.parent.map(|p| DeclRef { unit: r.unit, decl: p }).filter(|c| matches!(self.decl(*c).kind, DeclKind::Class | DeclKind::InterfaceClass)) {
                    for sub in self.all_subclasses(class) {
                        let u = self.unit(sub.unit);
                        for m in u.parsed.lookup_member(sub.decl, &d.name) {
                            let mr = DeclRef { unit: sub.unit, decl: *m };
                            out.extend(self.decl_location(mr));
                            if let Some(i) = self.implementation_of(mr) {
                                out.extend(self.decl_location(i));
                            }
                        }
                    }
                }
            }
            DeclKind::Class | DeclKind::InterfaceClass => {
                for sub in self.all_subclasses(r) {
                    out.extend(self.decl_location(sub));
                }
            }
            _ => {}
        }
        out
    }

    /// Direct subclasses (and implementors) of a class.
    pub fn direct_subclasses(&self, class: DeclRef) -> Vec<DeclRef> {
        let name = &self.decl(class).name;
        let key = self.decl_key(class);
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        if let Some(cands) = self.global.subclasses.get(name) {
            for c in cands {
                let bases = self.class_base(*c).into_iter().chain(self.class_interfaces(*c));
                if bases.into_iter().any(|b| self.decl_key(b) == key) && seen.insert(self.decl_key(*c)) {
                    out.push(*c);
                }
            }
        }
        out
    }

    pub fn all_subclasses(&self, class: DeclRef) -> Vec<DeclRef> {
        let mut out = Vec::new();
        let mut stack = vec![class];
        let mut seen = FxHashSet::default();
        while let Some(c) = stack.pop() {
            for s in self.direct_subclasses(c) {
                if seen.insert(self.decl_key(s)) && out.len() < 5000 {
                    out.push(s);
                    stack.push(s);
                }
            }
        }
        out
    }

    /// Every location referring to the declaration at the cursor.
    pub fn references(&self, file: FileId, off: u32, include_decl: bool) -> Vec<Location> {
        self.reference_spans(file, off, include_decl).into_iter().filter_map(|s| self.to_location(s)).collect()
    }

    pub fn reference_spans(&self, file: FileId, off: u32, include_decl: bool) -> Vec<Span> {
        let Some(hit) = self.hit_at(file, off) else { return Vec::new() };
        match self.target_of(&hit) {
            Some(Target::Decl(r)) => self.decl_references(r, include_decl),
            Some(Target::Macro(m)) => {
                let mut out = Vec::new();
                if include_decl {
                    out.push(m.name_span);
                }
                let mut seen = FxHashSet::default();
                for u in self.units.iter().flatten() {
                    for mu in &u.macro_uses {
                        if mu.name == m.name && mu.def.as_ref().is_some_and(|d| d.name_span == m.name_span) && seen.insert(mu.name_span) {
                            out.push(macro_name(mu.name_span));
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    pub fn decl_references(&self, r: DeclRef, include_decl: bool) -> Vec<Span> {
        let canon = self.canonical(r);
        let key = self.decl_key(canon);
        let name = self.decl(r).name.clone();
        let mut out: Vec<Span> = Vec::new();
        let mut seen: FxHashSet<Span> = FxHashSet::default();
        if include_decl {
            for s in [self.decl(canon).name_span].into_iter().chain(self.implementation_of(canon).map(|i| self.decl(i).name_span)) {
                if seen.insert(s) {
                    out.push(s);
                }
            }
            // Non-ANSI port declarations, out-of-block bodies found through other units.
        }
        let mut visited_files: FxHashMap<FileId, u32> = FxHashMap::default();
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
            }
            if let Some(occ) = u.names.get(&name) {
                for (rid, seg) in occ {
                    let s = &u.parsed.reference(*rid).segs[*seg as usize];
                    if s.from_macro || seen.contains(&s.span) {
                        continue;
                    }
                    // A file compiled in several units is searched once per unit; that is fine
                    // because spans are de-duplicated.
                    if let Some(t) = self.resolve_ref(u.id, *rid, *seg as usize) {
                        if self.decl_key(self.canonical(t)) == key {
                            seen.insert(s.span);
                            out.push(s.span);
                        }
                    }
                }
            }
            if include_decl {
                // Out-of-block definitions in this unit.
                for (i, d) in u.parsed.decls.iter().enumerate() {
                    if d.name == name && d.has(dflags::OUT_OF_BLOCK) {
                        let dr = DeclRef { unit: u.id, decl: i as DeclId };
                        if self.decl_key(self.canonical(dr)) == key && seen.insert(d.name_span) {
                            out.push(d.name_span);
                        }
                    }
                }
            }
            *visited_files.entry(u.root).or_default() += 1;
        }
        out
    }

    pub fn document_highlights(&self, file: FileId, off: u32) -> Vec<Span> {
        let Some(hit) = self.hit_at(file, off) else { return Vec::new() };
        match self.target_of(&hit) {
            Some(Target::Decl(r)) => {
                // Only this file's units.
                let canon = self.canonical(r);
                let key = self.decl_key(canon);
                let name = self.decl(r).name.clone();
                let mut out = Vec::new();
                let mut seen = FxHashSet::default();
                let d = self.decl(canon);
                if d.name_span.file == file && seen.insert(d.name_span) {
                    out.push(d.name_span);
                }
                for uid in self.units_for(file) {
                    let u = self.unit(uid);
                    if let Some(pos) = u.positions.get(&file) {
                        for (span, rid, seg) in &pos.refs {
                            if u.parsed.reference(*rid).segs[*seg as usize].name != name || seen.contains(span) {
                                continue;
                            }
                            if let Some(t) = self.resolve_ref(uid, *rid, *seg as usize) {
                                if self.decl_key(self.canonical(t)) == key {
                                    seen.insert(*span);
                                    out.push(*span);
                                }
                            }
                        }
                        for did in &pos.decls {
                            let x = u.parsed.decl(*did);
                            if x.name == name && x.has(dflags::OUT_OF_BLOCK) && seen.insert(x.name_span) {
                                let dr = DeclRef { unit: uid, decl: *did };
                                if self.decl_key(self.canonical(dr)) == key {
                                    out.push(x.name_span);
                                }
                            }
                        }
                    }
                }
                out
            }
            Some(Target::Macro(_)) => self.reference_spans(file, off, true).into_iter().filter(|s| s.file == file).collect(),
            _ => vec![hit.span()],
        }
    }

    /// Span of the renamable name at the cursor.
    pub fn prepare_rename(&self, file: FileId, off: u32) -> Option<Span> {
        let hit = self.hit_at(file, off)?;
        match (&hit, self.target_of(&hit)?) {
            (Hit::Include { .. }, _) => None,
            (_, Target::Decl(r)) => {
                let d = self.decl(r);
                if self.vfs.is_builtin(d.name_span.file) || d.has(dflags::FROM_MACRO) {
                    return None;
                }
                Some(hit.span())
            }
            (_, Target::Macro(m)) => (m.name_span.file != dvi_syntax::preproc::COMMAND_LINE_FILE).then(|| match hit {
                Hit::MacroUse { span, .. } => macro_name(span),
                _ => hit.span(),
            }),
            _ => None,
        }
    }

    pub fn rename(&self, file: FileId, off: u32, new_name: &str) -> Result<WorkspaceEdit, String> {
        if !is_identifier(new_name) {
            return Err(format!("'{new_name}' is not a valid SystemVerilog identifier"));
        }
        if self.prepare_rename(file, off).is_none() {
            return Err("This element cannot be renamed".into());
        }
        let spans = self.reference_spans(file, off, true);
        let mut seen = FxHashSet::default();
        let edits: Vec<(Span, String)> = spans
            .into_iter()
            .filter(|s| seen.insert(*s) && !self.vfs.is_builtin(s.file))
            .map(|s| (s, new_name.to_string()))
            .collect();
        self.versioned_edit(edits).ok_or_else(|| "The files to edit are not available".to_string())
    }

    // ------------------------------------------------------------ type hierarchy

    pub fn type_hierarchy_item(&self, r: DeclRef) -> Option<TypeHierarchyItem> {
        let d = self.decl(r);
        let loc = self.decl_location(r)?;
        let full = if d.span.file == d.name_span.file { self.to_range(d.span).unwrap_or(loc.range) } else { loc.range };
        Some(TypeHierarchyItem {
            name: d.name.to_string(),
            kind: if d.kind == DeclKind::InterfaceClass { SymbolKind::INTERFACE } else { SymbolKind::CLASS },
            tags: None,
            detail: Some(self.qualified_name(r)),
            uri: loc.uri,
            range: full,
            selection_range: loc.range,
            data: Some(serde_json_value(r)),
        })
    }

    pub fn prepare_type_hierarchy(&self, file: FileId, off: u32) -> Vec<TypeHierarchyItem> {
        let Some(Target::Decl(r)) = self.target_at(file, off) else { return Vec::new() };
        let r = match self.decl(r).kind {
            DeclKind::Class | DeclKind::InterfaceClass | DeclKind::Typedef => self.follow_typedef(r),
            _ => self.type_of(r),
        };
        let Some(r) = r else { return Vec::new() };
        if !matches!(self.decl(r).kind, DeclKind::Class | DeclKind::InterfaceClass) {
            return Vec::new();
        }
        self.type_hierarchy_item(r).into_iter().collect()
    }

    pub fn class_at_location(&self, file: FileId, off: u32) -> Option<DeclRef> {
        let t = self.target_at(file, off)?;
        match t {
            Target::Decl(r) if matches!(self.decl(r).kind, DeclKind::Class | DeclKind::InterfaceClass) => Some(r),
            _ => None,
        }
    }

    pub fn supertypes(&self, r: DeclRef) -> Vec<TypeHierarchyItem> {
        self.class_base(r)
            .into_iter()
            .chain(self.class_interfaces(r))
            .filter_map(|b| self.type_hierarchy_item(b))
            .collect()
    }

    pub fn subtypes(&self, r: DeclRef) -> Vec<TypeHierarchyItem> {
        self.direct_subclasses(r).into_iter().filter_map(|s| self.type_hierarchy_item(s)).collect()
    }
}

fn serde_json_value(r: DeclRef) -> lsp_types::LSPAny {
    lsp_types::LSPAny::Array(vec![lsp_types::LSPAny::from(r.unit), lsp_types::LSPAny::from(r.decl)])
}

/// The name part of a macro use span, which starts with the backtick: `` `NAME `` -> `NAME`.
fn macro_name(use_span: Span) -> Span {
    Span::new(use_span.file, use_span.start + 1, use_span.end.max(use_span.start + 1))
}

pub fn is_identifier(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$')
        && !dvi_syntax::token::is_keyword(s)
}
