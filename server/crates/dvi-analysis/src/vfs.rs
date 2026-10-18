//! In-memory view of source files: disk contents, editor overlays, lexing cache.

use dvi_syntax::lexer::lex;
use dvi_syntax::preproc::normalize;
use dvi_syntax::{FileId, LineIndex, SourceProvider, Token};
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Default)]
struct Entry {
    disk: Option<Arc<str>>,
    disk_loaded: bool,
    overlay: Option<Arc<str>>,
    version: u64,
    tokens: Option<(u64, Arc<Vec<Token>>)>,
    lines: Option<(u64, Arc<LineIndex>)>,
}

#[derive(Default)]
struct Inner {
    paths: Vec<PathBuf>,
    ids: FxHashMap<PathBuf, FileId>,
    entries: Vec<Entry>,
}

/// Thread-safe file store shared by all analysis snapshots.
#[derive(Default)]
pub struct Vfs {
    inner: RwLock<Inner>,
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
        let id = self.file_id(path);
        let mut g = self.inner.write().unwrap();
        let e = &mut g.entries[id as usize];
        e.overlay = Some(text);
        e.version += 1;
        id
    }

    pub fn clear_overlay(&self, path: &Path) {
        if let Some(id) = self.lookup(path) {
            let mut g = self.inner.write().unwrap();
            let e = &mut g.entries[id as usize];
            if e.overlay.take().is_some() {
                e.version += 1;
            }
        }
    }

    pub fn has_overlay(&self, id: FileId) -> bool {
        self.inner.read().unwrap().entries.get(id as usize).is_some_and(|e| e.overlay.is_some())
    }

    /// Forgets cached disk contents (after a change on disk).
    pub fn invalidate(&self, path: &Path) -> Option<FileId> {
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

    pub fn line_index(&self, id: FileId) -> Option<Arc<LineIndex>> {
        {
            let g = self.inner.read().unwrap();
            let e = g.entries.get(id as usize)?;
            if let Some((v, li)) = &e.lines {
                if *v == e.version && (e.overlay.is_some() || e.disk_loaded) {
                    return Some(li.clone());
                }
            }
        }
        let text = self.text(id)?;
        let li = Arc::new(LineIndex::new(&text));
        let mut g = self.inner.write().unwrap();
        let e = &mut g.entries[id as usize];
        e.lines = Some((e.version, li.clone()));
        Some(li)
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
        let path = {
            let g = self.inner.read().unwrap();
            let e = g.entries.get(id as usize)?;
            if let Some(o) = &e.overlay {
                return Some(o.clone());
            }
            if e.disk_loaded {
                return e.disk.clone();
            }
            g.paths[id as usize].clone()
        };
        let text: Option<Arc<str>> = std::fs::read(&path).ok().map(|b| match String::from_utf8(b) {
            Ok(s) => Arc::from(s),
            Err(e) => Arc::from(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        });
        let mut g = self.inner.write().unwrap();
        let e = &mut g.entries[id as usize];
        if e.overlay.is_some() {
            return e.overlay.clone();
        }
        e.disk = text.clone();
        e.disk_loaded = true;
        text
    }

    fn tokens(&self, id: FileId) -> Option<Arc<Vec<Token>>> {
        {
            let g = self.inner.read().unwrap();
            let e = g.entries.get(id as usize)?;
            if let Some((v, t)) = &e.tokens {
                if *v == e.version {
                    return Some(t.clone());
                }
            }
        }
        let text = self.text(id)?;
        let version = self.version(id);
        let toks = Arc::new(lex(&text, id));
        let mut g = self.inner.write().unwrap();
        let e = &mut g.entries[id as usize];
        if e.version == version {
            e.tokens = Some((version, toks.clone()));
        }
        Some(toks)
    }

    fn is_file(&self, path: &Path) -> bool {
        if let Some(id) = self.lookup(path) {
            let g = self.inner.read().unwrap();
            if g.entries[id as usize].overlay.is_some() {
                return true;
            }
        }
        path.is_file()
    }
}
