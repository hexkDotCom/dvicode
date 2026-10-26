//! DVICode Lint: rules about the meaning and the style of SystemVerilog/UVM code, run on the
//! same snapshot as the editor features and the `lint` command.
//!
//! A rule looks at one file in one compile context (the unit the editor shows the file in, or
//! each unit with `--all-contexts`) and reports findings at what the user wrote. A rule reports
//! only what the facts it needs prove: a candidate whose facts are not known (an unresolved
//! base class, code produced by a macro) is counted as unknown, not reported. Configuration
//! (`.dvicode/lint.json`, [`config`]) chooses the rules and their levels per file; waivers in
//! the code ([`waivers`]) suppress single findings.

pub mod config;
pub mod facts;
pub mod guide_ids;
pub mod json;
pub mod rules;
pub mod waivers;

use crate::select::ContextKey;
use crate::world::{Analysis, UnitId};
use dvi_syntax::{FileId, Severity, Span};
use rustc_hash::FxHashMap;
use serde::Serialize;
use std::sync::Arc;

/// A finding's level, from configuration (`off` turns a rule off).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Off,
    Hint,
    Info,
    Warning,
    Error,
}

impl Level {
    pub const NAMES: &'static [&'static str] = &["off", "hint", "info", "warning", "error"];

    pub fn parse(s: &str) -> Option<Level> {
        Some(match s {
            "off" => Level::Off,
            "hint" => Level::Hint,
            "info" => Level::Info,
            "warning" => Level::Warning,
            "error" => Level::Error,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        Level::NAMES[self as usize]
    }

    pub fn severity(self) -> Option<Severity> {
        Some(match self {
            Level::Off => return None,
            Level::Hint => Severity::Hint,
            Level::Info => Severity::Info,
            Level::Warning => Severity::Warning,
            Level::Error => Severity::Error,
        })
    }
}

/// A named set of rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Ruleset {
    /// Provable correctness rules, on by default.
    #[serde(rename = "recommended")]
    Recommended,
    /// Correctness rules that have not passed the precision review yet: opt-in.
    #[serde(rename = "recommended-preview")]
    RecommendedPreview,
    /// The checkable rules of the HEXK SystemVerilog/UVM Style Guide (includes both above).
    #[serde(rename = "hexk")]
    Hexk,
}

impl Ruleset {
    pub const ALL: &'static [Ruleset] = &[Ruleset::Recommended, Ruleset::RecommendedPreview, Ruleset::Hexk];

    pub fn parse(s: &str) -> Option<Ruleset> {
        Ruleset::ALL.iter().copied().find(|r| r.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Ruleset::Recommended => "recommended",
            Ruleset::RecommendedPreview => "recommended-preview",
            Ruleset::Hexk => "hexk",
        }
    }

    /// Whether choosing `self` enables a rule of `of`: `hexk` includes the others.
    pub fn includes(self, of: Ruleset) -> bool {
        self == of || self == Ruleset::Hexk
    }
}

/// A rule parameter and its allowed values.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ParamMeta {
    pub name: &'static str,
    pub doc: &'static str,
    pub min: i64,
    pub max: i64,
    pub default: i64,
}

/// What a rule is: shown by `--list-rules`, `--explain` and the rule pages.
#[derive(Debug, Serialize)]
pub struct RuleMeta {
    /// Stable identifier (`randomize-result-discarded`).
    pub id: &'static str,
    /// The guide rules it checks (all or part of), many-to-many.
    pub guide: &'static [&'static str],
    pub rulesets: &'static [Ruleset],
    pub default_level: Level,
    /// Named parts of the rule, each with its own message (`family`, `param`, ...).
    pub subchecks: &'static [&'static str],
    pub summary: &'static str,
    pub why: &'static str,
    pub params: &'static [ParamMeta],
}

impl RuleMeta {
    /// Where its documentation is published.
    pub fn docs_url(&self) -> String {
        format!("https://hexkdotcom.github.io/dvicode/lint/{}", self.id)
    }
}

pub trait Rule: Sync + Send {
    fn meta(&self) -> &'static RuleMeta;
    fn check(&self, cx: &mut Cx);
}

/// A finding before configuration and waivers are applied.
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub subcheck: Option<&'static str>,
    #[serde(skip)]
    pub span: Span,
    pub message: String,
    pub level: Level,
    #[serde(skip)]
    pub related: Vec<(Span, String)>,
    /// The waiver or suppression that covers it.
    pub suppressed: Option<String>,
}

/// What linting one file in one context gave.
#[derive(Clone, Debug, Default)]
pub struct FileLint {
    pub findings: Vec<Finding>,
    /// Candidates a rule saw but could not decide, by rule and reason.
    pub unknown: FxHashMap<(&'static str, &'static str), u32>,
    /// Rules that did not look at everything (budget, cancellation, code the walker cannot
    /// follow), with why.
    pub incomplete: Vec<(&'static str, &'static str)>,
    /// The context it was linted in.
    pub context: Option<ContextKey>,
    /// The file is owned (linted at all).
    pub owned: bool,
}

/// Steps one rule may take in one file before it stops (and says so).
pub const STEPS_PER_FILE: u64 = 2_000_000;

/// What a rule sees while it checks a file.
pub struct Cx<'a> {
    pub a: &'a Analysis,
    pub file: FileId,
    pub unit: UnitId,
    pub facts: Arc<facts::FileFacts>,
    pub params: &'a FxHashMap<String, i64>,
    rule: &'static RuleMeta,
    level: Level,
    steps: u64,
    findings: Vec<Finding>,
    unknown: FxHashMap<&'static str, u32>,
    incomplete: Option<&'static str>,
}

impl<'a> Cx<'a> {
    /// A finding at `span`, which must be in the file being linted.
    pub fn report(&mut self, subcheck: Option<&'static str>, span: Span, message: impl Into<String>, related: Vec<(Span, String)>) {
        if span.file != self.file {
            return;
        }
        self.findings.push(Finding { rule: self.rule.id, subcheck, span, message: message.into(), level: self.level, related, suppressed: None });
    }

    /// A candidate whose facts are not known.
    pub fn unknown(&mut self, reason: &'static str) {
        *self.unknown.entry(reason).or_default() += 1;
    }

    /// The rule could not look at everything in the file.
    pub fn incomplete(&mut self, reason: &'static str) {
        self.incomplete.get_or_insert(reason);
    }

    /// Counts work; `false` when the rule must stop (budget spent or the snapshot replaced).
    pub fn step(&mut self, n: u64) -> bool {
        self.steps += n;
        if self.steps > STEPS_PER_FILE {
            self.incomplete("budget");
            return false;
        }
        if self.steps % 4096 < n && self.a.cancelled() {
            self.incomplete("cancelled");
            return false;
        }
        true
    }

    pub fn param(&self, name: &str) -> i64 {
        self.params.get(name).copied().or_else(|| self.rule.params.iter().find(|p| p.name == name).map(|p| p.default)).unwrap_or(0)
    }
}

impl Analysis {
    /// The configuration of `file`: never owned when it is built in or part of the UVM
    /// library, unless a configuration includes it.
    pub fn lint_config(&self, set: &config::ConfigSet, env: &dyn config::ConfigEnv, file: FileId) -> config::FileConfig {
        let library = self.vfs.is_builtin(file) || file == dvi_syntax::preproc::COMMAND_LINE_FILE || self.lint_facts.uvm_files(self).contains(&file);
        set.for_file(env, &self.path(file), library)
    }

    /// Lints `file` with its resolved configuration, in `unit` (else the file's context unit).
    pub fn lint_file(&self, file: FileId, cfg: &config::FileConfig, unit: Option<UnitId>) -> FileLint {
        let mut out = FileLint { owned: cfg.owned.is_owned(), ..Default::default() };
        if !out.owned || !cfg.enabled {
            return out;
        }
        let Some(unit) = unit.or_else(|| self.context_unit(file)) else { return out };
        out.context = Some(self.context_key(unit));
        let facts = self.lint_facts.file(self, unit, file);
        let empty = FxHashMap::default();
        for rule in rules::all() {
            let meta = rule.meta();
            let Some(set) = cfg.rules.get(meta.id) else { continue };
            if set.level == Level::Off {
                continue;
            }
            let mut cx = Cx {
                a: self,
                file,
                unit,
                facts: facts.clone(),
                params: set.params.as_ref().unwrap_or(&empty),
                rule: meta,
                level: set.level,
                steps: 0,
                findings: Vec::new(),
                unknown: FxHashMap::default(),
                incomplete: None,
            };
            rule.check(&mut cx);
            for (reason, n) in cx.unknown {
                *out.unknown.entry((meta.id, reason)).or_default() += n;
            }
            if let Some(r) = cx.incomplete {
                out.incomplete.push((meta.id, r));
            }
            out.findings.extend(cx.findings);
        }
        waivers::apply(self, file, &facts, &mut out.findings, cfg);
        out.findings.sort_by_key(|f| (f.span.start, f.span.end, f.rule));
        out
    }
}
