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
}

/// One file as one unit compiles it.
pub struct FileFacts {
    pub file: FileId,
    /// The file's tokens as written, without the code `ifdef` leaves out in this unit.
    pub tokens: Vec<Token>,
    /// Functions, tasks and processes written in the file.
    pub bodies: Vec<Body>,
    pub waivers: Waivers,
    /// The unit includes the file more than once (each time it may read differently): only
    /// rules about its text apply.
    pub repeated: bool,
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

impl FactCache {
    pub fn file(&self, a: &Analysis, unit: UnitId, file: FileId) -> Arc<FileFacts> {
        if let Some(f) = self.files.lock().unwrap().get(&(unit, file)) {
            return f.clone();
        }
        let f = Arc::new(file_facts(a, unit, file));
        self.files.lock().unwrap().entry((unit, file)).or_insert(f).clone()
    }

    pub fn class(&self, a: &Analysis, c: DeclRef) -> Arc<ClassFacts> {
        let key = (c.unit, c.decl);
        if let Some(f) = self.classes.lock().unwrap().get(&key) {
            return f.clone();
        }
        let f = Arc::new(class_facts(a, c));
        self.classes.lock().unwrap().entry(key).or_insert(f).clone()
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
    let mut off: Vec<dvi_syntax::Span> = u.inactive.iter().filter(|s| s.file == file).copied().collect();
    off.sort_by_key(|s| s.start);
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
    let waivers = waivers::scan(a, unit, file, &tokens);
    let repeated = u.includes.iter().filter(|i| i.entered && i.resolved == Some(file)).count() > 1;
    FileFacts { file, tokens, bodies, waivers, repeated }
}

fn shift(s: Simple, by: usize) -> Simple {
    match s {
        Simple::Expr(a, b) => Simple::Expr(a + by, b + by),
        Simple::Assert(a, b) => Simple::Assert(a + by, b + by),
        Simple::Macro(a, b) => Simple::Macro(a + by, b + by),
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
