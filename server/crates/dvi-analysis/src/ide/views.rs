//! Data for the custom views: compiled files, design hierarchy, including files, status.

use super::factory::{glob_match, OverrideCall};
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

mod objects;
use objects::{ConfigSet, SeqStart, LATE_PHASES, RUN_PHASES};

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

/// A component of the UVM verification hierarchy (or, with `TreeQuery::objects`, an object).
#[derive(Serialize, Debug, Clone, Default)]
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
    /// Factory overrides: those that make `class` what it is (`applied`), in order, then those
    /// set under conditions that would change it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<OverrideStep>,
    /// An object (a configuration, register model, adapter or sequence), not a component.
    #[serde(skip_serializing_if = "is_false")]
    pub object: bool,
    /// Sequences: where they are started.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub starts: Vec<StartInfo>,
    /// Sequences started here but made elsewhere: `elsewhere`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocated: Option<String>,
    /// Notes for the tooltip (`set in config_db for ... as "cfg"`, default sequences).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The class requested when an override built another one.
    #[serde(skip)]
    pub(crate) declared: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Where a sequence is started.
#[derive(Serialize, Debug, Clone)]
pub struct StartInfo {
    /// The sequencer: a tree path (`path`), or the argument as written.
    pub on: String,
    pub path: bool,
    /// The phase or sequence method that starts it.
    pub phase: String,
    /// As [`VerifNode::conditions`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Vec<String>>,
}

/// One factory override of a node's class.
#[derive(Serialize, Debug, Clone)]
pub struct OverrideStep {
    pub from: String,
    pub to: String,
    pub instance: bool,
    /// Instance overrides: the path pattern, made full.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The method that sets it: `err_test::build_phase`.
    pub set_in: String,
    pub uri: Option<String>,
    pub line: u32,
    /// As [`VerifNode::conditions`]: the override is set only when these hold.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Vec<String>>,
    /// It makes the node's class; else it is set under conditions and would change it.
    pub applied: bool,
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
    /// Verification Hierarchy: objects (configurations, register models, adapters, sequences)
    /// are shown.
    pub objects: bool,
}

impl TreeQuery {
    pub fn new(max_nodes: usize) -> TreeQuery {
        TreeQuery { filter: None, target: None, reveal: Vec::new(), max_nodes, max_matches: 5_000, max_visits: 2_000_000, ports: true, objects: false }
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
    /// For a class: `component` or `object` (a UVM object the hierarchy shows only with objects).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
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
    /// Made by the factory (`type_id::create`), which applies overrides; else by `new`.
    factory: bool,
    /// The factory context (`create(name, parent, "contxt")`), when a literal (an empty one is the
    /// parent's: no context).
    contxt: Option<String>,
    /// The context is an expression: where the factory looks is not known.
    contxt_unknown: bool,
    /// The class is written with parameter values (`T#(8)::type_id::create`).
    params: bool,
}

/// A condition or loop around a creation, from the method it is in or a call leading to it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Guard {
    /// `if (text)`, or its `else` branch (`negated`).
    If { text: String, negated: bool },
    /// A `case` item: `case (e) labels:`.
    Case { text: String },
    /// A loop: its keyword, header tokens (resolved in `unit`), whether its body can leave
    /// early (`break`, `continue`, `return`), and whether it changes the loop variable.
    Loop { kind: &'static str, header: Vec<Token>, unit: crate::world::UnitId, exits: bool, modified: bool },
    /// The method's statements could not be followed.
    Unknown,
}

/// What a method body does that the hierarchy needs, in source order (argument values and
/// paths are filled in where it is called).
struct Summary {
    events: Vec<Event>,
    /// Names the body assigns (`n = ...`): an argument it changes no longer holds what the
    /// caller gave.
    assigned: FxHashSet<String>,
}

struct Event {
    guards: Vec<Guard>,
    what: What,
}

enum What {
    Create(Creation),
    Override(OverrideCall),
    Start(SeqStart),
    Set(ConfigSet),
    /// A call of a method of the object (`f()`, `this.f()`), or of the base class's (`super.f()`,
    /// `super.new()`): with the arguments given by name (`.n(v)`) or position.
    Call { name: String, sup: bool, args: Vec<(Option<String>, Arg)> },
}

/// An argument of a call, as far as the hierarchy needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Arg {
    /// A string literal.
    Lit(String),
    /// A name: an argument of the calling method passed on.
    Var(String),
    Other,
}

/// A creation reached while building a component, with the guards of each way it is reached
/// (an empty list: unconditionally), and the other creations of the same variable and name that
/// can run instead (`if (host) driver = A...; else driver = B...;`): UVM makes one of them.
#[derive(Debug, Clone)]
struct Made {
    body: DeclRef,
    cr: Creation,
    alts: Vec<Vec<Guard>>,
    others: Vec<Made>,
    /// The component's own overrides registered before it is made.
    seq: usize,
    /// When it is made: construction, build, or a later phase (then only objects count).
    stage: Mode,
    /// Replaced by a later creation.
    dead: bool,
    /// The creation or registration event of the component its last way is (see `Built::ev`).
    last_ev: usize,
}

/// What building a component makes: its creations, and the overrides it registers, in order;
/// with objects, also the `uvm_config_db` sets of its build and what its run-time phases start.
#[derive(Default)]
struct Built {
    /// Creations and registrations so far: a creation reached again merges with an earlier way
    /// only when nothing happened in between that could change the factory.
    ev: usize,
    made: Vec<Made>,
    /// `made` by left-hand side.
    by_lhs: FxHashMap<String, Vec<usize>>,
    regs: Vec<Reg>,
    sets: Vec<SetEv>,
    run: Vec<RunEv>,
    /// Some creations were not followed (a budget or the call depth).
    cut: bool,
}

/// A `uvm_config_db` set reached while building a component.
#[derive(Debug, Clone)]
struct SetEv {
    body: DeclRef,
    guards: Vec<Guard>,
    set: ConfigSet,
}

/// Something a run-time phase (or a sequence's method) does with sequences.
#[derive(Debug, Clone)]
struct RunEv {
    body: DeclRef,
    guards: Vec<Guard>,
    /// The phase or method it is in.
    phase: String,
    what: RunWhat,
}

#[derive(Debug, Clone)]
enum RunWhat {
    Alloc(Creation),
    /// A sequence made while building, its class already resolved.
    Made(Box<objects::MadeSeq>),
    Start(SeqStart),
    /// An override registered at run time: it applies to what is made after it.
    Override(OverrideCall),
}

/// What a scan collects: creations and overrides while building, objects after it, sequences
/// at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Construct,
    Build,
    /// A phase after build (`connect_phase` ...): objects only.
    Late(&'static str),
    Run(&'static str),
}

impl Mode {
    /// The phase (or `new`) it stands for.
    fn phase(self) -> &'static str {
        match self {
            Mode::Construct => "new",
            Mode::Build => "build_phase",
            Mode::Late(p) | Mode::Run(p) => p,
        }
    }
}

/// An override call reached while building a component.
#[derive(Debug, Clone)]
struct Reg {
    call: OverrideCall,
    body: DeclRef,
    guards: Vec<Guard>,
    /// Construction or build.
    stage: Mode,
}

/// The overrides registered with the factory, in order, by original class.
#[derive(Default, Clone)]
struct Factory {
    entries: Vec<Entry>,
    by_from: FxHashMap<(u32, u32), Vec<usize>>,
    /// Registrations replayed, and whether some were not (the model budget).
    processed: usize,
    dropped: bool,
}

impl Factory {
    fn push(&mut self, e: Entry) {
        self.by_from.entry(e.from).or_default().push(self.entries.len());
        self.entries.push(e);
    }

    /// Drops the entries registered after the first `len`.
    fn truncate(&mut self, len: usize) {
        while self.entries.len() > len {
            let Some(e) = self.entries.pop() else { break };
            if let Some(v) = self.by_from.get_mut(&e.from) {
                v.pop();
            }
        }
    }
}

/// An override registered with the factory.
#[derive(Debug, Clone)]
struct Entry {
    instance: bool,
    from: (u32, u32),
    from_class: DeclRef,
    to: DeclRef,
    /// Instance overrides: the full path pattern.
    path: String,
    replace: bool,
    /// Set without conditions: it applies for certain.
    certain: bool,
    conditions: Vec<Vec<String>>,
    set_in: String,
    span: Span,
}

/// Work shared by one request: tokens of files, summaries of method bodies, and a budget of
/// instantiated events.
#[derive(Default)]
struct Scan {
    tokens: RefCell<FxHashMap<FileId, Arc<Vec<Token>>>>,
    summaries: RefCell<FxHashMap<DeclRef, Rc<Summary>>>,
    /// The method a call reaches: (most derived class, name, index of the calling class,
    /// `super` call) to (implementation, index of its class).
    calls: RefCell<FxHashMap<(DeclRef, String, usize, bool), Option<(DeclRef, usize)>>>,
    events: Cell<usize>,
    exhausted: Cell<bool>,
    /// Events instantiated for the component being built, and whether its creations were cut
    /// (a budget or the call depth).
    component_events: Cell<usize>,
    cut: Cell<bool>,
    /// Objects are wanted: the phases after build and the run-time phases are scanned too.
    objects: bool,
    /// What building each class makes (by its most derived class).
    built: RefCell<FxHashMap<DeclRef, Rc<Built>>>,
}

/// Events instantiated per request at most.
const MAX_EVENTS: usize = 2_000_000;
/// Events instantiated per component at most.
const MAX_COMPONENT_EVENTS: usize = 20_000;
/// Nested helper calls followed at most (`super` calls do not count).
const MAX_CALL_DEPTH: usize = 8;
/// Ways to reach one creation kept at most.
const MAX_WAYS: usize = 16;

impl Scan {
    fn tokens(&self, a: &Analysis, f: FileId) -> Option<Arc<Vec<Token>>> {
        if let Some(t) = self.tokens.borrow().get(&f) {
            return Some(t.clone());
        }
        let t = a.file_tokens(f)?;
        self.tokens.borrow_mut().insert(f, t.clone());
        Some(t)
    }

    /// Counts an instantiated event; false once a budget is spent.
    fn spend(&self) -> bool {
        let n = self.events.get() + 1;
        self.events.set(n);
        let c = self.component_events.get() + 1;
        self.component_events.set(c);
        if n > MAX_EVENTS {
            self.exhausted.set(true);
        }
        if n > MAX_EVENTS || c > MAX_COMPONENT_EVENTS {
            self.cut.set(true);
            return false;
        }
        true
    }
}

/// A component (or the test) as UVM builds it: the model the view's nodes are made from.
#[derive(Default)]
struct Inst {
    name: String,
    path: String,
    /// The factory can match its path: no name on the way is computed.
    path_known: bool,
    made: Option<Made>,
    class: Option<DeclRef>,
    requested: Option<DeclRef>,
    overrides: Vec<OverrideStep>,
    alternatives: Vec<Alternative>,
    alts: Vec<Vec<Guard>>,
    kind: &'static str,
    dup: Option<u32>,
    /// Made for certain: no condition (or uncertain loop) on the way from the test.
    certain: bool,
    var: Option<(u32, u32)>,
    /// Components and ports, in the order they are made.
    children: Vec<Inst>,
    /// Objects made while it builds, with the class the factory gives them then.
    objs: Vec<ObjInst>,
    built: Option<Rc<Built>>,
    chain: Vec<DeclRef>,
    /// Sibling counts of the children (objects continue them).
    seen: FxHashMap<String, u32>,
    /// Some creations were not followed.
    cut: bool,
    /// Default sequences shown under it.
    defaults: Vec<DefaultSeq>,
}

/// An object made while a component builds.
struct ObjInst {
    made: Made,
    class: Option<DeclRef>,
    requested: Option<DeclRef>,
    overrides: Vec<OverrideStep>,
    kind: &'static str,
    /// The ways it is made, and the other classes made instead.
    alts: Vec<Vec<Guard>>,
    alternatives: Vec<Alternative>,
    /// The other creations made instead, with their classes (a sequence started later may be
    /// one of them).
    alt_made: Vec<(Made, Option<DeclRef>, Option<DeclRef>, Vec<OverrideStep>)>,
}

/// A default sequence set in `uvm_config_db`, and the component it is shown under.
struct DefaultSeq {
    set: SetEv,
    /// The full path of the sequencer it is for (a pattern when it has wildcards).
    target: String,
    phase: String,
    /// Shown under the sequencer it names (else under the component that sets it).
    placed: bool,
    /// A sequence object set (not a wrapper): the class the setter made it as.
    made_class: Option<DeclRef>,
}

/// Components, objects and override registrations modeled per test at most.
const MAX_MODEL: usize = 200_000;

/// Adds a creation, merging it with those of the same variable and name: the same creation
/// reached another way (with the factory as it was) adds that way; a later one at least as
/// unconditional as every earlier way replaces them (a derived class re-creating it);
/// otherwise it is another creation that can run instead.
fn add_made(built: &mut Built, mut m: Made) {
    built.ev += 1;
    let ev = built.ev;
    m.last_ev = ev;
    let g = m.alts[0].clone();
    let same_site = |x: &Made| x.body == m.body && x.cr.lhs_head == m.cr.lhs_head && x.cr.name == m.cr.name && x.seq == m.seq && x.stage == m.stage && x.last_ev + 1 == ev;
    let compatible = |x: &Made| !x.dead && x.stage == m.stage && (x.cr.name == m.cr.name || x.cr.name.is_none() || m.cr.name.is_none());
    let matching: Vec<usize> = built.by_lhs.get(&m.cr.lhs).map(|v| v.iter().copied().filter(|&i| compatible(&built.made[i])).collect()).unwrap_or_default();
    let Some(&first) = matching.first() else {
        built.by_lhs.entry(m.cr.lhs.clone()).or_default().push(built.made.len());
        built.made.push(m);
        return;
    };
    for &i in &matching {
        let x = &mut built.made[i];
        if same_site(x) {
            add_way(x, g);
            x.last_ev = ev;
            return;
        }
        if let Some(y) = x.others.iter_mut().find(|y| same_site(y)) {
            add_way(y, g);
            y.last_ev = ev;
            return;
        }
    }
    let made = &mut built.made;
    let superseded = matching.iter().all(|&i| made[i].alts.iter().chain(made[i].others.iter().flat_map(|o| &o.alts)).all(|a| a.starts_with(&g)));
    if superseded {
        for &i in matching.iter().skip(1) {
            made[i].dead = true;
        }
        made[first] = m;
    } else if made[first].others.len() < MAX_WAYS {
        made[first].others.push(m);
    }
}

/// Adds a way to reach a creation: an unconditional way makes it unconditional, whatever else.
fn add_way(x: &mut Made, g: Vec<Guard>) {
    if g.is_empty() {
        x.alts = vec![Vec::new()];
    } else if !x.alts.iter().any(|a| a.is_empty()) && x.alts.len() < MAX_WAYS && !x.alts.contains(&g) {
        x.alts.push(g);
    }
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

/// Methods UVM declares virtual that a testbench overrides: the common phases (and their old
/// names) and a sequence's methods. The run-time phases are in `RUN_PHASES`.
const UVM_PHASES: &[&str] = &[
    "build_phase", "connect_phase", "end_of_elaboration_phase", "start_of_simulation_phase", "extract_phase", "check_phase", "report_phase", "final_phase",
    "build", "connect", "end_of_elaboration", "start_of_simulation", "run", "extract", "check", "report",
];

/// A sequence's virtual methods.
const SEQUENCE_VIRTUALS: &[&str] = &["body", "pre_body", "post_body", "pre_start", "post_start", "pre_do", "mid_do", "post_do"];

/// Whether the tokens start by changing the variable `name` (`name = ...`, `name++`,
/// `name += ...`), or contain such a change.
fn changes(t: &[Token], name: &str) -> bool {
    (0..t.len()).any(|i| {
        let at = &t[i];
        let named = |x: &Token| x.is_ident() && x.text == name;
        let after = t.get(i + 1);
        (named(at) && !(i > 0 && t[i - 1].is_punct(".")) && after.is_some_and(|y| ["=", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>="].iter().any(|op| y.is_punct(op))))
            || ((at.is_punct("++") || at.is_punct("--")) && after.is_some_and(named))
    })
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
        let named = |r: DeclRef| NamedDef { name: self.decl(r).name.to_string(), uri: self.decl_location(r).map(|l| l.uri.to_string()), kind: None };
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
        out.class = self.enclosing_class(unit, scope).map(|c| {
            let kind = if self.derives_from(c, "uvm_component") {
                Some("component")
            } else if self.object_kind(c).is_some() {
                Some("object")
            } else {
                None
            };
            NamedDef { kind, ..named(c) }
        });
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
    pub(crate) fn in_uvm_library(&self, class: DeclRef) -> bool {
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
        let scan = Scan { objects: q.objects, ..Scan::default() };
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
                dup,
                ..Default::default()
            };
            if !w.stopped() {
                // The test as UVM builds it; run-time sequences see all the overrides of the build.
                let (model, factory) = self.model(t, &scan);
                if factory.dropped {
                    node.truncated = true;
                    w.meta.incomplete.get_or_insert("nodes");
                }
                let mut stack = vec![self.decl_key(t)];
                node.children = self.emit(&model, pin, &mut w, &mut node.truncated, &mut node.ports_hidden, &mut stack, &scan, &factory);
            }
            if matched || !node.children.is_empty() || pin.is_some() {
                out.push(node);
            }
        }
        let hint = if tests.is_empty() { self.unresolved_uvm_bases() } else { None };
        let h = VerifHierarchy { test: wanted, configured, tests: tests.iter().map(|r| self.decl(*r).name.to_string()).collect(), roots: out, hint };
        (h, w.meta)
    }

    /// How UVM builds a test: the test is constructed, then built; a component's creations (and
    /// the overrides it registers) run in order, each child constructed as it is created; then
    /// its children are built, in name order (UVM keeps them by name). The factory's overrides
    /// accumulate in that order. Returns the test and the overrides of the whole build.
    fn model(&self, test: DeclRef, scan: &Scan) -> (Inst, Factory) {
        let mut root = Inst {
            name: "uvm_test_top".into(),
            path: "uvm_test_top".into(),
            path_known: true,
            class: Some(test),
            requested: Some(test),
            kind: "test",
            certain: true,
            alts: vec![Vec::new()],
            ..Default::default()
        };
        let mut factory = Factory::default();
        let mut nodes = 0;
        let mut stack = vec![self.decl_key(test)];
        self.construct_inst(&mut root, &mut factory, &mut nodes, &mut stack, scan);
        self.build_inst(&mut root, &mut factory, &mut nodes, &mut stack, scan);
        if scan.objects {
            // The phases after build run once every component is built, bottom up.
            for phase in LATE_PHASES {
                self.late_inst(&mut root, Mode::Late(phase), &mut factory, &mut nodes, &mut stack, scan);
            }
            self.place_defaults(&mut root);
        }
        (root, factory)
    }

    /// A phase after build for a component and its children (children first).
    fn late_inst(&self, inst: &mut Inst, stage: Mode, factory: &mut Factory, nodes: &mut usize, stack: &mut Vec<(u32, u32)>, scan: &Scan) {
        let mut order: Vec<usize> = (0..inst.children.len()).collect();
        order.sort_by(|a, b| inst.children[*a].name.cmp(&inst.children[*b].name));
        for i in order {
            let child = &mut inst.children[i];
            let Some(c) = child.class.filter(|_| child.kind != "port") else { continue };
            let key = self.decl_key(c);
            if stack.contains(&key) || stack.len() >= 64 {
                continue;
            }
            stack.push(key);
            self.late_inst(child, stage, factory, nodes, stack, scan);
            stack.pop();
        }
        if let Some(built) = inst.built.clone() {
            self.stage(inst, &built, stage, factory, nodes, stack, scan);
        }
    }

    /// The default sequences the components set, each under the sequencer it names when that
    /// is one component of the tree, else under the component that sets it.
    fn place_defaults(&self, root: &mut Inst) {
        fn collect(a: &Analysis, inst: &Inst, out: &mut Vec<(String, DefaultSeq)>) {
            if let Some(built) = &inst.built {
                for s in built.sets.iter().filter(|s| s.set.field.as_deref() == Some("default_sequence")) {
                    let Some(i) = &s.set.inst else { continue };
                    let (target, phase) = match i.rsplit_once('.') {
                        Some((t, p)) if p.ends_with("_phase") => (t.to_string(), p.to_string()),
                        _ => (i.clone(), "run_phase".to_string()),
                    };
                    let full = match &s.set.ctx[..] {
                        [c] if c.is_kw("this") => if target.is_empty() { inst.path.clone() } else { format!("{}.{target}", inst.path) },
                        _ => target,
                    };
                    // A sequence object: the class the setter made it as.
                    let var = match &s.set.value[..] {
                        [v] => a.resolve_at_span(s.body.unit, v.span).map(|x| a.decl_key(x)),
                        _ => None,
                    };
                    let made_class = var.and_then(|k| inst.objs.iter().find(|o| o.kind == "sequence" && a.resolve_at_span(o.made.body.unit, o.made.cr.lhs_head).map(|x| a.decl_key(x)) == Some(k))).and_then(|o| o.class);
                    out.push((inst.path.clone(), DefaultSeq { set: s.clone(), target: full, phase, placed: false, made_class }));
                }
            }
            for c in &inst.children {
                collect(a, c, out);
            }
        }
        fn find<'a>(inst: &'a mut Inst, path: &str) -> Option<&'a mut Inst> {
            if inst.path == path {
                return Some(inst);
            }
            let child = inst.children.iter_mut().find(|c| path == c.path || path.starts_with(&format!("{}.", c.path)))?;
            find(child, path)
        }
        let mut found = Vec::new();
        collect(self, root, &mut found);
        for (setter, mut d) in found {
            let literal = !d.target.contains(['*', '?', '+']);
            if literal {
                if let Some(sqr) = find(root, &d.target).filter(|s| s.kind == "sequencer") {
                    d.placed = true;
                    sqr.defaults.push(d);
                    continue;
                }
            }
            if let Some(s) = find(root, &setter) {
                s.defaults.push(d);
            }
        }
    }

    /// Runs a component's construction.
    fn construct_inst(&self, inst: &mut Inst, factory: &mut Factory, nodes: &mut usize, stack: &mut Vec<(u32, u32)>, scan: &Scan) {
        let Some(c) = inst.class.filter(|_| inst.kind != "port") else { return };
        inst.chain = self.class_chain(c);
        let built = self.build_creations(&inst.chain, scan);
        inst.cut |= built.cut;
        inst.built = Some(built.clone());
        self.stage(inst, &built, Mode::Construct, factory, nodes, stack, scan);
    }

    /// Runs a component's build phase (and, for objects, the phases after it), then builds its
    /// children in name order.
    fn build_inst(&self, inst: &mut Inst, factory: &mut Factory, nodes: &mut usize, stack: &mut Vec<(u32, u32)>, scan: &Scan) {
        let Some(built) = inst.built.clone() else { return };
        self.stage(inst, &built, Mode::Build, factory, nodes, stack, scan);
        let mut order: Vec<usize> = (0..inst.children.len()).collect();
        order.sort_by(|a, b| inst.children[*a].name.cmp(&inst.children[*b].name));
        for i in order {
            let child = &mut inst.children[i];
            let Some(c) = child.class.filter(|_| child.kind != "port") else { continue };
            let key = self.decl_key(c);
            if stack.contains(&key) || stack.len() >= 64 {
                continue;
            }
            stack.push(key);
            self.build_inst(child, factory, nodes, stack, scan);
            stack.pop();
        }
    }

    /// The creations and override registrations of one stage of a component, in order.
    #[allow(clippy::too_many_arguments)]
    fn stage(&self, inst: &mut Inst, built: &Built, stage: Mode, factory: &mut Factory, nodes: &mut usize, stack: &mut Vec<(u32, u32)>, scan: &Scan) {
        let mut cursor = 0;
        for m in built.made.iter().filter(|m| !m.dead && m.stage == stage) {
            self.register(built, stage, m.seq, &mut cursor, inst, factory, scan);
            self.instantiate(inst, built, m, cursor, factory, nodes, stack, scan);
        }
        self.register(built, stage, built.regs.len(), &mut cursor, inst, factory, scan);
    }

    /// Registers the component's overrides of `stage` up to the `upto`-th.
    #[allow(clippy::too_many_arguments)]
    fn register(&self, built: &Built, stage: Mode, upto: usize, cursor: &mut usize, inst: &Inst, factory: &mut Factory, scan: &Scan) {
        while *cursor < upto.min(built.regs.len()) {
            let r = &built.regs[*cursor];
            *cursor += 1;
            if r.stage != stage {
                continue;
            }
            factory.processed += 1;
            if factory.processed > MAX_MODEL || factory.entries.len() >= MAX_MODEL {
                // Past the budget: what is built from here may miss overrides, and says so.
                factory.dropped = true;
                continue;
            }
            if let Some(e) = self.factory_entry(r, &inst.path, inst.certain, scan) {
                factory.push(e);
            }
        }
    }

    /// The class requested by a creation in a class of `chain`: `T` of `T::type_id::create`,
    /// else the variable's type; a type parameter of a base class (`T_ENV env`) is the type a
    /// derived class gives it.
    fn requested_class(&self, m: &Made, chain: &[DeclRef], scan: &Scan) -> Option<DeclRef> {
        let is_class = |c: &DeclRef| matches!(self.decl(*c).kind, DeclKind::Class);
        let cr = &m.cr;
        cr.class.filter(is_class).or_else(|| {
            let from_ty = cr.ty.and_then(|s| self.resolve_at_span(m.body.unit, s)).and_then(|r| self.follow_typedef(r));
            from_ty
                .and_then(|r| self.bind_type_param(r, chain, scan))
                .filter(is_class)
                .or_else(|| self.resolve_at_span(m.body.unit, cr.lhs_head).and_then(|v| self.type_of(v)).and_then(|r| self.bind_type_param(r, chain, scan)).filter(is_class))
        })
    }

    /// The class the factory makes for a creation now (with the component's overrides up to
    /// `m.seq`, registered for the moment when they are not yet), and the steps that make it.
    #[allow(clippy::too_many_arguments)]
    fn made_class(&self, inst: &Inst, built: &Built, m: &Made, cursor: usize, lookup: Option<&str>, factory: &mut Factory, scan: &Scan) -> (Option<DeclRef>, Option<DeclRef>, Vec<OverrideStep>) {
        let requested = self.requested_class(m, &inst.chain, scan);
        let Some(req) = requested.filter(|_| m.cr.factory) else { return (requested, requested, Vec::new()) };
        let len = factory.entries.len();
        let mut k = cursor;
        self.register(built, m.stage, m.seq, &mut k, inst, factory, scan);
        let (c, steps) = self.factory_resolve(factory, req, lookup, m.cr.params || self.parameterized(req));
        factory.truncate(len);
        (Some(c), requested, steps)
    }

    /// A creation of a component being built: a child (constructed now), or an object.
    #[allow(clippy::too_many_arguments)]
    fn instantiate(&self, inst: &mut Inst, built: &Built, m: &Made, cursor: usize, factory: &mut Factory, nodes: &mut usize, stack: &mut Vec<(u32, u32)>, scan: &Scan) {
        let name = m.cr.name.clone().unwrap_or_else(|| m.cr.lhs.trim_start_matches("this.").to_string());
        let path = format!("{}.{name}", inst.path);
        let path_known = inst.path_known && m.cr.name.is_some();
        // The factory's lookup path: the context when one is given, else the parent's path (an
        // object without a parent: its name alone).
        let lookup_of = |x: &Made| -> Option<String> {
            let n = x.cr.name.as_ref()?;
            if x.cr.contxt_unknown {
                return None;
            }
            match (&x.cr.contxt, x.cr.parent.as_str()) {
                (Some(c), _) => Some(format!("{c}.{n}")),
                (None, "this") => path_known.then(|| path.clone()),
                (None, "") => Some(n.clone()),
                _ => None,
            }
        };
        let (cls, requested, overrides) = self.made_class(inst, built, m, cursor, lookup_of(m).as_deref(), factory, scan);
        let object = cls.and_then(|c| self.object_kind(c));
        // Components are made in construction or build, with `this` as their parent.
        if object.is_none() && (matches!(m.stage, Mode::Late(_)) || m.cr.parent != "this") {
            return;
        }
        if object.is_some() && !scan.objects {
            return;
        }
        // Other creations of the same class only add ways to make it.
        let mut alts = m.alts.clone();
        let mut alternatives: Vec<Alternative> = Vec::new();
        let mut alt_made: Vec<(Made, Option<DeclRef>, Option<DeclRef>, Vec<OverrideStep>)> = Vec::new();
        for o in &m.others {
            let (oc, oreq, osteps) = self.made_class(inst, built, o, cursor, lookup_of(o).as_deref(), factory, scan);
            if object.is_some() {
                alt_made.push((o.clone(), oc, oreq, osteps));
            }
            if oc.map(|c| self.decl_key(c)) == cls.map(|c| self.decl_key(c)) {
                for g in &o.alts {
                    if g.is_empty() {
                        alts = vec![Vec::new()];
                    } else if !alts.iter().any(|a| a.is_empty()) && !alts.contains(g) && alts.len() < MAX_WAYS {
                        alts.push(g.clone());
                    }
                }
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
        if let Some(kind) = object {
            *nodes += 1;
            if *nodes > MAX_MODEL {
                inst.cut = true;
                return;
            }
            inst.objs.push(ObjInst { made: m.clone(), class: cls, requested, overrides, kind, alts, alternatives, alt_made });
            return;
        }
        let kind = match cls {
            Some(c) => match self.uvm_kind(c) {
                Some(k) => k,
                // An object of a class the switch does not show (an item, a library object).
                None if self.class_chain_complete(c) => return,
                None => "component",
            },
            None => "unresolved",
        };
        *nodes += 1;
        if *nodes > MAX_MODEL {
            inst.cut = true;
            return;
        }
        let dup = sibling(&mut inst.seen, &name);
        // Made for certain, and of that class for certain (no possible override).
        let certain = inst.certain && alternatives.is_empty() && conditions_text(&alts).is_empty() && self.loops_sure(&alts[0], scan) && overrides.iter().all(|o| o.applied);
        let mut child = Inst {
            name,
            path,
            path_known,
            made: Some(m.clone()),
            class: cls,
            requested,
            overrides,
            alternatives,
            alts,
            kind,
            dup,
            certain,
            var: self.resolve_at_span(m.body.unit, m.cr.lhs_head).map(|v| self.decl_key(v)),
            ..Default::default()
        };
        if let Some(c) = cls.filter(|_| kind != "port") {
            let key = self.decl_key(c);
            if !stack.contains(&key) && stack.len() < 64 {
                stack.push(key);
                self.construct_inst(&mut child, factory, nodes, stack, scan);
                stack.pop();
            }
        }
        inst.children.push(child);
    }

    /// The nodes of a component's children, depth first in the order they are made, as the
    /// query asks (limits, filter, target, revealed path); `run_factory`: the overrides of the
    /// whole build, for what the run-time phases make.
    #[allow(clippy::too_many_arguments)]
    fn emit(&self, inst: &Inst, pin: Option<&[String]>, w: &mut Walk, truncated: &mut bool, ports_hidden: &mut u32, stack: &mut Vec<(u32, u32)>, scan: &Scan, run_factory: &Factory) -> Vec<VerifNode> {
        if inst.cut {
            // Creations beyond a budget or the call depth are missing.
            *truncated = true;
            if w.meta.incomplete.is_none() {
                w.meta.incomplete = Some("nodes");
            }
        }
        let mut out = Vec::new();
        for child in &inst.children {
            // Hidden ports still count for `#2` numbering, so hiding them renames nothing.
            if child.kind == "port" && !w.q.ports {
                *ports_hidden += 1;
                continue;
            }
            let child_pin = pin_below(pin, &keyed(&child.name, child.dup));
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
            let Some(m) = &child.made else { continue };
            let loc = self.to_location(m.cr.lhs_head);
            let def_loc = child.class.and_then(|c| self.decl_location(c));
            let class_name = match child.class {
                Some(c) => self.decl(c).name.to_string(),
                None if !m.cr.ty_text.is_empty() => m.cr.ty_text.clone(),
                None => self.resolve_at_span(m.body.unit, m.cr.lhs_head).and_then(|v| self.decl(v).ty.as_ref().map(|t| t.text.to_string())).unwrap_or_default(),
            };
            let declared = child.requested.filter(|r| Some(self.decl_key(*r)) != child.class.map(|c| self.decl_key(c))).map(|r| self.decl(r).name.to_string());
            let mut node = VerifNode {
                name: child.name.clone(),
                path: child.path.clone(),
                class: class_name,
                kind: child.kind.into(),
                uri: loc.as_ref().map(|l| l.uri.to_string()),
                line: loc.as_ref().map_or(0, |l| l.range.start.line),
                character: loc.as_ref().map_or(0, |l| l.range.start.character),
                def_uri: def_loc.as_ref().map(|l| l.uri.to_string()),
                def_line: def_loc.as_ref().map_or(0, |l| l.range.start.line),
                dup: child.dup,
                conditions: conditions_text(&child.alts),
                count: self.loops_count(&child.alts[0], scan),
                loops: child.alts[0].iter().filter_map(loop_text).collect(),
                alternatives: child.alternatives.clone(),
                overrides: child.overrides.clone(),
                declared,
                ..Default::default()
            };
            // Matched before the children, so the match limit counts in tree order; an
            // overridden component also by the class requested.
            let declared = node.declared.clone().unwrap_or_default();
            let matched = match &w.q.target {
                Some(t) => {
                    let m = t.class.as_deref().is_some_and(|c| c == node.class || c == declared);
                    w.count(m)
                }
                None => w.matched(&[(&node.name, '.'), (&node.class, '.'), (&node.path, '.'), (&declared, '.')]),
            };
            if child.kind != "port" {
                node.children = self.emit(child, child_pin, w, &mut node.truncated, &mut node.ports_hidden, stack, scan, run_factory);
            }
            if matched || !node.children.is_empty() || child_pin.is_some() {
                out.push(node);
            } else if reserved {
                w.nodes_left += 1;
            }
        }
        // Objects after the components, so turning them on renames no component.
        if scan.objects && (!w.stopped() || pin.is_some()) {
            let mut seen = inst.seen.clone();
            out.extend(self.object_nodes(inst, pin, &mut seen, w, stack, truncated, scan, run_factory));
        }
        out
    }

    /// A registered override as a factory entry, its path made full from the component's
    /// path. `inst_certain`: the component is made for certain. `None` for an override UVM
    /// ignores (by name, of a class to itself).
    fn factory_entry(&self, r: &Reg, path: &str, inst_certain: bool, scan: &Scan) -> Option<Entry> {
        let c = &r.call;
        if c.by_name && self.decl_key(c.from) == self.decl_key(c.to) {
            return None;
        }
        // In a loop that never runs.
        let toks = |f: FileId| scan.tokens(self, f);
        if r.guards.iter().any(|g| matches!(g, Guard::Loop { kind, header, unit, modified: false, .. } if self.loop_count(kind, header, *unit, &toks) == Some(0))) {
            return None;
        }
        let full = if !c.instance {
            String::new()
        } else if c.relative {
            if c.path.is_empty() { path.to_string() } else { format!("{path}.{}", c.path) }
        } else {
            c.path.clone()
        };
        let conditions = conditions_text(std::slice::from_ref(&r.guards));
        let certain = inst_certain && conditions.is_empty() && self.loops_sure(&r.guards, scan) && !c.replace_unknown && !c.params && !self.parameterized(c.from);
        let class = self.enclosing_class(r.body.unit, r.body.decl).map(|k| self.decl(k).name.to_string()).unwrap_or_default();
        let method = self.decl(r.body).name.to_string();
        Some(Entry {
            instance: c.instance,
            from: self.decl_key(c.from),
            from_class: c.from,
            to: c.to,
            path: full,
            replace: c.replace,
            certain,
            conditions,
            set_in: if class.is_empty() { method } else { format!("{class}::{method}") },
            span: c.span,
        })
    }

    /// The class the factory builds for `requested` at `path` (`None`: a path the factory
    /// cannot match), as `uvm_default_factory` does: at each step a matching instance override
    /// (the first registered) wins over the type override; repeated from the result. Only
    /// overrides that apply for certain are applied (none to a class written with parameter
    /// values, `params`: which specialization they name is not known); the others are listed
    /// after them when they would change the result.
    fn factory_resolve(&self, factory: &Factory, requested: DeclRef, path: Option<&str>, params: bool) -> (DeclRef, Vec<OverrideStep>) {
        let step = |e: &Entry, applied: bool| {
            let loc = self.to_location(e.span);
            OverrideStep {
                from: self.decl(e.from_class).name.to_string(),
                to: self.decl(e.to).name.to_string(),
                instance: e.instance,
                path: e.instance.then(|| e.path.clone()),
                set_in: e.set_in.clone(),
                uri: loc.as_ref().map(|l| l.uri.to_string()),
                line: loc.as_ref().map_or(0, |l| l.range.start.line),
                conditions: e.conditions.clone(),
                applied,
            }
        };
        let certain = |_: usize, e: &Entry| e.certain && !params;
        let Some((built, chain)) = self.factory_chain(factory, requested, path, &certain) else {
            // A chain too long to follow: nothing is claimed but that.
            let name = self.decl(requested).name.to_string();
            return (requested, vec![OverrideStep { from: name, to: "?".into(), instance: false, path: None, set_in: "(an override chain too long to follow)".into(), uri: None, line: 0, conditions: Vec::new(), applied: false }]);
        };
        let mut steps: Vec<OverrideStep> = chain.iter().map(|&i| step(&factory.entries[i], true)).collect();
        // The other overrides of the classes on the way, each listed when, set, it would change
        // the class built.
        let mut seen_classes: Vec<(u32, u32)> = vec![self.decl_key(requested)];
        seen_classes.extend(chain.iter().map(|&i| self.decl_key(factory.entries[i].to)));
        let built_key = self.decl_key(built);
        let mut candidates: Vec<usize> = seen_classes.iter().flat_map(|k| factory.by_from.get(k).into_iter().flatten().copied()).filter(|&i| !certain(i, &factory.entries[i])).collect();
        candidates.sort_unstable();
        candidates.dedup();
        let mut listed = 0;
        for i in candidates.into_iter().take(256) {
            let with = |j: usize, e: &Entry| certain(j, e) || j == i;
            let changes = self.factory_chain(factory, requested, path, &with).map_or(true, |(c, _)| self.decl_key(c) != built_key);
            if changes {
                if listed == 16 {
                    steps.push(OverrideStep { from: self.decl(requested).name.to_string(), to: "?".into(), instance: false, path: None, set_in: "(more possible overrides not listed)".into(), uri: None, line: 0, conditions: Vec::new(), applied: false });
                    break;
                }
                steps.push(step(&factory.entries[i], false));
                listed += 1;
            }
        }
        (built, steps)
    }

    /// The factory's lookup with the overrides `on` accepts: the class built and the entries
    /// applied; `None` when the chain is longer than UVM would follow sensibly.
    fn factory_chain(&self, factory: &Factory, requested: DeclRef, path: Option<&str>, on: &dyn Fn(usize, &Entry) -> bool) -> Option<(DeclRef, Vec<usize>)> {
        let mut cur = requested;
        let mut seen = vec![self.decl_key(requested)];
        let mut applied = Vec::new();
        for _ in 0..64 {
            let key = self.decl_key(cur);
            let of: &[usize] = factory.by_from.get(&key).map_or(&[], |v| v.as_slice());
            let inst = of.iter().copied().find(|&i| {
                let e = &factory.entries[i];
                e.instance && on(i, e) && path.is_some_and(|p| glob_match(&e.path, p))
            });
            // The type override in effect: a later one replaces an earlier one unless it says
            // not to. One to the class itself leaves the class as it is.
            let mut ty: Option<usize> = None;
            for &i in of {
                let e = &factory.entries[i];
                if !e.instance && on(i, e) && (ty.is_none() || e.replace) {
                    ty = Some(i);
                }
            }
            let ty = ty.filter(|&i| self.decl_key(factory.entries[i].to) != key);
            let Some(i) = inst.or(ty) else { return Some((cur, applied)) };
            let e = &factory.entries[i];
            let to = self.decl_key(e.to);
            if to == key {
                return Some((cur, applied));
            }
            applied.push(i);
            cur = e.to;
            if seen.contains(&to) {
                // A loop: UVM reports it and builds the class where it closes.
                return Some((cur, applied));
            }
            seen.push(to);
        }
        None
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
    fn loops_count(&self, guards: &[Guard], scan: &Scan) -> Option<String> {
        let mut n: Option<u64> = Some(1);
        let mut early = false;
        let mut any = false;
        let toks = |f: FileId| scan.tokens(self, f);
        for g in guards {
            if let Guard::Loop { kind, header, unit, exits, modified } = g {
                any = true;
                early |= *exits;
                let c = if *modified { None } else { self.loop_count(kind, header, *unit, &toks) };
                n = n.zip(c).and_then(|(a, b)| a.checked_mul(b));
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

    /// Whether the loops among `guards` surely run (each a constant number of times, at least
    /// once, without leaving early).
    fn loops_sure(&self, guards: &[Guard], scan: &Scan) -> bool {
        let toks = |f: FileId| scan.tokens(self, f);
        guards.iter().all(|g| match g {
            Guard::Loop { kind, header, unit, exits, modified } => !exits && !modified && self.loop_count(kind, header, *unit, &toks).is_some_and(|n| n >= 1),
            _ => true,
        })
    }

    /// The component creations of a class (`derived_first`: it and its base classes, most
    /// derived first), in the order they run: its construction (the most derived `new`, with
    /// its base class's first, as `super.new()` runs), then its build phase (the most
    /// derived `build_phase`, with `super.build_phase()` where it is called; the UVM library's
    /// calls the most derived `build()`). Methods of the class they call are followed: large
    /// testbenches build in helpers. `scan.cut` tells whether some were not followed.
    fn build_creations(&self, derived_first: &[DeclRef], scan: &Scan) -> Rc<Built> {
        let Some(&first) = derived_first.first() else { return Rc::default() };
        if let Some(b) = scan.built.borrow().get(&first) {
            return b.clone();
        }
        let mut made = Built::default();
        let mut stack = Vec::new();
        let none = FxHashMap::default();
        scan.component_events.set(0);
        scan.cut.set(false);
        if let Some((body, at)) = self.method_from("new", 0, derived_first) {
            self.run_method(body, at, derived_first, &none, &[], 0, &mut stack, scan, &mut made, Mode::Construct);
        }
        if let Some((body, at)) = self.call_target("build_phase", 0, false, derived_first, scan).or_else(|| self.call_target("build", 0, false, derived_first, scan)) {
            self.run_method(body, at, derived_first, &none, &[], 0, &mut stack, scan, &mut made, Mode::Build);
        }
        if scan.objects {
            for phase in LATE_PHASES {
                if let Some((body, at)) = self.call_target(phase, 0, false, derived_first, scan) {
                    self.run_method(body, at, derived_first, &none, &[], 0, &mut stack, scan, &mut made, Mode::Late(phase));
                }
            }
            for phase in RUN_PHASES {
                // `run_phase` of the UVM library calls the old `run()`.
                let target = self.call_target(phase, 0, false, derived_first, scan).or_else(|| if *phase == "run_phase" { self.call_target("run", 0, false, derived_first, scan) } else { None });
                if let Some((body, at)) = target {
                    self.run_method(body, at, derived_first, &none, &[], 0, &mut stack, scan, &mut made, Mode::Run(phase));
                }
            }
        }
        made.cut = scan.cut.get();
        let made = Rc::new(made);
        scan.built.borrow_mut().insert(first, made.clone());
        made
    }

    /// Adds the creations of a method body (of `derived_first[at]`) and of the methods it calls.
    /// `env`: string literals given for its arguments; `outer`: the guards of the call; `depth`:
    /// helper calls on the way (`super` calls do not count, so long class chains are followed).
    #[allow(clippy::too_many_arguments)]
    fn run_method(&self, body: DeclRef, at: usize, derived_first: &[DeclRef], env: &FxHashMap<String, String>, outer: &[Guard], depth: usize, stack: &mut Vec<DeclRef>, scan: &Scan, made: &mut Built, mode: Mode) {
        // A spent budget stops everything; the call depth only this call.
        if stack.contains(&body) || scan.exhausted.get() || scan.component_events.get() > MAX_COMPONENT_EVENTS {
            return;
        }
        if depth > MAX_CALL_DEPTH || stack.len() >= 64 {
            scan.cut.set(true);
            return;
        }
        stack.push(body);
        let s = self.summary(body, scan);
        // The base class's constructor runs first: `super.new()` must be a constructor's first
        // statement, and is implied when it is not written.
        let explicit_super_new = s.events.iter().any(|e| matches!(&e.what, What::Call { name, sup: true, .. } if name == "new"));
        if self.decl(body).name == "new" && !explicit_super_new {
            if let Some((b, i)) = self.call_target("new", at, true, derived_first, scan) {
                self.run_method(b, i, derived_first, &FxHashMap::default(), outer, depth, stack, scan, made, mode);
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
                        cr.name = cr.name_arg.as_ref().filter(|a| !s.assigned.contains(*a)).and_then(|a| env.get(a)).cloned();
                    }
                    match mode {
                        Mode::Run(phase) => made.run.push(RunEv { body, guards, phase: phase.to_string(), what: RunWhat::Alloc(cr) }),
                        _ => {
                            let seq = made.regs.len();
                            add_made(made, Made { body, cr, alts: vec![guards], others: Vec::new(), seq, stage: mode, dead: false, last_ev: 0 });
                        }
                    }
                }
                What::Override(call) => match mode {
                    Mode::Run(phase) => made.run.push(RunEv { body, guards, phase: phase.to_string(), what: RunWhat::Override(call.clone()) }),
                    _ => {
                        made.ev += 1;
                        made.regs.push(Reg { call: call.clone(), body, guards, stage: mode });
                    }
                },
                What::Start(st) => {
                    if let Mode::Run(phase) = mode {
                        made.run.push(RunEv { body, guards, phase: phase.to_string(), what: RunWhat::Start(st.clone()) });
                    }
                }
                What::Set(set) => {
                    if !matches!(mode, Mode::Run(_)) {
                        made.sets.push(SetEv { body, guards, set: set.clone() });
                    }
                }
                What::Call { name, sup, args } => {
                    let Some((callee, ci)) = self.call_target(name, at, *sup, derived_first, scan) else { continue };
                    let callee_env = self.literal_args(callee, args, env, &s.assigned);
                    let d = if *sup { depth } else { depth + 1 };
                    self.run_method(callee, ci, derived_first, &callee_env, &guards, d, stack, scan, made, mode);
                }
            }
        }
        stack.pop();
    }

    /// The method a call made in a method of `derived_first[at]` runs: `super.name()` (`sup`)
    /// looks in the base classes, without virtual dispatch (the UVM library's `build_phase` calls
    /// `build()`); `name()` is dispatched. Cached per request.
    fn call_target(&self, name: &str, at: usize, sup: bool, derived_first: &[DeclRef], scan: &Scan) -> Option<(DeclRef, usize)> {
        let key = (*derived_first.first()?, name.to_string(), at, sup);
        if let Some(t) = scan.calls.borrow().get(&key) {
            return *t;
        }
        let t = if sup {
            self.method_from(name, at + 1, derived_first).or_else(|| match name {
                "build_phase" => self.call_target("build", 0, false, derived_first, scan),
                "run_phase" => self.call_target("run", 0, false, derived_first, scan),
                _ => None,
            })
        } else {
            self.dispatch(name, at, derived_first)
        };
        scan.calls.borrow_mut().insert(key, t);
        t
    }

    /// The string literals a call gives the arguments of `callee`, by name: written in the call,
    /// or passed on from the caller's (`env`).
    fn literal_args(&self, callee: DeclRef, args: &[(Option<String>, Arg)], env: &FxHashMap<String, String>, assigned: &FxHashSet<String>) -> FxHashMap<String, String> {
        let u = self.unit(callee.unit);
        let formals: Vec<&str> = u.parsed.decl(callee.decl).children.iter().map(|c| u.parsed.decl(*c)).filter(|d| d.kind == DeclKind::Argument).map(|d| d.name.as_str()).collect();
        let mut out = FxHashMap::default();
        for (i, (name, value)) in args.iter().enumerate() {
            let formal = match name {
                Some(n) => Some(n.as_str()),
                None => formals.get(i).copied(),
            };
            let value = match value {
                Arg::Lit(v) => Some(v.clone()),
                Arg::Var(v) if !assigned.contains(v) => env.get(v).cloned(),
                Arg::Var(_) => None,
                Arg::Other => None,
            };
            if let (Some(f), Some(v)) = (formal, value) {
                out.insert(f.to_string(), v);
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
        // Each frame as a guard, once.
        let frames: Vec<Guard> = map
            .as_ref()
            .map(|m| {
                m.frames
                    .iter()
                    .map(|f| match &f.kind {
                        FrameKind::If { cond, negated } => Guard::If { text: tokens_text(&toks[cond.0..cond.1]), negated: *negated },
                        FrameKind::Case { expr, labels } => {
                            let labels = labels.map_or_else(|| "default".to_string(), |l| tokens_text(&toks[l.0..l.1]));
                            Guard::Case { text: format!("{}: {labels}", tokens_text(&toks[expr.0..expr.1])) }
                        }
                        FrameKind::Loop { kind, header, body: b } => {
                            let inner = &toks[b.0..b.1.min(toks.len())];
                            let header = toks[header.0..header.1].to_vec();
                            // `for (i = 0; ...)` whose body changes `i`.
                            let var = if *kind == "for" { super::loops::for_variable(&header) } else { None };
                            let modified = var.is_some_and(|v| changes(inner, &v));
                            Guard::Loop { kind, header, unit: body.unit, exits: inner.iter().any(|x| x.is_kw("break") || x.is_kw("continue") || x.is_kw("return")), modified }
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let guards_at = |k: usize| -> Vec<Guard> {
            match &map {
                None => vec![Guard::Unknown],
                Some(m) => m.frame_ids_at(k).into_iter().map(|i| frames[i as usize].clone()).collect(),
            }
        };
        let mut events: Vec<(usize, Event)> = Vec::new();
        let creations = scan_creations(&toks).into_iter().chain(self.factory_calls(body, &toks, scan));
        for (k, cr) in creations {
            events.push((k, Event { guards: guards_at(k), what: What::Create(cr) }));
        }
        for (k, name, sup, args) in method_calls(&toks) {
            events.push((k, Event { guards: guards_at(k), what: What::Call { name, sup, args } }));
        }
        for (k, call) in self.override_calls(body, &toks) {
            events.push((k, Event { guards: guards_at(k), what: What::Override(call) }));
        }
        for (k, st) in objects::sequence_starts(&toks) {
            events.push((k, Event { guards: guards_at(k), what: What::Start(st) }));
        }
        for (k, set) in objects::config_sets(&toks) {
            events.push((k, Event { guards: guards_at(k), what: What::Set(set) }));
        }
        events.sort_by_key(|(k, _)| *k);
        // After the header (whose argument defaults are not assignments).
        let body_start = toks.iter().position(|x| x.is_kw("function") || x.is_kw("task")).map_or(0, |k| {
            let mut j = k;
            while j < toks.len() && !toks[j].is_punct(";") {
                j = if toks[j].is_punct("(") { skip_group(&toks, j) } else { j + 1 };
            }
            j
        });
        let assigned = (body_start..toks.len()).filter(|&i| toks[i].is_ident() && changes(&toks[i..(i + 2).min(toks.len())], &toks[i].text)).map(|i| toks[i].text.to_string()).collect();
        let s = Rc::new(Summary { events: events.into_iter().map(|(_, e)| e).collect(), assigned });
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
                Creation { lhs: tokens_text(&t[lhs_start..i]), lhs_head: t[head].span, ty: None, ty_text: cr.ty_text.clone(), name, name_arg: None, parent: "this".into(), class, factory: cr.factory, contxt: None, contxt_unknown: false, params: cr.params },
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
        if derived_first[i..].iter().any(|c| methods(c).iter().any(|m| self.decl(*m).has(dflags::VIRTUAL))) || self.virtual_in_library(name, derived_first) {
            (m, i) = find(0)?;
        }
        Some((self.implementation_of(m).unwrap_or(m), i))
    }

    /// Whether a class has parameters: which specialization a type names may not be known.
    pub(crate) fn parameterized(&self, class: DeclRef) -> bool {
        let u = self.unit(class.unit);
        u.parsed.decl(class.decl).children.iter().any(|c| matches!(u.parsed.decl(*c).kind, DeclKind::Parameter | DeclKind::TypeParam) && !u.parsed.decl(*c).has(dflags::FROM_MACRO))
    }

    /// Whether the UVM library classes above `derived_first` declare `name` virtual (then every
    /// override is virtual too, written so or not). The phase methods (and their old names) are
    /// virtual in every UVM and OVM version, also without the library's source.
    fn virtual_in_library(&self, name: &str, derived_first: &[DeclRef]) -> bool {
        // Phase methods in components, sequence methods in sequences.
        let first = derived_first.first().copied();
        let family = |base: &str| first.is_some_and(|c| self.derives_from(c, base));
        if (UVM_PHASES.contains(&name) || RUN_PHASES.contains(&name)) && family("uvm_component") {
            return true;
        }
        if SEQUENCE_VIRTUALS.contains(&name) && family("uvm_sequence_base") {
            return true;
        }
        let mut cur = derived_first.last().and_then(|c| self.class_base(*c));
        for _ in 0..32 {
            let Some(c) = cur else { break };
            let u = self.unit(c.unit);
            if u.parsed.lookup_member(c.decl, name).iter().any(|&m| {
                let d = u.parsed.decl(m);
                matches!(d.kind, DeclKind::Function | DeclKind::Task) && d.has(dflags::VIRTUAL)
            }) {
                return true;
            }
            cur = self.class_base(c);
        }
        false
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
    let mut params = false;
    let factory = !t.get(k).is_some_and(|x| x.is_kw("new"));
    if !factory {
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
                params = true;
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
    // `x = new;`: an object made without arguments.
    let args = match t.get(k) {
        Some(x) if x.is_punct("(") => call_args(t, k),
        Some(x) if !factory && x.is_punct(";") => Vec::new(),
        _ => return None,
    };
    // Components get their parent as the second argument: `this`, or a name (an argument of a
    // factory method). Objects have none, or one for the factory's context.
    let parent = match args.get(1) {
        Some(a) if a.len() == 1 && (a[0].is_kw("this") || a[0].is_ident()) => a[0].text.to_string(),
        Some(a) if a.len() == 1 && a[0].is_kw("null") => String::new(),
        Some(_) => return None,
        None => String::new(),
    };
    let first = args.first().filter(|a| a.len() == 1);
    let name = first.filter(|a| a[0].kind == TokKind::String).map(|a| a[0].text.trim_matches('"').to_string());
    let name_arg = first.filter(|a| a[0].is_ident()).map(|a| a[0].text.to_string());
    let (contxt, contxt_unknown) = match args.get(2) {
        None => (None, false),
        Some([s]) if s.kind == TokKind::String => {
            let c = s.text.trim_matches('"');
            ((!c.is_empty()).then(|| c.to_string()), false)
        }
        Some(_) => (None, true),
    };
    Some(Creation { lhs, lhs_head, ty, ty_text, name, name_arg, parent, class: None, factory, contxt, contxt_unknown, params })
}

/// Calls of the object's own methods in a method body (`f(...)`, `this.f(...)`) and of its base
/// class's (`super.f(...)`; not `obj.f(...)` or `pkg::f(...)`), with the index of each, whether
/// it is a `super` call, and its arguments: the name of each given by name, and its value when
/// it is a string literal.
#[allow(clippy::type_complexity)]
fn method_calls(t: &[Token]) -> Vec<(usize, String, bool, Vec<(Option<String>, Arg)>)> {
    let mut out = Vec::new();
    for i in 1..t.len().saturating_sub(1) {
        let sup = t[i - 1].is_punct(".") && i >= 2 && t[i - 2].is_kw("super");
        // `new` is a keyword: only `super.new(...)` is a call of it.
        if !(t[i].is_ident() || sup && t[i].is_kw("new")) || !t[i + 1].is_punct("(") {
            continue;
        }
        let own = if t[i - 1].is_punct(".") { sup || (i >= 2 && t[i - 2].is_kw("this")) } else { !t[i - 1].is_punct("::") };
        if !own {
            continue;
        }
        let literal = |a: &[Token]| match a {
            [x] if x.kind == TokKind::String => Arg::Lit(x.text.trim_matches('"').to_string()),
            [x] if x.is_ident() => Arg::Var(x.text.to_string()),
            _ => Arg::Other,
        };
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
pub(crate) fn skip_group(t: &[Token], open: usize) -> usize {
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
pub(crate) fn call_args(t: &[Token], open: usize) -> Vec<&[Token]> {
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

pub(crate) fn tokens_text(t: &[Token]) -> String {
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
