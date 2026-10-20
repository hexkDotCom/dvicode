//! Problems for a file: preprocessing, syntax and semantic checks.

use dvi_syntax::SourceProvider;
use crate::resolve::{CLASS_BUILTINS, COVERGROUP_BUILTINS};
use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, ParsedUnit, RefKind, Sep};
use dvi_syntax::{Diag, FileId};
use rustc_hash::FxHashSet;
use smol_str::SmolStr;

/// Names that are never reported as undeclared.
const BUILTIN_NAMES: &[&str] = &[
    "this", "super", "null", "local", "item", "index", "option", "type_option", "std", "randomize",
    "sample", "new", "srandom", "get_randstate", "set_randstate", "rand_mode", "constraint_mode",
    "pre_randomize", "post_randomize", "triggered", "matched",
];

pub const MAX_DIAGS_PER_FILE: usize = 500;
/// Syntax errors cascade; more than this per file is noise.
pub const MAX_SYNTAX_PER_FILE: usize = 50;

impl Analysis {
    pub fn file_diagnostics(&self, file: FileId) -> Vec<Diag> {
        let mut out: Vec<Diag> = Vec::new();
        let mut seen: FxHashSet<(u32, u32, String)> = FxHashSet::default();
        let mut push = |d: Diag, out: &mut Vec<Diag>| {
            if seen.insert((d.span.start, d.span.end, d.message.clone())) {
                out.push(d);
            }
        };
        let units = self.units_for(file);
        for &uid in &units {
            let u = self.unit(uid);
            for d in u.pp_diags.iter().chain(u.parsed.diags.iter()) {
                if d.span.file == file {
                    push(d.clone(), &mut out);
                }
            }
            for d in self.semantic_diags(uid, file) {
                push(d, &mut out);
            }
            // Analyse a header once, in its first unit.
            if u.root != file {
                break;
            }
        }
        // Build file problems.
        let path = self.vfs.path(file);
        for p in &self.setup.config.problems {
            if p.file == path {
                if let (Some(text), Some(li)) = (self.file_text(file), self.file_lines(file)) {
                    let (start, end) = match p.range {
                        Some((s, e)) if (e as usize) <= text.len() => (s, e),
                        _ => (li.offset(&text, p.line, 0), li.offset(&text, p.line, u32::MAX)),
                    };
                    push(Diag::warning(dvi_syntax::Span::new(file, start, end), "BUILD_CONFIG", p.message.clone()), &mut out);
                }
            }
        }
        let mut syntax = 0usize;
        out.retain(|d| {
            if d.code == "SYNTAX_ERROR" {
                syntax += 1;
                syntax <= MAX_SYNTAX_PER_FILE
            } else {
                true
            }
        });
        out.truncate(MAX_DIAGS_PER_FILE);
        out
    }

    fn semantic_diags(&self, uid: UnitId, file: FileId) -> Vec<Diag> {
        let u = self.unit(uid);
        let Some(pos) = u.positions.get(&file) else { return Vec::new() };
        let mut out = Vec::new();
        let mut implicit: Option<FxHashSet<(DeclId, SmolStr)>> = None;
        for (span, rid, si) in &pos.refs {
            if *si != 0 {
                continue;
            }
            let chain = u.parsed.reference(*rid);
            let seg0 = &chain.segs[0];
            if seg0.from_macro || seg0.name.starts_with('$') {
                continue;
            }
            match chain.kind {
                RefKind::Loose | RefKind::With { .. } => continue,
                _ => {}
            }
            if chain.segs[0].name == "local" {
                continue;
            }
            let first = self.resolve_ref(uid, *rid, 0);
            let Some(mut cur) = first else {
                if BUILTIN_NAMES.contains(&seg0.name.as_str()) {
                    continue;
                }
                let name = &seg0.name;
                match chain.kind {
                    RefKind::Package => out.push(Diag::error(*span, "NON_EXISTING_PACKAGE", format!("NON_EXISTING_PACKAGE: Package '{name}' is not defined"))),
                    RefKind::Definition => out.push(Diag::warning(*span, "NON_EXISTING_DESIGN_ELEMENT", format!("NON_EXISTING_DESIGN_ELEMENT: Design element '{name}' is not defined"))),
                    RefKind::Port { inst } | RefKind::Param { inst } => {
                        let inst_ref = DeclRef { unit: uid, decl: inst };
                        if let Some(m) = self.type_of(inst_ref) {
                            let md = self.decl(m);
                            if md.kind.is_definition() {
                                let what = if matches!(chain.kind, RefKind::Port { .. }) { "Port" } else { "Parameter" };
                                out.push(Diag::error(*span, "NON_EXISTING_PORT", format!("NON_EXISTING_PORT: {what} '{name}' is not declared in {} '{}'", md.kind.label(), md.name)));
                            }
                        }
                    }
                    RefKind::Type | RefKind::Base => {
                        if !self.scope_is_open(uid, chain.scope) && chain.segs.len() == 1 {
                            out.push(Diag::error(*span, "NON_EXISTING_TYPE", format!("NON_EXISTING_TYPE: Type '{name}' is not defined")));
                        } else if chain.segs.len() > 1 && !self.scope_is_open(uid, chain.scope) {
                            out.push(Diag::error(*span, "NON_EXISTING_TYPE", format!("NON_EXISTING_TYPE: '{name}' is not defined")));
                        }
                    }
                    RefKind::Expr | RefKind::ImportItem => {
                        if self.scope_is_open(uid, chain.scope) {
                            continue;
                        }
                        if let Some(design) = self.design_scope(uid, chain.scope) {
                            // Hierarchical references and implicit nets are legal in designs.
                            if chain.segs.len() > 1 {
                                continue;
                            }
                            let set = implicit.get_or_insert_with(|| self.implicit_net_names(uid));
                            if set.contains(&(design, name.clone())) {
                                continue;
                            }
                        }
                        out.push(Diag::error(*span, "UNDECLARED_IDENTIFIER", format!("UNDECLARED_IDENTIFIER: '{name}' is not declared")));
                    }
                    _ => {}
                }
                continue;
            };
            // Member checks along the chain.
            for i in 1..chain.segs.len() {
                let seg = &chain.segs[i];
                match self.resolve_ref(uid, *rid, i) {
                    Some(r) => cur = r,
                    None => {
                        if seg.from_macro || seg.called && CLASS_BUILTINS.contains(&seg.name.as_str()) {
                            break;
                        }
                        if seg.sep == Sep::Dot && (chain.segs[i - 1].selects as u32) < self.array_dims(cur) {
                            break;
                        }
                        if let Some(msg) = self.missing_member_message(cur, &seg.name, seg.sep) {
                            out.push(Diag::error(seg.span, "NOT_A_MEMBER", msg));
                        }
                        break;
                    }
                }
            }
        }
        out
    }

    fn missing_member_message(&self, prev: DeclRef, name: &str, sep: Sep) -> Option<String> {
        let pd = self.decl(prev);
        let container = match (sep, pd.kind) {
            (Sep::Scope, DeclKind::Package | DeclKind::Class | DeclKind::InterfaceClass) => Some(prev),
            (Sep::Scope, DeclKind::Typedef) => self.follow_typedef(prev),
            (Sep::Scope, _) => None,
            _ => self.type_of(prev),
        }?;
        let cd = self.decl(container);
        if cd.has(dflags::ENCRYPTED) || self.may_hide_members(container) {
            return None;
        }
        match cd.kind {
            DeclKind::Class | DeclKind::InterfaceClass => {
                if CLASS_BUILTINS.contains(&name) || !self.class_chain_complete(container) {
                    return None;
                }
                // A type parameter in the chain makes the class generic: be lenient.
                Some(format!("NOT_A_MEMBER: '{name}' is not a member of class '{}'", cd.name))
            }
            DeclKind::Package => Some(format!("NOT_A_MEMBER: '{name}' is not declared in package '{}'", cd.name)),
            DeclKind::StructType => Some(format!("NOT_A_MEMBER: '{name}' is not a member of the struct")),
            DeclKind::Covergroup if !COVERGROUP_BUILTINS.contains(&name) => Some(format!("NOT_A_MEMBER: '{name}' is not a member of covergroup '{}'", cd.name)),
            DeclKind::Interface | DeclKind::Module | DeclKind::Program => Some(format!("NOT_A_MEMBER: '{name}' is not declared in {} '{}'", cd.kind.label(), cd.name)),
            _ => None,
        }
    }

    /// The scope may see names we cannot know: a class whose base does not resolve, or an
    /// import of a missing package.
    pub fn scope_is_open(&self, uid: UnitId, scope: DeclId) -> bool {
        let u = self.unit(uid);
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            if d.has(dflags::ENCRYPTED) {
                return true;
            }
            if matches!(d.kind, DeclKind::Class | DeclKind::InterfaceClass) && !self.class_chain_complete(DeclRef { unit: uid, decl: s }) {
                return true;
            }
            if d.has(dflags::OUT_OF_BLOCK) {
                match self.owner_class(DeclRef { unit: uid, decl: s }) {
                    Some(c) if self.class_chain_complete(c) => {}
                    _ => return true,
                }
            }
            for imp in &d.imports {
                if !self.import_is_complete(&imp.pkg) {
                    return true;
                }
            }
            cur = d.parent;
        }
        let root = u.parsed.decl(ParsedUnit::ROOT);
        root.imports.iter().any(|i| !self.import_is_complete(&i.pkg))
    }

    /// The package exists and all its members are known.
    fn import_is_complete(&self, pkg: &str) -> bool {
        match self.global.packages.get(pkg).and_then(|v| v.first()) {
            Some(p) => !self.may_hide_members(*p),
            None => false,
        }
    }

    /// Without a build file the defines are unknown: a declaration with code removed by
    /// `` `ifdef `` may have members under the defines of the real build (a package whose body
    /// is inside `` `ifdef UVM ``, defined on the simulator command line).
    pub fn may_hide_members(&self, d: DeclRef) -> bool {
        if self.setup.config.build_file.is_some() {
            return false;
        }
        let span = self.decl(d).span;
        self.unit(d.unit).inactive.iter().any(|s| s.file == span.file && span.start <= s.start && s.end <= span.end)
    }

    /// Enclosing module/interface/program/checker.
    pub fn design_scope(&self, uid: UnitId, scope: DeclId) -> Option<DeclId> {
        let u = self.unit(uid);
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            if matches!(d.kind, DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Checker) {
                return Some(s);
            }
            if matches!(d.kind, DeclKind::Class | DeclKind::Package) {
                return None;
            }
            cur = d.parent;
        }
        None
    }

    /// Names used in port connections and assignments of each design element, which may be
    /// implicitly declared nets.
    fn implicit_net_names(&self, uid: UnitId) -> FxHashSet<(DeclId, SmolStr)> {
        let u = self.unit(uid);
        let mut set = FxHashSet::default();
        for r in &u.parsed.refs {
            if r.kind == RefKind::Loose {
                if let Some(d) = self.design_scope(uid, r.scope) {
                    set.insert((d, r.segs[0].name.clone()));
                }
            }
        }
        set
    }
}
