//! Build files (`.dvicode/*.build`) and argument files (`-f`) open in the editor: their
//! problems as they are typed, hover notes on what their words resolve to, and links to the
//! files they name. They are not SystemVerilog, so they stay out of the analysis.

use dvi_project::{fsx, ArgFile, BuildConfig, BuildProblem, Inspection};
use dvi_syntax::LineIndex;
use lsp_types::*;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An open build or argument file.
pub struct BuildDoc {
    pub path: PathBuf,
    pub text: Arc<str>,
    pub version: i32,
    /// The newest check started; an older one that finishes later is dropped.
    pub seq: u64,
    /// The last inspection, and the text it was made of.
    pub inspected: Option<Inspected>,
}

pub struct Inspected {
    pub text: Arc<str>,
    pub lines: LineIndex,
    /// The document version `text` is.
    pub version: i32,
    pub inspection: Inspection,
}

/// The reply to a hover or documentLink request (`Null` for anything else).
pub fn answer(method: &str, params: &serde_json::Value, ins: &Inspected) -> serde_json::Value {
    match method {
        "textDocument/hover" => serde_json::from_value::<HoverParams>(params.clone())
            .ok()
            .and_then(|p| hover(ins, p.text_document_position_params.position))
            .map_or(serde_json::Value::Null, |h| serde_json::to_value(h).unwrap_or_default()),
        "textDocument/documentLink" => serde_json::to_value(links(ins)).unwrap_or_default(),
        _ => serde_json::Value::Null,
    }
}

/// A build or argument file, by its name or the language the editor gives it.
pub fn is_build_doc(path: &Path, language: Option<&str>) -> bool {
    language == Some("dvicode-build") || path.extension().is_some_and(|e| e == "build" || e == "f" || e == "F")
}

/// What to inspect a document with: its project folder, and, for an argument file, how the
/// current build reads it (`None` when it does not read it: then the file is read from its own
/// folder, and its problems are not shown since they may not be problems at all).
pub struct Context {
    pub root: PathBuf,
    pub arg: Option<ArgFile>,
    pub show_problems: bool,
}

pub fn context(path: &Path, root: Option<&Path>, config: &BuildConfig, env: &FxHashMap<String, String>) -> Context {
    let project = root.map(Path::to_path_buf).unwrap_or_else(|| path.parent().map(Path::to_path_buf).unwrap_or_default());
    // How the build reads it, first: the build file in use, or a file it reads (a `.build`
    // read through `+dvicode_include_config+` too).
    if config.build_file.as_deref().is_some_and(|bf| same_file(bf, path)) {
        return Context { root: config.root.clone(), arg: None, show_problems: true };
    }
    if let Some(a) = find_arg_file(path, &config.arg_files) {
        return Context { root: config.root.clone(), arg: Some(a.clone()), show_problems: true };
    }
    // `<project>/.dvicode/x.build`
    if path.extension().is_some_and(|e| e == "build") {
        let dir = path.parent().filter(|d| d.file_name().is_some_and(|n| n == ".dvicode"));
        let root = dir.and_then(Path::parent).map(Path::to_path_buf).unwrap_or(project);
        return Context { root, arg: None, show_problems: true };
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| project.clone());
    let arg = ArgFile { path: path.to_path_buf(), base: dir, env: std::sync::Arc::new(env.clone()), env_bytes: 0, mode: "default".into(), comp_root: project.clone(), switches: Vec::new(), path_maps: Vec::new() };
    Context { root: project, arg: Some(arg), show_problems: false }
}

fn find_arg_file<'a>(path: &Path, files: &'a [ArgFile]) -> Option<&'a ArgFile> {
    if let Some(a) = files.iter().find(|a| a.path == path) {
        return Some(a);
    }
    // The same file under another spelling (case, links, `..`). Names are compared first,
    // ignoring case (Windows), so that only likely candidates are resolved on disk.
    files.iter().filter(|a| same_name(&a.path, path)).find(|a| fsx::same_file(&a.path, path))
}

fn same_name(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(x), Some(y)) => x.to_string_lossy().eq_ignore_ascii_case(&y.to_string_lossy()),
        _ => false,
    }
}

pub fn same_file(a: &Path, b: &Path) -> bool {
    a == b || (same_name(a, b) && fsx::same_file(a, b))
}

pub fn inspect(path: &Path, text: &str, cx: &Context, env: &FxHashMap<String, String>) -> Inspection {
    let mut ins = dvi_project::inspect(&cx.root, path, text, env, cx.arg.as_ref());
    if !cx.show_problems {
        ins.problems.clear();
    }
    ins
}

fn range(text: &str, lines: &LineIndex, start: u32, end: u32) -> Range {
    let pos = |o: u32| {
        let (l, c) = lines.position(text, o);
        Position::new(l, c)
    };
    Range::new(pos(start), pos(end))
}

pub fn diagnostics(ins: &Inspected) -> Vec<Diagnostic> {
    let text = &*ins.text;
    ins.inspection
        .problems
        .iter()
        .map(|p: &BuildProblem| {
            let (s, e) = match p.range {
                Some((s, e)) if (e as usize) <= text.len() => (s, e),
                _ => (ins.lines.offset(text, p.line, 0), ins.lines.offset(text, p.line, u32::MAX)),
            };
            Diagnostic {
                range: range(text, &ins.lines, s, e),
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(NumberOrString::String("BUILD_CONFIG".into())),
                source: Some("dvicode".into()),
                message: p.message.clone(),
                ..Default::default()
            }
        })
        .collect()
}

/// The note on the word at `position`: the narrowest one (an argument within its option).
pub fn hover(ins: &Inspected, position: Position) -> Option<Hover> {
    let text = &*ins.text;
    let off = ins.lines.offset(text, position.line, position.character);
    let (s, e, note) = ins.inspection.notes.iter().filter(|(s, e, _)| *s <= off && off < *e).min_by_key(|(s, e, _)| e - s)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: note.clone() }),
        range: Some(range(text, &ins.lines, *s, *e)),
    })
}

pub fn links(ins: &Inspected) -> Vec<DocumentLink> {
    let text = &*ins.text;
    ins.inspection
        .links
        .iter()
        .filter_map(|(s, e, target)| {
            // The path itself, not the option before it (`-f path`, `-f "a path"`).
            let word = text.get(*s as usize..*e as usize)?;
            let skip = match word.chars().last() {
                Some(q @ ('"' | '\'')) if word.len() > 1 => word[..word.len() - 1].rfind(q),
                _ => word.rfind(char::is_whitespace).map(|i| i + 1),
            }
            .unwrap_or(0) as u32;
            Some(DocumentLink {
                range: range(text, &ins.lines, s + skip, *e),
                target: Url::from_file_path(target).ok(),
                tooltip: Some(target.display().to_string()),
                data: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_skip_the_option_and_hover_takes_the_narrowest_note() {
        let dir = std::env::temp_dir().join("dvicode_build_docs_test");
        std::fs::create_dir_all(dir.join(".dvicode")).unwrap();
        std::fs::write(dir.join("a.f"), "").unwrap();
        let path = dir.join(".dvicode/default.build");
        let text = "-f   a.f\n";
        let env = FxHashMap::default();
        let cx = context(&path, Some(&dir), &BuildConfig::default(), &env);
        assert_eq!(cx.root, dir);
        let ins = Inspected { text: Arc::from(text), lines: LineIndex::new(text), version: 1, inspection: inspect(&path, text, &cx, &env) };
        let l = links(&ins);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].range, Range::new(Position::new(0, 5), Position::new(0, 8)));
        let h = hover(&ins, Position::new(0, 1)).expect("a note");
        let HoverContents::Markup(m) = h.contents else { panic!() };
        assert!(m.value.contains("argument file"), "{}", m.value);
    }

    #[test]
    fn a_quoted_path_with_spaces_is_one_link() {
        let dir = std::env::temp_dir().join("dvicode build docs quoted");
        std::fs::create_dir_all(dir.join("a dir")).unwrap();
        std::fs::write(dir.join("a dir/l.f"), "").unwrap();
        let path = dir.join(".dvicode/default.build");
        let text = "-f \"a dir/l.f\"\n";
        let env = FxHashMap::default();
        let cx = context(&path, Some(&dir), &BuildConfig::default(), &env);
        let ins = Inspected { text: Arc::from(text), lines: LineIndex::new(text), version: 1, inspection: inspect(&path, text, &cx, &env) };
        let l = links(&ins);
        assert_eq!(l.len(), 1, "{:?}", ins.inspection);
        assert_eq!(l[0].range, Range::new(Position::new(0, 3), Position::new(0, 14)));
    }

    #[test]
    fn an_included_build_file_is_checked_the_way_the_build_reads_it() {
        let dir = std::env::temp_dir().join("dvicode_build_docs_included");
        std::fs::create_dir_all(dir.join(".dvicode/sub")).unwrap();
        std::fs::write(dir.join(".dvicode/sub/a.sv"), "").unwrap();
        std::fs::write(dir.join(".dvicode/sub/Common.build"), "a.sv\n").unwrap();
        let bf = dir.join(".dvicode/default.build");
        let env = FxHashMap::default();
        let cfg = dvi_project::parse_build(&dir, &bf, "+dvicode_include_config+.dvicode/sub/Common.build\n", &env);
        assert!(cfg.problems.is_empty(), "{:?}", cfg.problems);
        // Opened under another case of its name (Windows), it is still the file the build reads.
        let opened = if cfg!(windows) { dir.join(".dvicode/sub/common.build") } else { dir.join(".dvicode/sub/Common.build") };
        let cx = context(&opened, Some(&dir), &cfg, &env);
        assert!(cx.show_problems && cx.arg.as_ref().is_some_and(|a| a.base == dir.join(".dvicode/sub")), "{:?}", cx.arg);
        assert!(inspect(&opened, "a.sv\n", &cx, &env).problems.is_empty());
    }
}
