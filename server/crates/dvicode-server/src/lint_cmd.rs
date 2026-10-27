//! `dvicode lint`: lint a project from the command line (for CI), with the same engine and
//! configuration as the editor.
//!
//! Exit status, the first that applies: 3 internal failure, 2 usage or configuration error
//! (or nothing to lint), 4 analysis incomplete (unless `--allow-incomplete`), 1 findings at
//! or above `--fail-on` or compile errors, 0.

use crate::check::{is_input_failure, load_setup};
use dvi_analysis::lint::config::{ConfigSet, Defaults, DiskEnv, FileConfig};
use crate::lint_out::{self, Item, Kept};
use dvi_analysis::lint::baseline::{self, Baseline, Fingerprint};
use dvi_analysis::lint::{rules, FileLint, Level, Ruleset};
use dvi_analysis::{Analysis, UnitKind, Vfs};
use dvi_syntax::FileId;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const USAGE: &str = "usage: dvicode lint <dir> [PATH...] [--build FILE] [--env K=V] [--config FILE]
         [--rulesets r1,r2] [--rule ID=LEVEL]... [--format text|json|sarif|junit|github]
         [--output FILE] [--fail-on error|warning|info|never] [--all-contexts] [--jobs N]
         [--output-sarif FILE] [--baseline FILE] [--write-baseline FILE]
         [--changed-since REF [--changed-exact]]
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
    baseline: Option<PathBuf>,
    write_baseline: Option<PathBuf>,
    docs: Option<PathBuf>,
    /// SARIF written there too (for CI: annotations on stdout, SARIF for code scanning).
    output_sarif: Option<PathBuf>,
    changed_since: Option<String>,
    changed_exact: bool,
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
        baseline: None,
        write_baseline: None,
        docs: None,
        output_sarif: None,
        changed_since: None,
        changed_exact: false,
    };
    let mut i = 0;
    // A value is never another option (`--output --stats` is a missing value).
    let value = |i: &mut usize, name: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).filter(|v| !v.starts_with("--")).cloned().ok_or_else(|| format!("{name} needs a value"))
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
            "--baseline" => o.baseline = Some(PathBuf::from(value(&mut i, a)?)),
            "--docs" => o.docs = Some(PathBuf::from(value(&mut i, a)?)),
            "--output-sarif" => o.output_sarif = Some(PathBuf::from(value(&mut i, a)?)),
            "--write-baseline" => o.write_baseline = Some(PathBuf::from(value(&mut i, a)?)),
            "--changed-since" => o.changed_since = Some(value(&mut i, a)?),
            "--changed-exact" => o.changed_exact = true,
            "-h" | "--help" => return Err(String::new()),
            _ if a.starts_with("--") => return Err(format!("unknown option {a}")),
            _ if o.dir.is_none() => o.dir = Some(PathBuf::from(a)),
            _ => o.paths.push(PathBuf::from(a)),
        }
        i += 1;
    }
    if o.changed_exact && o.changed_since.is_none() {
        return Err("--changed-exact goes with --changed-since REF".into());
    }
    if !matches!(o.format.as_str(), "text" | "json" | "md" | "sarif" | "junit" | "github") {
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
    if let Some(dir) = &o.docs {
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            for (name, text) in docs() {
                std::fs::write(dir.join(name), text)?;
            }
            Ok(())
        };
        return match write() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("lint: cannot write the rule pages in {}: {e}", dir.display());
                3
            }
        };
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
    // Files written are never reached through a symbolic link (one a project could plant).
    for p in [&o.output, &o.output_sarif, &o.write_baseline].into_iter().flatten() {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            eprintln!("lint: {} is a symbolic link: give the file itself", p.display());
            return 2;
        }
    }
    let defaults = Defaults { cli_rulesets: o.rulesets.clone(), cli_rules: o.rules.clone(), ..Defaults::default() };
    if let Some(c) = &o.config {
        if !c.is_file() {
            eprintln!("lint: cannot read the configuration {}", c.display());
            return 2;
        }
    }
    if let Some(file) = &o.print_config {
        // The project: the folder given, else the current one (as `lint` itself).
        let dir = o.dir.clone().unwrap_or_else(|| PathBuf::from("."));
        let root = crate::check::canonical_path(&dir);
        let file = crate::check::canonical_path(file);
        let set = ConfigSet::new(&root, defaults, o.config.as_ref().map(|c| crate::check::canonical_path(c)));
        let cfg = set.for_file(&DiskEnv, &file, None);
        let text = print_config(&set, &file, &cfg);
        let code = emit(&o, &text);
        if code != 0 {
            return code;
        }
        return if set.loaded().iter().any(|c| c.errors().next().is_some()) { 2 } else { 0 };
    }
    let Some(dir) = o.dir.clone() else {
        eprintln!("{USAGE}");
        return 2;
    };
    // `--jobs` bounds the whole run: the build and the compile checks too.
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(o.jobs).build() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lint: cannot start {} threads: {e}", o.jobs);
            return 3;
        }
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool.install(|| lint_project(&o, &dir, defaults)))) {
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
    // The files to report, in path order.
    let mut files = a.project_files();
    files.retain(|f| !a.vfs.is_builtin(*f));
    files.sort_by_key(|f| a.path(*f));
    // Each path given must exist and name project files; compared as canonical paths (the
    // build may spell them otherwise, or through a link).
    let filters: Vec<PathBuf> = o.paths.iter().map(|p| crate::check::canonical_path(p)).collect();
    if !filters.is_empty() {
        if let Some(p) = o.paths.iter().find(|p| !p.exists()) {
            eprintln!("lint: {} does not exist", p.display());
            return 2;
        }
        let canon: Vec<PathBuf> = files.iter().map(|f| crate::check::canonical_path(&a.path(*f))).collect();
        if let Some((p, _)) = o.paths.iter().zip(&filters).find(|(_, x)| !canon.iter().any(|c| c.starts_with(x))) {
            eprintln!("lint: {} holds no file of the project", p.display());
            return 2;
        }
        let keep: Vec<bool> = canon.iter().map(|c| filters.iter().any(|x| c.starts_with(x))).collect();
        let mut k = keep.into_iter();
        files.retain(|_| k.next().unwrap_or(false));
    }
    // Configurations first: a broken configuration lints nothing.
    let configs: Vec<FileConfig> = files.iter().map(|f| a.lint_config(&set, &DiskEnv, *f)).collect();
    let mut config_problems = 0;
    for c in set.loaded() {
        for p in c.problems.iter().chain(set.reason_notes(&DiskEnv, &c).iter()) {
            let (line, col) = line_col(&c.text, p.start);
            // A baseline that cannot be read is what `--write-baseline` writes again.
            let warning = p.warning || o.write_baseline.is_some() && p.message.starts_with("The baseline ");
            eprintln!("{}:{}:{}: {}: {}", lint_out::plain(&p.path.display().to_string()), line + 1, col + 1, if warning { "warning" } else { "error" }, lint_out::plain(&p.message));
            config_problems += usize::from(!warning);
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
    // Computed once before the files are checked in parallel: it runs parallel work itself, which
    // workers all waiting for it could not run.
    let _ = a.hidden_uses();
    // Every error (not the editor's first problems of a file), in every context with
    // --all-contexts; the first ones are printed, so that a failing run says why.
    let errors: Vec<(FileId, Vec<dvi_syntax::Diag>)> = a.project_files().par_iter().map(|f| (*f, a.file_errors(*f, o.all_contexts))).filter(|(_, e)| !e.is_empty()).collect();
    let compile_errors: usize = errors.iter().map(|(_, e)| e.len()).sum();
    let mut compile_list: Vec<Value> = Vec::new();
    for (f, e) in &errors {
        for d in e {
            if compile_list.len() == 100 {
                break;
            }
            let (l, c) = a.to_range(d.span).map_or((0, 0), |r| (r.start.line + 1, r.start.character + 1));
            eprintln!("{}:{l}:{c}: error: {}", lint_out::plain(&a.path(*f).display().to_string()), lint_out::plain(&d.message));
            compile_list.push(json!({ "path": a.path(*f).display().to_string(), "line": l, "column": c, "code": d.code, "message": d.message }));
        }
    }
    if compile_errors > compile_list.len() {
        eprintln!("lint: {} more compile errors", compile_errors - compile_list.len());
    }
    let t1 = std::time::Instant::now();
    let linted: Vec<Linted> = {
        owned
            .par_iter()
            .map(|(f, cfg)| {
                let units: Vec<Option<dvi_analysis::UnitId>> = if o.all_contexts {
                    a.units_for(*f).into_iter().filter(|u| a.unit(*u).kind != UnitKind::Builtin).map(Some).collect()
                } else {
                    vec![None]
                };
                // A failure in one file is reported and the others are still linted.
                let results = units
                    .into_iter()
                    .map(|u| {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.lint_file(*f, cfg, u))).unwrap_or_else(|_| {
                            let mut r = FileLint::default();
                            r.incomplete.push(("lint", "internal error"));
                            r
                        })
                    })
                    .collect();
                Linted { file: *f, results }
            })
            .collect()
    };
    let lint_ms = t1.elapsed().as_millis();
    let rel_of = |p: &Path| p.strip_prefix(&root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| p.to_string_lossy().to_string());
    // What changed, when only that is reported.
    let changed = match &o.changed_since {
        Some(rev) => match crate::changed::since(&root, rev, o.changed_exact) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("lint: --changed-since: {e}");
                return 2;
            }
        },
        None => None,
    };
    // Baselines: the one given, else each file's (from its configuration).
    let mut baselines: FxHashMap<PathBuf, Option<Baseline>> = FxHashMap::default();
    if let Some(b) = &o.baseline {
        let text = dvi_analysis::lint::read_control_file(b).ok_or_else(|| "cannot read it (a regular file of at most 256 MB)".to_string()).and_then(|t| String::from_utf8(t).map_err(|_| "not UTF-8".to_string()));
        match text.and_then(|t| Baseline::read(&t)) {
            Ok(x) => {
                baselines.insert(PathBuf::new(), Some(x));
            }
            Err(e) => {
                eprintln!("lint: the baseline {}: {e}", b.display());
                return 2;
            }
        }
    }
    let mut items: Vec<Item> = Vec::new();
    // Occurrences of equal core fingerprints, so that repeated statements differ.
    let mut seen_core: FxHashMap<u64, u32> = FxHashMap::default();
    let mut new_baseline: Vec<Fingerprint> = Vec::new();
    let mut unknown: FxHashMap<String, u32> = FxHashMap::default();
    let mut unknown_places: Vec<String> = Vec::new();
    let mut incomplete: Vec<Value> = Vec::new();
    let mut linted_files: Vec<String> = Vec::new();
    for (l, (_, cfg)) in linted.iter().zip(owned.iter()) {
        let path = a.path(l.file);
        let rel = rel_of(&path);
        // One finding per place, rule and message, with the contexts it holds in; it is
        // suppressed only when it is in every context. Each keeps the unit it was first found in
        // (its fingerprint names what encloses it there).
        let mut merged: Vec<(dvi_analysis::lint::Finding, Vec<Value>, Option<dvi_analysis::UnitId>)> = Vec::new();
        let mut index: FxHashMap<(&'static str, u32, u32, &str), usize> = FxHashMap::default();
        let mut unit = None;
        for r in &l.results {
            if r.context.is_some() && unit.is_none() {
                linted_files.push(rel.clone());
                unit = r.unit;
            }
            let ctx = r.context.as_ref().map(|c| context_dto(c, &root));
            for x in &r.findings {
                match index.get(&(x.rule, x.span.start, x.span.end, x.message.as_str())) {
                    Some(&i) => {
                        let (y, ctxs, _) = &mut merged[i];
                        ctxs.extend(ctx.clone());
                        if x.suppressed.is_none() {
                            y.suppressed = None;
                        }
                    }
                    None => {
                        index.insert((x.rule, x.span.start, x.span.end, x.message.as_str()), merged.len());
                        merged.push((x.clone(), ctx.clone().into_iter().collect(), r.unit));
                    }
                }
            }
            for ((rule, reason), n) in &r.unknown {
                *unknown.entry(format!("{rule}: {reason}")).or_default() += n;
            }
            for (rule, reason) in &r.incomplete {
                incomplete.push(json!({ "path": rel, "rule": rule, "reason": reason }));
            }
            // At most the 200 that --stats prints.
            for (rule, reason, at) in r.unknown_places.iter().take(200usize.saturating_sub(unknown_places.len())) {
                let (l, c) = a.to_range(*at).map_or((0, 0), |r| (r.start.line + 1, r.start.character + 1));
                unknown_places.push(format!("{}:{l}:{c}: {rule}: {reason}", path.display()));
            }
        }
        let Some(unit) = unit else { continue };
        let fps: Vec<Fingerprint> = merged.iter().map(|(x, _, u)| a.fingerprint(u.unwrap_or(unit), x, &root)).collect();
        // The baseline: what is not waived in the code.
        let open: Vec<usize> = (0..merged.len()).filter(|i| merged[*i].0.suppressed.is_none()).collect();
        let open_fps: Vec<Fingerprint> = open.iter().map(|i| fps[*i].clone()).collect();
        if o.write_baseline.is_some() {
            new_baseline.extend(open_fps.iter().cloned());
        }
        let base_key = if o.baseline.is_some() { Some(PathBuf::new()) } else { cfg.baseline.clone() };
        let mut in_base = vec![false; merged.len()];
        if let Some(k) = base_key {
            let b = baselines.entry(k.clone()).or_insert_with(|| dvi_analysis::lint::read_control_file(&k).and_then(|t| String::from_utf8(t).ok()).and_then(|t| Baseline::read(&t).ok()));
            if let Some(b) = b {
                for (j, hit) in b.take(&open_fps).into_iter().enumerate() {
                    in_base[open[j]] = hit;
                }
            }
        }
        for (i, (x, ctxs, _)) in merged.into_iter().enumerate() {
            // Numbered before the changed-lines filter: a finding's fingerprint does not
            // depend on what is reported.
            let fp = &fps[i];
            let core = fnv(format!("{}|{}|{}|{}|{}", fp.rule, fp.subcheck, fp.path, fp.symbol, fp.text).as_bytes());
            let nth = seen_core.entry(core).or_default();
            *nth += 1;
            let range = a.to_range(x.span);
            if let (Some(c), Some(r)) = (&changed, &range) {
                if !crate::changed::touches(c, &path, r.start.line + 1, r.end.line + 1) {
                    continue;
                }
            }
            let related = x.related.iter().filter_map(|(s, m)| Some((rel_of(&a.path(s.file)), a.to_range(*s)?, m.clone()))).collect();
            let kept = match (&x.suppressed, in_base[i]) {
                (Some(w), _) => Kept::InSource(w.clone()),
                (None, true) => Kept::Baseline,
                (None, false) => Kept::No,
            };
            let fingerprint = format!("{core:016x}:{nth}");
            let context_fingerprint = fp.around.clone();
            let outside = path.strip_prefix(&root).is_err();
            items.push(Item { rule: x.rule, subcheck: x.subcheck, level: x.level, message: x.message, path: path.display().to_string(), rel: rel.clone(), range, related, contexts: ctxs, kept, fingerprint, context_fingerprint, outside });
        }
    }
    if a.setup.config.invocations.iter().any(|i| i.truncated) {
        incomplete.push(json!({ "path": "", "rule": "build", "reason": "auto-configuration stopped at its limit of source files: describe the project with a build file" }));
    }
    let complete = incomplete.is_empty();
    let internal = incomplete.iter().any(|i| i["reason"] == "internal error");
    // An incomplete run writes no baseline (and fails, below), but still reports.
    let mut refused = false;
    if let Some(out) = o.write_baseline.as_ref().filter(|_| !complete) {
        eprintln!("lint: the baseline {} is not written: the analysis is incomplete (see --stats)", out.display());
        refused = true;
    }
    if let Some(out) = o.write_baseline.as_ref().filter(|_| complete) {
        let file = baseline::file_of(new_baseline, &format!("dvicode {}", env!("CARGO_PKG_VERSION")));
        let text = serde_json::to_string_pretty(&file).unwrap_or_default() + "\n";
        if let Err(e) = make_parent(out).and_then(|_| write_replacing(out, text.as_bytes())) {
            eprintln!("lint: cannot write the baseline {}: {e}", out.display());
            return 3;
        }
        eprintln!("dvicode lint: baseline of {} finding(s) written to {}", file.findings.iter().map(|e| e.count as usize).sum::<usize>(), out.display());
    }
    let mut counts = FxHashMap::<Level, usize>::default();
    let mut shown_fail = false;
    let (mut waived, mut baselined) = (0usize, 0usize);
    for x in &items {
        match x.kept {
            Kept::No => {
                *counts.entry(x.level).or_default() += 1;
                shown_fail |= o.fail_on.is_some_and(|f| x.level >= f);
            }
            Kept::InSource(_) => waived += 1,
            Kept::Baseline => baselined += 1,
        }
    }
    let n = |l: Level| counts.get(&l).copied().unwrap_or(0);
    let total: usize = counts.values().sum();
    let mut unknown_v: Vec<(String, u32)> = unknown.into_iter().collect();
    unknown_v.sort();
    let summary = format!(
        "dvicode lint: {total} finding(s) ({} errors, {} warnings, {} info, {} hints), {waived} suppressed, {baselined} in the baseline, {} unknown; files: {} owned, {} linted; compile: {compile_errors} errors{}{}",
        n(Level::Error),
        n(Level::Warning),
        n(Level::Info),
        n(Level::Hint),
        unknown_v.iter().map(|(_, n)| *n as usize).sum::<usize>(),
        owned.len(),
        linted_files.len(),
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
                "files": { "owned": owned.len(), "linted": linted_files.len() },
                "compile": { "errors": compile_errors, "buildInputsNotRead": input_failures, "first": compile_list },
                "unknown": unknown_v.iter().map(|(k, n)| (k.clone(), json!(n))).collect::<serde_json::Map<String, Value>>(),
                "incomplete": incomplete,
                "findings": lint_out::json_items(&items),
            });
            serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
        }
        "sarif" => sarif_text(&items, &root, complete),
        "junit" => lint_out::junit(&items, &linted_files, o.fail_on),
        "github" => lint_out::github(&items, std::env::current_dir().ok().map(|d| crate::check::canonical_path(&d)).as_deref()),
        _ => lint_out::text(&items, o.show_suppressed),
    };
    if emit(o, &out) != 0 {
        return 3;
    }
    if let Some(p) = &o.output_sarif {
        if let Err(e) = make_parent(p).and_then(|_| std::fs::write(p, sarif_text(&items, &root, complete))) {
            eprintln!("lint: cannot write {}: {e}", p.display());
            return 3;
        }
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
        for u in unknown_places.iter().take(200) {
            eprintln!("  unknown at {u}");
        }
    }
    if internal {
        eprintln!("lint: internal error while linting some files (see the messages above)");
        return 3;
    }
    if refused || !complete && !o.allow_incomplete {
        return 4;
    }
    if shown_fail || o.fail_on_compile && (compile_errors > 0 || input_failures > 0) {
        return 1;
    }
    0
}

/// Writes `path` whole or not at all: into a new file beside it (never one that exists), then
/// renamed over it.
fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for n in 0..100u32 {
        let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let done = f.write_all(bytes).and_then(|_| f.sync_all());
        drop(f);
        let done = done.and_then(|_| std::fs::rename(&tmp, path));
        if done.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        return done;
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free temporary name"))
}

fn sarif_text(items: &[Item], root: &Path, complete: bool) -> String {
    let mut root_uri = dvi_analysis::ide::url_of(root).to_string();
    if !root_uri.ends_with('/') {
        root_uri.push('/');
    }
    serde_json::to_string_pretty(&lint_out::sarif(items, &root_uri, complete, env!("CARGO_PKG_VERSION"))).unwrap_or_default() + "\n"
}

fn fnv(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The rule pages (`<id>.md`) and their index (`README.md`).
pub fn docs() -> Vec<(String, String)> {
    let metas: Vec<_> = rules::all().iter().map(|r| r.meta()).collect();
    let mut out = Vec::new();
    let mut index = String::from(
        "# DVICode Lint rules\n\n\
         Generated by `dvicode lint --docs docs/lint` from the rules themselves; do not edit.\n\n\
         Choose rulesets and levels in `.dvicode/lint.json` (see [the lint guide](../lint.md)). \
         `recommended` is on by default; `hexk` adds the checkable rules of the \
         [HEXK SystemVerilog/UVM Style Guide](https://github.com/hexkDotCom/sv-uvm-style-guide).\n\n\
         | Rule | Guide | Rulesets | Level | What it finds |\n|---|---|---|---|---|\n",
    );
    for m in &metas {
        let sets: Vec<&str> = m.rulesets.iter().map(|r| r.name()).collect();
        index.push_str(&format!("| [`{}`]({}.md) | {} | {} | {} | {} |\n", m.id, m.id, m.guide.join(", "), sets.join(", "), m.default_level.as_str(), m.summary.replace('|', "\\|")));
        let mut page = format!("# {}\n\n{}\n\n", m.id, m.summary);
        page.push_str(&format!("- Rulesets: {}\n- Default level: {}\n", sets.join(", "), m.default_level.as_str()));
        if !m.guide.is_empty() {
            let links: Vec<String> = m.guide.iter().map(|g| format!("[{g}](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)")).collect();
            page.push_str(&format!("- Style guide: {}\n", links.join(", ")));
        }
        if !m.subchecks.is_empty() {
            page.push_str(&format!("- Subchecks: {}\n", m.subchecks.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ")));
        }
        page.push('\n');
        page.push_str(dvi_analysis::lint::docs::page(m.id).unwrap_or(""));
        let id = m.guide.first().copied().unwrap_or(m.id);
        page.push_str(&format!(
            "\n## Waive\n\nOne finding: `// {id} waived: <reason>` on the line before the statement or declaration. \
             The rule: `\"{}\": \"off\"` in `.dvicode/lint.json`, for all files or under `overrides` for some.\n",
            m.id
        ));
        out.push((format!("{}.md", m.id), page));
    }
    out.push(("README.md".to_string(), index));
    out
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
        // The parameters of the rules it sets, in the range every one of them accepts (as the
        // configuration reader checks them).
        let mut obj = serde_json::Map::new();
        obj.insert("level".into(), level.clone());
        obj.insert("comment".into(), json!({ "type": "string" }));
        for p in metas.iter().filter(|m| m.guide.contains(g)).flat_map(|m| m.params) {
            let all = metas.iter().filter(|m| m.guide.contains(g)).flat_map(|m| m.params).filter(|q| q.name == p.name);
            let (min, max) = all.fold((i64::MIN, i64::MAX), |(lo, hi), q| (lo.max(q.min), hi.min(q.max)));
            obj.entry(p.name).or_insert_with(|| json!({ "type": "integer", "minimum": min, "maximum": max, "description": p.doc }));
        }
        rule_props.insert((*g).into(), json!({ "description": d, "oneOf": [level.clone(), { "type": "object", "properties": obj, "additionalProperties": false }] }));
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
            "formatter": { "enum": ["dvicode", "none"], "description": "The project's formatter (FMT-13): with \"dvicode\", the hexk rule formatter-drift reports lines DVICode's formatter would change" },
            "baseline": { "type": "string", "description": "A baseline file (written by `dvicode lint --write-baseline`), relative to the folder holding .dvicode: the findings it lists are not reported" },
        },
    })
}

fn context_dto(c: &dvi_analysis::select::ContextKey, root: &Path) -> Value {
    let r = c.root.strip_prefix(root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| c.root.to_string_lossy().to_string());
    json!({ "root": r, "invocation": c.invocation, "nth": c.nth, "before": c.before })
}

fn line_col(text: &str, at: u32) -> (usize, usize) {
    let mut at = (at as usize).min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let before = &text[..at];
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

/// Creates the folder an output file goes to (`.dvicode/` of a project that has none yet).
fn make_parent(p: &Path) -> std::io::Result<()> {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => std::fs::create_dir_all(d),
        _ => Ok(()),
    }
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
    fn a_missing_configuration_or_output_is_an_error() {
        let root = project("missing", &[("p.sv", SRC)]);
        assert_eq!(lint(&root, &["--config", "no-such.json"]).0, 2);
        // Writing into a folder fails: an internal failure, before anything else.
        let v: Vec<String> = vec![root.to_string_lossy().to_string(), "--output".into(), root.to_string_lossy().to_string()];
        assert_eq!(super::run(&v), 3);
        std::fs::write(root.join(".dvicode/lint.json"), "{ bad").unwrap();
        let file = root.join("p.sv");
        let v: Vec<String> = vec!["--print-config".into(), file.to_string_lossy().to_string(), root.to_string_lossy().to_string(), "--output".into(), root.to_string_lossy().to_string()];
        assert_eq!(super::run(&v), 3);
        let out = root.join("cfg.txt");
        let v: Vec<String> = vec!["--print-config".into(), file.to_string_lossy().to_string(), root.to_string_lossy().to_string(), "--output".into(), out.to_string_lossy().to_string()];
        assert_eq!(super::run(&v), 2);
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
    fn baselines_accept_the_findings_they_list() {
        let root = project("base", &[("p.sv", SRC)]);
        let b = root.join("base.json");
        let bs = b.to_string_lossy().to_string();
        assert_eq!(lint(&root, &["--write-baseline", &bs]).0, 1);
        let text = std::fs::read_to_string(&b).unwrap();
        assert!(text.contains("\"rule\": \"randomize-result-discarded\""), "{text}");
        assert_eq!(lint(&root, &["--baseline", &bs]).0, 0);
        // Lines moved: still accepted. A new finding: reported.
        std::fs::write(root.join("p.sv"), format!("// a new first line\n\n{}", SRC.replace("randomize();", "randomize();\n      randomize();"))).unwrap();
        let (code, out) = lint(&root, &["--baseline", &bs, "--format", "json"]);
        assert_eq!(code, 1, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let kept: Vec<&str> = v["findings"].as_array().unwrap().iter().map(|f| f["suppressed"].as_str().unwrap_or("-")).collect();
        assert_eq!(kept, ["baseline", "-"]);
        // Named in the configuration, the editor's way.
        std::fs::write(root.join(".dvicode/lint.json"), r#"{ "baseline": "base.json" }"#).unwrap();
        let (_, out) = lint(&root, &["--format", "json"]);
        assert!(out.contains("\"suppressed\": \"baseline\""), "{out}");
        // A broken baseline is an error.
        std::fs::write(&b, "{}").unwrap();
        assert_eq!(lint(&root, &["--baseline", &bs]).0, 2);
    }

    #[test]
    fn changed_code_only() {
        let root = project("changed", &[("p.sv", SRC), ("q.sv", &SRC.replace("package p", "package q"))]);
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git").arg("-C").arg(&root).args(args).output().map(|o| o.status.success()).unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        if std::process::Command::new("git").arg("--version").output().is_err() {
            return;
        }
        git(&["init", "-q"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "x"]);
        assert_eq!(lint(&root, &["--changed-since", "HEAD"]).0, 0);
        // A change in q.sv: only its finding.
        std::fs::write(root.join("q.sv"), SRC.replace("package p", "package q").replace("randomize();", "randomize(); randomize();")).unwrap();
        let (code, out) = lint(&root, &["--changed-since", "HEAD"]);
        assert_eq!(code, 1);
        assert!(out.contains("q.sv") && !out.contains("p.sv"), "{out}");
        assert_eq!(lint(&root, &["--changed-since", "no-such-rev"]).0, 2);
    }

    #[test]
    fn sarif_junit_and_github_outputs() {
        let root = project("formats", &[("p.sv", SRC)]);
        let (_, out) = lint(&root, &["--format", "sarif"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["runs"][0]["results"][0]["ruleId"], "randomize-result-discarded");
        assert_eq!(v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"], "p.sv");
        assert!(v["runs"][0]["originalUriBaseIds"]["%SRCROOT%"]["uri"].as_str().unwrap().starts_with("file:///"));
        let (_, out) = lint(&root, &["--format", "junit"]);
        assert!(out.contains("<testcase classname=\"randomize-result-discarded\" name=\"p.sv\">"), "{out}");
        let (_, out) = lint(&root, &["--format", "github"]);
        assert!(out.starts_with("::warning file=p.sv,title=randomize-result-discarded/LNG-10,line=5,col=7"), "{out}");
    }

    #[test]
    fn the_rule_pages_are_current() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/lint");
        for (name, text) in super::docs() {
            let shipped = std::fs::read_to_string(dir.join(&name)).unwrap_or_default().replace("\r\n", "\n");
            assert!(shipped == text, "docs/lint/{name} is out of date: run `dvicode-server lint --docs docs/lint`");
        }
    }

    #[test]
    fn the_extension_ships_the_current_schema() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../client/schemas/lint.schema.json");
        let shipped = std::fs::read_to_string(&p).expect("client/schemas/lint.schema.json").replace("\r\n", "\n");
        let now = serde_json::to_string_pretty(&super::schema()).unwrap() + "\n";
        assert!(shipped == now, "client/schemas/lint.schema.json is out of date: run `dvicode-server lint --schema --output client/schemas/lint.schema.json`");
    }
    /// From the CLI review: option values, file arguments, compile errors past the editor's
    /// limit, a broken configured baseline written again, and the temporary file.
    #[test]
    fn cli_review_cases() {
        let root = project("clirev", &[("p.sv", SRC)]);
        // A value is never the next option.
        assert_eq!(super::parse(&["x".into(), "--output".into(), "--stats".into()]).err().as_deref(), Some("--output needs a value"));
        // Paths given must exist and hold project files.
        assert_eq!(lint(&root, &["p.sv", "missing.sv"]).0, 2);
        std::fs::write(root.join("other.txt"), "x").unwrap();
        assert_eq!(lint(&root, &[root.join("other.txt").to_str().unwrap()]).0, 2);
        assert_eq!(lint(&root, &[root.join("p.sv").to_str().unwrap(), "--fail-on", "never"]).0, 0);
        // A configured baseline of another version is written again, not refused.
        std::fs::write(root.join(".dvicode/b.json"), r#"{"version":2,"fingerprint":1,"findings":[]}"#).unwrap();
        std::fs::write(root.join(".dvicode/lint.json"), r#"{ "baseline": ".dvicode/b.json" }"#).unwrap();
        assert_eq!(lint(&root, &["--fail-on", "never"]).0, 2);
        std::fs::write(root.join(".dvicode/b.tmp"), "keep").unwrap();
        let b = root.join(".dvicode/b.json");
        assert_eq!(lint(&root, &["--write-baseline", b.to_str().unwrap(), "--fail-on", "never"]).0, 0);
        assert!(std::fs::read_to_string(&b).unwrap().contains("randomize-result-discarded"));
        // A file of that name beside it is left alone.
        assert_eq!(std::fs::read_to_string(root.join(".dvicode/b.tmp")).unwrap(), "keep");
        assert_eq!(lint(&root, &[]).0, 0);

        // The only compile error after 500 warnings still fails the run, and is printed.
        let mut m = String::from("module m;\n");
        for i in 0..520 {
            m.push_str(&format!("  unknown_module u{i}();\n"));
        }
        m.push_str("  assign x = undeclared_y;\nendmodule\n");
        let root = project("clirev2", &[("m.sv", &m)]);
        let (code, out) = lint(&root, &["--format", "json"]);
        assert_eq!(code, 1, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["compile"]["errors"].as_u64().unwrap() >= 1, "{v}");
        assert!(v["compile"]["first"][0]["message"].as_str().unwrap().contains("undeclared_y"), "{v}");
    }
}
