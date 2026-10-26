//! The compile context chosen for a file that several units compile (a header included by
//! several roots, or a root compiled by several invocations).
//!
//! The server keeps the choices ([`ContextChoices`], by path) and resolves them against each
//! snapshot it installs ([`Analysis::select_contexts`]): the result ([`Selection`]) belongs to
//! that snapshot and never changes. Without a choice a file reads as before: its first unit,
//! falling through to later ones. With one, the editor features about the file see the chosen
//! unit only ([`Analysis::context_unit`], [`Analysis::editor_units`]); searches over the project
//! keep every unit.

use crate::ide::context::Place;
use crate::ide::url_of;
use crate::world::{Analysis, ProjectSetup, Unit, UnitId, UnitKind};
use dvi_syntax::FileId;
use rustc_hash::FxHashMap;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

/// A context that survives build file edits: the root file, how its invocation preprocesses
/// (a fingerprint of mode, defines, undefines and include directories, not its position), and
/// which of the units with both it is (in invocation order; 0 but for identical sections).
///
/// Sections that preprocess alike can still differ by the files compiled before the root (the
/// macros they leave): when several compile the root, the key also holds a fingerprint of
/// those files (`before`), and `nth` counts among the units with the same one, so reordering
/// or inserting sections never makes it name another compilation. A choice made while the
/// root had one such unit does not apply while it has several.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContextKey {
    pub root: PathBuf,
    pub invocation: String,
    pub nth: u32,
    /// The fingerprint of the files compiled before the root, when other units of the root
    /// preprocess alike.
    pub before: Option<String>,
}

/// The contexts chosen, by file path (a file the engine has not read yet keeps its choice).
#[derive(Clone, Debug, Default)]
pub struct ContextChoices(pub FxHashMap<PathBuf, ContextKey>);

/// The choices as resolved in one snapshot.
#[derive(Debug, Default)]
pub struct Selection {
    /// The server's count of choice changes this was resolved from.
    pub generation: u64,
    pub chosen: FxHashMap<FileId, UnitId>,
    /// Files with a choice that no unit of this snapshot matches: they read as by default.
    pub unavailable: FxHashMap<FileId, ContextKey>,
    /// Choices for files this snapshot has not read.
    pub unknown: FxHashMap<PathBuf, ContextKey>,
}

/// The fingerprint of how an invocation preprocesses, kept by clients from one engine version
/// to the next: `v1-` and the 64-bit FNV-1a, in hexadecimal, of a sequence of items, each
/// written as its length in bytes (64-bit little-endian) and its bytes: the mode; the number of
/// defines (8 bytes, little-endian), then for each its name and `=value` (empty without a
/// value); the number of undefines and their names; the number of include directories and
/// their paths as the build gives them (UTF-8).
pub fn invocation_key(inv: &dvi_project::Invocation) -> String {
    let mut h = Fnv::new();
    h.put(inv.mode.as_bytes());
    h.put(&(inv.defines.len() as u64).to_le_bytes());
    for (n, v) in &inv.defines {
        h.put(n.as_bytes());
        match v {
            Some(v) => h.put(format!("={v}").as_bytes()),
            None => h.put(b""),
        }
    }
    h.put(&(inv.undefines.len() as u64).to_le_bytes());
    for n in &inv.undefines {
        h.put(n.as_bytes());
    }
    h.put(&(inv.incdirs.len() as u64).to_le_bytes());
    for d in &inv.incdirs {
        h.put(d.to_string_lossy().as_bytes());
    }
    format!("v1-{:016x}", h.0)
}

/// FNV-1a over items written as their length (64-bit little-endian) and their bytes.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn put(&mut self, b: &[u8]) {
        for x in (b.len() as u64).to_le_bytes().iter().chain(b) {
            self.0 ^= *x as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl ProjectSetup {
    /// The fingerprint of invocation `inv` (`none` for a unit of no invocation).
    pub fn invocation_key(&self, inv: usize) -> &str {
        self.inv_keys.get(inv).map_or("none", String::as_str)
    }
}

/// One context of a file, for choosing it.
#[derive(Debug, Serialize)]
pub struct ContextItem {
    /// The key (with `root` as a uri): what the client sends back to choose it.
    pub root: String,
    pub invocation_key: String,
    pub nth: u32,
    pub before: Option<String>,
    /// Its 1-based position in the default order (the first is the default).
    pub ordinal: usize,
    /// Zero-based invocation index, for display (it changes when sections are added).
    pub invocation: usize,
    pub mode: String,
    /// `top`, `library` or `adhoc`.
    pub root_role: &'static str,
    /// The include that first read the file in this unit (none for the root itself).
    pub includer: Option<Place>,
    pub chosen: bool,
}

/// `dvicode/compileContexts`: the contexts of a file, a page of them, and the choice.
#[derive(Debug, Default, Serialize)]
pub struct ContextList {
    pub total: usize,
    /// Contexts the filter matches.
    pub matched: usize,
    pub offset: usize,
    /// `default` (no choice), `chosen`, or `unavailable` (a choice no context matches now).
    pub state: &'static str,
    pub chosen: Option<ContextItem>,
    pub default: Option<ContextItem>,
    /// The unavailable choice: its root (uri), invocation fingerprint and nth.
    pub unavailable: Option<(String, String, u32)>,
    pub items: Vec<ContextItem>,
}

pub const MAX_PAGE: usize = 1_000;

pub fn root_role(u: &Unit) -> &'static str {
    match u.kind {
        UnitKind::Project if u.library => "library",
        UnitKind::Project => "top",
        UnitKind::AdHoc => "adhoc",
        UnitKind::Builtin => "builtin",
    }
}

impl Analysis {
    /// The unit whose view of `file` the editor features use: the chosen one, else the first
    /// in the default order.
    pub fn context_unit(&self, file: FileId) -> Option<UnitId> {
        self.chosen_unit(file).or_else(|| self.context_unit_default(file))
    }

    /// The units the editor features look through for `file`: the chosen one only, else every
    /// unit in the default order (the first that has something wins).
    pub fn editor_units(&self, file: FileId) -> Vec<UnitId> {
        match self.chosen_unit(file) {
            Some(u) => vec![u],
            None => self.units_for(file),
        }
    }

    /// The chosen unit of `file`, when it still compiles it in this snapshot.
    pub fn chosen_unit(&self, file: FileId) -> Option<UnitId> {
        let &u = self.selection.chosen.get(&file)?;
        (self.try_unit(u).is_some() && self.units_of_file(file).contains(&u)).then_some(u)
    }

    /// The units with root `root` whose invocation has fingerprint `key`, in invocation order.
    fn alike(&self, root: FileId, key: &str) -> Vec<&Arc<Unit>> {
        let mut same: Vec<&Arc<Unit>> = self
            .units_of_file(root)
            .iter()
            .filter_map(|id| self.try_unit(*id))
            .filter(|o| o.root == root && o.kind != UnitKind::Builtin && self.setup.invocation_key(o.invocation) == key)
            .collect();
        same.sort_by_key(|o| (o.invocation, o.id));
        same
    }

    /// The fingerprint of what a unit is compiled after: the invocation's top files before its
    /// root, or the library units before it in that invocation.
    fn before_key(&self, u: &Unit) -> String {
        let mut h = Fnv::new();
        let inv = self.setup.config.invocations.get(u.invocation);
        if u.library {
            h.put(b"library");
            for id in self.library_order.iter().take_while(|id| **id != u.id) {
                if let Some(o) = self.try_unit(*id).filter(|o| o.invocation == u.invocation) {
                    h.put(self.path(o.root).to_string_lossy().as_bytes());
                }
            }
        } else if let (UnitKind::Project, Some(inv)) = (u.kind, inv) {
            h.put(b"top");
            // The build's spelling of the root may differ from the file table's (case).
            for f in inv.files.iter().take_while(|f| self.vfs.lookup(f) != Some(u.root)) {
                h.put(f.to_string_lossy().as_bytes());
            }
        } else {
            h.put(b"adhoc");
        }
        format!("{:016x}", h.0)
    }

    /// The durable key of a unit's context.
    pub fn context_key(&self, uid: UnitId) -> ContextKey {
        let u = self.unit(uid);
        let key = self.setup.invocation_key(u.invocation);
        let same = self.alike(u.root, key);
        if same.len() <= 1 {
            return ContextKey { root: self.path(u.root), invocation: key.to_string(), nth: 0, before: None };
        }
        let before = self.before_key(u);
        let nth = same.iter().filter(|o| self.before_key(o) == before).position(|o| o.id == uid).unwrap_or(0) as u32;
        ContextKey { root: self.path(u.root), invocation: key.to_string(), nth, before: Some(before) }
    }

    /// The unit of this snapshot that `key` names, when it compiles `file`.
    pub fn resolve_context(&self, file: FileId, key: &ContextKey) -> Option<UnitId> {
        let root = self.vfs.lookup(&key.root)?;
        let same = self.alike(root, &key.invocation);
        // Several alike: the one after the same files; none named while there was one.
        let u = match &key.before {
            Some(b) => *same.iter().filter(|o| self.before_key(o) == *b).nth(key.nth as usize)?,
            None if same.len() == 1 && key.nth == 0 => same[0],
            None => return None,
        };
        self.units_of_file(file).contains(&u.id).then_some(u.id)
    }

    /// Resolves `choices` against this snapshot's units and makes the result its selection.
    /// The files whose chosen unit differs from the selection it had: when there are any, the
    /// caches that depend on it are dropped from this copy.
    pub fn select_contexts(&mut self, choices: &ContextChoices, generation: u64) -> Vec<FileId> {
        let mut sel = Selection { generation, ..Default::default() };
        // In a fixed order: of two spellings of one file, the same one wins every time.
        let mut all: Vec<(&PathBuf, &ContextKey)> = choices.0.iter().collect();
        all.sort_by(|a, b| a.0.cmp(b.0));
        for (path, key) in all {
            let Some(file) = self.vfs.lookup(path) else {
                sel.unknown.insert(path.clone(), key.clone());
                continue;
            };
            sel.unavailable.remove(&file);
            sel.chosen.remove(&file);
            match self.resolve_context(file, key) {
                Some(u) => {
                    sel.chosen.insert(file, u);
                }
                None => {
                    sel.unavailable.insert(file, key.clone());
                }
            }
        }
        let old = &self.selection.chosen;
        let mut changed: Vec<FileId> = old.iter().filter(|(f, u)| sel.chosen.get(f) != Some(u)).map(|(f, _)| *f).collect();
        changed.extend(sel.chosen.iter().filter(|(f, u)| old.get(f) != Some(u)).map(|(f, _)| *f));
        changed.sort_unstable();
        changed.dedup();
        if !changed.is_empty() {
            // Skipped-code uses are counted, and config_db calls indexed, in each file's context
            // unit (and the places and test models made from those calls).
            self.hidden_uses = Arc::default();
            self.hidden_seed = None;
            self.config_calls = Arc::default();
            self.config_places_cache = Arc::default();
            self.override_models = Arc::default();
        }
        self.selection = Arc::new(sel);
        changed
    }

    fn context_item(&self, uid: UnitId, file: FileId, ordinal: usize, chosen: Option<UnitId>) -> ContextItem {
        let u = self.unit(uid);
        let key = self.context_key(uid);
        let includer = if u.root == file {
            None
        } else {
            u.includes.iter().find(|i| i.resolved == Some(file) && i.entered).and_then(|i| self.place(i.span))
        };
        ContextItem {
            root: url_of(&key.root).to_string(),
            invocation_key: key.invocation,
            nth: key.nth,
            before: key.before,
            ordinal,
            invocation: u.invocation,
            mode: self.setup.config.invocations.get(u.invocation).map(|i| i.mode.clone()).unwrap_or_default(),
            root_role: root_role(u),
            includer,
            chosen: chosen == Some(uid),
        }
    }

    /// The contexts of `file`: the choice, the default, and the page `offset..offset + limit`
    /// of those `filter` matches (case-insensitive, in the root's path, the mode, `inv N` and
    /// the includer's path), in the default order. `limit` 0: no page, no ordering of all the
    /// units unless there is a choice (for a status bar).
    pub fn context_list(&self, file: FileId, filter: Option<&str>, offset: usize, limit: usize) -> ContextList {
        let mut out = ContextList { total: self.units_of_file(file).len(), offset, state: "default", ..Default::default() };
        let chosen = self.chosen_unit(file);
        if let Some(key) = self.selection.unavailable.get(&file) {
            out.state = "unavailable";
            out.unavailable = Some((url_of(&key.root).to_string(), key.invocation.clone(), key.nth));
        }
        if chosen.is_some() {
            out.state = "chosen";
        }
        let default = self.context_unit_default(file);
        if limit == 0 {
            out.matched = out.total;
            if let Some(d) = default {
                out.default = Some(self.context_item(d, file, 1, chosen));
            }
            if let Some(c) = chosen {
                let ordinal = self.units_for(file).iter().position(|u| *u == c).map_or(0, |i| i + 1);
                out.chosen = Some(self.context_item(c, file, ordinal, chosen));
            }
            return out;
        }
        let units = self.units_for(file);
        let limit = limit.min(MAX_PAGE);
        // Every word must match.
        let words: Vec<String> = filter.map(|f| f.to_lowercase().replace("inv ", "inv").split_whitespace().map(str::to_string).collect()).unwrap_or_default();
        let needle = (!words.is_empty()).then_some(words);
        let mut matched = 0usize;
        for (i, &uid) in units.iter().enumerate() {
            if Some(uid) == chosen {
                out.chosen = Some(self.context_item(uid, file, i + 1, chosen));
            }
            if i == 0 {
                out.default = Some(self.context_item(uid, file, 1, chosen));
            }
            if let Some(n) = &needle {
                let u = self.unit(uid);
                let mut hay = self.path(u.root).to_string_lossy().to_lowercase();
                if let Some(inv) = self.setup.config.invocations.get(u.invocation) {
                    hay.push('\n');
                    hay.push_str(&inv.mode.to_lowercase());
                }
                hay.push_str(&format!("\ninv{}\n", u.invocation + 1));
                if u.root != file {
                    if let Some(i) = u.includes.iter().find(|i| i.resolved == Some(file) && i.entered) {
                        hay.push_str(&self.path(i.span.file).to_string_lossy().to_lowercase());
                    }
                }
                // `inv 3` (or `inv3`): invocation 3 exactly.
                let inv_word = |w: &str| w.len() > 3 && w.starts_with("inv") && w[3..].bytes().all(|b| b.is_ascii_digit());
                if !n.iter().all(|w| if inv_word(w) { hay.contains(&format!("\n{w}\n")) } else { hay.contains(w.as_str()) }) {
                    continue;
                }
            }
            if matched >= offset && out.items.len() < limit {
                out.items.push(self.context_item(uid, file, i + 1, chosen));
            }
            matched += 1;
        }
        out.matched = matched;
        out
    }

    /// [`Analysis::context_list`] of a file this snapshot has not read: no context, and the
    /// choice kept for it, if any.
    pub fn unknown_context_list(&self, path: &std::path::Path) -> ContextList {
        let mut out = ContextList { state: "default", ..Default::default() };
        if let Some(key) = self.selection.unknown.get(path) {
            out.state = "unavailable";
            out.unavailable = Some((url_of(&key.root).to_string(), key.invocation.clone(), key.nth));
        }
        out
    }

    /// The first unit of `file` in the default order, whatever is chosen.
    pub fn context_unit_default(&self, file: FileId) -> Option<UnitId> {
        let ids = self.units_of_file(file);
        // As `units_for` orders them, without its copy: project units by id.
        if ids.iter().all(|id| self.try_unit(*id).is_some_and(|u| u.kind == UnitKind::Project)) {
            return ids.iter().min().copied();
        }
        self.units_for(file).first().copied()
    }
}
