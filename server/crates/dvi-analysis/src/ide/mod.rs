//! IDE features on top of an [`Analysis`] snapshot.

pub mod actions;
pub mod calls;
pub mod completion;
pub mod diagnostics;
pub mod factory;
pub mod filter;
pub mod hover;
pub mod loops;
pub mod nav;
pub mod stmt;
pub mod structure;
pub mod symbols;
pub mod tokens;
pub mod views;

use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{DeclId, RefId};
use dvi_syntax::preproc::MacroDef;
use dvi_syntax::{FileId, SourceProvider, Span};
use lsp_types::{Location, Position, Range, Url};
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn url_of(path: &Path) -> Url {
    Url::from_file_path(path).unwrap_or_else(|_| Url::parse("file:///").unwrap())
}

pub fn path_of(url: &Url) -> Option<PathBuf> {
    url.to_file_path().ok()
}

/// What the cursor is on.
#[derive(Clone, Debug)]
pub enum Hit {
    Ref { unit: UnitId, rid: RefId, seg: usize, span: Span },
    Decl { unit: UnitId, decl: DeclId, span: Span },
    MacroUse { unit: UnitId, idx: usize, span: Span },
    /// A macro name in `` `ifdef `` and friends.
    MacroName { unit: UnitId, idx: usize, span: Span },
    /// The name in a `` `define ``.
    MacroDef { unit: UnitId, idx: usize, span: Span },
    Include { unit: UnitId, idx: usize, span: Span },
}

impl Hit {
    pub fn span(&self) -> Span {
        match self {
            Hit::Ref { span, .. } | Hit::Decl { span, .. } | Hit::MacroUse { span, .. } | Hit::MacroName { span, .. } | Hit::MacroDef { span, .. } | Hit::Include { span, .. } => *span,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Target {
    Decl(DeclRef),
    Macro(Arc<MacroDef>),
    File(FileId),
}

impl Analysis {
    pub fn to_range(&self, span: Span) -> Option<Range> {
        let src = self.source(span.file)?;
        let (text, li) = (&src.text, src.lines());
        let (l1, c1) = li.position(&text, span.start);
        let (l2, c2) = li.position(&text, span.end);
        Some(Range::new(Position::new(l1, c1), Position::new(l2, c2)))
    }

    pub fn to_location(&self, span: Span) -> Option<Location> {
        if self.vfs.is_builtin(span.file) || span.file == dvi_syntax::preproc::COMMAND_LINE_FILE {
            return None;
        }
        Some(Location::new(url_of(&self.vfs.path(span.file)), self.to_range(span)?))
    }

    pub fn offset_at(&self, file: FileId, pos: Position) -> Option<u32> {
        let src = self.source(file)?;
        let (text, li) = (&src.text, src.lines());
        Some(li.offset(&text, pos.line, pos.character))
    }

    pub fn file_of_url(&self, url: &Url) -> Option<FileId> {
        self.vfs.lookup(&path_of(url)?)
    }

    /// Units containing `file`: project units first (in compilation order), then the file's
    /// own ad-hoc unit, then other open files' units by path (not by when they were opened).
    pub fn units_for(&self, file: FileId) -> Vec<UnitId> {
        // Most files are in project units only: their order is their ids' (a header that
        // thousands of files include is asked for at every request).
        let ids = self.units_of_file(file);
        if ids.iter().all(|id| self.try_unit(*id).is_some_and(|u| u.kind == UnitKind::Project)) {
            let mut v = ids.to_vec();
            v.sort_unstable();
            return v;
        }
        let mut v: Vec<((u8, UnitId, PathBuf), UnitId)> = self
            .units_of_file(file)
            .iter()
            .filter_map(|id| self.try_unit(*id))
            .map(|u| {
                let key = match u.kind {
                    UnitKind::Project => (0, u.id, PathBuf::new()),
                    UnitKind::AdHoc if u.root == file => (1, 0, PathBuf::new()),
                    UnitKind::AdHoc => (2, 0, self.vfs.path(u.root)),
                    UnitKind::Builtin => (3, u.id, PathBuf::new()),
                };
                (key, u.id)
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, id)| id).collect()
    }

    /// The navigable element at `off` in `file`.
    pub fn hit_at(&self, file: FileId, off: u32) -> Option<Hit> {
        for uid in self.units_for(file) {
            let u = self.unit(uid);
            if let Some(pos) = u.positions.get(&file) {
                // References: innermost span containing the offset.
                let idx = pos.refs.partition_point(|(s, _, _)| s.start <= off);
                let mut best: Option<&(Span, RefId, u16)> = None;
                for e in pos.refs[..idx].iter().rev().take(64) {
                    if e.0.contains(off) && best.map_or(true, |b| e.0.len() < b.0.len()) {
                        best = Some(e);
                    }
                    if e.0.end + 512 < off {
                        break;
                    }
                }
                if let Some((s, rid, seg)) = best {
                    return Some(Hit::Ref { unit: uid, rid: *rid, seg: *seg as usize, span: *s });
                }
                let di = pos.decls.partition_point(|d| u.parsed.decl(*d).name_span.start <= off);
                for d in pos.decls[..di].iter().rev().take(8) {
                    let ns = u.parsed.decl(*d).name_span;
                    if ns.contains(off) {
                        return Some(Hit::Decl { unit: uid, decl: *d, span: ns });
                    }
                }
            }
            for (i, m) in u.macro_uses.iter().enumerate() {
                if m.name_span.file == file && m.name_span.contains(off) {
                    return Some(Hit::MacroUse { unit: uid, idx: i, span: m.name_span });
                }
            }
            for (i, m) in u.macro_names.iter().enumerate() {
                if m.span.file == file && m.span.contains(off) {
                    return Some(Hit::MacroName { unit: uid, idx: i, span: m.span });
                }
            }
            for (i, d) in u.macro_defs.iter().enumerate() {
                if d.name_span.file == file && d.name_span.contains(off) {
                    return Some(Hit::MacroDef { unit: uid, idx: i, span: d.name_span });
                }
            }
            for (i, inc) in u.includes.iter().enumerate() {
                if inc.span.file == file && inc.span.contains(off) {
                    return Some(Hit::Include { unit: uid, idx: i, span: inc.span });
                }
            }
        }
        None
    }

    pub fn target_of(&self, hit: &Hit) -> Option<Target> {
        match hit {
            Hit::Ref { unit, rid, seg, .. } => self.resolve_ref(*unit, *rid, *seg).map(Target::Decl),
            Hit::Decl { unit, decl, .. } => Some(Target::Decl(DeclRef { unit: *unit, decl: *decl })),
            Hit::MacroUse { unit, idx, .. } => self.unit(*unit).macro_uses[*idx].def.clone().map(Target::Macro),
            Hit::MacroDef { unit, idx, .. } => Some(Target::Macro(self.unit(*unit).macro_defs[*idx].clone())),
            // `ifndef GUARD before `define GUARD: the only definition of that name, if there is one.
            Hit::MacroName { unit, idx, .. } => {
                let m = &self.unit(*unit).macro_names[*idx];
                m.def.clone().or_else(|| self.unique_macro(&m.name)).map(Target::Macro)
            }
            Hit::Include { unit, idx, .. } => self.unit(*unit).includes[*idx].resolved.map(Target::File),
        }
    }

    /// The definition of a macro when all its definitions are the same one.
    pub fn unique_macro(&self, name: &str) -> Option<Arc<MacroDef>> {
        let defs = self.global.macros.get(name)?;
        let first = defs.first()?;
        defs.iter().all(|d| d.name_span == first.name_span).then(|| first.clone())
    }

    pub fn target_at(&self, file: FileId, off: u32) -> Option<Target> {
        let h = self.hit_at(file, off)?;
        self.target_of(&h)
    }

    pub fn decl_location(&self, r: DeclRef) -> Option<Location> {
        self.to_location(self.decl(r).name_span)
    }

    /// Edits as a workspace edit in which every document carries the version this snapshot
    /// saw, so the editor refuses them if the document changed since the request was made.
    pub fn versioned_edit(&self, edits: Vec<(Span, String)>) -> Option<lsp_types::WorkspaceEdit> {
        use lsp_types::{DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier, TextDocumentEdit, TextEdit};
        let mut by_file: Vec<(FileId, Vec<OneOf<TextEdit, lsp_types::AnnotatedTextEdit>>)> = Vec::new();
        let mut index: FxHashMap<FileId, usize> = FxHashMap::default();
        for (span, text) in edits {
            let range = self.to_location(span)?.range;
            let e = OneOf::Left(TextEdit::new(range, text));
            // Files in the order of their first edit, found by a map (a rename can touch
            // thousands).
            let k = *index.entry(span.file).or_insert_with(|| {
                by_file.push((span.file, Vec::new()));
                by_file.len() - 1
            });
            by_file[k].1.push(e);
        }
        let docs = by_file
            .into_iter()
            .map(|(f, edits)| TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier {
                    uri: url_of(&self.vfs.path(f)),
                    version: self.source(f).and_then(|s| s.lsp_version),
                },
                edits,
            })
            .collect();
        Some(lsp_types::WorkspaceEdit { changes: None, document_changes: Some(DocumentChanges::Edits(docs)), change_annotations: None })
    }

    /// Whether edits (rename) may change a file: the project's own files, under the
    /// workspace root, and documents open in the editor; never built-in files or libraries
    /// outside the workspace, such as the UVM sources.
    pub fn is_editable(&self, file: FileId) -> bool {
        if self.vfs.is_builtin(file) || file == dvi_syntax::preproc::COMMAND_LINE_FILE {
            return false;
        }
        if self.vfs.has_overlay(file) {
            return true;
        }
        let (p, root) = (self.vfs.path(file), &self.setup.root);
        let exact = p.starts_with(root);
        // Drive letters and folder names may differ in case on Windows.
        let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
        if !(exact || cfg!(windows) && lower(&p).starts_with(lower(root))) {
            return false;
        }
        // A link in the folder to a file outside it (shared IP) is not the folder's to change:
        // look for one below the root (each name exists, so a file that cannot be found is
        // not edited).
        let n = root.components().count();
        let mut cur: PathBuf = p.components().take(n).collect();
        let mut linked = false;
        for c in p.components().skip(n) {
            cur.push(c);
            match std::fs::symlink_metadata(&cur) {
                Ok(m) => linked |= m.file_type().is_symlink(),
                Err(_) => return false,
            }
        }
        if exact && !linked {
            return true;
        }
        // Through a link, or spelt in another case (a folder where case matters may hold both
        // `Work` and `work`): where it really is. Canonical paths have the names' real case.
        match (std::fs::canonicalize(&p), std::fs::canonicalize(root)) {
            (Ok(p), Ok(root)) => p.starts_with(root),
            _ => false,
        }
    }

    /// The snapshot text of a file (see [`Analysis::source`]).
    pub fn file_text(&self, file: FileId) -> Option<Arc<str>> {
        self.source(file).map(|s| s.text.clone())
    }
}
