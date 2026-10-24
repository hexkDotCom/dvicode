//! End-to-end checks on examples/demo-project.

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use dvi_syntax::SourceProvider;
use lsp_types::Position;
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn demo_root() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../examples/demo-project").canonicalize().unwrap();
    // Without the `\\?\` that canonicalize adds on Windows: paths from file URLs have none.
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

fn build(build_file: &str) -> Analysis {
    let root = demo_root();
    let cfg = dvi_project::load(&root, Some(&root.join(build_file)), &FxHashMap::default());
    assert!(cfg.problems.is_empty(), "build file problems: {:?}", cfg.problems);
    Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())))
}

/// (file id, offset) of the first occurrence of `needle` on 1-based `line`.
fn at(a: &Analysis, rel: &str, line: u32, needle: &str) -> (u32, u32) {
    let p = demo_root().join(rel);
    let f = a.vfs.file_id(&p);
    let text = a.vfs.text(f).unwrap();
    let l = text.lines().nth(line as usize - 1).unwrap();
    let col = l.find(needle).unwrap_or_else(|| panic!("'{needle}' not on line {line} of {rel}")) as u32;
    (f, a.offset_at(f, Position::new(line - 1, col)).unwrap())
}

fn loc_str(a: &Analysis, l: &lsp_types::Location) -> String {
    // A file URL, not a path: spaces are escaped and Windows drives look different.
    let p = l.uri.to_file_path().unwrap_or_else(|_| PathBuf::from(l.uri.path()));
    let rel = p
        .strip_prefix(demo_root())
        .map(|r| r.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| p.display().to_string());
    let _ = a;
    format!("{}:{}", rel, l.range.start.line + 1)
}

#[test]
fn clean_project_has_no_problems() {
    let a = build(".dvicode/default.build");
    for f in a.project_files() {
        let d = a.file_diagnostics(f);
        assert!(d.is_empty(), "{}: {:?}", a.vfs.path(f).display(), d);
    }
}

#[test]
fn deliberate_errors_are_reported() {
    let a = build(".dvicode/errors.build");
    let f = a.vfs.file_id(&demo_root().join("rtl/broken.sv"));
    let mut got: Vec<(u32, &str)> = a
        .file_diagnostics(f)
        .iter()
        .map(|d| (a.to_range(d.span).unwrap().start.line + 1, d.code))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (7, "NON_EXISTING_TYPE"),
            (11, "NOT_A_MEMBER"),
            (12, "UNDECLARED_IDENTIFIER"),
            (13, "SYNTAX_ERROR"),
            (18, "NON_EXISTING_PACKAGE"),
            (19, "INCLUDE_NOT_FOUND"),
            (20, "UNDEFINED_MACRO"),
            (21, "NON_EXISTING_PORT"),
            (25, "NON_EXISTING_TYPE"),
        ]
    );
}

#[test]
fn navigation() {
    let a = build(".dvicode/default.build");
    // Type through a wildcard import.
    let (f, o) = at(&a, "rtl/top.sv", 8, "state_e");
    let d: Vec<String> = a.goto_definition(f, o).iter().map(|l| loc_str(&a, l)).collect();
    assert_eq!(d, vec!["rtl/demo_pkg.sv:9"]);
    // Member through a mailbox element type: drv.mbx -> driver::mbx.
    let (f, o) = at(&a, "tb/tb_top.sv", 18, "mbx");
    let d: Vec<String> = a.goto_definition(f, o).iter().map(|l| loc_str(&a, l)).collect();
    assert_eq!(d, vec!["tb/tb_pkg.sv:39"]);
    // Out-of-block method: prototype <-> body.
    let (f, o) = at(&a, "tb/tb_top.sv", 18, "drv");
    assert!(!a.goto_definition(f, o).is_empty());
    let (f, o) = at(&a, "tb/tb_pkg.sv", 25, "print");
    let i: Vec<String> = a.goto_implementation(f, o).iter().map(|l| loc_str(&a, l)).collect();
    assert_eq!(i, vec!["tb/tb_pkg.sv:32"]);
    // Inherited member used inside the out-of-block body.
    let (f, o) = at(&a, "tb/tb_pkg.sv", 34, "addr");
    let d: Vec<String> = a.goto_definition(f, o).iter().map(|l| loc_str(&a, l)).collect();
    assert_eq!(d, vec!["tb/tb_pkg.sv:7"]);
    // Port connection -> port of the instantiated module.
    let (f, o) = at(&a, "rtl/top.sv", 14, "clk");
    let d: Vec<String> = a.goto_definition(f, o).iter().map(|l| loc_str(&a, l)).collect();
    assert_eq!(d, vec!["rtl/fifo.sv:6"]);
    // Macro use -> `define.
    let (f, o) = at(&a, "tb/tb_pkg.sv", 33, "LOG");
    let d: Vec<String> = a.goto_definition(f, o).iter().map(|l| loc_str(&a, l)).collect();
    assert_eq!(d, vec!["tb/macros.svh:5"]);
    // References of an enum item.
    let (f, o) = at(&a, "rtl/demo_pkg.sv", 6, "IDLE");
    let r: Vec<String> = a.references(f, o, true).iter().map(|l| loc_str(&a, l)).collect();
    assert_eq!(r, vec!["rtl/demo_pkg.sv:6", "rtl/top.sv:32", "rtl/top.sv:34"]);
}

#[test]
fn hover_completion_hierarchy() {
    let a = build(".dvicode/default.build");
    let (f, o) = at(&a, "rtl/top.sv", 9, "req_t");
    let (h, _) = a.hover(f, o).unwrap();
    assert!(h.contains("typedef struct packed"), "{h}");
    assert!(h.contains("A bus request."), "{h}");
    // Members of a class variable.
    let (f, o) = at(&a, "tb/tb_top.sv", 17, "randomize");
    let items = a.completions(f, o);
    let labels: Vec<&str> = items.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"convert2string") && labels.contains(&"data") && labels.contains(&"addr"), "{labels:?}");
    // Design hierarchy from the testbench top.
    let h = a.design_hierarchy(None, 100);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].name, "tb_top");
    assert_eq!(h[0].children[0].name, "dut");
    let kids: Vec<&str> = h[0].children[0].children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(kids, vec!["u_fifo", "g_mon[0]", "g_mon[1]"], "one generate block per value of the genvar");
}

fn verif_tree(n: &dvi_analysis::ide::views::VerifNode) -> String {
    let kids: Vec<String> = n.children.iter().map(verif_tree).collect();
    let me = format!("{}:{}", n.name, n.class);
    if kids.is_empty() {
        me
    } else {
        format!("{me}({})", kids.join(","))
    }
}

#[test]
fn verification_hierarchy() {
    let a = build(".dvicode/default.build");
    // +UVM_TESTNAME in the build file selects the test.
    let h = a.verification_hierarchy(None, 1000);
    assert_eq!(h.configured.as_deref(), Some("demo_full_test"));
    assert_eq!(h.tests, vec!["demo_base_test", "demo_err_test", "demo_full_test", "demo_smoke_test"]);
    assert_eq!(h.roots.len(), 1);
    let root = &h.roots[0];
    // The configuration object created with a parent is not a component; the derived test
    // adds to what its base test builds; the extern build_phase body and `new(name, this)`
    // are followed.
    assert_eq!(
        verif_tree(root),
        "uvm_test_top:demo_full_test(env:demo_env(agents[i]:demo_agent(mon:demo_monitor(ap:uvm_analysis_port),drv:demo_driver,sqr:uvm_sequencer),sb:demo_scoreboard,vsqr:uvm_sequencer),extra_sb:demo_scoreboard)"
    );
    let env = &root.children[0];
    assert_eq!((env.kind.as_str(), env.path.as_str()), ("env", "uvm_test_top.env"));
    assert!(env.uri.as_deref().unwrap().ends_with("tb/demo_env_pkg.sv"));
    assert_eq!(env.line + 1, 174);
    let kinds: Vec<&str> = env.children[0].children.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(kinds, vec!["monitor", "driver", "sequencer"]);
    // Another test, picked explicitly: inherits the base test's build_phase.
    let h = a.verification_hierarchy(Some("demo_smoke_test"), 1000);
    assert_eq!(h.test.as_deref(), Some("demo_smoke_test"));
    let names: Vec<&str> = h.roots[0].children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["env"]);
    // Every test.
    let h = a.verification_hierarchy(Some("*"), 1000);
    assert_eq!((h.test.as_deref(), h.roots.len()), (None, 4));
    // The node budget cuts the tree.
    let h = a.verification_hierarchy(Some("demo_full_test"), 2);
    assert!(h.roots[0].children[0].truncated || h.roots[0].truncated);
}

#[test]
fn call_hierarchy() {
    let a = build(".dvicode/default.build");
    // From the extern prototype: the item is the out-of-block body.
    let (f, o) = at(&a, "tb/tb_pkg.sv", 25, "print");
    let items = a.prepare_call_hierarchy(f, o);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "print");
    assert_eq!(items[0].selection_range.start.line + 1, 32);
    let print = a.callable_at(f, o).unwrap();
    let inc: Vec<(String, Vec<u32>)> =
        a.incoming_calls(print).iter().map(|c| (c.from.name.clone(), c.from_ranges.iter().map(|r| r.start.line + 1).collect())).collect();
    assert_eq!(inc, vec![("run".to_string(), vec![50])]);
    // Calls inside macro arguments count.
    let out: Vec<String> = a.outgoing_calls(print).iter().map(|c| format!("{}:{}", c.to.name, c.to.detail.clone().unwrap_or_default())).collect();
    assert_eq!(out, vec!["convert2string:function tb_pkg::my_item"]);
    // A task called from a module's initial block.
    let (f, o) = at(&a, "tb/tb_pkg.sv", 46, "run");
    let run = a.callable_at(f, o).unwrap();
    let inc = a.incoming_calls(run);
    assert_eq!(inc.len(), 1);
    assert_eq!(inc[0].from.name, "tb_top");
    assert_eq!(inc[0].from_ranges[0].start.line + 1, 20);
    let out: Vec<String> = a.outgoing_calls(run).iter().map(|c| c.to.name.clone()).collect();
    assert!(out.contains(&"print".to_string()), "{out:?}");
    // The module as a caller (follow-up request on the incoming item).
    let m = &inc[0].from;
    let (mf, mo) = (a.file_of_url(&m.uri).unwrap(), a.offset_at(a.file_of_url(&m.uri).unwrap(), m.selection_range.start).unwrap());
    let top = a.call_item_decl(mf, mo).unwrap();
    let out: Vec<String> = a.outgoing_calls(top).iter().map(|c| c.to.name.clone()).collect();
    assert!(out.contains(&"run".to_string()), "{out:?}");
    // `super.f()` from an override.
    let (f, o) = at(&a, "tb/tb_pkg.sv", 15, "convert2string");
    let base = a.callable_at(f, o).unwrap();
    let inc: Vec<String> = a.incoming_calls(base).iter().map(|c| c.from.detail.clone().unwrap_or_default()).collect();
    assert_eq!(inc, vec!["function tb_pkg::my_item"]);
    // Not a function or task.
    let (f, o) = at(&a, "tb/tb_pkg.sv", 7, "addr");
    assert!(a.prepare_call_hierarchy(f, o).is_empty());
}

/// Applies the edits of a fix to the text of `file`.
fn apply(a: &Analysis, file: u32, edits: &[(dvi_syntax::Span, String)]) -> String {
    let mut text = a.vfs.text(file).unwrap().to_string();
    let mut e: Vec<_> = edits.iter().filter(|(s, _)| s.file == file).collect();
    e.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    for (s, t) in e {
        text.replace_range(s.start as usize..s.end as usize, t);
    }
    // A Windows checkout may have CRLF files; the fixes keep the file's line endings, which
    // must not mix (checked here), and the expectations below are written with LF.
    if text.contains("\r\n") {
        assert!(!text.replace("\r\n", "").contains('\n'), "mixed line endings:\n{text}");
    }
    text.replace("\r\n", "\n")
}

#[test]
fn quick_fixes_on_deliberate_errors() {
    let a = build(".dvicode/errors.build");
    let f = a.vfs.file_id(&demo_root().join("rtl/broken.sv"));
    let fixes = a.quick_fixes(f, 0, u32::MAX);
    let titles: Vec<(u32, &str)> = fixes.iter().map(|q| (a.to_range(q.diag.span).unwrap().start.line + 1, q.title.as_str())).collect();
    assert_eq!(
        titles,
        vec![
            (7, "Did you mean 'state_e'?"),
            (12, "Declare variable 'undeclared_var'"),
            (25, "Import package 'demo_pkg' (demo_pkg::*)"),
        ]
    );
    assert!(fixes[0].preferred);
    let fixed = apply(&a, f, &fixes[0].edits);
    assert!(fixed.contains("\n  state_e u;"), "{fixed}");
    let fixed = apply(&a, f, &fixes[1].edits);
    assert!(fixed.contains("module broken (input logic clk);\n  logic undeclared_var;\n  import demo_pkg::*;"), "{fixed}");
    let fixed = apply(&a, f, &fixes[2].edits);
    assert!(fixed.contains("module broken3;\n  import demo_pkg::*;\n  req_t r;"), "{fixed}");
    // Only the fixes of problems in the requested range.
    let (_, o) = at(&a, "rtl/broken.sv", 12, "undeclared_var");
    let v = a.quick_fixes(f, o, o);
    assert_eq!(v.len(), 1);
    // A clean file has none.
    let top = a.vfs.file_id(&demo_root().join("rtl/top.sv"));
    assert!(a.quick_fixes(top, 0, u32::MAX).is_empty());
}

#[test]
fn quick_fixes_in_classes() {
    let a = build(".dvicode/default.build");
    let p = demo_root().join("tb/scratch_fixes.sv");
    let text = "class scratch extends tb_pkg::driver;\n  function void f();\n    cuont++;\n    tmp = 3;\n  endfunction\nendclass\n\nmodule scratch_m;\n  my_itme it;\n  base_item b;\nendmodule\n";
    let file = a.vfs.set_overlay(&p, std::sync::Arc::from(text));
    let a = a.add_adhoc(file);
    let fixes = a.quick_fixes(file, 0, u32::MAX);
    let titles: Vec<(u32, &str)> = fixes.iter().map(|q| (a.to_range(q.diag.span).unwrap().start.line + 1, q.title.as_str())).collect();
    assert_eq!(
        titles,
        vec![
            // Inherited member.
            (3, "Did you mean 'count'?"),
            (3, "Declare variable 'cuont'"),
            (4, "Declare variable 'tmp'"),
            // A misspelled type from a package that is not imported is found once imported;
            // a correctly spelled one gets the import.
            (10, "Import package 'tb_pkg' (tb_pkg::*)"),
        ]
    );
    let fixed = apply(&a, file, &fixes[2].edits);
    assert!(fixed.contains("  function void f();\n    int tmp;\n    cuont++;"), "{fixed}");
}

/// A copy whose request is cancelled: searches over the project stop and find (next to)
/// nothing; the snapshot it was copied from is not affected.
#[test]
fn searches_of_a_cancelled_request_stop() {
    let a = build(".dvicode/default.build");
    let mut c = a.clone();
    c.cancel = Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    assert!(c.cancelled() && !a.cancelled());
    let (f, o) = at(&a, "rtl/demo_pkg.sv", 6, "IDLE");
    assert_eq!(a.references(f, o, false).len(), 2);
    assert!(c.references(f, o, false).is_empty());
    let (f, o) = at(&a, "tb/tb_pkg.sv", 46, "run");
    let run = a.callable_at(f, o).unwrap();
    assert!(!a.incoming_calls(run).is_empty() && c.incoming_calls(run).is_empty());
    assert!(!a.workspace_symbols("module:", 50).is_empty() && c.workspace_symbols("module:", 50).is_empty());
    assert!(!a.verification_hierarchy(None, 1000).roots[0].children.is_empty());
    assert!(c.verification_hierarchy(None, 1000).roots.iter().all(|r| r.children.is_empty()));
    assert!(c.design_hierarchy(None, 1000).iter().all(|r| r.children.is_empty()));
    assert!(!a.design_hierarchy(None, 1000).iter().all(|r| r.children.is_empty()));
}
