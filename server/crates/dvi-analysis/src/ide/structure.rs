//! The Structure view: one file's declarations grouped by what they are, with the members a
//! class inherits, the processes of a module, the file's includes and defines, and the code
//! `ifdef` leaves out.

use super::filter::Filter;
use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{dflags, Decl, DeclId, DeclKind};
use dvi_syntax::{FileId, SourceProvider, Span, Token};
use lsp_types::Location;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;

/// A node of the Structure view.
#[derive(Serialize, Debug, Clone, Default)]
pub struct StructNode {
    /// Unique in the tree, and the same from one request to the next for the same code.
    pub id: String,
    pub name: String,
    /// What it is: `module`, `class`, `field`, `method`, `process`, ...; `group` for a group.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The source the node covers (none for groups): the view follows the editor with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extent: Option<Location>,
    /// Where a click goes (the name; an implementation; an included file).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Location>,
    /// Inherited members: the class they are declared in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherited_from: Option<String>,
    /// Methods: `overrides base::f`, `hides base::f`, `overridden in derived`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark: Option<String>,
    /// Code `ifdef` leaves out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inactive: bool,
    /// Groups of inherited members: shown closed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub collapsed: bool,
    pub children: Vec<StructNode>,
}

/// The Structure of a file.
#[derive(Serialize, Debug, Default)]
pub struct StructTree {
    /// `ok`; `not-compiled` (the file is not in the build: what the editor's own analysis of it
    /// finds); `none` (no analysis of the file).
    pub status: &'static str,
    /// The file the declarations are seen through (the top file of the unit compiling it), and
    /// how many other units compile it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub others: usize,
    /// Nodes were left out (the node limit).
    pub incomplete: bool,
    pub nodes: Vec<StructNode>,
}

/// Inherited members listed per class at most.
const MAX_INHERITED: usize = 2_000;

/// What is being built: the unit, the file and the node budget.
struct Build<'a> {
    a: &'a Analysis,
    unit: UnitId,
    file: FileId,
    left: usize,
    incomplete: bool,
}

impl Analysis {
    /// The Structure of `file` (`filter`: only the nodes whose name matches, with their
    /// ancestors and their contents).
    pub fn structure(&self, file: FileId, filter: Option<&Filter>, max_nodes: usize) -> StructTree {
        let units = self.units_for(file);
        let Some(&unit) = units.first() else { return StructTree { status: "none", ..Default::default() } };
        let u = self.unit(unit);
        let mut b = Build { a: self, unit, file, left: max_nodes, incomplete: false };
        let mut nodes = Vec::new();
        // Includes, defines and top-level declarations, in source order.
        let mut items: Vec<(u32, StructNode)> = Vec::new();
        for inc in u.includes.iter().filter(|i| i.span.file == file) {
            let target = inc.resolved.map(|f| Location::new(super::url_of(&self.vfs.path(f)), Default::default()));
            items.push((inc.span.start, b.leaf(&format!("`include \"{}\"", inc.path_text), "include", None, inc.span, target)));
        }
        for m in u.macro_defs.iter().filter(|m| m.name_span.file == file) {
            let detail = m.params.as_ref().map(|ps| format!("({})", ps.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")));
            items.push((m.name_span.start, b.leaf(&format!("`{}", m.name), "define", detail, m.name_span, None)));
        }
        if let Some(tops) = u.parsed.file_tops.get(&file) {
            for &t in tops {
                for n in b.decl_nodes(t, &[]) {
                    let at = n.extent.as_ref().map_or(0, |_| self.decl(DeclRef { unit, decl: t }).span.start);
                    items.push((at, n));
                }
            }
        }
        items.sort_by_key(|(at, _)| *at);
        nodes.extend(items.into_iter().map(|(_, n)| n));
        // Code `ifdef` leaves out.
        let mut off: Vec<Span> = u.inactive.iter().filter(|s| s.file == file).copied().collect();
        off.sort_by_key(|s| s.start);
        if !off.is_empty() {
            let lines = self.file_lines(file);
            let toks = self.file_tokens(file);
            let mut kids = Vec::new();
            for s in off {
                let (Some(lines), Some(r)) = (lines.as_ref(), self.to_range(s)) else { continue };
                let _ = lines;
                let name = if r.start.line == r.end.line { format!("line {}", r.start.line + 1) } else { format!("lines {}–{}", r.start.line + 1, r.end.line + 1) };
                // The directive just before it (`ifdef USE_AXI), as written.
                let detail = toks.as_ref().and_then(|t| directive_before(t, s.start));
                let mut n = b.leaf(&name, "inactive", detail, s, None);
                n.inactive = true;
                kids.push(n);
            }
            if !kids.is_empty() {
                nodes.push(group("Inactive code", "inactive", kids));
            }
        }
        if let Some(f) = filter {
            nodes = filtered(nodes, f);
        }
        assign_ids(&mut nodes, "");
        let context = (units.len() > 1 || u.kind != UnitKind::AdHoc || u.root != file).then(|| self.vfs.path(u.root).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        StructTree {
            status: if u.kind == UnitKind::AdHoc { "not-compiled" } else { "ok" },
            context: context.filter(|c| !c.is_empty()),
            others: units.len().saturating_sub(1),
            incomplete: b.incomplete,
            nodes,
        }
    }
}

impl Build<'_> {
    fn decl(&self, id: DeclId) -> &Decl {
        self.a.unit(self.unit).parsed.decl(id)
    }

    fn take(&mut self) -> bool {
        if self.left == 0 {
            self.incomplete = true;
            return false;
        }
        self.left -= 1;
        true
    }

    /// A node with no children.
    fn leaf(&mut self, name: &str, kind: &'static str, detail: Option<String>, at: Span, target: Option<Location>) -> StructNode {
        let _ = self.take();
        let loc = self.a.to_location(at);
        StructNode { name: name.to_string(), kind, detail, target: target.or_else(|| loc.clone()), extent: loc, ..Default::default() }
    }

    /// The nodes of a declaration: one node, or its contents for an anonymous container, or
    /// none for what the view leaves out. `hidden_macros`: macros whose declarations are left
    /// out besides the UVM library's.
    fn decl_nodes(&mut self, id: DeclId, hidden_macros: &[&str]) -> Vec<StructNode> {
        let d = self.decl(id).clone();
        // An out-of-block implementation is listed with its prototype.
        if d.has(dflags::OUT_OF_BLOCK) {
            return Vec::new();
        }
        if d.has(dflags::FROM_MACRO) {
            // Unnamed generate blocks are synthetic: their contents show in place.
            if d.kind == DeclKind::Generate && d.name.is_empty() {
                return d.children.iter().flat_map(|&c| self.decl_nodes(c, hidden_macros)).collect();
            }
            match self.macro_at(d.span) {
                Some(m) if m.starts_with("uvm_") || hidden_macros.contains(&m.as_str()) => return Vec::new(),
                None => return Vec::new(),
                Some(_) => {}
            }
        }
        if d.name_span.file != self.file && !matches!(d.kind, DeclKind::Block | DeclKind::Generate) {
            return Vec::new();
        }
        if d.name.is_empty() {
            return match d.kind {
                DeclKind::Block | DeclKind::Generate | DeclKind::StructType | DeclKind::EnumType => d.children.iter().flat_map(|&c| self.decl_nodes(c, hidden_macros)).collect(),
                _ => Vec::new(),
            };
        }
        if !self.take() {
            return Vec::new();
        }
        let r = DeclRef { unit: self.unit, decl: id };
        let mut n = self.member(r, None);
        n.children = match d.kind {
            DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Checker => self.element_groups(id),
            DeclKind::Generate => self.element_groups(id),
            DeclKind::Package => self.package_groups(id),
            DeclKind::Class | DeclKind::InterfaceClass => self.class_groups(id),
            DeclKind::Covergroup => d.children.iter().flat_map(|&c| self.decl_nodes(c, hidden_macros)).collect(),
            _ => Vec::new(),
        };
        // A user macro made it.
        if d.has(dflags::FROM_MACRO) {
            if let Some(m) = self.macro_at(d.span) {
                n.detail = Some(format!("from `{m}"));
            }
        }
        vec![n]
    }

    /// The node of one declaration (of any unit: inherited members), with no children.
    fn member(&self, r: DeclRef, inherited_from: Option<String>) -> StructNode {
        let a = self.a;
        let d = a.decl(r);
        let in_class = d.parent.is_some_and(|p| matches!(a.unit(r.unit).parsed.decl(p).kind, DeclKind::Class | DeclKind::InterfaceClass));
        let kind = kind_of(d, in_class);
        let sel = a.to_location(d.name_span);
        let full = if d.span.file == d.name_span.file { a.to_location(d.span) } else { None };
        // An extern method opens its implementation.
        let target = if d.has(dflags::PROTOTYPE) { a.implementation_of(r).and_then(|i| a.to_location(a.decl(i).name_span)).or(sel.clone()) } else { sel.clone() };
        StructNode {
            name: d.name.to_string(),
            kind,
            detail: detail_of(a, d, kind),
            extent: full.or(sel),
            target,
            inherited_from,
            ..Default::default()
        }
    }

    /// The groups of a module, interface, program, checker or generate block.
    fn element_groups(&mut self, id: DeclId) -> Vec<StructNode> {
        let d = self.decl(id).clone();
        let mut g = Groups::default();
        for imp in &d.imports {
            let name = format!("{}::{}", imp.pkg, imp.item.as_deref().unwrap_or("*"));
            let n = self.leaf(&name, "import", None, imp.pkg_span, None);
            g.add("Imports", n);
        }
        for &c in &d.children {
            let cd = self.decl(c).clone();
            let key = match cd.kind {
                DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam => "Parameters",
                DeclKind::Port => "Ports",
                DeclKind::Modport => "Modports",
                DeclKind::ClockingBlock => "Clocking blocks",
                DeclKind::Net | DeclKind::Variable | DeclKind::Genvar | DeclKind::Let => "Signals",
                DeclKind::Instance => "Instances",
                DeclKind::Generate if !cd.name.is_empty() => "Generate blocks",
                DeclKind::Function | DeclKind::Task => "Functions and tasks",
                DeclKind::Typedef | DeclKind::StructType | DeclKind::EnumType => "Typedefs",
                DeclKind::Property | DeclKind::Sequence => "Assertions",
                DeclKind::Block if self.is_assertion(&cd) => "Assertions",
                // Named blocks of processes.
                DeclKind::Block => continue,
                DeclKind::Covergroup => "Covergroups",
                DeclKind::Class | DeclKind::InterfaceClass => "Classes",
                _ => "Other",
            };
            for n in self.decl_nodes(c, &[]) {
                g.add(key, n);
            }
        }
        // Processes and unlabelled assertions are statements, not declarations.
        for (at, name, kind) in self.statements(&d) {
            if self.take() {
                let mut n = self.leaf_nocount(&name, kind, at);
                if kind == "assertion" {
                    n.kind = "assertion";
                    g.add("Assertions", n);
                } else {
                    g.add("Processes", n);
                }
            }
        }
        g.into_nodes(&[
            "Imports",
            "Parameters",
            "Ports",
            "Modports",
            "Clocking blocks",
            "Signals",
            "Instances",
            "Processes",
            "Generate blocks",
            "Functions and tasks",
            "Typedefs",
            "Assertions",
            "Covergroups",
            "Classes",
            "Other",
        ])
    }

    fn leaf_nocount(&self, name: &str, kind: &'static str, at: Span) -> StructNode {
        let loc = self.a.to_location(at);
        StructNode { name: name.to_string(), kind, target: loc.clone(), extent: loc, ..Default::default() }
    }

    /// The groups of a package.
    fn package_groups(&mut self, id: DeclId) -> Vec<StructNode> {
        let d = self.decl(id).clone();
        let mut g = Groups::default();
        for imp in &d.imports {
            let name = format!("{}::{}", imp.pkg, imp.item.as_deref().unwrap_or("*"));
            let n = self.leaf(&name, "import", None, imp.pkg_span, None);
            g.add("Imports", n);
        }
        for &c in &d.children {
            let key = match self.decl(c).kind {
                DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam => "Parameters",
                DeclKind::Typedef | DeclKind::StructType | DeclKind::EnumType => "Typedefs",
                DeclKind::Class | DeclKind::InterfaceClass => "Classes",
                DeclKind::Function | DeclKind::Task => "Functions and tasks",
                _ => "Other",
            };
            for n in self.decl_nodes(c, &[]) {
                g.add(key, n);
            }
        }
        g.into_nodes(&["Imports", "Parameters", "Typedefs", "Classes", "Functions and tasks", "Other"])
    }

    /// The groups of a class: its own members, then what it inherits from each base class.
    fn class_groups(&mut self, id: DeclId) -> Vec<StructNode> {
        let a = self.a;
        let d = self.decl(id).clone();
        let class = DeclRef { unit: self.unit, decl: id };
        let mut g = Groups::default();
        let mut own_methods: Vec<(String, usize)> = Vec::new();
        for &c in &d.children {
            let cd = self.decl(c).clone();
            // Out-of-block implementations are listed with their prototype.
            if cd.has(dflags::OUT_OF_BLOCK) {
                continue;
            }
            let key = match cd.kind {
                DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam => "Parameters",
                DeclKind::Typedef | DeclKind::StructType | DeclKind::EnumType => "Typedefs",
                DeclKind::Variable => "Fields",
                DeclKind::Constraint => "Constraints",
                DeclKind::Covergroup => "Covergroups",
                DeclKind::Function | DeclKind::Task => "Methods",
                DeclKind::Class | DeclKind::InterfaceClass => "Classes",
                _ => "Other",
            };
            for n in self.decl_nodes(c, &[]) {
                if key == "Methods" {
                    own_methods.push((n.name.clone(), g.len(key)));
                }
                g.add(key, n);
            }
        }
        // Each base class below the UVM library: its fields and methods, marked against the
        // classes below it.
        let mut chain = Vec::new();
        let mut cur = a.class_base(class);
        let mut library = None;
        for _ in 0..32 {
            let Some(c) = cur else { break };
            if a.in_uvm_library(c) {
                library = Some(c);
                break;
            }
            chain.push(c);
            cur = a.class_base(c);
        }
        // Methods seen so far, most derived first: name -> (class, virtual).
        let mut below: Vec<(String, String)> = own_methods.iter().map(|(n, _)| (n.clone(), d.name.to_string())).collect();
        let mut base_groups = Vec::new();
        for (k, &base) in chain.iter().enumerate() {
            let bd = a.decl(base);
            let mut kids = Vec::new();
            let mut listed = 0;
            for &m in &bd.children {
                let mr = DeclRef { unit: base.unit, decl: m };
                let md = a.decl(mr);
                let method = matches!(md.kind, DeclKind::Function | DeclKind::Task);
                let field = md.kind == DeclKind::Variable;
                if !(method || field) || md.has(dflags::FROM_MACRO) || md.has(dflags::LOCAL) || md.has(dflags::OUT_OF_BLOCK) || md.name == "new" || md.name.is_empty() {
                    continue;
                }
                if listed >= MAX_INHERITED || !self.take() {
                    self.incomplete = true;
                    break;
                }
                listed += 1;
                let mut n = self.member(mr, Some(bd.name.to_string()));
                if method {
                    // The nearest class below that defines the same name.
                    if let Some((_, by)) = below.iter().rev().find(|(name, _)| *name == md.name) {
                        let virtual_ = self.is_virtual(&chain[k..], &md.name, library);
                        n.mark = Some(if virtual_ { format!("overridden in {by}") } else { format!("hidden in {by}") });
                    }
                }
                kids.push(n);
            }
            // Mark the own methods that override or hide this base's.
            if k == 0 || below.iter().any(|(_, c)| *c == d.name) {
                for (name, at) in &own_methods {
                    if bd.children.iter().any(|&m| {
                        let md = a.decl(DeclRef { unit: base.unit, decl: m });
                        matches!(md.kind, DeclKind::Function | DeclKind::Task) && md.name == *name
                    }) {
                        if let Some(n) = g.get_mut("Methods", *at) {
                            if n.mark.is_none() {
                                let virtual_ = self.is_virtual(&chain[k..], name, library);
                                n.mark = Some(format!("{} {}::{name}", if virtual_ { "overrides" } else { "hides" }, bd.name));
                            }
                        }
                    }
                }
            }
            for &m in &bd.children {
                let md = a.decl(DeclRef { unit: base.unit, decl: m });
                if matches!(md.kind, DeclKind::Function | DeclKind::Task) {
                    below.push((md.name.to_string(), bd.name.to_string()));
                }
            }
            if !kids.is_empty() {
                let mut grp = group(&format!("Inherited from {}", bd.name), "inherited", kids);
                grp.collapsed = true;
                base_groups.push(grp);
            }
        }
        let mut out = g.into_nodes(&["Parameters", "Typedefs", "Fields", "Constraints", "Covergroups", "Methods", "Classes", "Other"]);
        out.extend(base_groups);
        if let Some(lib) = library {
            let ld = a.decl(lib);
            let loc = a.decl_location(lib);
            out.push(StructNode { name: format!("extends {}", ld.name), kind: "base", detail: Some("UVM".into()), target: loc, ..Default::default() });
        }
        out
    }

    /// Whether `name` is virtual in one of `classes` (a class and its bases) or the library
    /// class above them.
    fn is_virtual(&self, classes: &[DeclRef], name: &str, library: Option<DeclRef>) -> bool {
        let a = self.a;
        let mut all: Vec<DeclRef> = classes.to_vec();
        let mut cur = library;
        for _ in 0..32 {
            let Some(c) = cur else { break };
            all.push(c);
            cur = a.class_base(c);
        }
        all.iter().any(|c| {
            a.unit(c.unit).parsed.lookup_member(c.decl, name).iter().any(|&m| {
                let md = a.unit(c.unit).parsed.decl(m);
                matches!(md.kind, DeclKind::Function | DeclKind::Task) && md.has(dflags::VIRTUAL)
            })
        })
    }

    /// A labelled assertion (`a1: assert property (...)`), which the parser keeps as a block.
    fn is_assertion(&self, d: &Decl) -> bool {
        let Some(toks) = self.a.file_tokens(d.span.file) else { return false };
        let k = toks.partition_point(|t| t.span.start < d.name_span.end);
        toks.get(k).is_some_and(|t| t.is_punct(":")) && toks.get(k + 1).is_some_and(|t| t.is_kw("assert") || t.is_kw("assume") || t.is_kw("cover") || t.is_kw("restrict"))
    }

    /// The processes (`always*`, `initial`, `final`, `assign`) and unlabelled assertions of a
    /// design element, outside its nested declarations: (where, name, kind).
    fn statements(&self, d: &Decl) -> Vec<(Span, String, &'static str)> {
        let a = self.a;
        let Some(toks) = a.file_tokens(d.span.file) else { return Vec::new() };
        let lo = toks.partition_point(|t| t.span.start < d.span.start);
        let hi = toks.partition_point(|t| t.span.start < d.span.end);
        // Nested declarations whose contents are not this element's statements (unnamed
        // generate blocks are this element's).
        let u = a.unit(self.unit);
        let mut skip: Vec<Span> = d
            .children
            .iter()
            .map(|&c| u.parsed.decl(c))
            .filter(|c| matches!(c.kind, DeclKind::Function | DeclKind::Task | DeclKind::Class | DeclKind::Covergroup | DeclKind::Property | DeclKind::Sequence | DeclKind::Checker | DeclKind::Module | DeclKind::Interface | DeclKind::Program) || (c.kind == DeclKind::Generate && !c.name.is_empty()) || (c.kind == DeclKind::Block && !c.name.is_empty()))
            .map(|c| c.span)
            .collect();
        skip.sort_by_key(|s| s.start);
        let t = &toks[lo..hi];
        let mut out = Vec::new();
        let mut k = 1; // after the element's keyword
        let mut depth = 0i32;
        while k < t.len() {
            let x = &t[k];
            if skip.iter().any(|s| s.file == x.span.file && s.start <= x.span.start && x.span.start < s.end) {
                k += 1;
                continue;
            }
            if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
                depth += 1;
            } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
                depth -= 1;
            }
            let at_statement = depth == 0 && (k == 0 || t[k - 1].is_punct(";") || t[k - 1].is_kw("end") || t[k - 1].is_kw("begin") || t[k - 1].is_kw("generate") || t[k - 1].is_punct(")") || t[k - 1].is_punct(":") || t[k - 1].nl_before() || x.nl_before());
            if depth == 0 && at_statement && x.kind == dvi_syntax::TokKind::Keyword {
                match x.text.as_str() {
                    "always" | "always_ff" | "always_comb" | "always_latch" | "initial" | "final" => {
                        out.push((x.span, process_name(&t[k..]), "process"));
                    }
                    "assign" => out.push((x.span, assign_name(&t[k..]), "process")),
                    "assert" | "assume" | "cover" | "restrict" if !t.get(k.wrapping_sub(1)).is_some_and(|p| p.is_punct(":")) => {
                        out.push((x.span, header_text(&t[k..], 6), "assertion"));
                    }
                    _ => {}
                }
            }
            k += 1;
        }
        out
    }

    /// The macro whose call made a declaration at `at`.
    fn macro_at(&self, at: Span) -> Option<String> {
        let u = self.a.unit(self.unit);
        u.macro_uses.iter().filter(|m| m.call_span.file == at.file && m.call_span.start <= at.start && at.start <= m.call_span.end).min_by_key(|m| m.call_span.end - m.call_span.start).map(|m| m.name.to_string())
    }
}

/// A process named after its header: `always_ff @(posedge clk)`, `initial`, with ` : label`
/// when its block has one.
fn process_name(t: &[Token]) -> String {
    let mut end = 1;
    if t.get(1).is_some_and(|x| x.is_punct("@") || x.is_punct("@*")) {
        end = 2;
        if t.get(2).is_some_and(|x| x.is_punct("(")) {
            end = super::views::skip_group(t, 2);
        } else if t.get(1).is_some_and(|x| x.is_punct("@")) {
            end = 3;
        }
    }
    let mut name = super::views::tokens_text(&t[..end.min(t.len())]);
    if t.get(end).is_some_and(|x| x.is_kw("begin")) && t.get(end + 1).is_some_and(|x| x.is_punct(":")) {
        if let Some(l) = t.get(end + 2) {
            name.push_str(&format!(" : {}", l.text));
        }
    }
    name
}

/// `assign y`, `assign {a, b}`: the target of a continuous assignment.
fn assign_name(t: &[Token]) -> String {
    let eq = t.iter().position(|x| x.is_punct("=") || x.is_punct(";")).unwrap_or(t.len());
    let mut s = super::views::tokens_text(&t[..eq.min(8)]);
    if eq > 8 {
        s.push_str(" ...");
    }
    s
}

/// The first `n` tokens of a statement, as text.
fn header_text(t: &[Token], n: usize) -> String {
    let end = t.iter().position(|x| x.is_punct(";")).unwrap_or(t.len()).min(n);
    let mut s = super::views::tokens_text(&t[..end]);
    if end == n {
        s.push_str(" ...");
    }
    s
}

/// The conditional directive that opens an inactive region (`` `ifdef USE_AXI ``): its first
/// token, or the one before it.
fn directive_before(t: &[Token], at: u32) -> Option<String> {
    let k = t.partition_point(|x| x.span.start < at);
    let cond = |i: usize| -> Option<String> {
        let d = t.get(i)?;
        if d.kind != dvi_syntax::TokKind::Directive {
            return None;
        }
        match d.text.as_str() {
            "ifdef" | "ifndef" | "elsif" => Some(format!("`{} {}", d.text, t.get(i + 1)?.text)),
            "else" => Some("`else".into()),
            _ => None,
        }
    };
    cond(k).or_else(|| k.checked_sub(2).and_then(cond)).or_else(|| k.checked_sub(1).and_then(cond))
}

/// What the view calls a declaration.
fn kind_of(d: &Decl, in_class: bool) -> &'static str {
    match d.kind {
        DeclKind::Module => "module",
        DeclKind::Interface => "interface",
        DeclKind::Program => "program",
        DeclKind::Checker => "checker",
        DeclKind::Package => "package",
        DeclKind::Class => "class",
        DeclKind::InterfaceClass => "interface class",
        DeclKind::Function | DeclKind::Task if in_class && is_phase(&d.name) => "phase",
        DeclKind::Function | DeclKind::Task if in_class => "method",
        DeclKind::Function => "function",
        DeclKind::Task => "task",
        DeclKind::Variable if in_class => "field",
        DeclKind::Variable | DeclKind::Net | DeclKind::Genvar | DeclKind::Let => "signal",
        DeclKind::Port => "port",
        DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam => "parameter",
        DeclKind::Typedef | DeclKind::StructType | DeclKind::EnumType => "typedef",
        DeclKind::Constraint => "constraint",
        DeclKind::Covergroup => "covergroup",
        DeclKind::Coverpoint | DeclKind::Cross => "coverpoint",
        DeclKind::Instance => "instance",
        DeclKind::Generate => "generate",
        DeclKind::Modport => "modport",
        DeclKind::ClockingBlock => "clocking",
        DeclKind::Property | DeclKind::Sequence | DeclKind::Block => "assertion",
        _ => "other",
    }
}

/// UVM phase methods.
fn is_phase(name: &str) -> bool {
    name.ends_with("_phase") && name != "_phase"
}

/// The detail shown after a declaration's name.
fn detail_of(a: &Analysis, d: &Decl, kind: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match d.kind {
        DeclKind::Function | DeclKind::Task => {
            if kind == "phase" {
                parts.push("phase".into());
            }
            if d.has(dflags::VIRTUAL) {
                parts.push("virtual".into());
            }
            if d.has(dflags::STATIC) {
                parts.push("static".into());
            }
            if let Some(t) = d.ty.as_ref().filter(|t| !t.text.is_empty()) {
                parts.push(t.text.to_string());
            }
        }
        DeclKind::Variable | DeclKind::Net | DeclKind::Port | DeclKind::Field | DeclKind::Instance | DeclKind::Parameter | DeclKind::LocalParam => {
            if d.has(dflags::RAND) {
                parts.push("rand".into());
            }
            if d.has(dflags::STATIC) {
                parts.push("static".into());
            }
            if d.has(dflags::LOCAL) {
                parts.push("local".into());
            } else if d.has(dflags::PROTECTED) {
                parts.push("protected".into());
            }
            if let Some(dir) = d.dir {
                parts.push(dir.as_str().to_string());
            }
            if let Some(t) = &d.ty {
                parts.push(t.text.to_string());
            }
        }
        DeclKind::Class => {
            if let Some(e) = &d.extends {
                parts.push(format!("extends {}", e.text));
            }
        }
        DeclKind::Typedef => {
            if !d.detail.is_empty() {
                parts.push(d.detail.to_string());
            }
        }
        _ => {}
    }
    let _ = a;
    let s = parts.join(" ");
    let s = s.trim();
    (!s.is_empty()).then(|| if s.chars().count() > 80 { format!("{}...", s.chars().take(77).collect::<String>()) } else { s.to_string() })
}

/// A group of nodes.
fn group(name: &str, kind: &'static str, children: Vec<StructNode>) -> StructNode {
    let _ = kind;
    StructNode { name: name.to_string(), kind: "group", children, ..Default::default() }
}

/// Groups being filled, by name.
#[derive(Default)]
struct Groups {
    by: FxHashMap<&'static str, Vec<StructNode>>,
}

impl Groups {
    fn add(&mut self, key: &'static str, n: StructNode) {
        self.by.entry(key).or_default().push(n);
    }

    fn len(&self, key: &'static str) -> usize {
        self.by.get(key).map_or(0, |v| v.len())
    }

    fn get_mut(&mut self, key: &'static str, at: usize) -> Option<&mut StructNode> {
        self.by.get_mut(key).and_then(|v| v.get_mut(at))
    }

    /// The groups that are not empty, in `order`.
    fn into_nodes(mut self, order: &[&'static str]) -> Vec<StructNode> {
        order.iter().filter_map(|k| self.by.remove(k).filter(|v| !v.is_empty()).map(|v| group(k, "group", v))).collect()
    }
}

/// The nodes that match the filter, with their ancestors and their contents.
fn filtered(nodes: Vec<StructNode>, f: &Filter) -> Vec<StructNode> {
    nodes
        .into_iter()
        .filter_map(|mut n| {
            if n.kind != "group" && f.matches(&n.name, '.') {
                return Some(n);
            }
            n.children = filtered(std::mem::take(&mut n.children), f);
            (!n.children.is_empty()).then_some(n)
        })
        .collect()
}

/// Ids from the names: `parent/kind:name`, with `#2`... for repeats among siblings.
fn assign_ids(nodes: &mut [StructNode], parent: &str) {
    let mut seen: FxHashMap<String, u32> = FxHashMap::default();
    let mut used: FxHashSet<String> = FxHashSet::default();
    for n in nodes.iter_mut() {
        let base = format!("{parent}/{}:{}", n.kind, n.name);
        let k = seen.entry(base.clone()).or_insert(0);
        *k += 1;
        let id = if *k > 1 { format!("{base}#{k}") } else { base };
        used.insert(id.clone());
        n.id = id;
        let id = n.id.clone();
        assign_ids(&mut n.children, &id);
    }
}
