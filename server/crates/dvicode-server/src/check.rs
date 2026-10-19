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
    let num = |s: Option<&String>| s.and_then(|m| m.parse::<u64>().ok());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--fuzz-edits" | "--seed" | "--fuzz-samples" | "--fuzz-findings" => {
                i += 1;
                let Some(n) = num(args.get(i)) else {
                    eprintln!("check: {a} needs a number");
                    return 2;
                };
                match a {
                    "--fuzz-edits" => fuzz.edits = n as usize,
                    "--seed" => fuzz.seed = n,
                    "--fuzz-samples" => fuzz.samples = n as usize,
                    _ => fuzz.max_findings = (n as usize).max(1),
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
            "--goto" | "--hover" | "--refs" | "--complete" | "--outline" | "--hier" | "--uvm" | "--calls" | "--fixes" | "--sig" | "--impl" | "--tokens" | "--symbols" | "--expand" | "--expandall" | "--files" => {
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
    let t0 = std::time::Instant::now();
    // `--build auto` auto-configures even when the project has a build file, like the setting.
    let cfg = match build.as_deref() {
        Some(b) if b == Path::new("auto") => dvi_project::load_auto(&root, &env),
        b => dvi_project::load(&root, b, &env),
    };
    let cfg_ms = t0.elapsed().as_millis();
    let setup = Arc::new(ProjectSetup::new(&root, cfg, env));
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
    for (f, l, m) in &a.setup.config.problems {
        eprintln!("{}:{}: build config: {m}", f.display(), l + 1);
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
            for r in &h.roots {
                pr(r, 0);
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
