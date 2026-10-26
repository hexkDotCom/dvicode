//! Navigation: definition, implementation, references, highlights, rename, type hierarchy.

use dvi_syntax::SourceProvider;
use super::{Hit, Target};
use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, RefKind};
use dvi_syntax::{FileId, Span, TokKind, Token};
use lsp_types::{Location, SymbolKind, TypeHierarchyItem, WorkspaceEdit};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

impl Analysis {
    pub fn goto_definition(&self, file: FileId, off: u32) -> Vec<Location> {
        // A `uvm_config_db` get's field string: the sets that can provide it.
        if let Some(v) = self.config_definition(file, off) {
            return v;
        }
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
        // A `uvm_config_db` field string: every get and set of that field.
        if let Some(v) = self.config_references(file, off) {
            return v;
        }
        self.with_lookup_memo(|| self.references_inner(file, off, include_decl))
    }

    fn references_inner(&self, file: FileId, off: u32, include_decl: bool) -> Vec<Location> {
        self.reference_spans(file, off, include_decl).into_iter().filter_map(|s| self.to_location(s)).collect()
    }

    pub fn reference_spans(&self, file: FileId, off: u32, include_decl: bool) -> Vec<Span> {
        let Some(hit) = self.hit_at(file, off) else { return Vec::new() };
        match self.target_of(&hit) {
            Some(Target::Decl(r)) => self.decl_references(r, include_decl),
            Some(Target::Macro(m)) => self.macro_spans(&m, include_decl, None),
            _ => Vec::new(),
        }
    }

    /// Where the macro `m` is used, defined and tested (`ifdef`): in the whole project, or in
    /// `only` that file (highlights: its own units, not every unit's uses).
    fn macro_spans(&self, m: &dvi_syntax::preproc::MacroDef, include_decl: bool, only: Option<FileId>) -> Vec<Span> {
        let here = |s: &Span| only.is_none_or(|f| s.file == f);
        let units: Vec<UnitId> = match only {
            Some(f) => self.editor_units(f),
            None => self.units.iter().flatten().map(|u| u.id).collect(),
        };
        let mut out = Vec::new();
        if include_decl && here(&m.name_span) {
            out.push(m.name_span);
        }
        let mut seen = FxHashSet::default();
        for &uid in &units {
            if self.cancelled() {
                break;
            }
            for mu in &self.unit(uid).macro_uses {
                if here(&mu.name_span) && mu.name == m.name && mu.def.as_ref().is_some_and(|d| d.name_span == m.name_span) && seen.insert(mu.name_span) {
                    out.push(macro_name(mu.name_span));
                }
            }
        }
        out.extend(self.macro_uses_in_bodies(&m.name).into_iter().filter(|s| here(s) && seen.insert(*s)).map(macro_name));
        // `ifdef NAME and friends; where it is not defined yet (`ifndef GUARD), when it
        // has no other definition.
        let unique = self.unique_macro(&m.name).is_some_and(|d| d.name_span == m.name_span);
        for &uid in &units {
            if self.cancelled() {
                break;
            }
            for mn in &self.unit(uid).macro_names {
                if here(&mn.span) && mn.name == m.name && mn.def.as_ref().map_or(unique, |d| d.name_span == m.name_span) && seen.insert(mn.span) {
                    out.push(mn.span);
                }
            }
        }
        out
    }

    /// `` `NAME `` in the bodies and default arguments of macro definitions: they refer to
    /// the macro by name wherever they are expanded.
    pub fn macro_uses_in_bodies(&self, name: &str) -> Vec<Span> {
        let mut out = Vec::new();
        let mut defs = FxHashSet::default();
        for d in self.global.macros.values().flatten() {
            if !defs.insert(d.name_span) {
                continue;
            }
            let defaults = d.params.iter().flatten().filter_map(|p| p.default.as_ref()).flatten();
            for t in d.body.iter().chain(defaults) {
                if t.kind == TokKind::Directive && t.text == name && t.span.file == d.name_span.file && t.is_real() {
                    out.push(t.span);
                }
            }
        }
        out.sort_by_key(|s| (s.file, s.start));
        out.dedup();
        out
    }

    /// An instance whose `.*` connects `r`, which is a port of the module instantiated or a
    /// signal that a port of the same name takes there: where the instance is. Its connections
    /// name nothing, so rename would leave them behind.
    fn dot_star_connection(&self, r: DeclRef) -> Option<Span> {
        let d = self.decl(r);
        if !matches!(d.kind, DeclKind::Port | DeclKind::Variable | DeclKind::Net) {
            return None;
        }
        let owner = d.parent.map(|p| DeclRef { unit: r.unit, decl: p }).filter(|o| d.kind == DeclKind::Port && self.decl(*o).kind.is_definition());
        let owner = owner.map(|o| self.decl_key(o));
        for u in self.units.iter().flatten() {
            if self.cancelled() {
                break;
            }
            for (i, x) in u.parsed.decls.iter().enumerate() {
                if x.kind != DeclKind::Instance || !x.has(dflags::DOT_STAR) {
                    continue;
                }
                let Some(m) = self.type_of(DeclRef { unit: u.id, decl: i as DeclId }) else { continue };
                if owner.as_ref().is_some_and(|o| *o == self.decl_key(m)) {
                    return Some(x.name_span);
                }
                let takes = || self.member_of(m, &d.name).is_some_and(|p| self.decl(p).kind == DeclKind::Port);
                if x.parent.is_some_and(|p| self.lookup(u.id, p, &d.name) == Some(r)) && takes() {
                    return Some(x.name_span);
                }
            }
        }
        None
    }

    /// A use of `r` inside a macro body: the name of the macro and where it is expanded.
    /// Rename cannot change it there, and changing the macro could break its other uses.
    fn use_in_macro_body(&self, r: DeclRef) -> Option<(String, Span)> {
        let key = self.decl_key(self.canonical(r));
        let name = &self.decl(r).name;
        for u in self.units.iter().flatten() {
            if self.cancelled() {
                break;
            }
            let Some(occ) = u.names.get(name) else { continue };
            for (rid, seg) in occ {
                let s = &u.parsed.reference(*rid).segs[*seg as usize];
                if !s.from_macro || !self.resolve_ref(u.id, *rid, *seg as usize).is_some_and(|t| self.decl_key(self.canonical(t)) == key) {
                    continue;
                }
                let m = u.macro_uses.iter().find(|m| m.call_span.file == s.span.file && m.call_span.start <= s.span.start && s.span.start < m.call_span.end.max(m.call_span.start + 1));
                return Some((m.map_or_else(|| "?".to_string(), |m| m.name.to_string()), s.span));
            }
        }
        None
    }

    /// The text that replaces the name at `span` in a rename. An implicit `.name` port
    /// connection stands for both the port and the local signal of that name, so it becomes
    /// an explicit connection: `.new(name)` when the port is renamed, `.name(new)` when the
    /// signal is.
    fn rename_text(&self, span: Span, target: Option<(u32, u32)>, new_name: &str, tokens: &mut FxHashMap<FileId, Option<Arc<Vec<Token>>>>) -> String {
        let Some(key) = target else { return new_name.to_string() };
        // The port references written at this place, and whether one of them is the target.
        let mut port = None;
        for uid in self.units_for(span.file) {
            let u = self.unit(uid);
            let Some(pos) = u.positions.get(&span.file) else { continue };
            let lo = pos.refs.partition_point(|(s, _, _)| (s.start, s.end) < (span.start, span.end));
            for (_, rid, seg) in pos.refs[lo..].iter().take_while(|(s, _, _)| *s == span) {
                if matches!(u.parsed.reference(*rid).kind, RefKind::Port { .. }) {
                    let is_target = self.resolve_ref(uid, *rid, *seg as usize).is_some_and(|t| self.decl_key(self.canonical(t)) == key);
                    port = Some(port.unwrap_or(false) || is_target);
                }
            }
        }
        let Some(port_side) = port else { return new_name.to_string() };
        // `.name(...)` is explicit already. The file is lexed once for all its connections.
        let Some(toks) = tokens.entry(span.file).or_insert_with(|| self.file_tokens(span.file)).clone() else { return new_name.to_string() };
        let i = toks.partition_point(|t| t.span.start < span.start);
        if toks.get(i).is_some_and(|t| t.span != span) || toks.get(i + 1).is_some_and(|t| t.is_punct("(")) {
            return new_name.to_string();
        }
        let mut old = self.file_text(span.file).map(|t| t[span.start as usize..span.end as usize].to_string()).unwrap_or_default();
        // An escaped name ends at white space: before the `)` or `(` that follows it.
        if old.starts_with('\\') {
            old.push(' ');
        }
        if port_side {
            format!("{new_name}({old})")
        } else {
            format!("{old}({new_name})")
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
            if self.cancelled() {
                break;
            }
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
        // `f(.value(1))` names the argument `value` of `f`; the parser keeps no reference to it.
        if self.decl(canon).kind == DeclKind::Argument {
            let sub = self.decl(canon).parent.map(|p| DeclRef { unit: canon.unit, decl: p });
            if let Some(sub) = sub.filter(|s| matches!(self.decl(*s).kind, DeclKind::Function | DeclKind::Task)) {
                let mut tokens: FxHashMap<FileId, Option<Arc<Vec<Token>>>> = FxHashMap::default();
                for call in self.decl_references(sub, false) {
                    let Some(toks) = tokens.entry(call.file).or_insert_with(|| self.file_tokens(call.file)).clone() else { continue };
                    for s in named_arguments(&toks, call, &name) {
                        if seen.insert(s) {
                            out.push(s);
                        }
                    }
                }
            }
        }
        out
    }

    pub fn document_highlights(&self, file: FileId, off: u32) -> Vec<Span> {
        self.with_lookup_memo(|| self.document_highlights_inner(file, off))
    }

    fn document_highlights_inner(&self, file: FileId, off: u32) -> Vec<Span> {
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
                for uid in self.editor_units(file) {
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
            Some(Target::Macro(m)) => self.macro_spans(&m, true, Some(file)),
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
                if !self.is_editable(d.name_span.file) || d.has(dflags::FROM_MACRO) {
                    return None;
                }
                Some(hit.span())
            }
            (_, Target::Macro(m)) => self.is_editable(m.name_span.file).then(|| match hit {
                Hit::MacroUse { span, .. } => macro_name(span),
                _ => hit.span(),
            }),
            _ => None,
        }
    }

    pub fn rename(&self, file: FileId, off: u32, new_name: &str) -> Result<WorkspaceEdit, String> {
        self.with_lookup_memo(|| self.rename_inner(file, off, new_name))
    }

    fn rename_inner(&self, file: FileId, off: u32, new_name: &str) -> Result<WorkspaceEdit, String> {
        if !is_identifier(new_name) {
            return Err(format!("'{new_name}' is not a valid SystemVerilog identifier"));
        }
        if self.prepare_rename(file, off).is_none() {
            return Err("This element cannot be renamed".into());
        }
        let line = |s: Span| format!("{}:{}", self.vfs.path(s.file).display(), self.to_range(s).map_or(0, |r| r.start.line + 1));
        let target = match self.target_at(file, off) {
            Some(Target::Decl(r)) => {
                if let Some((m, at)) = self.use_in_macro_body(r) {
                    return Err(format!("'{}' is also used inside the macro `{m} (expanded at {}), which rename cannot change", self.decl(r).name, line(at)));
                }
                if let Some(at) = self.dot_star_connection(r) {
                    return Err(format!("'{}' is also connected by the `.*` of the instance at {}, which rename cannot change", self.decl(r).name, line(at)));
                }
                Some(self.decl_key(self.canonical(r)))
            }
            Some(Target::Macro(m)) => {
                if self.units.iter().flatten().any(|u| u.pasted_macros.contains(&m.name)) {
                    return Err(format!("`{} is also called through a name that a macro builds with `` (token pasting), which rename cannot change", m.name));
                }
                // Uses inside other macros name it without saying which definition they mean.
                let other = self.global.macros.get(&m.name).into_iter().flatten().find(|d| d.name_span != m.name_span);
                if let (Some(o), false) = (other, self.macro_uses_in_bodies(&m.name).is_empty()) {
                    return Err(format!("`{} is defined in more than one place ({} and {}) and used inside other macros, which could mean either", m.name, line(m.name_span), line(o.name_span)));
                }
                None
            }
            _ => None,
        };
        let spans = self.reference_spans(file, off, true);
        // A use in a library outside the workspace would be left behind, or the library
        // changed: neither is what the user asked for.
        let mut checked: FxHashMap<FileId, bool> = FxHashMap::default();
        if let Some(s) = spans.iter().find(|s| !*checked.entry(s.file).or_insert_with(|| self.is_editable(s.file))) {
            return Err(format!("Renaming would change {}, which is outside the workspace", self.vfs.path(s.file).display()));
        }
        let mut seen = FxHashSet::default();
        let mut tokens = FxHashMap::default();
        let edits: Vec<(Span, String)> = spans.into_iter().filter(|s| seen.insert(*s)).map(|s| (s, self.rename_text(s, target, new_name, &mut tokens))).collect();
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

/// The spans of `.name` bindings in the argument list of the call whose name is at `call`.
fn named_arguments(toks: &[Token], call: Span, name: &str) -> Vec<Span> {
    let i = toks.partition_point(|t| t.span.start < call.start);
    if !toks.get(i).is_some_and(|t| t.span == call) || !toks.get(i + 1).is_some_and(|t| t.is_punct("(")) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut depth = 0usize;
    for k in i + 1..toks.len() {
        let t = &toks[k];
        if t.is_punct("(") || t.is_punct("[") || t.is_punct("{") {
            depth += 1;
        } else if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
            depth -= 1;
            if depth == 0 {
                break;
            }
        } else if depth == 1
            // Starting an argument: `obj.value()` inside a positional argument is no binding.
            && (toks[k - 1].is_punct(",") || k - 1 == i + 1)
            && t.is_punct(".")
            && toks.get(k + 1).is_some_and(|n| n.text == name && toks.get(k + 2).is_some_and(|p| p.is_punct("(")))
        {
            out.push(toks[k + 1].span);
        }
    }
    out
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
