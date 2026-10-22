//! Data for the custom views: compiled files, design hierarchy, including files, status.

use crate::world::{Analysis, DeclRef, UnitKind};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, RefKind};
use dvi_syntax::{FileId, SourceProvider, Span, TokKind, Token};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;

#[derive(Serialize, Debug, Clone)]
pub struct FileNode {
    pub path: String,
    pub uri: String,
    /// `top`, `include`, `library` or `non-top`.
    pub kind: String,
    pub children: Vec<FileNode>,
    pub problems: usize,
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
        let mut out = Vec::new();
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
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
            out.push(self.file_node(u.root, kind, &by_parent, &mut visited));
        }
        out
    }

    fn file_node(&self, f: FileId, kind: &str, by_parent: &FxHashMap<FileId, Vec<FileId>>, visited: &mut FxHashSet<FileId>) -> FileNode {
        let path = self.vfs.path(f);
        let mut children = Vec::new();
        if visited.insert(f) {
            if let Some(cs) = by_parent.get(&f) {
                for c in cs {
                    children.push(self.file_node(*c, "include", by_parent, visited));
                }
            }
        }
        FileNode { path: path.display().to_string(), uri: super::url_of(&path).to_string(), kind: kind.into(), children, problems: 0 }
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
        let tops: Vec<DeclRef> = match top {
            Some(t) => self.global.definitions.get(t).and_then(|v| v.first().copied()).into_iter().collect(),
            None => self.design_tops(),
        };
        let mut budget = max_nodes;
        tops.into_iter()
            .map(|t| {
                let d = self.decl(t);
                let loc = self.decl_location(t);
                let mut node = HierNode {
                    name: d.name.to_string(),
                    definition: d.name.to_string(),
                    kind: d.kind.label().into(),
                    uri: loc.as_ref().map(|l| l.uri.to_string()),
                    line: loc.as_ref().map_or(0, |l| l.range.start.line),
                    character: loc.as_ref().map_or(0, |l| l.range.start.character),
                    def_uri: loc.as_ref().map(|l| l.uri.to_string()),
                    def_line: loc.as_ref().map_or(0, |l| l.range.start.line),
                    children: Vec::new(),
                    truncated: false,
                };
                let mut stack = vec![self.decl_key(t)];
                node.children = self.hier_children(t, &mut budget, &mut stack, &mut node.truncated);
                node
            })
            .collect()
    }

    fn hier_children(&self, scope: DeclRef, budget: &mut usize, stack: &mut Vec<(u32, u32)>, truncated: &mut bool) -> Vec<HierNode> {
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
                    if *budget == 0 {
                        *truncated = true;
                        return out;
                    }
                    *budget -= 1;
                    let cr = DeclRef { unit: scope.unit, decl: c };
                    let def = self.type_of(cr);
                    let loc = self.decl_location(cr);
                    let def_loc = def.and_then(|m| self.decl_location(m));
                    let def_name = d.ty.as_ref().map(|t| t.text.split(|ch: char| ch == ' ' || ch == '#').next().unwrap_or("").to_string()).unwrap_or_default();
                    let mut node = HierNode {
                        name: d.name.to_string(),
                        definition: def.map(|m| self.decl(m).name.to_string()).unwrap_or(def_name),
                        kind: def.map(|m| self.decl(m).kind.label().to_string()).unwrap_or_else(|| "unresolved".into()),
                        uri: loc.as_ref().map(|l| l.uri.to_string()),
                        line: loc.as_ref().map_or(0, |l| l.range.start.line),
                        character: loc.as_ref().map_or(0, |l| l.range.start.character),
                        def_uri: def_loc.as_ref().map(|l| l.uri.to_string()),
                        def_line: def_loc.as_ref().map_or(0, |l| l.range.start.line),
                        children: Vec::new(),
                        truncated: false,
                    };
                    if let Some(m) = def {
                        let key = self.decl_key(m);
                        if !stack.contains(&key) && stack.len() < 64 {
                            stack.push(key);
                            node.children = self.hier_children(m, budget, stack, &mut node.truncated);
                            stack.pop();
                        }
                    }
                    out.push(node);
                }
                DeclKind::Generate | DeclKind::Block => {
                    // Generate blocks: show named ones as scopes, flatten unnamed.
                    let gr = DeclRef { unit: scope.unit, decl: c };
                    let kids = self.hier_children(gr, budget, stack, truncated);
                    if kids.is_empty() {
                        continue;
                    }
                    if d.kind == DeclKind::Generate {
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
        let mut budget = max_nodes;
        let roots = roots
            .into_iter()
            .map(|t| {
                let d = self.decl(t);
                let loc = self.decl_location(t);
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
                };
                let mut stack = vec![self.decl_key(t)];
                node.children = self.component_children(t, "uvm_test_top", &mut budget, &mut stack, &mut node.truncated);
                node
            })
            .collect();
        VerifHierarchy { test: wanted, configured, tests: tests.iter().map(|r| self.decl(*r).name.to_string()).collect(), roots }
    }

    fn component_children(&self, class: DeclRef, path: &str, budget: &mut usize, stack: &mut Vec<(u32, u32)>, truncated: &mut bool) -> Vec<VerifNode> {
        // Base classes first: `super.build_phase()` usually runs before the derived code.
        let mut chain = Vec::new();
        let mut cur = Some(class);
        while let Some(c) = cur {
            if chain.len() >= 32 || self.in_uvm_library(c) {
                break;
            }
            chain.push(c);
            cur = self.class_base(c);
        }
        // Methods are looked up from the most derived class, as a virtual call would be.
        let derived_first = chain.clone();
        chain.reverse();
        // (body unit, creation); a derived class creating the same variable replaces it.
        let mut made: Vec<(DeclRef, Creation)> = Vec::new();
        let mut scanned: FxHashMap<DeclRef, usize> = FxHashMap::default();
        for (k, c) in chain.into_iter().enumerate() {
            let at = derived_first.len() - 1 - k;
            for method in ["new", "build", "build_phase"] {
                let u = self.unit(c.unit);
                for &m in u.parsed.lookup_member(c.decl, method) {
                    let mr = DeclRef { unit: c.unit, decl: m };
                    if !matches!(self.decl(mr).kind, DeclKind::Function | DeclKind::Task) {
                        continue;
                    }
                    let body = self.implementation_of(mr).unwrap_or(mr);
                    self.scan_build_method(body, at, &derived_first, &mut scanned, &mut made, 0);
                }
            }
        }
        let mut out = Vec::new();
        for (body, cr) in made {
            // The created class: `T` of `T::type_id::create`, else the variable's type. A type
            // parameter of a base class (`T_ENV env`) is the type a derived class gives it.
            let is_class = |c: &DeclRef| matches!(self.decl(*c).kind, DeclKind::Class);
            let from_ty = cr.ty.and_then(|s| self.resolve_at_span(body.unit, s)).and_then(|r| self.follow_typedef(r));
            let from_var = || self.resolve_at_span(body.unit, cr.lhs_head).and_then(|v| self.type_of(v));
            let cls = from_ty
                .and_then(|r| self.bind_type_param(r, &derived_first))
                .filter(is_class)
                .or_else(|| from_var().and_then(|r| self.bind_type_param(r, &derived_first)).filter(is_class));
            let kind = match cls {
                Some(c) => match self.uvm_kind(c) {
                    Some(k) => k,
                    // An object created with a parent (a configuration): not a component.
                    None if self.class_chain_complete(c) => continue,
                    None => "component",
                },
                None => "unresolved",
            };
            if *budget == 0 {
                *truncated = true;
                break;
            }
            *budget -= 1;
            let name = cr.name.clone().unwrap_or_else(|| cr.lhs.trim_start_matches("this.").to_string());
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
            };
            if let Some(c) = cls.filter(|_| kind != "port") {
                let key = self.decl_key(c);
                if !stack.contains(&key) && stack.len() < 64 {
                    stack.push(key);
                    node.children = self.component_children(c, &child_path, budget, stack, &mut node.truncated);
                    stack.pop();
                }
            }
            out.push(node);
        }
        out
    }

    /// The type a type parameter of a class in `derived_first` (a class and its bases, most
    /// derived first) stands for: the value the class below it gives in `extends B#(...)`
    /// (named or by position, possibly passing on its own parameter), else its default.
    /// Other declarations are returned as they are.
    fn bind_type_param(&self, mut r: DeclRef, derived_first: &[DeclRef]) -> Option<DeclRef> {
        for _ in 0..16 {
            let d = self.decl(r);
            if d.kind != DeclKind::TypeParam {
                return Some(r);
            }
            let owner = DeclRef { unit: r.unit, decl: d.parent? };
            let key = self.decl_key(owner);
            let at = derived_first.iter().position(|c| self.decl_key(*c) == key);
            let given = match at {
                Some(i) if i > 0 => self.extends_param_value(derived_first[i - 1], owner, r),
                _ => None,
            };
            let next = given.or_else(|| self.type_param_default(r))?;
            r = self.follow_typedef(next)?;
        }
        None
    }

    /// The type `derived` gives the type parameter `param` of its base `base` in
    /// `extends base#(...)`, or through a typedef (`typedef base#(...) base_t;`,
    /// `extends base_t`).
    fn extends_param_value(&self, derived: DeclRef, base: DeclRef, param: DeclRef) -> Option<DeclRef> {
        // The declaration whose header names the base type, and the keyword before that type.
        let (mut src, mut kw) = (derived, "extends");
        for _ in 0..8 {
            let d = self.decl(src);
            if d.has(dflags::FROM_MACRO) {
                return None;
            }
            let toks = self.file_tokens(d.span.file)?;
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
    fn param_value_in(&self, unit: crate::world::UnitId, args: Vec<&[Token]>, base: DeclRef, param: DeclRef) -> Option<DeclRef> {
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
        self.type_name_in(unit, value)
    }

    /// The default type of a type parameter (`type T = my_env`).
    fn type_param_default(&self, param: DeclRef) -> Option<DeclRef> {
        let d = self.decl(param);
        if d.has(dflags::FROM_MACRO) {
            return None;
        }
        let toks = self.file_tokens(d.span.file)?;
        let lo = toks.partition_point(|t| t.span.start < d.span.start);
        let hi = toks.partition_point(|t| t.span.start < d.span.end);
        let t = &toks[lo..hi];
        let eq = t.iter().position(|x| x.is_punct("="))?;
        self.type_name_in(param.unit, &t[eq + 1..])
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

    /// The component creations of a build method, and of the methods of the same class it
    /// calls (`build_envs()`, `this.build_agents()`), in the order they run: large testbenches
    /// build in helpers. `at`: the index in `derived_first` of the class the method is in;
    /// `scanned`: the shallowest call depth each method was scanned at.
    #[allow(clippy::too_many_arguments)]
    fn scan_build_method(&self, body: DeclRef, at: usize, derived_first: &[DeclRef], scanned: &mut FxHashMap<DeclRef, usize>, made: &mut Vec<(DeclRef, Creation)>, depth: usize) {
        match scanned.get(&body) {
            Some(&d) if d <= depth => return,
            None if scanned.len() >= 256 => return,
            _ => {}
        }
        scanned.insert(body, depth);
        let toks = self.body_tokens(body);
        let creations = scan_creations(&toks);
        let calls = if depth < 8 { called_methods(&toks, self.decl(body).name.as_str()) } else { Vec::new() };
        let (mut ci, mut ki) = (creations.into_iter().peekable(), calls.into_iter().peekable());
        loop {
            let creation_first = match (ci.peek(), ki.peek()) {
                (Some((a, _)), Some((b, _))) => a < b,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if creation_first {
                let Some((_, cr)) = ci.next() else { break };
                // Creating the same variable again (a derived class, a later step) replaces the
                // component, unless the two are given different names.
                match made.iter_mut().find(|(_, x)| x.lhs == cr.lhs && (x.name == cr.name || x.name.is_none() || cr.name.is_none())) {
                    Some(slot) => *slot = (body, cr),
                    None => made.push((body, cr)),
                }
            } else {
                let Some((_, name)) = ki.next() else { break };
                if let Some((callee, callee_at)) = self.dispatch(&name, at, derived_first) {
                    self.scan_build_method(callee, callee_at, derived_first, scanned, made, depth + 1);
                }
            }
        }
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
    fn body_tokens(&self, body: DeclRef) -> Vec<Token> {
        let d = self.decl(body);
        if d.has(dflags::FROM_MACRO) || d.has(dflags::PROTOTYPE) {
            return Vec::new();
        }
        let Some(toks) = self.file_tokens(d.span.file) else { return Vec::new() };
        let lo = toks.partition_point(|t| t.span.start < d.span.start);
        let hi = toks.partition_point(|t| t.span.start < d.span.end);
        let off: Vec<Span> = self.unit(body.unit).inactive.iter().filter(|s| s.file == d.span.file && s.end > d.span.start && s.start < d.span.end).copied().collect();
        toks[lo..hi].iter().filter(|t| !off.iter().any(|s| s.start <= t.span.start && t.span.start < s.end)).cloned().collect()
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
        // Right-hand side.
        let mut k = i + 1;
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
                continue;
            }
            if n >= 3 {
                let ti = idents[n - 3];
                ty = Some(t[ti].span);
                ty_text = t[ti].text.to_string();
            }
        }
        if !t.get(k).is_some_and(|x| x.is_punct("(")) {
            continue;
        }
        let args = call_args(t, k);
        // Components get their parent as the second argument.
        let parent_is_this = args.get(1).is_some_and(|a| a.len() == 1 && a[0].is_kw("this"));
        if !parent_is_this {
            continue;
        }
        let name = args.first().filter(|a| a.len() == 1 && a[0].kind == TokKind::String).map(|a| a[0].text.trim_matches('"').to_string());
        out.push((lhs_start, Creation { lhs: tokens_text(&t[lhs_start..i]), lhs_head: t[head].span, ty, ty_text, name }));
    }
    out
}

/// Methods called on the object itself in a method body (`f(...)`, `this.f(...)`; not
/// `obj.f(...)`, `super.f(...)` or `pkg::f(...)`), each with the index of its first call.
fn called_methods(t: &[Token], own: &str) -> Vec<(usize, String)> {
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    let mut out = Vec::new();
    for i in 1..t.len().saturating_sub(1) {
        if !t[i].is_ident() || !t[i + 1].is_punct("(") {
            continue;
        }
        let this_call = if t[i - 1].is_punct(".") { i >= 2 && t[i - 2].is_kw("this") } else { !t[i - 1].is_punct("::") };
        if this_call && t[i].text != own && seen.insert(t[i].text.as_str()) {
            out.push((i, t[i].text.to_string()));
        }
    }
    out
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
        s.push_str(&x.text);
    }
    s
}
