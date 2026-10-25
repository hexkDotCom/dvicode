//! `dvicode-server check`: compile a project from the command line, print its problems, and
//! optionally run IDE queries (useful for testing without an editor).

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use dvi_syntax::{Severity, SourceProvider};
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Absolute, normalized path; on Windows without the `\\?\` prefix that canonicalize adds.
fn canonical(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => c,
    }
}

fn parse_loc(s: &str) -> Option<(PathBuf, u32, u32)> {
    let mut parts = s.rsplitn(3, ':');
    let col: u32 = parts.next()?.parse().ok()?;
    let line: u32 = parts.next()?.parse().ok()?;
    let path = parts.next()?;
    Some((canonical(Path::new(path)), line.saturating_sub(1), col.saturating_sub(1)))
}

pub fn run(args: &[String]) -> i32 {
    let mut dir: Option<PathBuf> = None;
    let mut build: Option<PathBuf> = None;
    let mut quiet = false;
    let mut max = 200usize;
    let mut queries: Vec<(String, String)> = Vec::new();
    let mut env: FxHashMap<String, String> = FxHashMap::default();
    // Seconds one query may take before the process gives up (0: no limit).
    let mut query_timeout = 0u64;
    // Queries from a file: catch panics and time each one (used by scripts/corpus).
    let mut sweep = false;
    let mut fuzz = crate::fuzz::Options { edits: 0, ..Default::default() };
    let mut write_check = 0usize;
    let mut replay: Option<PathBuf> = None;
    let num = |s: Option<&String>| s.and_then(|m| m.parse::<u64>().ok());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--fuzz-edits" | "--seed" | "--fuzz-samples" | "--fuzz-findings" | "--write-check" => {
                i += 1;
                let Some(n) = num(args.get(i)) else {
                    eprintln!("check: {a} needs a number");
                    return 2;
                };
                match a {
                    "--fuzz-edits" => fuzz.edits = n as usize,
                    "--seed" => fuzz.seed = n,
                    "--fuzz-samples" => fuzz.samples = n as usize,
                    "--write-check" => write_check = n as usize,
                    _ => fuzz.max_findings = (n as usize).max(1),
                }
            }
            "--fuzz-save" | "--fuzz-replay" => {
                i += 1;
                let Some(p) = args.get(i).map(PathBuf::from) else {
                    eprintln!("check: {a} needs a path");
                    return 2;
                };
                if a == "--fuzz-save" {
                    fuzz.save = Some(p);
                } else {
                    replay = Some(p);
                }
            }
            "--fuzz-disk" => fuzz.disk = true,
            "--fuzz-verbose" => fuzz.verbose = true,
            "--build" => {
                i += 1;
                build = args.get(i).map(PathBuf::from);
            }
            "-q" | "--quiet" => quiet = true,
            "--max" => {
                i += 1;
                max = args.get(i).and_then(|m| m.parse().ok()).unwrap_or(max);
            }
            "--env" => {
                i += 1;
                if let Some((k, v)) = args.get(i).and_then(|kv| kv.split_once('=')) {
                    env.insert(k.into(), v.into());
                }
            }
            "--query-timeout" => {
                i += 1;
                query_timeout = args.get(i).and_then(|m| m.parse().ok()).unwrap_or(0);
            }
            "--queries" => {
                i += 1;
                let Some(text) = args.get(i).and_then(|f| std::fs::read_to_string(f).ok()) else {
                    eprintln!("check: cannot read the query file {}", args.get(i).map_or("", |s| s.as_str()));
                    return 2;
                };
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                    let (q, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
                    queries.push((q.to_string(), arg.trim().to_string()));
                }
                sweep = true;
            }
            "--registers" | "--regtree" | "--checks" | "--drivers" | "--loads" | "--format-check" | "--format" | "--format-twice" | "--goto" | "--hover" | "--refs" | "--complete" | "--outline" | "--structure" | "--hier" | "--uvm" | "--uvm-tests" | "--calls" | "--fixes" | "--sig" | "--impl" | "--tokens" | "--symbols" | "--expand" | "--expandall" | "--files" => {
                i += 1;
                queries.push((a.to_string(), args.get(i).cloned().unwrap_or_default()));
            }
            _ if dir.is_none() => dir = Some(PathBuf::from(a)),
            _ => {
                eprintln!("check: unexpected argument {a}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(dir) = dir else {
        eprintln!("usage: dvicode-server check <dir> [--build FILE] [-q] [--max N] [--goto F:L:C] ...");
        return 2;
    };
    let root = canonical(&dir);
    if !root.is_dir() {
        eprintln!("check: {} is not a folder", dir.display());
        return 2;
    }
    let t0 = std::time::Instant::now();
    // `--build auto` auto-configures even when the project has a build file, like the setting.
    let cfg = match build.as_deref() {
        Some(b) if b == Path::new("auto") => dvi_project::load_auto(&root, &env),
        b => dvi_project::load(&root, b, &env),
    };
    let cfg_ms = t0.elapsed().as_millis();
    // Nothing was checked: say so in the exit status, not only in a message.
    if let Some(bf) = cfg.build_file.as_ref() {
        let unread = cfg.problems.iter().find(|p| p.message.starts_with("Cannot read the build file"));
        if !bf.is_file() || unread.is_some() {
            eprintln!("check: {}", unread.map_or_else(|| format!("cannot read the build file {}", bf.display()), |p| p.message.clone()));
            return 2;
        }
    }
    let setup = Arc::new(ProjectSetup::new(&root, cfg, env));
    if let Some(file) = replay {
        return match crate::fuzz::replay_file(setup, &file) {
            Ok(d) if d.is_empty() => {
                eprintln!("dvicode check: the reproducer shows no difference");
                0
            }
            Ok(d) => {
                for x in d {
                    println!("{x}");
                }
                1
            }
            Err(e) => {
                eprintln!("check: {e}");
                2
            }
        };
    }
    if write_check > 0 {
        let t = std::time::Instant::now();
        let opts = crate::writes::Options { count: write_check, seed: fuzz.seed, verbose: fuzz.verbose };
        let r = crate::writes::run(setup, &opts, &mut |s| eprintln!("{s}"));
        eprintln!(
            "dvicode check: write check seed {}: {} renames, {} refused, {} quick fixes, {} finding(s), {} note(s), {} s",
            fuzz.seed,
            r.renames,
            r.refused,
            r.fixes,
            r.findings.len(),
            r.notes.len(),
            t.elapsed().as_secs()
        );
        for f in &r.findings {
            println!("finding: {f}");
        }
        for n in &r.notes {
            println!("note: {n}");
        }
        return if r.findings.is_empty() { 0 } else { 1 };
    }
    if fuzz.edits > 0 {
        let t = std::time::Instant::now();
        let r = crate::fuzz::run(setup, &fuzz, &mut |s| eprintln!("{s}"));
        let kinds: Vec<String> = r.kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
        eprintln!(
            "dvicode check: fuzz seed {}: {} edits in {} steps, {} finding(s), {} s ({})",
            fuzz.seed,
            r.edits,
            r.steps,
            r.findings.len(),
            t.elapsed().as_secs(),
            kinds.join(", ")
        );
        for f in &r.findings {
            eprintln!("dvicode check: fuzz finding at step {}: {}", f.step, f.differences.first().map_or("", |d| d.as_str()));
            println!("{}", f.reproducer);
        }
        return if r.findings.is_empty() { 0 } else { 1 };
    }
    let vfs = Arc::new(Vfs::new());
    let a = Analysis::build(vfs, setup);
    let st = a.status();
    eprintln!(
        "dvicode check: {} top files, {} files, {} declarations, {} macros, {} packages, {} classes, {} design elements; config {} ms, build {} ms",
        st.top_files, st.files, st.declarations, st.macros, st.packages, st.classes, st.modules, cfg_ms, st.build_ms
    );
    for p in &a.setup.config.problems {
        eprintln!("{}:{}: build config: {}", p.file.display(), p.line + 1, p.message);
    }
    let t1 = std::time::Instant::now();
    let mut counts: FxHashMap<&'static str, usize> = FxHashMap::default();
    let (mut errors, mut warnings, mut shown) = (0usize, 0usize, 0usize);
    let mut files = a.project_files();
    files.sort_by_key(|f| a.vfs.path(*f));
    for f in files {
        let diags = a.file_diagnostics(f);
        for d in diags {
            *counts.entry(d.code).or_default() += 1;
            match d.severity {
                Severity::Error => errors += 1,
                Severity::Warning => warnings += 1,
                _ => {}
            }
            if !quiet && shown < max {
                shown += 1;
                let r = a.to_range(d.span).map(|r| (r.start.line + 1, r.start.character + 1)).unwrap_or((0, 0));
                let code = if d.code.is_empty() || d.message.starts_with(d.code) { String::new() } else { format!("{}: ", d.code) };
                println!("{}:{}:{}: {:?}: {code}{}", a.vfs.path(f).display(), r.0, r.1, d.severity, d.message);
            }
        }
    }
    let mut c: Vec<_> = counts.into_iter().collect();
    c.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("dvicode check: {errors} errors, {warnings} warnings ({} ms for diagnostics) {:?}", t1.elapsed().as_millis(), c);
    // The query being run and since when, for the watchdog.
    let current = Arc::new(std::sync::Mutex::new((String::new(), std::time::Instant::now())));
    if query_timeout > 0 && !queries.is_empty() {
        let current = current.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let Ok(c) = current.lock() else { return };
            if !c.0.is_empty() && c.1.elapsed().as_secs() >= query_timeout {
                use std::io::Write;
                let _ = std::io::stdout().flush();
                eprintln!("dvicode check: query timeout after {query_timeout} s: {}", c.0);
                std::process::exit(124);
            }
        });
    }
    for (q, arg) in &queries {
        if let Ok(mut c) = current.lock() {
            *c = (format!("{q} {arg}"), std::time::Instant::now());
        }
        if !sweep {
            run_query(&a, q, arg);
            continue;
        }
        let t = std::time::Instant::now();
        if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_query(&a, q, arg))) {
            let m = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            println!("!! internal error: {m}");
            eprintln!("dvicode check: internal error in {q} {arg}: {m}");
        }
        println!("-- {} ms", t.elapsed().as_millis());
    }
    if let Ok(mut c) = current.lock() {
        c.0.clear();
    }
    if errors > 0 {
        1
    } else {
        0
    }
}

fn file_off(a: &Analysis, arg: &str) -> Option<(u32, u32)> {
    let (p, l, c) = parse_loc(arg)?;
    let f = a.vfs.lookup(&p).or_else(|| Some(a.vfs.file_id(&p)))?;
    let off = a.offset_at(f, lsp_types::Position::new(l, c))?;
    Some((f, off))
}

fn show_loc(l: &lsp_types::Location) -> String {
    format!("{}:{}:{}", l.uri.path(), l.range.start.line + 1, l.range.start.character + 1)
}

fn run_query(a: &Analysis, q: &str, arg: &str) {
    println!("== {q} {arg}");
    match q {
        "--format-twice" => {
            // The lines a second formatting (default options) changes, with the first's.
            let Some(f) = a.vfs.lookup(std::path::Path::new(arg)) else {
                println!("not a file of the project: {arg}");
                return;
            };
            let Some(text) = a.vfs.text(f) else { return };
            let o = dvi_syntax::format::Options::default();
            let Some(r1) = dvi_syntax::format::format(&text, &o) else { return println!("refused") };
            let p1 = dvi_syntax::format::apply(&text, &r1, None);
            let Some(r2) = dvi_syntax::format::format(&p1, &o) else { return println!("second refused") };
            let p2 = dvi_syntax::format::apply(&p1, &r2, None);
            let (l0, l1, l2): (Vec<&str>, Vec<&str>, Vec<&str>) = (text.lines().collect(), p1.lines().collect(), p2.lines().collect());
            for (i, (a1, a2)) in l1.iter().zip(&l2).enumerate() {
                if a1 != a2 {
                    for j in i.saturating_sub(3)..=i {
                        println!("{:>6} orig {:?}", j + 1, l0.get(j).unwrap_or(&""));
                        println!("{:>6} one  {:?}", j + 1, l1[j]);
                    }
                    println!("{:>6} two  {:?}", i + 1, a2);
                    println!("--");
                }
            }
        }
        "--format" => {
            // A file formatted with the default options.
            let Some(f) = a.vfs.lookup(std::path::Path::new(arg)) else {
                println!("not a file of the project: {arg}");
                return;
            };
            let Some(text) = a.vfs.text(f) else { return };
            match dvi_syntax::format::format(&text, &dvi_syntax::format::Options::default()) {
                Some(r) => print!("{}", dvi_syntax::format::apply(&text, &r, None)),
                None => println!("refused"),
            }
        }
        "--format-check" => {
            // Every file formatted with two option sets: refused results (more than whitespace
            // would change), results that change when formatted again, time.
            use dvi_syntax::format::{apply, difference, format, candidate, Directives, Options};
            let sets = [Options::default(), Options { indent_size: 4, use_tabs: true, align_lists: true, directives: Directives::Indent, max_blank_lines: 1, indent_case_items: false, continuation: 2, trim_trailing: true }];
            let mut files: Vec<_> = a.project_files().into_iter().filter(|f| !a.vfs.is_builtin(*f)).collect();
            files.sort_by_key(|f| a.path(*f));
            let (mut refused, mut unstable, mut changed, mut bytes) = (0, 0, 0, 0usize);
            let (mut lines, mut reindented) = (0usize, 0usize);
            // `DVICODE_FORMAT_SAMPLES`: print up to this many re-indented lines (set 0).
            let mut samples: usize = std::env::var("DVICODE_FORMAT_SAMPLES").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            let t0 = std::time::Instant::now();
            for f in &files {
                let Some(text) = a.vfs.text(*f) else { continue };
                bytes += text.len();
                for (k, o) in sets.iter().enumerate() {
                    match format(&text, o) {
                        None => {
                            refused += 1;
                            let out = apply(&text, &candidate(&text, o), None);
                            println!("refused (set {k}) {}: {:?}", a.path(*f).display(), difference(&text, &out, o.max_blank_lines > 0));
                        }
                        Some(r) => {
                            if !r.edits.is_empty() {
                                changed += 1;
                            }
                            if k == 0 {
                                lines += text.lines().count();
                                for (line, e) in &r.edits {
                                    if let dvi_syntax::format::LineEdit::Indent { new, .. } | dvi_syntax::format::LineEdit::Both { new, .. } = e {
                                        reindented += 1;
                                        if samples > 0 {
                                            samples -= 1;
                                            let old = text.lines().nth(*line).unwrap_or("");
                                            println!("reindent {}:{}: {:?} -> {} spaces", a.path(*f).display(), line + 1, old, new.len());
                                        }
                                    }
                                }
                            }
                            let out = apply(&text, &r, None);
                            if let Some(again) = format(&out, o).filter(|x| !x.edits.is_empty()) {
                                unstable += 1;
                                let (line, _) = &again.edits[0];
                                println!("not idempotent (set {k}) {}:{}: {:?}", a.path(*f).display(), line + 1, out.lines().nth(*line).unwrap_or(""));
                            }
                        }
                    }
                }
            }
            println!("format: {} files, {} KB, {changed} results with changes, {refused} refused, {unstable} not idempotent, {reindented} of {lines} lines re-indented, {} ms", files.len(), bytes / 1024, t0.elapsed().as_millis());
        }
        "--files" => {
            let mut files: Vec<_> = a.project_files().into_iter().map(|f| a.vfs.path(f)).collect();
            files.sort();
            for f in files {
                println!("{}", f.display());
            }
        }
        "--outline" => {
            let p = canonical(Path::new(arg));
            if let Some(f) = a.vfs.lookup(Path::new(&p)) {
                fn pr(s: &[lsp_types::DocumentSymbol], d: usize) {
                    for x in s {
                        println!("{}{} [{:?}] {}", "  ".repeat(d), x.name, x.kind, x.detail.clone().unwrap_or_default());
                        if let Some(c) = &x.children {
                            pr(c, d + 1);
                        }
                    }
                }
                pr(&a.document_symbols(f), 0);
            }
        }
        "--structure" => {
            let p = canonical(Path::new(arg));
            if let Some(f) = a.vfs.lookup(Path::new(&p)) {
                fn pr(s: &[dvi_analysis::ide::structure::StructNode], d: usize) {
                    for x in s {
                        let extra = [x.detail.clone(), x.mark.clone(), x.inherited_from.clone().map(|c| format!("from {c}"))].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                        println!("{}{} [{}] {extra}", "  ".repeat(d), x.name, x.kind);
                        pr(&x.children, d + 1);
                    }
                }
                let t = std::time::Instant::now();
                let tree = a.structure(f, None, 20_000);
                let ms = t.elapsed().as_millis();
                println!("status {}, context {:?} (+{}), incomplete {}", tree.status, tree.context, tree.others, tree.incomplete);
                pr(&tree.nodes, 0);
                eprintln!("structure: {ms} ms");
            }
        }
        "--hier" => {
            let v = a.design_hierarchy(if arg.is_empty() || arg == "-" { None } else { Some(arg) }, 2000);
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        "--uvm" => {
            fn pr(n: &dvi_analysis::ide::views::VerifNode, d: usize) {
                println!("{}{} : {} [{}]", "  ".repeat(d), n.name, n.class, n.kind);
                for c in &n.children {
                    pr(c, d + 1);
                }
            }
            let h = a.verification_hierarchy(if arg.is_empty() || arg == "-" { None } else { Some(arg) }, 5000);
            println!("tests: {} (configured: {})", h.tests.join(" "), h.configured.as_deref().unwrap_or("-"));
            if let Some(hint) = &h.hint {
                println!("{hint}");
            }
            for r in &h.roots {
                pr(r, 0);
            }
        }
        "--uvm-tests" => {
            // Every test's tree on its own, without a node limit, and the time each took.
            fn pr(n: &dvi_analysis::ide::views::VerifNode, d: usize) {
                let dup = n.dup.map(|k| format!("#{k}")).unwrap_or_default();
                println!("{}{}{dup} : {} [{}]{}", "  ".repeat(d), n.name, n.class, n.kind, if n.truncated { " ..." } else { "" });
                for c in &n.children {
                    pr(c, d + 1);
                }
            }
            // `--uvm-tests objects`: with the objects.
            let mut q = dvi_analysis::ide::views::TreeQuery::new(1_000_000);
            q.objects = arg == "objects";
            for t in a.uvm_tests() {
                let name = a.decl(t).name.to_string();
                let start = std::time::Instant::now();
                let (h, _) = a.verification_tree(Some(&name), &q);
                let ms = start.elapsed().as_millis();
                for r in &h.roots {
                    pr(r, 0);
                }
                eprintln!("uvm-tests: {name} {ms} ms");
            }
        }
        "--fixes" => {
            let p = canonical(Path::new(arg));
            if let Some(f) = a.vfs.lookup(Path::new(&p)) {
                for q in a.quick_fixes(f, 0, u32::MAX) {
                    let line = a.to_range(q.diag.span).map_or(0, |r| r.start.line + 1);
                    let edits: Vec<String> = q.edits.iter().map(|(s, t)| format!("{}@{} {:?}", a.vfs.path(s.file).display(), s.start, t)).collect();
                    println!("{line}: {} -> {}", q.title, edits.join("; "));
                }
            }
        }
        "--regtree" => {
            // One model's tree, as the view gets it (the first 60 lines).
            let (chosen, ..) = a.register_top(Some(arg));
            if let Some(b) = chosen {
                let model = a.register_model(b);
                let (tree, _) = a.register_tree(&model, 2000);
                let text = serde_json::to_string_pretty(&tree).unwrap_or_default();
                for l in text.lines().filter(|l| !l.contains("\"uri\"") && !l.contains("def_uri")).take(60) {
                    println!("{l}");
                }
            }
        }
        "--registers" => {
            // Every block class (or the one named), built as a top: what it holds, how long it took.
            let (blocks, _) = a.register_blocks();
            for b in blocks.iter().filter(|b| arg.is_empty() || arg == "*" || a.decl(**b).name == arg) {
                let t0 = std::time::Instant::now();
                let model = a.register_model(*b);
                let built = t0.elapsed();
                let fields: usize = (0..model.regs.len()).filter(|r| !model.regs[*r].memory).map(|r| a.register_fields(&model, r).len()).sum();
                let (tree, _) = a.register_tree(&model, usize::MAX);
                fn count(n: &dvi_analysis::ide::registers::RegNode, addressed: &mut usize, total: &mut usize) {
                    if n.kind == "register" || n.kind == "memory" {
                        *total += 1;
                        if n.address.is_some() {
                            *addressed += 1;
                        }
                    }
                    for c in &n.children {
                        count(c, addressed, total);
                    }
                }
                let (mut addressed, mut total) = (0, 0);
                count(&tree, &mut addressed, &mut total);
                println!(
                    "{}: {} blocks, {} maps, {} registers ({} entries in maps, {} with an address), {} fields; {:?}{}",
                    a.decl(*b).name, model.blocks.len(), model.maps.len(), model.regs.len(), total, addressed, fields, built,
                    model.cut.map(|c| format!(", cut at the {c} limit")).unwrap_or_default()
                );
            }
        }
        "--symbols" => {
            for s in a.workspace_symbols(arg, 50) {
                println!("{} [{:?}] {}", s.name, s.kind, show_loc(&s.location));
            }
        }
        _ => {
            let Some((f, off)) = file_off(a, arg) else {
                println!("bad location");
                return;
            };
            match q {
                "--goto" => {
                    for l in a.goto_definition(f, off) {
                        println!("{}", show_loc(&l));
                    }
                }
                "--impl" => {
                    for l in a.goto_implementation(f, off) {
                        println!("{}", show_loc(&l));
                    }
                }
                "--calls" => {
                    let Some(r) = a.callable_at(f, off) else {
                        println!("not a function or task");
                        return;
                    };
                    println!("{}", a.qualified_name(r));
                    for c in a.incoming_calls(r) {
                        println!("  <- {} ({}) {}:{}", c.from.name, c.from.detail.unwrap_or_default(), c.from.uri.path(), c.from_ranges.iter().map(|r| (r.start.line + 1).to_string()).collect::<Vec<_>>().join(","));
                    }
                    for c in a.outgoing_calls(r) {
                        println!("  -> {} ({}) at {}", c.to.name, c.to.detail.unwrap_or_default(), c.from_ranges.iter().map(|r| (r.start.line + 1).to_string()).collect::<Vec<_>>().join(","));
                    }
                }
                "--checks" => {
                    // The assertions and coverage of the declaration there, as a tree.
                    let t0 = std::time::Instant::now();
                    let r = a.checks_at(f, off);
                    let ms = t0.elapsed().as_millis();
                    match r {
                        None => println!("no searchable declaration"),
                        Some(r) => {
                            fn show(h: &dvi_analysis::ide::checks::CheckHit, depth: usize) {
                                let line = h.line.map_or(String::new(), |l| format!(":{}", l + 1));
                                let file = h.uri.as_deref().and_then(|u| u.rsplit('/').next()).unwrap_or("");
                                let notes = if h.notes.is_empty() { String::new() } else { format!(" ({})", h.notes.join("; ")) };
                                println!("{}{} {} [{}] {}{} | {}{}", "  ".repeat(depth), h.group, h.kind, h.roles.join(","), h.label.as_deref().unwrap_or("-"), notes, file, line);
                                println!("{}    {}", "  ".repeat(depth), h.detail);
                                for c in &h.children {
                                    show(c, depth + 1);
                                }
                            }
                            println!("{} ({}): {} hits, {} ms, limits {:?}, notes {:?}", r.target.name, r.target.kind, r.hits.len(), ms, r.limits, r.notes);
                            for h in &r.hits {
                                show(h, 1);
                            }
                        }
                    }
                }
                "--drivers" | "--loads" => {
                    // The writes or reads of the declaration there, as a tree.
                    let t0 = std::time::Instant::now();
                    let r = a.accesses_at(f, off, q == "--drivers");
                    let ms = t0.elapsed().as_millis();
                    match r {
                        None => println!("no signal"),
                        Some(r) => {
                            fn show(s: &dvi_analysis::ide::drivers::AccessSite, depth: usize) {
                                let line = s.line.map_or(String::new(), |l| format!(":{}", l + 1));
                                let file = s.uri.as_deref().and_then(|u| u.rsplit('/').next()).unwrap_or("");
                                let notes = if s.notes.is_empty() { String::new() } else { format!(" ({})", s.notes.join("; ")) };
                                let process = s.process.map(|p| format!(" in {p}")).unwrap_or_default();
                                let ctx = if s.context.is_empty() { String::new() } else { format!(" @{}", s.context) };
                                println!("{}{} {}{process}{ctx}{notes} | {}{}", "  ".repeat(depth), s.group, s.label, file, line);
                                println!("{}    {}", "  ".repeat(depth), s.detail);
                                for c in &s.children {
                                    show(c, depth + 1);
                                }
                            }
                            println!("{} ({}) {}: {} sites, {} ms, limits {:?}, notes {:?}", r.target.name, r.target.kind, r.mode, r.sites.len(), ms, r.limits, r.notes);
                            for s in &r.sites {
                                show(s, 1);
                            }
                        }
                    }
                }
                "--hover" => {
                    if let Some((h, _)) = a.hover(f, off) {
                        println!("{h}");
                    }
                }
                "--refs" => {
                    for l in a.references(f, off, true) {
                        println!("{}", show_loc(&l));
                    }
                }
                "--complete" => {
                    for c in a.completions(f, off).iter().take(40) {
                        println!("{} [{:?}] {}", c.label, c.kind, c.detail.clone().unwrap_or_default());
                        // An inherited method's whole definition (an override).
                        if c.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET) && c.sort_text.as_deref().is_some_and(|s| s.starts_with('0')) {
                            println!("    {}", c.insert_text.clone().unwrap_or_default().replace('\n', "\n    "));
                        }
                    }
                }
                "--sig" => {
                    if let Some(s) = a.signature_help(f, off) {
                        println!("{} (active {:?})", s.signatures[0].label, s.active_parameter);
                    }
                }
                "--expand" | "--expandall" => {
                    let levels = if q == "--expand" { 1 } else { usize::MAX };
                    if let Some(s) = a.expand_macro_at(f, off, levels) {
                        println!("{s}");
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_folder_is_an_error() {
        let dir = std::env::temp_dir().join(format!("dvicode-check-missing-{}", std::process::id()));
        assert_eq!(super::run(&[dir.display().to_string(), "-q".into()]), 2);
    }
}
