//! Drivers and loads of a signal: the places that write it and those that read it, in its
//! module and through the hierarchy, as possible static access sites (not proven hardware
//! drivers).
//!
//! The parser records what each assignment-like construct writes (`ParsedUnit::accesses`);
//! the other references are reads, unless they are connections to an instance's port (followed
//! into the child by the port's direction), arguments of a call (by the formal's direction),
//! terminals of a gate, or names in an `alias` or a clocking block (shown apart). A search that
//! starts at a port also goes up to every instantiation of its module, one branch each; a search
//! that went down through an instance never comes back up another one.

use super::checks::Occ;
use super::instances::{conn_list, ConnList, ItemKind};
use super::Target;
use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, AccessKind, DeclId, DeclKind, PortDir, ProcessKind, RefId};
use dvi_syntax::{FileId, Span, TokKind, Token};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::sync::Arc;

const MAX_CANDIDATES: usize = 200_000;
const MAX_SITES: usize = 2_000;
const MAX_HOPS: usize = 500;
const MAX_DEPTH: usize = 3;
/// Instantiations a search starting at a port goes up to at most.
const MAX_PARENTS: usize = 50;

/// What `accesses_at` found.
#[derive(Serialize, Debug, Clone, Default)]
pub struct AccessReport {
    pub target: super::checks::CheckTarget,
    /// `writes` or `reads`.
    pub mode: &'static str,
    pub sites: Vec<AccessSite>,
    pub limits: Vec<&'static str>,
    pub notes: Vec<String>,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct AccessSite {
    /// `blocking`, `nonblocking`, `continuous`, `compound`, `increment`, `procedural assign`,
    /// `force`, `release`, `deassign`, `net initializer`, `variable initializer`, `read`,
    /// `argument`, `gate output`, `gate input`, `switch terminal`, `alias`, `clocking`,
    /// `port`, `instance`.
    pub kind: &'static str,
    /// `writes`, `reads`, `control`, `connections` or `other`.
    pub group: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<&'static str>,
    pub label: String,
    /// Its source text on one line.
    pub detail: String,
    /// The instances on the way from the target's module (`u_core.u_fifo`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub context: String,
    pub uri: Option<String>,
    pub line: Option<u32>,
    pub character: Option<u32>,
    pub children: Vec<AccessSite>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Whether a system task or function writes its argument `k` (the others are read).
fn system_writes(name: &str, k: usize) -> bool {
    match name {
        "$sscanf" | "$fscanf" => k >= 2,
        "$value$plusargs" | "$readmemh" | "$readmemb" => k == 1,
        "$cast" | "$fgets" | "$fread" | "$swrite" | "$swriteb" | "$swriteh" | "$swriteo" | "$sformat" => k == 0,
        _ => false,
    }
}

/// The `(` around token `i`, through a concatenation's braces (not through an index's `[`:
/// what is inside one is read).
pub(crate) fn enclosing_paren(toks: &[Token], i: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut k = i;
    while k > 0 {
        k -= 1;
        let t = &toks[k];
        if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
            depth += 1;
        } else if t.is_punct("(") || t.is_punct("[") || t.is_punct("{") || t.is_punct("'{") {
            if depth == 0 {
                if t.is_punct("(") {
                    return Some(k);
                }
                if t.is_punct("[") {
                    return None;
                }
                continue;
            }
            depth -= 1;
        } else if t.is_punct(";") || i - k > 2000 {
            return None;
        }
    }
    None
}

/// The first token of the module item or statement token `i` is in, after a block label and
/// attributes.
fn item_start(toks: &[Token], i: usize) -> usize {
    let mut k = statement_start(toks, i);
    loop {
        if toks.get(k).is_some_and(|t| t.is_punct(":")) && toks.get(k + 1).is_some_and(|t| t.kind == TokKind::Ident) {
            k += 2;
        } else if toks.get(k).is_some_and(|t| t.is_punct("(*")) {
            while k < i && !toks[k].is_punct("*)") {
                k += 1;
            }
            k += 1;
        } else {
            return k;
        }
    }
}

fn kind_name(k: AccessKind) -> &'static str {
    match k {
        AccessKind::Continuous => "continuous",
        AccessKind::NetInit => "net initializer",
        AccessKind::VarInit => "variable initializer",
        AccessKind::Blocking => "blocking",
        AccessKind::NonBlocking => "nonblocking",
        AccessKind::Compound => "compound",
        AccessKind::IncDec => "increment",
        AccessKind::ProceduralAssign => "procedural assign",
        AccessKind::Force => "force",
        AccessKind::Release => "release",
        AccessKind::Deassign => "deassign",
    }
}

fn process_name(p: ProcessKind) -> Option<&'static str> {
    Some(match p {
        ProcessKind::None => return None,
        ProcessKind::Always => "always",
        ProcessKind::AlwaysFf => "always_ff",
        ProcessKind::AlwaysComb => "always_comb",
        ProcessKind::AlwaysLatch => "always_latch",
        ProcessKind::Initial => "initial",
        ProcessKind::Final => "final",
    })
}

struct Walk<'a> {
    a: &'a Analysis,
    writes: bool,
    candidates: usize,
    sites: usize,
    hops: usize,
    limits: Vec<&'static str>,
    notes: Vec<String>,
    /// Per unit: the access each written reference belongs to.
    targets: FxHashMap<UnitId, Arc<FxHashMap<RefId, usize>>>,
    tokens: FxHashMap<FileId, Option<Arc<Vec<Token>>>>,
    /// Per unit: its instances by the start of their name.
    instances: FxHashMap<UnitId, Arc<FxHashMap<(FileId, u32), DeclId>>>,
    /// Connection and argument lists by their file and `(` token.
    lists: FxHashMap<(FileId, usize), Option<Arc<ConnList>>>,
    /// The instances of each definition (built when a search first goes up).
    by_definition: Option<Arc<FxHashMap<(u32, u32), Vec<DeclRef>>>>,
    /// (declaration, instance path) on the current path.
    path: Vec<((u32, u32), String)>,
}

impl Analysis {
    /// The writes (`writes`) or reads of the declaration at `off` in `file`.
    pub fn accesses_at(&self, file: FileId, off: u32, writes: bool) -> Option<AccessReport> {
        self.with_lookup_memo(|| {
            let hit = self.hit_at(file, off)?;
            let Target::Decl(target) = self.target_of(&hit)? else { return None };
            if !matches!(self.decl(target).kind, DeclKind::Variable | DeclKind::Net | DeclKind::Port | DeclKind::Field | DeclKind::Argument) {
                return None;
            }
            let mut w = Walk { a: self, writes, candidates: 0, sites: 0, hops: 0, limits: Vec::new(), notes: Vec::new(), targets: FxHashMap::default(), tokens: FxHashMap::default(), instances: FxHashMap::default(), lists: FxHashMap::default(), by_definition: None, path: Vec::new() };
            let sites = w.search(target, "", None, None, 0);
            let d = self.decl(target);
            // `bind` instances in the target's module.
            if let Some(home) = super::checks::definition_of_pub(&self.unit(target.unit).parsed, target.decl) {
                let m = self.unit(target.unit).parsed.decl(home).name.clone();
                if self.units.iter().flatten().any(|u| u.parsed.binds.iter().any(|(to, _)| *to == m)) {
                    w.note(format!("`bind` instances in {m} are not followed"));
                }
            }
            let (uri, line, character) = w.location(d.name_span);
            let kind = match (d.kind, d.parent.map(|p| self.unit(target.unit).parsed.decl(p).name.to_string())) {
                (DeclKind::Port, Some(p)) => format!("port of {p}"),
                (k, _) => k.label().to_string(),
            };
            if d.kind == DeclKind::Field || d.parent.is_some_and(|p| self.unit(target.unit).parsed.decl(p).kind == DeclKind::Class) {
                w.note("a class field: the sites are those of every object of the class".into());
            }
            Some(AccessReport { target: super::checks::CheckTarget { name: d.name.to_string(), kind, uri, line, character }, mode: if writes { "writes" } else { "reads" }, sites, limits: w.limits, notes: w.notes })
        })
    }
}

impl Walk<'_> {
    fn limit(&mut self, what: &'static str) {
        if !self.limits.contains(&what) {
            self.limits.push(what);
        }
    }

    fn note(&mut self, what: String) {
        if !self.notes.contains(&what) && self.notes.len() < 20 {
            self.notes.push(what);
        }
    }

    fn stopped(&mut self) -> bool {
        if self.a.cancelled() {
            self.limit("cancelled");
        }
        self.limits.iter().any(|l| matches!(*l, "candidates" | "sites" | "cancelled"))
    }

    fn location(&self, span: Span) -> (Option<String>, Option<u32>, Option<u32>) {
        match self.a.to_range(span) {
            Some(r) => (Some(crate::ide::url_of(&self.a.path(span.file)).to_string()), Some(r.start.line), Some(r.start.character)),
            None => (None, None, None),
        }
    }

    fn file_tokens(&mut self, f: FileId) -> Option<Arc<Vec<Token>>> {
        let a = self.a;
        self.tokens.entry(f).or_insert_with(|| a.file_tokens(f)).clone()
    }

    /// The line of `span`, trimmed and shortened.
    fn line_text(&self, span: Span) -> String {
        let Some(t) = self.a.file_text(span.file) else { return String::new() };
        let s = span.start as usize;
        let a = t[..s.min(t.len())].rfind('\n').map_or(0, |i| i + 1);
        let b = t[s.min(t.len())..].find('\n').map_or(t.len(), |i| s + i);
        let line = t[a..b].trim();
        if line.chars().count() > 140 {
            format!("{}…", line.chars().take(139).collect::<String>())
        } else {
            line.to_string()
        }
    }

    fn site(&mut self, kind: &'static str, group: &'static str, at: Span, ctx: &str) -> AccessSite {
        self.sites += 1;
        if self.sites >= MAX_SITES {
            self.limit("sites");
        }
        let (uri, line, character) = self.location(at);
        AccessSite { kind, group, process: None, label: kind.to_string(), detail: self.line_text(at), context: ctx.to_string(), uri, line, character, children: Vec::new(), notes: Vec::new() }
    }

    /// The access (index in the unit's list) that writes reference `rid`.
    fn access_of(&mut self, unit: UnitId, rid: RefId) -> Option<usize> {
        let a = self.a;
        let m = self
            .targets
            .entry(unit)
            .or_insert_with(|| {
                let mut m = FxHashMap::default();
                for (i, x) in a.unit(unit).parsed.accesses.iter().enumerate() {
                    for r in &x.targets {
                        m.insert(*r, i);
                    }
                }
                Arc::new(m)
            })
            .clone();
        m.get(&rid).copied()
    }

    /// The sites of `target` in its module and below (and above, from a port), `ctx` the
    /// instance path from where the search started; `came_from`: the instance the search went
    /// down through (it does not go up again); `skip`: an instance whose connection is not
    /// followed down (the one the search came up through).
    fn search(&mut self, target: DeclRef, ctx: &str, came_from: Option<DeclRef>, skip: Option<DeclRef>, depth: usize) -> Vec<AccessSite> {
        let a = self.a;
        let key = (a.decl_key(a.canonical(target)), ctx.to_string());
        if self.path.contains(&key) || self.stopped() {
            return Vec::new();
        }
        if depth > MAX_DEPTH {
            self.limit("depth");
            return Vec::new();
        }
        self.path.push(key);
        let mut out = Vec::new();
        let (occs, limit) = a.find_occurrences(target, &mut self.candidates, MAX_CANDIDATES);
        if let Some(l) = limit {
            self.limit(l);
        }
        let class_handle = a.type_of(target).is_some_and(|t| a.decl(t).kind == DeclKind::Class);
        for o in occs {
            if self.stopped() {
                break;
            }
            // Followed into one instance: a hierarchical name through another instance
            // (`u1.y` while in `u0`) is that instance's signal, not this one's.
            if o.seg > 0 {
                if let (Some(inst), Some(head)) = (came_from, a.resolve_ref(o.unit, o.rid, 0)) {
                    if a.decl(head).kind == DeclKind::Instance && a.decl_key(a.canonical(head)) != a.decl_key(a.canonical(inst)) {
                        continue;
                    }
                }
            }
            if let Some(s) = self.classify(o, target, ctx, class_handle, skip, depth) {
                out.push(s);
            }
        }
        out.extend(self.dot_star(target, ctx, skip, depth));
        // Initializers of the declaration itself.
        if self.writes {
            let d = a.decl(target);
            let u = a.unit(target.unit);
            for x in u.parsed.accesses.iter().filter(|x| x.decl == Some(target.decl)) {
                let mut s = self.site(kind_name(x.kind), "writes", d.name_span, ctx);
                s.process = process_name(x.process);
                out.push(s);
            }
        }
        // Up from a port of the module, to every instantiation of it.
        let d = a.decl(target);
        if d.kind == DeclKind::Port && came_from.is_none() {
            let up = match d.dir {
                Some(PortDir::Input) | Some(PortDir::ConstRef) => self.writes,
                Some(PortDir::Output) => !self.writes,
                Some(PortDir::Inout) | Some(PortDir::Ref) => true,
                None => self.writes,
            };
            if up {
                if let Some(def) = d.parent.map(|p| DeclRef { unit: target.unit, decl: p }).filter(|p| a.decl(*p).kind.is_definition()) {
                    out.extend(self.up(def, &d.name.to_string(), ctx, depth));
                }
            }
        }
        self.path.pop();
        out
    }

    /// The instances of every definition, by the definition's key (none when cancelled).
    fn definitions(&mut self) -> Option<Arc<FxHashMap<(u32, u32), Vec<DeclRef>>>> {
        if let Some(m) = &self.by_definition {
            return Some(m.clone());
        }
        let a = self.a;
        let mut m: FxHashMap<(u32, u32), Vec<DeclRef>> = FxHashMap::default();
        let mut n = 0usize;
        for u in a.units.iter().flatten() {
            for (i, x) in u.parsed.decls.iter().enumerate() {
                if x.kind != DeclKind::Instance {
                    continue;
                }
                n += 1;
                if n % 1024 == 0 && self.stopped() {
                    return None;
                }
                let r = DeclRef { unit: u.id, decl: i as DeclId };
                if let Some(t) = a.type_of(r).filter(|t| a.decl(*t).kind.is_definition()) {
                    m.entry(a.decl_key(a.canonical(t))).or_default().push(r);
                }
            }
        }
        let m = Arc::new(m);
        self.by_definition = Some(m.clone());
        Some(m)
    }

    /// Instance `inst`'s name with the generate blocks it is in (`g1.u`).
    fn inst_path(&self, inst: DeclRef) -> String {
        let p = &self.a.unit(inst.unit).parsed;
        let mut names = vec![p.decl(inst.decl).name.to_string()];
        let mut cur = p.decl(inst.decl).parent;
        while let Some(c) = cur {
            let d = p.decl(c);
            if d.kind != DeclKind::Generate {
                break;
            }
            names.push(d.name.to_string());
            cur = d.parent;
        }
        names.reverse();
        names.join(".")
    }

    /// The instance whose name starts at `at` in unit `unit`.
    fn instance_named(&mut self, unit: UnitId, at: Span) -> Option<DeclRef> {
        let a = self.a;
        let m = self
            .instances
            .entry(unit)
            .or_insert_with(|| {
                let mut m = FxHashMap::default();
                for (i, d) in a.unit(unit).parsed.decls.iter().enumerate() {
                    if d.kind == DeclKind::Instance {
                        m.insert((d.name_span.file, d.name_span.start), i as DeclId);
                    }
                }
                Arc::new(m)
            })
            .clone();
        m.get(&(at.file, at.start)).map(|&decl| DeclRef { unit, decl }).filter(|r| a.decl(*r).name_span == at)
    }

    /// The list opening at `toks[open]` (cached).
    fn list_at(&mut self, toks: &[Token], open: usize) -> Option<Arc<ConnList>> {
        let key = (toks[open].span.file, open);
        self.lists.entry(key).or_insert_with(|| conn_list(toks, open).map(Arc::new)).clone()
    }

    /// `.*` instances in the target's module that connect it (by its name, where the instance
    /// is), to a port of the same name that carries the search on.
    fn dot_star(&mut self, target: DeclRef, ctx: &str, skip: Option<DeclRef>, depth: usize) -> Vec<AccessSite> {
        let a = self.a;
        let u = a.unit(target.unit);
        let d = a.decl(target);
        let mut out = Vec::new();
        if !matches!(d.kind, DeclKind::Variable | DeclKind::Net | DeclKind::Port) {
            return out;
        }
        let Some(home) = super::checks::definition_of_pub(&u.parsed, target.decl) else { return out };
        let name = d.name.to_string();
        let key = a.decl_key(a.canonical(target));
        for (id, x) in u.parsed.decls.iter().enumerate() {
            if x.kind != DeclKind::Instance || !x.has(dflags::DOT_STAR) || x.has(dflags::BOUND) {
                continue;
            }
            let inst = DeclRef { unit: target.unit, decl: id as DeclId };
            let parent = x.parent.unwrap_or(home);
            if Some(inst) == skip || super::checks::definition_of_pub(&u.parsed, parent) != Some(home) || self.stopped() {
                continue;
            }
            if a.lookup(target.unit, parent, &name).map(|t| a.decl_key(a.canonical(t))) != Some(key) {
                continue;
            }
            let Some(def) = a.type_of(inst).filter(|t| a.decl(*t).kind.is_definition()) else { continue };
            let Some(p) = a.member_of(def, &name).filter(|p| a.decl(*p).kind == DeclKind::Port) else { continue };
            // An explicit `.name(...)` wins over `.*`.
            let Some((toks, l)) = self.inst_list(inst) else { continue };
            let explicit = l.items.iter().any(|i| matches!(&i.kind, ItemKind::Named { name: n, .. } if *n == name));
            let star = l.items.iter().find(|i| matches!(i.kind, ItemKind::DotStar)).map(|i| toks[i.first].span);
            if explicit || !self.follows(p) {
                continue;
            }
            if let Some(n) = self.hop(inst, p, star.unwrap_or(x.name_span), true, " (.*)", ctx, depth) {
                out.push(n);
            }
        }
        out
    }

    /// The tokens of instance `inst`'s file and its connection list.
    fn inst_list(&mut self, inst: DeclRef) -> Option<(Arc<Vec<Token>>, super::instances::ConnList)> {
        let d = self.a.decl(inst);
        let toks = self.file_tokens(d.name_span.file)?;
        let k = toks.partition_point(|t| t.span.start < d.name_span.start);
        let mut j = k + 1;
        while toks.get(j).is_some_and(|t| t.is_punct("[")) {
            j = super::hints::skip(&toks, j);
        }
        if !toks.get(j).is_some_and(|t| t.is_punct("(")) {
            return None;
        }
        let l = conn_list(&toks, j)?;
        Some((toks, l))
    }

    /// The instantiations of `def`: for each, the actual connected to its port `port`, searched
    /// in the parent (one branch per instance).
    fn up(&mut self, def: DeclRef, port: &str, ctx: &str, depth: usize) -> Vec<AccessSite> {
        let a = self.a;
        let dk = a.decl_key(a.canonical(def));
        let Some(index) = self.definitions() else { return Vec::new() };
        let mut insts: Vec<DeclRef> = Vec::new();
        let mut seen: FxHashSet<(FileId, u32)> = FxHashSet::default();
        let mut bound = false;
        for &r in index.get(&dk).map(|v| v.as_slice()).unwrap_or(&[]) {
            let x = a.decl(r);
            if x.has(dflags::BOUND) {
                bound = true;
                continue;
            }
            if seen.insert((x.name_span.file, x.name_span.start)) {
                insts.push(r);
                if insts.len() > MAX_PARENTS {
                    self.limit("instances");
                    break;
                }
            }
        }
        if bound {
            self.note(format!("`bind` instances of {} are not followed", a.decl(def).name));
        }
        let mut out = Vec::new();
        for inst in insts.into_iter().take(MAX_PARENTS) {
            if self.stopped() {
                break;
            }
            self.hops += 1;
            if self.hops > MAX_HOPS {
                self.limit("connections");
                break;
            }
            let d = a.decl(inst);
            let parent = d.parent.and_then(|p| super::checks::definition_of_pub(&a.unit(inst.unit).parsed, p)).map(|p| a.unit(inst.unit).parsed.decl(p).name.to_string()).unwrap_or_default();
            let name = self.inst_path(inst);
            let mut node = self.site("instance", "connections", d.name_span, ctx);
            node.label = format!("{port} of {name} in {parent}");
            match self.actual_of(inst, def, port) {
                Some(Actual::Name(decl)) => {
                    node.children = self.search(decl, &parent, None, Some(inst), depth + 1);
                }
                Some(Actual::Expr(text)) => {
                    node.notes.push(format!("connected to an expression: {text}"));
                }
                Some(Actual::Open) => node.notes.push("left unconnected".into()),
                None => node.notes.push("connection not resolved".into()),
            }
            out.push(node);
        }
        out
    }

    /// What instance `inst` (of `def`) connects to its port `port`.
    fn actual_of(&mut self, inst: DeclRef, def: DeclRef, port: &str) -> Option<Actual> {
        let a = self.a;
        let d = a.decl(inst);
        let (toks, l) = self.inst_list(inst)?;
        if l.macros {
            return None;
        }
        let header = a.port_header(def);
        let parent = d.parent.unwrap_or(0);
        let plain = |first: usize, end: usize| -> Option<Actual> {
            let e = &toks[first..end];
            if e.is_empty() {
                return Some(Actual::Open);
            }
            if e.len() == 1 && e[0].kind == TokKind::Ident {
                return a.lookup(inst.unit, parent, &e[0].text).map(Actual::Name);
            }
            Some(Actual::Expr(super::views::tokens_text(e)))
        };
        let mut star = false;
        for (k, it) in l.items.iter().enumerate() {
            match &it.kind {
                ItemKind::Named { name, open, .. } if name == port => {
                    return match open {
                        Some(o) => plain(o + 1, it.end - 1),
                        None => a.lookup(inst.unit, parent, port).map(Actual::Name),
                    };
                }
                ItemKind::DotStar => star = true,
                ItemKind::Ordered | ItemKind::Empty if header.known && header.slots.get(k).and_then(|s| s.name.as_deref()) == Some(port) => {
                    return plain(it.body, it.end);
                }
                _ => {}
            }
        }
        if star {
            return a.lookup(inst.unit, parent, port).map(Actual::Name);
        }
        Some(Actual::Open)
    }

    /// One reference to the target: a site, a connection followed into a child, or nothing
    /// (a read in a search for writes, a write in a search for reads).
    fn classify(&mut self, o: Occ, target: DeclRef, ctx: &str, class_handle: bool, skip: Option<DeclRef>, depth: usize) -> Option<AccessSite> {
        let a = self.a;
        let u = a.unit(o.unit);
        let chain = u.parsed.reference(o.rid);
        // Written by an assignment (the target itself, or a part of it: a member of a struct,
        // not one of a class object, whose handle is only read).
        if let Some(i) = self.access_of(o.unit, o.rid) {
            let x = &u.parsed.accesses[i];
            let member_of_handle = class_handle && (o.seg as usize) + 1 < chain.segs.len();
            if !member_of_handle {
                let read_too = matches!(x.kind, AccessKind::Compound | AccessKind::IncDec);
                let control = matches!(x.kind, AccessKind::Release | AccessKind::Deassign);
                if self.writes || read_too {
                    let group = if control { "control" } else if self.writes { "writes" } else { "reads" };
                    let mut s = self.site(kind_name(x.kind), group, o.span, ctx);
                    s.process = process_name(x.process).or_else(|| self.subroutine_of(o.unit, chain.scope));
                    return Some(s);
                }
                return None;
            }
        }
        if o.from_macro {
            return (!self.writes).then(|| self.site("read", "reads", o.span, ctx));
        }
        let toks = self.file_tokens(o.span.file)?;
        let at = toks.partition_point(|t| t.span.start < o.span.start);
        // Inside parentheses: an instance connection, a gate terminal, or a call argument.
        if let Some(open) = enclosing_paren(&toks, at) {
            if let Some(s) = self.in_parens(&toks, open, at, o, target, ctx, skip, depth) {
                return s;
            }
        }
        // `alias` and clocking blocks: shown apart.
        let stmt_start = statement_start(&toks, at);
        if toks.get(stmt_start).is_some_and(|t| t.is_kw("alias")) {
            return Some(self.site("alias", "other", o.span, ctx));
        }
        if u.parsed.decl(chain.scope).kind == DeclKind::ClockingBlock {
            return Some(self.site("clocking", "other", o.span, ctx));
        }
        (!self.writes).then(|| {
            let mut s = self.site("read", "reads", o.span, ctx);
            s.process = self.process_at(o.unit, o.span).or_else(|| self.subroutine_of(o.unit, chain.scope));
            s
        })
    }

    /// The process `span` is in.
    fn process_at(&self, unit: UnitId, span: Span) -> Option<&'static str> {
        let xs = &self.a.unit(unit).parsed.processes;
        // Processes do not nest: in the order of their starts.
        let k = xs.partition_point(|(_, s, _)| (s.file, s.start) <= (span.file, span.start));
        let (kind, s, e) = xs[..k].last()?;
        (s.file == span.file && e.file == span.file && span.end <= e.end).then(|| process_name(*kind)).flatten()
    }

    /// `task`/`function` when scope `s` is in one.
    fn subroutine_of(&self, unit: UnitId, s: DeclId) -> Option<&'static str> {
        let p = &self.a.unit(unit).parsed;
        let mut d = Some(s);
        while let Some(x) = d {
            match p.decl(x).kind {
                DeclKind::Function => return Some("function"),
                DeclKind::Task => return Some("task"),
                k if k.is_definition() || k == DeclKind::Class => return None,
                _ => d = p.decl(x).parent,
            }
        }
        None
    }

    /// A reference in the parentheses opening at `toks[open]`: `Some(site or nothing)` when they
    /// are an instance's connections, a gate's terminals or a call's arguments.
    #[allow(clippy::too_many_arguments)]
    fn in_parens(&mut self, toks: &Arc<Vec<Token>>, open: usize, at: usize, o: Occ, target: DeclRef, ctx: &str, skip: Option<DeclRef>, depth: usize) -> Option<Option<AccessSite>> {
        let a = self.a;
        // `.name(expr)` binding a port or formal (after `(` or `,`, not a method call
        // `obj.name(...)`): the list is the one around it.
        let mut open = open;
        for _ in 0..64 {
            let named = open >= 3 && toks[open - 1].kind == TokKind::Ident && toks[open - 2].is_punct(".") && (toks[open - 3].is_punct("(") || toks[open - 3].is_punct(","));
            if !named {
                break;
            }
            open = enclosing_paren(toks, open - 2)?;
        }
        let callee = toks.get(open.checked_sub(1)?)?;
        let first = item_start(toks, open);
        let head = &toks[first];
        // The port list in a design element's header: no access.
        if head.kind == TokKind::Keyword && matches!(head.text.as_str(), "module" | "macromodule" | "interface" | "program" | "checker") {
            return Some(None);
        }
        // The item the reference is in, and its index (commas at depth 1).
        let l = self.list_at(toks, open)?;
        let (k, item) = l.items.iter().enumerate().find(|(_, i)| i.first <= at && at < i.end.max(i.first + 1))?;
        let item = item.clone();
        // A gate: `and g0 (o, a, b), g1 (...);` (with optional names, delays and strengths).
        if head.kind == TokKind::Keyword {
            let n = l.items.len();
            let role = match head.text.as_str() {
                "and" | "nand" | "or" | "nor" | "xor" | "xnor" => Some(if k == 0 { "gate output" } else { "gate input" }),
                "buf" | "not" => Some(if k + 1 < n { "gate output" } else { "gate input" }),
                "bufif0" | "bufif1" | "notif0" | "notif1" | "nmos" | "pmos" | "rnmos" | "rpmos" | "cmos" | "rcmos" => Some(if k == 0 { "gate output" } else { "gate input" }),
                "pullup" | "pulldown" => Some("gate output"),
                "tran" | "rtran" | "tranif0" | "tranif1" | "rtranif0" | "rtranif1" => Some(if k < 2 { "switch terminal" } else { "gate input" }),
                _ => None,
            };
            // Not a delay's or strength's parentheses: the list after an instance name or the
            // keyword's own list.
            let terminals = callee.kind == TokKind::Ident || callee.is_punct("]") || (open == first + 1);
            if let (Some(role), true) = (role, terminals) {
                let write = role == "gate output";
                let show = match role {
                    "switch terminal" => true,
                    _ => write == self.writes,
                };
                let group = match role {
                    "switch terminal" => "other",
                    _ if write => "writes",
                    _ => "reads",
                };
                return Some(show.then(|| self.site(role, group, o.span, ctx)));
            }
        }
        // An instance's connection: the instance named before `(`, after its dimensions.
        let mut c = open - 1;
        let mut steps = 0;
        while toks[c].is_punct("]") && steps < 16 {
            let mut d = 0i32;
            loop {
                if toks[c].is_punct("]") {
                    d += 1;
                } else if toks[c].is_punct("[") {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                c = c.checked_sub(1)?;
            }
            c = c.checked_sub(1)?;
            steps += 1;
        }
        if toks[c].kind == TokKind::Ident {
            if let Some(inst) = self.instance_named(o.unit, toks[c].span) {
                if a.decl(inst).has(dflags::BOUND) {
                    self.note(format!("`bind` instance {} is not followed", toks[c].text));
                    return Some(None);
                }
                if Some(inst) == skip {
                    return Some(None);
                }
                return Some(self.connection(toks, inst, &l, k, item.body, item.end, at, o, target, ctx, depth));
            }
        }
        // A call's argument.        // A call's argument.
        let _ = target;
        let (written, read) = if callee.kind == TokKind::SysIdent {
            let w = system_writes(&callee.text, k);
            (w, !w)
        } else if callee.kind == TokKind::Ident {
            let u = a.unit(o.unit);
            let pos = u.positions.get(&callee.span.file)?;
            let j = pos.refs.partition_point(|(sp, _, _)| sp.start < callee.span.start);
            let sub = pos.refs[j..].iter().take_while(|(sp, _, _)| sp.start == callee.span.start).find(|(sp, _, _)| sp.end == callee.span.end).and_then(|(_, rid, seg)| a.resolve_ref(o.unit, *rid, *seg as usize));
            match sub.filter(|s| matches!(a.decl(*s).kind, DeclKind::Function | DeclKind::Task)) {
                Some(sub) => {
                    let su = a.unit(sub.unit);
                    let formals: Vec<&dvi_syntax::ast::Decl> = su.parsed.decl(sub.decl).children.iter().map(|c| su.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Argument).collect();
                    let formal = match &item.kind {
                        ItemKind::Named { name, .. } => formals.iter().find(|f| f.name == name.as_str()).copied(),
                        _ => formals.get(k).copied(),
                    };
                    // An inout or ref actual is read and written.
                    match formal.and_then(|f| f.dir) {
                        Some(PortDir::Output) => (true, false),
                        Some(PortDir::Inout) | Some(PortDir::Ref) => (true, true),
                        _ => (false, true),
                    }
                }
                None => return None,
            }
        } else {
            return None;
        };
        if (self.writes && written) || (!self.writes && read) {
            let mut s = self.site("argument", if self.writes { "writes" } else { "reads" }, o.span, ctx);
            s.label = format!("argument of {}", callee.text);
            return Some(Some(s));
        }
        Some(None)
    }

    /// A reference in item `k` of instance `inst`'s connection list: by the port's direction,
    /// a connection followed into the child (or nothing in this search).
    #[allow(clippy::too_many_arguments)]
    fn connection(&mut self, toks: &Arc<Vec<Token>>, inst: DeclRef, l: &super::instances::ConnList, k: usize, body: usize, end: usize, at: usize, o: Occ, target: DeclRef, ctx: &str, depth: usize) -> Option<AccessSite> {
        let a = self.a;
        // Not followed: said in both searches.
        let unresolved = |w: &mut Self, why: &str| {
            let mut node = w.site("port", "connections", o.span, ctx);
            node.label = format!("connection of {}", a.decl(inst).name);
            node.notes.push(format!("{why}: not followed"));
            Some(node)
        };
        let Some(def) = a.type_of(inst).filter(|t| a.decl(*t).kind.is_definition()) else { return unresolved(self, "module not found") };
        let header = a.port_header(def);
        let (port, plain) = match &l.items[k].kind {
            ItemKind::Named { name, open, .. } => {
                // `.p` names the child's port: the reference to the parent's signal is the
                // actual (`.p(x)`'s x, or `.p` itself).
                if let Some(o) = open {
                    if at <= *o {
                        return None;
                    }
                    (name.clone(), is_plain(&toks[o + 1..end.saturating_sub(1)]))
                } else {
                    (name.clone(), true)
                }
            }
            ItemKind::Ordered if header.known => match header.slots.get(k).and_then(|s| s.name.clone()) {
                Some(n) => (n, is_plain(&toks[body..end])),
                None => return unresolved(self, "port not known"),
            },
            ItemKind::Ordered => return unresolved(self, "port list not known"),
            _ => return None,
        };
        let Some(p) = a.member_of(def, &port).filter(|p| a.decl(*p).kind == DeclKind::Port) else { return unresolved(self, "port not found") };
        // `.p` seen from inside the child: the name of its own port, no access of it.
        if a.decl_key(a.canonical(p)) == a.decl_key(a.canonical(target)) {
            return None;
        }
        if !self.follows(p) {
            return None;
        }
        // An index inside the actual is read, never written by the port.
        let mut depth_b = 0i32;
        for t in &toks[body..at] {
            if t.is_punct("[") {
                depth_b += 1;
            } else if t.is_punct("]") {
                depth_b -= 1;
            }
        }
        if depth_b > 0 {
            return (!self.writes).then(|| self.site("read", "reads", o.span, ctx));
        }
        self.hop(inst, p, o.span, plain, "", ctx, depth)
    }

    /// Whether port `p` carries the search into its module: an output (or inout, ref) for
    /// writes, an input (or inout, ref) for reads.
    fn follows(&self, p: DeclRef) -> bool {
        match self.a.decl(p).dir {
            Some(PortDir::Output) => self.writes,
            Some(PortDir::Inout) | Some(PortDir::Ref) => true,
            _ => !self.writes,
        }
    }

    /// The node of a connection at `at` to port `p` of instance `inst`, with the sites inside
    /// (none when the actual is an expression).
    #[allow(clippy::too_many_arguments)]
    fn hop(&mut self, inst: DeclRef, p: DeclRef, at: Span, plain: bool, how: &str, ctx: &str, depth: usize) -> Option<AccessSite> {
        let a = self.a;
        let port = a.decl(p).name.to_string();
        let dir = a.decl(p).dir;
        self.hops += 1;
        if self.hops > MAX_HOPS {
            self.limit("connections");
            return None;
        }
        let inst_name = self.inst_path(inst);
        let dir_name = match dir {
            Some(PortDir::Output) => "output",
            Some(PortDir::Inout) => "inout",
            Some(PortDir::Ref) => "ref",
            Some(PortDir::ConstRef) => "const ref",
            _ => "input",
        };
        let mut node = self.site("port", "connections", at, ctx);
        node.label = format!("{port} of {inst_name}{how} ({dir_name})");
        if !plain {
            node.notes.push("connected in an expression: not followed".into());
            return Some(node);
        }
        let sub = if ctx.is_empty() { inst_name } else { format!("{ctx}.{inst_name}") };
        node.children = self.search(p, &sub, Some(inst), None, depth + 1);
        Some(node)
    }
}

/// A name, with selects after it (`bus`, `bus[3]`, `mem[i][7:0]`).
fn is_plain(e: &[Token]) -> bool {
    if !e.first().is_some_and(|t| t.kind == TokKind::Ident) {
        return false;
    }
    let mut d = 0i32;
    for t in &e[1..] {
        if t.is_punct("[") {
            d += 1;
        } else if t.is_punct("]") {
            d -= 1;
        } else if d == 0 {
            return false;
        }
    }
    d == 0
}

enum Actual {
    Name(DeclRef),
    Expr(String),
    Open,
}

/// The index of the first token of the statement token `i` is in.
fn statement_start(toks: &[Token], i: usize) -> usize {
    let mut k = i;
    while k > 0 {
        let t = &toks[k - 1];
        if t.is_punct(";") || t.is_kw("begin") || t.is_kw("end") || (t.kind == TokKind::Keyword && dvi_syntax::token::is_end_kw(&t.text)) {
            break;
        }
        k -= 1;
        if i - k > 4000 {
            break;
        }
    }
    k
}
