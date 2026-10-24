//! In-memory view of source files: disk contents, editor overlays, lexing cache.
//!
//! The store is shared and mutable; analysis snapshots copy what they need (see
//! [`crate::world::Source`]) so that a snapshot never mixes its syntax with newer text.
//!
//! File existence checks go through a cache of directory listings: resolving an `include
//! probes every include directory, and on slow file systems (WSL's `/mnt` drives, network
//! shares) one `stat` per directory per include made builds take minutes. Each directory is
//! read once instead; [`Vfs::clear_dir_cache`] starts over (every full build does) and
//! [`Vfs::invalidate`] forgets the directory of a changed file.

use dvi_syntax::lexer::lex;
use dvi_syntax::preproc::normalize;
use dvi_syntax::{FileId, LineIndex, SourceProvider, Token};
use rustc_hash::{FxHashMap, FxHashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Default)]
struct Entry {
    disk: Option<Arc<str>>,
    disk_loaded: bool,
    overlay: Option<Arc<str>>,
    /// Editor (LSP) version of the overlay text.
    overlay_version: Option<i32>,
    /// Changes with every edit, invalidation or overlay change.
    version: u64,
    /// Tokens and the exact text they were lexed from.
    tokens: Option<(u64, Arc<str>, Arc<Vec<Token>>)>,
    lines: Option<(u64, Arc<LineIndex>)>,
}

#[derive(Default)]
struct Inner {
    paths: Vec<PathBuf>,
    /// By path, and other spellings of the same file as they are met.
    ids: FxHashMap<PathBuf, FileId>,
    /// The ids by [`fold`]ed path, to find another spelling of a file.
    folded: FxHashMap<PathBuf, Vec<FileId>>,
    entries: Vec<Entry>,
}

#[derive(Clone, Copy, PartialEq)]
enum EntryKind {
    File,
    Dir,
    /// Ask the file system: a symbolic link, a name spelled with different case (the same
    /// entry on file systems that ignore case), or a directory that cannot be listed.
    Unknown,
}

/// The entries of one directory.
enum DirListing {
    Missing,
    /// Exists but cannot be listed: ask the file system for each path.
    Unlistable,
    Listed {
        entries: FxHashMap<OsString, EntryKind>,
        /// Lowercase names, for file systems that ignore case (Windows, WSL's `/mnt` drives).
        folded: FxHashSet<String>,
    },
}

impl DirListing {
    fn read(dir: &Path) -> DirListing {
        // A network host that is not allowed: as if it were not there (`dvi_project::netpath`).
        if dvi_project::netpath::blocked(dir).is_some() {
            return DirListing::Missing;
        }
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DirListing::Missing,
            Err(_) if !dir.is_dir() => return DirListing::Missing,
            Err(_) => return DirListing::Unlistable,
        };
        let mut entries = FxHashMap::default();
        let mut folded = FxHashSet::default();
        for e in rd {
            let Ok(e) = e else { return DirListing::Unlistable };
            let kind = match e.file_type() {
                Ok(t) if t.is_file() => EntryKind::File,
                Ok(t) if t.is_dir() => EntryKind::Dir,
                _ => EntryKind::Unknown,
            };
            let name = e.file_name();
            folded.insert(name.to_string_lossy().to_lowercase());
            entries.insert(name, kind);
        }
        DirListing::Listed { entries, folded }
    }

    /// The kind of the entry `name`, `None` if there is none.
    fn lookup(&self, name: &OsStr) -> Option<EntryKind> {
        match self {
            DirListing::Missing => None,
            DirListing::Unlistable => Some(EntryKind::Unknown),
            DirListing::Listed { entries, folded } => match entries.get(name) {
                Some(k) => Some(*k),
                None if folded.contains(&name.to_string_lossy().to_lowercase()) => Some(EntryKind::Unknown),
                // A Windows 8.3 short name (`RUNNER~1`) is not in the listing, which has the long
                // name: let the file system decide.
                None if cfg!(windows) && name.to_string_lossy().contains('~') => Some(EntryKind::Unknown),
                None => None,
            },
        }
    }
}

/// Thread-safe file store shared by all analysis snapshots.
#[derive(Default)]
pub struct Vfs {
    inner: RwLock<Inner>,
    dirs: RwLock<DirCache>,
}

#[derive(Default)]
struct DirCache {
    listings: FxHashMap<PathBuf, Arc<DirListing>>,
    /// Changes whenever listings are forgotten: a listing read meanwhile may be outdated.
    generation: u64,
}

/// Contents of built-in files (the `std` package prelude).
pub const BUILTIN_DIR: &str = "/__dvicode_builtin__";

impl Vfs {
    pub fn new() -> Vfs {
        Vfs::default()
    }

    pub fn lookup(&self, path: &Path) -> Option<FileId> {
        let p = normalize(path);
        let candidates = {
            let g = self.inner.read().unwrap();
            if let Some(id) = g.ids.get(&p) {
                return Some(*id);
            }
            g.spellings(&p)
        };
        same_as(&candidates, &p)
    }

    /// Sets the editor contents of a file.
    pub fn set_overlay(&self, path: &Path, text: Arc<str>) -> FileId {
        self.set_overlay_versioned(path, text, None)
    }

    /// Sets the editor contents of a file with the editor's document version.
    pub fn set_overlay_versioned(&self, path: &Path, text: Arc<str>, lsp_version: Option<i32>) -> FileId {
        let id = self.file_id(path);
        let mut g = self.inner.write().unwrap();
        let e = &mut g.entries[id as usize];
        e.overlay = Some(text);
        e.overlay_version = lsp_version;
        e.version += 1;
        id
    }

    pub fn clear_overlay(&self, path: &Path) {
        if let Some(id) = self.lookup(path) {
            let mut g = self.inner.write().unwrap();
            let e = &mut g.entries[id as usize];
            e.overlay_version = None;
            if e.overlay.take().is_some() {
                e.version += 1;
            }
        }
    }

    pub fn has_overlay(&self, id: FileId) -> bool {
        self.inner.read().unwrap().entries.get(id as usize).is_some_and(|e| e.overlay.is_some())
    }

    /// The editor version of `text` if it is the current editor contents of the file.
    pub fn lsp_version_for(&self, id: FileId, text: &Arc<str>) -> Option<i32> {
        let g = self.inner.read().unwrap();
        let e = g.entries.get(id as usize)?;
        match &e.overlay {
            Some(o) if Arc::ptr_eq(o, text) => e.overlay_version,
            _ => None,
        }
    }

    /// Forgets cached disk contents (after a change on disk), and the directory listings that
    /// may no longer be right: of the directory that holds `path`, of `path` itself and below
    /// (a created or deleted directory), and of ancestors that did not have the way to it (a
    /// file created in a new directory).
    pub fn invalidate(&self, path: &Path) -> Option<FileId> {
        self.invalidate_all(path).first().copied()
    }

    /// [`Vfs::invalidate`] for every file id the path may name: where case does not matter,
    /// each spelling met (a deleted file cannot be asked which spellings are its own).
    pub fn invalidate_all(&self, path: &Path) -> Vec<FileId> {
        let p = normalize(path);
        let mut dirs = self.dirs.write().unwrap();
        dirs.generation += 1;
        dirs.listings.retain(|d, l| {
            if d.starts_with(&p) {
                return false;
            }
            let Ok(rest) = p.strip_prefix(d) else { return true };
            let mut below = rest.components();
            match (below.next(), below.next()) {
                (Some(child), Some(_)) => matches!(l.lookup(child.as_os_str()), Some(EntryKind::Dir | EntryKind::Unknown)),
                _ => false,
            }
        });
        drop(dirs);
        let mut ids: Vec<FileId> = self.lookup(path).into_iter().collect();
        {
            let g = self.inner.read().unwrap();
            for id in fold(&p).and_then(|f| g.folded.get(&f)).into_iter().flatten() {
                if !ids.contains(id) {
                    ids.push(*id);
                }
            }
        }
        let mut g = self.inner.write().unwrap();
        for id in &ids {
            let e = &mut g.entries[*id as usize];
            e.disk = None;
            e.disk_loaded = false;
            e.version += 1;
        }
        ids
    }

    pub fn version(&self, id: FileId) -> u64 {
        self.inner.read().unwrap().entries.get(id as usize).map_or(0, |e| e.version)
    }

    /// Line index of the current text. Analysis code uses the snapshot's line index instead.
    pub fn line_index(&self, id: FileId) -> Option<Arc<LineIndex>> {
        {
            let g = self.inner.read().unwrap();
            let e = g.entries.get(id as usize)?;
            if let Some((v, li)) = &e.lines {
                if *v == e.version {
                    return Some(li.clone());
                }
            }
        }
        let (text, version) = self.text_versioned(id)?;
        let li = Arc::new(LineIndex::new(&text));
        let mut g = self.inner.write().unwrap();
        let e = &mut g.entries[id as usize];
        // Cache only if nothing changed meanwhile; otherwise the result is still correct for
        // the text it was built from, but must not be reused for the newer one.
        if e.version == version {
            e.lines = Some((version, li.clone()));
        }
        Some(li)
    }

    /// The text of a file and the version it belongs to, read consistently: a concurrent edit
    /// or invalidation either happens entirely before (newer pair) or after (older pair).
    pub fn text_versioned(&self, id: FileId) -> Option<(Arc<str>, u64)> {
        self.read_text(id, true)
    }

    /// The disk contents of a file, also when the editor has it open.
    pub fn disk_text(&self, id: FileId) -> Option<Arc<str>> {
        self.read_text(id, false).map(|(t, _)| t)
    }

    /// The editor texts of all open files, with their versions, as they are now.
    pub fn pin(&self) -> Pin {
        let g = self.inner.read().unwrap();
        let overlays = g.entries.iter().enumerate().filter_map(|(i, e)| e.overlay.clone().map(|o| (i as FileId, (o, e.overlay_version)))).collect();
        Pin { overlays, tokens: Mutex::default() }
    }

    /// The store as an update reads it: with editor texts as of `pin`, or as they are now.
    pub fn reader<'a>(&'a self, pin: Option<&'a Pin>) -> Reader<'a> {
        Reader { vfs: self, pin }
    }

    fn read_text(&self, id: FileId, overlay: bool) -> Option<(Arc<str>, u64)> {
        loop {
            let (path, version) = {
                let g = self.inner.read().unwrap();
                let e = g.entries.get(id as usize)?;
                if let Some(o) = e.overlay.as_ref().filter(|_| overlay) {
                    return Some((o.clone(), e.version));
                }
                if e.disk_loaded {
                    return e.disk.clone().map(|t| (t, e.version));
                }
                (g.paths[id as usize].clone(), e.version)
            };
            // Regular files only: reading a FIFO named as a source would block. Nothing on a
            // network host that is not allowed: not even its metadata.
            let blocked = dvi_project::netpath::blocked(&path);
            if let Some(host) = &blocked {
                log::warn!("not reading {}: the network host {host} is not allowed (security.allowedUNCHosts)", path.display());
            }
            let meta = if blocked.is_some() { Err(()) } else { Ok(()) }.and_then(|()| std::fs::metadata(&path).map_err(|e| {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("cannot read {}: {e}", path.display());
                }
            }));
            let bytes = meta.ok().filter(|m| m.is_file()).and_then(|_| match std::fs::read(&path) {
                Ok(b) => Some(b),
                Err(e) => {
                    log::warn!("cannot read {}: {e}", path.display());
                    None
                }
            });
            let text: Option<Arc<str>> = bytes.map(|b| match String::from_utf8(b) {
                Ok(s) => Arc::from(s),
                Err(e) => Arc::from(String::from_utf8_lossy(e.as_bytes()).into_owned()),
            });
            let mut g = self.inner.write().unwrap();
            let e = &mut g.entries[id as usize];
            if e.version != version {
                // Edited or invalidated while reading the disk: read again.
                continue;
            }
            e.disk = text.clone();
            e.disk_loaded = true;
            return text.map(|t| (t, version));
        }
    }

    /// Forgets the cached disk contents of every file (open editors' text stays), for a
    /// rebuild that must also see changes made where the editor does not watch.
    pub fn clear_disk_cache(&self) {
        let mut g = self.inner.write().unwrap();
        for e in g.entries.iter_mut() {
            if e.disk_loaded {
                e.disk = None;
                e.disk_loaded = false;
                e.version += 1;
            }
        }
    }

    /// Forgets all directory listings, so that files created or deleted since are seen, also
    /// where the editor does not watch.
    pub fn clear_dir_cache(&self) {
        let mut dirs = self.dirs.write().unwrap();
        dirs.generation += 1;
        dirs.listings.clear();
    }

    /// Reads a directory into the cache ahead of the existence checks that need it.
    pub fn list_dir(&self, dir: &Path) {
        self.dir_listing(&normalize(dir));
    }

    /// What the listing of its directory says about a normalized path.
    fn entry_kind(&self, p: &Path) -> Option<EntryKind> {
        match (p.parent(), p.file_name()) {
            (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => self.dir_listing(dir).lookup(name),
            _ => Some(EntryKind::Unknown),
        }
    }

    fn dir_listing(&self, dir: &Path) -> Arc<DirListing> {
        let generation = {
            let dirs = self.dirs.read().unwrap();
            if let Some(l) = dirs.listings.get(dir) {
                return l.clone();
            }
            dirs.generation
        };
        // Include paths with directories (`include "sub/x.svh"`) name a missing directory in
        // most include directories: the parent's listing tells without opening it.
        let in_parent = self.entry_kind(dir);
        // Read without the lock; two threads may both read a directory the first time.
        let l = Arc::new(match in_parent {
            Some(EntryKind::Dir | EntryKind::Unknown) => DirListing::read(dir),
            Some(EntryKind::File) | None => DirListing::Missing,
        });
        let mut dirs = self.dirs.write().unwrap();
        if dirs.generation != generation {
            // A change on disk was reported meanwhile: good for this check (like a `stat` racing
            // the change), but not to keep.
            return l;
        }
        dirs.listings.entry(dir.to_path_buf()).or_insert(l).clone()
    }

    pub fn file_count(&self) -> usize {
        self.inner.read().unwrap().paths.len()
    }

    pub fn is_builtin(&self, id: FileId) -> bool {
        self.path(id).starts_with(BUILTIN_DIR)
    }
}

/// Paths that differ in case only, by their lowercase form (Windows and macOS, whose file
/// systems usually ignore case).
fn fold(p: &Path) -> Option<PathBuf> {
    cfg!(any(windows, target_os = "macos")).then(|| PathBuf::from(p.to_string_lossy().to_lowercase()))
}

/// Two spellings of one existing file: `Defs.svh` and `defs.svh` where the folder ignores
/// case. A folder that does not (a case-sensitive one) has two files, kept apart.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

impl Inner {
    /// The ids and paths `p` may be another spelling of (the same path ignoring case).
    fn spellings(&self, p: &Path) -> Vec<(FileId, PathBuf)> {
        let ids = fold(p).and_then(|f| self.folded.get(&f));
        ids.into_iter().flatten().map(|id| (*id, self.paths[*id as usize].clone())).collect()
    }
}

/// Which of `candidates` names the same file as `p`, asking the file system (without a lock
/// held: it may be slow, on a network share).
fn same_as(candidates: &[(FileId, PathBuf)], p: &Path) -> Option<FileId> {
    candidates.iter().find(|(_, q)| same_file(q, p)).map(|(id, _)| *id)
}

impl SourceProvider for Vfs {
    fn file_id(&self, path: &Path) -> FileId {
        let p = normalize(path);
        let mut candidates = {
            let g = self.inner.read().unwrap();
            if let Some(id) = g.ids.get(&p) {
                return *id;
            }
            g.spellings(&p)
        };
        // An `include "defs.svh"` and the editor's `Defs.svh` must share the file's text. The
        // file system is asked without the lock held; a spelling another thread added meanwhile
        // is asked about too, before this one gets an id of its own.
        let mut g = loop {
            let other = same_as(&candidates, &p);
            let mut g = self.inner.write().unwrap();
            if let Some(id) = g.ids.get(&p) {
                return *id;
            }
            if let Some(id) = other {
                g.ids.insert(p, id);
                return id;
            }
            let now = g.spellings(&p);
            if now.len() == candidates.len() {
                break g;
            }
            drop(g);
            candidates = now;
        };
        let id = g.paths.len() as FileId;
        if let Some(f) = fold(&p) {
            g.folded.entry(f).or_default().push(id);
        }
        g.paths.push(p.clone());
        g.ids.insert(p, id);
        g.entries.push(Entry::default());
        id
    }

    fn path(&self, id: FileId) -> PathBuf {
        self.inner.read().unwrap().paths.get(id as usize).cloned().unwrap_or_default()
    }

    fn text(&self, id: FileId) -> Option<Arc<str>> {
        self.text_versioned(id).map(|(t, _)| t)
    }

    fn source(&self, id: FileId) -> Option<(Arc<str>, Arc<Vec<Token>>)> {
        {
            let g = self.inner.read().unwrap();
            let e = g.entries.get(id as usize)?;
            if let Some((v, text, toks)) = &e.tokens {
                if *v == e.version {
                    return Some((text.clone(), toks.clone()));
                }
            }
        }
        let (text, version) = self.text_versioned(id)?;
        let toks = Arc::new(lex(&text, id));
        let mut g = self.inner.write().unwrap();
        let e = &mut g.entries[id as usize];
        if e.version == version {
            e.tokens = Some((version, text.clone(), toks.clone()));
        }
        Some((text, toks))
    }

    fn is_file(&self, path: &Path) -> bool {
        if let Some(id) = self.lookup(path) {
            let g = self.inner.read().unwrap();
            if g.entries[id as usize].overlay.is_some() {
                return true;
            }
        }
        self.is_file_on_disk(path)
    }
}

impl Vfs {
    fn is_file_on_disk(&self, path: &Path) -> bool {
        // `dir/..` resolves only if `dir` exists (on Windows, the system drops both first).
        let mut prefix = PathBuf::new();
        for c in path.components() {
            if c == Component::ParentDir && !cfg!(windows) {
                match self.entry_kind(&normalize(&prefix)) {
                    Some(EntryKind::Dir) => {}
                    Some(EntryKind::File) | None => return false,
                    Some(EntryKind::Unknown) => return path.is_file(),
                }
            }
            prefix.push(c);
        }
        // The normalized path is the one the file is then read from (see `file_id`).
        let p = normalize(path);
        match self.entry_kind(&p) {
            Some(EntryKind::File) => true,
            Some(EntryKind::Dir) | None => false,
            Some(EntryKind::Unknown) => p.is_file(),
        }
    }
}

/// The editor texts of the open files at one moment ([`Vfs::pin`]). An update reads these,
/// not the newer ones the editor sends while it runs: its snapshot is one consistent state of
/// the editor, and each text keeps its document version.
pub struct Pin {
    overlays: FxHashMap<FileId, (Arc<str>, Option<i32>)>,
    /// Tokens of texts the store no longer has (the editor's newer text, or a file opened
    /// after the pin, whose disk text counts): lexed once for all the units that include them.
    tokens: Mutex<FxHashMap<FileId, (Arc<str>, Arc<Vec<Token>>)>>,
}

/// The store read through a [`Pin`], or as it is now without one.
pub struct Reader<'a> {
    vfs: &'a Vfs,
    pin: Option<&'a Pin>,
}

impl Reader<'_> {
    /// The editor version of `text`, when it is the editor text this reader reads.
    pub fn lsp_version_for(&self, id: FileId, text: &Arc<str>) -> Option<i32> {
        match self.pin {
            None => self.vfs.lsp_version_for(id, text),
            Some(p) => p.overlays.get(&id).filter(|(o, _)| Arc::ptr_eq(o, text)).and_then(|(_, v)| *v),
        }
    }

    /// For a pin: the text it says the file has (the disk text of a file that was not open
    /// then, also when it is opened while the reader reads); `None` without a pin.
    fn pinned_text(&self, id: FileId) -> Option<Option<Arc<str>>> {
        let p = self.pin?;
        Some(match p.overlays.get(&id) {
            Some((o, _)) => Some(o.clone()),
            None => self.vfs.disk_text(id),
        })
    }
}

impl SourceProvider for Reader<'_> {
    fn file_id(&self, path: &Path) -> FileId {
        self.vfs.file_id(path)
    }

    fn path(&self, id: FileId) -> PathBuf {
        self.vfs.path(id)
    }

    fn text(&self, id: FileId) -> Option<Arc<str>> {
        match self.pinned_text(id) {
            Some(t) => t,
            None => self.vfs.text(id),
        }
    }

    fn source(&self, id: FileId) -> Option<(Arc<str>, Arc<Vec<Token>>)> {
        let (Some(text), Some(pin)) = (self.pinned_text(id), self.pin) else { return self.vfs.source(id) };
        let text = text?;
        // The store's tokens while it has the same text.
        if let Some((t, toks)) = self.vfs.source(id).filter(|(t, _)| Arc::ptr_eq(t, &text)) {
            return Some((t, toks));
        }
        let mut cache = pin.tokens.lock().unwrap();
        if let Some((t, toks)) = cache.get(&id).filter(|(t, _)| Arc::ptr_eq(t, &text)) {
            return Some((t.clone(), toks.clone()));
        }
        let toks = Arc::new(lex(&text, id));
        cache.insert(id, (text.clone(), toks.clone()));
        Some((text, toks))
    }

    fn is_file(&self, path: &Path) -> bool {
        let Some(pin) = self.pin else { return self.vfs.is_file(path) };
        self.vfs.lookup(path).is_some_and(|id| pin.overlays.contains_key(&id)) || self.vfs.is_file_on_disk(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_keeps_the_editor_texts_and_versions_of_its_moment() {
        let root = std::env::temp_dir().join(format!("dvicode-vfs-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("b.sv"), "on disk").unwrap();
        let vfs = Vfs::new();
        let (a, b, c) = (root.join("a.sv"), root.join("b.sv"), root.join("c.sv"));
        let ia = vfs.set_overlay_versioned(&a, Arc::from("a v1"), Some(1));
        let pin = vfs.pin();
        // Edited, opened and created after the pin.
        vfs.set_overlay_versioned(&a, Arc::from("a v2"), Some(2));
        let ib = vfs.set_overlay_versioned(&b, Arc::from("b in the editor"), Some(7));
        let ic = vfs.set_overlay_versioned(&c, Arc::from("c in the editor"), Some(1));
        let r = vfs.reader(Some(&pin));
        let (ta, toks) = r.source(ia).unwrap();
        assert_eq!((&*ta, toks[0].text.as_str()), ("a v1", "a"));
        assert_eq!(r.lsp_version_for(ia, &ta), Some(1));
        assert_eq!(&*r.text(ib).unwrap(), "on disk");
        assert_eq!(r.lsp_version_for(ib, &r.text(ib).unwrap()), None);
        assert!(r.text(ic).is_none() && !r.is_file(&c) && r.is_file(&b) && r.is_file(&a));
        // Without a pin: the texts of now.
        let now = vfs.reader(None);
        let ta2 = now.text(ia).unwrap();
        assert_eq!((&*ta2, now.lsp_version_for(ia, &ta2)), ("a v2", Some(2)));
        assert!(now.is_file(&c));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn caches_follow_the_latest_text() {
        let vfs = Vfs::new();
        let p = Path::new("/__dvicode_test__/a.sv");
        let id = vfs.set_overlay_versioned(p, Arc::from("module a;"), Some(1));
        let (t1, v1) = vfs.text_versioned(id).unwrap();
        assert_eq!(&*t1, "module a;");
        let (st1, toks1) = vfs.source(id).unwrap();
        assert_eq!(toks1[1].text, "a");
        assert_eq!(vfs.lsp_version_for(id, &st1), Some(1));
        vfs.set_overlay_versioned(p, Arc::from("module bb;\nendmodule"), Some(2));
        let (t2, v2) = vfs.text_versioned(id).unwrap();
        assert!(v2 > v1);
        assert_eq!(&*t2, "module bb;\nendmodule");
        let (st2, toks2) = vfs.source(id).unwrap();
        assert_eq!(toks2[1].text, "bb");
        assert_eq!(&*st2, &*t2);
        // The old text is no longer the editor's text.
        assert_eq!(vfs.lsp_version_for(id, &st1), None);
        assert_eq!(vfs.line_index(id).unwrap().line_count(), 2);
    }

    #[test]
    fn file_checks_see_changes_reported_on_disk() {
        let root = std::env::temp_dir().join(format!("dvicode-vfs-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("inc")).unwrap();
        std::fs::write(root.join("inc/a.svh"), "").unwrap();
        let vfs = Vfs::new();
        let (a, b, c) = (root.join("inc/a.svh"), root.join("inc/b.svh"), root.join("inc/sub/c.svh"));
        assert!(vfs.is_file(&a));
        assert!(vfs.is_file(&root.join("inc/./../inc/a.svh")));
        assert_eq!(vfs.is_file(&root.join("inc/x/../a.svh")), cfg!(windows));
        assert!(!vfs.is_file(&root.join("inc")));
        assert!(!vfs.is_file(&b));
        assert!(!vfs.is_file(&c));
        assert!(!vfs.is_file(&root.join("inc/a.svh/d.svh")));
        // Listings are cached until a change is reported...
        std::fs::write(&b, "").unwrap();
        std::fs::create_dir_all(root.join("inc/sub")).unwrap();
        std::fs::write(&c, "").unwrap();
        assert!(!vfs.is_file(&b));
        vfs.invalidate(&b);
        assert!(vfs.is_file(&b));
        // ... also for a file in a new directory, which its parent's listing did not have.
        assert!(!vfs.is_file(&c));
        vfs.invalidate(&c);
        assert!(vfs.is_file(&c));
        std::fs::remove_file(&a).unwrap();
        vfs.invalidate(&a);
        assert!(!vfs.is_file(&a));
        // Or until the next build.
        std::fs::remove_dir_all(root.join("inc/sub")).unwrap();
        assert!(vfs.is_file(&c));
        vfs.clear_dir_cache();
        assert!(!vfs.is_file(&c));
        // Editor contents count as a file.
        vfs.set_overlay(&a, Arc::from(""));
        assert!(vfs.is_file(&a));
        let _ = std::fs::remove_dir_all(&root);
    }
}
