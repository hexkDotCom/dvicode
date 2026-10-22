//! Name resolution: lexical scopes, class inheritance, package imports, `pkg::name` and
//! member chains (`a.b.c`).

use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{dflags, Decl, DeclId, DeclKind, ParsedUnit, RefChain, RefId, RefKind, Sep, TypeRef};
use dvi_syntax::Span;
use rustc_hash::{FxHashMap, FxHashSet};

const MAX_DEPTH: usize = 32;

thread_local! {
    /// How many `class_base` calls are on this thread's stack.
    static BASE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Limits nested `class_base` calls (a base resolved through the class itself).
struct BaseGuard;

impl BaseGuard {
    fn enter() -> Option<BaseGuard> {
        BASE_DEPTH.with(|d| {
            if d.get() >= MAX_DEPTH {
                return None;
            }
            d.set(d.get() + 1);
            Some(BaseGuard)
        })
    }
}

impl Drop for BaseGuard {
    fn drop(&mut self) {
        BASE_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

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
            RefKind::Package => return self.package_next(unit, chain, idx),
            RefKind::ImportItem if idx == 0 => return self.package_next(unit, chain, idx),
            RefKind::Definition => {
                if let Some(d) = self.instance_definitions().get(&(unit, seg.span)) {
                    return Some(*d);
                }
                if let Some(d) = self.definition(unit, name) {
                    return Some(d);
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
                if k.is_type() {
                    return Some(r);
                }
                if k == DeclKind::Package {
                    return self.package_next(unit, chain, idx).or(Some(r));
                }
            }
            if let Some(p) = self.package_next(unit, chain, idx) {
                return Some(p);
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
                if let Some(p) = self.package_with(unit, &imp.pkg, name) {
                    if let Some(m) = self.member_of(p, name) {
                        return Some(m);
                    }
                }
            }
            cur = d.parent;
        }
        self.lookup_global(unit, name)
    }

    /// Which of several packages, design elements or compilation-unit items of one name a use
    /// in `from` means. With a build file it is the first one compiled, as in a simulator.
    /// Auto-configuration compiles every copy in the folder (one per top or per generated
    /// configuration, a vendored copy next to the original, ...), so in auto-configured code it
    /// is the copy nearest to `from` in the folder tree, and the first compiled one among
    /// copies that are equally near.
    pub fn pick_global(&self, from: UnitId, v: &[DeclRef]) -> Option<DeclRef> {
        self.candidates(from, v).into_iter().next()
    }

    /// The definitions of a name in the order `pick_global` prefers them: only the first one
    /// with a build file, all of them, nearest first, in auto-configured code.
    fn candidates(&self, from: UnitId, v: &[DeclRef]) -> Vec<DeclRef> {
        if v.len() < 2 || !self.nearest_wins(from) {
            return v.first().copied().into_iter().collect();
        }
        let here = self.path(self.unit(from).root);
        let mut ranked: Vec<(usize, DeclRef)> = v
            .iter()
            .map(|r| (here.components().zip(self.path(self.unit(r.unit).root).components()).take_while(|(a, b)| a == b).count(), *r))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        ranked.into_iter().map(|(_, r)| r).collect()
    }

    /// The package `name` as a use in `from` sees it (see `pick_global`).
    pub fn package(&self, from: UnitId, name: &str) -> Option<DeclRef> {
        self.global.packages.get(name).and_then(|v| self.pick_global(from, v))
    }

    /// The package `name` for a use of its member `member` in `from`: in auto-configured code,
    /// the nearest copy that declares the member, so that a use is not checked against a
    /// different version of the package (see `pick_global`).
    pub fn package_with(&self, from: UnitId, name: &str, member: &str) -> Option<DeclRef> {
        let c = self.candidates(from, self.global.packages.get(name)?);
        c.iter().copied().find(|p| self.member_of(*p, member).is_some()).or_else(|| c.first().copied())
    }

    /// The package named by segment `idx` of a chain (`pkg::name`, `import pkg::name`), for a
    /// use of the member in the next segment.
    fn package_next(&self, unit: UnitId, chain: &RefChain, idx: usize) -> Option<DeclRef> {
        let name = chain.segs[idx].name.as_str();
        match chain.segs.get(idx + 1) {
            Some(next) if next.sep == Sep::Scope || chain.kind == RefKind::ImportItem => self.package_with(unit, name, &next.name),
            _ => self.package(unit, name),
        }
    }

    /// The design element `name` as a use in `from` sees it (see `pick_global`).
    pub fn definition(&self, from: UnitId, name: &str) -> Option<DeclRef> {
        self.global.definitions.get(name).and_then(|v| self.pick_global(from, v))
    }

    fn nearest_wins(&self, unit: UnitId) -> bool {
        self.try_unit(unit).is_some_and(|u| u.kind != UnitKind::Builtin && self.setup.config.invocations.get(u.invocation).is_some_and(|i| i.auto))
    }

    /// For instances in auto-configured code of design elements defined more than once: the
    /// nearest copy that has every port and parameter the instance connects by name, by the
    /// span of the element's name in the instantiation. Computed once per snapshot.
    fn instance_definitions(&self) -> &FxHashMap<(UnitId, Span), DeclRef> {
        self.instance_defs.get_or_init(|| {
            let mut out = FxHashMap::default();
            for u in self.units.iter().flatten() {
                if !self.nearest_wins(u.id) {
                    continue;
                }
                let mut named: FxHashMap<DeclId, Vec<&str>> = FxHashMap::default();
                for c in &u.parsed.refs {
                    if let RefKind::Port { inst } | RefKind::Param { inst } = c.kind {
                        if let Some(s) = c.segs.first() {
                            named.entry(inst).or_default().push(s.name.as_str());
                        }
                    }
                }
                // Instances in one statement (`M a(.x(x)), b(.x(x), .y(y));`) share the name: the
                // copy must fit all of them.
                let mut by_name: FxHashMap<Span, (&str, Vec<&str>)> = FxHashMap::default();
                for (inst, names) in named {
                    let Some(rid) = u.parsed.decl(inst).ty.as_ref().and_then(|t| t.ref_id) else { continue };
                    let Some(seg) = u.parsed.reference(rid).segs.first().filter(|s| !s.from_macro) else { continue };
                    by_name.entry(seg.span).or_insert_with(|| (seg.name.as_str(), Vec::new())).1.extend(names);
                }
                for (span, (name, names)) in by_name {
                    let Some(v) = self.global.definitions.get(name).filter(|v| v.len() > 1) else { continue };
                    // Own members only: following `bind`s would resolve instances again.
                    let fits = |c: &DeclRef| {
                        let p = &self.unit(c.unit).parsed;
                        names.iter().all(|n| !p.lookup_member(c.decl, n).is_empty())
                    };
                    if let Some(c) = self.candidates(u.id, v).into_iter().find(fits) {
                        out.insert((u.id, span), c);
                    }
                }
            }
            out
        })
    }

    pub fn lookup_global(&self, unit: UnitId, name: &str) -> Option<DeclRef> {
        if let Some(v) = self.global.unit_items.get(name) {
            if let Some(r) = v.iter().find(|r| r.unit == unit).copied().or_else(|| self.pick_global(unit, v)) {
                return Some(r);
            }
        }
        if let Some(r) = self.definition(unit, name) {
            return Some(r);
        }
        if let Some(r) = self.package(unit, name) {
            return Some(r);
        }
        // `import p::*;` outside any design element in an earlier file of the same invocation.
        if let Some(inv) = self.units.get(unit as usize).and_then(|u| u.as_ref()).map(|u| u.invocation) {
            for (u, i, pkg, item) in &self.global.unit_imports {
                if *i != inv || *u >= unit || item.as_ref().is_some_and(|it| it != name) {
                    continue;
                }
                if let Some(m) = self.package_with(unit, pkg, name).and_then(|p| self.member_of(p, name)) {
                    return Some(m);
                }
            }
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
        // `class C extends C::T` needs C's base to resolve its own base: give up instead of
        // recursing until the stack overflows.
        let _guard = BaseGuard::enter()?;
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
        let container = self.follow_typedef(container)?;
        let d = self.decl(container);
        match d.kind {
            DeclKind::Class | DeclKind::InterfaceClass => self.class_member(container, name, 0),
            _ => {
                let u = self.unit(container.unit);
                let own = pick(&u.parsed, u.parsed.lookup_member(container.decl, name)).map(|m| DeclRef { unit: container.unit, decl: m });
                if own.is_none() && d.kind.is_definition() {
                    if let Some(b) = self.bound_instance(container, &d.name, name) {
                        return Some(b);
                    }
                }
                if own.is_some() || d.kind != DeclKind::Package {
                    return own;
                }
                let mut seen = FxHashSet::default();
                seen.insert(self.decl_key(container));
                self.exported_member(container, name, &mut seen)
            }
        }
    }

    /// An instance called `name` that a `bind` adds to `module`: `bind module_name ...`, or
    /// `bind inst.path ...` where the path, from the scope of the `bind`, names an instance of
    /// the module.
    fn bound_instance(&self, module: DeclRef, module_name: &str, name: &str) -> Option<DeclRef> {
        if let Some(b) = self.global.binds.get(module_name).and_then(|v| v.iter().find(|b| self.decl(**b).name == name)) {
            return Some(*b);
        }
        for (target, v) in &self.global.binds {
            if self.global.definitions.contains_key(target) {
                continue;
            }
            for b in v.iter().filter(|b| self.decl(**b).name == name) {
                let scope = self.decl(*b).parent.unwrap_or(ParsedUnit::ROOT);
                let mut segs = target.split('.');
                let Some(mut inst) = segs.next().and_then(|s| self.lookup(b.unit, scope, s)) else { continue };
                // Further segments go through declared instances only (not other binds).
                for s in segs {
                    let Some(m) = self.type_of(inst) else { break };
                    let u = self.unit(m.unit);
                    match pick(&u.parsed, u.parsed.lookup_member(m.decl, s)) {
                        Some(d) => inst = DeclRef { unit: m.unit, decl: d },
                        None => break,
                    }
                }
                if self.decl(inst).kind == DeclKind::Instance && self.decl(inst).name == target.rsplit('.').next().unwrap_or("") && self.type_of(inst) == Some(module) {
                    return Some(*b);
                }
            }
        }
        None
    }

    /// Names a package re-exports with `export other_pkg::name;`, `export other_pkg::*;` or
    /// `export *::*;`. `seen`: the packages searched so far (packages may export each other).
    fn exported_member(&self, pkg: DeclRef, name: &str, seen: &mut FxHashSet<(u32, u32)>) -> Option<DeclRef> {
        // Depth first, each package once: the nearest copy of a package, and what it exports,
        // before a farther copy.
        let mut todo = vec![pkg];
        while let Some(p) = todo.pop() {
            if p != pkg {
                let u = self.unit(p.unit);
                if let Some(m) = pick(&u.parsed, u.parsed.lookup_member(p.decl, name)) {
                    return Some(DeclRef { unit: p.unit, decl: m });
                }
            }
            let d = self.decl(p);
            let mut next = Vec::new();
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
                    let Some(all) = self.global.packages.get(s) else { continue };
                    for c in self.candidates(p.unit, all) {
                        if seen.insert(self.decl_key(c)) {
                            next.push(c);
                        }
                    }
                }
            }
            // The first to look at goes last on the stack.
            todo.extend(next.into_iter().rev());
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
