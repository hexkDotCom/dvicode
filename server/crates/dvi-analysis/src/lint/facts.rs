//! The facts lint rules share, computed once per snapshot when a rule first asks: a file's
//! active tokens and procedural bodies with their simple statements, and each class's ancestry
//! and UVM role. A fact that cannot be established is `Err(reason)`, never a guess.

use super::waivers::{self, Waivers};
use crate::ide::stmt::{self, Simple};
use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, DeclId, DeclKind};
use dvi_syntax::{FileId, TokKind, Token};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::{Arc, Mutex};

/// Facts of one snapshot (see [`Analysis::lint_facts`]).
#[derive(Default)]
pub struct FactCache {
    files: Mutex<FxHashMap<(UnitId, FileId), Arc<FileFacts>>>,
    classes: Mutex<FxHashMap<(UnitId, DeclId), Arc<ClassFacts>>>,
    uvm_files: Mutex<Option<Arc<FxHashSet<FileId>>>>,
    /// Each unit's macro calls by file: (start, end) by start, with the largest end so far.
    macro_calls: Mutex<FxHashMap<UnitId, Arc<FxHashMap<FileId, Vec<(u32, u32, u32)>>>>>,
    /// Files whose path is in the project folder but whose real place is not (a link), and the
    /// project folder's real place.
    linked: Mutex<(Option<Option<std::path::PathBuf>>, FxHashMap<FileId, bool>)>,
}

/// One file as one unit compiles it.
pub struct FileFacts {
    pub file: FileId,
    /// The file's tokens as written, without the code `ifdef` leaves out in this unit.
    pub tokens: Vec<Token>,
    /// Functions, tasks and processes written in the file.
    pub bodies: Vec<Body>,
    pub waivers: Waivers,
    /// The token ranges of `` `define `` directives, in order.
    pub defines: Vec<(usize, usize)>,
    /// The comments of the file, in order.
    pub comments: Vec<waivers::Comment>,
    /// The unit includes the file more than once (each time it may read differently): only
    /// rules about its text apply.
    pub repeated: bool,
    /// The parser reported syntax errors in the file.
    pub syntax_errors: bool,
    /// The file holds encrypted code.
    pub encrypted: bool,
    /// The classes written in the file.
    pub classes: ClassIndex,
}

/// The classes of a file, by start, each with the class around it: finds the innermost class
/// around a place without scanning them all.
#[derive(Default)]
pub struct ClassIndex {
    /// (start, end, class, index of the class around it).
    items: Vec<(u32, u32, DeclId, Option<usize>)>,
}

impl ClassIndex {
    fn new(u: &crate::world::Unit, file: FileId) -> ClassIndex {
        let mut items: Vec<(u32, u32, DeclId, Option<usize>)> =
            u.parsed.decls.iter().enumerate().filter(|(_, d)| d.kind == DeclKind::Class && d.span.file == file).map(|(i, d)| (d.span.start, d.span.end, i as DeclId, None)).collect();
        items.sort_by_key(|c| (c.0, std::cmp::Reverse(c.1)));
        let mut open: Vec<usize> = Vec::new();
        for i in 0..items.len() {
            while open.last().is_some_and(|&j| items[j].1 <= items[i].0) {
                open.pop();
            }
            items[i].3 = open.last().copied();
            open.push(i);
        }
        ClassIndex { items }
    }

    /// The innermost class around `s`.
    pub fn innermost(&self, s: dvi_syntax::Span) -> Option<DeclId> {
        // The last class to start before it, then the classes around that one.
        let mut cur = self.items.partition_point(|c| c.0 <= s.start).checked_sub(1);
        while let Some(i) = cur {
            let c = &self.items[i];
            if s.end <= c.1 {
                return Some(c.2);
            }
            cur = c.3;
        }
        None
    }
}

/// A procedural body: a function, a task, or an `initial`/`always*`/`final` process.
pub struct Body {
    /// The function or task (`None` for a process).
    pub decl: Option<DeclId>,
    /// Its tokens: `tokens[start..end]`.
    pub start: usize,
    pub end: usize,
    /// Its simple statements (indexes into `tokens`); `None` when the walker could not follow
    /// the body.
    pub stmts: Option<Vec<Simple>>,
}

/// What a class is, as far as lint needs.
#[derive(Clone, Debug)]
pub struct ClassFacts {
    /// The class and its bases, nearest first; `Err` when a base does not resolve.
    pub ancestry: Result<Vec<DeclRef>, &'static str>,
    pub role: Result<UvmRole, &'static str>,
    /// Has a `#(...)` parameter list.
    pub parameterized: bool,
    /// `virtual class`.
    pub is_virtual: bool,
}

/// The UVM base a class derives from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UvmRole {
    /// Not a UVM class.
    None,
    Object,
    Component,
    /// A TLM port, export or imp (UVM gives it a component proxy; it is not a component).
    Port,
}

/// Bases followed at most (a deeper chain is reported unknown, not walked).
const MAX_ANCESTRY: usize = 1_000;

impl FileFacts {
    /// Whether token `k` is part of a `` `define `` (its name or its body).
    pub fn in_define(&self, k: usize) -> bool {
        let i = self.defines.partition_point(|(s, _)| *s <= k);
        i > 0 && k < self.defines[i - 1].1
    }
}

impl FactCache {
    pub fn file(&self, a: &Analysis, unit: UnitId, file: FileId) -> Arc<FileFacts> {
        if let Some(f) = self.files.lock().unwrap().get(&(unit, file)) {
            return f.clone();
        }
        let f = Arc::new(file_facts(a, unit, file));
        self.files.lock().unwrap().entry((unit, file)).or_insert(f).clone()
    }

    /// `unit`'s macro calls, by file and start (see [`in_macro_call`]).
    fn macro_calls(&self, a: &Analysis, unit: UnitId) -> Arc<FxHashMap<FileId, Vec<(u32, u32, u32)>>> {
        if let Some(m) = self.macro_calls.lock().unwrap().get(&unit) {
            return m.clone();
        }
        let mut by: FxHashMap<FileId, Vec<(u32, u32, u32)>> = FxHashMap::default();
        for m in &a.unit(unit).macro_uses {
            by.entry(m.call_span.file).or_default().push((m.call_span.start, m.call_span.end, 0));
        }
        for v in by.values_mut() {
            v.sort_unstable();
            let mut reach = 0;
            for c in v.iter_mut() {
                reach = reach.max(c.1);
                c.2 = reach;
            }
        }
        let m = Arc::new(by);
        self.macro_calls.lock().unwrap().entry(unit).or_insert(m).clone()
    }

    pub fn class(&self, a: &Analysis, c: DeclRef) -> Arc<ClassFacts> {
        let key = (c.unit, c.decl);
        if let Some(f) = self.classes.lock().unwrap().get(&key) {
            return f.clone();
        }
        let f = Arc::new(class_facts(a, c));
        self.classes.lock().unwrap().entry(key).or_insert(f).clone()
    }

    /// Whether `file` is in the project folder by its path but elsewhere in fact: a link (to a
    /// file, or in a linked folder) that leaves the project.
    pub fn linked_out(&self, a: &Analysis, file: FileId) -> bool {
        if let Some(x) = self.linked.lock().unwrap().1.get(&file) {
            return *x;
        }
        let root = {
            let mut l = self.linked.lock().unwrap();
            l.0.get_or_insert_with(|| dvi_project::fsx::canonicalize(&a.setup.root).ok()).clone()
        };
        let path = a.path(file);
        let out = match (root, path.starts_with(&a.setup.root)) {
            (Some(root), true) => dvi_project::fsx::canonicalize(&path).is_ok_and(|p| !p.starts_with(&root)),
            _ => false,
        };
        self.linked.lock().unwrap().1.insert(file, out);
        out
    }

    /// The files of the UVM library: the file that declares `uvm_pkg` and every file it
    /// includes, directly or not.
    pub fn uvm_files(&self, a: &Analysis) -> Arc<FxHashSet<FileId>> {
        if let Some(s) = self.uvm_files.lock().unwrap().as_ref() {
            return s.clone();
        }
        let mut s = FxHashSet::default();
        for p in a.global.packages.get("uvm_pkg").into_iter().flatten() {
            let u = a.unit(p.unit);
            let pf = a.decl(*p).name_span.file;
            s.insert(pf);
            // An include is in the library when the file holding it is.
            let mut lib = vec![false; u.includes.len()];
            for (i, inc) in u.includes.iter().enumerate() {
                let holder_in = match inc.from {
                    None => u.root == pf,
                    Some(f) => lib.get(f as usize).copied().unwrap_or(false) || u.includes.get(f as usize).and_then(|x| x.resolved) == Some(pf),
                };
                lib[i] = holder_in;
                if holder_in {
                    if let Some(r) = inc.resolved {
                        s.insert(r);
                    }
                }
            }
        }
        let s = Arc::new(s);
        *self.uvm_files.lock().unwrap() = Some(s.clone());
        s
    }
}

fn file_facts(a: &Analysis, unit: UnitId, file: FileId) -> FileFacts {
    let u = a.unit(unit);
    let raw = a.file_tokens(file).unwrap_or_default();
    let off = inactive(a, unit, file);
    let mut k = 0;
    let tokens: Vec<Token> = raw
        .iter()
        .filter(|t| {
            if t.kind == TokKind::Eof {
                return false;
            }
            while k < off.len() && off[k].end <= t.span.start {
                k += 1;
            }
            !(k < off.len() && off[k].start <= t.span.start)
        })
        .cloned()
        .collect();
    let at = |pos: u32| tokens.partition_point(|t| t.span.start < pos);
    let mut bodies = Vec::new();
    let p = &u.parsed;
    for (i, d) in p.decls.iter().enumerate() {
        if !matches!(d.kind, DeclKind::Function | DeclKind::Task) || d.span.file != file || d.has(dflags::FROM_MACRO) || d.has(dflags::PROTOTYPE) || d.has(dflags::DPI) {
            continue;
        }
        let (start, end) = (at(d.span.start), at(d.span.end));
        let stmts = stmt::method_statements(&tokens[start..end]).map(|v| v.into_iter().map(|s| shift(s, start)).collect());
        bodies.push(Body { decl: Some(i as DeclId), start, end, stmts });
    }
    for (_, first, last) in &p.processes {
        if first.file != file || last.file != file {
            continue;
        }
        let (start, end) = (at(first.start), at(last.end));
        if start >= end {
            continue;
        }
        // After the keyword (`initial`, `always_ff`, ...).
        let stmts = stmt::statements_from(&tokens[start..end], 1).filter(|(_, e)| *e == end - start).map(|(v, _)| v.into_iter().map(|s| shift(s, start)).collect());
        bodies.push(Body { decl: None, start, end, stmts });
    }
    bodies.sort_by_key(|b| b.start);
    // A `define runs to the end of its line, continued lines included.
    let mut defines = Vec::new();
    let mut k = 0;
    while k < tokens.len() {
        if tokens[k].kind == TokKind::Directive && tokens[k].text == "define" {
            let mut e = k + 1;
            while e < tokens.len() && (!tokens[e].nl_before() || tokens[e].flags & dvi_syntax::token::flags::CONT_BEFORE != 0) {
                e += 1;
            }
            defines.push((k, e));
            k = e;
        } else {
            k += 1;
        }
    }
    let comments = waivers::comments(a, unit, file, &tokens);
    let waivers = waivers::scan(a, file, &tokens, &comments);
    // Included more than once, and not guarded (a guarded header's later copies are empty).
    let repeated = u.includes.iter().filter(|i| i.entered && i.resolved == Some(file)).count() > 1 && !guarded(&raw);
    let syntax_errors = u.parsed.diags.iter().any(|d| d.code == "SYNTAX_ERROR" && d.span.file == file);
    let encrypted = tokens.iter().enumerate().any(|(i, t)| {
        t.kind == TokKind::Directive && (t.text == "protected" || t.text == "pragma" && tokens.get(i + 1).is_some_and(|n| n.text == "protect"))
    }) || u.parsed.decls.iter().any(|d| d.span.file == file && d.has(dflags::ENCRYPTED));
    let classes = ClassIndex::new(u, file);
    FileFacts { file, tokens, bodies, waivers, defines, comments, repeated, syntax_errors, encrypted, classes }
}

/// What `ifdef` leaves out of `file` in `unit`, in order: what is inactive in every inclusion of
/// the file (the later inclusions of a guarded header are empty, its first is not).
pub fn inactive(a: &Analysis, unit: UnitId, file: FileId) -> Vec<dvi_syntax::Span> {
    let u = a.unit(unit);
    // Each inclusion, and the unit's own visit when the file is its root.
    let times = (u.includes.iter().filter(|i| i.entered && i.resolved == Some(file)).count() + usize::from(u.root == file)).max(1) as i32;
    // Each inclusion's ranges do not overlap: count how many inclusions leave each place out.
    let mut ev: Vec<(u32, i32)> = u.inactive.iter().filter(|s| s.file == file && s.start < s.end).flat_map(|s| [(s.start, 1), (s.end, -1)]).collect();
    ev.sort_unstable();
    let (mut out, mut depth, mut from) = (Vec::new(), 0, 0);
    for (at, d) in ev {
        let was = depth >= times;
        depth += d;
        if !was && depth >= times {
            from = at;
        } else if was && depth < times && from < at {
            out.push(dvi_syntax::Span::new(file, from, at));
        }
    }
    out
}

/// Whether a file's text is wrapped in an include guard: `ifndef G / `define G first, `endif last.
pub fn guarded(t: &[Token]) -> bool {
    let t: Vec<&Token> = t.iter().filter(|x| x.kind != TokKind::Eof).collect();
    let [a, b, c, d, ..] = t.as_slice() else { return false };
    a.kind == TokKind::Directive && a.text == "ifndef" && c.kind == TokKind::Directive && c.text == "define" && b.text == d.text && t.last().is_some_and(|e| e.kind == TokKind::Directive && e.text == "endif")
}

fn shift(s: Simple, by: usize) -> Simple {
    match s {
        Simple::Expr(a, b) => Simple::Expr(a + by, b + by),
        Simple::Assert(a, b) => Simple::Assert(a + by, b + by),
        Simple::Macro(a, b) => Simple::Macro(a + by, b + by),
        Simple::Delay(a, b) => Simple::Delay(a + by, b + by),
    }
}

fn class_facts(a: &Analysis, c: DeclRef) -> ClassFacts {
    let d = a.decl(c);
    let u = a.unit(c.unit);
    let parameterized = d.children.iter().any(|ch| u.parsed.decl(*ch).has(dflags::PARAM_PORT));
    let is_virtual = d.has(dflags::VIRTUAL);
    let mut chain = vec![c];
    let mut seen: FxHashSet<(u32, u32)> = FxHashSet::default();
    seen.insert(a.decl_key(c));
    let mut role: Option<UvmRole> = None;
    let mut cur = c;
    let ancestry = loop {
        if role.is_none() && a.in_uvm_library(cur) {
            role = match a.decl(cur).name.as_str() {
                "uvm_component" => Some(UvmRole::Component),
                "uvm_port_base" => Some(UvmRole::Port),
                "uvm_object" => Some(UvmRole::Object),
                _ => None,
            };
            // A port's base is its interface type parameter: stop at the port.
            if role == Some(UvmRole::Port) {
                break Ok(chain);
            }
        }
        if a.decl(cur).extends.is_none() {
            break Ok(chain);
        }
        let Some(b) = a.class_base(cur) else { break Err("unresolved base class") };
        if !seen.insert(a.decl_key(b)) {
            break Err("cyclic base classes");
        }
        if chain.len() >= MAX_ANCESTRY {
            break Err("base classes nested too deeply");
        }
        chain.push(b);
        cur = b;
    };
    let role = match (&ancestry, role) {
        (_, Some(r)) => Ok(r),
        (Ok(_), None) => Ok(UvmRole::None),
        (Err(e), None) => Err(*e),
    };
    ClassFacts { ancestry, role, parameterized, is_virtual }
}

/// Whether the declaration at `name` is made by a macro: its name is written as an argument of
/// a macro call (`` `MY_PHASE(build_phase) ``), so its header is the macro's.
pub fn in_macro_call(a: &Analysis, unit: UnitId, name: dvi_syntax::Span) -> bool {
    let calls = a.lint_facts.macro_calls(a, unit);
    let Some(v) = calls.get(&name.file) else { return false };
    // Some call starting at or before it reaches past its end.
    let i = v.partition_point(|c| c.0 <= name.start);
    i > 0 && v[i - 1].2 >= name.end
}

/// The class a member is declared in (its parent, or the class of an out-of-block method).
pub fn member_class(a: &Analysis, r: DeclRef) -> Option<DeclRef> {
    let d = a.decl(r);
    match d.parent {
        Some(p) if matches!(a.unit(r.unit).parsed.decl(p).kind, DeclKind::Class | DeclKind::InterfaceClass) => Some(DeclRef { unit: r.unit, decl: p }),
        _ => a.owner_class(r),
    }
}
