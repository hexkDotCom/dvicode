//! In-memory view of source files: disk contents, editor overlays, lexing cache.
//!
//! The store is shared and mutable; analysis snapshots copy what they need (see
//! [`crate::world::Source`]) so that a snapshot never mixes its syntax with newer text.

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
        path.is_file()
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
}
