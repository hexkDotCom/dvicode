//! The compile-context inspector: how a file is compiled. The units that compile it (with the
//! role it has in each and the exact include chain the preprocessor recorded), and for the
//! first of them — the one the editor features use — the invocation, the macros the file
//! refers to as seen there, where each of its includes was found, how much of it `` `ifdef ``
//! removes, its design elements and packages that are defined more than once, and its
//! problems.
//!
//! Everything comes from the snapshot's records and says no more than they prove: includes are
//! the executed ones, macro states the definitions seen at the recorded places. The only
//! present-time look is at include candidates that also exist (shadowed copies), labelled so.

use crate::ide::url_of;
use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, ParsedUnit};
use dvi_syntax::preproc::{include_candidates, IncludeStep, MacroDef, COMMAND_LINE_FILE};
use dvi_syntax::{FileId, Span, TokKind};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;

/// Contexts listed at most (the total is given).
pub const MAX_CONTEXTS: usize = 100;
const MAX_CHAIN: usize = 64;
const MAX_MACROS: usize = 500;
/// Distinct states listed per macro.
const MAX_STATES: usize = 20;
const MAX_INCLUDES: usize = 500;
const MAX_LISTED: usize = 200;
/// File system looks for shadowed include copies, per report.
const MAX_PROBES: usize = 2_000;
const MAX_DUP_NAMES: usize = 200;
const MAX_DUP_PLACES: usize = 20;
const MAX_PROBLEMS: usize = 50;
const VALUE_CHARS: usize = 120;

/// A place in a file: its uri and zero-based line.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Place {
    pub uri: String,
    pub line: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct CompileContext {
    pub uri: String,
    /// The engine knows the file (it was read or opened).
    pub known: bool,
    /// The project has no build file: auto-configuration.
    pub auto: bool,
    pub build_file: Option<String>,
    /// The units compiling the file, in the default order (the first is the default context).
    pub contexts: Vec<Context>,
    pub contexts_total: usize,
    /// `default` (no context chosen), `chosen`, or `unavailable` (the context chosen is not
    /// compiled in this build: the default is used).
    pub choice: &'static str,
    /// The chosen context when it is not among those listed.
    pub chosen_context: Option<Context>,
    /// The unavailable choice: its root (uri), invocation fingerprint and nth.
    pub unavailable: Option<(String, String, u32)>,
    /// `+dvicode_skip_compile+` patterns the file matches: (invocation, pattern). They remove
    /// top-file entries only; the file can still be included.
    pub skip_matches: Vec<(usize, String)>,
    /// About the context the editor features use (the chosen one, else the first).
    pub detail: Option<Detail>,
}

#[derive(Debug, Serialize)]
pub struct Context {
    /// Zero-based invocation index.
    pub invocation: usize,
    pub mode: String,
    /// The root's role: `top` (a top file of the invocation), `library` (a `-v`/`-y` file),
    /// `adhoc` (open in the editor, not part of the build), `builtin`.
    pub root_role: &'static str,
    /// The root file.
    pub root: String,
    /// The root's place in the invocation's top files (1-based) and their number.
    pub order: Option<(usize, usize)>,
    /// The file is the root itself (no chain).
    pub is_root: bool,
    /// Its 1-based position in the default order.
    pub ordinal: usize,
    /// The editor features use this context because it was chosen.
    pub chosen: bool,
    /// The includes from the root down to this file: each the file holding the `` `include ``
    /// and its line (the directive, or the macro call that made it).
    pub chain: Vec<Place>,
    pub chain_cut: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Detail {
    pub invocation: Option<InvocationInfo>,
    pub macros: Vec<MacroState>,
    pub macros_total: usize,
    pub includes: Vec<IncludeRow>,
    pub includes_total: usize,
    /// Looks for shadowed copies stopped at the budget.
    pub probes_cut: bool,
    /// Lines in regions `` `ifdef `` removed from this file in this context.
    pub inactive_lines: u32,
    pub duplicates: Vec<Duplicate>,
    pub duplicates_cut: bool,
    /// Problems of the file as published, by code (`capped`: the file's list was cut).
    pub problem_counts: Vec<(String, usize)>,
    pub problems_capped: bool,
    /// The preprocessing problems, listed.
    pub preprocessing: Vec<Problem>,
}

#[derive(Debug, Default, Serialize)]
pub struct InvocationInfo {
    pub index: usize,
    pub mode: String,
    pub auto: bool,
    pub top_files: usize,
    pub defines: Vec<(String, Option<String>)>,
    pub defines_total: usize,
    pub undefines: Vec<String>,
    pub incdirs: Vec<String>,
    pub incdirs_total: usize,
    pub lib_dirs: usize,
    pub lib_files: usize,
}

/// A macro the file refers to (`` `NAME ``) or tests (`` `ifdef NAME `` ...), and the
/// definitions seen at those places in this context.
#[derive(Debug, Serialize)]
pub struct MacroState {
    pub name: String,
    pub used: bool,
    /// Named by `` `ifdef `` / `` `ifndef `` / `` `elsif ``.
    pub tested: bool,
    /// Named by `` `undef ``.
    pub undefined: bool,
    /// Distinct states seen, in the order first met.
    pub seen: Vec<SeenDef>,
    /// Distinct states not listed.
    pub states_cut: usize,
}

#[derive(Debug, Serialize)]
pub struct SeenDef {
    pub defined: bool,
    /// Where it is defined (`None`: on the command line, or not defined).
    pub at: Option<Place>,
    pub command_line: bool,
    /// Predefined by the simulator (`wrealZState`): not from the command line or a file.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub predefined: bool,
    /// `(a, b = 1)` for a function-like macro.
    pub params: Option<String>,
    pub value: String,
    /// Places in the file that saw this state.
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct IncludeRow {
    pub line: u32,
    pub text: String,
    pub resolved: Option<String>,
    /// The file was read there (false: missing, or recursive).
    pub entered: bool,
    /// What found it: `absolute`, `folder` (the including file's), `incdir`, `root`.
    pub step: Option<&'static str>,
    /// For `incdir`: its 1-based number and path.
    pub incdir: Option<(usize, String)>,
    /// Other candidates that exist now (a look at the file system today), in search order.
    pub shadowed: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Duplicate {
    pub kind: &'static str,
    pub name: String,
    pub here: Place,
    /// The other source definitions (by place: one header compiled many times is one).
    pub others: Vec<Place>,
    pub others_total: usize,
    /// The definition the build gives a use of the name from this context by default.
    pub default: Option<Place>,
    /// `first compiled` or `nearest copy`.
    pub rule: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Problem {
    pub code: String,
    pub line: u32,
    pub message: String,
}

impl Analysis {
    /// How `path` (known as `file` when the engine has it) is compiled.
    pub fn compile_context(&self, path: &Path, file: Option<FileId>) -> CompileContext {
        let mut out = CompileContext {
            uri: url_of(path).to_string(),
            known: file.is_some(),
            auto: self.setup.config.build_file.is_none(),
            build_file: self.setup.config.build_file.as_deref().map(|b| url_of(b).to_string()),
            ..Default::default()
        };
        for (i, inv) in self.setup.config.invocations.iter().enumerate() {
            for p in inv.skip.iter().filter(|p| dvi_project::glob::matches_path(p, path)) {
                out.skip_matches.push((i, p.clone()));
            }
        }
        let chosen = file.and_then(|f| self.chosen_unit(f));
        let lost = match file {
            Some(f) => self.selection.unavailable.get(&f),
            None => self.selection.unknown.get(path),
        };
        out.choice = match (chosen, lost) {
            (Some(_), _) => "chosen",
            (None, Some(k)) => {
                out.unavailable = Some((url_of(&k.root).to_string(), k.invocation.clone(), k.nth));
                "unavailable"
            }
            (None, None) => "default",
        };
        let Some(file) = file else { return out };
        let units = self.units_for(file);
        out.contexts_total = units.len();
        for (i, &uid) in units.iter().enumerate() {
            if i < MAX_CONTEXTS {
                out.contexts.push(self.context_of(uid, file, i + 1, chosen));
            } else if Some(uid) == chosen {
                out.chosen_context = Some(self.context_of(uid, file, i + 1, chosen));
            }
        }
        if let Some(uid) = self.context_unit(file) {
            out.detail = Some(self.context_detail(uid, file));
        }
        out
    }

    pub(crate) fn place(&self, span: Span) -> Option<Place> {
        if span.file == COMMAND_LINE_FILE || self.vfs.is_builtin(span.file) {
            return None;
        }
        let r = self.to_range(span)?;
        Some(Place { uri: url_of(&self.path(span.file)).to_string(), line: r.start.line })
    }

    fn context_of(&self, uid: UnitId, file: FileId, ordinal: usize, chosen: Option<UnitId>) -> Context {
        let u = self.unit(uid);
        let inv = self.setup.config.invocations.get(u.invocation);
        let root_path = self.path(u.root);
        let root_role = crate::select::root_role(u);
        let order = (root_role == "top").then(|| inv.and_then(|i| i.files.iter().position(|f| *f == root_path).map(|k| (k + 1, i.files.len())))).flatten();
        let mut chain = Vec::new();
        let mut chain_cut = false;
        if u.root != file {
            // The first include that read the file, then each include that read the file
            // holding it, back to the root.
            if let Some(first) = u.includes.iter().position(|i| i.resolved == Some(file) && i.entered) {
                let mut at = Some(first as u32);
                let mut seen = FxHashSet::default();
                while let Some(k) = at {
                    if chain.len() >= MAX_CHAIN || !seen.insert(k) {
                        chain_cut = true;
                        break;
                    }
                    let inc = &u.includes[k as usize];
                    chain.extend(self.place(inc.span));
                    at = inc.from;
                }
                chain.reverse();
            }
        }
        Context {
            invocation: u.invocation,
            mode: inv.map(|i| i.mode.clone()).unwrap_or_default(),
            root_role,
            root: url_of(&root_path).to_string(),
            order,
            is_root: u.root == file,
            ordinal,
            chosen: chosen == Some(uid),
            chain,
            chain_cut,
        }
    }

    fn context_detail(&self, uid: UnitId, file: FileId) -> Detail {
        let u = self.unit(uid);
        let mut d = Detail::default();
        let pp = self.setup.pp_configs.get(u.invocation).unwrap_or(&self.setup.pp_configs[0]);
        if let Some(inv) = self.setup.config.invocations.get(u.invocation) {
            d.invocation = Some(InvocationInfo {
                index: u.invocation,
                mode: inv.mode.clone(),
                auto: inv.auto,
                top_files: inv.files.len(),
                defines: inv.defines.iter().take(MAX_LISTED).cloned().collect(),
                defines_total: inv.defines.len(),
                undefines: inv.undefines.iter().take(MAX_LISTED).cloned().collect(),
                incdirs: inv.incdirs.iter().take(MAX_LISTED).map(|p| p.display().to_string()).collect(),
                incdirs_total: inv.incdirs.len(),
                lib_dirs: inv.lib_dirs.len(),
                lib_files: inv.lib_files.len(),
            });
        }
        // Macros: the recorded references and conditional names in this file. The names after
        // `` `undef `` are recorded with the conditional ones: told apart by the directive.
        let undefs: FxHashSet<u32> = self
            .file_tokens(file)
            .map(|t| t.windows(2).filter(|w| w[0].kind == TokKind::Directive && w[0].text.trim_start_matches('`') == "undef").map(|w| w[1].span.start).collect())
            .unwrap_or_default();
        #[derive(Default)]
        struct Acc {
            used: bool,
            tested: bool,
            undefined: bool,
            /// Each distinct state (definition place and text, or none) and its index in `seen`.
            keys: FxHashMap<Option<(FileId, u32, usize)>, usize>,
            seen: Vec<(Option<Arc<MacroDef>>, usize)>,
            cut: usize,
        }
        let mut order: Vec<String> = Vec::new();
        let mut states: FxHashMap<String, Acc> = FxHashMap::default();
        let uses = u.macro_uses.iter().filter(|m| m.name_span.file == file).map(|m| (&m.name, &m.def, 0u8, m.name_span.start));
        let names = u.macro_names.iter().filter(|m| m.span.file == file).map(|m| (&m.name, &m.def, 1u8, m.span.start));
        let mut counted: FxHashSet<&str> = FxHashSet::default();
        for (name, def, what, at) in uses.chain(names) {
            // Past the names listed, only counted.
            if order.len() >= MAX_MACROS && !states.contains_key(name.as_str()) {
                counted.insert(name.as_str());
                continue;
            }
            let e = states.entry(name.to_string()).or_insert_with(|| {
                order.push(name.to_string());
                Acc::default()
            });
            match what {
                0 => e.used = true,
                _ if undefs.contains(&at) => e.undefined = true,
                _ => e.tested = true,
            }
            // One definition (a shared table entry) is one state; not its text copied.
            let key = def.as_ref().map(|d| (d.name_span.file, d.name_span.start, Arc::as_ptr(d) as usize));
            let next = e.keys.len();
            let k = *e.keys.entry(key).or_insert(next);
            if k < e.seen.len() {
                e.seen[k].1 += 1;
            } else if e.seen.len() < MAX_STATES {
                e.seen.push((def.clone(), 1));
            } else if k == next {
                e.cut += 1;
            }
        }
        d.macros_total = order.len() + counted.len();
        for name in order.into_iter().take(MAX_MACROS) {
            let a = states.remove(&name).unwrap_or_default();
            let seen = a.seen.into_iter().map(|(def, count)| self.seen_def(def.as_deref(), count)).collect();
            d.macros.push(MacroState { name, used: a.used, tested: a.tested, undefined: a.undefined, seen, states_cut: a.cut });
        }
        // Includes executed in this file, with the step that found each and (now) what else
        // exists further down the search.
        let mut probes = 0usize;
        let here: Vec<_> = u.includes.iter().filter(|i| i.span.file == file).collect();
        d.includes_total = here.len();
        let folder = self.path(file).parent().map(Path::to_path_buf);
        for inc in here.into_iter().take(MAX_INCLUDES) {
            let line = self.to_range(inc.span).map_or(0, |r| r.start.line);
            let (step, incdir) = match inc.step {
                Some(IncludeStep::Absolute) => (Some("absolute"), None),
                Some(IncludeStep::IncluderFolder) => (Some("folder"), None),
                Some(IncludeStep::Incdir(k)) => (Some("incdir"), pp.incdirs.get(k as usize).map(|p| (k as usize + 1, p.display().to_string()))),
                Some(IncludeStep::Root) => (Some("root"), None),
                None => (None, None),
            };
            let mut shadowed = Vec::new();
            if let Some(found) = inc.step {
                let mut after = false;
                for (cand, s) in include_candidates(&inc.path_text, folder.as_deref(), pp) {
                    if s == found {
                        after = true;
                        continue;
                    }
                    if !after {
                        continue;
                    }
                    if probes >= MAX_PROBES {
                        d.probes_cut = true;
                        break;
                    }
                    probes += 1;
                    // Another spelling of the file found (an incdir that is its folder) is
                    // no other copy.
                    let known = self.vfs.lookup(&cand);
                    if known.is_some() && known == inc.resolved {
                        continue;
                    }
                    let uri = url_of(&cand).to_string();
                    if shadowed.len() < MAX_LISTED && !shadowed.contains(&uri) && dvi_project::fsx::is_file(&cand) {
                        shadowed.push(uri);
                    }
                }
            }
            d.includes.push(IncludeRow {
                line,
                text: inc.path_text.clone(),
                resolved: inc.resolved.map(|f| url_of(&self.path(f)).to_string()),
                entered: inc.entered,
                step,
                incdir,
                shadowed,
            });
        }
        d.inactive_lines = self.inactive_lines(&u.inactive, file);
        self.duplicates_in(uid, file, &mut d);
        // Problems as published (the first context's).
        let diags = self.file_diagnostics(file);
        d.problems_capped = diags.len() >= super::diagnostics::MAX_DIAGS_PER_FILE;
        let mut counts: FxHashMap<String, usize> = FxHashMap::default();
        for x in &diags {
            *counts.entry(x.code.to_string()).or_default() += 1;
        }
        let mut listed = FxHashSet::default();
        for x in u.pp_diags.iter().filter(|x| x.span.file == file) {
            if d.preprocessing.len() >= MAX_PROBLEMS {
                break;
            }
            let line = self.to_range(x.span).map_or(0, |r| r.start.line);
            if listed.insert((x.code.to_string(), line, x.message.clone())) {
                d.preprocessing.push(Problem { code: x.code.to_string(), line, message: x.message.clone() });
            }
        }
        let mut counts: Vec<(String, usize)> = counts.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        d.problem_counts = counts;
        d
    }

    fn seen_def(&self, def: Option<&MacroDef>, count: usize) -> SeenDef {
        let Some(def) = def else { return SeenDef { defined: false, at: None, command_line: false, predefined: false, params: None, value: String::new(), count } };
        let params = def.params.as_ref().map(|ps| {
            let ps: Vec<String> = ps.iter().map(|p| match &p.default {
                Some(t) => format!("{} = {}", p.name, t.iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join(" ")),
                None => p.name.to_string(),
            }).collect();
            format!("({})", ps.join(", "))
        });
        let mut value: String = def.body_text.chars().take(VALUE_CHARS).collect();
        if def.body_text.chars().count() > VALUE_CHARS {
            value.push('…');
        }
        let predefined = dvi_syntax::preproc::predefined_doc(def).is_some();
        SeenDef { defined: true, at: self.place(def.name_span), command_line: def.name_span.file == COMMAND_LINE_FILE && !predefined, predefined, params, value, count }
    }

    /// Lines of `file` in the union of the removed regions recorded for it. A region runs from
    /// one directive to the next: the first directive's line never counts (the region starts in
    /// it, after the directive or on its condition), the last line only when it holds removed
    /// text before the closing directive.
    fn inactive_lines(&self, spans: &[Span], file: FileId) -> u32 {
        let Some(text) = self.file_text(file) else { return 0 };
        let Some(li) = self.file_lines(file) else { return 0 };
        let blank = |a: usize, b: usize| text.get(a.min(text.len())..b.min(text.len())).is_none_or(|s| s.trim().is_empty());
        let mut ranges: Vec<(u32, u32)> = spans
            .iter()
            .filter(|s| s.file == file && s.end > s.start)
            .filter_map(|s| {
                let (l1, _) = li.position(&text, s.start);
                let (l2, _) = li.position(&text, s.end);
                let start_of = |l: u32| li.offset(&text, l, 0) as usize;
                let first = l1 + 1;
                let last = if blank(start_of(l2), s.end as usize) { l2.checked_sub(1)? } else { l2 };
                (first <= last).then_some((first, last))
            })
            .collect();
        ranges.sort_unstable();
        let mut total = 0u32;
        let mut cur: Option<(u32, u32)> = None;
        for (a, b) in ranges {
            match cur {
                Some((x, y)) if a <= y + 1 => cur = Some((x, y.max(b))),
                Some((x, y)) => {
                    total += y - x + 1;
                    cur = Some((a, b));
                }
                None => cur = Some((a, b)),
            }
        }
        if let Some((x, y)) = cur {
            total += y - x + 1;
        }
        total
    }

    /// Design elements, packages and compilation-unit items this file declares that have other
    /// source definitions.
    fn duplicates_in(&self, uid: UnitId, file: FileId, d: &mut Detail) {
        let u = self.unit(uid);
        let auto = self.setup.config.invocations.get(u.invocation).is_some_and(|i| i.auto);
        let root = u.parsed.decl(ParsedUnit::ROOT);
        let mut names = 0usize;
        // A declaration in no other place: a forward `typedef class`, an out-of-block body.
        let real = |x: &dvi_syntax::ast::Decl| !x.has(dflags::FORWARD) && !x.has(dflags::OUT_OF_BLOCK);
        let mut inspected = FxHashSet::default();
        for &c in &root.children {
            let decl = u.parsed.decl(c);
            if decl.name.is_empty() || decl.name_span.file != file || !real(decl) || !inspected.insert(decl.name_span.start) {
                continue;
            }
            let all = if decl.kind.is_definition() {
                self.global.definitions.get(&decl.name)
            } else if decl.kind == DeclKind::Package {
                self.global.packages.get(&decl.name)
            } else {
                self.global.unit_items.get(&decl.name)
            };
            let Some(all) = all else { continue };
            let here = DeclRef { unit: uid, decl: c as DeclId };
            let key = |r: &DeclRef| {
                let x = self.decl(*r);
                (x.name_span.file, x.name_span.start)
            };
            let mine = key(&here);
            let mut seen_places = FxHashSet::default();
            let mut places: Vec<(FileId, u32)> = Vec::new();
            for (i, r) in all.iter().enumerate() {
                if i % 4096 == 4095 && self.cancelled() {
                    return;
                }
                let k = key(r);
                if k != mine && real(self.decl(*r)) && seen_places.insert(k) && places.len() < MAX_DUP_PLACES {
                    places.push(k);
                }
            }
            if places.is_empty() {
                continue;
            }
            // A compilation-unit item: a use in this unit gets this unit's own declaration.
            let unit_item = !decl.kind.is_definition() && decl.kind != DeclKind::Package;
            if names >= MAX_DUP_NAMES {
                d.duplicates_cut = true;
                break;
            }
            names += 1;
            let span_of = |(f, s): (FileId, u32)| Span::new(f, s, s);
            let Some(here_place) = self.place(decl.name_span) else { continue };
            d.duplicates.push(Duplicate {
                kind: decl.kind.label(),
                name: decl.name.to_string(),
                here: here_place.clone(),
                others_total: seen_places.len(),
                others: places.iter().filter_map(|p| self.place(span_of(*p))).collect(),
                default: if unit_item { Some(here_place.clone()) } else { self.pick_global(uid, all).and_then(|r| self.place(self.decl(r).name_span)) },
                rule: if unit_item {
                    "this compilation unit's own"
                } else if auto {
                    "nearest copy"
                } else {
                    "first compiled"
                },
            });
        }
    }

    /// The include written at `off` in `file` (its file name), in the first unit that has it:
    /// what it read and what found it.
    pub fn include_at(&self, file: FileId, off: u32) -> Option<(String, Option<String>, String, Span)> {
        // Most hovers are not on an include: their line says so without looking at any unit.
        let text = self.file_text(file)?;
        let li = self.file_lines(file)?;
        let (line, _) = li.position(&text, off);
        let (a, b) = (li.offset(&text, line, 0) as usize, li.offset(&text, line + 1, 0) as usize);
        let l = text.get(a..b.max(a)).unwrap_or("");
        if !l.contains('`') {
            return None;
        }
        // A macro call that writes an include (no `include` on the line): the editor's unit only.
        let units = self.editor_units(file);
        let n = if l.contains("include") { units.len() } else { 1 };
        for uid in units.into_iter().take(n) {
            let u = self.unit(uid);
            // One directive read several times (a file included again): a time it was refused
            // (recursive, too deep) is what is said.
            let here = |i: &&dvi_syntax::preproc::IncludeInfo| i.span.file == file && i.span.start <= off && off <= i.span.end;
            let Some(inc) = u.includes.iter().filter(here).find(|i| i.resolved.is_some() && !i.entered).or_else(|| u.includes.iter().find(here)) else { continue };
            let pp = self.setup.pp_configs.get(u.invocation).unwrap_or(&self.setup.pp_configs[0]);
            let how = match inc.step {
                Some(IncludeStep::Absolute) => "an absolute path".to_string(),
                Some(IncludeStep::IncluderFolder) => "in the folder of the including file".to_string(),
                Some(IncludeStep::Incdir(k)) => format!("in include directory {} of {}: {}", k + 1, pp.incdirs.len(), md_code(&pp.incdirs.get(k as usize).map(|p| p.display().to_string()).unwrap_or_default())),
                Some(IncludeStep::Root) => "in the project folder (after the include directories)".to_string(),
                None if Path::new(&inc.path_text).is_absolute() => "not found (an absolute path: nothing else is searched)".to_string(),
                None => format!("not found: looked in the including file's folder, {} include director{} and the project folder", pp.incdirs.len(), if pp.incdirs.len() == 1 { "y" } else { "ies" }),
            };
            let resolved = inc.resolved.map(|f| self.path(f).display().to_string());
            let how = if inc.resolved.is_some() && !inc.entered { format!("{how}; not read here (recursive, or included too deeply)") } else { how };
            return Some((inc.path_text.clone(), resolved, how, inc.span));
        }
        None
    }
}

/// `s` as inline code in Markdown, whatever backticks it holds.
pub fn md_code(s: &str) -> String {
    let flat = s.replace(['\r', '\n'], " ");
    let mut run = 0usize;
    let mut cur = 0usize;
    for c in flat.chars() {
        cur = if c == '`' { cur + 1 } else { 0 };
        run = run.max(cur);
    }
    let fence = "`".repeat(run + 1);
    let pad = if run > 0 { " " } else { "" };
    format!("{fence}{pad}{flat}{pad}{fence}")
}
