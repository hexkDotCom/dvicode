//! Project compilation and analysis snapshots.

use crate::vfs::{Vfs, BUILTIN_DIR};
use dvi_project::BuildConfig;
use dvi_syntax::ast::{DeclId, DeclKind, ParsedUnit, RefId, RefKind};
use dvi_syntax::preproc::{command_line_define, preprocess, IncludeInfo, MacroDef, MacroTable, MacroUse, PpConfig};
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

pub struct Unit {
    pub id: UnitId,
    pub root: FileId,
    pub kind: UnitKind,
    /// Index of the build invocation.
    pub invocation: usize,
    pub parsed: ParsedUnit,
    pub includes: Vec<IncludeInfo>,
    pub macro_uses: Vec<MacroUse>,
    pub macro_defs: Vec<Arc<MacroDef>>,
    pub files: Vec<FileId>,
    pub inactive: Vec<Span>,
    pub pp_diags: Vec<Diag>,
    pub macros_before: MacroTable,
    pub macros_after: MacroTable,
    pub positions: FxHashMap<FileId, FilePositions>,
    /// Name -> reference segments with that name.
    pub names: FxHashMap<SmolStr, Vec<(RefId, u16)>>,
    pub parse_ms: u128,
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
}

const PRELUDE: &str = include_str!("prelude.sv");

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
    let parsed = parser::parse(&pp.tokens, root);
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
    let _ = vfs;
    Unit {
        id,
        root,
        kind,
        invocation,
        parsed,
        includes: pp.includes,
        macro_uses: pp.macro_uses,
        macro_defs: pp.macro_defs,
        files: pp.files,
        inactive: pp.inactive,
        pp_diags: pp.diags,
        macros_before,
        macros_after: pp.macros_after,
        positions,
        names,
        parse_ms: t0.elapsed().as_millis(),
    }
}

impl Analysis {
    /// Compiles the whole project.
    pub fn build(vfs: Arc<Vfs>, setup: Arc<ProjectSetup>) -> Analysis {
        let t0 = Instant::now();
        let mut units: Vec<Option<Arc<Unit>>> = Vec::new();
        units.push(Some(Arc::new(Self::builtin_unit(&vfs))));
        // Preprocess in order (macros flow from file to file), parse in parallel.
        let mut jobs = Vec::new();
        for (inv_idx, inv) in setup.config.invocations.iter().enumerate() {
            let mut macros = setup.initial_macros[inv_idx].clone();
            let cfg = &setup.pp_configs[inv_idx];
            for f in &inv.files {
                let id = vfs.file_id(f);
                let before = macros.clone();
                let pp = preprocess(&*vfs, cfg, id, macros);
                macros = pp.macros_after.clone();
                jobs.push((id, inv_idx, pp, before));
            }
        }
        let base = units.len() as UnitId;
        let built: Vec<Unit> = jobs
            .into_par_iter()
            .enumerate()
            .map(|(i, (id, inv, pp, before))| make_unit(&vfs, base + i as UnitId, id, UnitKind::Project, inv, pp, before))
            .collect();
        units.extend(built.into_iter().map(|u| Some(Arc::new(u))));
        let mut a = Analysis {
            vfs,
            setup,
            units,
            global: Arc::new(GlobalIndex::default()),
            file_units: Arc::new(FxHashMap::default()),
            build_ms: 0,
        };
        a.reindex();
        a.add_library_units();
        a.build_ms = t0.elapsed().as_millis();
        a
    }

    fn builtin_unit(vfs: &Vfs) -> Unit {
        let path = PathBuf::from(BUILTIN_DIR).join("std.sv");
        let id = vfs.set_overlay(&path, Arc::from(PRELUDE));
        let pp = preprocess(vfs, &PpConfig::default(), id, MacroTable::new());
        make_unit(vfs, 0, id, UnitKind::Builtin, usize::MAX, pp, MacroTable::new())
    }

    /// Compiles `-y`/`-v` library files that provide otherwise missing design elements.
    fn add_library_units(&mut self) {
        let has_libs = self.setup.config.invocations.iter().any(|i| !i.lib_dirs.is_empty() || !i.lib_files.is_empty());
        if !has_libs {
            return;
        }
        for _round in 0..8 {
            let mut missing: FxHashSet<SmolStr> = FxHashSet::default();
            for u in self.units.iter().flatten() {
                for r in &u.parsed.refs {
                    if r.kind == RefKind::Definition {
                        let n = &r.segs[0].name;
                        if !self.global.definitions.contains_key(n) {
                            missing.insert(n.clone());
                        }
                    }
                }
            }
            if missing.is_empty() {
                return;
            }
            let mut new_files: Vec<(PathBuf, usize)> = Vec::new();
            let compiled: FxHashSet<FileId> = self.units.iter().flatten().map(|u| u.root).collect();
            for (inv_idx, inv) in self.setup.config.invocations.iter().enumerate() {
                let exts: Vec<String> = if inv.libext.is_empty() { vec![".v".into(), ".sv".into()] } else { inv.libext.clone() };
                for name in &missing {
                    'dirs: for d in &inv.lib_dirs {
                        for e in &exts {
                            let p = d.join(format!("{name}{e}"));
                            if p.is_file() {
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
                if compiled.contains(&id) || self.units.iter().flatten().any(|u| u.root == id) {
                    continue;
                }
                let macros = self.last_macros(inv_idx);
                let pp = preprocess(&*self.vfs, &self.setup.pp_configs[inv_idx], id, macros.clone());
                let uid = self.units.len() as UnitId;
                self.units.push(Some(Arc::new(make_unit(&self.vfs, uid, id, UnitKind::Project, inv_idx, pp, macros))));
                added = true;
            }
            if !added {
                return;
            }
            self.reindex();
        }
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
        // Prefer project units over ad-hoc ones for the same name.
        let units = &self.units;
        let rank = |r: &DeclRef| units[r.unit as usize].as_ref().map_or(3, |u| match u.kind {
            UnitKind::Project => 0,
            UnitKind::Builtin => 1,
            UnitKind::AdHoc => 2,
        });
        for v in g.packages.values_mut().chain(g.definitions.values_mut()).chain(g.unit_items.values_mut()) {
            v.sort_by_key(|r| (rank(r), r.unit));
        }
        self.global = Arc::new(g);
        self.file_units = Arc::new(fu);
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
        let rebuilt: Vec<Unit> = affected
            .par_iter()
            .map(|&uid| {
                let old = self.unit(uid);
                let cfg = self.pp_config_for(old);
                let pp = preprocess(&*self.vfs, cfg, old.root, old.macros_before.clone());
                make_unit(&self.vfs, uid, old.root, old.kind, old.invocation, pp, old.macros_before.clone())
            })
            .collect();
        for u in rebuilt {
            let id = u.id as usize;
            next.units[id] = Some(Arc::new(u));
        }
        next.reindex();
        next
    }

    fn pp_config_for(&self, u: &Unit) -> &PpConfig {
        self.setup.pp_configs.get(u.invocation).unwrap_or(&self.setup.pp_configs[0])
    }

    /// Compiles an editor file that is not part of the build, with the final macro state.
    pub fn add_adhoc(&self, file: FileId) -> Analysis {
        if !self.units_of_file(file).is_empty() {
            return self.clone();
        }
        let mut next = self.clone();
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

    pub fn path(&self, f: FileId) -> PathBuf {
        self.vfs.path(f)
    }
}
