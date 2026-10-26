//! Project compilation and analysis snapshots.

use crate::vfs::{Pin, Reader, Vfs, BUILTIN_DIR};
use dvi_project::{BuildConfig, Phase, Progress};
use dvi_syntax::ast::{DeclId, DeclKind, ParsedUnit, RefId, RefKind};
use dvi_syntax::preproc::{command_line_define, def_identity, preprocess, IncludeInfo, MacroDef, MacroName, MacroTable, MacroUse, PpConfig, PpResult, SameMemo};
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
    /// Scopes whose extent is in this file, named or not (`scope_at` looks among these only).
    pub scopes: Vec<DeclId>,
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
    /// The root file's path, kept here so that how near two units are (the number of leading
    /// path parts they share) is found without asking the file table.
    pub root_path: Arc<Path>,
}

/// Without a build file: the uses in each project file that were not checked because code that
/// may declare them is removed by `` `ifdef `` (the removed regions they point at), and their
/// number by region (file, start).
#[derive(Default)]
pub struct HiddenUses {
    pub per_file: FxHashMap<FileId, Arc<[Span]>>,
    pub counts: FxHashMap<(FileId, u32), usize>,
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
    /// Each invocation's fingerprint (`select::invocation_key`): what names it in a chosen
    /// compile context.
    pub inv_keys: Vec<String>,
}

impl ProjectSetup {
    pub fn new(root: &Path, config: BuildConfig, env: FxHashMap<String, String>) -> ProjectSetup {
        let mut pp_configs = Vec::new();
        let mut initial_macros = Vec::new();
        for inv in &config.invocations {
            pp_configs.push(PpConfig { incdirs: inv.incdirs.clone(), root_dir: root.to_path_buf() });
            let mut m = MacroTable::new();
            for (n, v) in &inv.defines {
                m.insert(Arc::new(command_line_define(n, v.as_deref())));
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
        let inv_keys = config.invocations.iter().map(crate::select::invocation_key).collect();
        ProjectSetup { root: root.to_path_buf(), config, pp_configs, initial_macros, env, inv_keys }
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
    /// The compile contexts chosen for files several units compile, resolved in this snapshot
    /// (set by the server, `Analysis::select_contexts`).
    pub selection: Arc<crate::select::Selection>,
    /// Without a build file: uses not checked because code that may declare them is removed by
    /// `` `ifdef ``. Counted for each snapshot when first needed (`Analysis::hidden_uses`).
    pub hidden_uses: Arc<std::sync::OnceLock<HiddenUses>>,
    /// Each file's part of `hidden_uses` taken over from the snapshot before, where an edit
    /// cannot have changed it (`Analysis::reuse_hidden_uses`): only the others are counted.
    pub hidden_seed: Option<Arc<FxHashMap<FileId, Arc<[Span]>>>>,
    /// Without a build file: the copy of a design element defined more than once that each
    /// instance means (`Analysis::instance_definitions`).
    pub instance_defs: Arc<std::sync::OnceLock<FxHashMap<(UnitId, Span), DeclRef>>>,
    /// Out-of-block method and constraint bodies (`function C::f()`) by name, for finding a
    /// prototype's implementation (`Analysis::implementation_of`).
    pub out_of_block: Arc<std::sync::OnceLock<FxHashMap<SmolStr, Vec<DeclRef>>>>,
    /// The `uvm_config_db` calls of the project (`Analysis::config_index`), and where the
    /// configured test places component classes (`Analysis::config_places`).
    pub config_calls: Arc<std::sync::OnceLock<Arc<crate::ide::configdb::ConfigIndex>>>,
    pub config_places_cache: Arc<std::sync::OnceLock<Arc<crate::ide::configdb::Places>>>,
    /// The test hierarchies factory override hovers read, by test (`Analysis::test_model`).
    pub override_models: Arc<std::sync::Mutex<FxHashMap<(u32, u32), Arc<crate::ide::override_impact::TestModel>>>>,
    /// The elaborated instances of the design (`Analysis::elab_index`), once made.
    pub elab_index: Arc<std::sync::Mutex<Option<Arc<crate::ide::elab::ElabIndex>>>>,
    /// The class each factory name registers (`class_by_factory_name`), as asked.
    pub factory_names: Arc<std::sync::Mutex<FxHashMap<SmolStr, Option<DeclRef>>>>,
    /// The `-y`/`-v` library units in the order they are compiled (after the build's own
    /// files): the order a full build and every update give them, whatever their ids.
    pub library_order: Arc<Vec<UnitId>>,
    /// Set by the server on the copy that answers a request: when the request is cancelled,
    /// searches over the whole project stop early (see `Analysis::cancelled`).
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Set by the server on the copy an update starts from: units compiled from it read the
    /// editor texts of that moment (see `Pin`), not newer ones.
    pub pin: Option<Arc<Pin>>,
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
    src: &Reader,
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
            // Most names come again: no copy of the name for those.
            match names.get_mut(&s.name) {
                Some(v) => v.push((rid as RefId, si as u16)),
                None => {
                    names.insert(s.name.clone(), vec![(rid as RefId, si as u16)]);
                }
            }
            if !s.from_macro {
                positions.entry(s.span.file).or_default().refs.push((s.span, rid as RefId, si as u16));
            }
        }
    }
    for (did, d) in parsed.decls.iter().enumerate() {
        if did != 0 && d.kind.is_scope() {
            positions.entry(d.span.file).or_default().scopes.push(did as DeclId);
        }
        if did == 0 || d.name.is_empty() || d.has(dvi_syntax::ast::dflags::FROM_MACRO) {
            continue;
        }
        positions.entry(d.name_span.file).or_default().decls.push(did as DeclId);
    }
    for p in positions.values_mut() {
        // Usually in order already (they are pushed as parsed): checked first.
        if !p.refs.is_sorted_by_key(|(s, _, _)| (s.start, s.end)) {
            p.refs.sort_by_key(|(s, _, _)| (s.start, s.end));
        }
        if !p.decls.is_sorted_by_key(|d| parsed.decls[*d as usize].name_span.start) {
            p.decls.sort_by_key(|d| parsed.decls[*d as usize].name_span.start);
        }
    }
    let sources = pp
        .sources
        .iter()
        .map(|(f, text)| (*f, Arc::new(Source::new(text.clone(), src.lsp_version_for(*f, text)))))
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
        root_path: Arc::from(src.path(root)),
    }
}

/// The macro definitions of one invocation that only moved during an update (see
/// `Analysis::propagate_macros`), and the macro tables already pointed at their new places.
#[derive(Default)]
struct Relocation {
    /// Identity of the old definition -> the new one (same name, same text).
    defs: FxHashMap<usize, Arc<MacroDef>>,
    /// Each change of `defs` in order: the identity and name of the old definition, and
    /// whether it was new in `defs`. Its length is the generation of `defs`.
    log: Vec<(usize, SmolStr, bool)>,
    /// By identity of the table: the table pointed at the moved definitions known when it was
    /// last asked for, brought up to date from there when it is asked for again.
    current: FxHashMap<usize, Relocated>,
    /// The table pointed at the moved definitions last: the next one is usually made from it
    /// with a few changes (the next unit's table, or the same unit's table before an edit).
    last: Option<Relocated>,
    /// The old and new tables after of the last unit whose moved definitions were looked for.
    seen: Option<(MacroTable, MacroTable)>,
}

/// A macro table pointed at the moved definitions.
#[derive(Clone)]
struct Relocated {
    /// `None` when the table holds none of them.
    table: Option<MacroTable>,
    /// How many of its definitions moved.
    count: usize,
    /// The generation of [`Relocation::defs`] it was made for.
    generation: usize,
    /// The table itself (it also keeps its identity, an address, its own while it is a key).
    original: MacroTable,
}

impl Relocation {
    /// `old` only moved, to `new`.
    fn add(&mut self, old: &Arc<MacroDef>, new: Arc<MacroDef>) {
        let id = def_identity(old);
        let first = match self.defs.get(&id) {
            Some(d) if Arc::ptr_eq(d, &new) => return,
            Some(_) => false,
            None => true,
        };
        self.defs.insert(id, new);
        self.log.push((id, old.name.clone(), first));
    }

    /// `r` pointed at the definitions that moved since it was made.
    fn caught_up(&self, r: &Relocated) -> Relocated {
        if r.generation == self.log.len() {
            return r.clone();
        }
        let (table, count) = r.original.relocated_more((r.table.as_ref(), r.count), &self.log[r.generation..], &self.defs);
        Relocated { table, count, generation: self.log.len(), original: r.original.clone() }
    }

    /// `t` pointed at the moved definitions known now (as from scratch): brought up to date
    /// from when `t` was last asked for, or made from the table pointed at them last, at the
    /// cost of their differences.
    fn current(&mut self, t: &MacroTable) -> Relocated {
        let r = if self.defs.is_empty() {
            Relocated { table: None, count: 0, generation: 0, original: t.clone() }
        } else if let Some(r) = self.current.get(&t.identity()) {
            self.caught_up(r)
        } else {
            match self.last.as_ref().map(|l| self.caught_up(l)) {
                Some(l) if l.original.identity() == t.identity() => l,
                Some(l) => {
                    let (table, count) = t.relocated_from(&l.original, l.table.as_ref(), l.count, &self.defs);
                    Relocated { table, count, generation: self.log.len(), original: t.clone() }
                }
                None => {
                    let (table, count) = t.relocated(&self.defs);
                    Relocated { table, count, generation: self.log.len(), original: t.clone() }
                }
            }
        };
        self.remember(&r);
        r
    }

    /// Records what a unit compiled again leaves behind that only moved: each definition of
    /// its old table after (`old`) that the new one (`new`) has elsewhere, with the same text.
    ///
    /// Only where the tables differ from what is known already. After the first unit of the
    /// invocation, every pair of definitions (old, new) of the last unit looked at that only
    /// moved is in `defs`, and only this method changes `defs`: so a name can tell something
    /// new only where this unit's old or new table differs from that unit's (for the files
    /// compiled again after a changed macro, the names each changes itself). For the first,
    /// where the new table has what the old one pointed at the moved definitions has (the old
    /// definition, or where it already moved), there is nothing new.
    fn discover(&mut self, old: &MacroTable, new: &MacroTable) {
        let add = |this: &mut Self, od: Option<&Arc<MacroDef>>, nd: Option<&Arc<MacroDef>>| {
            if let (Some(od), Some(nd)) = (od, nd) {
                if !Arc::ptr_eq(od, nd) && od.same_as(nd) {
                    this.add(od, nd.clone());
                }
            }
        };
        match self.seen.take() {
            Some((so, sn)) => {
                let mut names: Vec<SmolStr> = Vec::new();
                for (a, b) in [(&so, old), (&sn, new)] {
                    let _ = a.diff(b, |x, y| {
                        names.extend(x.or(y).map(|d| d.name.clone()));
                        std::ops::ControlFlow::Continue(())
                    });
                }
                for name in names {
                    add(self, old.get(&name), new.get(&name));
                }
            }
            _ => {
                let known = self.current(old);
                let known = known.table.unwrap_or_else(|| old.clone());
                let _ = known.diff(new, |_, nd| {
                    add(self, nd.and_then(|nd| old.get(&nd.name)), nd);
                    std::ops::ControlFlow::Continue(())
                });
            }
        }
        self.seen = Some((old.clone(), new.clone()));
    }

    /// `r` is the table pointed at the moved definitions known now.
    fn remember(&mut self, r: &Relocated) {
        if !self.defs.is_empty() {
            self.current.insert(r.original.identity(), r.clone());
        }
        self.last = Some(r.clone());
    }

    /// A copy of `u` whose macro uses and tables refer to the new places of the moved
    /// definitions; `None` when it refers to none of them.
    fn unit(&mut self, u: &Unit) -> Option<Unit> {
        let (mut macros_before, mut macros_after) = (u.macros_before.clone(), u.macros_after.clone());
        let tables_moved = self.tables(&mut macros_before, &mut macros_after);
        let is_moved = |d: &Option<Arc<MacroDef>>| d.as_ref().is_some_and(|d| self.defs.contains_key(&def_identity(d)));
        let uses = u.macro_uses.iter().any(|m| is_moved(&m.def)) || u.macro_names.iter().any(|m| is_moved(&m.def));
        if !tables_moved && !uses {
            return None;
        }
        let mut nu = u.clone();
        nu.macros_before = macros_before;
        nu.macros_after = macros_after;
        Analysis::relocate_uses(&mut nu.macro_uses, &mut nu.macro_names, &self.defs);
        Some(nu)
    }

    /// Points a unit's two macro tables at the new places of the definitions that moved
    /// before it (in compile order); whether either of them changed.
    ///
    /// Each table is what pointing it at those definitions from scratch gives, as a full
    /// build has it, even when a table shared with earlier units (a file that defines nothing
    /// passes its table on) was pointed at fewer: a file in between may have defined a macro
    /// again the same way. (Each table used to keep its first result.) But each is made from
    /// the table pointed at them last: the table after is the one before with the unit's own
    /// changes, and the next unit starts with it. So each costs about as much as those changes
    /// and shares the rest (copying every table in full made an edit above a header's
    /// definitions cost each later unit the whole table).
    fn tables(&mut self, macros_before: &mut MacroTable, macros_after: &mut MacroTable) -> bool {
        let before = self.current(macros_before);
        let after = self.current(macros_after);
        let mut changed = false;
        for (t, n) in [(macros_before, before.table), (macros_after, after.table)] {
            if let Some(n) = n {
                *t = n;
                changed = true;
            }
        }
        changed
    }
}

/// What library discovery reads of a unit: the design elements it defines at the top level, and
/// those it instantiates, in order of first use.
fn library_names(u: &Unit) -> (Vec<&SmolStr>, Vec<&SmolStr>) {
    let defined = u
        .parsed
        .decl(ParsedUnit::ROOT)
        .children
        .iter()
        .map(|c| u.parsed.decl(*c))
        .filter(|d| d.kind.is_definition() && !d.name.is_empty() && !d.has(dvi_syntax::ast::dflags::OUT_OF_BLOCK))
        .map(|d| &d.name)
        .collect();
    let mut seen = FxHashSet::default();
    let used = u.parsed.refs.iter().filter(|r| r.kind == RefKind::Definition).map(|r| &r.segs[0].name).filter(|n| seen.insert(*n)).collect();
    (defined, used)
}

/// Hash of what other files can see of a unit: its declarations (outside bodies), the connections
/// and binds that decide what instances are, and where its removed regions are in its scopes.
fn interface_fingerprint(u: &Unit, defined_twice: &dyn Fn(&str) -> bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    for d in &u.parsed.decls {
        // An unnamed block is no scope others reach.
        if d.kind == DeclKind::Block && d.name.is_empty() {
            continue;
        }
        // Skip anything inside a subroutine, unnamed block, constraint or covergroup body.
        let mut p = d.parent;
        let mut local = false;
        while let Some(x) = p {
            let pd = u.parsed.decl(x);
            // A named block outside subroutines is a scope other files reach (`top.blk.x`).
            if matches!(pd.kind, DeclKind::Function | DeclKind::Task | DeclKind::Constraint | DeclKind::Covergroup) || pd.kind == DeclKind::Block && pd.name.is_empty() {
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
    // The ports and parameters the instances of one statement connect by name, for design
    // elements defined twice: without a build file they pick the copy the instances are,
    // which other files reach through them (`Analysis::instance_definitions`).
    // A statement is the span of its type name, as the resolver groups them (and its copy is
    // that of all its instances). Hashed are its instances, each with the scopes it is in (a
    // header may be included in several), and the names they connect, not where it is; only
    // statements that connect names, the only ones whose copy is picked by them.
    type Group<'a> = (&'a str, std::collections::BTreeSet<(Vec<&'a str>, &'a str)>, std::collections::BTreeSet<&'a str>);
    let mut groups: std::collections::BTreeMap<Span, Group> = Default::default();
    let statement = |d: &dvi_syntax::ast::Decl| -> Option<(Span, &str)> {
        let rid = d.ty.as_ref().and_then(|t| t.ref_id)?;
        let seg = u.parsed.reference(rid).segs.first().filter(|s| !s.from_macro)?;
        defined_twice(&seg.name).then_some((seg.span, seg.name.as_str()))
    };
    for d in u.parsed.decls.iter().filter(|d| d.kind == DeclKind::Instance) {
        let Some((span, name)) = statement(d) else { continue };
        let mut scope = Vec::new();
        let mut p = d.parent;
        while let Some(x) = p {
            let pd = u.parsed.decl(x);
            scope.push(pd.name.as_str());
            p = pd.parent;
        }
        groups.entry(span).or_insert_with(|| (name, Default::default(), Default::default())).1.insert((scope, d.name.as_str()));
    }
    for r in &u.parsed.refs {
        if let RefKind::Port { inst } | RefKind::Param { inst } = r.kind {
            if let Some(g) = statement(u.parsed.decl(inst)).and_then(|(span, _)| groups.get_mut(&span)) {
                g.2.extend(r.segs.first().map(|s| s.name.as_str()));
            }
        }
    }
    for g in groups.values().filter(|g| !g.2.is_empty()) {
        g.hash(&mut h);
    }
    // `bind`: which design element gets which instance.
    for (target, inst) in &u.parsed.binds {
        let d = u.parsed.decl(*inst);
        (target.as_str(), d.name.as_str(), d.ty.as_ref().map(|t| t.text.as_str())).hash(&mut h);
    }
    // The scopes each region removed by `ifdef` is in (not its position): whether it may hide
    // members of a package or class that other files use.
    for s in &u.inactive {
        let mut cur = u.parsed.decl(ParsedUnit::ROOT);
        while let Some(c) = cur.children.iter().map(|c| u.parsed.decl(*c)).find(|c| c.span.file == s.file && c.span.start <= s.start && s.end <= c.span.end) {
            (c.name.as_str(), c.kind as u8).hash(&mut h);
            cur = c;
        }
        0xffu8.hash(&mut h);
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
                let u = make_unit(&vfs.reader(None), base + i as UnitId, id, UnitKind::Project, inv, pp, before);
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
            selection: Arc::default(),
            hidden_uses: Arc::default(),
            hidden_seed: None,
            instance_defs: Arc::default(),
            out_of_block: Arc::default(),
            config_calls: Arc::default(),
            config_places_cache: Arc::default(),
            override_models: Arc::default(),
            elab_index: Arc::default(),
            factory_names: Arc::default(),
            library_order: Arc::default(),
            cancel: None,
            pin: None,
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
        make_unit(&vfs.reader(None), 0, id, UnitKind::Builtin, usize::MAX, pp, MacroTable::new())
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
        // Library units in the order a full build adds them (and as a set, to ask).
        let mut kept: Vec<UnitId> = Vec::new();
        let mut kept_set: FxHashSet<UnitId> = FxHashSet::default();
        let mut changed = false;
        // The files the units in use start from; the old library units by their file; the
        // design elements the units in use refer to (each unit's references read once).
        let mut roots: FxHashSet<FileId> = self.units.iter().flatten().filter(|u| own(u)).map(|u| u.root).collect();
        let mut old_by_root: FxHashMap<FileId, Vec<UnitId>> = FxHashMap::default();
        for o in &old {
            old_by_root.entry(self.unit(*o).root).or_default().push(*o);
        }
        // In the order the references come (units in order): it is the order library files are
        // compiled in, and macros one defines can decide what the next one declares.
        let mut wanted: Vec<SmolStr> = Vec::new();
        let mut wanted_set: FxHashSet<SmolStr> = FxHashSet::default();
        let mut to_scan: Vec<UnitId> = self.units.iter().flatten().filter(|u| own(u)).map(|u| u.id).collect();
        // One round per level of the hierarchy found in libraries; it ends when a round adds
        // nothing (the bound only guards against a loop).
        for _round in 0..256 {
            if progress.is_some_and(|p| p.is_cancelled()) {
                return;
            }
            for uid in std::mem::take(&mut to_scan) {
                for r in &self.unit(uid).parsed.refs {
                    if r.kind == RefKind::Definition && wanted_set.insert(r.segs[0].name.clone()) {
                        wanted.push(r.segs[0].name.clone());
                    }
                }
            }
            let missing: Vec<&SmolStr> = wanted.iter().filter(|n| !defined.contains(*n)).collect();
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
                            // As the units are read: with the editor texts of the pin.
                            if self.reader().is_file(&p) {
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
                if roots.contains(&id) {
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
                // Only a unit whose id comes after those kept so far: ids stay in compile order
                // (macros, imports and the files' first context follow ids), as in a full build.
                let after = kept.last().copied();
                let reuse = old_by_root.get(&id).into_iter().flatten().copied().find(|o| {
                    let u = self.unit(*o);
                    !kept_set.contains(o) && after.is_none_or(|k| *o > k) && u.invocation == inv_idx && u.macros_before.same_definitions(&macros)
                });
                let uid = match reuse {
                    Some(o) => o,
                    None => {
                        let src = self.reader();
                        let pp = preprocess(&src, &self.setup.pp_configs[inv_idx], id, macros.clone());
                        let uid = self.units.len() as UnitId;
                        let mut u = make_unit(&src, uid, id, UnitKind::Project, inv_idx, pp, macros);
                        u.library = true;
                        self.units.push(Some(Arc::new(u)));
                        changed = true;
                        uid
                    }
                };
                kept.push(uid);
                kept_set.insert(uid);
                roots.insert(id);
                to_scan.push(uid);
                defined.extend(definitions(self.unit(uid)));
                added = true;
            }
            if !added {
                break;
            }
        }
        for o in old {
            if !kept_set.contains(&o) {
                self.units[o as usize] = None;
                changed = true;
            }
        }
        if *self.library_order != kept {
            self.library_order = Arc::new(kept);
            changed = true;
        }
        if changed {
            self.reindex();
            self.refresh_adhoc_macros();
        }
    }

    /// Compiles again the ad-hoc units that no longer start with the macros at the end of
    /// their invocation (the library units changed under them).
    fn refresh_adhoc_macros(&mut self) {
        let stale: Vec<(UnitId, FileId, usize)> = self
            .units
            .iter()
            .flatten()
            .filter(|u| u.kind == UnitKind::AdHoc && !u.macros_before.same_definitions(&self.last_macros(u.invocation)))
            .map(|u| (u.id, u.root, u.invocation))
            .collect();
        if stale.is_empty() {
            return;
        }
        for (uid, file, inv) in stale {
            let macros = self.last_macros(inv);
            let src = self.reader();
            let pp = preprocess(&src, &self.setup.pp_configs[inv.min(self.setup.pp_configs.len() - 1)], file, macros.clone());
            let u = make_unit(&src, uid, file, UnitKind::AdHoc, inv, pp, macros);
            self.units[uid as usize] = Some(Arc::new(u));
        }
        self.reindex();
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

    /// Macro table at the end of an invocation: after its last library unit compiled, if any,
    /// else after its own files.
    pub fn last_macros(&self, inv: usize) -> MacroTable {
        self.library_order
            .iter()
            .rev()
            .filter_map(|id| self.units.get(*id as usize).and_then(|u| u.as_deref()))
            .find(|u| u.invocation == inv)
            .map(|u| u.macros_after.clone())
            .unwrap_or_else(|| self.invocation_macros(inv))
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
            // The constants of an enum declared outside any scope are names of the compilation
            // unit too (`typedef enum {IDLE, RUN} st_e;` in a file of its own).
            for (i, d) in u.parsed.decls.iter().enumerate() {
                if d.kind != DeclKind::EnumMember || d.name.is_empty() {
                    continue;
                }
                let mut p = d.parent;
                while let Some(x) = p.filter(|x| matches!(u.parsed.decl(*x).kind, DeclKind::EnumType | DeclKind::Typedef)) {
                    p = u.parsed.decl(x).parent;
                }
                if p == Some(ParsedUnit::ROOT) && d.parent != Some(ParsedUnit::ROOT) {
                    g.unit_items.entry(d.name.clone()).or_default().push(DeclRef { unit: u.id, decl: i as DeclId });
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
        // Prefer project units, in compilation order, then library units, in theirs, over
        // ad-hoc ones for the same name. Ad-hoc units come in the order they were opened, which
        // depends on the editing session: order them by path.
        let units = &self.units;
        let lib_pos: FxHashMap<UnitId, u32> = self.library_order.iter().enumerate().map(|(i, id)| (*id, i as u32)).collect();
        let order: FxHashMap<UnitId, (u8, u32, PathBuf)> = units
            .iter()
            .flatten()
            .map(|u| {
                let key = match u.kind {
                    UnitKind::Project if u.library => (1, lib_pos.get(&u.id).copied().unwrap_or(u32::MAX), self.vfs.path(u.root)),
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
        self.out_of_block = Arc::default();
        self.config_calls = Arc::default();
        self.config_places_cache = Arc::default();
        self.override_models = Arc::default();
        self.elab_index = Arc::default();
        self.factory_names = Arc::default();
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

    /// The request this copy answers was cancelled. Searches that check it return what they
    /// found so far, which the server throws away; they must not be part of anything kept in
    /// the snapshot (such as the `OnceLock` caches), which other copies share.
    pub fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// What units compiled from this snapshot read.
    fn reader(&self) -> Reader<'_> {
        self.vfs.reader(self.pin.as_deref())
    }

    pub fn units_of_file(&self, f: FileId) -> &[UnitId] {
        self.file_units.get(&f).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Whether an edit of `from` may change the macros `to` is compiled with: a build unit
    /// that compiles `from` comes before one that compiles `to` in the same invocation (macros
    /// flow from file to file, see `propagate_macros`), or `to` is an ad-hoc file of that
    /// invocation (which sees its final macros). Library units start with the macros of their
    /// invocation's own files and follow one another in the order they were found, which
    /// their numbers do not tell (one kept from before can come after a new one): any other
    /// unit of the invocation may come before them.
    pub fn macros_flow(&self, from: FileId, to: FileId) -> bool {
        // The first and last unit of each invocation that compiles `from`.
        let mut span: FxHashMap<usize, (UnitId, UnitId)> = FxHashMap::default();
        for u in self.units_of_file(from).iter().filter_map(|id| self.try_unit(*id)) {
            if u.kind == UnitKind::Project {
                let e = span.entry(u.invocation).or_insert((u.id, u.id));
                *e = (e.0.min(u.id), e.1.max(u.id));
            }
        }
        !span.is_empty()
            && self.units_of_file(to).iter().filter_map(|id| self.try_unit(*id)).any(|u| {
                span.get(&u.invocation).is_some_and(|&(first, last)| match u.kind {
                    UnitKind::AdHoc => true,
                    UnitKind::Project if u.library => first != u.id || last != u.id,
                    UnitKind::Project => u.id > first,
                    UnitKind::Builtin => false,
                })
            })
    }

    /// Recompiles the units that contain any of `files`.
    pub fn update(&self, files: &[FileId]) -> Analysis {
        self.update_with(files, &Progress::new()).expect("an update that nothing cancels finishes")
    }

    /// [`Analysis::update`], stopped when `progress` is cancelled (`None`): a header that
    /// thousands of files include takes long to compile again, and a newer edit replaces it.
    pub fn update_with(&self, files: &[FileId], progress: &Progress) -> Option<Analysis> {
        let mut affected: Vec<UnitId> = files.iter().flat_map(|f| self.units_of_file(*f)).copied().collect();
        affected.sort_unstable();
        affected.dedup();
        if progress.is_cancelled() {
            return None;
        }
        // A file in a `-y` folder (or a `-v` file) that no build unit compiles may be the
        // library file a design element is now found in, saved or not; one that is compiled
        // but gone (an unsaved one closed, a deleted one) may leave it to another. One that is
        // compiled and still there is compared like any other unit below.
        let library_file = files.iter().any(|f| {
            self.in_library_folder(*f) && (!self.units_of_file(*f).iter().any(|u| self.unit(*u).kind != UnitKind::AdHoc) || !self.reader().is_file(&self.vfs.path(*f)))
        });
        if affected.is_empty() && !library_file {
            return Some(self.clone());
        }
        let mut next = self.clone();
        next.hidden_uses = Arc::default();
        next.hidden_seed = None;
        next.instance_defs = Arc::default();
        next.out_of_block = Arc::default();
        next.config_calls = Arc::default();
        next.config_places_cache = Arc::default();
        next.override_models = Arc::default();
        next.elab_index = Arc::default();
        next.factory_names = Arc::default();
        // Preprocess the edited units in parallel with the macros they started with: that is
        // their result unless a file before them now leaves different macros behind.
        let edited: Vec<Option<(UnitId, PpResult)>> = affected
            .par_iter()
            .map(|&uid| {
                if progress.is_cancelled() {
                    return None;
                }
                let old = self.unit(uid);
                Some((uid, self.preprocess_unit(old, old.macros_before.clone())))
            })
            .collect();
        if progress.is_cancelled() {
            return None;
        }
        let jobs = next.propagate_macros(self, edited.into_iter().flatten().collect(), progress)?;
        // Parse in parallel, as a full build does.
        let rebuilt: Vec<Option<Unit>> = jobs
            .into_par_iter()
            .map(|(uid, pp, macros_before)| {
                if progress.is_cancelled() {
                    return None;
                }
                let old = self.unit(uid);
                let mut u = make_unit(&self.reader(), uid, old.root, old.kind, old.invocation, pp, macros_before);
                u.library = old.library;
                Some(u)
            })
            .collect();
        if progress.is_cancelled() {
            return None;
        }
        for u in rebuilt.into_iter().flatten() {
            let id = u.id as usize;
            next.units[id] = Some(Arc::new(u));
        }
        next.reindex();
        // Which library files are compiled depends only on what the build's units define and
        // instantiate, and on the macros they leave behind: an edit inside a body changes none
        // of that, and looking for every missing design element in every library folder again
        // costs more than the edit itself. (New files in library folders start a build.)
        if library_file || !next.library_inputs_same(self) {
            next.update_library_units(Some(progress));
        }
        if progress.is_cancelled() {
            return None;
        }
        Some(next)
    }

    /// Whether `f` is in a `-y` folder or is a `-v` file of some invocation.
    fn in_library_folder(&self, f: FileId) -> bool {
        let p = self.vfs.path(f);
        let same = |a: &Path, b: &Path| a == b || cfg!(any(windows, target_os = "macos")) && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase();
        self.setup.config.invocations.iter().any(|inv| {
            inv.lib_files.iter().any(|l| same(l, &p)) || p.parent().is_some_and(|d| inv.lib_dirs.iter().any(|l| same(l, d)))
        })
    }

    /// Whether every unit compiled again since `old` defines and instantiates the same design
    /// elements (by name, in order) and leaves the same macros behind as before, and no unit
    /// came or went (ad-hoc units aside, which library files do not depend on).
    fn library_inputs_same(&self, old: &Analysis) -> bool {
        let n = self.units.len().max(old.units.len());
        let mut same = SameMemo::default();
        (0..n).all(|i| match (self.units.get(i).and_then(|u| u.as_ref()), old.units.get(i).and_then(|u| u.as_ref())) {
            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => true,
            (Some(a), Some(b)) if a.kind == UnitKind::AdHoc && b.kind == UnitKind::AdHoc => true,
            (Some(a), Some(b)) => {
                a.library == b.library
                    && a.invocation == b.invocation
                    && library_names(a) == library_names(b)
                    && a.macros_after.same_definitions_memo(&b.macros_after, &mut same)
            }
            (None, None) => true,
            (Some(a), None) | (None, Some(a)) => a.kind == UnitKind::AdHoc,
        })
    }

    /// Preprocesses `u` again, reading what this snapshot reads (the text pinned for it).
    fn preprocess_unit(&self, u: &Unit, macros_before: MacroTable) -> PpResult {
        preprocess(&self.reader(), self.pp_config_for(u), u.root, macros_before)
    }

    /// Decides, in compilation order, how each unit is compiled once the `edited` units were
    /// preprocessed again (with the macros they started with): when one of them now leaves
    /// different macros behind, the files compiled after it in the same invocation are
    /// preprocessed again with the new macros, until the macro state is back to what it was.
    /// Returns the units to parse again, with their preprocessing and starting macros; the
    /// others stay as they were.
    ///
    /// Definitions that only moved (a line added above a `` `define ``) expand the same way,
    /// so the files after them are not compiled again; but their macro uses and macro tables
    /// are pointed at the new definitions, or they would lead to the old location.
    ///
    /// `None` when `progress` is cancelled, leaving a half-updated snapshot.
    fn propagate_macros(
        &mut self,
        before: &Analysis,
        mut edited: FxHashMap<UnitId, PpResult>,
        progress: &Progress,
    ) -> Option<Vec<(UnitId, PpResult, MacroTable)>> {
        // Pending macro state per invocation, valid for the next unit of that invocation.
        let mut carry: FxHashMap<usize, MacroTable> = FxHashMap::default();
        // Moved definitions, and the tables pointed at them, per invocation: macros do not
        // carry over from one invocation to the next.
        let mut relocations: FxHashMap<usize, Relocation> = FxHashMap::default();
        // Old and new tables of consecutive units share most of their parts on each side.
        let mut same = SameMemo::default();
        let mut jobs = Vec::new();
        // In compile order: the build's files, its library files, then files outside it.
        let mut ids: Vec<(u8, UnitId)> = self
            .units
            .iter()
            .flatten()
            .map(|u| {
                let rank = match u.kind {
                    UnitKind::Builtin => 0,
                    UnitKind::Project if u.library => 2,
                    UnitKind::Project => 1,
                    UnitKind::AdHoc => 3,
                };
                (rank, u.id)
            })
            .collect();
        ids.sort_unstable();
        for (_, uid) in ids {
            if progress.is_cancelled() {
                return None;
            }
            let u = self.unit(uid).clone();
            let old = before.try_unit(uid);
            let pre = edited.remove(&uid);
            let is_edited = pre.is_some();
            // The unit's new preprocessing and starting macros (`None`: it stays as it was);
            // `recompiled` when it starts with other macros than before.
            let (mut job, recompiled) = match u.kind {
                UnitKind::Builtin => {
                    if let Some(pp) = pre {
                        jobs.push((uid, pp, u.macros_before.clone()));
                    }
                    continue;
                }
                // Ad-hoc files see the final state of their invocation.
                UnitKind::AdHoc => match carry.get(&u.invocation) {
                    Some(m) => (Some((self.preprocess_unit(&u, m.clone()), m.clone())), true),
                    None => (pre.map(|pp| (pp, u.macros_before.clone())), false),
                },
                UnitKind::Project => {
                    let old_after = old.map(|o| o.macros_after.clone());
                    if let Some(m) = carry.get(&u.invocation).cloned() {
                        // A file before this one changed the macros: preprocess again with them.
                        let pp = self.preprocess_unit(&u, m.clone());
                        if old_after.as_ref().is_some_and(|o| pp.macros_after.same_definitions_memo(o, &mut same)) {
                            carry.remove(&u.invocation);
                        } else {
                            carry.insert(u.invocation, pp.macros_after.clone());
                        }
                        (Some((pp, m)), true)
                    } else {
                        if let Some(pp) = &pre {
                            if !old_after.as_ref().is_some_and(|o| pp.macros_after.same_definitions_memo(o, &mut same)) {
                                carry.insert(u.invocation, pp.macros_after.clone());
                            }
                        }
                        (pre.map(|pp| (pp, u.macros_before.clone())), false)
                    }
                }
            };
            let moved = relocations.entry(u.invocation).or_default();
            // Compiled with its previous macro state (edited, or not compiled again): point it
            // at definitions that moved before it.
            if !recompiled && !moved.defs.is_empty() {
                match &mut job {
                    Some((pp, macros_before)) => {
                        moved.tables(macros_before, &mut pp.macros_after);
                        Self::relocate_uses(&mut pp.macro_uses, &mut pp.macro_names, &moved.defs);
                    }
                    None => {
                        if let Some(nu) = moved.unit(&u) {
                            self.units[uid as usize] = Some(Arc::new(nu));
                        }
                    }
                }
                // The files after an edited one that leaves other macros behind start from its
                // table pointed at the moved definitions too, as a full build has it.
                if let (Some((pp, _)), Some(c)) = (&job, carry.get_mut(&u.invocation)) {
                    if is_edited && u.kind == UnitKind::Project {
                        *c = pp.macros_after.clone();
                    }
                }
            }
            // Ad-hoc units each start from the end of their invocation: what one of them moves,
            // no other unit sees.
            if (recompiled || is_edited) && u.kind != UnitKind::AdHoc {
                if let (Some(o), Some((pp, _))) = (old, &job) {
                    moved.discover(&o.macros_after, &pp.macros_after);
                }
            }
            if let Some((pp, macros_before)) = job {
                jobs.push((uid, pp, macros_before));
            }
        }
        Some(jobs)
    }

    /// Points macro uses and macro names at the new places of `moved` definitions.
    fn relocate_uses(uses: &mut [MacroUse], names: &mut [MacroName], moved: &FxHashMap<usize, Arc<MacroDef>>) {
        let new = |d: &Option<Arc<MacroDef>>| d.as_ref().and_then(|d| moved.get(&def_identity(d))).cloned();
        for m in uses {
            if let Some(d) = new(&m.def) {
                m.def = Some(d);
            }
        }
        for m in names {
            if let Some(d) = new(&m.def) {
                m.def = Some(d);
            }
        }
    }

    /// Keeps ad-hoc units in step with the files open in the editor: an open file that no
    /// project unit compiles (new, or its last includer dropped the `` `include ``) gets one,
    /// also when another open file includes it (so that the result does not depend on which
    /// was opened first), and an open file that a project unit now compiles loses its own.
    pub fn sync_open_files(&self, open: &[FileId]) -> Analysis {
        self.sync_open_files_with(open, &Progress::new()).expect("nothing cancels it")
    }

    /// [`Analysis::sync_open_files`], stopped when `progress` is cancelled (`None`).
    pub fn sync_open_files_with(&self, open: &[FileId], progress: &Progress) -> Option<Analysis> {
        let mut next: Option<Analysis> = None;
        // Until nothing changes: a new ad-hoc unit can make a library unit unnecessary, whose
        // file, when open, then needs one of its own.
        for _ in 0..=open.len() {
            let mut changed = false;
            for &f in open {
                if progress.is_cancelled() {
                    return None;
                }
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
        Some(next.unwrap_or_else(|| self.clone()))
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
        next.hidden_seed = None;
        next.instance_defs = Arc::default();
        next.out_of_block = Arc::default();
        next.config_calls = Arc::default();
        next.config_places_cache = Arc::default();
        next.override_models = Arc::default();
        next.elab_index = Arc::default();
        next.factory_names = Arc::default();
        let inv = self.setup.config.invocations.len().saturating_sub(1);
        let macros = self.last_macros(inv);
        let src = self.reader();
        let pp = preprocess(&src, &self.setup.pp_configs[inv.min(self.setup.pp_configs.len() - 1)], file, macros.clone());
        let uid = next.units.len() as UnitId;
        next.units.push(Some(Arc::new(make_unit(&src, uid, file, UnitKind::AdHoc, inv, pp, macros))));
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
        next.hidden_seed = None;
        next.instance_defs = Arc::default();
        next.out_of_block = Arc::default();
        next.config_calls = Arc::default();
        next.config_places_cache = Arc::default();
        next.override_models = Arc::default();
        next.elab_index = Arc::default();
        next.factory_names = Arc::default();
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
        // The units' old and new tables share most of their parts on each side.
        let mut same = SameMemo::default();
        (0..n).any(|i| match (self.units.get(i).and_then(|u| u.as_ref()), old.units.get(i).and_then(|u| u.as_ref())) {
            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => false,
            // Only pointed at moved macro definitions: the same declarations.
            (Some(a), Some(b)) if Arc::ptr_eq(&a.parsed, &b.parsed) => !a.macros_after.same_definitions_memo(&b.macros_after, &mut same),
            (Some(a), Some(b)) => {
                // Connections pick a copy only without a build file, among copies.
                let twice = |n: &str| {
                    (self.nearest_wins_unit(a) || old.nearest_wins_unit(b))
                        && (self.global.definitions.get(n).is_some_and(|v| v.len() > 1) || old.global.definitions.get(n).is_some_and(|v| v.len() > 1))
                };
                interface_fingerprint(a, &twice) != interface_fingerprint(b, &twice) || !a.macros_after.same_definitions_memo(&b.macros_after, &mut same)
            }
            (None, None) => false,
            _ => true,
        })
    }

    /// After an update from `old` that changed nothing other files can see
    /// ([`Analysis::interface_changed`] is false): the skipped-code uses of the files whose units
    /// were not compiled again are those `old` counted, if it did. The uses point at removed
    /// regions by position, so the regions of the units compiled again must not have moved.
    pub fn reuse_hidden_uses(&mut self, old: &Analysis) {
        let Some(prev) = old.hidden_uses.get() else { return };
        for u in self.units.iter().flatten() {
            if old.try_unit(u.id).is_some_and(|o| !Arc::ptr_eq(&o.parsed, &u.parsed) && o.inactive != u.inactive) {
                return;
            }
        }
        let recompiled: FxHashSet<FileId> = self.recompiled_files(old).into_iter().collect();
        let seed = prev.per_file.iter().filter(|(f, _)| !recompiled.contains(*f)).map(|(f, v)| (*f, v.clone())).collect();
        self.hidden_uses = Arc::default();
        self.hidden_seed = Some(Arc::new(seed));
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
        // The chosen unit's copy: positions it gives are converted with the text it read.
        let chosen = self.chosen_unit(f);
        for uid in chosen.iter().chain(self.units_of_file(f)) {
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
    /// The tokens of the snapshot's text of `f`: those the file system layer keeps for that
    /// text, else lexed now.
    pub fn file_tokens(&self, f: FileId) -> Option<Arc<Vec<dvi_syntax::Token>>> {
        let s = self.source(f)?;
        if let Some((t, toks)) = SourceProvider::source(&*self.vfs, f) {
            if Arc::ptr_eq(&t, &s.text) {
                return Some(toks);
            }
        }
        Some(Arc::new(dvi_syntax::lexer::lex(&s.text, f)))
    }

    pub fn path(&self, f: FileId) -> PathBuf {
        self.vfs.path(f)
    }
}
