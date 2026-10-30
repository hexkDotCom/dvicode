//! Assertions and coverage of a signal or field: the assertions that check it, the covers and
//! coverpoints that sample it, and through what (a named property, a port of an instance or a
//! `bind`, a covergroup's `sample()` argument).
//!
//! The parser records each assertion and coverage item with the references of each of its
//! parts (`ParsedUnit::verif`), on the preprocessed text, so assertions written through macros
//! are found like the others. A reference to the target is classified by the item it is in; a
//! reference in a port connection or in a covergroup's arguments continues at the port or the
//! formal, a few steps at most. What is found is what mentions the target, not a proof that it
//! is checked.

use super::hints::list;
use super::Target;
use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, RefId, VerifForm, VerifKind, VerifRole};
use dvi_syntax::{FileId, Span, TokKind, Token};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::sync::Arc;

/// References looked at (resolved) at most, over a whole search.
const MAX_CANDIDATES: usize = 200_000;
/// Results made at most.
const MAX_HITS: usize = 2_000;
/// Steps through properties, ports and arguments followed at most.
const MAX_DEPTH: usize = 3;

/// What `checks_at` found.
#[derive(Serialize, Debug, Clone, Default)]
pub struct ChecksReport {
    pub target: CheckTarget,
    pub hits: Vec<CheckHit>,
    /// What stopped the search before its end (`candidates`, `hits`, `depth`, `cancelled`);
    /// empty when it was complete.
    pub limits: Vec<&'static str>,
    /// What was not followed (a `bind` to an instance path, a macro in a port list...).
    pub notes: Vec<String>,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct CheckTarget {
    pub name: String,
    pub kind: String,
    pub uri: Option<String>,
    pub line: Option<u32>,
    pub character: Option<u32>,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct CheckHit {
    /// `assert`, `assume`, `cover`, `restrict`, `expect`, `cover sequence`, `coverpoint`,
    /// `cross`, `covergroup event`, `default clocking`, `default disable`, `property`,
    /// `sequence`, `let`, `port`, `argument`.
    pub kind: &'static str,
    /// `checks`, `covers`, `coverage`, `helpers` or `connections`.
    pub group: &'static str,
    /// Assertions: `concurrent`, `immediate` or `deferred`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form: Option<&'static str>,
    /// What the target is there: `checked`, `clock`, `reset`, `gate`, `bins`, `cross item`,
    /// `connection`, `argument`.
    pub roles: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Its source text, on one line, shortened.
    pub detail: String,
    pub uri: Option<String>,
    pub line: Option<u32>,
    pub character: Option<u32>,
    pub children: Vec<CheckHit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Identity, to merge what several references (and units) find: first and last span.
    #[serde(skip)]
    key: Option<(Span, Span, u64)>,
}

/// A number for a name, to tell apart results at the same place.
fn name_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    s.hash(&mut h);
    h.finish()
}

/// A reference to a declaration: the reference, its segment that names it, and that segment's
/// span.
#[derive(Clone, Copy)]
pub(crate) struct Occ {
    pub unit: UnitId,
    pub rid: RefId,
    pub seg: u16,
    pub span: Span,
    /// Made by a macro body: its span is the macro call's (classified, never followed through
    /// a port or an argument).
    pub from_macro: bool,
}

impl Analysis {
    /// Every reference to `target` in the project (from macro bodies too), the units that
    /// compile its file first; `candidates` counts the references resolved, up to `max`. The
    /// reason it stopped early (`candidates`, `cancelled`), if it did.
    pub(crate) fn find_occurrences(&self, target: DeclRef, candidates: &mut usize, max: usize) -> (Vec<Occ>, Option<&'static str>) {
        let key = self.decl_key(self.canonical(target));
        let name = self.decl(target).name.clone();
        let mut out = Vec::new();
        let mut seen: FxHashSet<(UnitId, RefId, u16)> = FxHashSet::default();
        // A file compiled in many units repeats its references: a source span found to be a use
        // is not resolved again in other units (in its own unit every time: one macro argument
        // can be expanded twice).
        let mut spans: FxHashMap<Span, (u8, bool, UnitId)> = FxHashMap::default();
        let home = self.decl(target).name_span.file;
        let mut units: Vec<&Arc<crate::world::Unit>> = self.units.iter().flatten().filter(|u| u.kind != UnitKind::Builtin).collect();
        units.sort_by_key(|u| !u.files.contains(&home));
        for u in units {
            if self.cancelled() {
                return (out, Some("cancelled"));
            }
            let Some(occ) = u.names.get(&name) else { continue };
            for (rid, seg) in occ.iter() {
                *candidates += 1;
                if *candidates > max {
                    return (out, Some("candidates"));
                }
                if *candidates % 1024 == 0 && self.cancelled() {
                    return (out, Some("cancelled"));
                }
                let s = &u.parsed.reference(*rid).segs[*seg as usize];
                let entry = if s.from_macro { None } else { spans.get(&s.span).copied() };
                if let Some((tries, found, first)) = entry {
                    if first != u.id && (found || tries >= 3) {
                        *candidates -= 1;
                        continue;
                    }
                }
                let hit = self.resolve_ref(u.id, *rid, *seg as usize).is_some_and(|t| self.decl_key(self.canonical(t)) == key);
                if !s.from_macro {
                    let e = spans.entry(s.span).or_insert((0, false, u.id));
                    e.0 = e.0.saturating_add(1);
                    e.1 |= hit;
                }
                if hit && seen.insert((u.id, *rid, *seg)) {
                    out.push(Occ { unit: u.id, rid: *rid, seg: *seg, span: s.span, from_macro: s.from_macro });
                }
            }
        }
        (out, None)
    }
}

struct Search<'a> {
    a: &'a Analysis,
    candidates: usize,
    hits: usize,
    limits: Vec<&'static str>,
    notes: Vec<String>,
    /// Per unit: the parts of its verification items, `(first ref, end ref, item, role)`,
    /// sorted by first reference.
    parts: FxHashMap<UnitId, Arc<Vec<(RefId, RefId, usize, VerifRole)>>>,
    tokens: FxHashMap<FileId, Option<Arc<Vec<Token>>>>,
    /// Per unit and file: its instances (not bound ones), by start.
    instances: FxHashMap<(UnitId, FileId), Arc<Vec<(Span, DeclId)>>>,
    /// Declarations on the current path (a loop through ports is cut).
    path: Vec<(u32, u32)>,
    /// Per unit: the spans of its macro calls, by start (an item written through a macro shows
    /// the whole call).
    calls: FxHashMap<UnitId, Arc<Vec<Span>>>,
}

impl Analysis {
    /// The assertions and coverage of the declaration at `off` in `file`.
    pub fn checks_at(&self, file: FileId, off: u32) -> Option<ChecksReport> {
        self.with_lookup_memo(|| {
            let hit = self.hit_at(file, off)?;
            let Target::Decl(target) = self.target_of(&hit)? else { return None };
            if !searchable(self.decl(target).kind) {
                return None;
            }
            Some(self.checks_of(target))
        })
    }

    /// The assertions and coverage of `target`.
    pub fn checks_of(&self, target: DeclRef) -> ChecksReport {
        let mut s = Search { a: self, candidates: 0, hits: 0, limits: Vec::new(), notes: Vec::new(), parts: FxHashMap::default(), tokens: FxHashMap::default(), instances: FxHashMap::default(), path: Vec::new(), calls: FxHashMap::default() };
        let mut hits = s.classify(target, 0);
        sort_hits(&mut hits);
        let d = self.decl(target);
        let (uri, line, character) = s.location(d.name_span);
        let kind = match (d.kind, self.decl_parent_kind(target)) {
            (DeclKind::Port, Some(p)) => format!("port of {}", p),
            (k, _) => k.label().to_string(),
        };
        ChecksReport { target: CheckTarget { name: d.name.to_string(), kind, uri, line, character }, hits, limits: s.limits, notes: s.notes }
    }

    /// The name of the definition a declaration is in (`fifo` for its port).
    fn decl_parent_kind(&self, r: DeclRef) -> Option<String> {
        let u = self.unit(r.unit);
        let p = u.parsed.decl(r.decl).parent?;
        Some(u.parsed.decl(p).name.to_string())
    }
}

/// Declarations whose uses can be searched: values, ports, fields, helpers, coverpoints.
fn searchable(k: DeclKind) -> bool {
    matches!(
        k,
        DeclKind::Variable
            | DeclKind::Net
            | DeclKind::Port
            | DeclKind::Field
            | DeclKind::Parameter
            | DeclKind::LocalParam
            | DeclKind::Argument
            | DeclKind::Property
            | DeclKind::Sequence
            | DeclKind::Let
            | DeclKind::Coverpoint
            | DeclKind::EnumMember
            | DeclKind::Genvar
            | DeclKind::ClockingBlock
    )
}

/// The order of the groups shown.
fn group_rank(g: &str) -> u8 {
    match g {
        "checks" => 0,
        "covers" => 1,
        "coverage" => 2,
        "helpers" => 3,
        _ => 4,
    }
}

fn sort_hits(hits: &mut [CheckHit]) {
    hits.sort_by(|x, y| (group_rank(x.group), &x.uri, x.line, x.character).cmp(&(group_rank(y.group), &y.uri, y.line, y.character)));
    for h in hits {
        sort_hits(&mut h.children);
    }
}

fn role_name(r: VerifRole) -> &'static str {
    match r {
        VerifRole::Checked => "checked",
        VerifRole::Clock => "clock",
        VerifRole::Reset => "reset",
        VerifRole::Gate => "gate",
        VerifRole::Bins => "bins",
        VerifRole::CrossItem => "cross item",
    }
}

fn kind_name(k: VerifKind) -> (&'static str, &'static str) {
    match k {
        VerifKind::Assert => ("assert", "checks"),
        VerifKind::Assume => ("assume", "checks"),
        VerifKind::Restrict => ("restrict", "checks"),
        VerifKind::Expect => ("expect", "checks"),
        VerifKind::Cover => ("cover", "covers"),
        VerifKind::CoverSequence => ("cover sequence", "covers"),
        VerifKind::Coverpoint => ("coverpoint", "coverage"),
        VerifKind::Cross => ("cross", "coverage"),
        VerifKind::CovergroupEvent => ("covergroup event", "coverage"),
        VerifKind::DefaultClocking => ("default clocking", "checks"),
        VerifKind::DefaultDisable => ("default disable", "checks"),
        VerifKind::PropertyBody => ("property", "helpers"),
        VerifKind::SequenceBody => ("sequence", "helpers"),
        VerifKind::ClockingBlock => ("clocking", "checks"),
    }
}

fn form_name(f: VerifForm) -> Option<&'static str> {
    match f {
        VerifForm::Concurrent => Some("concurrent"),
        VerifForm::Immediate => Some("immediate"),
        VerifForm::Deferred => Some("deferred"),
        VerifForm::None => None,
    }
}

impl Search<'_> {
    /// A budget ran out or the request was cancelled: nothing more is searched.
    fn stopped(&mut self) -> bool {
        if self.a.cancelled() {
            self.limit("cancelled");
        }
        self.limits.iter().any(|l| matches!(*l, "candidates" | "hits" | "cancelled"))
    }

    fn limit(&mut self, what: &'static str) {
        if !self.limits.contains(&what) {
            self.limits.push(what);
        }
    }

    fn note(&mut self, what: String) {
        if !self.notes.contains(&what) && self.notes.len() < 50 {
            self.notes.push(what);
        }
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

    /// The source text from `start` to `end` on one line, shortened.
    fn text(&self, start: Span, end: Span) -> String {
        if start.file != end.file || end.end <= start.start || end.end - start.start > 4000 {
            return String::new();
        }
        let Some(t) = self.a.file_text(start.file) else { return String::new() };
        let Some(s) = t.get(start.start as usize..end.end as usize) else { return String::new() };
        let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if one.chars().count() > 160 {
            format!("{}…", one.chars().take(159).collect::<String>())
        } else {
            one
        }
    }

    /// Every reference to `target` in the project (from macro bodies too), within the budget.
    fn occurrences(&mut self, target: DeclRef) -> Vec<Occ> {
        let (out, limit) = self.a.find_occurrences(target, &mut self.candidates, MAX_CANDIDATES);
        if let Some(l) = limit {
            self.limit(l);
        }
        out
    }

    /// The item and role of reference `rid` of `unit`, when it is in a verification item.
    fn part_of(&mut self, unit: UnitId, rid: RefId) -> Option<(usize, VerifRole)> {
        let a = self.a;
        let parts = self
            .parts
            .entry(unit)
            .or_insert_with(|| {
                let mut v: Vec<(RefId, RefId, usize, VerifRole)> = Vec::new();
                for (i, item) in a.unit(unit).parsed.verif.iter().enumerate() {
                    for &(role, first, end) in &item.parts {
                        if first < end {
                            v.push((first, end, i, role));
                        }
                    }
                }
                v.sort_by_key(|p| p.0);
                Arc::new(v)
            })
            .clone();
        let k = parts.partition_point(|p| p.0 <= rid);
        let p = parts.get(k.checked_sub(1)?)?;
        (rid < p.1).then_some((p.2, p.3))
    }

    /// Where `target` is checked or sampled, following properties, ports and arguments.
    fn classify(&mut self, target: DeclRef, depth: usize) -> Vec<CheckHit> {
        let a = self.a;
        let key = a.decl_key(a.canonical(target));
        if self.path.contains(&key) {
            return Vec::new();
        }
        if depth > MAX_DEPTH {
            self.limit("depth");
            return Vec::new();
        }
        if self.stopped() {
            return Vec::new();
        }
        self.path.push(key);
        let mut hits: Vec<CheckHit> = Vec::new();
        let mut by_key: FxHashMap<(Span, Span, u64), usize> = FxHashMap::default();
        // Helpers already shown (their index in `hits`, when they are), with their roles.
        let mut helpers: FxHashMap<(u32, u32), Option<usize>> = FxHashMap::default();
        let add = |hits: &mut Vec<CheckHit>, by_key: &mut FxHashMap<(Span, Span, u64), usize>, h: CheckHit| match h.key.and_then(|k| by_key.get(&k).copied()) {
            Some(i) => {
                for r in h.roles {
                    if !hits[i].roles.contains(&r) {
                        hits[i].roles.push(r);
                    }
                }
            }
            None => {
                if let Some(k) = h.key {
                    by_key.insert(k, hits.len());
                }
                hits.push(h);
            }
        };
        for o in self.occurrences(target) {
            if self.hits >= MAX_HITS {
                self.limit("hits");
                break;
            }
            let part = self.part_of(o.unit, o.rid);
            let in_body = part.and_then(|(item, role)| {
                let v = &a.unit(o.unit).parsed.verif[item];
                matches!(v.kind, VerifKind::PropertyBody | VerifKind::SequenceBody).then(|| (DeclRef { unit: o.unit, decl: v.decl.unwrap_or(v.scope) }, role))
            });
            if let Some((item, _)) = part.filter(|(i, _)| a.unit(o.unit).parsed.verif[*i].kind == VerifKind::ClockingBlock) {
                for d in self.defaults_selecting(o.unit, item) {
                    let h = self.item_hit(o.unit, d, VerifRole::Clock, depth);
                    add(&mut hits, &mut by_key, h);
                }
                continue;
            }
            if let Some((item, role)) = part.filter(|_| in_body.is_none()) {
                let h = self.item_hit(o.unit, item, role, depth);
                add(&mut hits, &mut by_key, h);
                continue;
            }
            let helper = in_body.or_else(|| self.helper_of(o).map(|h| (h, VerifRole::Checked)));
            if let Some((helper, role)) = helper {
                let hk = a.decl_key(a.canonical(helper));
                let r = role_name(role);
                match helpers.get(&hk) {
                    Some(Some(i)) => {
                        if !hits[*i].roles.contains(&r) {
                            hits[*i].roles.push(r);
                        }
                    }
                    Some(None) => {}
                    None => {
                        let at = self.helper_hit(helper, r, depth).map(|h| {
                            add(&mut hits, &mut by_key, h);
                            hits.len() - 1
                        });
                        helpers.insert(hk, at);
                    }
                }
                continue;
            }
            if o.from_macro {
                continue;
            }
            if let Some(h) = self.through_port(o, depth) {
                add(&mut hits, &mut by_key, h);
            } else if let Some(h) = self.through_argument(o, depth) {
                add(&mut hits, &mut by_key, h);
            }
        }
        for h in self.through_implicit_ports(target, depth) {
            add(&mut hits, &mut by_key, h);
        }
        self.path.pop();
        hits
    }

    /// The hit for item `idx` of `unit`; a labelled coverpoint has the crosses that cross it.
    fn item_hit(&mut self, unit: UnitId, idx: usize, role: VerifRole, depth: usize) -> CheckHit {
        self.hits += 1;
        let a = self.a;
        let u = a.unit(unit);
        let item = &u.parsed.verif[idx];
        // Items one macro call makes share their spans: their order among those tells them apart.
        let nth = u.parsed.verif[..idx].iter().filter(|v| v.start == item.start && v.end == item.end).count() as u64;
        let (kind, group) = kind_name(item.kind);
        let label = item.label.as_ref().map(|l| l.to_string()).or_else(|| match item.kind {
            VerifKind::Coverpoint | VerifKind::Cross | VerifKind::CovergroupEvent => Some(format!("{kind} of {}", u.parsed.decl(item.scope).name)),
            _ => None,
        });
        let (uri, line, character) = self.location(item.start);
        let mut notes = Vec::new();
        if matches!(item.kind, VerifKind::DefaultClocking | VerifKind::DefaultDisable) {
            let own = if item.kind == VerifKind::DefaultClocking { VerifRole::Clock } else { VerifRole::Reset };
            let home = definition_of(&u.parsed, item.scope);
            let n = u.parsed.verif.iter().filter(|v| v.form == VerifForm::Concurrent && definition_of(&u.parsed, v.scope) == home && !v.parts.iter().any(|p| p.0 == own)).count();
            notes.push(format!("applies to {n} concurrent assertion{} without their own {}", if n == 1 { "" } else { "s" }, if own == VerifRole::Clock { "clock" } else { "disable iff" }));
        }
        let children = match (item.kind, item.decl) {
            (VerifKind::Coverpoint, Some(d)) => self.classify(DeclRef { unit, decl: d }, depth + 1),
            _ => Vec::new(),
        };
        CheckHit {
            kind,
            group,
            form: form_name(item.form),
            roles: vec![role_name(role)],
            label,
            detail: self.item_text(unit, item.start, item.end),
            uri,
            line,
            character,
            children,
            notes,
            key: Some((item.start, item.end, nth)),
        }
    }

    /// The text of an item from `start` to `end`; the whole macro call when it is in one.
    fn item_text(&mut self, unit: UnitId, start: Span, end: Span) -> String {
        let a = self.a;
        let calls = self
            .calls
            .entry(unit)
            .or_insert_with(|| {
                let mut v: Vec<Span> = a.unit(unit).macro_uses.iter().map(|m| m.call_span).filter(|s| !s.is_empty()).collect();
                v.sort_by_key(|s| (s.file, s.start));
                Arc::new(v)
            })
            .clone();
        let k = calls.partition_point(|c| (c.file, c.start) <= (start.file, start.start));
        // The outermost call holding the start (calls written in calls come after it).
        let outer = calls[..k].iter().rev().take(64).filter(|c| c.file == start.file && c.end >= start.end).max_by_key(|c| c.end - c.start);
        match outer {
            Some(c) => self.text(*c, *c),
            None => self.text(start, end),
        }
    }

    /// The `default clocking <name>;` items of `unit` that select clocking block item `block`.
    fn defaults_selecting(&self, unit: UnitId, block: usize) -> Vec<usize> {
        let p = &self.a.unit(unit).parsed;
        let b = &p.verif[block];
        let home = definition_of(p, b.scope);
        p.verif
            .iter()
            .enumerate()
            .filter(|(_, v)| v.kind == VerifKind::DefaultClocking && v.parts.is_empty() && v.label == b.label && definition_of(p, v.scope) == home)
            .map(|(i, _)| i)
            .collect()
    }

    /// The property, sequence or `let` whose body reference `o` is in.
    fn helper_of(&self, o: Occ) -> Option<DeclRef> {
        let u = self.a.unit(o.unit);
        let mut d = Some(u.parsed.reference(o.rid).scope);
        while let Some(id) = d {
            let decl = u.parsed.decl(id);
            match decl.kind {
                DeclKind::Property | DeclKind::Sequence | DeclKind::Let => return Some(DeclRef { unit: o.unit, decl: id }),
                k if k.is_definition() || matches!(k, DeclKind::Class | DeclKind::Package | DeclKind::Unit) => return None,
                _ => d = decl.parent,
            }
        }
        None
    }

    /// A property, sequence or `let` that uses the target, with what uses it in turn. A `let`
    /// that no assertion or coverage uses is left out (it is often a plain function).
    fn helper_hit(&mut self, helper: DeclRef, role: &'static str, depth: usize) -> Option<CheckHit> {
        let a = self.a;
        let d = a.decl(helper);
        let kind = match d.kind {
            DeclKind::Property => "property",
            DeclKind::Sequence => "sequence",
            _ => "let",
        };
        let children = self.classify(helper, depth + 1);
        if kind == "let" && children.is_empty() {
            return None;
        }
        self.hits += 1;
        let (uri, line, character) = self.location(d.name_span);
        let mut notes = Vec::new();
        if children.is_empty() {
            notes.push("declaration only: no assertion uses it".to_string());
        }
        let end = Span::new(d.span.file, d.span.end, d.span.end);
        Some(CheckHit {
            kind,
            group: "helpers",
            form: None,
            roles: vec![role],
            label: Some(d.name.to_string()),
            detail: self.text(d.span, end),
            uri,
            line,
            character,
            children,
            notes,
            key: Some((d.name_span, d.name_span, 0)),
        })
    }

    /// The instances (not bound ones) of `unit` in file `f`, by start.
    fn instances_in(&mut self, unit: UnitId, f: FileId) -> Arc<Vec<(Span, DeclId)>> {
        let a = self.a;
        self.instances
            .entry((unit, f))
            .or_insert_with(|| {
                let p = &a.unit(unit).parsed;
                let mut v: Vec<(Span, DeclId)> = p.decls.iter().enumerate().filter(|(_, d)| d.kind == DeclKind::Instance && !d.has(dflags::BOUND) && d.span.file == f).map(|(i, d)| (d.span, i as DeclId)).collect();
                v.sort_by_key(|x| x.0.start);
                Arc::new(v)
            })
            .clone()
    }

    /// The connection list of instance `inst` (the token index of its `(`), in `toks`.
    fn connection_list(toks: &[Token], name: Span) -> Option<(usize, Vec<(usize, usize)>)> {
        let k = toks.partition_point(|t| t.span.start < name.start);
        if !toks.get(k).is_some_and(|t| t.span == name) {
            return None;
        }
        let mut j = k + 1;
        while toks.get(j).is_some_and(|t| t.is_punct("[")) {
            j = super::hints::skip(toks, j);
        }
        if !toks.get(j).is_some_and(|t| t.is_punct("(")) {
            return None;
        }
        let (items, _) = list(toks, j);
        Some((j, items))
    }

    /// Reference `o` in a port connection of an instance: what checks that port inside it.
    fn through_port(&mut self, o: Occ, depth: usize) -> Option<CheckHit> {
        let a = self.a;
        let insts = self.instances_in(o.unit, o.span.file);
        let toks = self.file_tokens(o.span.file)?;
        let at = toks.partition_point(|t| t.span.start < o.span.start);
        let k = insts.partition_point(|x| x.0.start <= o.span.start);
        // Instances of one statement share its start: the one named before the `(` around the
        // reference (through a `.port(...)` and a concatenation).
        let mut open = super::drivers::enclosing_paren(&toks, at)?;
        for _ in 0..64 {
            if open >= 3 && toks[open - 1].kind == TokKind::Ident && toks[open - 2].is_punct(".") && (toks[open - 3].is_punct("(") || toks[open - 3].is_punct(",")) {
                open = super::drivers::enclosing_paren(&toks, open - 2)?;
            } else {
                break;
            }
        }
        let mut c = open.checked_sub(1)?;
        // An array of instances: `u[3:0] (...)`.
        while toks.get(c).is_some_and(|t| t.is_punct("]")) {
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
        }
        let name_at = toks.get(c)?.span;
        let (inst, items, slot, first, end) = insts[..k].iter().rev().filter(|x| x.0.end >= o.span.end).map(|x| DeclRef { unit: o.unit, decl: x.1 }).find(|r| a.decl(*r).name_span == name_at).and_then(|inst| {
            let (_, items) = Self::connection_list(&toks, a.decl(inst).name_span)?;
            let (slot, &(first, end)) = items.iter().enumerate().find(|(_, (f, e))| *f <= at && at < *e)?;
            Some((inst, items.clone(), slot, first, end))
        })?;
        let name_span = a.decl(inst).name_span;
        let inst_name = a.decl(inst).name.to_string();
        if items.iter().any(|&(f, e)| toks[f..e].iter().any(|t| t.kind == TokKind::Directive)) {
            self.note(format!("not followed: a macro in the port list of {inst_name}"));
            return None;
        }
        let def = a.type_of(inst).filter(|d| a.decl(*d).kind.is_definition())?;
        let (port, inner) = slot_port(a, &toks, def, slot, first, end)?;
        // Not the port's own name (`.req` of `.req(x)` names the instance's port), nor an index.
        if at < inner.0 || at >= inner.1 || inside_brackets(&toks, inner, at) {
            return None;
        }
        let expr: Vec<Token> = toks[inner.0..inner.1].to_vec();
        self.port_hit(def, &port, &inst_name, name_span, &expr, "", depth)
    }

    /// The hit for port `port` of instance `inst_name` of `def`, with what checks it there.
    #[allow(clippy::too_many_arguments)]
    fn port_hit(&mut self, def: DeclRef, port: &str, inst_name: &str, at: Span, expr: &[Token], how: &str, depth: usize) -> Option<CheckHit> {
        let a = self.a;
        let p = a.member_of(def, port).filter(|p| a.decl(*p).kind == DeclKind::Port)?;
        let children = self.classify(p, depth + 1);
        if children.is_empty() {
            return None;
        }
        self.hits += 1;
        let plain = plain_expr(expr);
        let mut notes = Vec::new();
        if !plain {
            notes.push("connected in an expression".to_string());
        }
        let (uri, line, character) = self.location(at);
        let def_name = a.decl(def).name.to_string();
        let detail = if expr.is_empty() { format!("{def_name} {inst_name} (.*)") } else { format!("{def_name} {inst_name} (.{port}({}))", super::views::tokens_text(expr)) };
        Some(CheckHit {
            kind: "port",
            group: "connections",
            form: None,
            roles: vec!["connection"],
            label: Some(format!("{port} of {inst_name}{how}")),
            detail,
            uri,
            line,
            character,
            children,
            notes,
            key: Some((at, at, name_hash(port))),
        })
    }

    /// Ports connected without naming the target: `.*` of instances next to it, and `bind`s
    /// to its module (their actuals are names of that module, read here as text).
    fn through_implicit_ports(&mut self, target: DeclRef, depth: usize) -> Vec<CheckHit> {
        let a = self.a;
        let d = a.decl(target);
        if !matches!(d.kind, DeclKind::Port | DeclKind::Net | DeclKind::Variable | DeclKind::Parameter | DeclKind::LocalParam) {
            return Vec::new();
        }
        let u = a.unit(target.unit);
        let Some(home) = definition_of(&u.parsed, target.decl) else { return Vec::new() };
        let home_decl = u.parsed.decl(home);
        let name = d.name.to_string();
        let mut out = Vec::new();
        // `.*` in the target's module: the name the instance would connect, resolved where the
        // instance is, must be the target.
        let key = a.decl_key(a.canonical(target));
        let dot_star: Vec<DeclId> = u.parsed.decls.iter().enumerate().filter(|(_, x)| x.kind == DeclKind::Instance && x.has(dflags::DOT_STAR) && !x.has(dflags::BOUND) && definition_of(&u.parsed, x.parent.unwrap_or(0)) == Some(home)).map(|(i, _)| i as DeclId).collect();
        for id in dot_star {
            if self.stopped() {
                return out;
            }
            self.candidates += 1;
            let parent = u.parsed.decl(id).parent.unwrap_or(home);
            if a.lookup(target.unit, parent, &name).map(|t| a.decl_key(a.canonical(t))) != Some(key) {
                continue;
            }
            let inst = DeclRef { unit: target.unit, decl: id };
            out.extend(self.implicit_hits(inst, &name, false, depth));
        }
        // `bind`s into the module (`bind <module> ...`, `bind <module> : <instances> ...`,
        // `bind <instance path> ...`), in any unit. The bind's actuals are names of the module:
        // the target's name there must be the target (not one of a nested scope).
        if a.lookup(target.unit, home, &name).map(|t| a.decl_key(a.canonical(t))) != Some(key) {
            return out;
        }
        let module = home_decl.name.as_str();
        let mut seen: FxHashSet<(FileId, u32)> = FxHashSet::default();
        for un in a.units.iter().flatten() {
            for (_, inst) in &un.parsed.binds {
                let r = DeclRef { unit: un.id, decl: *inst };
                let sp = a.decl(r).name_span;
                if !seen.insert((sp.file, sp.start)) {
                    continue;
                }
                if self.stopped() {
                    return out;
                }
                self.candidates += 1;
                if a.bind_target(r).is_some_and(|t| a.decl(t.module).name == module) {
                    out.extend(self.implicit_hits(r, &name, true, depth));
                }
            }
        }
        out
    }

    /// The ports of instance `inst` that `name` reaches without a reference: `.*` (unless a
    /// slot names that port), and for a `bind` every slot whose expression names it.
    fn implicit_hits(&mut self, inst: DeclRef, name: &str, bound: bool, depth: usize) -> Vec<CheckHit> {
        let a = self.a;
        let d = a.decl(inst);
        let mut out = Vec::new();
        let Some(toks) = self.file_tokens(d.name_span.file) else { return out };
        let Some((_, items)) = Self::connection_list(&toks, d.name_span) else { return out };
        let inst_name = d.name.to_string();
        if items.iter().any(|&(f, e)| toks[f..e].iter().any(|t| t.kind == TokKind::Directive)) {
            self.note(format!("not followed: a macro in the port list of {inst_name}"));
            return out;
        }
        let Some(def) = a.type_of(inst).filter(|x| a.decl(*x).kind.is_definition()) else { return out };
        let named = |f: usize| toks[f].is_punct(".") && toks.get(f + 1).is_some_and(|t| t.text == name);
        let explicit = items.iter().any(|&(f, e)| f < e && named(f));
        let how = if bound { " (bind)" } else { " (.*)" };
        if !explicit && items.iter().any(|&(f, e)| e == f + 1 && toks[f].is_punct(".*")) {
            out.extend(self.port_hit(def, name, &inst_name, d.name_span, &[], how, depth));
        }
        if !bound {
            return out;
        }
        for (slot, &(first, end)) in items.iter().enumerate() {
            if first >= end || self.stopped() {
                continue;
            }
            let Some((port, inner)) = slot_port(a, &toks, def, slot, first, end) else { continue };
            let names_it = (inner.0..inner.1).any(|i| {
                toks[i].kind == TokKind::Ident && toks[i].text == name && !(i > 0 && (toks[i - 1].is_punct(".") || toks[i - 1].is_punct("::"))) && !inside_brackets(&toks, inner, i)
            });
            if names_it {
                let expr: Vec<Token> = toks[inner.0..inner.1].to_vec();
                out.extend(self.port_hit(def, &port, &inst_name, d.name_span, &expr, how, depth));
            }
        }
        out
    }

    /// Reference `o` in the arguments of a covergroup's `sample(...)` or `new(...)`: what
    /// samples that formal.
    fn through_argument(&mut self, o: Occ, depth: usize) -> Option<CheckHit> {
        let a = self.a;
        let toks = self.file_tokens(o.span.file)?;
        let at = toks.partition_point(|t| t.span.start < o.span.start);
        // The `sample(`/`new(` call around it, through named arguments and parentheses.
        let mut inner = at;
        let mut found = None;
        for _ in 0..8 {
            let open = enclosing_open(&toks, inner)?;
            let callee = toks.get(open.checked_sub(1)?)?;
            if callee.text == "sample" && open >= 3 && toks[open - 2].is_punct(".") {
                found = Some((open, true, open - 3));
                break;
            }
            if callee.is_kw("new") && open >= 3 && toks[open - 2].is_punct("=") {
                found = Some((open, false, open - 3));
                break;
            }
            inner = open;
        }
        let (open, sample, recv) = found?;
        let callee = &toks[open - 1];
        let recv_tok = toks.get(recv).filter(|t| t.kind == TokKind::Ident)?;
        let u = a.unit(o.unit);
        let pos = u.positions.get(&o.span.file)?;
        let k = pos.refs.partition_point(|(sp, _, _)| sp.start < recv_tok.span.start);
        let by_ref = pos.refs[k..].iter().take_while(|(sp, _, _)| sp.start == recv_tok.span.start).find(|(sp, _, _)| sp.end == recv_tok.span.end).and_then(|(_, rid, seg)| a.resolve_ref(o.unit, *rid, *seg as usize));
        // `cg_t cg = new(x);`: the receiver is the declaration itself.
        let var = by_ref.or_else(|| u.parsed.decls.iter().position(|d| d.name_span == recv_tok.span).map(|i| DeclRef { unit: o.unit, decl: i as DeclId }))?;
        let cg = if a.decl(var).kind == DeclKind::Covergroup { var } else { a.type_of(var).filter(|t| a.decl(*t).kind == DeclKind::Covergroup)? };
        let (items, _) = list(&toks, open);
        let (slot, &(first, _)) = items.iter().enumerate().find(|(_, (f, e))| *f <= at && at < *e)?;
        let cu = a.unit(cg.unit);
        let formals: Vec<DeclId> = cu.parsed.decl(cg.decl).children.iter().copied().filter(|c| {
            let d = cu.parsed.decl(*c);
            d.kind == DeclKind::Argument && d.has(dflags::SAMPLE_ARG) == sample
        }).collect();
        let formal = if toks[first].is_punct(".") {
            let n = toks.get(first + 1)?.text.clone();
            formals.into_iter().find(|f| cu.parsed.decl(*f).name == n)?
        } else {
            *formals.get(slot)?
        };
        let formal = DeclRef { unit: cg.unit, decl: formal };
        let children = self.classify(formal, depth + 1);
        if children.is_empty() {
            return None;
        }
        self.hits += 1;
        let fname = a.decl(formal).name.to_string();
        let cg_name = a.decl(cg).name.to_string();
        let (uri, line, character) = self.location(callee.span);
        let call = if sample { "sample" } else { "new" };
        Some(CheckHit {
            kind: "argument",
            group: "connections",
            form: None,
            roles: vec!["argument"],
            label: Some(format!("{fname} of {cg_name}.{call}()")),
            detail: self.text(toks[recv].span, toks.get(list(&toks, open).1.saturating_sub(1)).map_or(callee.span, |t| t.span)),
            uri,
            line,
            character,
            children,
            notes: Vec::new(),
            key: Some((callee.span, o.span, 0)),
        })
    }
}

/// The design element (module, interface, program, checker) declaration `id` is in.
pub(crate) fn definition_of_pub(p: &dvi_syntax::ast::ParsedUnit, id: DeclId) -> Option<DeclId> {
    definition_of(p, id)
}

/// The design element (module, interface, program, checker) declaration `id` is in.
fn definition_of(p: &dvi_syntax::ast::ParsedUnit, id: DeclId) -> Option<DeclId> {
    let mut d = Some(id);
    let mut steps = 0;
    while let Some(x) = d {
        let decl = p.decl(x);
        if decl.kind.is_definition() {
            return Some(x);
        }
        steps += 1;
        if steps > 1000 {
            return None;
        }
        d = decl.parent;
    }
    None
}

/// The port slot `slot` (tokens `first..end` of a connection list) binds, and the token range
/// of its expression: `.p(expr)`, `.p` (the expression is `p`), or a positional item (the
/// definition's port order).
fn slot_port(a: &Analysis, toks: &[Token], def: DeclRef, slot: usize, first: usize, end: usize) -> Option<(String, (usize, usize))> {
    if toks[first].is_punct(".") {
        let n = toks.get(first + 1).filter(|t| t.kind == TokKind::Ident)?;
        if end == first + 2 {
            return Some((n.text.to_string(), (first + 1, end)));
        }
        if !toks.get(first + 2).is_some_and(|t| t.is_punct("(")) || !toks[end - 1].is_punct(")") {
            return None;
        }
        return Some((n.text.to_string(), (first + 3, end - 1)));
    }
    if toks[first].is_punct(".*") {
        return None;
    }
    let ports = header_ports(a, def)?;
    let p = ports.get(slot).filter(|p| !p.is_empty())?;
    Some((p.clone(), (first, end)))
}

/// The port names of a design element's header, in order (non-ANSI `module c(a, b);` too):
/// each slot's last name outside brackets and before a default value; an empty name for a
/// slot that is not a plain port (`.p(expr)`, `{a, b}`).
fn header_ports(a: &Analysis, def: DeclRef) -> Option<Vec<String>> {
    let d = a.decl(def);
    let toks = a.file_tokens(d.name_span.file)?;
    let k = toks.partition_point(|t| t.span.start < d.name_span.start);
    if !toks.get(k).is_some_and(|t| t.span == d.name_span) {
        return None;
    }
    let mut j = k + 1;
    // `import pkg::*;` and `#(...)` before the ports.
    while toks.get(j).is_some_and(|t| t.is_kw("import")) {
        while toks.get(j).is_some_and(|t| !t.is_punct(";")) {
            j += 1;
        }
        j += 1;
    }
    if toks.get(j).is_some_and(|t| t.is_punct("#")) && toks.get(j + 1).is_some_and(|t| t.is_punct("(")) {
        j = super::hints::skip(&toks, j + 1);
    }
    if !toks.get(j).is_some_and(|t| t.is_punct("(")) {
        return None;
    }
    let (items, _) = list(&toks, j);
    // A macro may add ports: their positions are not known.
    if items.iter().any(|&(f, e)| toks[f..e].iter().any(|t| t.kind == TokKind::Directive)) {
        return None;
    }
    Some(
        items
            .iter()
            .map(|&(f, e)| {
                let item = &toks[f..e];
                if item.first().is_some_and(|t| t.is_punct(".") || t.is_punct("{")) {
                    return String::new();
                }
                let mut depth = 0i32;
                let mut name = String::new();
                for t in item {
                    if t.is_punct("[") {
                        depth += 1;
                    } else if t.is_punct("]") {
                        depth -= 1;
                    } else if t.is_punct("=") && depth == 0 {
                        break;
                    } else if depth == 0 && t.kind == TokKind::Ident {
                        name = t.text.to_string();
                    }
                }
                name
            })
            .collect(),
    )
}

/// Whether token `i` is inside brackets in `range` (an index: `bus[idx]` for `idx`).
fn inside_brackets(toks: &[Token], range: (usize, usize), i: usize) -> bool {
    if i < range.0 || range.1 > toks.len() {
        return false;
    }
    let mut depth = 0i32;
    for t in &toks[range.0..i.min(range.1)] {
        if t.is_punct("[") {
            depth += 1;
        } else if t.is_punct("]") {
            depth -= 1;
        }
    }
    depth > 0
}

/// Whether an expression is a name with selects only (`sig`, `sig[3]`, `bus[7:0]`).
fn plain_expr(e: &[Token]) -> bool {
    if e.is_empty() {
        return true;
    }
    if e[0].kind != TokKind::Ident {
        return false;
    }
    let mut depth = 0i32;
    for t in &e[1..] {
        if t.is_punct("[") {
            depth += 1;
        } else if t.is_punct("]") {
            depth -= 1;
        } else if depth == 0 {
            return false;
        }
    }
    true
}

/// The `(` that token `i` is inside, within its statement.
pub(crate) fn enclosing_open(toks: &[Token], i: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut k = i;
    while k > 0 {
        k -= 1;
        let t = &toks[k];
        if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
            depth += 1;
        } else if t.is_punct("(") || t.is_punct("[") || t.is_punct("{") {
            if depth == 0 {
                return t.is_punct("(").then_some(k);
            }
            depth -= 1;
        } else if t.is_punct(";") || i - k > 2000 {
            return None;
        }
    }
    None
}
