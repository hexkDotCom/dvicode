//! IDE features on top of an [`Analysis`] snapshot.

pub mod actions;
pub mod calls;
pub mod completion;
pub mod diagnostics;
pub mod hover;
pub mod nav;
pub mod symbols;
pub mod tokens;
pub mod views;

use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{DeclId, RefId};
use dvi_syntax::preproc::MacroDef;
use dvi_syntax::{FileId, SourceProvider, Span};
use lsp_types::{Location, Position, Range, Url};
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
    Include { unit: UnitId, idx: usize, span: Span },
}

impl Hit {
    pub fn span(&self) -> Span {
        match self {
            Hit::Ref { span, .. } | Hit::Decl { span, .. } | Hit::MacroUse { span, .. } | Hit::Include { span, .. } => *span,
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
        let text = self.vfs.text(span.file)?;
        let li = self.vfs.line_index(span.file)?;
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
        let text = self.vfs.text(file)?;
        let li = self.vfs.line_index(file)?;
        Some(li.offset(&text, pos.line, pos.character))
    }

    pub fn file_of_url(&self, url: &Url) -> Option<FileId> {
        self.vfs.lookup(&path_of(url)?)
    }

    /// Units containing `file`, project units first.
    pub fn units_for(&self, file: FileId) -> Vec<UnitId> {
        let mut v: Vec<UnitId> = self.units_of_file(file).to_vec();
        v.sort_by_key(|u| (self.try_unit(*u).map_or(9, |u| match u.kind {
            UnitKind::Project => 0,
            UnitKind::AdHoc => 1,
            UnitKind::Builtin => 2,
        }), *u));
        v
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
            Hit::Include { unit, idx, .. } => self.unit(*unit).includes[*idx].resolved.map(Target::File),
        }
    }

    pub fn target_at(&self, file: FileId, off: u32) -> Option<Target> {
        let h = self.hit_at(file, off)?;
        self.target_of(&h)
    }

    pub fn decl_location(&self, r: DeclRef) -> Option<Location> {
        self.to_location(self.decl(r).name_span)
    }

    pub fn file_text(&self, file: FileId) -> Option<Arc<str>> {
        self.vfs.text(file)
    }
}
