//! `dvicode-server check DIR --write-check N`: rename and quick fixes on real code, applied
//! the way the editor applies them.
//!
//! Renames names at sampled positions (macros, identifiers in macro arguments, names in
//! headers, any name) and applies the quick fixes for problems it makes on purpose (a
//! misspelled name, a removed `import`). Every workspace edit is applied like the editor does:
//! each document must carry the version of the open document (or none for a closed file), and
//! the ranges must not overlap. The result is compiled again from scratch and checked:
//!
//! - rename: no new problems, the references of the renamed element are exactly the edited
//!   places, only the old name was replaced, no file outside the workspace (or built-in) was
//!   touched, and no use of the element inside a macro body was left behind;
//! - quick fixes: "Did you mean" restores the misspelled name exactly, "Import package" brings
//!   the file's problems back to what they were with the import, and inserted lines use the
//!   file's line endings.

use crate::fuzz::{disk_text, guarded, is_ident_byte, rel, word_offsets, Rng};
use dvi_analysis::ide::Target;
use dvi_analysis::{Analysis, ProjectSetup, UnitKind, Vfs};
use dvi_syntax::{lexer, FileId, LineIndex, SourceProvider, TokKind};
use lsp_types::{DocumentChanges, OneOf, WorkspaceEdit};
use rustc_hash::FxHashSet;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Options {
    /// Renames to make (quick fixes: half as many of each kind).
    pub count: usize,
    pub seed: u64,
    pub verbose: bool,
}

#[derive(Default)]
pub struct Report {
    pub renames: usize,
    /// Renames refused: they would change files outside the workspace, or a name is used
    /// inside macro bodies.
    pub refused: usize,
    pub fixes: usize,
    /// Wrong results.
    pub findings: Vec<String>,
    /// Missing quick fixes and other things worth a look that are not wrong results.
    pub notes: Vec<String>,
}

#[derive(Clone)]
struct Doc {
    text: Arc<str>,
    version: Option<i32>,
}

/// A full build with `docs` as the editor's open documents.
fn build(setup: &Arc<ProjectSetup>, docs: &BTreeMap<PathBuf, Doc>) -> Analysis {
    let vfs = Arc::new(Vfs::new());
    let mut open: Vec<FileId> = docs.iter().map(|(p, d)| vfs.set_overlay_versioned(p, d.text.clone(), d.version)).collect();
    open.sort_unstable();
    Analysis::build(vfs, setup.clone()).sync_open_files(&open)
}

type Problems = BTreeMap<(PathBuf, u32, &'static str), usize>;

/// Problems of every compiled file: (file, line, code) -> count.
fn problems(a: &Analysis) -> Problems {
    let mut out = Problems::new();
    for f in a.project_files() {
        if a.vfs.is_builtin(f) {
            continue;
        }
        let p = a.vfs.path(f);
        for d in a.file_diagnostics(f) {
            let line = a.to_range(d.span).map_or(0, |r| r.start.line);
            *out.entry((p.clone(), line, d.code)).or_default() += 1;
        }
    }
    out
}

/// Problems in `after` that `before` does not have.
fn new_problems(root: &Path, before: &Problems, after: &Problems) -> Vec<String> {
    after
        .iter()
        .filter(|(k, n)| before.get(*k).copied().unwrap_or(0) < **n)
        .take(6)
        .map(|((p, l, c), _)| format!("{}:{} {c}", rel(root, p), l + 1))
        .collect()
}

/// One document as changed by a workspace edit.
struct Applied {
    path: PathBuf,
    before: Arc<str>,
    after: String,
    /// Byte ranges in `before` and their new text, in order.
    edits: Vec<(usize, usize, String)>,
}

/// Applies a workspace edit as the editor does. Errors are what the editor would refuse, or
/// what it must never be asked to do.
fn apply(edit: &WorkspaceEdit, docs: &BTreeMap<PathBuf, Doc>) -> Result<Vec<Applied>, String> {
    if edit.changes.is_some() {
        return Err("an edit without document versions".into());
    }
    let Some(DocumentChanges::Edits(list)) = &edit.document_changes else { return Err("no document edits".into()) };
    let mut out: Vec<Applied> = Vec::new();
    for d in list {
        let path = d.text_document.uri.to_file_path().map_err(|_| format!("not a file: {}", d.text_document.uri))?;
        if out.iter().any(|a| a.path == path) {
            return Err(format!("{} edited twice", path.display()));
        }
        let (text, version) = match docs.get(&path) {
            Some(doc) => (doc.text.clone(), doc.version),
            None => (disk_text(&path), None),
        };
        if d.text_document.version != version {
            return Err(format!("{}: edit for version {:?}, the document has {:?}", path.display(), d.text_document.version, version));
        }
        let li = LineIndex::new(&text);
        let mut edits: Vec<(usize, usize, String)> = d
            .edits
            .iter()
            .map(|e| {
                let t = match e {
                    OneOf::Left(t) => t,
                    OneOf::Right(a) => &a.text_edit,
                };
                let s = li.offset(&text, t.range.start.line, t.range.start.character) as usize;
                let e = li.offset(&text, t.range.end.line, t.range.end.character) as usize;
                (s, e, t.new_text.clone())
            })
            .collect();
        edits.sort_by_key(|e| (e.0, e.1));
        if edits.windows(2).any(|w| w[0].1 > w[1].0) {
            return Err(format!("{}: overlapping edits", path.display()));
        }
        let mut after = text.to_string();
        for (s, e, t) in edits.iter().rev() {
            if *e > after.len() || !after.is_char_boundary(*s) || !after.is_char_boundary(*e) {
                return Err(format!("{}: edit outside the text", path.display()));
            }
            after.replace_range(*s..*e, t);
        }
        out.push(Applied { path, before: text, after, edits });
    }
    Ok(out)
}

/// The documents after applying: every changed file is open in the editor with a new version.
fn docs_after(docs: &BTreeMap<PathBuf, Doc>, applied: &[Applied]) -> BTreeMap<PathBuf, Doc> {
    let mut d = docs.clone();
    for a in applied {
        let version = docs.get(&a.path).and_then(|x| x.version).map_or(1, |v| v + 1);
        d.insert(a.path.clone(), Doc { text: Arc::from(a.after.as_str()), version: Some(version) });
    }
    d
}

/// Line endings of a text: (CRLF, lone LF, lone CR).
fn endings(t: &str) -> (usize, usize, usize) {
    let crlf = t.matches("\r\n").count();
    (crlf, t.matches('\n').count() - crlf, t.matches('\r').count() - crlf)
}

/// Where an offset of the old text ends up after the edits.
fn shifted(edits: &[(usize, usize, String)], off: usize) -> usize {
    let mut delta: isize = 0;
    for (s, e, t) in edits {
        if *e <= off && *s < off || *s == off && *e == off {
            delta += t.len() as isize - (*e - *s) as isize;
        } else if *s <= off && off < *e {
            return (*s as isize + delta) as usize + (off - *s).min(t.len());
        }
    }
    (off as isize + delta) as usize
}

struct Session<'a> {
    setup: Arc<ProjectSetup>,
    root: PathBuf,
    rng: Rng,
    docs: BTreeMap<PathBuf, Doc>,
    a0: Analysis,
    p0: Problems,
    report: Report,
    log: &'a mut dyn FnMut(&str),
    verbose: bool,
}

impl Session<'_> {
    fn finding(&mut self, s: String) {
        (self.log)(&format!("write check: FINDING: {s}"));
        self.report.findings.push(s);
    }

    fn note(&mut self, s: String) {
        if self.verbose {
            (self.log)(&format!("write check: note: {s}"));
        }
        self.report.notes.push(s);
    }

    fn at(&self, a: &Analysis, f: FileId, off: u32) -> String {
        let r = a.to_range(dvi_syntax::Span::new(f, off, off)).unwrap_or_default();
        format!("{}:{}:{}", rel(&self.root, &a.vfs.path(f)), r.start.line + 1, r.start.character + 1)
    }

    /// Renames the element at `off` in `f` and checks the result.
    fn rename(&mut self, f: FileId, off: u32, kind: &str, done: &mut FxHashSet<(PathBuf, u32)>) -> bool {
        let a0 = self.a0.clone();
        let Some(span) = a0.prepare_rename(f, off) else { return false };
        let text = a0.file_text(f).unwrap();
        let old = text[span.start as usize..span.end as usize].to_string();
        if old.is_empty() || !old.bytes().all(is_ident_byte) {
            return false;
        }
        // Each element once.
        let target = match a0.target_at(f, off) {
            Some(Target::Decl(r)) => a0.decl(r).name_span,
            Some(Target::Macro(m)) => m.name_span,
            _ => return false,
        };
        if !done.insert((a0.vfs.path(target.file), target.start)) {
            return false;
        }
        let new = format!("{old}_rn{}", self.report.renames + self.report.refused + 1);
        let here = self.at(&a0, f, off);
        let what = format!("rename '{old}' ({kind}) at {here} to '{new}'");
        if self.verbose {
            (self.log)(&format!("write check: {what}"));
        }
        let refs: BTreeSet<(PathBuf, u32)> = a0.rename_spans(f, off).iter().map(|s| (a0.vfs.path(s.file), s.start)).collect();
        let edit = match a0.rename(f, off, &new) {
            Ok(e) => e,
            Err(e) if e.contains("outside the workspace") => {
                self.report.refused += 1;
                if refs.iter().all(|(p, _)| p.starts_with(&self.root) || self.docs.contains_key(p)) {
                    self.finding(format!("{what}: refused although every place is in the workspace: {e}"));
                }
                return true;
            }
            Err(e) if e.contains("inside the macro") || e.contains("more than one place") => {
                self.report.refused += 1;
                self.note(format!("{what}: refused: {e}"));
                return true;
            }
            Err(e) => {
                self.finding(format!("{what}: {e}"));
                return true;
            }
        };
        self.report.renames += 1;
        let applied = match apply(&edit, &self.docs) {
            Ok(x) => x,
            Err(e) => {
                self.finding(format!("{what}: the editor would refuse the edit: {e}"));
                return true;
            }
        };
        let mut problems_here = Vec::new();
        let mut edited: BTreeSet<(PathBuf, u32)> = BTreeSet::new();
        for ap in &applied {
            if !ap.path.starts_with(&self.root) && !self.docs.contains_key(&ap.path) {
                problems_here.push(format!("changes {}, outside the workspace", ap.path.display()));
            }
            if ap.path.starts_with(dvi_analysis::vfs::BUILTIN_DIR) {
                problems_here.push(format!("changes the built-in file {}", ap.path.display()));
            }
            // An implicit `.name` port connection becomes explicit: `.new(name)` or `.name(new)`.
            let explicit = [format!("{new}({old})"), format!("{old}({new})")];
            let mut keep = 0;
            for (s, e, t) in &ap.edits {
                let implicit = ap.before[..*s].trim_end().ends_with('.') && !ap.before[*e..].trim_start().starts_with('(');
                if &ap.before[*s..*e] != old.as_str() || !(*t == new || implicit && explicit.contains(t)) {
                    problems_here.push(format!("{}: replaces '{}' with '{t}'", rel(&self.root, &ap.path), &ap.before[*s..*e]));
                }
                if *t != new {
                    keep += 1;
                }
                edited.insert((ap.path.clone(), *s as u32));
            }
            if endings(&ap.before) != endings(&ap.after) {
                problems_here.push(format!("{}: line endings changed", rel(&self.root, &ap.path)));
            }
            // Nothing but the old name changed: as many fewer whole-word occurrences as edits
            // that replace it.
            let n0 = word_offsets(&ap.before, &old, usize::MAX).len();
            let n1 = word_offsets(&ap.after, &old, usize::MAX).len();
            if n0 - n1.min(n0) != ap.edits.len() - keep {
                problems_here.push(format!("{}: '{old}' occurs {n0} times before and {n1} after {} edits", rel(&self.root, &ap.path), ap.edits.len()));
            }
        }
        if edited != refs {
            let missing: Vec<String> = refs.difference(&edited).take(4).map(|(p, o)| format!("{}@{o}", rel(&self.root, p))).collect();
            let extra: Vec<String> = edited.difference(&refs).take(4).map(|(p, o)| format!("{}@{o}", rel(&self.root, p))).collect();
            problems_here.push(format!("edits differ from the references: not edited {missing:?}, edited but not a reference {extra:?}"));
        }
        // Uses inside macro bodies cannot be edited where they are expanded.
        if let Some(Target::Decl(r)) = a0.target_at(f, off) {
            let key = a0.decl_key(a0.canonical(r));
            'units: for u in a0.units.iter().flatten() {
                let Some(occ) = u.names.get(old.as_str()) else { continue };
                for (rid, seg) in occ {
                    let s = &u.parsed.reference(*rid).segs[*seg as usize];
                    if s.from_macro && a0.resolve_ref(u.id, *rid, *seg as usize).is_some_and(|t| a0.decl_key(a0.canonical(t)) == key) {
                        problems_here.push(format!("a use inside a macro body, expanded at {}, keeps the old name", self.at(&a0, s.span.file, s.span.start)));
                        break 'units;
                    }
                }
            }
        }
        // Compile the result from scratch.
        let docs1 = docs_after(&self.docs, &applied);
        let setup = self.setup.clone();
        let a1 = match guarded("build after rename", || build(&setup, &docs1)) {
            Ok(a) => a,
            Err(e) => {
                self.finding(format!("{what}: {e}"));
                return true;
            }
        };
        let p1 = problems(&a1);
        let added = new_problems(&self.root, &self.p0, &p1);
        if !added.is_empty() {
            problems_here.push(format!("new problems: {}", added.join(", ")));
        }
        // The references of the renamed element are the edited places.
        let shifted_of = |p: &Path, o: usize| applied.iter().find(|ap| ap.path == p).map_or(o, |ap| shifted(&ap.edits, o));
        // Where the new name is: after `name(` in `.name(new)`.
        let new_at = |p: &Path, o: usize| {
            let kept = applied.iter().find(|ap| ap.path == p).and_then(|ap| ap.edits.iter().find(|e| e.0 == o)).is_some_and(|e| e.2.starts_with(&format!("{old}(")));
            shifted_of(p, o) + if kept { old.len() + 1 } else { 0 }
        };
        let expected: BTreeSet<(PathBuf, u32)> = edited.iter().map(|(p, o)| (p.clone(), new_at(p, *o as usize) as u32)).collect();
        let path = a0.vfs.path(f);
        if let Some(f1) = a1.vfs.lookup(&path) {
            let off1 = new_at(&path, span.start as usize) as u32;
            let got: BTreeSet<(PathBuf, u32)> = a1.rename_spans(f1, off1).iter().map(|s| (a1.vfs.path(s.file), s.start)).collect();
            if got != expected {
                let missing: Vec<String> = expected.difference(&got).take(4).map(|(p, o)| format!("{}@{o}", rel(&self.root, p))).collect();
                let extra: Vec<String> = got.difference(&expected).take(4).map(|(p, o)| format!("{}@{o}", rel(&self.root, p))).collect();
                problems_here.push(format!("after the rename, references are missing {missing:?}, extra {extra:?}"));
            }
        }
        if !problems_here.is_empty() {
            self.finding(format!("{what}: {}", problems_here.join("; ")));
        }
        true
    }

    /// Misspells the reference at `off` in `f` and applies "Did you mean".
    fn typo(&mut self, f: FileId, start: u32, end: u32) -> bool {
        let a0 = self.a0.clone();
        let path = a0.vfs.path(f);
        let text = a0.file_text(f).unwrap();
        let name = &text[start as usize..end as usize];
        if name.len() < 4 || a0.goto_definition(f, start).is_empty() {
            return false;
        }
        let i = 1 + self.rng.below(name.len() - 1);
        let wrong = format!("{}{}", &name[..i], &name[i + 1..]);
        let mut broken = text.to_string();
        broken.replace_range(start as usize..end as usize, &wrong);
        let what = format!("misspell '{name}' as '{wrong}' at {}", self.at(&a0, f, start));
        // The editor changes the document; the server updates incrementally.
        let mut docs = self.docs.clone();
        let version = docs.get(&path).and_then(|d| d.version).map_or(1, |v| v + 1);
        docs.insert(path.clone(), Doc { text: Arc::from(broken.as_str()), version: Some(version) });
        let a1 = {
            let vfs = a0.vfs.clone();
            let mut open: Vec<FileId> = docs.iter().map(|(p, d)| vfs.set_overlay_versioned(p, d.text.clone(), d.version)).collect();
            open.sort_unstable();
            let a1 = a0.update(&[f]).sync_open_files(&open);
            // Leave the shared store as it was.
            for (p, d) in &self.docs {
                vfs.set_overlay_versioned(p, d.text.clone(), d.version);
            }
            if !self.docs.contains_key(&path) {
                vfs.clear_overlay(&path);
            }
            a1
        };
        let diag = a1.file_diagnostics(f).into_iter().find(|d| d.span.start == start && matches!(d.code, "UNDECLARED_IDENTIFIER" | "NON_EXISTING_TYPE"));
        let Some(diag) = diag else { return false };
        let fixes = a1.quick_fixes(f, diag.span.start, diag.span.end);
        self.report.fixes += 1;
        let Some(fix) = fixes.iter().find(|q| q.title == format!("Did you mean '{name}'?")) else {
            let titles: Vec<&str> = fixes.iter().map(|q| q.title.as_str()).collect();
            self.note(format!("{what}: no 'Did you mean' for it (offered: {titles:?})"));
            return true;
        };
        let Some(edit) = a1.versioned_edit(fix.edits.clone()) else {
            self.finding(format!("{what}: the fix has no workspace edit"));
            return true;
        };
        match apply(&edit, &docs) {
            Ok(applied) => {
                let back = applied.iter().find(|ap| ap.path == path).map(|ap| ap.after.as_str());
                if applied.len() != 1 || back != Some(&*text) {
                    self.finding(format!("{what}: 'Did you mean' does not restore the text ({} files changed)", applied.len()));
                }
            }
            Err(e) => self.finding(format!("{what}: the editor would refuse the fix: {e}")),
        }
        true
    }

    /// Removes an `import pkg::*;` line and applies "Import package".
    fn import(&mut self, f: FileId, line_start: usize, line_end: usize, pkg: &str) -> bool {
        let a0 = self.a0.clone();
        let path = a0.vfs.path(f);
        let text = a0.file_text(f).unwrap();
        let mut broken = text.to_string();
        broken.replace_range(line_start..line_end, "");
        let what = format!("remove 'import {pkg}::*' at {}", self.at(&a0, f, line_start as u32));
        let mut docs = self.docs.clone();
        let version = docs.get(&path).and_then(|d| d.version).map_or(1, |v| v + 1);
        docs.insert(path.clone(), Doc { text: Arc::from(broken.as_str()), version: Some(version) });
        let setup = self.setup.clone();
        let Ok(a1) = guarded("build", || build(&setup, &docs)) else { return false };
        let Some(f1) = a1.vfs.lookup(&path) else { return false };
        let diag = a1.file_diagnostics(f1).into_iter().find(|d| matches!(d.code, "UNDECLARED_IDENTIFIER" | "NON_EXISTING_TYPE"));
        let Some(diag) = diag else { return false };
        let fixes = a1.quick_fixes(f1, diag.span.start, diag.span.end);
        self.report.fixes += 1;
        let title = format!("Import package '{pkg}' ({pkg}::*)");
        let Some(fix) = fixes.iter().find(|q| q.title == title) else {
            let titles: Vec<&str> = fixes.iter().map(|q| q.title.as_str()).collect();
            self.note(format!("{what}: no '{title}' for {} at {} (offered: {titles:?})", diag.message, self.at(&a1, f1, diag.span.start)));
            return true;
        };
        let Some(edit) = a1.versioned_edit(fix.edits.clone()) else {
            self.finding(format!("{what}: the fix has no workspace edit"));
            return true;
        };
        let applied = match apply(&edit, &docs) {
            Ok(x) => x,
            Err(e) => {
                self.finding(format!("{what}: the editor would refuse the fix: {e}"));
                return true;
            }
        };
        let mut wrong = Vec::new();
        for ap in &applied {
            let crlf = ap.before.contains("\r\n");
            for (_, _, t) in &ap.edits {
                if crlf && t.replace("\r\n", "").contains('\n') || !crlf && t.contains('\r') {
                    wrong.push(format!("{}: inserted text with other line endings", rel(&self.root, &ap.path)));
                }
            }
        }
        let docs2 = docs_after(&docs, &applied);
        let Ok(a2) = guarded("build", || build(&setup, &docs2)) else { return true };
        // The file's problems are back to what they were with the import: the same codes,
        // as many of each (lines may have moved by the inserted one).
        let codes = |a: &Analysis| -> BTreeMap<&'static str, usize> {
            let mut m = BTreeMap::new();
            if let Some(f) = a.vfs.lookup(&path) {
                for d in a.file_diagnostics(f) {
                    *m.entry(d.code).or_default() += 1;
                }
            }
            m
        };
        let (c0, c2) = (codes(&a0), codes(&a2));
        if c0 != c2 {
            wrong.push(format!("problems with the import {c0:?}, after the fix {c2:?}"));
        }
        if !wrong.is_empty() {
            self.finding(format!("{what}: {}", wrong.join("; ")));
        }
        true
    }
}

/// Runs the checks on a project.
pub fn run(setup: Arc<ProjectSetup>, opts: &Options, log: &mut dyn FnMut(&str)) -> Report {
    let root = setup.root.clone();
    let mut rng = Rng(opts.seed);
    // Some files open in the editor, so that edits carry versions.
    let probe = build(&setup, &BTreeMap::new());
    let mut files: Vec<FileId> = probe.project_files().into_iter().filter(|f| !probe.vfs.is_builtin(*f) && probe.vfs.path(*f).starts_with(&root)).collect();
    files.sort_by_key(|f| probe.vfs.path(*f));
    let mut docs = BTreeMap::new();
    for i in 0..files.len().min(4) {
        let f = files[rng.below(files.len())];
        if let Some(t) = probe.vfs.text(f) {
            docs.insert(probe.vfs.path(f), Doc { text: t, version: Some(10 + i as i32) });
        }
    }
    let a0 = build(&setup, &docs);
    let p0 = problems(&a0);
    let mut s = Session { setup, root: root.clone(), rng, docs, a0: a0.clone(), p0, report: Report::default(), log, verbose: opts.verbose };
    let files: Vec<FileId> = a0.project_files().into_iter().filter(|f| !a0.vfs.is_builtin(*f) && a0.vfs.path(*f).starts_with(&root)).collect();
    if files.is_empty() {
        return s.report;
    }
    // Positions by kind.
    let mut macro_uses: Vec<(FileId, u32)> = Vec::new();
    let mut macro_args: Vec<(FileId, u32)> = Vec::new();
    for u in a0.units.iter().flatten() {
        if u.kind == UnitKind::Builtin {
            continue;
        }
        for m in &u.macro_uses {
            if !files.contains(&m.name_span.file) {
                continue;
            }
            macro_uses.push((m.name_span.file, m.name_span.start + 1));
            if m.call_span.end > m.name_span.end + 1 {
                macro_args.push((m.name_span.file, m.name_span.end + 1 + (m.call_span.end - m.name_span.end - 1) / 2));
            }
        }
        // `define names and `ifdef operands: renames that start there.
        for d in &u.macro_defs {
            if files.contains(&d.name_span.file) {
                macro_uses.push((d.name_span.file, d.name_span.start));
            }
        }
        for m in &u.macro_names {
            if files.contains(&m.span.file) {
                macro_uses.push((m.span.file, m.span.start));
            }
        }
    }
    macro_uses.sort();
    macro_uses.dedup();
    macro_args.sort();
    macro_args.dedup();
    let headers: Vec<FileId> = files.iter().copied().filter(|f| a0.units_of_file(*f).iter().all(|u| a0.unit(*u).root != *f)).collect();
    let idents = |a: &Analysis, f: FileId| -> Vec<(u32, u32)> {
        a.file_text(f).map(|t| lexer::lex(&t, f).iter().filter(|t| t.kind == TokKind::Ident).map(|t| (t.span.start, t.span.end)).collect()).unwrap_or_default()
    };
    let mut done: FxHashSet<(PathBuf, u32)> = FxHashSet::default();
    let mut attempts = 0;
    while s.report.renames + s.report.refused < opts.count && attempts < opts.count * 40 {
        attempts += 1;
        let (f, off, kind) = match attempts % 4 {
            0 if !macro_uses.is_empty() => {
                let (f, o) = macro_uses[s.rng.below(macro_uses.len())];
                (f, o, "macro")
            }
            1 if !macro_args.is_empty() => {
                // An identifier inside the arguments of a macro call.
                let (f, o) = macro_args[s.rng.below(macro_args.len())];
                let ids = idents(&a0, f);
                let Some(&(st, _)) = ids.iter().find(|(a, b)| *a <= o && o < *b).or_else(|| ids.iter().find(|(a, _)| *a >= o)) else { continue };
                (f, st, "macro argument")
            }
            2 if !headers.is_empty() => {
                let f = headers[s.rng.below(headers.len())];
                let ids = idents(&a0, f);
                let Some(&(st, _)) = s.rng.pick(&ids) else { continue };
                (f, st, "header")
            }
            _ => {
                let f = files[s.rng.below(files.len())];
                let ids = idents(&a0, f);
                let Some(&(st, _)) = s.rng.pick(&ids) else { continue };
                (f, st, "any")
            }
        };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.rename(f, off, kind, &mut done))).is_err() {
            let at = s.at(&a0, f, off);
            s.finding(format!("rename at {at}: internal error"));
        }
    }
    // Quick fixes for misspelled names.
    let mut tries = 0;
    let want = (opts.count / 2).max(1);
    let mut typos = 0;
    while typos < want && tries < want * 40 {
        tries += 1;
        let f = files[s.rng.below(files.len())];
        let ids = idents(&a0, f);
        let Some(&(st, en)) = s.rng.pick(&ids) else { continue };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.typo(f, st, en))).unwrap_or_else(|_| {
            s.report.findings.push(format!("quick fix at {}: internal error", s.at(&a0, f, st)));
            true
        }) {
            typos += 1;
        }
    }
    // Quick fixes for removed imports.
    let mut imports: Vec<(FileId, usize, usize, String)> = Vec::new();
    for &f in &files {
        let Some(text) = a0.file_text(f) else { continue };
        let mut at = 0;
        for line in text.split_inclusive('\n') {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("import ").and_then(|r| r.strip_suffix("::*;")) {
                if rest.bytes().all(is_ident_byte) {
                    imports.push((f, at, at + line.len(), rest.to_string()));
                }
            }
            at += line.len();
        }
    }
    let mut n = 0;
    tries = 0;
    while n < want && tries < want * 10 && !imports.is_empty() {
        tries += 1;
        let (f, a, b, pkg) = imports.swap_remove(s.rng.below(imports.len()));
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.import(f, a, b, &pkg))).unwrap_or(true) {
            n += 1;
        }
    }
    s.report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_offsets() {
        let e = vec![(2, 4, "abcd".to_string()), (10, 12, String::new())];
        assert_eq!(shifted(&e, 0), 0);
        assert_eq!(shifted(&e, 2), 2);
        assert_eq!(shifted(&e, 3), 3);
        assert_eq!(shifted(&e, 5), 7);
        assert_eq!(shifted(&e, 10), 12);
        assert_eq!(shifted(&e, 13), 13);
    }

    #[test]
    fn line_endings_are_counted_apart() {
        assert_eq!(endings("a\r\nb\nc\rd"), (1, 1, 1));
    }

    /// Renames and quick fixes on the demo project are valid.
    #[test]
    fn renames_and_quick_fixes_on_the_demo_are_valid() {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../examples/demo-project").canonicalize().unwrap();
        let root = match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
            Some(rest) => PathBuf::from(rest),
            None => p,
        };
        let env = rustc_hash::FxHashMap::default();
        let cfg = dvi_project::load(&root, Some(&root.join(".dvicode/default.build")), &env);
        let setup = Arc::new(ProjectSetup::new(&root, cfg, env));
        let mut out = String::new();
        let r = run(setup, &Options { count: 40, seed: 3, verbose: false }, &mut |s| {
            out.push_str(s);
            out.push('\n');
        });
        assert!(r.renames >= 20, "{} renames\n{out}", r.renames);
        assert!(r.fixes >= 5, "{} fixes\n{out}", r.fixes);
        assert!(r.findings.is_empty(), "{out}");
    }
}
