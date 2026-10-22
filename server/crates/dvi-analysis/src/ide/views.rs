//! Data for the custom views: compiled files, design hierarchy, including files, status.

use super::filter::Filter;
use super::stmt::{self, FrameKind};
use crate::world::{Analysis, DeclRef, UnitKind};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, RefKind};
use dvi_syntax::{FileId, SourceProvider, Span, TokKind, Token};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Serialize, Debug, Clone)]
pub struct FileNode {
    pub path: String,
    pub uri: String,
    /// `top`, `include`, `library` or `non-top`.
    pub kind: String,
    pub children: Vec<FileNode>,
    pub problems: usize,
    /// True when the children were cut by a limit.
    pub truncated: bool,
    /// The same file at the top more than once (compiled in two invocations): which one (2, 3...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dup: Option<u32>,
}

#[derive(Serialize, Debug, Clone)]
pub struct HierNode {
    /// Instance name (the top's module name for tops).
    pub name: String,
    /// Module/interface/program name.
    pub definition: String,
    pub kind: String,
    pub uri: Option<String>,
    pub line: u32,
    pub character: u32,
    /// Location of the definition.
    pub def_uri: Option<String>,
    pub def_line: u32,
    pub children: Vec<HierNode>,
    /// True when the children were cut by the depth/size limit.
    pub truncated: bool,
    /// When siblings share the name: which one this is (2, 3...). The views name it `name#2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dup: Option<u32>,
}

/// A component of the UVM verification hierarchy.
#[derive(Serialize, Debug, Clone)]
pub struct VerifNode {
    /// Instance name: the string given to `create`/`new` (`uvm_test_top` for the test), or
    /// the variable when the name is computed.
    pub name: String,
    /// Hierarchical path, e.g. `uvm_test_top.env.agent0`.
    pub path: String,
    /// Class of the component.
    pub class: String,
    /// `test`, `env`, `agent`, `driver`, `monitor`, `sequencer`, `scoreboard`, `subscriber`,
    /// `component`, `port` or `unresolved`.
    pub kind: String,
    /// Where the component is created (the class for the test).
    pub uri: Option<String>,
    pub line: u32,
    pub character: u32,
    /// Location of the class.
    pub def_uri: Option<String>,
    pub def_line: u32,
    pub children: Vec<VerifNode>,
    /// True when the children were cut by the depth/size limit.
    pub truncated: bool,
    /// When siblings share the name: which one this is (2, 3...). The views name it `name#2`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dup: Option<u32>,
    /// The ways the component is created, each the conditions around it, outermost first
    /// (`?`: not known); empty when one way is unconditional.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Vec<String>>,
    /// Created in a loop: its iterations (`4`, `up to 4`: the loop can stop early), or `?`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<String>,
    /// The loops it is created in, outermost first (`for (int i = 0; i < 4; i++)`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<String>,
    /// Other classes created instead, under other conditions
    /// (`if (host) driver = A::...; else driver = B::...;`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<Alternative>,
    /// TLM ports of the component left out (`TreeQuery::ports` off).
    #[serde(skip_serializing_if = "is_zero")]
    pub ports_hidden: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A class created instead of a node's, under other conditions.
#[derive(Serialize, Debug, Clone)]
pub struct Alternative {
    pub class: String,
    /// Where it is created.
    pub uri: Option<String>,
    pub line: u32,
    pub character: u32,
    /// Location of the class.
    pub def_uri: Option<String>,
    pub def_line: u32,
    /// As [`VerifNode::conditions`].
    pub conditions: Vec<Vec<String>>,
}

#[derive(Serialize, Debug, Clone)]
pub struct VerifHierarchy {
    /// The test shown (`+UVM_TESTNAME` or the requested one); `None` when all tests are shown.
    pub test: Option<String>,
    /// `+UVM_TESTNAME` of the build configuration.
    pub configured: Option<String>,
    /// Every non-virtual class derived from `uvm_test`.
    pub tests: Vec<String>,
    pub roots: Vec<VerifNode>,
    /// Without tests: why there may be none (classes whose UVM base class does not resolve).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// What a view asks for besides its root: a filter, a path to show, and limits.
#[derive(Clone, Debug)]
pub struct TreeQuery {
    pub filter: Option<Filter>,
    /// Instead of a filter: the nodes of one design element, instance or class (to find
    /// every place the editor's position occurs in a view).
    pub target: Option<Target>,
    /// Names from a root to a node that is returned even beyond the node limit (with its
    /// ancestors): for the Design Hierarchy the top's name, then instance names; for the
    /// Verification Hierarchy the test class, then component names.
    pub reveal: Vec<String>,
    /// Nodes returned at most (not counting roots and the revealed path).
    pub max_nodes: usize,
    /// With a filter: matching nodes returned at most.
    pub max_matches: usize,
    /// Nodes looked at at most.
    pub max_visits: usize,
    /// Verification Hierarchy: TLM ports are shown.
    pub ports: bool,
}

impl TreeQuery {
    pub fn new(max_nodes: usize) -> TreeQuery {
        TreeQuery { filter: None, target: None, reveal: Vec::new(), max_nodes, max_matches: 5_000, max_visits: 2_000_000, ports: true }
    }
}

/// What `dvicode/locate` found, as nodes of a view: instances of `definition` (or, with
/// `instance`, the instance of that name in `definition`), or components of `class`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct Target {
    pub definition: Option<String>,
    pub instance: Option<String>,
    pub class: Option<String>,
}

/// How complete a tree is.
#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeMeta {
    /// Why nodes are missing: `nodes` (the node limit; nodes with `truncated` have more
    /// children), `matches` (the match limit) or `visits` (the search stopped); `None` when
    /// the tree is complete.
    pub incomplete: Option<&'static str>,
    /// With a filter: the matching nodes returned.
    pub matches: usize,
}

/// The enclosing definitions of a position, for following the editor in the views.
#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Located {
    /// The module, interface or program around the position.
    pub design: Option<NamedDef>,
    /// The instance whose instantiation the position is in (`fifo a(), b();` on `b`).
    pub instance: Option<String>,
    /// The class around the position.
    pub class: Option<NamedDef>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct NamedDef {
    pub name: String,
    pub uri: Option<String>,
}

struct Walk<'q> {
    q: &'q TreeQuery,
    nodes_left: usize,
    visits_left: usize,
    meta: TreeMeta,
}

impl Walk<'_> {
    fn new(q: &TreeQuery) -> Walk<'_> {
        Walk { q, nodes_left: q.max_nodes, visits_left: q.max_visits, meta: TreeMeta::default() }
    }

    /// The match or visit limit was reached: nothing more is looked at.
    fn stopped(&self) -> bool {
        matches!(self.meta.incomplete, Some("matches" | "visits"))
    }

    /// Counts a node looked at; false at the visit limit.
    fn visit(&mut self) -> bool {
        if self.visits_left == 0 {
            self.meta.incomplete = Some("visits");
            return false;
        }
        self.visits_left -= 1;
        true
    }

    /// Whether a node with these texts (and their path separator) matches the filter; always
    /// true without one.
    fn matched(&mut self, texts: &[(&str, char)]) -> bool {
        let Some(f) = self.q.filter.as_ref().filter(|_| self.q.target.is_none()) else { return self.q.target.is_none() };
        let m = texts.iter().any(|(t, sep)| f.matches(t, *sep));
        self.count(m)
    }

    /// Whether a node is the target: an instance `name` of `definition` in `parent`, or a
    /// component of class `class`.
    fn hit(&mut self, name: &str, definition: &str, parent: &str, class: &str) -> bool {
        let Some(t) = &self.q.target else { return false };
        let m = match (&t.class, &t.definition, &t.instance) {
            (Some(c), ..) => c == class,
            (None, Some(d), Some(i)) => i == name && d == parent,
            (None, Some(d), None) => d == definition,
            _ => false,
        };
        self.count(m)
    }

    fn count(&mut self, m: bool) -> bool {
        if m {
            self.meta.matches += 1;
            if self.meta.matches >= self.q.max_matches {
                self.meta.incomplete = Some("matches");
            }
        }
        m
    }

    /// Before a node is added: whether there is room for it. `pinned`: on the revealed path,
    /// added anyway. `Some(true)` when a slot was taken (given back if the node is dropped).
    fn reserve(&mut self, pinned: bool) -> Option<bool> {
        if self.nodes_left > 0 {
            self.nodes_left -= 1;
            return Some(true);
        }
        if self.meta.incomplete.is_none() {
            self.meta.incomplete = Some("nodes");
        }
        pinned.then_some(false)
    }
}

/// The rest of a revealed path below a node called `name`, when the path goes through it.
fn pin_below<'a>(pin: Option<&'a [String]>, name: &str) -> Option<&'a [String]> {
    pin.and_then(|p| p.split_first()).filter(|(first, _)| first.as_str() == name).map(|(_, rest)| rest)
}

/// A `x = T::type_id::create("name", this)` or `x = new("name", this)` in a method body.
#[derive(Debug, Clone)]
struct Creation {
    /// Left-hand side text (`agents[i]`).
    lhs: String,
    /// The variable name token of the left-hand side.
    lhs_head: Span,
    /// The `T` of `T::type_id::create`; `None` for `new`.
    ty: Option<Span>,
    ty_text: String,
    /// Literal instance name.
    name: Option<String>,
    /// The instance name when it is a variable (an argument of a factory method).
    name_arg: Option<String>,
    /// The parent: `this`, or a variable.
    parent: String,
    /// The created class, when known already (a factory method's creation).
    class: Option<DeclRef>,
}

/// A condition or loop around a creation, from the method it is in or a call leading to it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Guard {
    /// `if (text)`, or its `else` branch (`negated`).
    If { text: String, negated: bool },
    /// A `case` item: `case (e) labels:`.
    Case { text: String },
    /// A loop: its keyword, header tokens (resolved in `unit`), and whether its body can leave
    /// early (`break`, `continue`, `return`).
    Loop { kind: &'static str, header: Vec<Token>, unit: crate::world::UnitId, exits: bool },
    /// The method's statements could not be followed.
    Unknown,
}

/// What a method body does that the hierarchy needs, in source order (argument values and
/// paths are filled in where it is called).
struct Summary {
    events: Vec<Event>,
}

struct Event {
    guards: Vec<Guard>,
    what: What,
}

enum What {
    Create(Creation),
    /// A call of a method of the object (`f()`, `this.f()`), or of the base class's (`super.f()`):
    /// with the arguments given by name (`.n(v)`) or position, and their value when it is a
    /// string literal.
    Call { name: String, sup: bool, args: Vec<(Option<String>, Option<String>)> },
}

/// A component creation reached while building a component, with the guards of each way it
/// is reached (an empty list: unconditionally), and the other creations of the same variable
/// and name that can run instead (`if (host) driver = A...; else driver = B...;`): UVM makes one
/// of them.
#[derive(Debug, Clone)]
struct Made {
    body: DeclRef,
    cr: Creation,
    alts: Vec<Vec<Guard>>,
    others: Vec<Made>,
}

/// Work shared by one request: tokens of files, summaries of method bodies, and a budget of
/// instantiated events.
#[derive(Default)]
struct Scan {
    tokens: RefCell<FxHashMap<FileId, Arc<Vec<Token>>>>,
    summaries: RefCell<FxHashMap<DeclRef, Rc<Summary>>>,
    events: Cell<usize>,
    exhausted: Cell<bool>,
}

/// Events instantiated per request at most.
const MAX_EVENTS: usize = 2_000_000;
/// Nested method calls followed at most.
const MAX_CALL_DEPTH: usize = 8;

impl Scan {
    fn tokens(&self, a: &Analysis, f: FileId) -> Option<Arc<Vec<Token>>> {
        if let Some(t) = self.tokens.borrow().get(&f) {
            return Some(t.clone());
        }
        let t = a.file_tokens(f)?;
        self.tokens.borrow_mut().insert(f, t.clone());
        Some(t)
    }

    /// Counts an instantiated event; false once the budget is spent.
    fn spend(&self) -> bool {
        let n = self.events.get() + 1;
        self.events.set(n);
        if n > MAX_EVENTS {
            self.exhausted.set(true);
        }
        n <= MAX_EVENTS
    }
}

/// Adds a creation, merging it with one of the same variable and name: the same creation
/// reached another way adds that way; a later one at least as unconditional as every earlier
/// way replaces them (a derived class re-creating it); otherwise it is another creation that can
/// run instead.
fn add_made(made: &mut Vec<Made>, m: Made) {
    let g = m.alts[0].clone();
    let same_site = |x: &Made| x.body == m.body && x.cr.lhs_head == m.cr.lhs_head && x.cr.name == m.cr.name;
    for x in made.iter_mut() {
        if x.cr.lhs != m.cr.lhs || !(x.cr.name == m.cr.name || x.cr.name.is_none() || m.cr.name.is_none()) {
            continue;
        }
        if let Some(y) = std::iter::once(&mut *x).find(|y| same_site(y)) {
            if !y.alts.contains(&g) {
                y.alts.push(g);
            }
            return;
        }
        if let Some(y) = x.others.iter_mut().find(|y| same_site(y)) {
            if !y.alts.contains(&g) {
                y.alts.push(g);
            }
            return;
        }
        if x.alts.iter().chain(x.others.iter().flat_map(|o| &o.alts)).all(|a| a.starts_with(&g)) {
            *x = m;
        } else {
            x.others.push(m);
        }
        return;
    }
    made.push(m);
}

/// The conditions of each way a creation is reached (loops left out), as text; empty when one
/// way has none.
fn conditions_text(alts: &[Vec<Guard>]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for a in alts {
        let c: Vec<String> = a
            .iter()
            .filter_map(|g| match g {
                Guard::If { text, negated: false } => Some(short(text)),
                Guard::If { text, negated: true } => Some(format!("!({})", short(text))),
                Guard::Case { text } => Some(format!("case {}", short(text))),
                Guard::Unknown => Some("?".to_string()),
                Guard::Loop { .. } => None,
            })
            .collect();
        if c.is_empty() {
            return Vec::new();
        }
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// A loop guard as text: `for (int i = 0; i < 4; i++)`, `forever`.
fn loop_text(g: &Guard) -> Option<String> {
    let Guard::Loop { kind, header, .. } = g else { return None };
    Some(match *kind {
        "forever" => "forever".to_string(),
        "do" => format!("do ... while ({})", short(&tokens_text(header))),
        k => format!("{k} ({})", short(&tokens_text(header))),
    })
}

/// A condition cut to 120 characters.
fn short(s: &str) -> String {
    match s.char_indices().nth(119) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_string(),
    }
}

/// Nearest UVM base class names and the kind they give a component.
const UVM_KINDS: &[(&str, &str)] = &[
    ("uvm_test", "test"),
    ("uvm_env", "env"),
    ("uvm_agent", "agent"),
    ("uvm_driver", "driver"),
    ("uvm_monitor", "monitor"),
    ("uvm_sequencer", "sequencer"),
    ("uvm_sequencer_base", "sequencer"),
    ("uvm_push_sequencer", "sequencer"),
    ("uvm_scoreboard", "scoreboard"),
    ("uvm_subscriber", "subscriber"),
    ("uvm_port_base", "port"),
    ("uvm_component", "component"),
];

#[derive(Serialize, Debug, Clone)]
pub struct Status {
    pub root: String,
    pub build_file: Option<String>,
    pub invocations: usize,
    pub top_files: usize,
    pub files: usize,
    pub units: usize,
    pub declarations: usize,
    pub macros: usize,
    pub packages: usize,
    pub classes: usize,
    pub modules: usize,
    pub build_ms: u128,
    pub problems_in_build_file: usize,
}

impl Analysis {
    pub fn status(&self) -> Status {
        let classes = self
            .units
            .iter()
            .flatten()
            .map(|u| u.parsed.decls.iter().filter(|d| matches!(d.kind, DeclKind::Class)).count())
            .sum();
        Status {
            root: self.setup.root.display().to_string(),
            build_file: self.setup.config.build_file.as_ref().map(|p| p.display().to_string()),
            invocations: self.setup.config.invocations.len(),
            top_files: self.units.iter().flatten().filter(|u| u.kind == UnitKind::Project).count(),
            files: self.project_files().len(),
            units: self.units.iter().flatten().count(),
            declarations: self.decl_count(),
            macros: self.global.macros.len(),
            packages: self.global.packages.len(),
            classes,
            modules: self.global.definitions.len(),
            build_ms: self.build_ms,
            problems_in_build_file: self.setup.config.problems.len(),
        }
    }

    /// Compiled files in compile order, with their include trees.
    pub fn compiled_files(&self) -> Vec<FileNode> {
        let mut q = TreeQuery::new(usize::MAX);
        q.max_visits = usize::MAX;
        self.compiled_files_tree(&q).0
    }

    /// [`Analysis::compiled_files`] as `q` asks: only the files whose path matches its filter and
    /// the files that include them, within its limits.
    pub fn compiled_files_tree(&self, q: &TreeQuery) -> (Vec<FileNode>, TreeMeta) {
        let mut out = Vec::new();
        let mut w = Walk::new(q);
        let mut seen = FxHashMap::default();
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
            }
            if w.stopped() || w.meta.incomplete == Some("nodes") {
                break;
            }
            // Children per including file.
            let mut by_parent: FxHashMap<FileId, Vec<FileId>> = FxHashMap::default();
            for inc in &u.includes {
                if let Some(r) = inc.resolved {
                    let v = by_parent.entry(inc.span.file).or_default();
                    if !v.contains(&r) {
                        v.push(r);
                    }
                }
            }
            let mut visited = FxHashSet::default();
            let kind = match u.kind {
                UnitKind::AdHoc => "non-top",
                _ if u.invocation < self.setup.config.invocations.len()
                    && self.setup.config.invocations[u.invocation].lib_files.iter().chain(std::iter::empty()).any(|p| *p == self.vfs.path(u.root)) =>
                {
                    "library"
                }
                _ => "top",
            };
            if let Some(mut n) = self.file_walk(u.root, kind, &by_parent, &mut visited, &mut w) {
                n.dup = sibling(&mut seen, &n.uri);
                out.push(n);
            }
        }
        (out, w.meta)
    }

    /// A file and the files it includes, as the walk's query asks; `None` when it is left out.
    fn file_walk(&self, f: FileId, kind: &str, by_parent: &FxHashMap<FileId, Vec<FileId>>, visited: &mut FxHashSet<FileId>, w: &mut Walk) -> Option<FileNode> {
        if w.stopped() || !w.visit() {
            return None;
        }
        let reserved = w.reserve(false)?;
        let path = self.vfs.path(f);
        let shown = path.display().to_string();
        let matched = w.matched(&[(&shown.replace('\\', "/"), '/')]);
        let mut children = Vec::new();
        let mut truncated = false;
        if visited.insert(f) {
            for c in by_parent.get(&f).into_iter().flatten() {
                match self.file_walk(*c, "include", by_parent, visited, w) {
                    Some(n) => children.push(n),
                    None if w.meta.incomplete.is_some() => {
                        truncated = true;
                        break;
                    }
                    None => {}
                }
            }
        }
        if matched || !children.is_empty() {
            Some(FileNode { path: shown, uri: super::url_of(&path).to_string(), kind: kind.into(), children, problems: 0, truncated, dup: None })
        } else {
            if reserved {
                w.nodes_left += 1;
            }
            None
        }
    }

    /// Files that include `file`, with the line of the `` `include ``.
    pub fn including_files(&self, file: FileId) -> Vec<lsp_types::Location> {
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        for u in self.units.iter().flatten() {
            for inc in &u.includes {
                if inc.resolved == Some(file) && seen.insert(inc.span) {
                    out.extend(self.to_location(inc.span));
                }
            }
        }
        out
    }

    /// Design elements that are never instantiated (candidate tops), or the `-top` ones.
    pub fn design_tops(&self) -> Vec<DeclRef> {
        let configured = self.setup.config.tops();
        if !configured.is_empty() {
            let v: Vec<DeclRef> = configured.iter().filter_map(|t| self.global.definitions.get(t.as_str()).and_then(|v| v.first().copied())).collect();
            if !v.is_empty() {
                return v;
            }
        }
        let mut instantiated: FxHashSet<(u32, u32)> = FxHashSet::default();
        for u in self.units.iter().flatten() {
            for (i, d) in u.parsed.decls.iter().enumerate() {
                if d.kind == DeclKind::Instance {
                    if let Some(m) = self.type_of(DeclRef { unit: u.id, decl: i as DeclId }) {
                        instantiated.insert(self.decl_key(m));
                    }
                }
            }
            // `bind` and interface ports also count as uses.
            for r in &u.parsed.refs {
                if r.kind == RefKind::Definition {
                    if let Some(v) = self.global.definitions.get(&r.segs[0].name) {
                        for x in v {
                            instantiated.insert(self.decl_key(*x));
                        }
                    }
                }
            }
        }
        let mut tops: Vec<DeclRef> = self
            .global
            .definitions
            .values()
            .filter_map(|v| v.first().copied())
            .filter(|r| matches!(self.decl(*r).kind, DeclKind::Module | DeclKind::Program) && !instantiated.contains(&self.decl_key(*r)))
            // Files opened outside the build do not change the design.
            .filter(|r| self.unit(r.unit).kind == UnitKind::Project)
            .collect();
        tops.sort_by(|a, b| self.decl(*a).name.cmp(&self.decl(*b).name));
        tops
    }

    pub fn design_hierarchy(&self, top: Option<&str>, max_nodes: usize) -> Vec<HierNode> {
        self.design_tree(top, &TreeQuery::new(max_nodes)).0
    }

    /// The instance tree under `top` (or under each candidate top), as `q` asks.
    pub fn design_tree(&self, top: Option<&str>, q: &TreeQuery) -> (Vec<HierNode>, TreeMeta) {
        let tops: Vec<DeclRef> = match top {
            Some(t) => self.global.definitions.get(t).and_then(|v| v.first().copied()).into_iter().collect(),
            None => self.design_tops(),
        };
        let mut w = Walk::new(q);
        let mut roots = Vec::new();
        for t in tops {
            if w.stopped() && !q.reveal.first().is_some_and(|r| *r == self.decl(t).name) {
                break;
            }
            let d = self.decl(t);
            let loc = self.decl_location(t);
            let name = d.name.to_string();
            let pin = pin_below(Some(q.reveal.as_slice()), &name);
            let matched = if q.target.is_some() { w.hit(&name, &name, "", "") } else { w.matched(&[(&name, '.')]) };
            let mut node = HierNode {
                name: name.clone(),
                definition: name.clone(),
                kind: d.kind.label().into(),
                uri: loc.as_ref().map(|l| l.uri.to_string()),
                line: loc.as_ref().map_or(0, |l| l.range.start.line),
                character: loc.as_ref().map_or(0, |l| l.range.start.character),
                def_uri: loc.as_ref().map(|l| l.uri.to_string()),
                def_line: loc.as_ref().map_or(0, |l| l.range.start.line),
                children: Vec::new(),
                truncated: false,
                dup: None,
            };
            let mut stack = vec![self.decl_key(t)];
            if !w.stopped() {
                node.children = self.hier_walk(t, &name, &name, pin, &mut w, &mut stack, &mut node.truncated, &mut FxHashMap::default());
            }
            if matched || !node.children.is_empty() || pin.is_some() {
                roots.push(node);
            }
        }
        (roots, w.meta)
    }

    #[allow(clippy::too_many_arguments)]
    /// `seen`: names of the siblings so far (unnamed blocks add to their parent's level).
    #[allow(clippy::too_many_arguments)]
    fn hier_walk(&self, scope: DeclRef, def_name: &str, path: &str, pin: Option<&[String]>, w: &mut Walk, stack: &mut Vec<(u32, u32)>, truncated: &mut bool, seen: &mut FxHashMap<String, u32>) -> Vec<HierNode> {
        let mut out = Vec::new();
        let u = self.unit(scope.unit);
        let mut todo: Vec<DeclId> = u.parsed.decl(scope.decl).children.clone();
        todo.reverse();
        while let Some(c) = todo.pop() {
            let d = u.parsed.decl(c);
            match d.kind {
                // A `bind` adds its instance to the target, not to the module it is written in.
                DeclKind::Instance if d.has(dflags::BOUND) => {}
                DeclKind::Instance => {
                    let dup = sibling(seen, &d.name);
                    let child_pin = pin_below(pin, &keyed(&d.name, dup));
                    if w.stopped() && child_pin.is_none() {
                        *truncated = true;
                        if pin.is_some() {
                            continue;
                        }
                        return out;
                    }
                    if !w.visit() && child_pin.is_none() {
                        *truncated = true;
                        return out;
                    }
                    let Some(reserved) = w.reserve(child_pin.is_some()) else {
                        *truncated = true;
                        if pin.is_some() {
                            continue;
                        }
                        return out;
                    };
                    let cr = DeclRef { unit: scope.unit, decl: c };
                    let def = self.type_of(cr);
                    let loc = self.decl_location(cr);
                    let def_loc = def.and_then(|m| self.decl_location(m));
                    let ty_name = d.ty.as_ref().map(|t| t.text.split(|ch: char| ch == ' ' || ch == '#').next().unwrap_or("").to_string()).unwrap_or_default();
                    let child_path = format!("{path}.{}", d.name);
                    let mut node = HierNode {
                        name: d.name.to_string(),
                        definition: def.map(|m| self.decl(m).name.to_string()).unwrap_or(ty_name),
                        kind: def.map(|m| self.decl(m).kind.label().to_string()).unwrap_or_else(|| "unresolved".into()),
                        uri: loc.as_ref().map(|l| l.uri.to_string()),
                        line: loc.as_ref().map_or(0, |l| l.range.start.line),
                        character: loc.as_ref().map_or(0, |l| l.range.start.character),
                        def_uri: def_loc.as_ref().map(|l| l.uri.to_string()),
                        def_line: def_loc.as_ref().map_or(0, |l| l.range.start.line),
                        children: Vec::new(),
                        truncated: false,
                        dup,
                    };
                    let matched = if w.q.target.is_some() {
                        w.hit(&node.name, &node.definition, def_name, "")
                    } else {
                        w.matched(&[(&node.name, '.'), (&node.definition, '.'), (&child_path, '.')])
                    };
                    if let Some(m) = def {
                        let key = self.decl_key(m);
                        if !stack.contains(&key) && stack.len() < 64 {
                            stack.push(key);
                            let m_name = self.decl(m).name.to_string();
                            node.children = self.hier_walk(m, &m_name, &child_path, child_pin, w, stack, &mut node.truncated, &mut FxHashMap::default());
                            stack.pop();
                        }
                    }
                    if matched || !node.children.is_empty() || child_pin.is_some() {
                        out.push(node);
                    } else if reserved {
                        w.nodes_left += 1;
                    }
                }
                DeclKind::Generate | DeclKind::Block => {
                    // Generate blocks: show named ones as scopes, flatten unnamed.
                    let gr = DeclRef { unit: scope.unit, decl: c };
                    let named = d.kind == DeclKind::Generate;
                    let dup = if named { sibling(seen, &d.name) } else { None };
                    let (gen_path, gen_pin) = if named { (format!("{path}.{}", d.name), pin_below(pin, &keyed(&d.name, dup))) } else { (path.to_string(), pin) };
                    let mut own = FxHashMap::default();
                    let level = if named { &mut own } else { &mut *seen };
                    let kids = self.hier_walk(gr, def_name, &gen_path, gen_pin, w, stack, truncated, level);
                    if kids.is_empty() {
                        continue;
                    }
                    if named {
                        let loc = self.decl_location(gr);
                        out.push(HierNode {
                            name: d.name.to_string(),
                            definition: String::new(),
                            kind: "generate".into(),
                            uri: loc.as_ref().map(|l| l.uri.to_string()),
                            line: loc.as_ref().map_or(0, |l| l.range.start.line),
                            character: loc.as_ref().map_or(0, |l| l.range.start.character),
                            def_uri: None,
                            def_line: 0,
                            children: kids,
                            truncated: false,
                            dup,
                        });
                    } else {
                        out.extend(kids);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The design element, instance and class around a position.
    pub fn locate(&self, file: FileId, off: u32) -> Located {
        let mut out = Located::default();
        let Some(&unit) = self.units_for(file).first() else { return out };
        let u = self.unit(unit);
        let scope = self.scope_at(unit, file, off);
        let named = |r: DeclRef| NamedDef { name: self.decl(r).name.to_string(), uri: self.decl_location(r).map(|l| l.uri.to_string()) };
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            if matches!(d.kind, DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Checker) {
                out.design = Some(named(DeclRef { unit, decl: s }));
                break;
            }
            cur = d.parent;
        }
        if out.design.is_some() {
            // The innermost instantiation around the position.
            out.instance = u
                .parsed
                .decls
                .iter()
                .filter(|d| d.kind == DeclKind::Instance && !d.has(dflags::BOUND) && d.span.file == file && d.span.start <= off && off <= d.span.end)
                .min_by_key(|d| d.span.len())
                .map(|d| d.name.to_string());
        }
        out.class = self.enclosing_class(unit, scope).map(named);
        out
    }

    // ------------------------------------------------------------ UVM verification hierarchy

    /// Whether `class` is, or derives from, a class named `base`.
    pub fn derives_from(&self, class: DeclRef, base: &str) -> bool {
        let mut cur = Some(class);
        for _ in 0..32 {
            let Some(c) = cur else { return false };
            if self.decl(c).name == base {
                return true;
            }
            cur = self.class_base(c);
        }
        false
    }

    /// Kind of a UVM component class from its nearest UVM base class.
    fn uvm_kind(&self, class: DeclRef) -> Option<&'static str> {
        let mut cur = Some(class);
        for _ in 0..32 {
            let c = cur?;
            let name = &self.decl(c).name;
            if let Some((_, k)) = UVM_KINDS.iter().find(|(n, _)| n == name) {
                return Some(k);
            }
            cur = self.class_base(c);
        }
        None
    }

    /// Classes of the UVM library itself (`uvm_pkg`), whose internals are not shown.
    fn in_uvm_library(&self, class: DeclRef) -> bool {
        let u = self.unit(class.unit);
        let mut cur = u.parsed.decl(class.decl).parent;
        while let Some(p) = cur {
            let d = u.parsed.decl(p);
            if d.kind == DeclKind::Package {
                return d.name == "uvm_pkg" || d.name == "ovm_pkg";
            }
            cur = d.parent;
        }
        false
    }

    /// Why no test is found when classes extend UVM base classes that do not resolve (UVM not
    /// compiled, or `uvm_pkg` not imported where they are declared).
    fn unresolved_uvm_bases(&self) -> Option<String> {
        let mut bases: Vec<(String, usize)> = Vec::new();
        let mut seen = FxHashSet::default();
        let uvm = self.global.packages.get("uvm_pkg").and_then(|v| v.first().copied());
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
            }
            for (i, d) in u.parsed.decls.iter().enumerate() {
                if d.kind != DeclKind::Class || !seen.insert((d.span.file, d.span.start)) {
                    continue;
                }
                // The last class of the chain, whose base does not resolve.
                let mut c = DeclRef { unit: u.id, decl: i as DeclId };
                for _ in 0..32 {
                    match self.class_base(c) {
                        Some(b) => c = b,
                        None => break,
                    }
                }
                let Some(ext) = self.decl(c).extends.as_ref() else { continue };
                let base = ext.text.split(|ch: char| ch == '#' || ch == ' ').next().unwrap_or("").rsplit("::").next().unwrap_or("").to_string();
                if !base.starts_with("uvm_") || self.class_base(c).is_some() {
                    continue;
                }
                // With UVM compiled, only names it declares (a misspelled one is another problem).
                if uvm.is_some_and(|p| self.member_of(p, &base).is_none()) {
                    continue;
                }
                match bases.iter_mut().find(|(b, _)| *b == base) {
                    Some((_, n)) => *n += 1,
                    None => bases.push((base, 1)),
                }
            }
        }
        if bases.is_empty() {
            return None;
        }
        bases.sort_by(|a, b| b.1.cmp(&a.1));
        let total: usize = bases.iter().map(|(_, n)| n).sum();
        let names: Vec<String> = bases.iter().take(3).map(|(b, _)| b.clone()).collect();
        let why = match (uvm.is_some(), self.setup.config.build_file.is_some()) {
            (true, _) => "uvm_pkg is not imported where they are declared",
            (false, true) => "the UVM library is not compiled: add -uvm to the build file (with UVM_HOME or the dvicode.uvmHome setting naming its folder), or its uvm_pkg.sv",
            (false, false) => "the UVM library is not compiled: set dvicode.uvmHome to its folder",
        };
        let classes = if total == 1 { "1 class extends".to_string() } else { format!("{total} classes extend") };
        let mut s = format!("No UVM test found: {classes} {} but {why}.", names.join(", "));
        if self.setup.config.build_file.is_none() {
            s.push_str(" Without a build file the simulator's defines are unknown: see the SKIPPED_CODE notices, or add a build file.");
        }
        Some(s)
    }

    /// Non-virtual classes derived from `uvm_test`, by name.
    pub fn uvm_tests(&self) -> Vec<DeclRef> {
        let mut seen = FxHashSet::default();
        let mut out = Vec::new();
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
            }
            for (i, d) in u.parsed.decls.iter().enumerate() {
                if d.kind != DeclKind::Class || d.has(dflags::VIRTUAL) || d.name == "uvm_test" {
                    continue;
                }
                let r = DeclRef { unit: u.id, decl: i as DeclId };
                if !self.in_uvm_library(r) && self.derives_from(r, "uvm_test") && seen.insert(self.decl_key(r)) {
                    out.push(r);
                }
            }
        }
        out.sort_by(|a, b| self.decl(*a).name.cmp(&self.decl(*b).name));
        out
    }

    /// The component tree of a UVM test (or of every test), built from the
    /// `type_id::create(name, this)` and `new(name, this)` calls in `build_phase` and the
    /// constructors of each class and its base classes. `test`: a test class name, `*` for
    /// every test, or `None` for `+UVM_TESTNAME` (every test when it is not set).
    pub fn verification_hierarchy(&self, test: Option<&str>, max_nodes: usize) -> VerifHierarchy {
        self.verification_tree(test, &TreeQuery::new(max_nodes)).0
    }

    /// [`Analysis::verification_hierarchy`] as `q` asks.
    pub fn verification_tree(&self, test: Option<&str>, q: &TreeQuery) -> (VerifHierarchy, TreeMeta) {
        let configured = self.setup.config.uvm_testname();
        let tests = self.uvm_tests();
        let wanted = match test {
            Some("*") => None,
            Some(t) => Some(t.to_string()),
            None => configured.clone(),
        };
        let roots: Vec<DeclRef> = match &wanted {
            Some(t) => tests.iter().copied().filter(|r| self.decl(*r).name == t.as_str()).take(1).collect(),
            None => tests.clone(),
        };
        let mut w = Walk::new(q);
        let scan = Scan::default();
        let mut out = Vec::new();
        let mut seen = FxHashMap::default();
        for t in roots {
            let d = self.decl(t);
            let dup = sibling(&mut seen, &d.name);
            let key = keyed(&d.name, dup);
            if w.stopped() && !q.reveal.first().is_some_and(|r| *r == key) {
                break;
            }
            let loc = self.decl_location(t);
            let pin = pin_below(Some(q.reveal.as_slice()), &key);
            let matched = if q.target.is_some() { w.hit("uvm_test_top", "", "", &d.name) } else { w.matched(&[("uvm_test_top", '.'), (&d.name, '.')]) };
            let mut node = VerifNode {
                name: "uvm_test_top".into(),
                path: "uvm_test_top".into(),
                class: d.name.to_string(),
                kind: "test".into(),
                uri: loc.as_ref().map(|l| l.uri.to_string()),
                line: loc.as_ref().map_or(0, |l| l.range.start.line),
                character: loc.as_ref().map_or(0, |l| l.range.start.character),
                def_uri: loc.as_ref().map(|l| l.uri.to_string()),
                def_line: loc.as_ref().map_or(0, |l| l.range.start.line),
                children: Vec::new(),
                truncated: false,
                dup,
                conditions: Vec::new(),
                count: None,
                loops: Vec::new(),
                alternatives: Vec::new(),
                ports_hidden: 0,
            };
            let mut stack = vec![self.decl_key(t)];
            if !w.stopped() {
                node.children = self.component_children(t, "uvm_test_top", pin, &mut w, &mut stack, &mut node.truncated, &mut node.ports_hidden, &scan);
            }
            if matched || !node.children.is_empty() || pin.is_some() {
                out.push(node);
            }
        }
        let hint = if tests.is_empty() { self.unresolved_uvm_bases() } else { None };
        let h = VerifHierarchy { test: wanted, configured, tests: tests.iter().map(|r| self.decl(*r).name.to_string()).collect(), roots: out, hint };
        (h, w.meta)
    }

    #[allow(clippy::too_many_arguments)]
    fn component_children(&self, class: DeclRef, path: &str, pin: Option<&[String]>, w: &mut Walk, stack: &mut Vec<(u32, u32)>, truncated: &mut bool, ports_hidden: &mut u32, scan: &Scan) -> Vec<VerifNode> {
        // The class and its base classes below the UVM library, most derived first.
        let mut derived_first = Vec::new();
        let mut cur = Some(class);
        while let Some(c) = cur {
            if derived_first.len() >= 32 || self.in_uvm_library(c) {
                break;
            }
            derived_first.push(c);
            cur = self.class_base(c);
        }
        let made = self.build_creations(&derived_first, scan);
        if scan.exhausted.get() && w.meta.incomplete.is_none() {
            w.meta.incomplete = Some("visits");
        }
        let mut out = Vec::new();
        let mut seen = FxHashMap::default();
        // The created class: `T` of `T::type_id::create`, else the variable's type. A type
        // parameter of a base class (`T_ENV env`) is the type a derived class gives it.
        let class_of = |body: DeclRef, cr: &Creation| -> Option<DeclRef> {
            let is_class = |c: &DeclRef| matches!(self.decl(*c).kind, DeclKind::Class);
            let from_ty = cr.ty.and_then(|s| self.resolve_at_span(body.unit, s)).and_then(|r| self.follow_typedef(r));
            let from_var = || self.resolve_at_span(body.unit, cr.lhs_head).and_then(|v| self.type_of(v));
            cr.class.filter(is_class).or_else(|| {
                from_ty
                    .and_then(|r| self.bind_type_param(r, &derived_first, scan))
                    .filter(is_class)
                    .or_else(|| from_var().and_then(|r| self.bind_type_param(r, &derived_first, scan)).filter(is_class))
            })
        };
        for Made { body, cr, mut alts, others } in made {
            let cls = class_of(body, &cr);
            // Other creations of the same class only add ways to make it.
            let mut alternatives: Vec<Alternative> = Vec::new();
            for o in others {
                let oc = class_of(o.body, &o.cr);
                if oc.map(|c| self.decl_key(c)) == cls.map(|c| self.decl_key(c)) {
                    alts.extend(o.alts);
                    continue;
                }
                let loc = self.to_location(o.cr.lhs_head);
                let def = oc.and_then(|c| self.decl_location(c));
                alternatives.push(Alternative {
                    class: oc.map_or_else(|| o.cr.ty_text.clone(), |c| self.decl(c).name.to_string()),
                    uri: loc.as_ref().map(|l| l.uri.to_string()),
                    line: loc.as_ref().map_or(0, |l| l.range.start.line),
                    character: loc.as_ref().map_or(0, |l| l.range.start.character),
                    def_uri: def.as_ref().map(|l| l.uri.to_string()),
                    def_line: def.as_ref().map_or(0, |l| l.range.start.line),
                    conditions: conditions_text(&o.alts),
                });
            }
            let kind = match cls {
                Some(c) => match self.uvm_kind(c) {
                    Some(k) => k,
                    // An object created with a parent (a configuration): not a component.
                    None if self.class_chain_complete(c) => continue,
                    None => "component",
                },
                None => "unresolved",
            };
            let name = cr.name.clone().unwrap_or_else(|| cr.lhs.trim_start_matches("this.").to_string());
            let dup = sibling(&mut seen, &name);
            // Hidden ports still count for `#2` numbering, so hiding them renames nothing.
            if kind == "port" && !w.q.ports {
                *ports_hidden += 1;
                continue;
            }
            let child_pin = pin_below(pin, &keyed(&name, dup));
            if (w.stopped() || !w.visit()) && child_pin.is_none() {
                *truncated = true;
                if pin.is_some() {
                    continue;
                }
                break;
            }
            let Some(reserved) = w.reserve(child_pin.is_some()) else {
                *truncated = true;
                if pin.is_some() {
                    continue;
                }
                break;
            };
            let child_path = format!("{path}.{name}");
            let loc = self.to_location(cr.lhs_head);
            let def_loc = cls.and_then(|c| self.decl_location(c));
            let class_name = match cls {
                Some(c) => self.decl(c).name.to_string(),
                None if !cr.ty_text.is_empty() => cr.ty_text.clone(),
                None => self.resolve_at_span(body.unit, cr.lhs_head).and_then(|v| self.decl(v).ty.as_ref().map(|t| t.text.to_string())).unwrap_or_default(),
            };
            let mut node = VerifNode {
                name,
                path: child_path.clone(),
                class: class_name,
                kind: kind.into(),
                uri: loc.as_ref().map(|l| l.uri.to_string()),
                line: loc.as_ref().map_or(0, |l| l.range.start.line),
                character: loc.as_ref().map_or(0, |l| l.range.start.character),
                def_uri: def_loc.as_ref().map(|l| l.uri.to_string()),
                def_line: def_loc.as_ref().map_or(0, |l| l.range.start.line),
                children: Vec::new(),
                truncated: false,
                dup,
                conditions: conditions_text(&alts),
                count: self.loops_count(&alts[0]),
                loops: alts[0].iter().filter_map(loop_text).collect(),
                alternatives,
                ports_hidden: 0,
            };
            // Matched before the children, so the match limit counts in tree order.
            let matched = if w.q.target.is_some() { w.hit(&node.name, "", "", &node.class) } else { w.matched(&[(&node.name, '.'), (&node.class, '.'), (&node.path, '.')]) };
            if let Some(c) = cls.filter(|_| kind != "port") {
                let key = self.decl_key(c);
                if !stack.contains(&key) && stack.len() < 64 {
                    stack.push(key);
                    node.children = self.component_children(c, &child_path, child_pin, w, stack, &mut node.truncated, &mut node.ports_hidden, scan);
                    stack.pop();
                }
            }
            if matched || !node.children.is_empty() || child_pin.is_some() {
                out.push(node);
            } else if reserved {
                w.nodes_left += 1;
            }
        }
        out
    }

    /// The type a type parameter of a class in `derived_first` (a class and its bases, most
    /// derived first) stands for: the value the class below it gives in `extends B#(...)`
    /// (named or by position, possibly passing on its own parameter), else its default.
    /// Other declarations are returned as they are.
    fn bind_type_param(&self, mut r: DeclRef, derived_first: &[DeclRef], scan: &Scan) -> Option<DeclRef> {
        for _ in 0..16 {
            let d = self.decl(r);
            if d.kind != DeclKind::TypeParam {
                return Some(r);
            }
            let owner = DeclRef { unit: r.unit, decl: d.parent? };
            let key = self.decl_key(owner);
            let at = derived_first.iter().position(|c| self.decl_key(*c) == key);
            let given = match at {
                Some(i) if i > 0 => self.extends_param_value(derived_first[i - 1], owner, r, scan),
                _ => None,
            };
            // A value that cannot be resolved (a macro) is not replaced by the default.
            let next = match given {
                Some(v) => v?,
                None => self.type_param_default(r, scan)?,
            };
            r = self.follow_typedef(next)?;
        }
        None
    }

    /// The type `derived` gives the type parameter `param` of its base `base` in
    /// `extends base#(...)`, or through a typedef (`typedef base#(...) base_t;`,
    /// `extends base_t`).
    /// `Some(None)`: a value is given but not resolved.
    fn extends_param_value(&self, derived: DeclRef, base: DeclRef, param: DeclRef, scan: &Scan) -> Option<Option<DeclRef>> {
        // The declaration whose header names the base type, and the keyword before that type.
        let (mut src, mut kw) = (derived, "extends");
        for _ in 0..8 {
            let d = self.decl(src);
            if d.has(dflags::FROM_MACRO) {
                return None;
            }
            let toks = scan.tokens(self, d.span.file)?;
            let lo = toks.partition_point(|t| t.span.start < d.span.start);
            let hi = toks.partition_point(|t| t.span.start < d.span.end);
            let t = &toks[lo..hi];
            // `class C #(...) extends B #(...);` or `typedef B #(...) C;`.
            let header_end = t.iter().position(|x| x.is_punct(";")).unwrap_or(t.len());
            let at = t[..header_end].iter().position(|x| x.is_kw(kw))? + 1;
            let mut k = at;
            while k < header_end && (t[k].is_ident() || t[k].is_punct("::")) {
                k += 1;
            }
            if k + 1 < header_end && t[k].is_punct("#") && t[k + 1].is_punct("(") {
                return self.param_value_in(src.unit, call_args(t, k + 1), base, param);
            }
            // No values here: the base may be a typedef that gives them.
            let named = self.type_name_in(src.unit, &t[at..k])?;
            if self.decl(named).kind != DeclKind::Typedef {
                return None;
            }
            (src, kw) = (named, "typedef");
        }
        None
    }

    /// The type a `#(...)` value list (`args`, resolved in `unit`) gives `param` of `base`.
    fn param_value_in(&self, unit: crate::world::UnitId, args: Vec<&[Token]>, base: DeclRef, param: DeclRef) -> Option<Option<DeclRef>> {
        let name = self.decl(param).name.as_str();
        let value = args.iter().find_map(|a| match a {
            [dot, n, open, inner @ .., close] if dot.is_punct(".") && open.is_punct("(") && close.is_punct(")") => (n.text == name).then_some(inner),
            _ => None,
        });
        let value = match value {
            Some(v) => v,
            None if args.first().is_some_and(|a| a.first().is_some_and(|x| x.is_punct("."))) => return None,
            None => {
                let u = self.unit(base.unit);
                let header_params = u.parsed.decl(base.decl).children.iter().filter(|c| matches!(u.parsed.decl(**c).kind, DeclKind::Parameter | DeclKind::TypeParam));
                let index = header_params.clone().position(|c| *c == param.decl)?;
                args.get(index).copied()?
            }
        };
        if value.is_empty() {
            return None;
        }
        // A macro (`base#(`ENV)`): what its expansion at that place resolves to, if anything.
        let by_macro = || value.first().filter(|x| x.kind == TokKind::Directive).and_then(|x| self.resolve_at_span(unit, x.span));
        Some(self.type_name_in(unit, value).or_else(by_macro))
    }

    /// The default type of a type parameter (`type T = my_env`).
    fn type_param_default(&self, param: DeclRef, scan: &Scan) -> Option<DeclRef> {
        let d = self.decl(param);
        if d.has(dflags::FROM_MACRO) {
            return None;
        }
        let toks = scan.tokens(self, d.span.file)?;
        // After the parameter's own name: `parameter type A = a_t, B = b_t;`.
        let lo = toks.partition_point(|t| t.span.start < d.name_span.end);
        let hi = toks.partition_point(|t| t.span.start < d.span.end.max(d.name_span.end));
        let t = &toks[lo..hi];
        if !t.first().is_some_and(|x| x.is_punct("=")) {
            return None;
        }
        self.type_name_in(param.unit, &t[1..])
    }

    /// The type named by the tokens of a type expression (`my_env`, `pkg::my_env`,
    /// `my_env#(8)`): the declaration its last name resolves to (typedefs not followed).
    fn type_name_in(&self, unit: crate::world::UnitId, t: &[Token]) -> Option<DeclRef> {
        let mut last = None;
        for (k, x) in t.iter().enumerate() {
            if x.is_ident() && (k == 0 || t[k - 1].is_punct("::")) {
                last = Some(x.span);
            } else if !x.is_punct("::") {
                break;
            }
        }
        self.resolve_at_span(unit, last?)
    }

    /// The iterations of the loops among `guards`: their product, `up to` it when a loop can
    /// stop early, `?` when one is not a constant; `None` outside loops.
    fn loops_count(&self, guards: &[Guard]) -> Option<String> {
        let mut n: Option<u64> = Some(1);
        let mut early = false;
        let mut any = false;
        for g in guards {
            if let Guard::Loop { kind, header, unit, exits } = g {
                any = true;
                early |= *exits;
                n = n.zip(self.loop_count(kind, header, *unit)).and_then(|(a, b)| a.checked_mul(b));
            }
        }
        if !any {
            return None;
        }
        Some(match n {
            Some(n) if early => format!("up to {n}"),
            Some(n) => n.to_string(),
            None => "?".to_string(),
        })
    }

    /// The component creations of a class (`derived_first`: it and its base classes, most
    /// derived first), in the order they run: its construction (the most derived `new`, with
    /// its base class's first, as `super.new()` runs), then its build phase (the most
    /// derived `build_phase`, with `super.build_phase()` where it is called; the UVM library's
    /// calls the most derived `build()`). Methods of the class they call are followed: large
    /// testbenches build in helpers.
    fn build_creations(&self, derived_first: &[DeclRef], scan: &Scan) -> Vec<Made> {
        let mut made = Vec::new();
        let mut stack = Vec::new();
        let none = FxHashMap::default();
        if let Some((body, at)) = self.method_from("new", 0, derived_first) {
            self.run_method(body, at, derived_first, &none, &[], &mut stack, scan, &mut made);
        }
        if let Some((body, at)) = self.dispatch("build_phase", 0, derived_first).or_else(|| self.dispatch("build", 0, derived_first)) {
            self.run_method(body, at, derived_first, &none, &[], &mut stack, scan, &mut made);
        }
        made
    }

    /// Adds the creations of a method body (of `derived_first[at]`) and of the methods it calls.
    /// `env`: string literals given for its arguments; `outer`: the guards of the call.
    #[allow(clippy::too_many_arguments)]
    fn run_method(&self, body: DeclRef, at: usize, derived_first: &[DeclRef], env: &FxHashMap<String, String>, outer: &[Guard], stack: &mut Vec<DeclRef>, scan: &Scan, made: &mut Vec<Made>) {
        if stack.contains(&body) || stack.len() > MAX_CALL_DEPTH || scan.exhausted.get() {
            return;
        }
        stack.push(body);
        let s = self.summary(body, scan);
        // The base class's constructor runs first: `super.new()` must be a constructor's first
        // statement, and is implied when it is not written.
        if self.decl(body).name == "new" {
            if let Some((b, i)) = self.method_from("new", at + 1, derived_first) {
                self.run_method(b, i, derived_first, &FxHashMap::default(), outer, stack, scan, made);
            }
        }
        for e in &s.events {
            if !scan.spend() {
                break;
            }
            let guards: Vec<Guard> = outer.iter().chain(&e.guards).cloned().collect();
            match &e.what {
                What::Create(cr) => {
                    let mut cr = cr.clone();
                    if cr.name.is_none() {
                        cr.name = cr.name_arg.as_ref().and_then(|a| env.get(a)).cloned();
                    }
                    add_made(made, Made { body, cr, alts: vec![guards], others: Vec::new() });
                }
                What::Call { name, sup, args } => {
                    let target = if *sup {
                        // The UVM library's `build_phase` calls `build()`.
                        self.method_from(name, at + 1, derived_first).or_else(|| if name == "build_phase" { self.dispatch("build", 0, derived_first) } else { None })
                    } else {
                        self.dispatch(name, at, derived_first)
                    };
                    let Some((callee, ci)) = target else { continue };
                    let env = self.literal_args(callee, args);
                    self.run_method(callee, ci, derived_first, &env, &guards, stack, scan, made);
                }
            }
        }
        stack.pop();
    }

    /// The string literals a call gives the arguments of `callee`, by name.
    fn literal_args(&self, callee: DeclRef, args: &[(Option<String>, Option<String>)]) -> FxHashMap<String, String> {
        let u = self.unit(callee.unit);
        let formals: Vec<&str> = u.parsed.decl(callee.decl).children.iter().map(|c| u.parsed.decl(*c)).filter(|d| d.kind == DeclKind::Argument).map(|d| d.name.as_str()).collect();
        let mut out = FxHashMap::default();
        for (i, (name, value)) in args.iter().enumerate() {
            let formal = match name {
                Some(n) => Some(n.as_str()),
                None => formals.get(i).copied(),
            };
            if let (Some(f), Some(v)) = (formal, value) {
                out.insert(f.to_string(), v.clone());
            }
        }
        out
    }

    /// What a method body does: its component creations and the calls of its own methods, with
    /// the conditions and loops around each. Computed once per request.
    fn summary(&self, body: DeclRef, scan: &Scan) -> Rc<Summary> {
        if let Some(s) = scan.summaries.borrow().get(&body) {
            return s.clone();
        }
        let toks = self.body_tokens(body, scan);
        let map = stmt::walk_method(&toks);
        let guards_at = |k: usize| -> Vec<Guard> {
            let Some(m) = &map else { return vec![Guard::Unknown] };
            m.frames_at(k)
                .into_iter()
                .map(|f| match &f.kind {
                    FrameKind::If { cond, negated } => Guard::If { text: tokens_text(&toks[cond.0..cond.1]), negated: *negated },
                    FrameKind::Case { expr, labels } => {
                        let labels = labels.map_or_else(|| "default".to_string(), |l| tokens_text(&toks[l.0..l.1]));
                        Guard::Case { text: format!("{}: {labels}", tokens_text(&toks[expr.0..expr.1])) }
                    }
                    FrameKind::Loop { kind, header, body: b } => Guard::Loop {
                        kind,
                        header: toks[header.0..header.1].to_vec(),
                        unit: body.unit,
                        exits: toks[b.0..b.1.min(toks.len())].iter().any(|x| x.is_kw("break") || x.is_kw("continue") || x.is_kw("return")),
                    },
                })
                .collect()
        };
        let mut events: Vec<(usize, Event)> = Vec::new();
        let creations = scan_creations(&toks).into_iter().filter(|(_, c)| c.parent == "this").chain(self.factory_calls(body, &toks, scan));
        for (k, cr) in creations {
            events.push((k, Event { guards: guards_at(k), what: What::Create(cr) }));
        }
        for (k, name, sup, args) in method_calls(&toks) {
            events.push((k, Event { guards: guards_at(k), what: What::Call { name, sup, args } }));
        }
        events.sort_by_key(|(k, _)| *k);
        let s = Rc::new(Summary { events: events.into_iter().map(|(_, e)| e).collect() });
        scan.summaries.borrow_mut().insert(body, s.clone());
        s
    }

    /// The method `name` of the first class in `derived_first[from..]` that has one (a
    /// non-virtual lookup, as `super.name()` makes from `derived_first[from - 1]`).
    fn method_from(&self, name: &str, from: usize, derived_first: &[DeclRef]) -> Option<(DeclRef, usize)> {
        derived_first.iter().enumerate().skip(from).find_map(|(i, c)| {
            let u = self.unit(c.unit);
            u.parsed
                .lookup_member(c.decl, name)
                .iter()
                .map(|&m| DeclRef { unit: c.unit, decl: m })
                .find(|m| matches!(self.decl(*m).kind, DeclKind::Function | DeclKind::Task))
                .map(|m| (self.implementation_of(m).unwrap_or(m), i))
        })
    }

    /// Components made by a factory: `lhs = obj.make(..., this, ...)` (or `make(...)`,
    /// `pkg::make(...)`) where `make` creates exactly one component, with a parent it is given
    /// (`T::type_id::create(name, parent)`, `parent` an argument of `make`), and the call gives
    /// `this` for that argument, by position or by name (`.parent(this)`). The created class is
    /// `T` (else the method's return type); the name is the one given, else the variable's.
    /// Anything less certain (no creation, several, a parent that is not `this`) is left out.
    fn factory_calls(&self, body: DeclRef, t: &[Token], scan: &Scan) -> Vec<(usize, Creation)> {
        let mut out = Vec::new();
        for i in 1..t.len() {
            if !t[i].is_punct("=") {
                continue;
            }
            let Some((lhs_start, head)) = lhs_before(t, i) else { continue };
            // `a.b.make(` / `pkg::make(` / `make(`.
            let mut k = i + 1;
            if t.get(k).is_some_and(|x| x.is_kw("this")) && t.get(k + 1).is_some_and(|x| x.is_punct(".")) {
                k += 2;
            }
            let mut last = None;
            while t.get(k).is_some_and(|x| x.is_ident()) {
                last = Some(k);
                if !t.get(k + 1).is_some_and(|x| x.is_punct(".") || x.is_punct("::")) {
                    break;
                }
                k += 2;
            }
            let Some(m) = last else { continue };
            if !t.get(m + 1).is_some_and(|x| x.is_punct("(")) || matches!(t[m].text.as_str(), "create" | "new") {
                continue;
            }
            let Some(f) = self.resolve_at_span(body.unit, t[m].span).filter(|f| self.decl(*f).kind == DeclKind::Function) else { continue };
            let fb = self.implementation_of(f).unwrap_or(f);
            let formals: Vec<String> = {
                let u = self.unit(fb.unit);
                u.parsed.decl(fb.decl).children.iter().map(|c| u.parsed.decl(*c)).filter(|d| d.kind == DeclKind::Argument).map(|d| d.name.to_string()).collect()
            };
            let fb_toks = self.body_tokens(fb, scan);
            let made: Vec<Creation> = scan_creations(&fb_toks).into_iter().chain(scan_returned_creations(&fb_toks)).map(|(_, c)| c).filter(|c| formals.contains(&c.parent)).collect();
            let [cr] = made.as_slice() else { continue };
            // `parent = owner;` inside the factory: the argument is not what the caller gave.
            if [Some(&cr.parent), cr.name_arg.as_ref()].into_iter().flatten().filter(|f| formals.contains(f)).any(|f| assigns(&fb_toks, f)) {
                continue;
            }
            // What the call gives an argument of `make`: by name, else by position.
            let args = call_args(t, m + 1);
            let actual = |formal: &str| -> Option<&[Token]> {
                let named = args.iter().find_map(|a| match a {
                    [dot, n, open, inner @ .., close] if dot.is_punct(".") && open.is_punct("(") && close.is_punct(")") => (n.text == formal).then_some(inner),
                    _ => None,
                });
                if args.first().is_some_and(|a| a.first().is_some_and(|x| x.is_punct("."))) {
                    return named;
                }
                formals.iter().position(|x| x == formal).and_then(|p| args.get(p).copied())
            };
            if !actual(&cr.parent).is_some_and(|a| a.len() == 1 && a[0].is_kw("this")) {
                continue;
            }
            let class = cr
                .ty
                .and_then(|s| self.resolve_at_span(fb.unit, s))
                .and_then(|r| self.follow_typedef(r))
                .filter(|c| self.decl(*c).kind == DeclKind::Class)
                .or_else(|| self.type_of(f).filter(|c| self.decl(*c).kind == DeclKind::Class));
            let name = cr.name.clone().or_else(|| {
                let a = actual(cr.name_arg.as_deref()?)?;
                (a.len() == 1 && a[0].kind == TokKind::String).then(|| a[0].text.trim_matches('"').to_string())
            });
            out.push((
                lhs_start,
                Creation { lhs: tokens_text(&t[lhs_start..i]), lhs_head: t[head].span, ty: None, ty_text: cr.ty_text.clone(), name, name_arg: None, parent: "this".into(), class },
            ));
        }
        out
    }

    /// The method `name` a method of `derived_first[at]` calls on `this`: the one that class
    /// sees (its own or inherited), or the most derived override when it is virtual (declared
    /// `virtual` there or in a base class). With the index of its class.
    fn dispatch(&self, name: &str, at: usize, derived_first: &[DeclRef]) -> Option<(DeclRef, usize)> {
        let methods = |c: &DeclRef| {
            let u = self.unit(c.unit);
            u.parsed
                .lookup_member(c.decl, name)
                .iter()
                .map(|&m| DeclRef { unit: c.unit, decl: m })
                .filter(|m| matches!(self.decl(*m).kind, DeclKind::Function | DeclKind::Task))
                .collect::<Vec<_>>()
        };
        let find = |from: usize| derived_first.iter().enumerate().skip(from).find_map(|(i, c)| methods(c).first().map(|m| (*m, i)));
        let (mut m, mut i) = find(at)?;
        if derived_first[i..].iter().any(|c| methods(c).iter().any(|m| self.decl(*m).has(dflags::VIRTUAL))) {
            (m, i) = find(0)?;
        }
        Some((self.implementation_of(m).unwrap_or(m), i))
    }

    /// The tokens of a method body that are compiled (not those in `ifdef branches that are
    /// off).
    fn body_tokens(&self, body: DeclRef, scan: &Scan) -> Vec<Token> {
        let d = self.decl(body);
        if d.has(dflags::FROM_MACRO) || d.has(dflags::PROTOTYPE) {
            return Vec::new();
        }
        let Some(toks) = scan.tokens(self, d.span.file) else { return Vec::new() };
        let lo = toks.partition_point(|t| t.span.start < d.span.start);
        let hi = toks.partition_point(|t| t.span.start < d.span.end);
        let mut off: Vec<Span> = self.unit(body.unit).inactive.iter().filter(|s| s.file == d.span.file && s.end > d.span.start && s.start < d.span.end).copied().collect();
        off.sort_by_key(|s| s.start);
        // Tokens in order, regions in order: one pass.
        let mut k = 0;
        toks[lo..hi]
            .iter()
            .filter(|t| {
                while k < off.len() && off[k].end <= t.span.start {
                    k += 1;
                }
                !(k < off.len() && off[k].start <= t.span.start)
            })
            .cloned()
            .collect()
    }

    /// The declaration a reference segment written exactly at `span` resolves to.
    pub fn resolve_at_span(&self, unit: crate::world::UnitId, span: Span) -> Option<DeclRef> {
        let u = self.unit(unit);
        let pos = u.positions.get(&span.file)?;
        let i = pos.refs.partition_point(|(s, _, _)| (s.start, s.end) < (span.start, span.end));
        let (s, rid, seg) = pos.refs.get(i)?;
        if *s != span {
            return None;
        }
        self.resolve_ref(unit, *rid, *seg as usize)
    }

    /// Text of a macro expansion at a position (`levels`: 1 or all).
    pub fn expand_macro_at(&self, file: FileId, off: u32, levels: usize) -> Option<String> {
        for uid in self.units_for(file) {
            let u = self.unit(uid);
            for (i, m) in u.macro_uses.iter().enumerate() {
                if m.call_span.file == file && m.call_span.start <= off && off <= m.call_span.end {
                    return self.expand_macro_use(uid, i, levels);
                }
            }
        }
        None
    }

    /// Text for a whole file with every macro expanded (for "expand all").
    pub fn source_text(&self, file: FileId) -> Option<std::sync::Arc<str>> {
        self.file_text(file)
    }
}

/// Component creations (`lhs = T::type_id::create(name, this)`, `lhs = new(name, this)`)
/// in the tokens of a method body, with the index where each starts.
fn scan_creations(t: &[Token]) -> Vec<(usize, Creation)> {
    let mut out = Vec::new();
    for i in 1..t.len() {
        if !t[i].is_punct("=") {
            continue;
        }
        let Some((lhs_start, head)) = lhs_before(t, i) else { continue };
        if let Some(c) = creation_call(t, i + 1, tokens_text(&t[lhs_start..i]), t[head].span) {
            out.push((lhs_start, c));
        }
    }
    out
}

/// Creations a method returns directly (`return T::type_id::create(name, parent);`).
fn scan_returned_creations(t: &[Token]) -> Vec<(usize, Creation)> {
    (0..t.len()).filter(|&i| t[i].is_kw("return")).filter_map(|i| creation_call(t, i + 1, String::new(), t[i].span).map(|c| (i, c))).collect()
}

/// A `T::type_id::create(name, parent)` or `new(name, parent)` call starting at `t[k]`.
fn creation_call(t: &[Token], mut k: usize, lhs: String, lhs_head: Span) -> Option<Creation> {
    let mut ty = None;
    let mut ty_text = String::new();
    if t.get(k).is_some_and(|x| x.is_kw("new")) {
        k += 1;
    } else {
        let mut idents: Vec<usize> = Vec::new();
        while k < t.len() {
            if t[k].is_ident() && (idents.is_empty() || t[k - 1].is_punct("::")) {
                idents.push(k);
                k += 1;
            } else if t[k].is_punct("::") {
                k += 1;
            } else if t[k].is_punct("#") && t.get(k + 1).is_some_and(|x| x.is_punct("(")) {
                k = skip_group(t, k + 1);
            } else {
                break;
            }
        }
        let n = idents.len();
        if n < 2 || t[idents[n - 1]].text != "create" || t[idents[n - 2]].text != "type_id" {
            return None;
        }
        if n >= 3 {
            let ti = idents[n - 3];
            ty = Some(t[ti].span);
            ty_text = t[ti].text.to_string();
        }
    }
    if !t.get(k).is_some_and(|x| x.is_punct("(")) {
        return None;
    }
    let args = call_args(t, k);
    // Components get their parent as the second argument: `this`, or a name (an argument of a
    // factory method).
    let parent = args.get(1).filter(|a| a.len() == 1 && (a[0].is_kw("this") || a[0].is_ident())).map(|a| a[0].text.to_string())?;
    let first = args.first().filter(|a| a.len() == 1);
    let name = first.filter(|a| a[0].kind == TokKind::String).map(|a| a[0].text.trim_matches('"').to_string());
    let name_arg = first.filter(|a| a[0].is_ident()).map(|a| a[0].text.to_string());
    Some(Creation { lhs, lhs_head, ty, ty_text, name, name_arg, parent, class: None })
}

/// Calls of the object's own methods in a method body (`f(...)`, `this.f(...)`) and of its base
/// class's (`super.f(...)`; not `obj.f(...)` or `pkg::f(...)`), with the index of each, whether
/// it is a `super` call, and its arguments: the name of each given by name, and its value when
/// it is a string literal.
#[allow(clippy::type_complexity)]
fn method_calls(t: &[Token]) -> Vec<(usize, String, bool, Vec<(Option<String>, Option<String>)>)> {
    let mut out = Vec::new();
    for i in 1..t.len().saturating_sub(1) {
        if !t[i].is_ident() || !t[i + 1].is_punct("(") {
            continue;
        }
        let sup = t[i - 1].is_punct(".") && i >= 2 && t[i - 2].is_kw("super");
        let own = if t[i - 1].is_punct(".") { sup || (i >= 2 && t[i - 2].is_kw("this")) } else { !t[i - 1].is_punct("::") };
        if !own {
            continue;
        }
        let literal = |a: &[Token]| (a.len() == 1 && a[0].kind == TokKind::String).then(|| a[0].text.trim_matches('"').to_string());
        let args = call_args(t, i + 1)
            .into_iter()
            .map(|a| match a {
                [dot, n, open, inner @ .., close] if dot.is_punct(".") && open.is_punct("(") && close.is_punct(")") => (Some(n.text.to_string()), literal(inner)),
                _ => (None, literal(a)),
            })
            .collect();
        out.push((i, t[i].text.to_string(), sup, args));
    }
    out
}

/// Whether the tokens assign to the variable `name` (`name = ...`, not `name == ...`).
fn assigns(t: &[Token], name: &str) -> bool {
    t.windows(2).enumerate().any(|(i, w)| w[0].is_ident() && w[0].text == name && w[1].is_punct("=") && !(i > 0 && t[i - 1].is_punct(".")))
}

/// Counts a sibling called `name`: `Some(k)` for the k-th (k > 1) of that name.
fn sibling(seen: &mut FxHashMap<String, u32>, name: &str) -> Option<u32> {
    let n = seen.entry(name.to_string()).or_insert(0);
    *n += 1;
    (*n > 1).then_some(*n)
}

/// A node's name in paths: `f`, or `f#2` for the second sibling called `f`.
fn keyed(name: &str, dup: Option<u32>) -> String {
    match dup {
        Some(k) => format!("{name}#{k}"),
        None => name.to_string(),
    }
}

/// Start and variable-name token of the assignment target before `t[eq]` (`a.b[i]`).
fn lhs_before(t: &[Token], eq: usize) -> Option<(usize, usize)> {
    let mut j = eq;
    let mut head = None;
    while j > 0 {
        let p = &t[j - 1];
        if p.is_punct("]") {
            let mut depth = 0i32;
            let mut m = j - 1;
            loop {
                if t[m].is_punct("]") {
                    depth += 1;
                } else if t[m].is_punct("[") {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                if m == 0 {
                    return None;
                }
                m -= 1;
            }
            j = m;
            continue;
        }
        if p.is_ident() || p.is_kw("this") {
            if head.is_none() {
                if !p.is_ident() {
                    return None;
                }
                head = Some(j - 1);
            }
            j -= 1;
            if j > 0 && t[j - 1].is_punct(".") {
                j -= 1;
                continue;
            }
        }
        break;
    }
    Some((j, head?))
}

/// Index after the group opened at `t[open]`.
fn skip_group(t: &[Token], open: usize) -> usize {
    let mut depth = 0i32;
    for (k, x) in t.iter().enumerate().skip(open) {
        if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
            depth += 1;
        } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
            depth -= 1;
            if depth == 0 {
                return k + 1;
            }
        }
    }
    t.len()
}

/// Top-level arguments of the call whose `(` is `t[open]`.
fn call_args(t: &[Token], open: usize) -> Vec<&[Token]> {
    let end = skip_group(t, open).saturating_sub(1).max(open + 1);
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    for k in open + 1..end.min(t.len()) {
        let x = &t[k];
        if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
            depth += 1;
        } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
            depth -= 1;
        } else if depth == 0 && x.is_punct(",") {
            out.push(&t[start..k]);
            start = k + 1;
        }
    }
    if start < end.min(t.len()) {
        out.push(&t[start..end.min(t.len())]);
    }
    out
}

fn tokens_text(t: &[Token]) -> String {
    let mut s = String::new();
    for (i, x) in t.iter().enumerate() {
        if i > 0 && x.ws_before() {
            s.push(' ');
        }
        if x.kind == TokKind::Directive {
            s.push('`');
        }
        s.push_str(&x.text);
    }
    s
}
