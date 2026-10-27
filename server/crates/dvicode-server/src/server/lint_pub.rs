//! Lint in the editor: the settings, the configuration set of the project, and lint findings
//! and configuration problems as LSP diagnostics. Findings are published with the compile
//! problems of the same file, by the same problem check (see `Server::publish`).

use dvi_analysis::ide::url_of;
use dvi_analysis::lint::baseline::Baseline;
use dvi_analysis::lint::config::{ConfigFile, ConfigSet, Defaults, DiskEnv};
use dvi_analysis::lint::{rules, FileLint, Finding, Level, Ruleset};
use dvi_analysis::Analysis;
use dvi_syntax::{FileId, LineIndex};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeDescription, Command, Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, NumberOrString, Position, Range, Url,
};
use serde_json::Value;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const SOURCE: &str = "dvicode-lint";

/// Findings of one file shown at most (compile problems have their own limit).
pub const MAX_PER_FILE: usize = 500;

/// The `dvicode.lint` settings.
#[derive(Clone, Debug, PartialEq)]
pub struct LintSettings {
    pub enabled: bool,
    /// `None`: not set (the built-in default, `recommended`).
    pub rulesets: Option<Vec<Ruleset>>,
}

impl Default for LintSettings {
    fn default() -> Self {
        LintSettings { enabled: true, rulesets: None }
    }
}

impl LintSettings {
    /// From the `dvicode` settings object; a value of the wrong type keeps its default.
    pub fn of(v: &Value) -> LintSettings {
        let l = v.get("lint");
        let enabled = l.and_then(|l| l.get("enable")).and_then(Value::as_bool).unwrap_or(true);
        let rulesets = l.and_then(|l| l.get("rulesets")).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().and_then(Ruleset::parse)).collect());
        LintSettings { enabled, rulesets }
    }
}

/// The project's configuration set, made again when the project root or the settings change,
/// and emptied when a configuration file changes.
#[derive(Default)]
pub struct LintConfigs {
    set: Mutex<Option<Arc<ConfigSet>>>,
    /// Baselines read, by path, with the size and time of the file read (`None`: missing or
    /// unreadable).
    baselines: Mutex<FxHashMap<PathBuf, (Option<(u64, std::time::SystemTime)>, Option<Arc<Baseline>>)>>,
}

impl LintConfigs {
    pub fn get(&self, root: &Path, s: &LintSettings) -> Option<Arc<ConfigSet>> {
        if !s.enabled {
            return None;
        }
        let mut cur = self.set.lock().unwrap();
        let rulesets = s.rulesets.clone().unwrap_or_else(|| vec![Ruleset::Recommended]);
        if let Some(c) = cur.as_ref() {
            if c.root == root && c.defaults.rulesets == rulesets {
                return Some(c.clone());
            }
        }
        let d = Defaults { enabled: true, from_setting: s.rulesets.is_some(), rulesets, ..Defaults::default() };
        let c = Arc::new(ConfigSet::new(root, d, None));
        *cur = Some(c.clone());
        // The baselines of another project (or of other settings) are not kept.
        self.baselines.lock().unwrap().clear();
        Some(c)
    }

    /// A configuration file changed: read them all again.
    pub fn invalidate(&self) {
        if let Some(c) = self.set.lock().unwrap().as_ref() {
            c.invalidate();
        }
        self.baselines.lock().unwrap().clear();
    }

    /// The baseline at `path`, read again when the file changed (wherever it is, watched or
    /// not). A change also has the configurations read again, so that what they say about
    /// their baseline (missing, unreadable) is current.
    pub fn baseline(&self, path: &Path) -> Option<Arc<Baseline>> {
        let stamp = dvi_project::fsx::metadata(path).ok().and_then(|m| Some((m.len(), m.modified().ok()?)));
        let changed = match self.baselines.lock().unwrap().get(path) {
            Some((s, b)) if *s == stamp => return b.clone(),
            Some(_) => true,
            None => false,
        };
        if changed {
            if let Some(c) = self.set.lock().unwrap().as_ref() {
                c.invalidate();
            }
        }
        let b = dvi_analysis::lint::read_control_file(path).and_then(|t| String::from_utf8(t).ok()).and_then(|t| Baseline::read(&t).ok()).map(Arc::new);
        self.baselines.lock().unwrap().insert(path.to_path_buf(), (stamp, b.clone()));
        b
    }
}

/// Whether `path` is a lint configuration or baseline file: a JSON file in a `.dvicode` folder.
pub fn is_config(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json") && path.parent().and_then(Path::file_name).is_some_and(|n| n == ".dvicode")
}

/// What linting a file gave, as diagnostics: the findings shown, how many more there are, and
/// notices about lint itself.
#[derive(Default)]
pub struct FileDiags {
    pub shown: Vec<Diagnostic>,
    pub hidden: usize,
    pub notes: Vec<Diagnostic>,
}

impl FileDiags {
    /// Keeps at most `room` findings; returns how many it hid.
    pub fn cut(&mut self, room: usize) -> usize {
        let n = self.shown.len().saturating_sub(room);
        self.shown.truncate(room);
        self.hidden += n;
        n
    }

    /// The diagnostics, with a notice of the hidden findings (`limit`: the problem limit hid
    /// some, not only the limit per file).
    pub fn into_vec(self, limit: bool) -> Vec<Diagnostic> {
        let mut out = self.shown;
        if self.hidden > 0 {
            out.push(notice(if limit {
                format!("Lint: {} findings in this file are not shown (the problem limit, dvicode.maxProblems, is reached)", self.hidden)
            } else {
                format!("Lint: {} more findings in this file are not shown", self.hidden)
            }));
        }
        out.extend(self.notes);
        out
    }
}

/// What linting a file gave (nothing when it is not linted): at most `room` findings, those
/// the baseline does not list first.
pub fn file_diagnostics(a: &Analysis, configs: &LintConfigs, set: &ConfigSet, f: FileId, room: usize) -> FileDiags {
    let cfg = a.lint_config(set, &DiskEnv, f);
    if !cfg.owned.is_owned() {
        return FileDiags::default();
    }
    let lint: FileLint = a.lint_file(f, &cfg, None);
    let open: Vec<&Finding> = lint.findings.iter().filter(|x| x.suppressed.is_none()).collect();
    // Findings the baseline lists are shown as hints (not in the Problems view).
    let mut listed = vec![false; open.len()];
    if let (Some(path), Some(unit)) = (&cfg.baseline, lint.unit) {
        if let Some(b) = configs.baseline(path) {
            let fps: Vec<_> = open.iter().map(|x| a.fingerprint(unit, x, &set.root)).collect();
            listed = b.matched(&fps);
        }
    }
    // Only the findings shown become diagnostics.
    let mut order: Vec<usize> = (0..open.len()).collect();
    order.sort_by_key(|i| listed[*i]);
    let room = room.min(MAX_PER_FILE);
    let shown = order
        .iter()
        .take(room)
        .filter_map(|&i| {
            let mut d = to_lsp(a, open[i])?;
            if listed[i] {
                d.severity = Some(DiagnosticSeverity::HINT);
            }
            Some(d)
        })
        .collect();
    let mut out = FileDiags { shown, hidden: open.len().saturating_sub(room), ..Default::default() };
    if let Some((rule, why)) = lint.incomplete.first() {
        out.notes.push(notice(format!("Lint did not finish checking this file ({rule}: {why}); some findings may be missing")));
    }
    out
}

/// An information diagnostic at the start of the file, about lint itself.
pub fn notice(message: String) -> Diagnostic {
    Diagnostic {
        range: Range::default(),
        severity: Some(DiagnosticSeverity::INFORMATION),
        code: Some(NumberOrString::String("lint-incomplete".into())),
        source: Some(SOURCE.into()),
        message,
        ..Default::default()
    }
}

fn to_lsp(a: &Analysis, x: &Finding) -> Option<Diagnostic> {
    let range = a.to_range(x.span)?;
    let meta = rules::meta(x.rule)?;
    let severity = match x.level {
        Level::Error => DiagnosticSeverity::ERROR,
        Level::Warning => DiagnosticSeverity::WARNING,
        Level::Info => DiagnosticSeverity::INFORMATION,
        Level::Hint => DiagnosticSeverity::HINT,
        Level::Off => return None,
    };
    let related: Vec<DiagnosticRelatedInformation> =
        x.related.iter().filter_map(|(s, m)| Some(DiagnosticRelatedInformation { location: a.to_location(*s)?, message: m.clone() })).collect();
    Some(Diagnostic {
        range,
        severity: Some(severity),
        code: Some(NumberOrString::String(x.rule.to_string())),
        code_description: Url::parse(&meta.docs_url()).ok().map(|href| CodeDescription { href }),
        source: Some(SOURCE.into()),
        message: match meta.guide.first() {
            Some(g) => format!("{} ({g})", x.message),
            None => x.message.clone(),
        },
        related_information: (!related.is_empty()).then_some(related),
        tags: None,
        data: None,
    })
}

/// The code actions of the lint findings of `f` that overlap `start..=end`: each finding's
/// fixes, a waiver above it, and turning its rule off in the file. A waiver or a suppression
/// is inserted with a `<reason>` placeholder, which the VS Code extension then selects
/// (`dvicode.lint.selectReason`, only for a client that runs it: `select_reason`).
pub fn code_actions(a: &Analysis, set: &ConfigSet, f: FileId, start: u32, end: u32, select_reason: bool) -> Vec<CodeActionOrCommand> {
    let cfg = a.lint_config(set, &DiskEnv, f);
    if !cfg.owned.is_owned() {
        return Vec::new();
    }
    let (Some(text), Some(li)) = (a.file_text(f), a.file_lines(f)) else { return Vec::new() };
    let uri = url_of(&a.path(f));
    let lint = a.lint_file(f, &cfg, None);
    let mut out = Vec::new();
    let mut waived: Vec<(&str, u32)> = Vec::new();
    let mut disabled: Vec<&str> = Vec::new();
    let select = |line: u32| {
        select_reason.then(|| Command {
            title: "Select the reason".into(),
            command: "dvicode.lint.selectReason".into(),
            arguments: Some(vec![serde_json::json!(uri.to_string()), serde_json::json!(line)]),
        })
    };
    for x in lint.findings.iter().filter(|x| x.suppressed.is_none() && x.span.start <= end && start <= x.span.end) {
        let Some(meta) = rules::meta(x.rule) else { continue };
        let diag = to_lsp(a, x);
        let diagnostics = diag.clone().map(|d| vec![d]);
        for fix in &x.fixes {
            if let Some(edit) = a.versioned_edit(fix.edits.clone()) {
                out.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: fix.title.clone(),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: diagnostics.clone(),
                    edit: Some(edit),
                    is_preferred: fix.safe.then_some(true),
                    ..Default::default()
                }));
            }
        }
        // Findings about waivers and suppressions cannot be waived.
        if meta.id.starts_with("waiver-") || meta.id.starts_with("suppression-") {
            continue;
        }
        // A waiver on its own line above the finding's line, indented like it: only when a
        // waiver there covers the finding (not inside a `define, nor when the line starts in
        // the middle of another statement).
        let line = li.line_of(x.span.start);
        let place = lint.unit.and_then(|u| a.lint_waiver_place(f, u, x.span.start));
        if let Some(ls) = place.filter(|_| !waived.contains(&(x.rule, line))) {
            waived.push((x.rule, line));
            let indent: String = text[ls as usize..].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let id = meta.guide.first().copied().unwrap_or(meta.id);
            let at = dvi_syntax::Span::new(f, ls, ls);
            if let Some(edit) = a.versioned_edit(vec![(at, format!("{indent}// {id} waived: <reason>\n"))]) {
                out.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Waive {id} here"),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: diagnostics.clone(),
                    edit: Some(edit),
                    command: select(line),
                    ..Default::default()
                }));
            }
        }
        if !disabled.contains(&x.rule) {
            disabled.push(x.rule);
            let at = dvi_syntax::Span::new(f, 0, 0);
            if let Some(edit) = a.versioned_edit(vec![(at, format!("// dvicode-lint: disable-file={} <reason>\n", x.rule))]) {
                out.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Turn {} off in this file", x.rule),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics,
                    edit: Some(edit),
                    command: select(0),
                    ..Default::default()
                }));
            }
        }
    }
    out
}

/// A configuration file's problems (and its notes), as diagnostics of that file.
pub fn config_diagnostics(set: &ConfigSet, c: &ConfigFile) -> (Url, Vec<Diagnostic>) {
    let li = LineIndex::new(&c.text);
    let pos = |at: u32| {
        let (l, ch) = li.position(&c.text, at);
        Position::new(l, ch)
    };
    let notes = set.reason_notes(&DiskEnv, c);
    let diags = c
        .problems
        .iter()
        .chain(notes.iter())
        .map(|p| Diagnostic {
            range: Range::new(pos(p.start), pos(p.end)),
            severity: Some(if p.warning { DiagnosticSeverity::WARNING } else { DiagnosticSeverity::ERROR }),
            code: Some(NumberOrString::String("lint-config".into())),
            source: Some(SOURCE.into()),
            message: p.message.clone(),
            ..Default::default()
        })
        .collect();
    (url_of(&c.path), diags)
}
