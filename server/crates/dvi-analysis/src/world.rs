//! Project compilation and analysis snapshots.

use crate::vfs::{Vfs, BUILTIN_DIR};
use dvi_project::{BuildConfig, Phase, Progress};
use dvi_syntax::ast::{DeclId, DeclKind, ParsedUnit, RefId, RefKind};
use dvi_syntax::preproc::{command_line_define, def_identity, preprocess, IncludeInfo, MacroDef, MacroName, MacroTable, MacroUse, PpConfig};
use dvi_syntax::{parser, Diag, FileId, SourceProvider, Span};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub type UnitId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeclRef {
    pub unit: UnitId,
    pub decl: DeclId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    Builtin,
    /// A top file of the build configuration (or a library file pulled in by `-y`/`-v`).
    Project,
    /// A file opened in the editor that is not part of the build ("non-top file").
    AdHoc,
}

/// Positions of everything navigable in one file of a unit.
#[derive(Default, Debug)]
pub struct FilePositions {
    /// Reference segments: (span, reference, segment index), sorted by start.
    pub refs: Vec<(Span, RefId, u16)>,
    /// Declarations with a real name in this file, sorted by name start.
    pub decls: Vec<DeclId>,
}

/// The heavy parts are shared, so that a copy that only differs in its macros is cheap (see
/// `Analysis::propagate_macros`).
#[derive(Clone)]
pub struct Unit {
    pub id: UnitId,
    pub root: FileId,
    pub kind: UnitKind,
    /// Index of the build invocation.
    pub invocation: usize,
    pub parsed: Arc<ParsedUnit>,
    pub includes: Vec<IncludeInfo>,
    pub macro_uses: Vec<MacroUse>,
    pub macro_defs: Vec<Arc<MacroDef>>,
    /// Macro names in `` `ifdef ``, `` `ifndef ``, `` `elsif `` and `` `undef ``.
    pub macro_names: Vec<MacroName>,
    /// Macros called through a name built by token pasting (`` `dv_``SEV_ ``).
    pub pasted_macros: Vec<SmolStr>,
    pub files: Vec<FileId>,
    pub inactive: Vec<Span>,
    pub pp_diags: Vec<Diag>,
    pub macros_before: MacroTable,
    pub macros_after: MacroTable,
    pub positions: Arc<FxHashMap<FileId, FilePositions>>,
    /// Name -> reference segments with that name.
    pub names: Arc<FxHashMap<SmolStr, Vec<(RefId, u16)>>>,
    /// The text of each file exactly as this unit was compiled from it.
    pub sources: Arc<FxHashMap<FileId, Arc<Source>>>,
    pub parse_ms: u128,
    /// A `-y`/`-v` library file, compiled because it provides a missing design element.
    pub library: bool,
}

/// A file's text as an analysis snapshot saw it. Positions in the snapshot are converted with
/// this text, never with the (possibly newer) text in the file store.
pub struct Source {
    pub text: Arc<str>,
    /// Editor document version of this text, when it came from an open editor.
    pub lsp_version: Option<i32>,
    lines: std::sync::OnceLock<Arc<dvi_syntax::LineIndex>>,
}

impl Source {
    pub fn new(text: Arc<str>, lsp_version: Option<i32>) -> Source {
        Source { text, lsp_version, lines: std::sync::OnceLock::new() }
    }

    pub fn lines(&self) -> Arc<dvi_syntax::LineIndex> {
        self.lines.get_or_init(|| Arc::new(dvi_syntax::LineIndex::new(&self.text))).clone()
    }
}

impl Unit {
    pub fn decl(&self, id: DeclId) -> &dvi_syntax::ast::Decl {
        self.parsed.decl(id)
    }
}

#[derive(Default)]
pub struct GlobalIndex {
    pub packages: FxHashMap<SmolStr, Vec<DeclRef>>,
    /// Modules, interfaces, programs, checkers, primitives.
    pub definitions: FxHashMap<SmolStr, Vec<DeclRef>>,
    /// Compilation-unit level declarations (classes, typedefs, functions outside packages).
    pub unit_items: FxHashMap<SmolStr, Vec<DeclRef>>,
    /// Imports at compilation-unit level of project files: (unit, invocation, package, item).
    /// Files compiled later in the same invocation see them (single compilation unit).
    pub unit_imports: Vec<(UnitId, usize, SmolStr, Option<SmolStr>)>,
    /// Target module name -> instances that `bind` adds to it.
    pub binds: FxHashMap<SmolStr, Vec<DeclRef>>,
    pub macros: FxHashMap<SmolStr, Vec<Arc<MacroDef>>>,
    /// Base class name -> classes that extend a class of that name.
    pub subclasses: FxHashMap<SmolStr, Vec<DeclRef>>,
    pub std_pkg: Option<DeclRef>,
}

/// Everything needed to (re)compile units.
pub struct ProjectSetup {
    pub root: PathBuf,
    pub config: BuildConfig,
    pub pp_configs: Vec<PpConfig>,
    pub initial_macros: Vec<MacroTable>,
    pub env: FxHashMap<String, String>,
}

impl ProjectSetup {
    pub fn new(root: &Path, config: BuildConfig, env: FxHashMap<String, String>) -> ProjectSetup {
        let mut pp_configs = Vec::new();
        let mut initial_macros = Vec::new();
        for inv in &config.invocations {
            pp_configs.push(PpConfig { incdirs: inv.incdirs.clone(), root_dir: root.to_path_buf() });
            let mut m = MacroTable::new();
            for (n, v) in &inv.defines {
                m.insert(SmolStr::new(n), Arc::new(command_line_define(n, v.as_deref())));
            }
            for n in &inv.undefines {
                m.remove(n.as_str());
            }
            initial_macros.push(m);
        }
        if pp_configs.is_empty() {
            pp_configs.push(PpConfig { incdirs: Vec::new(), root_dir: root.to_path_buf() });
            initial_macros.push(MacroTable::new());
        }
        ProjectSetup { root: root.to_path_buf(), config, pp_configs, initial_macros, env }
    }
}

/// An immutable analysis snapshot. Cloning is cheap.
#[derive(Clone)]
pub struct Analysis {
    pub vfs: Arc<Vfs>,
    pub setup: Arc<ProjectSetup>,
    pub units: Vec<Option<Arc<Unit>>>,
    pub global: Arc<GlobalIndex>,
    pub file_units: Arc<FxHashMap<FileId, Vec<UnitId>>>,
    pub build_ms: u128,
    /// Set by the server: a number that grows with each snapshot it uses.
    pub revision: u64,
    /// Without a build file: uses not checked because code that may declare them is removed by
    /// `` `ifdef ``, by region (file, start). Counted for each snapshot when first needed
    /// (`Analysis::hidden_uses`).
    pub hidden_uses: Arc<std::sync::OnceLock<FxHashMap<(FileId, u32), usize>>>,
    /// Without a build file: the copy of a design element defined more than once that each
    /// instance means (`Analysis::instance_definitions`).
    pub instance_defs: Arc<std::sync::OnceLock<FxHashMap<(UnitId, Span), DeclRef>>>,
}

const PRELUDE: &str = include_str!("prelude.sv");

/// Flags the innermost scope around each encrypted region: names it uses may be declared in
/// the ciphertext.
fn mark_encrypted(parsed: &mut dvi_syntax::ast::ParsedUnit, regions: &[Span]) {
    use dvi_syntax::ast::{dflags, DeclKind};
    for r in regions {
        let inner = parsed
            .decls
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, d)| {
                matches!(
                    d.kind,
                    DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Checker | DeclKind::Package
                        | DeclKind::Class | DeclKind::InterfaceClass | DeclKind::Function | DeclKind::Task
                ) && d.span.file == r.file
                    && d.span.start <= r.start
                    && r.end <= d.span.end
            })
            .min_by_key(|(_, d)| d.span.end - d.span.start)
            .map(|(i, _)| i);
        if let Some(i) = inner {
            parsed.decls[i].flags |= dflags::ENCRYPTED;
        }
    }
}

fn make_unit(
    vfs: &Vfs,
    id: UnitId,
    root: FileId,
    kind: UnitKind,
    invocation: usize,
    pp: dvi_syntax::preproc::PpResult,
    macros_before: MacroTable,
) -> Unit {
    let t0 = Instant::now();
    let mut parsed = parser::parse(&pp.tokens, root);
    mark_encrypted(&mut parsed, &pp.protected);
    let mut positions: FxHashMap<FileId, FilePositions> = FxHashMap::default();
    let mut names: FxHashMap<SmolStr, Vec<(RefId, u16)>> = FxHashMap::default();
    for (rid, r) in parsed.refs.iter().enumerate() {
        for (si, s) in r.segs.iter().enumerate() {
            names.entry(s.name.clone()).or_default().push((rid as RefId, si as u16));
            if !s.from_macro {
                positions.entry(s.span.file).or_default().refs.push((s.span, rid as RefId, si as u16));
            }
        }
    }
    for (did, d) in parsed.decls.iter().enumerate() {
        if did == 0 || d.name.is_empty() || d.has(dvi_syntax::ast::dflags::FROM_MACRO) {
            continue;
        }
        positions.entry(d.name_span.file).or_default().decls.push(did as DeclId);
    }
    for p in positions.values_mut() {
        p.refs.sort_by_key(|(s, _, _)| (s.start, s.end));
        p.decls.sort_by_key(|d| parsed.decls[*d as usize].name_span.start);
    }
    let sources = pp
        .sources
        .iter()
        .map(|(f, text)| (*f, Arc::new(Source::new(text.clone(), vfs.lsp_version_for(*f, text)))))
        .collect();
    Unit {
        id,
        root,
        kind,
        invocation,
        parsed: Arc::new(parsed),
        includes: pp.includes,
        macro_uses: pp.macro_uses,
        macro_defs: pp.macro_defs,
        macro_names: pp.macro_names,
        pasted_macros: pp.pasted_macros,
        files: pp.files,
        inactive: pp.inactive,
        pp_diags: pp.diags,
        macros_before,
        macros_after: pp.macros_after,
        positions: Arc::new(positions),
        names: Arc::new(names),
        sources: Arc::new(sources),
        parse_ms: t0.elapsed().as_millis(),
        library: false,
    }
}

/// Hash of a unit's declarations that other files can refer to.
fn interface_fingerprint(u: &Unit) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    for d in &u.parsed.decls {
        // Skip anything inside a subroutine, block, constraint or covergroup body.
        let mut p = d.parent;
        let mut local = false;
        while let Some(x) = p {
            let pd = u.parsed.decl(x);
            if matches!(pd.kind, DeclKind::Function | DeclKind::Task | DeclKind::Block | DeclKind::Constraint | DeclKind::Covergroup) {
                local = true;
                break;
            }
            p = pd.parent;
        }
        if local {
            continue;
        }
        // Names, not ids: ids shift when a local declaration is added above.
        let parent = d.parent.map(|x| u.parsed.decl(x).name.as_str());
        (d.name.as_str(), d.kind as u8, d.flags, parent).hash(&mut h);
        d.ty.as_ref().map(|t| t.text.as_str()).hash(&mut h);
        d.extends.as_ref().map(|t| t.text.as_str()).hash(&mut h);
        d.dir.map(|x| x as u8).hash(&mut h);
        d.detail.as_str().hash(&mut h);
        // A package's imports and exports decide what `pkg::name` finds in other files.
        for i in d.imports.iter().chain(&d.exports) {
            (i.pkg.as_str(), i.item.as_deref()).hash(&mut h);
        }
        d.exports.len().hash(&mut h);
    }
    h.finish()
}

impl Analysis {
    /// Compiles the whole project.
    pub fn build(vfs: Arc<Vfs>, setup: Arc<ProjectSetup>) -> Analysis {
        Self::build_with(vfs, setup, &Progress::new()).expect("a build that nothing cancels finishes")
    }

    /// [`Analysis::build`], reporting each phase on `progress`; `None` when it is cancelled.
    pub fn build_with(vfs: Arc<Vfs>, setup: Arc<ProjectSetup>, progress: &Progress) -> Option<Analysis> {
        let t0 = Instant::now();
        vfs.clear_dir_cache();
        let mut units: Vec<Option<Arc<Unit>>> = Vec::new();
        units.push(Some(Arc::new(Self::builtin_unit(&vfs))));
        // Read and lex the top files in parallel; preprocessing reads them one after another,
        // which adds up where each read waits for the file system (network or WSL drives).
        let tops: Vec<FileId> = setup.config.invocations.iter().flat_map(|i| &i.files).map(|f| vfs.file_id(f)).collect();
        progress.begin(Phase::Reading, tops.len());
        tops.par_iter().for_each(|&id| {
            if !progress.is_cancelled() {
                vfs.source(id);
            }
            progress.step();
        });
        if progress.is_cancelled() {
            return None;
        }
        // Likewise list the include directories: an `include not found in the first ones
        // probes all the others.
        let mut incdirs: Vec<&PathBuf> = setup.pp_configs.iter().flat_map(|c| &c.incdirs).collect();
        incdirs.sort();
        incdirs.dedup();
        incdirs.par_iter().for_each(|d| vfs.list_dir(d));
        // Preprocess in order (macros flow from file to file), parse in parallel.
        progress.begin(Phase::Preprocessing, tops.len());
        let mut jobs = Vec::new();
        for (inv_idx, inv) in setup.config.invocations.iter().enumerate() {
            let mut macros = setup.initial_macros[inv_idx].clone();
            let cfg = &setup.pp_configs[inv_idx];
            for f in &inv.files {
                if progress.is_cancelled() {
                    return None;
                }
                let id = vfs.file_id(f);
                let before = macros.clone();
                let pp = preprocess(&*vfs, cfg, id, macros);
                macros = pp.macros_after.clone();
                jobs.push((id, inv_idx, pp, before));
                progress.step();
            }
        }
        let base = units.len() as UnitId;
        progress.begin(Phase::Parsing, jobs.len());
        let built: Vec<Option<Unit>> = jobs
            .into_par_iter()
            .enumerate()
            .map(|(i, (id, inv, pp, before))| {
                if progress.is_cancelled() {
                    return None;
                }
                let u = make_unit(&vfs, base + i as UnitId, id, UnitKind::Project, inv, pp, before);
                progress.step();
                Some(u)
            })
            .collect();
        if progress.is_cancelled() {
            return None;
        }
        units.extend(built.into_iter().flatten().map(|u| Some(Arc::new(u))));
        progress.begin(Phase::Indexing, 0);
        let mut a = Analysis {
            vfs,
            setup,
            units,
            global: Arc::new(GlobalIndex::default()),
            file_units: Arc::new(FxHashMap::default()),
            build_ms: 0,
            revision: 0,
            hidden_uses: Arc::default(),
            instance_defs: Arc::default(),
        };
        a.reindex();
        a.update_library_units(Some(progress));
        // Cancelled while indexing or compiling `-y`/`-v` libraries: a newer build replaces it.
        if progress.is_cancelled() {
            return None;
        }
        a.build_ms = t0.elapsed().as_millis();
        Some(a)
    }

    fn builtin_unit(vfs: &Vfs) -> Unit {
        let path = PathBuf::from(BUILTIN_DIR).join("std.sv");
        let id = vfs.set_overlay(&path, Arc::from(PRELUDE));
        let pp = preprocess(vfs, &PpConfig::default(), id, MacroTable::new());
        make_unit(vfs, 0, id, UnitKind::Builtin, usize::MAX, pp, MacroTable::new())
    }

    /// Compiles the `-y`/`-v` library files that provide otherwise missing design elements,
    /// in as many rounds as their own instances need. After an edit, the library units that
    /// are still needed (with the same macros) are kept, and those that nothing needs any more
    /// (the last instance was removed) are dropped: a full build would not compile them.
    fn update_library_units(&mut self, progress: Option<&Progress>) {
        let old: Vec<UnitId> = self.units.iter().flatten().filter(|u| u.library).map(|u| u.id).collect();
        let has_libs = self.setup.config.invocations.iter().any(|i| !i.lib_dirs.is_empty() || !i.lib_files.is_empty());
        if !has_libs && old.is_empty() {
            return;
        }
        let definitions = |u: &Unit| -> Vec<SmolStr> {
            u.parsed
                .decl(ParsedUnit::ROOT)
                .children
                .iter()
                .map(|c| u.parsed.decl(*c))
                .filter(|d| d.kind.is_definition() && !d.name.is_empty() && !d.has(dvi_syntax::ast::dflags::OUT_OF_BLOCK))
                .map(|d| d.name.clone())
                .collect()
        };
        // Only the build's own files count, as when a full build adds library files (before any
        // ad-hoc unit exists): which library files are compiled does not depend on what is open.
        let own = |u: &Unit| u.kind != UnitKind::AdHoc && !u.library;
        let mut defined: FxHashSet<SmolStr> = self.units.iter().flatten().filter(|u| own(u)).flat_map(|u| definitions(u)).collect();
        // Library units in the order a full build adds them.
        let mut kept: Vec<UnitId> = Vec::new();
        let mut changed = false;
        for _round in 0..8 {
            if progress.is_some_and(|p| p.is_cancelled()) {
                return;
            }
            let mut missing: FxHashSet<SmolStr> = FxHashSet::default();
            for u in self.units.iter().flatten().filter(|u| own(u) || kept.contains(&u.id)) {
                for r in &u.parsed.refs {
                    if r.kind == RefKind::Definition && !defined.contains(&r.segs[0].name) {
                        missing.insert(r.segs[0].name.clone());
                    }
                }
            }
            if missing.is_empty() {
                break;
            }
            let mut new_files: Vec<(PathBuf, usize)> = Vec::new();
            for (inv_idx, inv) in self.setup.config.invocations.iter().enumerate() {
                let exts: Vec<String> = if inv.libext.is_empty() { vec![".v".into(), ".sv".into()] } else { inv.libext.clone() };
                for name in &missing {
                    'dirs: for d in &inv.lib_dirs {
                        for e in &exts {
                            let p = d.join(format!("{name}{e}"));
                            if self.vfs.is_file(&p) {
                                new_files.push((p, inv_idx));
                                break 'dirs;
                            }
                        }
                    }
                }
                for f in &inv.lib_files {
                    new_files.push((f.clone(), inv_idx));
                }
            }
            let mut added = false;
            for (p, inv_idx) in new_files {
                let id = self.vfs.file_id(&p);
                if self.units.iter().flatten().any(|u| u.root == id && (own(u) || kept.contains(&u.id))) {
                    continue;
                }
                // The macro state after the library unit added before it, else after the
                // invocation's files.
                let macros = kept
                    .iter()
                    .rev()
                    .map(|k| self.unit(*k))
                    .find(|u| u.invocation == inv_idx)
                    .map(|u| u.macros_after.clone())
                    .unwrap_or_else(|| self.invocation_macros(inv_idx));
                let reuse = old.iter().copied().find(|o| {
                    let u = self.unit(*o);
                    !kept.contains(o) && u.root == id && u.invocation == inv_idx && u.macros_before.same_definitions(&macros)
                });
                let uid = match reuse {
                    Some(o) => o,
                    None => {
                        let pp = preprocess(&*self.vfs, &self.setup.pp_configs[inv_idx], id, macros.clone());
                        let uid = self.units.len() as UnitId;
                        let mut u = make_unit(&self.vfs, uid, id, UnitKind::Project, inv_idx, pp, macros);
                        u.library = true;
                        self.units.push(Some(Arc::new(u)));
                        changed = true;
                        uid
                    }
                };
                kept.push(uid);
                defined.extend(definitions(self.unit(uid)));
                added = true;
            }
            if !added {
                break;
            }
        }
        for o in old {
            if !kept.contains(&o) {
                self.units[o as usize] = None;
                changed = true;
            }
        }
        if changed {
            self.reindex();
        }
    }

    /// Macro state at the end of an invocation's own files (not its library files).
    fn invocation_macros(&self, inv: usize) -> MacroTable {
        self.units
            .iter()
            .flatten()
            .filter(|u| u.kind == UnitKind::Project && !u.library && u.invocation == inv)
            .last()
            .map(|u| u.macros_after.clone())
            .unwrap_or_else(|| self.setup.initial_macros.get(inv).cloned().unwrap_or_default())
    }

    /// Macro table at the end of an invocation.
    pub fn last_macros(&self, inv: usize) -> MacroTable {
        self.units
            .iter()
            .flatten()
            .filter(|u| u.kind == UnitKind::Project && u.invocation == inv)
            .last()
            .map(|u| u.macros_after.clone())
            .unwrap_or_else(|| self.setup.initial_macros.get(inv).cloned().unwrap_or_default())
    }

    /// Rebuilds the global index and the file-to-unit map.
    pub fn reindex(&mut self) {
        let mut g = GlobalIndex::default();
        let mut fu: FxHashMap<FileId, Vec<UnitId>> = FxHashMap::default();
        for u in self.units.iter().flatten() {
            for f in &u.files {
                fu.entry(*f).or_default().push(u.id);
            }
            let root = u.parsed.decl(ParsedUnit::ROOT);
            if u.kind == UnitKind::Project {
                for imp in &root.imports {
                    g.unit_imports.push((u.id, u.invocation, imp.pkg.clone(), imp.item.clone()));
                }
            }
            for (target, inst) in &u.parsed.binds {
                g.binds.entry(target.clone()).or_default().push(DeclRef { unit: u.id, decl: *inst });
            }
            for &c in &root.children {
                let d = u.parsed.decl(c);
                if d.name.is_empty() || d.has(dvi_syntax::ast::dflags::OUT_OF_BLOCK) {
                    continue;
                }
                let r = DeclRef { unit: u.id, decl: c };
                if d.kind == DeclKind::Package {
                    if u.kind == UnitKind::Builtin && d.name == "std" {
                        g.std_pkg = Some(r);
                    }
                    g.packages.entry(d.name.clone()).or_default().push(r);
                } else if d.kind.is_definition() {
                    g.definitions.entry(d.name.clone()).or_default().push(r);
                } else {
                    g.unit_items.entry(d.name.clone()).or_default().push(r);
                }
            }
            for (i, d) in u.parsed.decls.iter().enumerate() {
                if matches!(d.kind, DeclKind::Class | DeclKind::InterfaceClass) {
                    let bases = d.extends.iter().chain(d.implements.iter());
                    for b in bases {
                        if let Some(rid) = b.ref_id {
                            if let Some(last) = u.parsed.reference(rid).segs.last() {
                                g.subclasses.entry(last.name.clone()).or_default().push(DeclRef { unit: u.id, decl: i as DeclId });
                            }
                        }
                    }
                }
            }
            for m in &u.macro_defs {
                g.macros.entry(m.name.clone()).or_default().push(m.clone());
            }
        }
        // Command-line defines.
        for t in &self.setup.initial_macros {
            for (n, m) in t.iter() {
                g.macros.entry(n.clone()).or_default().push(m.clone());
            }
        }
        // Prefer project units, in compilation order, then library units, over ad-hoc ones
        // for the same name. Library and ad-hoc units come in the order they were needed or
        // opened, which depends on the editing session: order them by path.
        let units = &self.units;
        let order: FxHashMap<UnitId, (u8, u32, PathBuf)> = units
            .iter()
            .flatten()
            .map(|u| {
                let key = match u.kind {
                    UnitKind::Project if u.library => (1, 0, self.vfs.path(u.root)),
                    UnitKind::Project => (0, u.id, PathBuf::new()),
                    UnitKind::Builtin => (2, u.id, PathBuf::new()),
                    UnitKind::AdHoc => (3, 0, self.vfs.path(u.root)),
                };
                (u.id, key)
            })
            .collect();
        for v in g.packages.values_mut().chain(g.definitions.values_mut()).chain(g.unit_items.values_mut()) {
            v.sort_by(|a, b| order.get(&a.unit).cmp(&order.get(&b.unit)).then(a.decl.cmp(&b.decl)));
        }
        self.global = Arc::new(g);
        self.file_units = Arc::new(fu);
        self.instance_defs = Arc::default();
    }

    pub fn unit(&self, id: UnitId) -> &Arc<Unit> {
        self.units[id as usize].as_ref().expect("unit removed")
    }

    pub fn try_unit(&self, id: UnitId) -> Option<&Arc<Unit>> {
        self.units.get(id as usize).and_then(|u| u.as_ref())
    }

    pub fn decl(&self, r: DeclRef) -> &dvi_syntax::ast::Decl {
        self.unit(r.unit).parsed.decl(r.decl)
    }

    pub fn units_of_file(&self, f: FileId) -> &[UnitId] {
        self.file_units.get(&f).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Recompiles the units that contain any of `files`.
    pub fn update(&self, files: &[FileId]) -> Analysis {
        let mut affected: Vec<UnitId> = Vec::new();
        for f in files {
            for u in self.units_of_file(*f) {
                if !affected.contains(u) {
                    affected.push(*u);
                }
            }
        }
        if affected.is_empty() {
            return self.clone();
        }
        let mut next = self.clone();
        next.hidden_uses = Arc::default();
        next.instance_defs = Arc::default();
        let rebuilt: Vec<Unit> = affected
            .par_iter()
            .map(|&uid| {
                let old = self.unit(uid);
                self.recompile(old, old.macros_before.clone())
            })
            .collect();
        for u in rebuilt {
            let id = u.id as usize;
            next.units[id] = Some(Arc::new(u));
        }
        next.propagate_macros(self, &affected);
        next.reindex();
        next.update_library_units(None);
        next
    }

    fn recompile(&self, old: &Unit, macros_before: MacroTable) -> Unit {
        let cfg = self.pp_config_for(old);
        let pp = preprocess(&*self.vfs, cfg, old.root, macros_before.clone());
        let mut u = make_unit(&self.vfs, old.id, old.root, old.kind, old.invocation, pp, macros_before);
        u.library = old.library;
        u
    }

    /// After `changed` units were recompiled: when one of them now leaves different macros
    /// behind, the files compiled after it in the same invocation are recompiled with the new
    /// macros, until the macro state is back to what it was.
    ///
    /// Definitions that only moved (a line added above a `` `define ``) expand the same way,
    /// so the files after them are not compiled again; but their macro uses and macro tables
    /// are pointed at the new definitions, or they would lead to the old location.
    fn propagate_macros(&mut self, before: &Analysis, changed: &[UnitId]) {
        // Pending macro state per invocation, valid for the next unit of that invocation.
        let mut carry: FxHashMap<usize, MacroTable> = FxHashMap::default();
        // Moved definitions: identity of the old one -> the new one.
        let mut moved: FxHashMap<usize, Arc<MacroDef>> = FxHashMap::default();
        // Tables already pointed at the moved definitions, by identity.
        let mut replaced: FxHashMap<usize, Option<MacroTable>> = FxHashMap::default();
        let ids: Vec<UnitId> = self.units.iter().flatten().map(|u| u.id).collect();
        for uid in ids {
            let u = self.unit(uid).clone();
            let old = before.try_unit(uid);
            let edited = changed.contains(&uid);
            let recompiled = match u.kind {
                UnitKind::Builtin => continue,
                // Ad-hoc files see the final state of their invocation.
                UnitKind::AdHoc => match carry.get(&u.invocation) {
                    Some(m) => {
                        let nu = self.recompile(&u, m.clone());
                        self.units[uid as usize] = Some(Arc::new(nu));
                        true
                    }
                    None => false,
                },
                UnitKind::Project => {
                    let old_after = old.map(|o| o.macros_after.clone());
                    if let Some(m) = carry.get(&u.invocation).cloned() {
                        // A file before this one changed the macros: compile again with them.
                        let nu = self.recompile(&u, m);
                        let same = old_after.as_ref().is_some_and(|o| nu.macros_after.same_definitions(o));
                        if same {
                            carry.remove(&u.invocation);
                        } else {
                            carry.insert(u.invocation, nu.macros_after.clone());
                        }
                        self.units[uid as usize] = Some(Arc::new(nu));
                        true
                    } else {
                        if edited {
                            let same = old_after.as_ref().is_some_and(|o| u.macros_after.same_definitions(o));
                            if !same {
                                carry.insert(u.invocation, u.macros_after.clone());
                            }
                        }
                        false
                    }
                }
            };
            // Compiled with its previous macro state (edited, or not compiled again): point it
            // at definitions that moved before it.
            if !recompiled && !moved.is_empty() {
                if let Some(nu) = Self::relocated(self.unit(uid), &moved, &mut replaced) {
                    self.units[uid as usize] = Some(Arc::new(nu));
                }
            }
            if recompiled || edited {
                // What this unit leaves behind that only moved.
                let (Some(o), Some(n)) = (old, self.try_unit(uid)) else { continue };
                for (name, od) in o.macros_after.iter() {
                    if let Some(nd) = n.macros_after.get(name) {
                        if !Arc::ptr_eq(od, nd) && od.body_text == nd.body_text && od.signature() == nd.signature() {
                            moved.insert(def_identity(od), nd.clone());
                        }
                    }
                }
            }
        }
    }

    /// A copy of `u` whose macro uses and tables refer to the new places of `moved`
    /// definitions; `None` when it refers to none of them.
    fn relocated(u: &Unit, moved: &FxHashMap<usize, Arc<MacroDef>>, tables: &mut FxHashMap<usize, Option<MacroTable>>) -> Option<Unit> {
        let mut table = |t: &MacroTable| tables.entry(t.identity()).or_insert_with(|| t.replaced(moved)).clone();
        let before = table(&u.macros_before);
        let after = table(&u.macros_after);
        let is_moved = |d: &Option<Arc<MacroDef>>| d.as_ref().is_some_and(|d| moved.contains_key(&def_identity(d)));
        let uses = u.macro_uses.iter().any(|m| is_moved(&m.def)) || u.macro_names.iter().any(|m| is_moved(&m.def));
        if before.is_none() && after.is_none() && !uses {
            return None;
        }
        let mut nu = u.clone();
        if let Some(t) = before {
            nu.macros_before = t;
        }
        if let Some(t) = after {
            nu.macros_after = t;
        }
        for m in &mut nu.macro_uses {
            if let Some(d) = m.def.as_ref().and_then(|d| moved.get(&def_identity(d))) {
                m.def = Some(d.clone());
            }
        }
        for m in &mut nu.macro_names {
            if let Some(d) = m.def.as_ref().and_then(|d| moved.get(&def_identity(d))) {
                m.def = Some(d.clone());
            }
        }
        Some(nu)
    }

    /// Keeps ad-hoc units in step with the files open in the editor: an open file that no
    /// project unit compiles (new, or its last includer dropped the `` `include ``) gets one,
    /// also when another open file includes it (so that the result does not depend on which
    /// was opened first), and an open file that a project unit now compiles loses its own.
    pub fn sync_open_files(&self, open: &[FileId]) -> Analysis {
        let mut next: Option<Analysis> = None;
        // Until nothing changes: a new ad-hoc unit can make a library unit unnecessary, whose
        // file, when open, then needs one of its own.
        for _ in 0..=open.len() {
            let mut changed = false;
            for &f in open {
                let cur = next.as_ref().unwrap_or(self);
                let in_project = cur.in_project_unit(f);
                let own = cur.is_adhoc_root(f);
                if !in_project && !own {
                    next = Some(cur.add_adhoc(f));
                    changed = true;
                } else if in_project && own {
                    next = Some(cur.remove_adhoc(f));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        next.unwrap_or_else(|| self.clone())
    }

    /// Whether a unit of the build compiles `file`.
    fn in_project_unit(&self, file: FileId) -> bool {
        self.units_of_file(file).iter().any(|u| self.try_unit(*u).is_some_and(|u| u.kind == UnitKind::Project))
    }

    fn pp_config_for(&self, u: &Unit) -> &PpConfig {
        self.setup.pp_configs.get(u.invocation).unwrap_or(&self.setup.pp_configs[0])
    }

    /// Compiles an editor file that is not part of the build, with the final macro state.
    pub fn add_adhoc(&self, file: FileId) -> Analysis {
        if self.in_project_unit(file) || self.is_adhoc_root(file) {
            return self.clone();
        }
        let mut next = self.clone();
        next.hidden_uses = Arc::default();
        next.instance_defs = Arc::default();
        let inv = self.setup.config.invocations.len().saturating_sub(1);
        let macros = self.last_macros(inv);
        let pp = preprocess(&*self.vfs, &self.setup.pp_configs[inv.min(self.setup.pp_configs.len() - 1)], file, macros.clone());
        let uid = next.units.len() as UnitId;
        next.units.push(Some(Arc::new(make_unit(&self.vfs, uid, file, UnitKind::AdHoc, inv, pp, macros))));
        next.reindex();
        next
    }

    /// Drops the ad-hoc unit of a closed file.
    pub fn remove_adhoc(&self, file: FileId) -> Analysis {
        let ids: Vec<UnitId> = self
            .units
            .iter()
            .flatten()
            .filter(|u| u.kind == UnitKind::AdHoc && u.root == file)
            .map(|u| u.id)
            .collect();
        if ids.is_empty() {
            return self.clone();
        }
        let mut next = self.clone();
        next.hidden_uses = Arc::default();
        next.instance_defs = Arc::default();
        for id in ids {
            next.units[id as usize] = None;
        }
        next.reindex();
        next
    }

    pub fn is_adhoc_root(&self, file: FileId) -> bool {
        self.units.iter().flatten().any(|u| u.kind == UnitKind::AdHoc && u.root == file)
    }

    pub fn project_files(&self) -> Vec<FileId> {
        let mut seen = FxHashSet::default();
        let mut out = Vec::new();
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
            }
            for f in &u.files {
                if seen.insert(*f) {
                    out.push(*f);
                }
            }
        }
        out
    }

    pub fn decl_count(&self) -> usize {
        self.units.iter().flatten().map(|u| u.parsed.decls.len()).sum()
    }

    /// Whether units recompiled since `old` changed anything other files can see: declarations
    /// outside subroutine bodies and blocks (names, kinds, types, bases, ports) or the macros
    /// they leave behind. When not, only the edited files' problems can have changed.
    pub fn interface_changed(&self, old: &Analysis) -> bool {
        let n = self.units.len().max(old.units.len());
        (0..n).any(|i| match (self.units.get(i).and_then(|u| u.as_ref()), old.units.get(i).and_then(|u| u.as_ref())) {
            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => false,
            // Only pointed at moved macro definitions: the same declarations.
            (Some(a), Some(b)) if Arc::ptr_eq(&a.parsed, &b.parsed) => !a.macros_after.same_definitions(&b.macros_after),
            (Some(a), Some(b)) => interface_fingerprint(a) != interface_fingerprint(b) || !a.macros_after.same_definitions(&b.macros_after),
            (None, None) => false,
            _ => true,
        })
    }

    /// Files of the units compiled again since `old` (not those only pointed at moved macro
    /// definitions), before and after: their problems may have changed even where their
    /// text did not, for example when a syntax error in a header continues into the file that
    /// includes it, or when a header is no longer included at all.
    pub fn recompiled_files(&self, old: &Analysis) -> Vec<FileId> {
        let mut seen = FxHashSet::default();
        let mut out = Vec::new();
        for u in self.units.iter().flatten() {
            let before = old.try_unit(u.id);
            if u.kind == UnitKind::Builtin || before.is_some_and(|o| Arc::ptr_eq(&o.parsed, &u.parsed)) {
                continue;
            }
            for f in u.files.iter().chain(before.map(|o| &o.files).into_iter().flatten()) {
                if seen.insert(*f) {
                    out.push(*f);
                }
            }
        }
        out
    }

    /// The text of a file as this snapshot compiled it; for files no unit read (build files,
    /// for example), the current text.
    pub fn source(&self, f: FileId) -> Option<Arc<Source>> {
        for uid in self.units_of_file(f) {
            if let Some(s) = self.try_unit(*uid).and_then(|u| u.sources.get(&f)) {
                return Some(s.clone());
            }
        }
        let (text, _) = self.vfs.text_versioned(f)?;
        let v = self.vfs.lsp_version_for(f, &text);
        Some(Arc::new(Source::new(text, v)))
    }

    pub fn file_lines(&self, f: FileId) -> Option<Arc<dvi_syntax::LineIndex>> {
        self.source(f).map(|s| s.lines())
    }

    /// Tokens of a file's snapshot text.
    pub fn file_tokens(&self, f: FileId) -> Option<Arc<Vec<dvi_syntax::Token>>> {
        self.source(f).map(|s| Arc::new(dvi_syntax::lexer::lex(&s.text, f)))
    }

    pub fn path(&self, f: FileId) -> PathBuf {
        self.vfs.path(f)
    }
}
