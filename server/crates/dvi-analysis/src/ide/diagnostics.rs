//! Problems for a file: preprocessing, syntax and semantic checks.

use dvi_syntax::SourceProvider;
use crate::resolve::{CLASS_BUILTINS, COVERGROUP_BUILTINS};
use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, ParsedUnit, RefKind, Sep};
use dvi_syntax::{Diag, FileId, Span};
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

/// Names that are never reported as undeclared.
const BUILTIN_NAMES: &[&str] = &[
    "this", "super", "null", "local", "item", "index", "option", "type_option", "std", "randomize",
    "sample", "new", "srandom", "get_randstate", "set_randstate", "rand_mode", "constraint_mode",
    "pre_randomize", "post_randomize", "triggered", "matched",
];

/// The conditional directive that starts a region of removed code at byte `start` (just after
/// its keyword): the byte range of the macro it tests (of `` `else `` when there is none), the
/// macro name, and whether the code is removed because the macro is defined (`` `ifndef ``, or
/// the `` `else `` of an `` `ifdef ``). `None` for anything else.
fn guarding_macro(text: &str, start: u32) -> Option<((u32, u32), String, bool)> {
    let start = start as usize;
    if start > text.len() || !text.is_char_boundary(start) {
        return None;
    }
    // The region starts right after the directive's keyword.
    let kw_start = text[..start].rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_').map_or(0, |i| i + 1);
    if !text[..kw_start].ends_with('`') {
        return None;
    }
    let kw = &text[kw_start..start];
    let tick = kw_start - 1;
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let name_of = |s: &str, at: usize| -> Option<((u32, u32), String)> {
        let lead = s.len() - s.trim_start().len();
        let s = s.trim_start();
        let len = s.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(s.len());
        (len > 0).then(|| (((at + lead) as u32, (at + lead + len) as u32), s[..len].to_string()))
    };
    match kw {
        "ifdef" | "elsif" | "ifndef" => {
            let (span, name) = name_of(&text[start..line_end], start)?;
            Some((span, name, kw == "ifndef"))
        }
        "else" => {
            // The `ifdef or `ifndef it belongs to.
            let mut depth = 0usize;
            let before = &text[..tick];
            let mut rest = before;
            while let Some(i) = rest.rfind('`') {
                let word: String = rest[i + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                match word.as_str() {
                    "endif" => depth += 1,
                    "ifdef" | "ifndef" if depth > 0 => depth -= 1,
                    "ifdef" | "ifndef" => {
                        let at = i + 1 + word.len();
                        let (_, name) = name_of(&before[at..], at)?;
                        return Some(((tick as u32, start as u32), name, word == "ifdef"));
                    }
                    _ => {}
                }
                rest = &rest[..i];
            }
            None
        }
        _ => None,
    }
}

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
        // Instance connections: duplicates, mixed styles, too many ordered items.
        for d in self.instance_problems(file) {
            push(d, &mut out);
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
        // Code removed by `ifdef (no build file) that hid names used elsewhere.
        let mut notices = Vec::new();
        self.hidden_code_notices(file, &mut notices);
        self.missing_macro_notices(&mut out, &mut notices);
        for d in notices {
            push(d, &mut out);
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
        self.semantic_check(uid, file, &mut Vec::new())
    }

    /// The semantic problems of `file` in unit `uid`; `hidden` gets, for each name not checked
    /// because code that may declare it is removed by `` `ifdef `` (no build file), that code.
    fn semantic_check(&self, uid: UnitId, file: FileId, hidden: &mut Vec<Span>) -> Vec<Diag> {
        self.with_lookup_memo(|| self.semantic_check_inner(uid, file, hidden))
    }

    fn semantic_check_inner(&self, uid: UnitId, file: FileId, hidden: &mut Vec<Span>) -> Vec<Diag> {
        let u = self.unit(uid);
        let Some(pos) = u.positions.get(&file) else { return Vec::new() };
        let mut out = Vec::new();
        let mut implicit: Option<FxHashMap<SmolStr, Vec<DeclId>>> = None;
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
                RefKind::Loose | RefKind::Foreign | RefKind::With { .. } => continue,
                _ => {}
            }
            if chain.segs[0].name == "local" {
                continue;
            }
            // Every segment, resolved once (`a.b.c`: `a` once, not three times).
            let segs = if chain.segs.len() > 1 { self.resolve_segments(uid, *rid) } else { vec![self.resolve_ref(uid, *rid, 0)] };
            let Some(mut cur) = segs[0] else {
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
                            // Ports and parameters of a module with encrypted parts may be in the
                            // ciphertext.
                            if md.kind.is_definition() && !md.has(dflags::ENCRYPTED) {
                                let what = if matches!(chain.kind, RefKind::Port { .. }) { "Port" } else { "Parameter" };
                                // A member of that name that a connection cannot reach: say what it is.
                                let msg = match self.member_of(m, name).map(|x| self.decl(x).kind) {
                                    Some(DeclKind::LocalParam) => format!("NON_EXISTING_PORT: '{name}' is a localparam of {} '{}', which cannot be overridden", md.kind.label(), md.name),
                                    Some(k) => format!("NON_EXISTING_PORT: {what} '{name}' is not declared in {} '{}' ('{name}' is a {} inside it)", md.kind.label(), md.name, k.label()),
                                    None => format!("NON_EXISTING_PORT: {what} '{name}' is not declared in {} '{}'", md.kind.label(), md.name),
                                };
                                out.push(Diag::error(*span, "NON_EXISTING_PORT", msg));
                            }
                        }
                    }
                    RefKind::Type | RefKind::Base => match self.scope_gap(uid, chain.scope) {
                        None if chain.segs.len() == 1 => out.push(Diag::error(*span, "NON_EXISTING_TYPE", format!("NON_EXISTING_TYPE: Type '{name}' is not defined"))),
                        None => out.push(Diag::error(*span, "NON_EXISTING_TYPE", format!("NON_EXISTING_TYPE: '{name}' is not defined"))),
                        Some(Some(region)) => hidden.push(region),
                        Some(None) => {}
                    },
                    RefKind::Expr | RefKind::ImportItem => {
                        match self.scope_gap(uid, chain.scope) {
                            Some(Some(region)) => {
                                hidden.push(region);
                                continue;
                            }
                            Some(None) => continue,
                            None => {}
                        }
                        if let Some(design) = self.design_scope(uid, chain.scope) {
                            // Hierarchical references and implicit nets are legal in designs.
                            if chain.segs.len() > 1 {
                                continue;
                            }
                            // Declared in this scope or one around it, up to the design element.
                            let owners = implicit.get_or_insert_with(|| self.implicit_net_names(uid));
                            if let Some(scopes) = owners.get(name) {
                                let u = self.unit(uid);
                                let mut cur = Some(chain.scope);
                                let mut found = false;
                                while let Some(s) = cur {
                                    if scopes.contains(&s) {
                                        found = true;
                                        break;
                                    }
                                    if s == design {
                                        break;
                                    }
                                    cur = u.parsed.decl(s).parent;
                                }
                                if found {
                                    continue;
                                }
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
                match segs[i] {
                    Some(r) => cur = r,
                    None => {
                        if seg.from_macro || seg.called && CLASS_BUILTINS.contains(&seg.name.as_str()) {
                            break;
                        }
                        if seg.sep == Sep::Dot && (chain.segs[i - 1].selects as u32) < self.array_dims(cur) {
                            break;
                        }
                        match self.missing_member_message(cur, &seg.name, seg.sep) {
                            Ok(Some(msg)) => out.push(Diag::error(seg.span, "NOT_A_MEMBER", msg)),
                            Ok(None) => {}
                            Err(region) => hidden.push(region),
                        }
                        break;
                    }
                }
            }
        }
        out
    }

    /// The problem for `name`, not found after `prev`: `Ok(None)` when it is not reported,
    /// `Err(region)` when code that may declare it is removed by `` `ifdef `` in `region`.
    fn missing_member_message(&self, prev: DeclRef, name: &str, sep: Sep) -> Result<Option<String>, Span> {
        let pd = self.decl(prev);
        let container = match (sep, pd.kind) {
            (Sep::Scope, DeclKind::Package | DeclKind::Class | DeclKind::InterfaceClass) => Some(prev),
            (Sep::Scope, DeclKind::Typedef) => self.follow_typedef(prev),
            (Sep::Scope, _) => None,
            _ => self.type_of(prev),
        };
        let Some(container) = container else { return Ok(None) };
        let cd = self.decl(container);
        if cd.has(dflags::ENCRYPTED) {
            return Ok(None);
        }
        if let Some(region) = self.hiding_region(container) {
            return Err(region);
        }
        Ok(match cd.kind {
            DeclKind::Class | DeclKind::InterfaceClass => {
                if CLASS_BUILTINS.contains(&name) || !self.class_chain_complete(container) {
                    return Ok(None);
                }
                // A type parameter in the chain makes the class generic: be lenient.
                Some(format!("NOT_A_MEMBER: '{name}' is not a member of class '{}'", cd.name))
            }
            DeclKind::Package => Some(format!("NOT_A_MEMBER: '{name}' is not declared in package '{}'", cd.name)),
            DeclKind::StructType => Some(format!("NOT_A_MEMBER: '{name}' is not a member of the struct")),
            DeclKind::Covergroup if !COVERGROUP_BUILTINS.contains(&name) => Some(format!("NOT_A_MEMBER: '{name}' is not a member of covergroup '{}'", cd.name)),
            DeclKind::Interface | DeclKind::Module | DeclKind::Program => Some(format!("NOT_A_MEMBER: '{name}' is not declared in {} '{}'", cd.kind.label(), cd.name)),
            _ => None,
        })
    }

    /// The scope may see names we cannot know: a class whose base does not resolve, or an
    /// import of a missing package.
    pub fn scope_is_open(&self, uid: UnitId, scope: DeclId) -> bool {
        self.scope_gap(uid, scope).is_some()
    }

    /// Why the scope may see names we cannot know: `Some(Some(region))` when an imported
    /// package has code removed by `` `ifdef `` in `region`, `Some(None)` for other reasons.
    fn scope_gap(&self, uid: UnitId, scope: DeclId) -> Option<Option<Span>> {
        let u = self.unit(uid);
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            if d.has(dflags::ENCRYPTED) {
                return Some(None);
            }
            if matches!(d.kind, DeclKind::Class | DeclKind::InterfaceClass) && !self.class_chain_complete(DeclRef { unit: uid, decl: s }) {
                return Some(None);
            }
            if d.has(dflags::OUT_OF_BLOCK) {
                match self.owner_class(DeclRef { unit: uid, decl: s }) {
                    Some(c) if self.class_chain_complete(c) => {}
                    _ => return Some(None),
                }
            }
            for imp in &d.imports {
                if let Some(gap) = self.import_gap(uid, imp) {
                    return Some(gap);
                }
            }
            cur = d.parent;
        }
        let root = u.parsed.decl(ParsedUnit::ROOT);
        root.imports.iter().find_map(|i| self.import_gap(uid, i))
    }

    /// `None` when the import brings in only names we know: its package exists and, for
    /// `pkg::*`, has no members we cannot see; `pkg::item` is complete when the item is found.
    /// Otherwise the region of the package removed by `` `ifdef `` (`None`: no such package).
    fn import_gap(&self, uid: UnitId, imp: &dvi_syntax::ast::Import) -> Option<Option<Span>> {
        let found = match &imp.item {
            Some(item) => self.package_with(uid, &imp.pkg, item),
            None => self.package(uid, &imp.pkg),
        };
        let Some(p) = found else { return Some(None) };
        if imp.item.as_ref().is_some_and(|item| self.member_of(p, item).is_some()) {
            return None;
        }
        self.hiding_region(p).map(Some)
    }

    /// Without a build file the defines are unknown: a declaration with code removed by
    /// `` `ifdef `` may have members under the defines of the real build (a package whose body
    /// is inside `` `ifdef UVM ``, defined on the simulator command line).
    pub fn may_hide_members(&self, d: DeclRef) -> bool {
        self.hiding_region(d).is_some()
    }

    /// The first region of [`Self::may_hide_members`]: code removed by `` `ifdef `` directly in `d`.
    fn hiding_region(&self, d: DeclRef) -> Option<Span> {
        if self.setup.config.build_file.is_some() {
            return None;
        }
        let decl = self.decl(d);
        let span = decl.span;
        let u = self.unit(d.unit);
        let within = |outer: Span, s: &Span| s.file == outer.file && outer.start <= s.start && s.end <= outer.end;
        // Only code removed directly in its scope: in a method or a struct it would declare
        // locals or fields, not members. Enum items are members of the enclosing scope, so
        // code removed in an enum type (or its typedef) counts.
        let direct = |s: &Span| {
            let mut cur = decl;
            loop {
                match cur.children.iter().map(|c| u.parsed.decl(*c)).find(|c| within(c.span, s)) {
                    None => return true,
                    Some(c) if matches!(c.kind, DeclKind::EnumType | DeclKind::Typedef) => cur = c,
                    Some(_) => return false,
                }
            }
        };
        u.inactive.iter().find(|s| within(span, s) && direct(s)).copied()
    }

    /// For each region of code removed by `` `ifdef `` that may declare names used elsewhere
    /// (no build file), how many uses in the project were not checked because of it. Counted
    /// over every project file once per snapshot, but for the files whose part was taken over
    /// from the snapshot before (`Analysis::reuse_hidden_uses`).
    pub fn hidden_uses(&self) -> &FxHashMap<(FileId, u32), usize> {
        &self.hidden_uses.get_or_init(|| self.count_hidden_uses()).counts
    }

    fn count_hidden_uses(&self) -> crate::world::HiddenUses {
        let mut out = crate::world::HiddenUses::default();
        if self.setup.config.build_file.is_some() {
            return out;
        }
        use rayon::prelude::*;
        let seed = self.hidden_seed.as_deref();
        out.per_file = self
            .project_files()
            .par_iter()
            .map(|&file| {
                if let Some(v) = seed.and_then(|s| s.get(&file)) {
                    return (file, v.clone());
                }
                let mut hidden = Vec::new();
                for uid in self.units_for(file) {
                    let _ = self.semantic_check(uid, file, &mut hidden);
                    // A header is analysed once, in its first unit (as for its problems).
                    if self.unit(uid).root != file {
                        break;
                    }
                }
                (file, std::sync::Arc::from(hidden))
            })
            .collect();
        for v in out.per_file.values() {
            for s in v.iter() {
                *out.counts.entry((s.file, s.start)).or_default() += 1;
            }
        }
        out
    }

    /// Files that may show a SKIPPED_CODE notice (no build file): those with code removed by
    /// `` `ifdef `` directly in a package, class or other scope that names are looked up in. Their
    /// notices count uses in every file, so an edit anywhere may change them. Cheap: no counting.
    pub fn files_with_skipped_code(&self) -> Vec<FileId> {
        if self.setup.config.build_file.is_some() {
            return Vec::new();
        }
        let mut out: FxHashSet<FileId> = FxHashSet::default();
        for u in self.units.iter().flatten() {
            if u.inactive.is_empty() {
                continue;
            }
            for (i, d) in u.parsed.decls.iter().enumerate() {
                let container = matches!(
                    d.kind,
                    DeclKind::Package | DeclKind::Class | DeclKind::InterfaceClass | DeclKind::StructType | DeclKind::Covergroup | DeclKind::Interface | DeclKind::Module | DeclKind::Program
                );
                if container && !out.contains(&d.span.file) && self.hiding_region(DeclRef { unit: u.id, decl: i as DeclId }).is_some() {
                    out.insert(d.span.file);
                }
            }
        }
        out.into_iter().collect()
    }

    /// The notice on the `` `ifdef NAME `` line of a removed region that hid names used elsewhere.
    fn hidden_code_notices(&self, file: FileId, out: &mut Vec<Diag>) {
        if self.setup.config.build_file.is_some() {
            return;
        }
        // Counting needs every file: only for a file with removed code.
        if !self.units_for(file).iter().any(|u| self.unit(*u).inactive.iter().any(|s| s.file == file)) {
            return;
        }
        let Some(text) = self.file_text(file) else { return };
        let mut regions: Vec<(u32, usize)> = self.hidden_uses().iter().filter(|((f, _), _)| *f == file).map(|((_, s), n)| (*s, *n)).collect();
        regions.sort_unstable();
        for (start, uses) in regions {
            let Some((span, name, defined)) = guarding_macro(&text, start) else { continue };
            let span = Span::new(file, span.0, span.1);
            let uses = if uses == 1 { "1 use".to_string() } else { format!("{uses} uses") };
            let msg = if defined {
                format!("SKIPPED_CODE: This code is skipped because `{name}` is defined. {uses} in the project may need names declared here and are not checked. Without a build file DVICode does not know your simulator's defines.")
            } else {
                format!("SKIPPED_CODE: `{name}` is not defined, so this code is skipped. {uses} in the project may need names declared here and are not checked. Without a build file DVICode does not know your simulator's defines: add +define+{name} to a build file (docs/build-files.md).")
            };
            out.push(Diag::info(span, "SKIPPED_CODE", msg));
        }
    }

    /// Without a build file, a macro that no file in the folder defines usually comes from the
    /// simulator's command line (`+define+WIDTH=4`) or from a library outside the folder, which
    /// DVICode cannot know. Instead of an error on each use, one notice in each file, on the
    /// first use of the macro there, says how many uses are not expanded.
    fn missing_macro_notices(&self, out: &mut Vec<Diag>, notices: &mut Vec<Diag>) {
        if self.setup.config.build_file.is_some() {
            return;
        }
        let mut uses: Vec<(SmolStr, Span)> = Vec::new();
        out.retain(|d| {
            if d.code != "UNDEFINED_MACRO" {
                return true;
            }
            let Some(name) = d.message.strip_prefix("Macro `").and_then(|m| m.strip_suffix(" is not defined")) else { return true };
            if self.global.macros.contains_key(name) {
                return true;
            }
            uses.push((SmolStr::new(name), d.span));
            false
        });
        let mut first: Vec<(Span, SmolStr, usize)> = Vec::new();
        let mut index: FxHashMap<SmolStr, usize> = FxHashMap::default();
        for (name, span) in uses {
            match index.get(&name) {
                Some(&i) => {
                    let f = &mut first[i];
                    f.2 += 1;
                    if span.start < f.0.start {
                        f.0 = span;
                    }
                }
                None => {
                    index.insert(name.clone(), first.len());
                    first.push((span, name, 1));
                }
            }
        }
        first.sort_by_key(|(s, _, _)| s.start);
        for (span, name, n) in first {
            let uses = if n == 1 { "Its use in this file is".to_string() } else { format!("Its {n} uses in this file are") };
            notices.push(Diag::info(
                span,
                "UNDEFINED_MACRO",
                format!("Macro `{name} is not defined in any file of the folder. {uses} not expanded. Without a build file DVICode does not know your simulator's defines or libraries outside the folder: add +define+{name}=... or the files that define it to a build file (docs/build-files.md)."),
            ));
        }
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

    /// Names used in port connections and assignments, which may be implicitly declared nets,
    /// and the scopes that declare them: the generate block they are in (IEEE 1800 6.10), else
    /// the design element.
    fn implicit_net_names(&self, uid: UnitId) -> FxHashMap<SmolStr, Vec<DeclId>> {
        let u = self.unit(uid);
        let mut map: FxHashMap<SmolStr, Vec<DeclId>> = FxHashMap::default();
        for r in &u.parsed.refs {
            if r.kind != RefKind::Loose {
                continue;
            }
            let Some(design) = self.design_scope(uid, r.scope) else { continue };
            let mut owner = r.scope;
            while owner != design && u.parsed.decl(owner).kind != DeclKind::Generate {
                match u.parsed.decl(owner).parent {
                    Some(p) => owner = p,
                    None => break,
                }
            }
            let v = map.entry(r.segs[0].name.clone()).or_default();
            if !v.contains(&owner) {
                v.push(owner);
            }
        }
        map
    }
}
