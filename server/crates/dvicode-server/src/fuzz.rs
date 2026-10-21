//! `dvicode-server check DIR --fuzz-edits N`: differential testing of incremental updates.
//!
//! Makes random but realistic edits (insert and delete lines, rename an identifier in a file,
//! add or remove `` `define ``, `` `include `` and `import`, add class members, break the
//! syntax and fix it again, type a statement in pieces, close a file, edit a header that many
//! files include) and analyses them the way the server does: editor contents in the file store,
//! then [`Analysis::update`] of the changed files. After every edit the result is compared with
//! a full build of the same texts: the problems of every file, the outline of the edited files,
//! and go to definition, hover and references at sampled positions in the edited files and in
//! files that use names from the edited lines. The problems the editor shows are modelled
//! too: after an update that leaves the interface of the changed units alone, the server
//! publishes only the changed and open files, so the others must still be right.
//!
//! Any difference is a bug. It is reduced to a short reproducer: a starting state (editor
//! contents) and the fewest last edits that still show it.

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use dvi_syntax::{lexer, FileId, SourceProvider, TokKind};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Options {
    /// Number of edits (a step may make several, like edits within one debounce).
    pub edits: usize,
    pub seed: u64,
    /// Positions sampled per checked file for go to definition and hover.
    pub samples: usize,
    /// Stop after this many findings.
    pub max_findings: usize,
    /// Also change files on disk (reported like watched-file changes). Writes the project's
    /// files: only for a copy of a project.
    pub disk: bool,
    /// Print every edit.
    pub verbose: bool,
    /// Directory to save each reproducer in (JSON, see `replay_file`).
    pub save: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Options {
        Options { edits: 100, seed: 1, samples: 6, max_findings: 5, disk: false, verbose: false, save: None }
    }
}

pub struct Finding {
    pub step: usize,
    pub differences: Vec<String>,
    pub reproducer: String,
}

#[derive(Default)]
pub struct Report {
    pub steps: usize,
    pub edits: usize,
    pub findings: Vec<Finding>,
    /// Edits made, by kind.
    pub kinds: BTreeMap<&'static str, usize>,
}

// ------------------------------------------------------------------------------ random

/// SplitMix64: small, fast, and the same everywhere for a seed.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    pub(crate) fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    pub(crate) fn pick<'a, T>(&mut self, v: &'a [T]) -> Option<&'a T> {
        if v.is_empty() {
            None
        } else {
            Some(&v[self.below(v.len())])
        }
    }
}

// ------------------------------------------------------------------------------ edits

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Channel {
    /// `didOpen`/`didChange`: new editor contents.
    Editor,
    /// Written to disk and reported as a watched-file change (the file is not open).
    Disk,
    /// `didClose`: back to the disk contents.
    Close,
}

#[derive(Clone)]
struct Change {
    path: PathBuf,
    kind: &'static str,
    before: Arc<str>,
    after: Arc<str>,
    channel: Channel,
}

/// Problem as compared: position, severity, code and message.
type DiagKey = (u32, u32, u32, u32, u8, &'static str, String);

fn diag_keys(a: &Analysis, f: FileId) -> Vec<DiagKey> {
    let mut v: Vec<DiagKey> = a
        .file_diagnostics(f)
        .into_iter()
        .map(|d| {
            let r = a.to_range(d.span).unwrap_or_default();
            (r.start.line, r.start.character, r.end.line, r.end.character, d.severity as u8, d.code, d.message)
        })
        .collect();
    v.sort();
    v
}

/// Problems of every project file and open file, by path; files without problems left out.
fn all_problems(a: &Analysis, open: &BTreeMap<PathBuf, Arc<str>>) -> BTreeMap<PathBuf, Vec<DiagKey>> {
    let mut files = a.project_files();
    for p in open.keys() {
        if let Some(f) = a.vfs.lookup(p) {
            if !files.contains(&f) {
                files.push(f);
            }
        }
    }
    let mut out = BTreeMap::new();
    for f in files {
        if a.vfs.is_builtin(f) {
            continue;
        }
        let d = diag_keys(a, f);
        if !d.is_empty() {
            out.insert(a.vfs.path(f), d);
        }
    }
    out
}

/// The incremental side, as the server keeps it, and a store for full builds of the same texts.
struct Incr {
    setup: Arc<ProjectSetup>,
    vfs: Arc<Vfs>,
    a: Analysis,
    /// Editor contents of the open documents.
    open: BTreeMap<PathBuf, Arc<str>>,
    /// The problems the editor shows.
    shown: BTreeMap<PathBuf, Vec<DiagKey>>,
    /// Store for the full builds, kept in step with `vfs`.
    full_vfs: Arc<Vfs>,
}

impl Incr {
    /// A full build with `open` as editor contents, as after the server's first build.
    fn start(setup: Arc<ProjectSetup>, open: &BTreeMap<PathBuf, Arc<str>>) -> Incr {
        let vfs = Arc::new(Vfs::new());
        let full_vfs = Arc::new(Vfs::new());
        for (p, t) in open {
            vfs.set_overlay(p, t.clone());
            full_vfs.set_overlay(p, t.clone());
        }
        let a = Self::build(&vfs, &setup, open);
        let shown = all_problems(&a, open);
        Incr { setup, vfs, a, open: open.clone(), shown, full_vfs }
    }

    /// A full build plus the open files that are not part of it, like `Server::on_built`.
    fn build(vfs: &Arc<Vfs>, setup: &Arc<ProjectSetup>, open: &BTreeMap<PathBuf, Arc<str>>) -> Analysis {
        Analysis::build(vfs.clone(), setup.clone()).sync_open_files(&Self::open_ids(vfs, open))
    }

    /// The open documents in the server's order.
    fn open_ids(vfs: &Vfs, open: &BTreeMap<PathBuf, Arc<str>>) -> Vec<FileId> {
        let mut v: Vec<FileId> = open.keys().map(|p| vfs.file_id(p)).collect();
        v.sort_unstable();
        v
    }

    /// Applies one batch of changes like the server's notification handlers and debounce
    /// (`flush_pending`). With `disk` false, disk changes are made as editor contents.
    fn apply(&mut self, changes: &[Change], disk: bool) {
        let mut files: Vec<FileId> = Vec::new();
        for c in changes {
            match c.channel {
                Channel::Disk if disk => {
                    std::fs::write(&c.path, c.after.as_bytes()).expect("write a fuzzed file");
                    let id = self.vfs.invalidate(&c.path);
                    self.full_vfs.invalidate(&c.path);
                    if let Some(id) = id {
                        if !self.vfs.has_overlay(id) && !self.a.units_of_file(id).is_empty() && !files.contains(&id) {
                            files.push(id);
                        }
                    }
                }
                Channel::Editor | Channel::Disk => {
                    let id = self.vfs.set_overlay(&c.path, c.after.clone());
                    self.full_vfs.set_overlay(&c.path, c.after.clone());
                    self.open.insert(c.path.clone(), c.after.clone());
                    if !files.contains(&id) {
                        files.push(id);
                    }
                }
                Channel::Close => {
                    self.vfs.clear_overlay(&c.path);
                    self.full_vfs.clear_overlay(&c.path);
                    self.open.remove(&c.path);
                    let Some(id) = self.vfs.lookup(&c.path) else { continue };
                    if self.a.is_adhoc_root(id) {
                        self.a = self.a.remove_adhoc(id);
                        self.shown = all_problems(&self.a, &self.open);
                    }
                    if !files.contains(&id) {
                        files.push(id);
                    }
                }
            }
        }
        if files.is_empty() {
            return;
        }
        let old = self.a.clone();
        let next = old.update(&files).sync_open_files(&Self::open_ids(&self.vfs, &self.open));
        let interface_changed = next.interface_changed(&old);
        let recompiled = next.recompiled_files(&old);
        self.a = next;
        if interface_changed {
            self.shown = all_problems(&self.a, &self.open);
            return;
        }
        let mut targets: FxHashSet<FileId> = files.iter().copied().collect();
        targets.extend(recompiled);
        for p in self.open.keys() {
            targets.insert(self.vfs.file_id(p));
        }
        // As the server does: notices that count uses in every file.
        targets.extend(self.a.files_with_skipped_code());
        for f in targets {
            if self.vfs.is_builtin(f) {
                continue;
            }
            let d = diag_keys(&self.a, f);
            let p = self.vfs.path(f);
            if d.is_empty() {
                self.shown.remove(&p);
            } else {
                self.shown.insert(p, d);
            }
        }
    }

    fn full(&self) -> Analysis {
        Self::build(&self.full_vfs, &self.setup, &self.open)
    }
}

// ------------------------------------------------------------------------------ comparison

struct Probe<'a> {
    root: &'a Path,
    /// Files whose outline and positions are checked.
    focus: Vec<PathBuf>,
    /// Names from the edited lines: their uses in other files are checked too.
    names: Vec<String>,
    seed: u64,
    samples: usize,
}

pub(crate) fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string().replace('\\', "/")
}

fn show_diag(d: &DiagKey) -> String {
    format!("{}:{} {} {}", d.0 + 1, d.1 + 1, d.5, d.6)
}

fn locations(root: &Path, v: Vec<lsp_types::Location>) -> Vec<String> {
    v.iter()
        .map(|l| {
            let p = l.uri.to_file_path().map(|p| rel(root, &p)).unwrap_or_else(|_| l.uri.to_string());
            format!("{p}:{}:{}-{}:{}", l.range.start.line + 1, l.range.start.character + 1, l.range.end.line + 1, l.range.end.character + 1)
        })
        .collect()
}

pub(crate) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Offsets of whole-word occurrences of `name` in `text`.
pub(crate) fn word_offsets(text: &str, name: &str, limit: usize) -> Vec<u32> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let s = from + i;
        let e = s + name.len();
        if (s == 0 || !is_ident_byte(b[s - 1])) && (e >= b.len() || !is_ident_byte(b[e])) {
            out.push(s as u32);
            if out.len() >= limit {
                break;
            }
        }
        from = e;
    }
    out
}

/// Differences between the incremental analysis (and the problems shown from it) and a full
/// build of the same texts.
fn differences(inc: &Incr, full: &Analysis, probe: &Probe) -> Vec<String> {
    let root = probe.root;
    let a = &inc.a;
    let mut out = Vec::new();
    let pa = all_problems(a, &inc.open);
    let pf = all_problems(full, &inc.open);
    let mut paths: Vec<&PathBuf> = pa.keys().chain(pf.keys()).collect();
    paths.sort();
    paths.dedup();
    let none = Vec::new();
    for p in &paths {
        let (x, y) = (pa.get(*p).unwrap_or(&none), pf.get(*p).unwrap_or(&none));
        if x != y {
            let only_a: Vec<String> = x.iter().filter(|d| !y.contains(d)).take(4).map(show_diag).collect();
            let only_f: Vec<String> = y.iter().filter(|d| !x.contains(d)).take(4).map(show_diag).collect();
            out.push(format!("problems of {}: incremental only [{}], full build only [{}]", rel(root, p), only_a.join("; "), only_f.join("; ")));
        }
    }
    let mut files_a: Vec<PathBuf> = a.project_files().into_iter().map(|f| a.vfs.path(f)).collect();
    let mut files_f: Vec<PathBuf> = full.project_files().into_iter().map(|f| full.vfs.path(f)).collect();
    files_a.sort();
    files_f.sort();
    if files_a != files_f {
        let extra: Vec<String> = files_a.iter().filter(|p| !files_f.contains(p)).take(4).map(|p| rel(root, p)).collect();
        let missing: Vec<String> = files_f.iter().filter(|p| !files_a.contains(p)).take(4).map(|p| rel(root, p)).collect();
        out.push(format!("compiled files: incremental only [{}], full build only [{}]", extra.join(", "), missing.join(", ")));
    }
    // Positions to query.
    let mut rng = Rng(probe.seed);
    let mut positions: Vec<(PathBuf, u32)> = Vec::new();
    for p in &probe.focus {
        let (Some(fa), Some(ff)) = (a.vfs.lookup(p), full.vfs.lookup(p)) else { continue };
        let sa = serde_json::to_string(&a.document_symbols(fa)).unwrap_or_default();
        let sf = serde_json::to_string(&full.document_symbols(ff)).unwrap_or_default();
        if sa != sf {
            out.push(format!("outline of {} differs", rel(root, p)));
        }
        let Some(text) = a.file_text(fa) else { continue };
        let toks: Vec<u32> = lexer::lex(&text, 0)
            .iter()
            .filter(|t| matches!(t.kind, TokKind::Ident | TokKind::Directive))
            .map(|t| t.span.start + t.span.len() / 2)
            .collect();
        for _ in 0..probe.samples.min(toks.len()) {
            positions.push((p.clone(), toks[rng.below(toks.len())]));
        }
    }
    if !probe.names.is_empty() {
        let mut uses: Vec<(PathBuf, u32)> = Vec::new();
        for f in a.project_files() {
            if a.vfs.is_builtin(f) {
                continue;
            }
            let p = a.vfs.path(f);
            if probe.focus.contains(&p) {
                continue;
            }
            let Some(text) = a.file_text(f) else { continue };
            for n in &probe.names {
                for off in word_offsets(&text, n, 8) {
                    uses.push((p.clone(), off));
                }
            }
            if uses.len() > 400 {
                break;
            }
        }
        for _ in 0..probe.samples.min(uses.len()) {
            positions.push(uses[rng.below(uses.len())].clone());
        }
    }
    for (i, (p, off)) in positions.iter().enumerate() {
        let (Some(fa), Some(ff)) = (a.vfs.lookup(p), full.vfs.lookup(p)) else { continue };
        let at = || {
            let r = a.to_range(dvi_syntax::Span::new(fa, *off, *off)).unwrap_or_default();
            format!("{}:{}:{}", rel(root, p), r.start.line + 1, r.start.character + 1)
        };
        let ga = locations(root, a.goto_definition(fa, *off));
        let gf = locations(root, full.goto_definition(ff, *off));
        if ga != gf {
            out.push(format!("definition at {}: incremental {:?}, full build {:?}", at(), ga, gf));
        }
        let ha = a.hover(fa, *off).map(|(h, s)| (h, a.to_range(s)));
        let hf = full.hover(ff, *off).map(|(h, s)| (h, full.to_range(s)));
        if ha != hf {
            out.push(format!("hover at {}: incremental {:?}, full build {:?}", at(), ha.map(|h| h.0), hf.map(|h| h.0)));
        }
        if i % 4 == 0 {
            let mut ra = locations(root, a.references(fa, *off, true));
            let mut rf = locations(root, full.references(ff, *off, true));
            ra.sort();
            rf.sort();
            if ra != rf {
                let only_a: Vec<&String> = ra.iter().filter(|x| !rf.contains(x)).take(4).collect();
                let only_f: Vec<&String> = rf.iter().filter(|x| !ra.contains(x)).take(4).collect();
                out.push(format!("references at {}: incremental only {:?}, full build only {:?}", at(), only_a, only_f));
            }
        }
    }
    // What the editor shows, when the analysis itself is right.
    if out.is_empty() {
        let mut paths: Vec<&PathBuf> = inc.shown.keys().chain(pf.keys()).collect();
        paths.sort();
        paths.dedup();
        for p in paths {
            let (x, y) = (inc.shown.get(p).unwrap_or(&none), pf.get(p).unwrap_or(&none));
            if x != y {
                let only_s: Vec<String> = x.iter().filter(|d| !y.contains(d)).take(4).map(show_diag).collect();
                let only_f: Vec<String> = y.iter().filter(|d| !x.contains(d)).take(4).map(show_diag).collect();
                out.push(format!("problems shown for {} are stale: shown only [{}], full build only [{}]", rel(root, p), only_s.join("; "), only_f.join("; ")));
            }
        }
    }
    out
}

/// Panics become differences, so that a crash in an update is a finding like any other.
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|p| {
        let m = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
        format!("internal error in {what}: {m}")
    })
}

/// Applies `steps` incrementally on top of a full build of `base`, then compares.
fn replay(setup: &Arc<ProjectSetup>, base: &BTreeMap<PathBuf, Arc<str>>, steps: &[Vec<Change>], probe: &Probe) -> Vec<String> {
    let r = guarded("replay", || {
        let mut inc = Incr::start(setup.clone(), base);
        for s in steps {
            inc.apply(s, false);
        }
        let full = inc.full();
        differences(&inc, &full, probe)
    });
    r.unwrap_or_else(|e| vec![e])
}

// ------------------------------------------------------------------------------ text edits

/// Lines without their terminators, and how to join them again.
struct Lines {
    v: Vec<String>,
    nl: &'static str,
    trailing: bool,
}

impl Lines {
    fn of(t: &str) -> Lines {
        let nl = if t.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing = t.ends_with('\n');
        let body = t.strip_suffix('\n').map(|s| s.strip_suffix('\r').unwrap_or(s)).unwrap_or(t);
        let v = if t.is_empty() { Vec::new() } else { body.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect() };
        Lines { v, nl, trailing }
    }

    fn text(&self) -> String {
        let mut s = self.v.join(self.nl);
        if self.trailing && !self.v.is_empty() {
            s.push_str(self.nl);
        }
        s
    }
}

/// What edits can use: names that exist in the project.
#[derive(Default)]
struct Vocabulary {
    packages: Vec<String>,
    macros: Vec<String>,
    includes: Vec<String>,
    classes: Vec<String>,
}

const END_KEYWORDS: &[&str] = &["end", "endclass", "endfunction", "endtask", "endmodule", "endpackage", "endinterface", "begin", "endgenerate", "endcase"];

/// The line after the header (`...;`) of a random element that starts with one of `keywords`.
fn after_header(text: &str, keywords: &[&str], rng: &mut Rng) -> Option<usize> {
    let toks = lexer::lex(text, 0);
    let starts: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| t.kind == TokKind::Keyword && keywords.contains(&t.text.as_str())).map(|(i, _)| i).collect();
    let &i = rng.pick(&starts)?;
    let mut depth = 0i32;
    for t in &toks[i..] {
        if t.is_punct("(") || t.is_punct("[") || t.is_punct("{") {
            depth += 1;
        } else if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
            depth -= 1;
        } else if depth == 0 && t.is_punct(";") {
            return Some(text[..t.span.end as usize].matches('\n').count() + 1);
        }
    }
    None
}

struct Gen<'a> {
    rng: &'a mut Rng,
    voc: &'a Vocabulary,
    n: &'a mut u64,
}

impl Gen<'_> {
    fn id(&mut self) -> u64 {
        *self.n += 1;
        *self.n
    }

    fn package(&mut self) -> String {
        self.rng.pick(&self.voc.packages).cloned().unwrap_or_else(|| "fuzz_missing_pkg".into())
    }

    fn class(&mut self) -> String {
        self.rng.pick(&self.voc.classes).cloned().unwrap_or_else(|| "int".into())
    }

    fn mac(&mut self) -> String {
        self.rng.pick(&self.voc.macros).cloned().unwrap_or_else(|| "FUZZ_UNDEFINED".into())
    }

    /// A line of code someone might write.
    fn snippet(&mut self) -> String {
        let n = self.id();
        match self.rng.below(12) {
            0 => format!("  int fuzz_v{n} = {n};"),
            1 => format!("`define FUZZ_M{n} {n}"),
            2 => format!("`define FUZZ_F{n}(a) ((a) + {n})"),
            3 => format!("  import {}::*;", self.package()),
            4 => format!("  {} fuzz_h{n};", self.class()),
            5 => format!("  typedef struct packed {{ logic [3:0] f{n}; }} fuzz_s{n}_t;"),
            6 => format!("  function automatic int fuzz_f{n}(int a); return a + {n}; endfunction"),
            7 => format!("  localparam int FUZZ_P{n} = `{};", self.mac()),
            8 => format!("`undef {}", self.mac()),
            9 => match self.rng.pick(&self.voc.includes) {
                Some(i) => format!("`include \"{i}\""),
                None => "`include \"fuzz_missing.svh\"".into(),
            },
            10 => format!("  typedef enum {{ FZ_A{n}, FZ_B{n} }} fuzz_e{n};"),
            _ => format!("  // edited {n}"),
        }
    }

    /// One edit of `text`: (kind, new text), or `None` when this kind does not apply.
    fn edit(&mut self, text: &str, other: Option<&str>) -> Option<(&'static str, String)> {
        let mut l = Lines::of(text);
        let len = l.v.len();
        let line = self.rng.below(len + 1);
        let kind = self.rng.below(100);
        let r: (&'static str, String) = match kind {
            0..=5 => {
                l.v.insert(line, String::new());
                ("insert-blank-line", l.text())
            }
            6..=17 => {
                let s = self.snippet();
                l.v.insert(line, s);
                ("insert-line", l.text())
            }
            18..=25 => {
                let cands: Vec<usize> = (0..len).filter(|i| !l.v[*i].trim().is_empty()).collect();
                let &i = self.rng.pick(&cands)?;
                // A `define continued with backslashes goes as a whole.
                let mut e = i + 1;
                while e < len && l.v[e - 1].trim_end().ends_with('\\') {
                    e += 1;
                }
                l.v.drain(i..e);
                ("delete-line", l.text())
            }
            26..=29 => {
                let i = self.rng.below(len.max(1)).min(len.saturating_sub(1));
                let s = l.v.get(i)?.clone();
                l.v.insert(i, s);
                ("duplicate-line", l.text())
            }
            30..=39 => {
                let toks = lexer::lex(text, 0);
                let ids: Vec<&str> = toks.iter().filter(|t| t.kind == TokKind::Ident).map(|t| t.text.as_str()).collect();
                let name = self.rng.pick(&ids)?.to_string();
                let new = format!("{name}_fz{}", self.id());
                let mut out = String::with_capacity(text.len() + 64);
                let mut last = 0;
                for off in word_offsets(text, &name, usize::MAX) {
                    out.push_str(&text[last..off as usize]);
                    out.push_str(&new);
                    last = off as usize + name.len();
                }
                out.push_str(&text[last..]);
                ("rename-in-file", out)
            }
            40..=44 => {
                // Change the value of a `define, or its name.
                let cands: Vec<usize> = (0..len).filter(|i| l.v[*i].trim_start().starts_with("`define")).collect();
                let &i = self.rng.pick(&cands)?;
                if self.rng.chance(50) {
                    let s = l.v[i].trim_end().to_string();
                    if s.ends_with('\\') {
                        return None;
                    }
                    l.v[i] = format!("{s} + 1");
                    ("change-define-value", l.text())
                } else {
                    let s = l.v[i].clone();
                    let body = s.trim_start().trim_start_matches("`define").trim_start();
                    let name: String = body.bytes().take_while(|b| is_ident_byte(*b)).map(|b| b as char).collect();
                    if name.is_empty() {
                        return None;
                    }
                    let n = self.id();
                    l.v[i] = s.replacen(&name, &format!("{name}_FZ{n}"), 1);
                    ("rename-define", l.text())
                }
            }
            45..=49 => {
                let cands: Vec<usize> = (0..len).filter(|i| {
                    let t = l.v[*i].trim_start();
                    t.starts_with("`include") || t.starts_with("import ") || t.starts_with("`define")
                }).collect();
                let &i = self.rng.pick(&cands)?;
                let mut e = i + 1;
                while e < len && l.v[e - 1].trim_end().ends_with('\\') {
                    e += 1;
                }
                l.v.drain(i..e);
                ("remove-directive-or-import", l.text())
            }
            50..=57 => {
                let at = after_header(text, &["class"], self.rng)?;
                let n = self.id();
                let m = match self.rng.below(5) {
                    0 => format!("  int fuzz_m{n};"),
                    1 => format!("  rand bit [3:0] fuzz_r{n};"),
                    2 => format!("  function void fuzz_f{n}(); endfunction"),
                    3 => format!("  typedef int fuzz_t{n};"),
                    _ => format!("  {} fuzz_c{n};", self.class()),
                };
                l.v.insert(at.min(l.v.len()), m);
                ("add-class-member", l.text())
            }
            58..=62 => {
                let at = after_header(text, &["module", "package", "interface", "program"], self.rng)?;
                let n = self.id();
                let m = match self.rng.below(4) {
                    0 => format!("  import {}::*;", self.package()),
                    1 => format!("  logic fuzz_s{n};"),
                    2 => format!("  parameter int FUZZ_P{n} = {n};"),
                    _ => format!("  typedef logic [{n}:0] fuzz_w{n}_t;"),
                };
                l.v.insert(at.min(l.v.len()), m);
                ("add-element-item", l.text())
            }
            63..=74 => {
                // Break the syntax (fixed again by a later undo, mostly).
                let toks = lexer::lex(text, 0);
                let pick = |rng: &mut Rng, f: &dyn Fn(&dvi_syntax::Token) -> bool| {
                    let v: Vec<(u32, u32)> = toks.iter().filter(|t| f(t)).map(|t| (t.span.start, t.span.end)).collect();
                    rng.pick(&v).copied()
                };
                let (s, e, ins) = match self.rng.below(6) {
                    0 => pick(self.rng, &|t| t.is_punct(";")).map(|(s, e)| (s, e, ""))?,
                    1 => pick(self.rng, &|t| t.kind == TokKind::Keyword && END_KEYWORDS.contains(&t.text.as_str())).map(|(s, e)| (s, e, ""))?,
                    2 => pick(self.rng, &|t| t.kind == TokKind::Ident).map(|(s, _)| (s, s, "begin "))?,
                    3 => pick(self.rng, &|t| t.kind != TokKind::Eof).map(|(s, _)| (s, s, "( "))?,
                    4 => pick(self.rng, &|t| t.is_punct(")")).map(|(s, e)| (s, e, ""))?,
                    _ => {
                        let (s, _) = pick(self.rng, &|t| t.kind != TokKind::Eof)?;
                        let e = text[s as usize..].find('\n').map_or(text.len(), |i| s as usize + i);
                        (s, e as u32, "")
                    }
                };
                let mut out = text.to_string();
                out.replace_range(s as usize..e as usize, ins);
                ("break-syntax", out)
            }
            75..=79 => {
                if len == 0 {
                    return None;
                }
                let i = self.rng.below(len);
                let k = (1 + self.rng.below(6)).min(len - i);
                let n = self.id();
                let (open, close) = if self.rng.chance(50) { (format!("`ifdef FUZZ_OFF{n}"), "`endif".to_string()) } else { (format!("`ifndef FUZZ_OFF{n}"), "`endif".to_string()) };
                l.v.insert(i + k, close);
                l.v.insert(i, open);
                ("wrap-in-ifdef", l.text())
            }
            80..=85 => {
                // Move a block of lines elsewhere in the file.
                if len < 2 {
                    return None;
                }
                let i = self.rng.below(len);
                let k = (1 + self.rng.below(8)).min(len - i);
                let block: Vec<String> = l.v.drain(i..i + k).collect();
                let j = self.rng.below(l.v.len() + 1);
                for (x, s) in block.into_iter().enumerate() {
                    l.v.insert(j + x, s);
                }
                ("move-lines", l.text())
            }
            _ => {
                // Paste a few lines copied from another file.
                let o = Lines::of(other?);
                if o.v.is_empty() {
                    return None;
                }
                let i = self.rng.below(o.v.len());
                let k = (1 + self.rng.below(10)).min(o.v.len() - i);
                for (x, s) in o.v[i..i + k].iter().enumerate() {
                    l.v.insert(line + x, s.clone());
                }
                ("paste-lines", l.text())
            }
        };
        (r.1 != text).then_some(r)
    }
}

// ------------------------------------------------------------------------------ reproducers

/// The changed lines between two texts: `(line in before, removed, added)` hunks.
fn line_diff<'a>(a: &'a [String], b: &'a [String]) -> Vec<(usize, &'a [String], &'a [String])> {
    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
        s += 1;
    }
    let (x, y) = (&a[p..a.len() - s], &b[p..b.len() - s]);
    if x.is_empty() && y.is_empty() {
        return Vec::new();
    }
    if x.len() * y.len() > 4_000_000 {
        return vec![(p, x, y)];
    }
    // Longest common subsequence of the changed middle.
    let (n, m) = (x.len(), y.len());
    let mut l = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            l[i * (m + 1) + j] = if x[i] == y[j] { l[(i + 1) * (m + 1) + j + 1] + 1 } else { l[(i + 1) * (m + 1) + j].max(l[i * (m + 1) + j + 1]) };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && x[i] == y[j] {
            i += 1;
            j += 1;
            continue;
        }
        let (i0, j0) = (i, j);
        while (i < n || j < m) && !(i < n && j < m && x[i] == y[j]) {
            if j < m && (i == n || l[i * (m + 1) + j + 1] >= l[(i + 1) * (m + 1) + j]) {
                j += 1;
            } else {
                i += 1;
            }
        }
        out.push((p + i0, &x[i0..i], &y[j0..j]));
    }
    out
}

/// The changed lines between two texts.
fn hunk(before: &str, after: &str, out: &mut String) {
    let (a, b) = (Lines::of(before), Lines::of(after));
    for (line, old, new) in line_diff(&a.v, &b.v) {
        let _ = writeln!(out, "    @@ line {}", line + 1);
        for (sign, lines) in [('-', old), ('+', new)] {
            for x in lines.iter().take(12) {
                let _ = writeln!(out, "    {sign} {x}");
            }
            if lines.len() > 12 {
                let _ = writeln!(out, "    {sign} ... {} more lines", lines.len() - 12);
            }
        }
    }
    if a.nl != b.nl || a.trailing != b.trailing {
        let _ = writeln!(out, "    (line endings changed)");
    }
}

pub(crate) fn disk_text(p: &Path) -> Arc<str> {
    Arc::from(std::fs::read_to_string(p).unwrap_or_default())
}

fn describe(root: &Path, seed: u64, step: usize, base: &BTreeMap<PathBuf, Arc<str>>, steps: &[Vec<Change>], diffs: &[String]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "reproducer (seed {seed}, step {step}):");
    if base.is_empty() {
        let _ = writeln!(s, "  start: the files on disk, nothing open");
    } else {
        let _ = writeln!(s, "  start: the files on disk, with these files open in the editor:");
        for (p, t) in base {
            let _ = writeln!(s, "  {}", rel(root, p));
            let d = disk_text(p);
            if **t != *d {
                hunk(&d, t, &mut s);
            }
        }
    }
    for (i, st) in steps.iter().enumerate() {
        let _ = writeln!(s, "  edit {} (then an incremental update):", i + 1);
        for c in st {
            let _ = writeln!(s, "  {} {} ({:?})", c.kind, rel(root, &c.path), c.channel);
            hunk(&c.before, &c.after, &mut s);
        }
    }
    let _ = writeln!(s, "  differences:");
    for d in diffs.iter().take(12) {
        let _ = writeln!(s, "    {d}");
    }
    if diffs.len() > 12 {
        let _ = writeln!(s, "    ... {} more", diffs.len() - 12);
    }
    s
}

/// The editor contents after applying `steps` to `base`.
fn state_after(base: &BTreeMap<PathBuf, Arc<str>>, steps: &[Vec<Change>]) -> BTreeMap<PathBuf, Arc<str>> {
    let mut s = base.clone();
    for st in steps {
        for c in st {
            match c.channel {
                Channel::Close => {
                    s.remove(&c.path);
                }
                _ => {
                    s.insert(c.path.clone(), c.after.clone());
                }
            }
        }
    }
    s
}

type Reproducer = (BTreeMap<PathBuf, Arc<str>>, Vec<Vec<Change>>, Vec<String>);

/// Reduces a divergence to a starting state and the fewest last edits that still show it,
/// then drops open files from the starting state that do not matter. When it needs more of
/// the history than that, removes edits from the whole sequence first (delta debugging).
fn minimize(setup: &Arc<ProjectSetup>, base: &BTreeMap<PathBuf, Arc<str>>, steps: &[Vec<Change>], probe: &Probe) -> Option<Reproducer> {
    if let Some(r) = shortest_tail(setup, base, steps, probe) {
        return Some(r);
    }
    // Any subset of the edits is a valid sequence: each change carries the whole new text.
    let mut steps = steps.to_vec();
    let mut budget = 400;
    let mut parts = 2;
    while steps.len() >= 2 && budget > 0 {
        let chunk = steps.len().div_ceil(parts);
        let mut reduced = false;
        let mut i = 0;
        while i < steps.len() && budget > 0 {
            let mut fewer = steps.clone();
            fewer.drain(i..(i + chunk).min(steps.len()));
            budget -= 1;
            if !replay(setup, base, &fewer, probe).is_empty() {
                steps = fewer;
                parts = (parts - 1).max(2);
                reduced = true;
                break;
            }
            i += chunk;
        }
        if !reduced {
            if parts >= steps.len() {
                break;
            }
            parts = (parts * 2).min(steps.len());
        }
    }
    let steps = rebased(base, &steps);
    if let Some(r) = shortest_tail(setup, base, &steps, probe) {
        return Some(r);
    }
    let diffs = replay(setup, base, &steps, probe);
    (!diffs.is_empty()).then(|| (base.clone(), steps, diffs))
}

/// The changes with their `before` texts as they are when applied in this sequence.
fn rebased(base: &BTreeMap<PathBuf, Arc<str>>, steps: &[Vec<Change>]) -> Vec<Vec<Change>> {
    let mut state = base.clone();
    let mut out = Vec::new();
    for st in steps {
        let mut v = Vec::new();
        for c in st {
            let mut c = c.clone();
            c.before = state.get(&c.path).cloned().unwrap_or_else(|| disk_text(&c.path));
            match c.channel {
                Channel::Close => state.remove(&c.path),
                _ => state.insert(c.path.clone(), c.after.clone()),
            };
            v.push(c);
        }
        out.push(v);
    }
    out
}

/// The fewest last edits that show the divergence from a full build of the state before them,
/// with the open files that do not matter dropped from that state.
fn shortest_tail(setup: &Arc<ProjectSetup>, base: &BTreeMap<PathBuf, Arc<str>>, steps: &[Vec<Change>], probe: &Probe) -> Option<Reproducer> {
    let n = steps.len();
    for j in 1..=n.min(12) {
        let start = state_after(base, &steps[..n - j]);
        let tail = steps[n - j..].to_vec();
        let d = replay(setup, &start, &tail, probe);
        if d.is_empty() {
            continue;
        }
        // Drop open files that are not needed: first the whole file, else its edits before
        // the tail (the tail's first change of that file then starts from the disk text).
        let mut start = start;
        let mut tail = tail;
        let mut diffs = d;
        let paths: Vec<PathBuf> = start.keys().cloned().collect();
        for p in paths {
            let mut s2 = start.clone();
            s2.remove(&p);
            let touched = tail.iter().flatten().any(|c| c.path == p);
            let mut t2 = tail.clone();
            if touched {
                let disk = disk_text(&p);
                if let Some(c) = t2.iter_mut().flatten().find(|c| c.path == p) {
                    c.before = disk;
                }
            }
            let d2 = replay(setup, &s2, &t2, probe);
            if !d2.is_empty() {
                start = s2;
                tail = t2;
                diffs = d2;
            }
        }
        return Some((start, tail, diffs));
    }
    None
}

// ------------------------------------------------------------------------------ saved reproducers

/// Writes a reproducer as JSON: the editor contents to start from, the edits, and the
/// positions the comparison looks at.
fn save(path: &Path, probe: &Probe, base: &BTreeMap<PathBuf, Arc<str>>, steps: &[Vec<Change>]) -> std::io::Result<()> {
    let steps: Vec<Vec<serde_json::Value>> = steps
        .iter()
        .map(|st| {
            st.iter()
                .map(|c| serde_json::json!({ "path": c.path, "kind": c.kind, "channel": format!("{:?}", c.channel), "after": &*c.after }))
                .collect()
        })
        .collect();
    let v = serde_json::json!({
        "focus": probe.focus,
        "names": probe.names,
        "seed": probe.seed,
        "samples": probe.samples,
        "base": base.iter().map(|(p, t)| (p.display().to_string(), serde_json::Value::String(t.to_string()))).collect::<serde_json::Map<_, _>>(),
        "steps": steps,
    });
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default())
}

/// Replays a reproducer saved by `--fuzz-save`: the differences it still shows.
pub fn replay_file(setup: Arc<ProjectSetup>, file: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    let paths = |x: &serde_json::Value| -> Vec<PathBuf> { x.as_array().into_iter().flatten().filter_map(|p| p.as_str()).map(PathBuf::from).collect() };
    let strings = |x: &serde_json::Value| -> Vec<String> { x.as_array().into_iter().flatten().filter_map(|p| p.as_str()).map(String::from).collect() };
    let base: BTreeMap<PathBuf, Arc<str>> = v["base"].as_object().into_iter().flatten().map(|(p, t)| (PathBuf::from(p), Arc::from(t.as_str().unwrap_or("")))).collect();
    let mut steps = Vec::new();
    for st in v["steps"].as_array().into_iter().flatten() {
        let mut batch = Vec::new();
        for c in st.as_array().into_iter().flatten() {
            let channel = match c["channel"].as_str() {
                Some("Close") => Channel::Close,
                Some("Disk") => Channel::Disk,
                _ => Channel::Editor,
            };
            let path = PathBuf::from(c["path"].as_str().unwrap_or(""));
            batch.push(Change { path, kind: "replayed", before: Arc::from(""), after: Arc::from(c["after"].as_str().unwrap_or("")), channel });
        }
        steps.push(batch);
    }
    let root = setup.root.clone();
    let probe = Probe {
        root: &root,
        focus: paths(&v["focus"]),
        names: strings(&v["names"]),
        seed: v["seed"].as_u64().unwrap_or(0),
        samples: v["samples"].as_u64().unwrap_or(6) as usize,
    };
    Ok(replay(&setup, &base, &steps, &probe))
}

// ------------------------------------------------------------------------------ the session

/// Runs the fuzzer on a project. `log` receives progress and findings as they happen.
pub fn run(setup: Arc<ProjectSetup>, opts: &Options, log: &mut dyn FnMut(&str)) -> Report {
    let root = setup.root.clone();
    let mut rng = Rng(opts.seed);
    let mut report = Report::default();
    let mut inc = Incr::start(setup.clone(), &BTreeMap::new());
    // Files to edit: the project's own sources (not the builtin prelude, nor large files).
    let a0 = inc.a.clone();
    let mut editable: Vec<PathBuf> = Vec::new();
    let mut weight: FxHashMap<PathBuf, usize> = FxHashMap::default();
    for f in a0.project_files() {
        if a0.vfs.is_builtin(f) {
            continue;
        }
        let p = a0.vfs.path(f);
        let Some(t) = a0.vfs.text(f) else { continue };
        if t.len() > 400_000 || !p.is_file() {
            continue;
        }
        weight.insert(p.clone(), a0.units_of_file(f).len());
        editable.push(p);
    }
    editable.sort();
    let under_root: Vec<PathBuf> = editable.iter().filter(|p| p.starts_with(&root)).cloned().collect();
    if !under_root.is_empty() {
        editable = under_root;
    }
    if editable.is_empty() {
        log("fuzz: no files to edit");
        return report;
    }
    // Headers, by how many units include them.
    let mut headers: Vec<(PathBuf, usize)> = editable
        .iter()
        .filter(|p| a0.vfs.lookup(p).is_some_and(|f| a0.units_of_file(f).iter().all(|u| a0.unit(*u).root != f)))
        .map(|p| (p.clone(), weight[p]))
        .collect();
    headers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut voc = Vocabulary::default();
    for (n, v) in &a0.global.packages {
        if v.first().is_some_and(|r| !a0.vfs.is_builtin(a0.decl(*r).name_span.file)) {
            voc.packages.push(n.to_string());
        }
    }
    for (n, v) in &a0.global.macros {
        if v.iter().any(|m| m.name_span.file != dvi_syntax::preproc::COMMAND_LINE_FILE && !a0.vfs.is_builtin(m.name_span.file)) {
            voc.macros.push(n.to_string());
        }
    }
    let mut incs: FxHashSet<String> = FxHashSet::default();
    for u in a0.units.iter().flatten() {
        for i in &u.includes {
            if i.resolved.is_some() {
                incs.insert(i.path_text.clone());
            }
        }
        for d in &u.parsed.decls {
            if d.kind == dvi_syntax::ast::DeclKind::Class && u.kind != dvi_analysis::UnitKind::Builtin && voc.classes.len() < 500 {
                voc.classes.push(d.name.to_string());
            }
        }
    }
    voc.includes = incs.into_iter().collect();
    for v in [&mut voc.packages, &mut voc.macros, &mut voc.includes, &mut voc.classes] {
        v.sort();
        v.dedup();
    }
    log(&format!(
        "fuzz: seed {}, {} edits; {} editable files ({} headers), {} packages, {} macros, {} classes",
        opts.seed,
        opts.edits,
        editable.len(),
        headers.len(),
        voc.packages.len(),
        voc.macros.len(),
        voc.classes.len()
    ));

    let mut n_ids = 0u64;
    let mut history: FxHashMap<PathBuf, Vec<Arc<str>>> = FxHashMap::default();
    let mut working: Vec<PathBuf> = Vec::new();
    // Typing a snippet in pieces: (file, offset, pieces left).
    let mut typing: Option<(PathBuf, usize, Vec<String>)> = None;
    let mut broken: Option<PathBuf> = None;
    let mut scratch: Option<PathBuf> = None;
    // The incremental chain since the last full build: its starting state and its edits.
    let mut base: BTreeMap<PathBuf, Arc<str>> = BTreeMap::new();
    let mut chain: Vec<Vec<Change>> = Vec::new();
    let current = |inc: &Incr, p: &Path| -> Arc<str> { inc.open.get(p).cloned().unwrap_or_else(|| disk_text(p)) };

    let mut step = 0usize;
    while report.edits < opts.edits {
        step += 1;
        let mut batch: Vec<Change> = Vec::new();
        let count = if typing.is_none() && rng.chance(12) { 2 + rng.below(2) } else { 1 };
        for _ in 0..count {
            // Finish typing first.
            if let Some((p, off, mut rest)) = typing.take() {
                let before = current(&inc, &p);
                let piece = rest.remove(0);
                let mut t = before.to_string();
                if off <= t.len() && t.is_char_boundary(off) {
                    t.insert_str(off, &piece);
                    if !rest.is_empty() {
                        typing = Some((p.clone(), off + piece.len(), rest));
                    }
                    batch.push(Change { path: p, kind: "type", before, after: Arc::from(t), channel: Channel::Editor });
                }
                break;
            }
            // Fix a broken file again, mostly by undo.
            if let Some(p) = broken.take() {
                if rng.chance(70) {
                    if let Some(prev) = history.get_mut(&p).and_then(|h| h.pop()) {
                        let before = current(&inc, &p);
                        batch.push(Change { path: p, kind: "undo", before, after: prev, channel: Channel::Editor });
                        continue;
                    }
                }
            }
            // Close an open file now and then.
            if !inc.open.is_empty() && rng.chance(3) {
                let open: Vec<&PathBuf> = inc.open.keys().collect();
                let p = (*rng.pick(&open).unwrap()).clone();
                if batch.iter().any(|c| c.path == p) {
                    continue;
                }
                let before = current(&inc, &p);
                let after = if p.is_file() { disk_text(&p) } else { Arc::from("") };
                if scratch.as_ref() == Some(&p) {
                    scratch = None;
                }
                working.retain(|w| *w != p);
                batch.push(Change { path: p, kind: "close", before, after, channel: Channel::Close });
                continue;
            }
            // Open a file that is not part of the build.
            if scratch.is_none() && rng.chance(2) {
                let p = root.join(format!("fuzz_scratch_{}.sv", opts.seed));
                if !p.exists() {
                    let mut g = Gen { rng: &mut rng, voc: &voc, n: &mut n_ids };
                    let n = g.id();
                    let text = format!(
                        "module fuzz_scratch{n};\n  import {}::*;\n  {} h;\n  int v = `{};\nendmodule\n",
                        g.package(),
                        g.class(),
                        g.mac()
                    );
                    scratch = Some(p.clone());
                    working.push(p.clone());
                    batch.push(Change { path: p, kind: "open-scratch-file", before: Arc::from(""), after: Arc::from(text), channel: Channel::Editor });
                    continue;
                }
            }
            // Which file: mostly the few being worked on; sometimes a header many files include.
            let p = if !working.is_empty() && rng.chance(75) {
                rng.pick(&working).unwrap().clone()
            } else if !headers.is_empty() && rng.chance(30) {
                // Weighted towards the most included ones.
                let i = rng.below(headers.len()).min(rng.below(headers.len()));
                headers[i].0.clone()
            } else {
                rng.pick(&editable).unwrap().clone()
            };
            if batch.iter().any(|c| c.path == p) {
                continue;
            }
            if !working.contains(&p) {
                working.push(p.clone());
                if working.len() > 3 {
                    working.remove(0);
                }
            }
            let before = current(&inc, &p);
            let hist = history.entry(p.clone()).or_default();
            // Undo to an earlier text now and then.
            if !hist.is_empty() && rng.chance(6) {
                let k = rng.below(hist.len());
                let after = hist[k].clone();
                hist.truncate(k);
                batch.push(Change { path: p, kind: "undo-several", before, after, channel: Channel::Editor });
                continue;
            }
            let other = rng.pick(&editable).map(|o| current(&inc, o));
            let mut g = Gen { rng: &mut rng, voc: &voc, n: &mut n_ids };
            // Start typing a line in pieces.
            if g.rng.chance(8) {
                let s = format!("{}\n", g.snippet());
                let at = {
                    let starts: Vec<usize> = std::iter::once(0).chain(before.match_indices('\n').map(|(i, _)| i + 1)).filter(|i| *i <= before.len()).collect();
                    *g.rng.pick(&starts).unwrap()
                };
                let mut cuts: Vec<usize> = (0..1 + g.rng.below(3)).map(|_| g.rng.below(s.len())).filter(|c| s.is_char_boundary(*c)).collect();
                cuts.push(0);
                cuts.push(s.len());
                cuts.sort();
                cuts.dedup();
                let pieces: Vec<String> = cuts.windows(2).map(|w| s[w[0]..w[1]].to_string()).collect();
                typing = Some((p.clone(), at, pieces));
                // The first piece goes in now.
                let (_, off, mut rest) = typing.take().unwrap();
                let piece = rest.remove(0);
                let mut t = before.to_string();
                t.insert_str(off, &piece);
                if !rest.is_empty() {
                    typing = Some((p.clone(), off + piece.len(), rest));
                }
                hist.push(before.clone());
                batch.push(Change { path: p, kind: "type", before, after: Arc::from(t), channel: Channel::Editor });
                break;
            }
            let Some((kind, after)) = (0..8).find_map(|_| g.edit(&before, other.as_deref())) else { continue };
            hist.push(before.clone());
            if hist.len() > 50 {
                hist.remove(0);
            }
            if kind == "break-syntax" {
                broken = Some(p.clone());
            }
            let channel = if opts.disk && !inc.open.contains_key(&p) && p.is_file() && rng.chance(30) { Channel::Disk } else { Channel::Editor };
            batch.push(Change { path: p, kind, before, after: Arc::from(after), channel });
        }
        if batch.is_empty() {
            continue;
        }
        report.steps += 1;
        report.edits += batch.len();
        for c in &batch {
            *report.kinds.entry(c.kind).or_default() += 1;
            if opts.verbose {
                let mut s = format!("fuzz: step {step}: {} {} ({:?})\n", c.kind, rel(&root, &c.path), c.channel);
                hunk(&c.before, &c.after, &mut s);
                log(s.trim_end());
            }
        }
        // Names on the edited lines, to check their uses in other files.
        let mut names: Vec<String> = Vec::new();
        for c in &batch {
            let (a, b) = (Lines::of(&c.before), Lines::of(&c.after));
            let hunks = line_diff(&a.v, &b.v);
            for l in hunks.iter().flat_map(|(_, x, y)| x.iter().chain(y.iter())).take(20) {
                for t in lexer::lex(l, 0) {
                    if matches!(t.kind, TokKind::Ident | TokKind::Directive) && t.text.len() > 2 && !names.contains(&t.text.to_string()) {
                        names.push(t.text.to_string());
                    }
                }
            }
        }
        names.truncate(6);
        let probe = Probe {
            root: &root,
            focus: batch.iter().filter(|c| c.channel != Channel::Close || c.path.is_file()).map(|c| c.path.clone()).collect(),
            names,
            seed: opts.seed ^ (step as u64).wrapping_mul(0x9E37_79B9),
            samples: opts.samples,
        };
        let applied = guarded("update", || inc.apply(&batch, opts.disk));
        chain.push(batch);
        let diffs = match applied {
            Ok(()) => guarded("full build", || {
                let full = inc.full();
                differences(&inc, &full, &probe)
            })
            .unwrap_or_else(|e| vec![e]),
            Err(e) => vec![e],
        };
        if diffs.is_empty() {
            continue;
        }
        log(&format!("fuzz: step {step}: {} difference(s), reducing...", diffs.len()));
        let (text, start, tail) = match minimize(&setup, &base, &chain, &probe) {
            Some((start, tail, d)) => (describe(&root, opts.seed, step, &start, &tail, &d), start, tail),
            None => {
                let mut s = describe(&root, opts.seed, step, &base, &chain, &diffs);
                s.push_str("  (seen only with the exact sequence above, applied in this order)\n");
                (s, base.clone(), chain.clone())
            }
        };
        log(&text);
        if let Some(dir) = &opts.save {
            let p = dir.join(format!("fuzz-{}-{step}.json", opts.seed));
            match save(&p, &probe, &start, &tail) {
                Ok(()) => log(&format!("fuzz: reproducer saved to {} (replay with --fuzz-replay)", p.display())),
                Err(e) => log(&format!("fuzz: cannot save {}: {e}", p.display())),
            }
        }
        report.findings.push(Finding { step, differences: diffs, reproducer: text });
        if report.findings.len() >= opts.max_findings {
            break;
        }
        // Continue from a full build of the current state.
        let open = inc.open.clone();
        inc = Incr::start(setup.clone(), &open);
        base = open;
        chain.clear();
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_setup(root: &Path) -> Arc<ProjectSetup> {
        let env = FxHashMap::default();
        let cfg = dvi_project::load(root, Some(&root.join(".dvicode/default.build")), &env);
        Arc::new(ProjectSetup::new(root, cfg, env))
    }

    fn demo_root() -> PathBuf {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../examples/demo-project").canonicalize().unwrap();
        match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
            Some(rest) => PathBuf::from(rest),
            None => p,
        }
    }

    #[test]
    fn lines_round_trip() {
        for t in ["", "a", "a\n", "a\r\nb\r\n", "a\n\nb", "\n"] {
            assert_eq!(Lines::of(t).text(), t, "{t:?}");
        }
    }

    /// A short seeded run on the demo project: incremental updates agree with full builds.
    #[test]
    fn incremental_updates_match_full_builds_on_the_demo() {
        let setup = demo_setup(&demo_root());
        let opts = Options { edits: 150, seed: 7, samples: 4, max_findings: 3, ..Default::default() };
        let mut out = String::new();
        let r = run(setup, &opts, &mut |s| {
            out.push_str(s);
            out.push('\n');
        });
        assert!(r.edits >= 150);
        assert!(r.findings.is_empty(), "{out}");
    }
}
