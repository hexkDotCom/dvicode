//! Lint configuration: `.dvicode/lint.json` files, how they combine for a file, and what is
//! wrong with them.
//!
//! For a file, the configurations from the project root down to the file's folder apply in
//! that order (a nested one inherits unless it says `"extends": "none"`). Within one file:
//! `rulesets` (replaces the inherited list), then `rules` (guide IDs, then rule IDs, so a rule's
//! own ID wins), then each matching `overrides` entry in order. Command-line rulesets and rules
//! come last. A rule's level is its last explicit level, else its default level when one of
//! the chosen rulesets includes it, else off. Paths and globs are relative to the folder that
//! holds the `.dvicode` folder.

use super::guide_ids::{guide_level, looks_like_guide_id};
use super::json::{self, Member, Node, Value};
use super::{rules, Level, Ruleset};
use rustc_hash::FxHashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A problem in a configuration file (byte offsets into its text).
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigProblem {
    pub path: PathBuf,
    pub start: u32,
    pub end: u32,
    pub message: String,
    /// A note that does not make the configuration unusable (shown, never failing a run).
    pub warning: bool,
}

/// Where a setting comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Default,
    Setting,
    CommandLine,
    /// A configuration file and the byte offset of the value.
    File(PathBuf, u32),
}

impl Origin {
    pub fn describe(&self, line_of: &dyn Fn(&Path, u32) -> u32) -> String {
        match self {
            Origin::Default => "default".into(),
            Origin::Setting => "editor setting".into(),
            Origin::CommandLine => "command line".into(),
            Origin::File(p, at) => format!("{}:{}", p.display(), line_of(p, *at) + 1),
        }
    }
}

/// A glob of a configuration file, with the folder it is relative to.
#[derive(Clone, Debug)]
pub struct Glob {
    pub text: String,
    pub base: PathBuf,
    parts: Vec<String>,
}

impl Glob {
    fn new(text: &str, base: &Path) -> Result<Glob, String> {
        if text.is_empty() {
            return Err("an empty path".into());
        }
        if text.contains('\\') {
            return Err("write paths with / (a \\ is not a separator here)".into());
        }
        if text.starts_with('/') || text.as_bytes().get(1) == Some(&b':') {
            return Err("paths are relative to the folder that holds .dvicode".into());
        }
        let mut parts: Vec<String> = text.split('/').filter(|p| !p.is_empty() && *p != ".").map(fold_case).collect();
        if parts.iter().skip_while(|p| *p == "..").any(|p| p == "..") {
            return Err("\"..\" may only start a path".into());
        }
        // `../x`: `x` in the folder above.
        let mut base = base.to_path_buf();
        while parts.first().is_some_and(|p| p == "..") {
            parts.remove(0);
            base = base.parent().ok_or("the path leaves the file system's root")?.to_path_buf();
        }
        if parts.len() > 64 {
            return Err("too many parts".into());
        }
        Ok(Glob { text: text.to_string(), base, parts })
    }

    /// Whether `path` (absolute) is matched, or is in a folder that is.
    pub fn matches(&self, path: &Path) -> bool {
        let Some(rel) = relative(&self.base, path) else { return false };
        if rel.first().is_some_and(|c| c == "..") {
            return false;
        }
        let rel: Vec<String> = rel.iter().map(|s| fold_case(s)).collect();
        // The pattern against each leading part of the path: a folder covers what is in it.
        match_prefixes(&self.parts, &rel)
    }
}

fn fold_case(s: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

/// `path` relative to `base`, both absolute, lexically (`..` for each level up).
fn relative(base: &Path, path: &Path) -> Option<Vec<String>> {
    let norm = |p: &Path| -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for c in p.components() {
            match c {
                Component::ParentDir => {
                    v.pop();
                }
                Component::CurDir => {}
                c => v.push(fold_case(&c.as_os_str().to_string_lossy())),
            }
        }
        v
    };
    let (b, p) = (norm(base), norm(path));
    let common = b.iter().zip(&p).take_while(|(x, y)| x == y).count();
    if common == 0 && !b.is_empty() {
        return None;
    }
    let mut out: Vec<String> = std::iter::repeat("..".to_string()).take(b.len() - common).collect();
    out.extend(p[common..].iter().cloned());
    Some(out)
}

/// Whether `pat` matches `comps[..k]` for some `k`: one pass per pattern part from the first,
/// each keeping the set of path positions the parts so far can end at (no backtracking).
fn match_prefixes(pat: &[String], comps: &[String]) -> bool {
    let n = comps.len();
    // at[j]: the parts so far match comps[..j].
    let mut at = vec![false; n + 1];
    at[0] = true;
    for p in pat {
        let mut next = vec![false; n + 1];
        if p == "**" {
            let mut any = false;
            for j in 0..=n {
                any |= at[j];
                next[j] = any;
            }
        } else {
            for j in 0..n {
                next[j + 1] = at[j] && dvi_project::glob::match_component(p, &comps[j]);
            }
        }
        at = next;
        if !at.contains(&true) {
            return false;
        }
    }
    at.contains(&true)
}

/// One `rules` entry.
#[derive(Clone, Debug)]
struct RuleEntry {
    /// The rules it sets (a guide ID sets every rule mapped to it).
    targets: Vec<&'static str>,
    guide: bool,
    level: Option<Level>,
    params: FxHashMap<String, i64>,
    at: u32,
}

#[derive(Clone, Debug)]
struct Override {
    at: u32,
    paths: Vec<Glob>,
    rulesets: Option<(Vec<Ruleset>, u32)>,
    rules: Vec<RuleEntry>,
    reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PathEntry {
    pub glob: Glob,
    pub reason: Option<String>,
    pub at: u32,
}

/// One configuration file, read and checked.
#[derive(Clone, Debug, Default)]
pub struct ConfigFile {
    pub path: PathBuf,
    /// The folder that holds `.dvicode`: what its paths are relative to.
    pub base: PathBuf,
    pub text: Arc<str>,
    pub problems: Vec<ConfigProblem>,
    extends_none: bool,
    rulesets: Option<(Vec<Ruleset>, u32)>,
    rules: Vec<RuleEntry>,
    overrides: Vec<Override>,
    owned: Option<Vec<PathEntry>>,
    exclude: Vec<PathEntry>,
    include: Vec<PathEntry>,
    examples: Vec<PathEntry>,
    pub uvm: Option<String>,
    /// The baseline file named (absolute), and where its value is written.
    pub baseline: Option<PathBuf>,
    baseline_at: (u32, u32),
}

const TOP_KEYS: &[&str] = &["$schema", "comment", "extends", "rulesets", "rules", "overrides", "owned", "exclude", "include", "examples", "uvm", "baseline"];

impl ConfigFile {
    /// Reads `text` as the configuration at `path` (`<base>/.dvicode/lint.json`).
    pub fn parse(path: &Path, text: &str) -> ConfigFile {
        let base = path.parent().and_then(Path::parent).unwrap_or(Path::new("")).to_path_buf();
        let mut c = ConfigFile { path: path.to_path_buf(), base, text: Arc::from(text), ..Default::default() };
        let root = match json::parse(text) {
            Ok(r) => r,
            Err(e) => {
                c.problem(e.at, e.at, format!("Not valid JSON: {}", e.message));
                return c;
            }
        };
        let Some(members) = root.as_object() else {
            c.problem(root.start, root.end, "The configuration must be a JSON object".into());
            return c;
        };
        for m in members {
            let v = &m.value;
            match m.key.as_str() {
                "$schema" | "comment" => {
                    if v.as_str().is_none() {
                        c.problem(v.start, v.end, format!("\"{}\" must be a string", m.key));
                    }
                }
                "extends" => match v.as_str() {
                    Some("parent") => {}
                    Some("none") => c.extends_none = true,
                    _ => c.problem(v.start, v.end, "\"extends\" must be \"parent\" or \"none\"".into()),
                },
                "rulesets" => c.rulesets = c.rulesets_of(v),
                "rules" => c.rules = c.rules_of(v),
                "overrides" => c.overrides_of(v),
                "owned" => c.owned = Some(c.paths_of(v, "owned")),
                "exclude" => c.exclude = c.paths_of(v, "exclude"),
                "include" => c.include = c.paths_of(v, "include"),
                "examples" => c.examples = c.paths_of(v, "examples"),
                "baseline" => match v.as_str().map(|p| Glob::new(p, &c.base)) {
                    Some(Ok(g)) if !dvi_project::glob::has_wildcard(&g.text) => {
                        c.baseline = Some(c.base.join(&g.text));
                        c.baseline_at = (v.start, v.end);
                    }
                    _ => c.problem(v.start, v.end, "\"baseline\" must be a file path relative to the folder that holds .dvicode".into()),
                },
                "uvm" => match v.as_str() {
                    Some(s @ ("1.2" | "ieee")) => c.uvm = Some(s.to_string()),
                    _ => c.problem(v.start, v.end, "\"uvm\" must be \"1.2\" or \"ieee\"".into()),
                },
                _ => c.unknown_key(m, TOP_KEYS),
            }
        }
        c
    }

    fn problem(&mut self, start: u32, end: u32, message: String) {
        self.problems.push(ConfigProblem { path: self.path.clone(), start, end: end.max(start), message, warning: false });
    }

    fn note(&mut self, start: u32, end: u32, message: String) {
        self.problems.push(ConfigProblem { path: self.path.clone(), start, end: end.max(start), message, warning: true });
    }

    /// The problems that make it unusable.
    pub fn errors(&self) -> impl Iterator<Item = &ConfigProblem> {
        self.problems.iter().filter(|p| !p.warning)
    }

    fn unknown_key(&mut self, m: &Member, known: &[&str]) {
        let hint = known.iter().find(|k| k.eq_ignore_ascii_case(&m.key) || edit_distance(k, &m.key) <= 2).map(|k| format!(" (did you mean \"{k}\"?)")).unwrap_or_default();
        self.problem(m.key_start, m.key_end, format!("Unknown key \"{}\"{hint}", m.key));
    }

    fn rulesets_of(&mut self, v: &Node) -> Option<(Vec<Ruleset>, u32)> {
        let Some(items) = v.as_array() else {
            self.problem(v.start, v.end, "\"rulesets\" must be an array of ruleset names".into());
            return None;
        };
        let mut out = Vec::new();
        for i in items {
            match i.as_str().and_then(Ruleset::parse) {
                Some(r) => out.push(r),
                None => {
                    let names: Vec<&str> = Ruleset::ALL.iter().map(|r| r.name()).collect();
                    self.problem(i.start, i.end, format!("Unknown ruleset (known: {})", names.join(", ")));
                }
            }
        }
        Some((out, v.start))
    }

    fn rules_of(&mut self, v: &Node) -> Vec<RuleEntry> {
        let Some(members) = v.as_object() else {
            self.problem(v.start, v.end, "\"rules\" must be an object of rule IDs".into());
            return Vec::new();
        };
        let mut out = Vec::new();
        for m in members {
            let (targets, guide) = if let Some(meta) = rules::meta(&m.key) {
                (vec![meta.id], false)
            } else if looks_like_guide_id(&m.key) {
                if guide_level(&m.key).is_none() {
                    self.problem(m.key_start, m.key_end, format!("The style guide has no rule {}", m.key));
                    continue;
                }
                (rules::all().iter().map(|r| r.meta()).filter(|r| r.guide.contains(&m.key.as_str())).map(|r| r.id).collect(), true)
            } else {
                let hint = rules::all().iter().map(|r| r.meta().id).find(|id| edit_distance(id, &m.key) <= 3).map(|k| format!(" (did you mean \"{k}\"?)")).unwrap_or_default();
                self.problem(m.key_start, m.key_end, format!("Unknown rule \"{}\"{hint}", m.key));
                continue;
            };
            let (level, params) = self.setting_of(&m.value, &targets);
            out.push(RuleEntry { targets, guide, level, params, at: m.value.start });
        }
        // Guide IDs first, then rule IDs: a rule's own ID wins over its guide rule.
        out.sort_by_key(|e| !e.guide);
        out
    }

    /// A rule's value: a level, or `{ "level": ..., "comment": ..., <parameter>: n }`.
    fn setting_of(&mut self, v: &Node, targets: &[&'static str]) -> (Option<Level>, FxHashMap<String, i64>) {
        let mut params = FxHashMap::default();
        let level_of = |n: &Node| n.as_str().and_then(Level::parse);
        let bad_level = |c: &mut Self, n: &Node| c.problem(n.start, n.end, format!("A level must be one of {}", Level::NAMES.join(", ")));
        match &v.value {
            Value::String(_) => match level_of(v) {
                Some(l) => (Some(l), params),
                None => {
                    bad_level(self, v);
                    (None, params)
                }
            },
            Value::Object(members) => {
                let mut level = None;
                for m in members {
                    match m.key.as_str() {
                        "level" => match level_of(&m.value) {
                            Some(l) => level = Some(l),
                            None => bad_level(self, &m.value),
                        },
                        "comment" => {
                            if m.value.as_str().is_none() {
                                self.problem(m.value.start, m.value.end, "\"comment\" must be a string".into());
                            }
                        }
                        name => {
                            let metas: Vec<_> = targets.iter().filter_map(|t| rules::meta(t)).filter_map(|r| r.params.iter().find(|p| p.name == name)).collect();
                            if metas.is_empty() {
                                self.problem(m.key_start, m.key_end, format!("Unknown parameter \"{name}\""));
                                continue;
                            }
                            match m.value.value {
                                Value::Number(n) if n.fract() == 0.0 && metas.iter().all(|p| (p.min as f64..=p.max as f64).contains(&n)) => {
                                    params.insert(name.to_string(), n as i64);
                                }
                                _ => self.problem(m.value.start, m.value.end, format!("\"{name}\" must be a whole number from {} to {}", metas[0].min, metas[0].max)),
                            }
                        }
                    }
                }
                (level, params)
            }
            _ => {
                self.problem(v.start, v.end, "A rule is set with a level or an object with \"level\"".into());
                (None, params)
            }
        }
    }

    fn overrides_of(&mut self, v: &Node) {
        let Some(items) = v.as_array() else {
            self.problem(v.start, v.end, "\"overrides\" must be an array".into());
            return;
        };
        for item in items {
            let Some(members) = item.as_object() else {
                self.problem(item.start, item.end, "An override must be an object with \"paths\"".into());
                continue;
            };
            let mut o = Override { at: item.start, paths: Vec::new(), rulesets: None, rules: Vec::new(), reason: None };
            let mut has_paths = false;
            for m in members {
                match m.key.as_str() {
                    "paths" => {
                        has_paths = true;
                        o.paths = self.paths_of(&m.value, "paths").into_iter().map(|p| p.glob).collect();
                    }
                    "rulesets" => o.rulesets = self.rulesets_of(&m.value),
                    "rules" => o.rules = self.rules_of(&m.value),
                    "reason" | "comment" => match m.value.as_str() {
                        Some(s) if m.key == "reason" => o.reason = Some(s.to_string()),
                        Some(_) => {}
                        None => self.problem(m.value.start, m.value.end, format!("\"{}\" must be a string", m.key)),
                    },
                    _ => self.unknown_key(m, &["paths", "rulesets", "rules", "reason", "comment"]),
                }
            }
            if !has_paths {
                self.problem(item.start, item.start + 1, "An override needs \"paths\"".into());
                continue;
            }
            self.overrides.push(o);
        }
    }

    /// Paths: strings, or `{ "path": ..., "reason": ..., "comment": ... }`.
    fn paths_of(&mut self, v: &Node, key: &str) -> Vec<PathEntry> {
        let Some(items) = v.as_array() else {
            self.problem(v.start, v.end, format!("\"{key}\" must be an array of paths"));
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in items {
            let (text, reason, at) = match &i.value {
                Value::String(s) => (s.clone(), None, i.start),
                Value::Object(members) => {
                    let mut path = None;
                    let mut reason = None;
                    for m in members {
                        match (m.key.as_str(), m.value.as_str()) {
                            ("path", Some(p)) => path = Some((p.to_string(), m.value.start)),
                            ("reason", Some(r)) => reason = Some(r.to_string()),
                            ("comment", Some(_)) => {}
                            ("path" | "reason" | "comment", None) => self.problem(m.value.start, m.value.end, format!("\"{}\" must be a string", m.key)),
                            _ => self.unknown_key(m, &["path", "reason", "comment"]),
                        }
                    }
                    let Some((p, at)) = path else {
                        self.problem(i.start, i.start + 1, "A path entry needs \"path\"".into());
                        continue;
                    };
                    (p, reason, at)
                }
                _ => {
                    self.problem(i.start, i.end, "A path must be a string or an object with \"path\"".into());
                    continue;
                }
            };
            match Glob::new(&text, &self.base) {
                Ok(glob) => out.push(PathEntry { glob, reason, at }),
                Err(e) => self.problem(i.start, i.end, format!("Bad path \"{text}\": {e}")),
            }
        }
        out
    }
}

/// `path` below `root`, compared as the file system compares names (ignoring case on Windows
/// and macOS).
fn strip_root<'p>(path: &'p Path, root: &Path) -> Option<&'p Path> {
    if let Ok(r) = path.strip_prefix(root) {
        return Some(r);
    }
    if !cfg!(any(windows, target_os = "macos")) {
        return None;
    }
    let n = root.components().count();
    let same = path.components().zip(root.components()).take_while(|(a, b)| a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()).count();
    if same != n {
        return None;
    }
    let mut it = path.components();
    for _ in 0..n {
        it.next();
    }
    Some(it.as_path())
}

/// Small edit distance, for "did you mean" (names are short).
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 3 || a.len() > 64 || b.len() > 64 {
        return usize::MAX;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The settings that do not come from files: defaults (the editor's settings) and the command
/// line.
#[derive(Clone, Debug)]
pub struct Defaults {
    pub enabled: bool,
    pub rulesets: Vec<Ruleset>,
    /// `rulesets` came from an editor setting (else the built-in default).
    pub from_setting: bool,
    pub cli_rulesets: Option<Vec<Ruleset>>,
    pub cli_rules: Vec<(&'static str, Level)>,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults { enabled: true, rulesets: vec![Ruleset::Recommended], from_setting: false, cli_rulesets: None, cli_rules: Vec::new() }
    }
}

/// A rule's resolved setting.
#[derive(Clone, Debug)]
pub struct RuleSetting {
    pub level: Level,
    pub params: Option<FxHashMap<String, i64>>,
    pub origin: Origin,
}

/// Whether a file is linted, and why not.
#[derive(Clone, Debug, PartialEq)]
pub enum Owned {
    Yes(Origin),
    No(String),
}

impl Owned {
    pub fn is_owned(&self) -> bool {
        matches!(self, Owned::Yes(_))
    }
}

/// The configuration of one file.
#[derive(Clone, Debug)]
pub struct FileConfig {
    pub enabled: bool,
    /// Every rule, with its level (`Off` included).
    pub rules: FxHashMap<&'static str, RuleSetting>,
    pub owned: Owned,
    /// The guide's example exceptions apply (`examples`).
    pub example: bool,
    pub uvm: Option<String>,
    /// The baseline that applies (the nearest configuration that names one).
    pub baseline: Option<PathBuf>,
    pub rulesets: (Vec<Ruleset>, Origin),
    /// The reasons of the overrides that matched, for `--print-config`.
    pub overrides: Vec<(Origin, Option<String>)>,
}

/// What [`ConfigSet`] needs from its caller: reading files (the engine's guarded reads, or an
/// editor's unsaved text) and knowing where the project and the UVM library are.
pub trait ConfigEnv {
    fn read(&self, path: &Path) -> Option<String>;
    fn is_file(&self, path: &Path) -> bool;
}

/// The configurations of one project, read as asked and kept until [`ConfigSet::invalidate`].
pub struct ConfigSet {
    pub root: PathBuf,
    pub defaults: Defaults,
    /// `--config FILE`: this file only, no lookup.
    pub explicit: Option<PathBuf>,
    /// What was read, and the generation it was read in: a read that started before the last
    /// invalidation is not kept.
    files: Mutex<(u64, FxHashMap<PathBuf, Option<Arc<ConfigFile>>>)>,
}

impl ConfigSet {
    pub fn new(root: &Path, defaults: Defaults, explicit: Option<PathBuf>) -> ConfigSet {
        ConfigSet { root: root.to_path_buf(), defaults, explicit, files: Mutex::default() }
    }

    /// Forgets what was read (a configuration file changed).
    pub fn invalidate(&self) {
        let mut f = self.files.lock().unwrap();
        f.0 += 1;
        f.1.clear();
    }

    fn load(&self, env: &dyn ConfigEnv, path: &Path) -> Option<Arc<ConfigFile>> {
        let generation = {
            let f = self.files.lock().unwrap();
            if let Some(c) = f.1.get(path) {
                return c.clone();
            }
            f.0
        };
        let c = if env.is_file(path) {
            let mut c = match env.read(path) {
                Some(t) => ConfigFile::parse(path, &t),
                None => {
                    let mut c = ConfigFile::parse(path, "{}");
                    c.problem(0, 0, "Cannot read this configuration file".into());
                    c
                }
            };
            // The baseline it names: missing is a note (it may not be written yet); unreadable
            // is an error, since its findings would come back without a word.
            if let Some(b) = c.baseline.clone() {
                let (s, e) = c.baseline_at;
                if !env.is_file(&b) {
                    c.note(s, e, format!("The baseline {} does not exist (write it with `dvicode lint --write-baseline`)", b.display()));
                } else if let Err(why) = env.read(&b).ok_or_else(|| "cannot read it".to_string()).and_then(|t| super::baseline::Baseline::read(&t)) {
                    c.problem(s, e, format!("The baseline {}: {why}", b.display()));
                }
            }
            Some(Arc::new(c))
        } else {
            None
        };
        let mut f = self.files.lock().unwrap();
        if f.0 == generation {
            f.1.insert(path.to_path_buf(), c.clone());
        }
        c
    }

    /// The configuration files that apply to `file`, outermost first.
    pub fn chain(&self, env: &dyn ConfigEnv, file: &Path) -> Vec<Arc<ConfigFile>> {
        if let Some(p) = &self.explicit {
            return self.load(env, p).into_iter().collect();
        }
        let mut dirs = vec![self.root.clone()];
        if let Some(rel) = file.parent().and_then(|d| strip_root(d, &self.root)) {
            let mut d = self.root.clone();
            for c in rel.components() {
                d.push(c);
                dirs.push(d.clone());
            }
        }
        let mut out: Vec<Arc<ConfigFile>> = Vec::new();
        for d in dirs {
            if let Some(c) = self.load(env, &d.join(".dvicode").join("lint.json")) {
                if c.extends_none {
                    out.clear();
                }
                out.push(c);
            }
        }
        out
    }

    /// The configuration files read so far (with their problems).
    pub fn loaded(&self) -> Vec<Arc<ConfigFile>> {
        let mut v: Vec<Arc<ConfigFile>> = self.files.lock().unwrap().1.values().flatten().cloned().collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }

    /// The configuration of `file`. `outside`: why the file is not the project's although its
    /// path may say so (built in, the UVM library, a link to elsewhere): it is then owned only
    /// when a configuration includes it.
    pub fn for_file(&self, env: &dyn ConfigEnv, file: &Path, outside: Option<&str>) -> FileConfig {
        let chain = self.chain(env, file);
        let d = &self.defaults;
        let default_origin = if d.from_setting { Origin::Setting } else { Origin::Default };
        let mut rulesets = (d.rulesets.clone(), default_origin.clone());
        let mut explicit: FxHashMap<&'static str, (Option<Level>, FxHashMap<String, i64>, Origin)> = FxHashMap::default();
        let mut overrides = Vec::new();
        let mut uvm = None;
        let mut baseline = None;
        let apply = |explicit: &mut FxHashMap<&'static str, (Option<Level>, FxHashMap<String, i64>, Origin)>, entries: &[RuleEntry], path: &Path| {
            for e in entries {
                for t in &e.targets {
                    let x = explicit.entry(t).or_insert((None, FxHashMap::default(), Origin::Default));
                    if e.level.is_some() {
                        x.0 = e.level;
                        x.2 = Origin::File(path.to_path_buf(), e.at);
                    }
                    for (k, v) in &e.params {
                        x.1.insert(k.clone(), *v);
                    }
                }
            }
        };
        // Ownership: the owned roots (the project by default), then each file's exclusions and
        // inclusions in order; the last match wins.
        let mut owned_roots: Vec<(Glob, Origin)> = vec![(Glob::new(".", &self.root).unwrap(), Origin::Default)];
        let mut marks: Vec<(bool, &PathEntry, &Path)> = Vec::new();
        let mut example = false;
        for c in &chain {
            if let Some((r, at)) = &c.rulesets {
                rulesets = (r.clone(), Origin::File(c.path.clone(), *at));
            }
            apply(&mut explicit, &c.rules, &c.path);
            for o in &c.overrides {
                if o.paths.iter().any(|g| g.matches(file)) {
                    if let Some((r, at)) = &o.rulesets {
                        rulesets = (r.clone(), Origin::File(c.path.clone(), *at));
                    }
                    apply(&mut explicit, &o.rules, &c.path);
                    overrides.push((Origin::File(c.path.clone(), o.at), o.reason.clone()));
                }
            }
            if let Some(o) = &c.owned {
                owned_roots = o.iter().map(|p| (p.glob.clone(), Origin::File(c.path.clone(), p.at))).collect();
            }
            for p in &c.exclude {
                marks.push((false, p, &c.path));
            }
            for p in &c.include {
                marks.push((true, p, &c.path));
            }
            if !c.examples.is_empty() {
                example = c.examples.iter().any(|p| p.glob.matches(file));
            }
            if c.uvm.is_some() {
                uvm = c.uvm.clone();
            }
            if c.baseline.is_some() {
                baseline = c.baseline.clone();
            }
        }
        if let Some(r) = &d.cli_rulesets {
            rulesets = (r.clone(), Origin::CommandLine);
        }
        for (id, level) in &d.cli_rules {
            let x = explicit.entry(id).or_insert((None, FxHashMap::default(), Origin::CommandLine));
            x.0 = Some(*level);
            x.2 = Origin::CommandLine;
        }
        let mut rules_out = FxHashMap::default();
        for r in rules::all() {
            let m = r.meta();
            let chosen = rulesets.0.iter().any(|s| m.rulesets.iter().any(|of| s.includes(*of)));
            let (level, origin) = match explicit.get(m.id) {
                Some((Some(l), _, o)) => (*l, o.clone()),
                _ if chosen => (m.default_level, rulesets.1.clone()),
                _ => (Level::Off, rulesets.1.clone()),
            };
            let params = explicit.get(m.id).map(|x| x.1.clone()).filter(|p| !p.is_empty());
            rules_out.insert(m.id, RuleSetting { level, params, origin });
        }
        let mut owned = match owned_roots.iter().find(|(g, _)| g.matches(file)) {
            Some((_, o)) if outside.is_none() => Owned::Yes(o.clone()),
            Some(_) => Owned::No(outside.unwrap_or_default().to_string()),
            None => Owned::No("outside the owned folders".into()),
        };
        for (include, p, path) in marks {
            if p.glob.matches(file) {
                owned = if include {
                    Owned::Yes(Origin::File(path.to_path_buf(), p.at))
                } else {
                    Owned::No(format!("excluded by \"{}\" ({}){}", p.glob.text, path.display(), p.reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default()))
                };
            }
        }
        FileConfig { enabled: d.enabled, rules: rules_out, owned, example, uvm, baseline, rulesets, overrides }
    }

    /// A hash of everything that decides configurations (for the JSON output).
    pub fn hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = rustc_hash::FxHasher::default();
        for c in self.loaded() {
            c.path.hash(&mut h);
            c.text.hash(&mut h);
        }
        self.defaults.enabled.hash(&mut h);
        for r in &self.defaults.rulesets {
            r.name().hash(&mut h);
        }
        if let Some(r) = &self.defaults.cli_rulesets {
            for x in r {
                x.name().hash(&mut h);
            }
        }
        for (id, l) in &self.defaults.cli_rules {
            id.hash(&mut h);
            l.hash(&mut h);
        }
        h.finish()
    }
}

/// Reads through the engine's guarded file access.
pub struct DiskEnv;

impl ConfigEnv for DiskEnv {
    fn read(&self, path: &Path) -> Option<String> {
        dvi_project::fsx::read(path).ok().and_then(|b| String::from_utf8(b).ok())
    }

    fn is_file(&self, path: &Path) -> bool {
        dvi_project::fsx::is_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(Vec<(PathBuf, String)>);

    impl ConfigEnv for Mem {
        fn read(&self, path: &Path) -> Option<String> {
            self.0.iter().find(|(p, _)| p == path).map(|(_, t)| t.clone())
        }
        fn is_file(&self, path: &Path) -> bool {
            self.0.iter().any(|(p, _)| p == path)
        }
    }

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\proj")
        } else {
            PathBuf::from("/proj")
        }
    }

    fn cfg_at(dir: &str, text: &str) -> (PathBuf, String) {
        let mut p = root();
        if !dir.is_empty() {
            p.push(dir);
        }
        (p.join(".dvicode").join("lint.json"), text.to_string())
    }

    fn file(rel: &str) -> PathBuf {
        let mut p = root();
        for c in rel.split('/') {
            p.push(c);
        }
        p
    }

    fn level(set: &ConfigSet, env: &Mem, f: &str, rule: &str) -> Level {
        set.for_file(env, &file(f), None).rules[rule].level
    }

    #[test]
    fn defaults_rulesets_and_levels() {
        let env = Mem(vec![]);
        let set = ConfigSet::new(&root(), Defaults::default(), None);
        assert_eq!(level(&set, &env, "tb/a.sv", "randomize-result-discarded"), Level::Warning);
        assert_eq!(level(&set, &env, "tb/a.sv", "trailing-whitespace"), Level::Off);
        let env = Mem(vec![cfg_at("", r#"{ "rulesets": ["hexk"] }"#)]);
        let set = ConfigSet::new(&root(), Defaults::default(), None);
        assert_eq!(level(&set, &env, "tb/a.sv", "trailing-whitespace"), Level::Info);
        assert_eq!(level(&set, &env, "tb/a.sv", "randomize-result-discarded"), Level::Warning);
    }

    #[test]
    fn resolution_order() {
        let env = Mem(vec![
            cfg_at("", r#"{ "rules": { "LNG-10": "error", "randomize-result-discarded": "info" },
                "overrides": [ { "paths": ["tb/legacy"], "rules": { "randomize-result-discarded": "off" }, "reason": "old" } ] }"#),
            cfg_at("tb", r#"{ "rulesets": ["hexk"], "rules": { "FMT-2": "warning" } }"#),
        ]);
        let set = ConfigSet::new(&root(), Defaults::default(), None);
        // A rule's own ID wins over its guide ID in one file.
        assert_eq!(level(&set, &env, "a.sv", "randomize-result-discarded"), Level::Info);
        assert_eq!(level(&set, &env, "tb/legacy/x.sv", "randomize-result-discarded"), Level::Off);
        // The nested file inherits the rules and replaces the rulesets.
        assert_eq!(level(&set, &env, "tb/b.sv", "randomize-result-discarded"), Level::Info);
        assert_eq!(level(&set, &env, "tb/b.sv", "trailing-whitespace"), Level::Warning);
        assert_eq!(level(&set, &env, "b.sv", "trailing-whitespace"), Level::Off);
        let mut d = Defaults::default();
        d.cli_rules.push(("randomize-result-discarded", Level::Error));
        let set = ConfigSet::new(&root(), d, None);
        assert_eq!(level(&set, &env, "tb/legacy/x.sv", "randomize-result-discarded"), Level::Error);
    }

    #[test]
    fn extends_none_starts_over() {
        let env = Mem(vec![cfg_at("", r#"{ "rules": { "randomize-result-discarded": "error" } }"#), cfg_at("ip", r#"{ "extends": "none" }"#)]);
        let set = ConfigSet::new(&root(), Defaults::default(), None);
        assert_eq!(level(&set, &env, "ip/a.sv", "randomize-result-discarded"), Level::Warning);
        assert_eq!(level(&set, &env, "a.sv", "randomize-result-discarded"), Level::Error);
    }

    #[test]
    fn ownership() {
        let env = Mem(vec![cfg_at("", r#"{ "exclude": ["vendor", {"path": "**/*_reg_block.sv", "reason": "generated"}], "include": ["vendor/ours/**", "../shared/pkg"] }"#)]);
        let set = ConfigSet::new(&root(), Defaults::default(), None);
        let own = |f: &str| set.for_file(&env, &file(f), None).owned.is_owned();
        assert!(own("tb/a.sv"));
        assert!(!own("vendor/x/a.sv"));
        assert!(own("vendor/ours/a.sv"));
        assert!(!own("tb/x_reg_block.sv"));
        let shared = root().parent().unwrap().join("shared").join("pkg").join("a.sv");
        assert!(set.for_file(&env, &shared, None).owned.is_owned());
        let other = root().parent().unwrap().join("other").join("a.sv");
        assert!(!set.for_file(&env, &other, None).owned.is_owned());
        assert!(!set.for_file(&env, &file("tb/uvm_pkg.sv"), Some("part of the UVM library")).owned.is_owned());
    }

    #[test]
    fn parent_folders_and_many_double_stars() {
        // `..` names the folder above, which covers the project folder too.
        let g = Glob::new("..", &root()).unwrap();
        assert!(g.matches(&file("tb/a.sv")));
        let g = Glob::new("../shared/**/*.sv", &root()).unwrap();
        assert!(g.matches(&root().parent().unwrap().join("shared").join("x").join("a.sv")));
        assert!(!g.matches(&file("shared/a.sv")));
        // No backtracking blow-up.
        let deep = format!("{}missing.sv", "**/".repeat(40));
        let g = Glob::new(&deep, &root()).unwrap();
        let mut p = root();
        for i in 0..60 {
            p.push(format!("d{i}"));
        }
        assert!(!g.matches(&p.join("a.sv")));
    }

    #[test]
    fn globs_are_anchored() {
        let g = Glob::new("tb/**", &root()).unwrap();
        assert!(g.matches(&file("tb/a/b.sv")));
        assert!(!g.matches(&file("x/tb/a.sv")));
        let g = Glob::new("**/*.svh", &root()).unwrap();
        assert!(g.matches(&file("a/b/c.svh")));
        assert!(Glob::new("/abs", &root()).is_err());
        assert!(Glob::new("a/../b", &root()).is_err());
    }

    #[test]
    fn problems_have_places() {
        let t = r#"{ "rulez": {}, "rules": { "no-such-rule": "warning", "LNG-99": "off", "LNG-10": "loud" }, "rulesets": ["strict"], "exclude": ["/x"] }"#;
        let c = ConfigFile::parse(Path::new("/p/.dvicode/lint.json"), t);
        let msgs: Vec<(&str, &str)> = c.problems.iter().map(|p| (&t[p.start as usize..p.end as usize], p.message.as_str())).collect();
        assert!(msgs.iter().any(|(at, m)| *at == "\"rulez\"" && m.contains("did you mean \"rules\"")), "{msgs:?}");
        assert!(msgs.iter().any(|(at, m)| *at == "\"no-such-rule\"" && m.contains("Unknown rule")));
        assert!(msgs.iter().any(|(at, m)| *at == "\"LNG-99\"" && m.contains("no rule LNG-99")));
        assert!(msgs.iter().any(|(at, m)| *at == "\"loud\"" && m.contains("level")));
        assert!(msgs.iter().any(|(at, m)| *at == "\"strict\"" && m.contains("ruleset")));
        assert!(msgs.iter().any(|(at, _)| *at == "\"/x\""));
        let c = ConfigFile::parse(Path::new("/p/.dvicode/lint.json"), "{ // c\n}");
        assert!(c.problems[0].message.contains("comment"));
    }

    #[test]
    fn guide_ids_without_rules_are_accepted() {
        let c = ConfigFile::parse(Path::new("/p/.dvicode/lint.json"), r#"{ "rules": { "REU-1": "off" } }"#);
        assert!(c.problems.is_empty(), "{:?}", c.problems);
    }
}
