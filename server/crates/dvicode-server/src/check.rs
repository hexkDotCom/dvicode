//! `dvicode-server check`: compile a project from the command line, print its problems, and
//! optionally run IDE queries (useful for testing without an editor).

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use dvi_syntax::{Severity, SourceProvider};
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn parse_loc(s: &str) -> Option<(PathBuf, u32, u32)> {
    let mut parts = s.rsplitn(3, ':');
    let col: u32 = parts.next()?.parse().ok()?;
    let line: u32 = parts.next()?.parse().ok()?;
    let path = parts.next()?;
    Some((std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path)), line.saturating_sub(1), col.saturating_sub(1)))
}

pub fn run(args: &[String]) -> i32 {
    let mut dir: Option<PathBuf> = None;
    let mut build: Option<PathBuf> = None;
    let mut quiet = false;
    let mut max = 200usize;
    let mut queries: Vec<(String, String)> = Vec::new();
    let mut env: FxHashMap<String, String> = FxHashMap::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
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
            "--goto" | "--hover" | "--refs" | "--complete" | "--outline" | "--hier" | "--uvm" | "--calls" | "--fixes" | "--sig" | "--impl" | "--tokens" | "--symbols" | "--expand" | "--expandall" => {
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
    let root = std::fs::canonicalize(&dir).unwrap_or(dir);
    let t0 = std::time::Instant::now();
    let cfg = dvi_project::load(&root, build.as_deref(), &env);
    let cfg_ms = t0.elapsed().as_millis();
    let vfs = Arc::new(Vfs::new());
    let setup = Arc::new(ProjectSetup::new(&root, cfg, env));
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
                println!("{}:{}:{}: {:?}: {}", a.vfs.path(f).display(), r.0, r.1, d.severity, d.message);
            }
        }
    }
    let mut c: Vec<_> = counts.into_iter().collect();
    c.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("dvicode check: {errors} errors, {warnings} warnings ({} ms for diagnostics) {:?}", t1.elapsed().as_millis(), c);
    for (q, arg) in &queries {
        run_query(&a, q, arg);
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
        "--outline" => {
            let p = std::fs::canonicalize(arg).unwrap_or_else(|_| PathBuf::from(arg));
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
            let p = std::fs::canonicalize(arg).unwrap_or_else(|_| PathBuf::from(arg));
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
