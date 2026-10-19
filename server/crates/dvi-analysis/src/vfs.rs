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
use std::sync::{Arc, RwLock};

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
    ids: FxHashMap<PathBuf, FileId>,
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
        self.inner.read().unwrap().ids.get(&p).copied()
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
        let id = self.lookup(path)?;
        let mut g = self.inner.write().unwrap();
        let e = &mut g.entries[id as usize];
        e.disk = None;
        e.disk_loaded = false;
        e.version += 1;
        Some(id)
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
        loop {
            let (path, version) = {
                let g = self.inner.read().unwrap();
                let e = g.entries.get(id as usize)?;
                if let Some(o) = &e.overlay {
                    return Some((o.clone(), e.version));
                }
                if e.disk_loaded {
                    return e.disk.clone().map(|t| (t, e.version));
                }
                (g.paths[id as usize].clone(), e.version)
            };
            let text: Option<Arc<str>> = std::fs::read(&path).ok().map(|b| match String::from_utf8(b) {
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

impl SourceProvider for Vfs {
    fn file_id(&self, path: &Path) -> FileId {
        let p = normalize(path);
        if let Some(id) = self.inner.read().unwrap().ids.get(&p) {
            return *id;
        }
        let mut g = self.inner.write().unwrap();
        if let Some(id) = g.ids.get(&p) {
            return *id;
        }
        let id = g.paths.len() as FileId;
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

#[cfg(test)]
mod tests {
    use super::*;

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
