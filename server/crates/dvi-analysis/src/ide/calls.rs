//! Call hierarchy of functions and tasks, on top of the reference index.

use super::Target;
use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{DeclId, DeclKind, Sep};
use dvi_syntax::{FileId, Span};
use lsp_types::{CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, SymbolKind};
use rustc_hash::{FxHashMap, FxHashSet};

/// Callers and callees grouped by declaration, in order of first appearance.
struct Groups {
    order: Vec<(u32, u32)>,
    by_key: FxHashMap<(u32, u32), (DeclRef, Vec<Span>)>,
}

impl Groups {
    fn new() -> Groups {
        Groups { order: Vec::new(), by_key: FxHashMap::default() }
    }

    fn add(&mut self, key: (u32, u32), r: DeclRef, span: Span) {
        let e = self.by_key.entry(key).or_insert_with(|| {
            self.order.push(key);
            (r, Vec::new())
        });
        if !e.1.contains(&span) {
            e.1.push(span);
        }
    }

    fn into_vec(mut self) -> Vec<(DeclRef, Vec<Span>)> {
        self.order.iter().filter_map(|k| self.by_key.remove(k)).collect()
    }
}

impl Analysis {
    /// The function or task at the cursor (a call or its declaration), as its prototype.
    pub fn callable_at(&self, file: FileId, off: u32) -> Option<DeclRef> {
        let Target::Decl(r) = self.target_at(file, off)? else { return None };
        let r = self.canonical(r);
        matches!(self.decl(r).kind, DeclKind::Function | DeclKind::Task).then_some(r)
    }

    /// The element of a call hierarchy item (located by its selection range): a function or
    /// task, or the module/class whose initial blocks or properties contain calls.
    pub fn call_item_decl(&self, file: FileId, off: u32) -> Option<DeclRef> {
        let Target::Decl(r) = self.target_at(file, off)? else { return None };
        Some(self.canonical(r))
    }

    /// An item for a function/task (at its body for extern methods) or another scope.
    pub fn call_item(&self, r: DeclRef) -> Option<CallHierarchyItem> {
        let r = self.implementation_of(r).unwrap_or(r);
        let d = self.decl(r);
        let loc = self.decl_location(r)?;
        let full = if d.span.file == d.name_span.file { self.to_range(d.span).unwrap_or(loc.range) } else { loc.range };
        let canon = self.canonical(r);
        let in_class = self.decl(canon).parent.is_some_and(|p| matches!(self.decl(DeclRef { unit: canon.unit, decl: p }).kind, DeclKind::Class | DeclKind::InterfaceClass));
        let kind = match d.kind {
            DeclKind::Function | DeclKind::Task if in_class => SymbolKind::METHOD,
            DeclKind::Function | DeclKind::Task => SymbolKind::FUNCTION,
            DeclKind::Class => SymbolKind::CLASS,
            DeclKind::InterfaceClass | DeclKind::Interface => SymbolKind::INTERFACE,
            DeclKind::Package => SymbolKind::PACKAGE,
            DeclKind::Covergroup => SymbolKind::STRUCT,
            _ => SymbolKind::MODULE,
        };
        let detail = match d.kind {
            DeclKind::Function | DeclKind::Task => {
                let q = self.qualified_name(canon);
                let container = q.rsplit_once("::").map(|(c, _)| c.to_string()).unwrap_or_default();
                format!("{} {}", d.kind.label(), container).trim().to_string()
            }
            k => k.label().to_string(),
        };
        Some(CallHierarchyItem {
            name: d.name.to_string(),
            kind,
            tags: None,
            detail: Some(detail),
            uri: loc.uri,
            range: full,
            selection_range: loc.range,
            data: None,
        })
    }

    pub fn prepare_call_hierarchy(&self, file: FileId, off: u32) -> Vec<CallHierarchyItem> {
        self.callable_at(file, off).and_then(|r| self.call_item(r)).into_iter().collect()
    }

    /// The function/task (or, outside of one, the design element/class) that contains `scope`.
    fn caller_of(&self, unit: UnitId, scope: DeclId) -> Option<DeclRef> {
        let u = self.unit(unit);
        let mut cur = Some(scope);
        let mut outer = None;
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            match d.kind {
                DeclKind::Function | DeclKind::Task => return Some(DeclRef { unit, decl: s }),
                DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Checker | DeclKind::Class
                | DeclKind::InterfaceClass | DeclKind::Package | DeclKind::Covergroup
                    if outer.is_none() =>
                {
                    outer = Some(DeclRef { unit, decl: s });
                }
                _ => {}
            }
            cur = d.parent;
        }
        outer
    }

    /// Call sites of a function/task, grouped by the calling function/task.
    pub fn incoming_calls(&self, callee: DeclRef) -> Vec<CallHierarchyIncomingCall> {
        let callee = self.canonical(callee);
        let key = self.decl_key(callee);
        let name = self.decl(callee).name.clone();
        let mut groups = Groups::new();
        let mut seen: FxHashSet<Span> = FxHashSet::default();
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
            }
            let Some(occ) = u.names.get(&name) else { continue };
            for (rid, seg) in occ {
                let chain = u.parsed.reference(*rid);
                let s = &chain.segs[*seg as usize];
                // A bare `new` names the constructor of the assigned variable's class.
                if s.from_macro || (s.name == "new" && s.sep == Sep::None) || seen.contains(&s.span) {
                    continue;
                }
                let Some(t) = self.resolve_ref(u.id, *rid, *seg as usize) else { continue };
                if self.decl_key(self.canonical(t)) != key {
                    continue;
                }
                let Some(caller) = self.caller_of(u.id, chain.scope) else { continue };
                let ckey = self.decl_key(self.canonical(caller));
                // `f = value` inside `f` sets the return value.
                if ckey == key && !s.called {
                    continue;
                }
                seen.insert(s.span);
                groups.add(ckey, caller, s.span);
            }
        }
        groups
            .into_vec()
            .into_iter()
            .filter_map(|(caller, spans)| {
                let from = match self.decl(caller).kind {
                    DeclKind::Function | DeclKind::Task => self.call_item(self.canonical(caller))?,
                    _ => self.call_item(caller)?,
                };
                let from_ranges = spans.iter().filter(|s| self.to_location(**s).is_some_and(|l| l.uri == from.uri)).filter_map(|s| self.to_range(*s)).collect();
                Some(CallHierarchyIncomingCall { from, from_ranges })
            })
            .collect()
    }

    /// Functions and tasks called from the body of `caller` (or from a module/class outside
    /// of its functions).
    pub fn outgoing_calls(&self, caller: DeclRef) -> Vec<CallHierarchyOutgoingCall> {
        let caller = self.canonical(caller);
        let own = self.decl_key(caller);
        let body = self.implementation_of(caller).unwrap_or(caller);
        let d = self.decl(body);
        let u = self.unit(body.unit);
        let Some(pos) = u.positions.get(&d.span.file) else { return Vec::new() };
        let mut groups = Groups::new();
        let lo = pos.refs.partition_point(|(s, _, _)| s.start < d.span.start);
        for (span, rid, seg) in &pos.refs[lo..] {
            if span.start > d.span.end {
                break;
            }
            let chain = u.parsed.reference(*rid);
            let s = &chain.segs[*seg as usize];
            if s.name == "new" && s.sep == Sep::None {
                continue;
            }
            // Only calls made by this element itself, not by the functions it contains.
            if self.caller_of(body.unit, chain.scope) != Some(body) {
                continue;
            }
            let Some(t) = self.resolve_ref(body.unit, *rid, *seg as usize) else { continue };
            let t = self.canonical(t);
            if !matches!(self.decl(t).kind, DeclKind::Function | DeclKind::Task) {
                continue;
            }
            let k = self.decl_key(t);
            if k == own && !s.called {
                continue;
            }
            groups.add(k, t, *span);
        }
        groups
            .into_vec()
            .into_iter()
            .filter_map(|(callee, spans)| {
                let to = self.call_item(callee)?;
                Some(CallHierarchyOutgoingCall { to, from_ranges: spans.iter().filter_map(|s| self.to_range(*s)).collect() })
            })
            .collect()
    }
}
