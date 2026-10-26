//! `dvicode lint`: lint a project from the command line (for CI), with the same engine and
//! configuration as the editor.
//!
//! Exit status, the first that applies: 3 internal failure, 2 usage or configuration error
//! (or nothing to lint), 4 analysis incomplete (unless `--allow-incomplete`), 1 findings at
//! or above `--fail-on` or compile errors, 0.

use crate::check::{is_input_failure, load_setup};
use dvi_analysis::lint::config::{ConfigSet, Defaults, DiskEnv, FileConfig};
use dvi_analysis::lint::{rules, FileLint, Level, Ruleset};
use dvi_analysis::{Analysis, UnitKind, Vfs};
use dvi_syntax::{FileId, Severity};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const USAGE: &str = "usage: dvicode lint <dir> [PATH...] [--build FILE] [--env K=V] [--config FILE]
         [--rulesets r1,r2] [--rule ID=LEVEL]... [--format text|json] [--output FILE]
         [--fail-on error|warning|info|never] [--all-contexts] [--jobs N]
         [--allow-incomplete] [--no-fail-on-compile-errors] [--show-suppressed] [--stats]
       dvicode lint --list-rules [--format text|json|md]
       dvicode lint --explain ID
       dvicode lint --print-config FILE [<dir>] [--config FILE] [--rulesets ...] [--rule ...]";

struct Opts {
    dir: Option<PathBuf>,
    paths: Vec<PathBuf>,
    build: Option<PathBuf>,
    env: FxHashMap<String, String>,
    config: Option<PathBuf>,
    rulesets: Option<Vec<Ruleset>>,
    rules: Vec<(&'static str, Level)>,
    format: String,
    output: Option<PathBuf>,
    fail_on: Option<Level>,
    all_contexts: bool,
    jobs: usize,
    allow_incomplete: bool,
    fail_on_compile: bool,
    show_suppressed: bool,
    stats: bool,
    list_rules: bool,
    explain: Option<String>,
    print_config: Option<PathBuf>,
    schema: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        dir: None,
        paths: Vec::new(),
        build: None,
        env: FxHashMap::default(),
        config: None,
        rulesets: None,
        rules: Vec::new(),
        format: "text".into(),
        output: None,
        fail_on: Some(Level::Warning),
        all_contexts: false,
        jobs: 0,
        allow_incomplete: false,
        fail_on_compile: true,
        show_suppressed: false,
        stats: false,
        list_rules: false,
        explain: None,
        print_config: None,
        schema: false,
    };
    let mut i = 0;
    let value = |i: &mut usize, name: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| format!("{name} needs a value"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--build" => o.build = Some(PathBuf::from(value(&mut i, a)?)),
            "--env" => {
                let kv = value(&mut i, a)?;
                let (k, v) = kv.split_once('=').ok_or("--env needs NAME=VALUE")?;
                o.env.insert(k.into(), v.into());
            }
            "--config" => o.config = Some(PathBuf::from(value(&mut i, a)?)),
            "--rulesets" => {
                let v = value(&mut i, a)?;
                let mut sets = Vec::new();
                for s in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    sets.push(Ruleset::parse(s).ok_or_else(|| format!("unknown ruleset {s}"))?);
                }
                o.rulesets = Some(sets);
            }
            "--rule" => {
                let v = value(&mut i, a)?;
                let (id, level) = v.split_once('=').ok_or("--rule needs ID=LEVEL")?;
                let meta = rules::meta(id).ok_or_else(|| format!("unknown rule {id} (see --list-rules)"))?;
                let level = Level::parse(level).ok_or_else(|| format!("unknown level {level} (off, hint, info, warning, error)"))?;
                o.rules.push((meta.id, level));
            }
            "--format" => o.format = value(&mut i, a)?,
            "--output" => o.output = Some(PathBuf::from(value(&mut i, a)?)),
            "--fail-on" => {
                let v = value(&mut i, a)?;
                o.fail_on = match v.as_str() {
                    "never" => None,
                    l => Some(Level::parse(l).filter(|l| *l != Level::Off).ok_or_else(|| format!("--fail-on takes error, warning, info, hint or never, not {l}"))?),
                };
            }
            "--all-contexts" => o.all_contexts = true,
            "--jobs" => o.jobs = value(&mut i, a)?.parse().map_err(|_| "--jobs needs a number")?,
            "--allow-incomplete" => o.allow_incomplete = true,
            "--no-fail-on-compile-errors" => o.fail_on_compile = false,
            "--show-suppressed" => o.show_suppressed = true,
            "--stats" => o.stats = true,
            "--list-rules" => o.list_rules = true,
            "--explain" => o.explain = Some(value(&mut i, a)?),
            "--print-config" => o.print_config = Some(PathBuf::from(value(&mut i, a)?)),
            "--schema" => o.schema = true,
            "-h" | "--help" => return Err(String::new()),
            _ if a.starts_with("--") => return Err(format!("unknown option {a}")),
            _ if o.dir.is_none() => o.dir = Some(PathBuf::from(a)),
            _ => o.paths.push(PathBuf::from(a)),
        }
        i += 1;
    }
    if !matches!(o.format.as_str(), "text" | "json" | "md") {
        return Err(format!("unknown format {}", o.format));
    }
    Ok(o)
}

pub fn run(args: &[String]) -> i32 {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("lint: {e}");
            }
            eprintln!("{USAGE}");
            return if e.is_empty() { 0 } else { 2 };
        }
    };
    if o.schema {
        return emit(&o, &(serde_json::to_string_pretty(&schema()).unwrap_or_default() + "\n"));
    }
    if o.list_rules {
        return emit(&o, &list_rules(&o.format));
    }
    if let Some(id) = &o.explain {
        return match explain(id) {
            Some(t) => emit(&o, &t),
            None => {
                eprintln!("lint: no rule {id} (see --list-rules)");
                2
            }
        };
    }
    let defaults = Defaults { cli_rulesets: o.rulesets.clone(), cli_rules: o.rules.clone(), ..Defaults::default() };
    if let Some(file) = &o.print_config {
        let dir = o.dir.clone().or_else(|| file.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
        let root = crate::check::canonical_path(&dir);
        let file = crate::check::canonical_path(file);
        let set = ConfigSet::new(&root, defaults, o.config.clone());
        let cfg = set.for_file(&DiskEnv, &file, false);
        let text = print_config(&set, &file, &cfg);
        let code = emit(&o, &text);
        return if set.loaded().iter().any(|c| !c.problems.is_empty()) { 2 } else { code };
    }
    let Some(dir) = o.dir.clone() else {
        eprintln!("{USAGE}");
        return 2;
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lint_project(&o, &dir, defaults))) {
        Ok(code) => code,
        Err(_) => {
            eprintln!("lint: internal error (see the message above)");
            3
        }
    }
}

/// Writes `text` to `--output` or stdout; 3 when it cannot be written.
fn emit(o: &Opts, text: &str) -> i32 {
    let r = match &o.output {
        Some(p) => std::fs::write(p, text),
        None => std::io::stdout().write_all(text.as_bytes()),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("lint: cannot write the output: {e}");
            3
        }
    }
}

/// A linted file's results in each context it was linted in.
struct Linted {
    file: FileId,
    results: Vec<FileLint>,
}

fn lint_project(o: &Opts, dir: &Path, defaults: Defaults) -> i32 {
    let t0 = std::time::Instant::now();
    let (setup, _) = match load_setup("lint", dir, o.build.as_deref(), o.env.clone()) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let root = setup.root.clone();
    let setup = Arc::new(setup);
    let a = Analysis::build(Arc::new(Vfs::new()), setup);
    let build_ms = t0.elapsed().as_millis();
    let set = ConfigSet::new(&root, defaults, o.config.clone().map(|c| crate::check::canonical_path(&c)));
    if let Some(c) = &o.config {
        if !c.is_file() {
            eprintln!("lint: cannot read the configuration {}", c.display());
            return 2;
        }
    }
    // The files to report, in path order.
    let mut files = a.project_files();
    files.retain(|f| !a.vfs.is_builtin(*f));
    files.sort_by_key(|f| a.path(*f));
    let filters: Vec<PathBuf> = o.paths.iter().map(|p| crate::check::canonical_path(p)).collect();
    if !filters.is_empty() {
        files.retain(|f| {
            let p = a.path(*f);
            filters.iter().any(|x| p.starts_with(x))
        });
    }
    // Configurations first: a broken configuration lints nothing.
    let configs: Vec<FileConfig> = files.iter().map(|f| a.lint_config(&set, &DiskEnv, *f)).collect();
    let mut config_problems = 0;
    for c in set.loaded() {
        for p in &c.problems {
            let (line, col) = line_col(&c.text, p.start);
            eprintln!("{}:{}:{}: error: {}", p.path.display(), line + 1, col + 1, p.message);
            config_problems += 1;
        }
    }
    if config_problems > 0 {
        eprintln!("lint: {config_problems} problem(s) in the lint configuration");
        return 2;
    }
    let owned: Vec<(FileId, &FileConfig)> = files.iter().copied().zip(configs.iter()).filter(|(_, c)| c.owned.is_owned()).collect();
    if owned.is_empty() {
        eprintln!("lint: no file to lint (no project file is owned: check \"owned\", \"exclude\" and the paths given)");
        return 2;
    }
    // Compile failure, whatever is owned: errors anywhere in the project, or build inputs that
    // were not read.
    let input_failures = a.setup.config.problems.iter().filter(|p| is_input_failure(p)).count();
    let compile_errors: usize = a.project_files().par_iter().map(|f| a.file_diagnostics(*f).iter().filter(|d| d.severity == Severity::Error).count()).sum();
    let t1 = std::time::Instant::now();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(o.jobs).build().expect("thread pool");
    let linted: Vec<Linted> = pool.install(|| {
        owned
            .par_iter()
            .map(|(f, cfg)| {
                let units: Vec<Option<dvi_analysis::UnitId>> = if o.all_contexts {
                    a.units_for(*f).into_iter().filter(|u| a.unit(*u).kind != UnitKind::Builtin).map(Some).collect()
                } else {
                    vec![None]
                };
                Linted { file: *f, results: units.into_iter().map(|u| a.lint_file(*f, cfg, u)).collect() }
            })
            .collect()
    });
    let lint_ms = t1.elapsed().as_millis();
    // Findings: one per place, rule and message, with the contexts it holds in.
    let mut findings: Vec<Value> = Vec::new();
    let mut text = String::new();
    let (mut counts, mut suppressed, mut shown_fail) = (FxHashMap::<Level, usize>::default(), 0usize, false);
    let mut unknown: FxHashMap<String, u32> = FxHashMap::default();
    let mut incomplete: Vec<Value> = Vec::new();
    let mut linted_files = 0usize;
    for l in &linted {
        let path = a.path(l.file);
        let rel = path.strip_prefix(&root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| path.to_string_lossy().to_string());
        let mut merged: Vec<(&dvi_analysis::lint::Finding, Vec<Value>)> = Vec::new();
        for r in &l.results {
            if r.context.is_some() {
                linted_files += 1;
            }
            let ctx = r.context.as_ref().map(|c| context_dto(c, &root));
            for x in &r.findings {
                match merged.iter_mut().find(|(y, _)| y.rule == x.rule && y.span == x.span && y.message == x.message) {
                    Some((_, ctxs)) => ctxs.extend(ctx.clone()),
                    None => merged.push((x, ctx.clone().into_iter().collect())),
                }
            }
            for ((rule, reason), n) in &r.unknown {
                *unknown.entry(format!("{rule}: {reason}")).or_default() += n;
            }
            for (rule, reason) in &r.incomplete {
                incomplete.push(json!({ "path": rel, "rule": rule, "reason": reason }));
            }
        }
        for (x, ctxs) in merged {
            let range = a.to_range(x.span);
            let (line, col) = range.map_or((0, 0), |r| (r.start.line + 1, r.start.character + 1));
            if x.suppressed.is_some() {
                suppressed += 1;
            } else {
                *counts.entry(x.level).or_default() += 1;
                shown_fail |= o.fail_on.is_some_and(|f| x.level >= f);
            }
            if x.suppressed.is_none() || o.show_suppressed {
                let meta = rules::meta(x.rule);
                let tag = match meta.and_then(|m| m.guide.first()) {
                    Some(g) => format!("{}/{g}", x.rule),
                    None => x.rule.to_string(),
                };
                let waived = x.suppressed.as_ref().map(|s| format!(" (suppressed: {s})")).unwrap_or_default();
                text.push_str(&format!("{}:{line}:{col}: {}: {} [{tag}]{waived}\n", path.display(), x.level.as_str(), x.message));
            }
            let related: Vec<Value> = x
                .related
                .iter()
                .filter_map(|(s, m)| {
                    let r = a.to_range(*s)?;
                    let p = a.path(s.file);
                    Some(json!({ "path": p.strip_prefix(&root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| p.to_string_lossy().to_string()), "range": r, "message": m }))
                })
                .collect();
            findings.push(json!({
                "rule": x.rule,
                "subcheck": x.subcheck,
                "level": x.level,
                "message": x.message,
                "path": rel,
                "range": range,
                "contexts": ctxs,
                "related": related,
                "suppressed": x.suppressed,
            }));
        }
    }
    let complete = incomplete.is_empty();
    let n = |l: Level| counts.get(&l).copied().unwrap_or(0);
    let total: usize = counts.values().sum();
    let mut unknown_v: Vec<(String, u32)> = unknown.into_iter().collect();
    unknown_v.sort();
    let summary = format!(
        "dvicode lint: {total} finding(s) ({} errors, {} warnings, {} info, {} hints), {suppressed} suppressed, {} unknown; files: {} owned, {linted_files} linted; compile: {compile_errors} errors{}{}",
        n(Level::Error),
        n(Level::Warning),
        n(Level::Info),
        n(Level::Hint),
        unknown_v.iter().map(|(_, n)| *n as usize).sum::<usize>(),
        owned.len(),
        if input_failures > 0 { format!(", {input_failures} build inputs not read") } else { String::new() },
        if complete { String::new() } else { format!("; incomplete in {} place(s)", incomplete.len()) },
    );
    let out = match o.format.as_str() {
        "json" => {
            let v = json!({
                "version": 1,
                "tool": { "name": "dvicode", "version": env!("CARGO_PKG_VERSION") },
                "config": { "hash": format!("{:016x}", set.hash()) },
                "complete": complete,
                "files": { "owned": owned.len(), "linted": linted_files },
                "compile": { "errors": compile_errors, "buildInputsNotRead": input_failures },
                "unknown": unknown_v.iter().map(|(k, n)| (k.clone(), json!(n))).collect::<serde_json::Map<String, Value>>(),
                "incomplete": incomplete,
                "findings": findings,
            });
            serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
        }
        _ => text,
    };
    if emit(o, &out) != 0 {
        return 3;
    }
    eprintln!("{summary}");
    if o.stats {
        eprintln!("dvicode lint: build {build_ms} ms, lint {lint_ms} ms");
        for (k, v) in &unknown_v {
            eprintln!("  unknown {k}: {v}");
        }
        for i in &incomplete {
            eprintln!("  incomplete {i}");
        }
    }
    if !complete && !o.allow_incomplete {
        return 4;
    }
    if shown_fail || o.fail_on_compile && (compile_errors > 0 || input_failures > 0) {
        return 1;
    }
    0
}

/// The JSON Schema of `.dvicode/lint.json` (editors validate and complete the file with it).
pub fn schema() -> Value {
    let metas: Vec<_> = rules::all().iter().map(|r| r.meta()).collect();
    let level = json!({ "$ref": "#/definitions/level" });
    let mut rule_props = serde_json::Map::new();
    for m in &metas {
        let mut obj = serde_json::Map::new();
        obj.insert("level".into(), level.clone());
        obj.insert("comment".into(), json!({ "type": "string" }));
        for p in m.params {
            obj.insert(p.name.into(), json!({ "type": "integer", "minimum": p.min, "maximum": p.max, "default": p.default, "description": p.doc }));
        }
        rule_props.insert(
            m.id.into(),
            json!({
                "markdownDescription": format!("{}\n\n{}", m.summary, m.why),
                "oneOf": [level.clone(), { "type": "object", "properties": obj, "additionalProperties": false }],
            }),
        );
    }
    for (g, _) in dvi_analysis::lint::guide_ids::GUIDE_IDS {
        let checked: Vec<&str> = metas.iter().filter(|m| m.guide.contains(g)).map(|m| m.id).collect();
        let d = if checked.is_empty() { format!("Style guide rule {g} (no lint rule checks it yet)") } else { format!("Style guide rule {g}: sets {}", checked.join(", ")) };
        rule_props.insert((*g).into(), json!({ "description": d, "allOf": [{ "$ref": "#/definitions/setting" }] }));
    }
    let rulesets: Vec<&str> = Ruleset::ALL.iter().map(|r| r.name()).collect();
    let rules = json!({ "type": "object", "properties": rule_props, "additionalProperties": false, "description": "Rule IDs or style guide rule IDs, with a level or { \"level\", \"comment\", parameters }" });
    let paths = json!({ "$ref": "#/definitions/paths" });
    let sets = json!({ "$ref": "#/definitions/rulesets" });
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "DVICode lint configuration",
        "type": "object",
        "additionalProperties": false,
        "definitions": {
            "level": { "enum": Level::NAMES, "description": "off, hint, info, warning or error" },
            "setting": { "oneOf": [level.clone(), { "type": "object", "properties": { "level": level.clone(), "comment": { "type": "string" } }, "additionalProperties": false }] },
            "path": { "type": "string", "description": "A path or glob relative to the folder holding .dvicode, with / separators; a folder covers what is in it" },
            "paths": { "type": "array", "items": { "oneOf": [{ "$ref": "#/definitions/path" }, { "type": "object", "properties": { "path": { "$ref": "#/definitions/path" }, "reason": { "type": "string" }, "comment": { "type": "string" } }, "required": ["path"], "additionalProperties": false }] } },
            "rulesets": { "type": "array", "items": { "enum": rulesets }, "description": "recommended: provable correctness rules; recommended-preview: correctness rules still under review; hexk: the HEXK SV/UVM Style Guide (includes the others)" },
            "rules": rules,
        },
        "properties": {
            "$schema": { "type": "string" },
            "comment": { "type": "string" },
            "extends": { "enum": ["parent", "none"], "description": "parent (default): start from the configuration above; none: from the defaults" },
            "rulesets": sets.clone(),
            "rules": { "$ref": "#/definitions/rules" },
            "overrides": { "type": "array", "items": { "type": "object", "required": ["paths"], "additionalProperties": false, "properties": {
                "paths": { "type": "array", "items": { "$ref": "#/definitions/path" } },
                "rulesets": sets,
                "rules": { "$ref": "#/definitions/rules" },
                "reason": { "type": "string", "description": "Why these paths are linted differently" },
                "comment": { "type": "string" },
            } } },
            "owned": paths.clone(),
            "exclude": paths.clone(),
            "include": paths.clone(),
            "examples": paths,
            "uvm": { "enum": ["1.2", "ieee"] },
        },
    })
}

fn context_dto(c: &dvi_analysis::select::ContextKey, root: &Path) -> Value {
    let r = c.root.strip_prefix(root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| c.root.to_string_lossy().to_string());
    json!({ "root": r, "invocation": c.invocation, "nth": c.nth })
}

fn line_col(text: &str, at: u32) -> (usize, usize) {
    let before = &text[..(at as usize).min(text.len())];
    let line = before.matches('\n').count();
    let col = before.rfind('\n').map_or(before.chars().count(), |n| before[n + 1..].chars().count());
    (line, col)
}

fn list_rules(format: &str) -> String {
    let metas: Vec<_> = rules::all().iter().map(|r| r.meta()).collect();
    match format {
        "json" => serde_json::to_string_pretty(&metas.iter().map(|m| rule_json(m)).collect::<Vec<_>>()).unwrap_or_default() + "\n",
        "md" => {
            let mut s = String::from("| Rule | Guide | Rulesets | Level | Summary |\n|---|---|---|---|---|\n");
            for m in &metas {
                let sets: Vec<&str> = m.rulesets.iter().map(|r| r.name()).collect();
                s.push_str(&format!("| `{}` | {} | {} | {} | {} |\n", m.id, m.guide.join(", "), sets.join(", "), m.default_level.as_str(), m.summary));
            }
            s
        }
        _ => {
            let mut s = String::new();
            for m in &metas {
                let sets: Vec<&str> = m.rulesets.iter().map(|r| r.name()).collect();
                s.push_str(&format!("{:<32} {:<8} {:<22} {}\n", m.id, m.default_level.as_str(), sets.join(","), m.summary));
            }
            s
        }
    }
}

pub fn rule_json(m: &dvi_analysis::lint::RuleMeta) -> Value {
    json!({
        "id": m.id,
        "guide": m.guide,
        "rulesets": m.rulesets,
        "defaultLevel": m.default_level,
        "subchecks": m.subchecks,
        "summary": m.summary,
        "why": m.why,
        "params": m.params,
        "docs": m.docs_url(),
    })
}

fn explain(id: &str) -> Option<String> {
    let metas: Vec<_> = rules::all().iter().map(|r| r.meta()).filter(|m| m.id == id || m.guide.contains(&id)).collect();
    if metas.is_empty() {
        return None;
    }
    let mut s = String::new();
    for m in metas {
        let sets: Vec<&str> = m.rulesets.iter().map(|r| r.name()).collect();
        s.push_str(&format!("{}: {}\n\n{}\n\n", m.id, m.summary, m.why));
        if !m.guide.is_empty() {
            s.push_str(&format!("Style guide: {}\n", m.guide.join(", ")));
        }
        s.push_str(&format!("Rulesets: {}; default level: {}\n", sets.join(", "), m.default_level.as_str()));
        if !m.subchecks.is_empty() {
            s.push_str(&format!("Subchecks: {}\n", m.subchecks.join(", ")));
        }
        for p in m.params {
            s.push_str(&format!("Parameter {} ({}..{}, default {}): {}\n", p.name, p.min, p.max, p.default, p.doc));
        }
        s.push_str(&format!("Waive with: // {} waived: <reason>\nDocumentation: {}\n\n", m.guide.first().copied().unwrap_or(m.id), m.docs_url()));
    }
    Some(s)
}

fn print_config(set: &ConfigSet, file: &Path, cfg: &FileConfig) -> String {
    let texts: FxHashMap<PathBuf, Arc<str>> = set.loaded().iter().map(|c| (c.path.clone(), c.text.clone())).collect();
    let line_of = |p: &Path, at: u32| texts.get(p).map_or(0, |t| line_col(t, at).0 as u32);
    let mut s = format!("Configuration of {}\n", file.display());
    for c in set.chain(&DiskEnv, file) {
        s.push_str(&format!("  from {}\n", c.path.display()));
    }
    for c in set.loaded() {
        for p in &c.problems {
            let (l, col) = line_col(&c.text, p.start);
            s.push_str(&format!("  problem: {}:{}:{}: {}\n", p.path.display(), l + 1, col + 1, p.message));
        }
    }
    s.push_str(&format!("linted: {}\n", if !cfg.enabled { "no (lint is turned off)".to_string() } else { match &cfg.owned {
        dvi_analysis::lint::config::Owned::Yes(o) => format!("yes ({})", o.describe(&line_of)),
        dvi_analysis::lint::config::Owned::No(why) => format!("no: {why}"),
    } }));
    let sets: Vec<&str> = cfg.rulesets.0.iter().map(|r| r.name()).collect();
    s.push_str(&format!("rulesets: {} ({})\n", sets.join(", "), cfg.rulesets.1.describe(&line_of)));
    for (o, reason) in &cfg.overrides {
        s.push_str(&format!("override: {}{}\n", o.describe(&line_of), reason.as_ref().map(|r| format!(" (reason: {r})")).unwrap_or_default()));
    }
    if cfg.example {
        s.push_str("examples: yes (the guide's example exceptions apply)\n");
    }
    let mut ids: Vec<&&str> = cfg.rules.keys().collect();
    ids.sort();
    for id in ids {
        let r = &cfg.rules[*id];
        let params = r.params.as_ref().map(|p| {
            let mut v: Vec<String> = p.iter().map(|(k, v)| format!("{k}={v}")).collect();
            v.sort();
            format!(" {}", v.join(" "))
        });
        s.push_str(&format!("  {:<32} {:<8}{} ({})\n", id, r.level.as_str(), params.unwrap_or_default(), r.origin.describe(&line_of)));
    }
    let guide: Vec<&str> = dvi_analysis::lint::guide_ids::GUIDE_IDS.iter().map(|(i, _)| *i).filter(|g| !rules::all().iter().any(|r| r.meta().guide.contains(g))).collect();
    s.push_str(&format!("guide rules not checked by any lint rule: {}\n", guide.len()));
    s
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    const SRC: &str = "package p;\n  class c;\n    rand int a;\n    function void f();\n      randomize();\n    endfunction\n  endclass\nendpackage\n";

    fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dvicode-lintcli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".dvicode")).unwrap();
        let mut build = String::new();
        for (f, text) in files {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
            if f.ends_with(".sv") {
                build.push_str(f);
                build.push('\n');
            }
        }
        std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
        root
    }

    /// Runs `lint` with `args` after the project folder; the exit status and the output.
    fn lint(root: &PathBuf, args: &[&str]) -> (i32, String) {
        let out = root.join("out.txt");
        let _ = std::fs::remove_file(&out);
        let mut v: Vec<String> = vec![root.to_string_lossy().to_string()];
        v.extend(args.iter().map(|s| s.to_string()));
        v.extend(["--output".to_string(), out.to_string_lossy().to_string()]);
        let code = super::run(&v);
        (code, std::fs::read_to_string(&out).unwrap_or_default())
    }

    #[test]
    fn exit_statuses() {
        let root = project("exit", &[("p.sv", SRC)]);
        let (code, text) = lint(&root, &[]);
        assert_eq!(code, 1, "{text}");
        assert!(text.contains("p.sv:5:7: warning: The result of randomize() is discarded"), "{text}");
        assert!(text.contains("[randomize-result-discarded/LNG-10]"), "{text}");
        assert_eq!(lint(&root, &["--fail-on", "never"]).0, 0);
        assert_eq!(lint(&root, &["--fail-on", "error"]).0, 0);
        assert_eq!(lint(&root, &["--rule", "randomize-result-discarded=error", "--fail-on", "error"]).0, 1);
        assert_eq!(lint(&root, &["--rule", "no-such-rule=error"]).0, 2);
        assert_eq!(lint(&root, &["--rulesets", "strict"]).0, 2);
        // A broken configuration lints nothing.
        std::fs::write(root.join(".dvicode/lint.json"), r#"{ "rules": { "LNG-10": "loud" } }"#).unwrap();
        assert_eq!(lint(&root, &[]).0, 2);
        // Nothing owned: an error, not a clean run.
        std::fs::write(root.join(".dvicode/lint.json"), r#"{ "exclude": ["."] }"#).unwrap();
        assert_eq!(lint(&root, &[]).0, 2);
        std::fs::write(root.join(".dvicode/lint.json"), r#"{ "rules": { "LNG-10": "off" } }"#).unwrap();
        assert_eq!(lint(&root, &[]).0, 0);
    }

    #[test]
    fn compile_errors_fail_unless_asked_not_to() {
        let root = project("compile", &[("p.sv", "module m;\n  assign x = undeclared_y;\nendmodule\n")]);
        assert_eq!(lint(&root, &[]).0, 1);
        assert_eq!(lint(&root, &["--fail-on", "never"]).0, 1);
        assert_eq!(lint(&root, &["--no-fail-on-compile-errors"]).0, 0);
    }

    #[test]
    fn json_output() {
        let root = project("json", &[("p.sv", SRC)]);
        let (code, text) = lint(&root, &["--format", "json"]);
        assert_eq!(code, 1);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["complete"], true);
        assert_eq!(v["files"]["owned"], 1);
        let f = &v["findings"][0];
        assert_eq!((f["rule"].as_str(), f["level"].as_str(), f["path"].as_str()), (Some("randomize-result-discarded"), Some("warning"), Some("p.sv")));
        assert_eq!((f["range"]["start"]["line"].as_u64(), f["range"]["start"]["character"].as_u64()), (Some(4), Some(6)));
        assert_eq!(f["contexts"][0]["root"], "p.sv");
    }

    #[test]
    fn print_config_and_rule_lists() {
        let root = project("print", &[("p.sv", SRC), (".dvicode/lint.json", r#"{ "rules": { "LNG-10": "error" } }"#)]);
        let out = root.join("cfg.txt");
        let file = root.join("p.sv");
        let code = super::run(&["--print-config".into(), file.to_string_lossy().to_string(), root.to_string_lossy().to_string(), "--output".into(), out.to_string_lossy().to_string()]);
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("linted: yes"), "{text}");
        assert!(text.lines().any(|l| l.contains("randomize-result-discarded") && l.contains("error") && l.contains("lint.json:1")), "{text}");
        let list = super::list_rules("json");
        let v: serde_json::Value = serde_json::from_str(&list).unwrap();
        assert!(v.as_array().unwrap().iter().any(|r| r["id"] == "uvm-registration-mismatch"));
        assert!(super::explain("LNG-10").unwrap().contains("randomize-result-discarded"));
        assert!(super::explain("nothing").is_none());
    }

    #[test]
    fn the_extension_ships_the_current_schema() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../client/schemas/lint.schema.json");
        let shipped = std::fs::read_to_string(&p).expect("client/schemas/lint.schema.json").replace("\r\n", "\n");
        let now = serde_json::to_string_pretty(&super::schema()).unwrap() + "\n";
        assert!(shipped == now, "client/schemas/lint.schema.json is out of date: run `dvicode-server lint --schema --output client/schemas/lint.schema.json`");
    }
}
