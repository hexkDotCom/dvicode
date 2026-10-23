//! Work budgets that take seconds to reach: in a test binary of their own, so that they do not
//! slow down the timed tests of the others (test binaries run one after another).

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-limits-{name}-{}", std::process::id()));
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
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    (a, root)
}

fn problems(a: &Analysis) -> Vec<String> {
    let mut v = Vec::new();
    for f in a.project_files() {
        for d in a.file_diagnostics(f) {
            v.push(format!("{}: {}", a.path(f).file_name().unwrap().to_string_lossy(), d.message));
        }
    }
    v
}

#[test]
fn expansions_that_double_at_every_level_stop() {
    // A20 expands to 2^20 copies of a 100-token body; h20.svh includes h0.svh 2^20 times.
    // Both stay within the depth limits; the work budget stops them.
    let body = "x ".repeat(100);
    let mut m = format!("`define A0 {body}\n");
    for k in 1..=20 {
        m.push_str(&format!("`define A{k} `A{} `A{}\n", k - 1, k - 1));
    }
    m.push_str("module m;\n  initial begin `A20; end\nendmodule\n");
    let mut files: Vec<(String, String)> = vec![("m.sv".into(), m), ("h0.svh".into(), format!("{body}\n"))];
    for k in 1..=20 {
        files.push((format!("h{k}.svh"), format!("`include \"h{}.svh\"\n`include \"h{}.svh\"\n", k - 1, k - 1)));
    }
    files.push(("n.sv".into(), "module n;\n  initial begin\n`include \"h20.svh\"\n  end\nendmodule\n".into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let t = std::time::Instant::now();
    let (a, _) = project("doubling", &refs);
    let p = problems(&a);
    assert!(t.elapsed().as_secs() < 60, "{:?}", t.elapsed());
    assert_eq!(p.iter().filter(|m| m.contains("Preprocessing stops here")).count(), 2, "{p:?}");
}

#[test]
fn the_expansion_budget_counts_what_is_produced() {
    // A guarded header of 50,000 tokens included 101 times costs nothing past the first.
    let table: String = "0, ".repeat(25_000);
    let defs = format!("`ifndef DEFS\n`define DEFS\n`define TABLE {table}\n`endif\n");
    let incs: String = "`include \"defs.svh\"\n".repeat(101);
    let guarded = format!("{incs}module after_includes; endmodule\n");
    // A body repeating its argument 10,000 times, given 10,000 tokens: stopped while built.
    let xs = "x ".repeat(10_000);
    let args = "a ".repeat(10_000);
    let wide = format!("`define W(x) {xs}\nmodule w;\n  initial begin `W({args}); end\nendmodule\n");
    // Crossing the budget inside a stringification is still reported.
    let mut s = format!("`define A0 {}\n", "0 ".repeat(100));
    for k in 1..=20 {
        s.push_str(&format!("`define A{k} `A{} `A{}\n", k - 1, k - 1));
    }
    s.push_str("`define STR `\"`A20`\"\nmodule q;\n  string v = `STR;\nendmodule\n");
    let (a, _) = project("budgetkinds", &[("defs.svh", defs.as_str()), ("g.sv", guarded.as_str()), ("w.sv", wide.as_str()), ("s.sv", s.as_str())]);
    let p = problems(&a);
    let limit = |f: &str| p.iter().filter(|m| m.starts_with(f) && m.contains("Preprocessing stops here")).count();
    assert_eq!((limit("g.sv"), limit("w.sv"), limit("s.sv")), (0, 1, 1), "{p:?}");
    assert!(a.workspace_symbols("after_includes", 5).iter().any(|s| s.name == "after_includes"));
}
