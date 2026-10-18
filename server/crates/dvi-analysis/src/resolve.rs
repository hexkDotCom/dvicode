//! Name resolution: lexical scopes, class inheritance, package imports, `pkg::name` and
//! member chains (`a.b.c`).

use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, Decl, DeclId, DeclKind, ParsedUnit, RefChain, RefId, RefKind, Sep, TypeRef};

const MAX_DEPTH: usize = 32;

/// Built-in methods of every class.
pub const CLASS_BUILTINS: &[&str] = &[
    "randomize", "srandom", "get_randstate", "set_randstate", "pre_randomize", "post_randomize",
    "rand_mode", "constraint_mode", "new",
];

/// Built-in members of covergroups.
pub const COVERGROUP_BUILTINS: &[&str] = &[
    "sample", "get_coverage", "get_inst_coverage", "set_inst_name", "start", "stop", "option",
    "type_option",
];

impl Analysis {
    /// Resolves segment `upto` of a reference chain.
    pub fn resolve_ref(&self, unit: UnitId, rid: RefId, upto: usize) -> Option<DeclRef> {
        let u = self.unit(unit);
        let chain = u.parsed.reference(rid);
        self.resolve_chain(unit, chain, upto)
    }

    pub fn resolve_chain(&self, unit: UnitId, chain: &RefChain, upto: usize) -> Option<DeclRef> {
        let mut start = 0;
        // `local::x` refers to the scope of the randomize() call.
        if chain.segs.len() > 1 && chain.segs[0].name == "local" && chain.segs[1].sep == Sep::Scope {
            if upto == 0 {
                return None;
            }
            start = 1;
        }
        let mut cur = self.resolve_first(unit, chain, start)?;
        for i in start + 1..=upto.min(chain.segs.len() - 1) {
            let seg = &chain.segs[i];
            cur = self.resolve_member(cur, &seg.name, seg.sep, chain.segs[i - 1].selects)?;
        }
        Some(cur)
    }

    fn resolve_first(&self, unit: UnitId, chain: &RefChain, idx: usize) -> Option<DeclRef> {
        let seg = &chain.segs[idx];
        let name = seg.name.as_str();
        let mut scope = chain.scope;
        if chain.kind == RefKind::Base {
            // The base of a class is looked up outside the class (no inherited members).
            let u = self.unit(unit);
            let d = u.parsed.decl(scope);
            if matches!(d.kind, DeclKind::Class | DeclKind::InterfaceClass) {
                // Class parameters are still visible.
                if let Some(m) = u.parsed.lookup_member(scope, name).first() {
                    return Some(DeclRef { unit, decl: *m });
                }
                scope = d.parent.unwrap_or(ParsedUnit::ROOT);
            }
        }
        match name {
            "this" => return self.enclosing_class(unit, scope),
            "super" => {
                let c = self.enclosing_class(unit, scope)?;
                return self.class_base(c);
            }
            _ => {}
        }
        match chain.kind {
            RefKind::Port { inst } | RefKind::Param { inst } => {
                let module = self.type_of(DeclRef { unit, decl: inst })?;
                return self.member_of(module, name);
            }
            RefKind::Package => return self.global.packages.get(name).and_then(|v| v.first().copied()),
            RefKind::ImportItem if idx == 0 => return self.global.packages.get(name).and_then(|v| v.first().copied()),
            RefKind::Definition => {
                if let Some(d) = self.global.definitions.get(name).and_then(|v| v.first()) {
                    return Some(*d);
                }
            }
            RefKind::With { target } => {
                let u = self.unit(unit);
                let t = u.parsed.reference(target);
                if let Some(obj) = self.resolve_chain(unit, t, t.segs.len() - 1) {
                    if let Some(ty) = self.type_of(obj) {
                        if let Some(m) = self.member_of(ty, name) {
                            return Some(m);
                        }
                    }
                }
            }
            _ => {}
        }
        let scoped = chain.segs.get(idx + 1).is_some_and(|s| s.sep == Sep::Scope);
        if scoped {
            // `X::y`: X is a class/type in scope or a package.
            if let Some(r) = self.lookup(unit, scope, name) {
                let k = self.decl(r).kind;
                if k.is_type() || k == DeclKind::Package {
                    return Some(r);
                }
            }
            if let Some(p) = self.global.packages.get(name).and_then(|v| v.first()) {
                return Some(*p);
            }
        }
        self.lookup(unit, scope, name)
    }

    /// The class whose scope contains `scope` (or that owns an out-of-block method).
    pub fn enclosing_class(&self, unit: UnitId, scope: DeclId) -> Option<DeclRef> {
        let u = self.unit(unit);
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            match d.kind {
                DeclKind::Class | DeclKind::InterfaceClass => return Some(DeclRef { unit, decl: s }),
                DeclKind::Function | DeclKind::Task | DeclKind::Constraint if d.has(dflags::OUT_OF_BLOCK) => {
                    return self.owner_class(DeclRef { unit, decl: s });
                }
                _ => {}
            }
            cur = d.parent;
        }
        None
    }

    /// The class of an out-of-block method (`C` in `function void C::f()`).
    pub fn owner_class(&self, r: DeclRef) -> Option<DeclRef> {
        let d = self.decl(r);
        let owner = d.owner?;
        let parent = d.parent?;
        let u = self.unit(r.unit);
        let name = &u.parsed.reference(owner).segs[0].name;
        let c = self.lookup(r.unit, parent, name)?;
        let c = self.follow_typedef(c)?;
        matches!(self.decl(c).kind, DeclKind::Class | DeclKind::InterfaceClass).then_some(c)
    }

    /// Lexical lookup of `name` from `scope`, then compilation-unit and global names.
    pub fn lookup(&self, unit: UnitId, scope: DeclId, name: &str) -> Option<DeclRef> {
        let u = self.unit(unit);
        let mut cur = Some(scope);
        let mut depth = 0;
        while let Some(s) = cur {
            depth += 1;
            if depth > 256 {
                break;
            }
            if let Some(m) = pick(&u.parsed, u.parsed.lookup_member(s, name)) {
                return Some(DeclRef { unit, decl: m });
            }
            let d = u.parsed.decl(s);
            match d.kind {
                DeclKind::Class | DeclKind::InterfaceClass => {
                    if let Some(base) = self.class_base(DeclRef { unit, decl: s }) {
                        if let Some(m) = self.class_member(base, name, 1) {
                            return Some(m);
                        }
                    }
                }
                DeclKind::Function | DeclKind::Task | DeclKind::Constraint if d.has(dflags::OUT_OF_BLOCK) => {
                    if let Some(c) = self.owner_class(DeclRef { unit, decl: s }) {
                        if let Some(m) = self.class_member(c, name, 0) {
                            return Some(m);
                        }
                        // Continue in the scope that encloses the class.
                        if c.unit != unit {
                            let cp = self.decl(c).parent.unwrap_or(ParsedUnit::ROOT);
                            return self.lookup(c.unit, cp, name);
                        }
                    }
                }
                _ => {}
            }
            for imp in &d.imports {
                if imp.item.as_ref().is_some_and(|i| i != name) {
                    continue;
                }
                if let Some(p) = self.global.packages.get(&imp.pkg).and_then(|v| v.first()) {
                    if let Some(m) = self.member_of(*p, name) {
                        return Some(m);
                    }
                }
            }
            cur = d.parent;
        }
        self.lookup_global(unit, name)
    }

    pub fn lookup_global(&self, unit: UnitId, name: &str) -> Option<DeclRef> {
        if let Some(v) = self.global.unit_items.get(name) {
            if let Some(r) = v.iter().find(|r| r.unit == unit).or_else(|| v.first()) {
                return Some(*r);
            }
        }
        if let Some(v) = self.global.definitions.get(name) {
            return v.first().copied();
        }
        if let Some(v) = self.global.packages.get(name) {
            return v.first().copied();
        }
        let std = self.global.std_pkg?;
        self.member_of(std, name)
    }

    /// Members of a class, including inherited ones.
    pub fn class_member(&self, class: DeclRef, name: &str, depth: usize) -> Option<DeclRef> {
        if depth > MAX_DEPTH {
            return None;
        }
        let u = self.unit(class.unit);
        if let Some(m) = pick(&u.parsed, u.parsed.lookup_member(class.decl, name)) {
            return Some(DeclRef { unit: class.unit, decl: m });
        }
        let base = self.class_base(class)?;
        self.class_member(base, name, depth + 1)
    }

    /// The base class (`extends`) of a class, through typedefs.
    pub fn class_base(&self, class: DeclRef) -> Option<DeclRef> {
        let d = self.decl(class);
        let ext = d.extends.as_ref()?;
        let b = self.resolve_typeref(class.unit, ext)?;
        matches!(self.decl(b).kind, DeclKind::Class | DeclKind::InterfaceClass).then_some(b)
    }

    /// Classes and interface classes a class implements or (interface classes) extends.
    pub fn class_interfaces(&self, class: DeclRef) -> Vec<DeclRef> {
        let d = self.decl(class);
        d.implements.iter().filter_map(|t| self.resolve_typeref(class.unit, t)).collect()
    }

    /// Whether every base class of `class` resolves (so a missing member is really missing).
    pub fn class_chain_complete(&self, class: DeclRef) -> bool {
        let mut cur = class;
        for _ in 0..MAX_DEPTH {
            let d = self.decl(cur);
            if d.extends.is_none() {
                return true;
            }
            match self.class_base(cur) {
                Some(b) => cur = b,
                None => return false,
            }
        }
        false
    }

    pub fn resolve_typeref(&self, unit: UnitId, t: &TypeRef) -> Option<DeclRef> {
        if let Some(i) = t.inline {
            return Some(DeclRef { unit, decl: i });
        }
        let rid = t.ref_id?;
        let n = self.unit(unit).parsed.reference(rid).segs.len();
        let r = self.resolve_ref(unit, rid, n - 1)?;
        self.follow_typedef(r)
    }

    /// Follows typedefs (and forward typedefs) to the defining declaration.
    pub fn follow_typedef(&self, mut r: DeclRef) -> Option<DeclRef> {
        for _ in 0..MAX_DEPTH {
            let d = self.decl(r);
            if d.kind != DeclKind::Typedef {
                return Some(r);
            }
            if d.has(dflags::FORWARD) {
                // The real definition has the same name in the same scope or globally.
                let u = self.unit(r.unit);
                let parent = d.parent.unwrap_or(ParsedUnit::ROOT);
                let real = u
                    .parsed
                    .lookup_member(parent, &d.name)
                    .iter()
                    .copied()
                    .find(|m| !u.parsed.decl(*m).has(dflags::FORWARD))
                    .map(|m| DeclRef { unit: r.unit, decl: m })
                    .or_else(|| {
                        self.global
                            .unit_items
                            .get(&d.name)
                            .and_then(|v| v.iter().copied().find(|x| !self.decl(*x).has(dflags::FORWARD)))
                    });
                match real {
                    Some(x) if x != r => {
                        r = x;
                        continue;
                    }
                    _ => return Some(r),
                }
            }
            let ty = d.ty.as_ref()?;
            if let Some(i) = ty.inline {
                return Some(DeclRef { unit: r.unit, decl: i });
            }
            let rid = ty.ref_id?;
            let n = self.unit(r.unit).parsed.reference(rid).segs.len();
            let next = self.resolve_ref(r.unit, rid, n - 1)?;
            if next == r {
                return Some(r);
            }
            r = next;
        }
        Some(r)
    }

    /// The type whose members are accessed through `r` with `.`: the declared type of a
    /// variable, the return type of a function, or the scope itself.
    pub fn type_of(&self, r: DeclRef) -> Option<DeclRef> {
        let d = self.decl(r);
        match d.kind {
            DeclKind::Variable | DeclKind::Net | DeclKind::Port | DeclKind::Field | DeclKind::Argument
            | DeclKind::Parameter | DeclKind::LocalParam | DeclKind::Instance | DeclKind::Function => {
                let ty = d.ty.as_ref()?;
                self.resolve_typeref(r.unit, ty)
            }
            DeclKind::Typedef => self.follow_typedef(r),
            DeclKind::TypeParam | DeclKind::Task | DeclKind::EnumMember => None,
            _ => Some(r),
        }
    }

    /// Looks up `name` inside a container (class, package, struct, module, block...).
    pub fn member_of(&self, container: DeclRef, name: &str) -> Option<DeclRef> {
        self.member_of_depth(container, name, 0)
    }

    fn member_of_depth(&self, container: DeclRef, name: &str, depth: usize) -> Option<DeclRef> {
        let container = self.follow_typedef(container)?;
        let d = self.decl(container);
        match d.kind {
            DeclKind::Class | DeclKind::InterfaceClass => self.class_member(container, name, 0),
            _ => {
                let u = self.unit(container.unit);
                let own = pick(&u.parsed, u.parsed.lookup_member(container.decl, name)).map(|m| DeclRef { unit: container.unit, decl: m });
                if own.is_some() || d.kind != DeclKind::Package || depth > 8 {
                    return own;
                }
                self.exported_member(container, name, depth)
            }
        }
    }

    /// Names a package re-exports with `export other_pkg::name;`, `export other_pkg::*;` or
    /// `export *::*;`.
    fn exported_member(&self, pkg: DeclRef, name: &str, depth: usize) -> Option<DeclRef> {
        let d = self.decl(pkg);
        for e in &d.exports {
            if e.item.as_ref().is_some_and(|i| i != name) {
                continue;
            }
            let sources: Vec<&str> = if e.pkg == "*" {
                // Everything imported into the package.
                d.imports.iter().filter(|i| i.item.as_ref().map_or(true, |x| x == name)).map(|i| i.pkg.as_str()).collect()
            } else {
                vec![e.pkg.as_str()]
            };
            for s in sources {
                if let Some(p) = self.global.packages.get(s).and_then(|v| v.first()) {
                    if let Some(m) = self.member_of_depth(*p, name, depth + 1) {
                        return Some(m);
                    }
                }
            }
        }
        None
    }

    fn resolve_member(&self, prev: DeclRef, name: &str, sep: Sep, selects: u8) -> Option<DeclRef> {
        // Methods of an array itself (`q.size()`), not of its elements.
        if sep == Sep::Dot && (selects as u32) < self.array_dims(prev) {
            return None;
        }
        let pd = self.decl(prev);
        let container = match (sep, pd.kind) {
            (Sep::Scope, DeclKind::Package | DeclKind::Class | DeclKind::InterfaceClass | DeclKind::Covergroup) => Some(prev),
            (Sep::Scope, DeclKind::Typedef) => self.follow_typedef(prev),
            (Sep::Scope, DeclKind::TypeParam) => None,
            _ => self.type_of(prev),
        }?;
        self.member_of(container, name)
    }

    /// Unpacked dimensions of a declaration, including those of array typedefs it uses.
    pub fn array_dims(&self, r: DeclRef) -> u32 {
        let mut cur = r;
        let mut dims = 0;
        for _ in 0..8 {
            let d = self.decl(cur);
            dims += d.unpacked_dims();
            if !matches!(d.kind, DeclKind::Variable | DeclKind::Net | DeclKind::Port | DeclKind::Field | DeclKind::Argument | DeclKind::Typedef | DeclKind::Function) {
                break;
            }
            let Some(rid) = d.ty.as_ref().and_then(|t| t.ref_id) else { break };
            let n = self.unit(cur.unit).parsed.reference(rid).segs.len();
            match self.resolve_ref(cur.unit, rid, n - 1) {
                Some(next) if self.decl(next).kind == DeclKind::Typedef && next != cur => cur = next,
                _ => break,
            }
        }
        dims
    }

    /// Location key that identifies a declaration across units (a header compiled twice
    /// yields two decls with the same key).
    pub fn decl_key(&self, r: DeclRef) -> (u32, u32) {
        let d = self.decl(r);
        (d.name_span.file, d.name_span.start)
    }

    /// For an out-of-block method body, the prototype inside its class; otherwise itself.
    pub fn canonical(&self, r: DeclRef) -> DeclRef {
        let d = self.decl(r);
        if d.has(dflags::OUT_OF_BLOCK) {
            if let Some(c) = self.owner_class(r) {
                let u = self.unit(c.unit);
                if let Some(m) = u.parsed.lookup_member(c.decl, &d.name).first() {
                    return DeclRef { unit: c.unit, decl: *m };
                }
            }
        }
        r
    }

    /// For a method prototype (`extern`/`pure virtual`), the out-of-block body if any.
    pub fn implementation_of(&self, r: DeclRef) -> Option<DeclRef> {
        let d = self.decl(r);
        if !matches!(d.kind, DeclKind::Function | DeclKind::Task | DeclKind::Constraint) || !d.has(dflags::PROTOTYPE) {
            return None;
        }
        let class = d.parent.map(|p| DeclRef { unit: r.unit, decl: p })?;
        let key = self.decl_key(class);
        for u in self.units.iter().flatten() {
            for (i, x) in u.parsed.decls.iter().enumerate() {
                if x.has(dflags::OUT_OF_BLOCK) && x.name == d.name && x.kind == d.kind {
                    let xr = DeclRef { unit: u.id, decl: i as DeclId };
                    if let Some(c) = self.owner_class(xr) {
                        if self.decl_key(c) == key {
                            return Some(xr);
                        }
                    }
                }
            }
        }
        None
    }

    /// Qualified name such as `pkg::cls::method`.
    pub fn qualified_name(&self, r: DeclRef) -> String {
        let u = self.unit(r.unit);
        let mut parts = vec![u.parsed.decl(r.decl).name.to_string()];
        if let Some(c) = (self.decl(r).has(dflags::OUT_OF_BLOCK)).then(|| self.owner_class(r)).flatten() {
            return format!("{}::{}", self.qualified_name(c), parts[0]);
        }
        let mut cur = u.parsed.decl(r.decl).parent;
        while let Some(p) = cur {
            let d = u.parsed.decl(p);
            if d.kind == DeclKind::Unit {
                break;
            }
            if !d.name.is_empty() && !matches!(d.kind, DeclKind::StructType | DeclKind::EnumType) {
                parts.push(d.name.to_string());
            }
            cur = d.parent;
        }
        parts.reverse();
        parts.join("::")
    }

    /// Innermost declaration scope of `unit` containing offset `off` of `file`.
    pub fn scope_at(&self, unit: UnitId, file: dvi_syntax::FileId, off: u32) -> DeclId {
        let u = self.unit(unit);
        let mut best = ParsedUnit::ROOT;
        let mut best_len = u32::MAX;
        for (i, d) in u.parsed.decls.iter().enumerate() {
            if i == 0 || !d.kind.is_scope() || d.span.file != file {
                continue;
            }
            if d.span.start <= off && off <= d.span.end && d.span.len() <= best_len {
                best = i as DeclId;
                best_len = d.span.len();
            }
        }
        best
    }
}

/// Picks the best of several same-name members: a real definition over a forward typedef.
fn pick(p: &ParsedUnit, ids: &[DeclId]) -> Option<DeclId> {
    match ids.len() {
        0 => None,
        1 => Some(ids[0]),
        _ => ids
            .iter()
            .copied()
            .find(|i| !p.decl(*i).has(dflags::FORWARD))
            .or_else(|| ids.first().copied()),
    }
}

pub fn decl_is(d: &Decl, kinds: &[DeclKind]) -> bool {
    kinds.contains(&d.kind)
}
