//! Lint in the editor: the settings, the configuration set of the project, and lint findings
//! and configuration problems as LSP diagnostics. Findings are published with the compile
//! problems of the same file, by the same problem check (see `Server::publish`).

use dvi_analysis::ide::url_of;
use dvi_analysis::lint::config::{ConfigFile, ConfigSet, Defaults, DiskEnv};
use dvi_analysis::lint::{rules, FileLint, Finding, Level, Ruleset};
use dvi_analysis::Analysis;
use dvi_syntax::{FileId, LineIndex};
use lsp_types::{CodeDescription, Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, NumberOrString, Position, Range, Url};
use serde_json::Value;
use std::path::Path;
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
        Some(c)
    }

    /// A configuration file changed: read them all again.
    pub fn invalidate(&self) {
        if let Some(c) = self.set.lock().unwrap().as_ref() {
            c.invalidate();
        }
    }
}

/// Whether `path` is a lint configuration file.
pub fn is_config(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "lint.json") && path.parent().and_then(Path::file_name).is_some_and(|n| n == ".dvicode")
}

/// What linting a file gave, as diagnostics (empty when it is not linted).
pub fn file_diagnostics(a: &Analysis, set: &ConfigSet, f: FileId) -> Vec<Diagnostic> {
    let cfg = a.lint_config(set, &DiskEnv, f);
    if !cfg.owned.is_owned() {
        return Vec::new();
    }
    let lint: FileLint = a.lint_file(f, &cfg, None);
    let mut out: Vec<Diagnostic> = lint.findings.iter().filter(|x| x.suppressed.is_none()).filter_map(|x| to_lsp(a, x)).collect();
    let more = out.len().saturating_sub(MAX_PER_FILE);
    out.truncate(MAX_PER_FILE);
    if more > 0 {
        out.push(notice(format!("Lint: {more} more findings in this file are not shown")));
    }
    if let Some((rule, why)) = lint.incomplete.first() {
        out.push(notice(format!("Lint did not finish checking this file ({rule}: {why}); some findings may be missing")));
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

/// A configuration file's problems, as diagnostics of that file.
pub fn config_diagnostics(c: &ConfigFile) -> (Url, Vec<Diagnostic>) {
    let li = LineIndex::new(&c.text);
    let pos = |at: u32| {
        let (l, ch) = li.position(&c.text, at);
        Position::new(l, ch)
    };
    let diags = c
        .problems
        .iter()
        .map(|p| Diagnostic {
            range: Range::new(pos(p.start), pos(p.end)),
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String("lint-config".into())),
            source: Some(SOURCE.into()),
            message: p.message.clone(),
            ..Default::default()
        })
        .collect();
    (url_of(&c.path), diags)
}
