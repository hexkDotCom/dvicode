//! The views' tree requests: filter, limits, revealing a node, and locating the cursor.

use dvi_analysis::ide::filter::Filter;
use dvi_analysis::ide::views::{HierNode, TreeQuery};
use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-views-{name}-{}", std::process::id()));
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

/// Paths of every node, depth first.
fn paths(nodes: &[HierNode]) -> Vec<String> {
    fn walk(n: &HierNode, prefix: &str, out: &mut Vec<String>) {
        let p = if prefix.is_empty() { n.name.clone() } else { format!("{prefix}.{}", n.name) };
        out.push(p.clone());
        for c in &n.children {
            walk(c, &p, out);
        }
    }
    let mut out = Vec::new();
    for n in nodes {
        walk(n, "", &mut out);
    }
    out
}

const DESIGN: &str = "module fifo; endmodule\nmodule core;\n  fifo u_rx_fifo();\n  fifo u_tx_fifo();\nendmodule\nmodule top;\n  core c0();\n  core c1();\n  for (genvar i = 0; i < 2; i++) begin : g\n    fifo f();\n  end\nendmodule\n";

fn query(filter: &str) -> TreeQuery {
    let mut q = TreeQuery::new(1000);
    q.filter = Filter::new(filter);
    q
}

#[test]
fn design_filter_keeps_matches_and_their_parents() {
    let (a, _) = project("dfilter", &[("d.sv", DESIGN)]);
    let all = paths(&a.design_tree(Some("top"), &TreeQuery::new(1000)).0);
    assert_eq!(all, ["top", "top.c0", "top.c0.u_rx_fifo", "top.c0.u_tx_fifo", "top.c1", "top.c1.u_rx_fifo", "top.c1.u_tx_fifo", "top.g[0]", "top.g[0].f", "top.g[1]", "top.g[1].f"]);
    let (t, meta) = a.design_tree(Some("top"), &query("rx"));
    assert_eq!(paths(&t), ["top", "top.c0", "top.c0.u_rx_fifo", "top.c1", "top.c1.u_rx_fifo"]);
    assert_eq!((meta.matches, meta.incomplete), (2, None));
    // `*` stays within one level; the module name matches too (without its children).
    assert_eq!(paths(&a.design_tree(Some("top"), &query("c1.*fifo")).0), ["top", "top.c1", "top.c1.u_rx_fifo", "top.c1.u_tx_fifo"]);
    assert_eq!(paths(&a.design_tree(Some("top"), &query("CORE")).0), ["top", "top.c0", "top.c1"]);
    // Named generate blocks are levels of the path.
    // A filter without an index matches every copy of a generate loop; one with an index, it.
    assert_eq!(paths(&a.design_tree(Some("top"), &query("g.f")).0), ["top", "top.g[0]", "top.g[0].f", "top.g[1]", "top.g[1].f"]);
    assert_eq!(paths(&a.design_tree(Some("top"), &query("g[1].f")).0), ["top", "top.g[1]", "top.g[1].f"]);
    assert!(a.design_tree(Some("top"), &query("nothing")).0.is_empty());
}

#[test]
fn design_limits_and_revealing_a_node() {
    let (a, _) = project("dlimits", &[("d.sv", DESIGN)]);
    let mut q = TreeQuery::new(2);
    let (t, meta) = a.design_tree(Some("top"), &q);
    assert_eq!(paths(&t), ["top", "top.c0", "top.c0.u_rx_fifo"]);
    assert_eq!(meta.incomplete, Some("nodes"));
    assert!(t[0].children[0].truncated);
    // A node beyond the limit is returned with its parents when asked for.
    q.reveal = vec!["top".into(), "c1".into(), "u_tx_fifo".into()];
    let p = paths(&a.design_tree(Some("top"), &q).0);
    assert_eq!(p, ["top", "top.c0", "top.c0.u_rx_fifo", "top.c1", "top.c1.u_tx_fifo"]);
    // The match limit stops the search.
    let mut q = query("fifo");
    q.max_matches = 1;
    let (t, meta) = a.design_tree(Some("top"), &q);
    assert_eq!(paths(&t), ["top", "top.c0", "top.c0.u_rx_fifo"]);
    assert_eq!((meta.matches, meta.incomplete), (1, Some("matches")));
    // So does the visit limit.
    let mut q = query("f");
    q.max_visits = 3;
    assert_eq!(a.design_tree(Some("top"), &q).1.incomplete, Some("visits"));
}

#[test]
fn locate_finds_the_design_element_instance_and_class() {
    let text = "module fifo; endmodule\nmodule core;\n  fifo a(), b();\n  logic x;\nendmodule\nclass my_env;\n  function void f(); endfunction\nendclass\n";
    let (a, root) = project("locate", &[("d.sv", text)]);
    let f = a.project_files().into_iter().find(|f| a.path(*f) == root.join("d.sv")).unwrap();
    let at = |s: &str| a.locate(f, text.find(s).unwrap() as u32);
    let l = at("b()");
    assert_eq!((l.design.as_ref().map(|d| d.name.as_str()), l.instance.as_deref()), (Some("core"), Some("b")));
    let l = at("logic x");
    assert_eq!((l.design.as_ref().map(|d| d.name.as_str()), l.instance.as_deref()), (Some("core"), None));
    let l = at("function void f");
    assert_eq!((l.design, l.class.as_ref().map(|c| c.name.as_str())), (None, Some("my_env")));
}

#[test]
fn verification_and_file_filters() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_agent extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = "package tb_pkg;\n  import uvm_pkg::*;\n  class rx_agent extends uvm_agent; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class my_env extends uvm_env;\n    rx_agent a0, a1;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_phase(uvm_phase phase); a0 = rx_agent::type_id::create(\"a0\", this); a1 = rx_agent::type_id::create(\"a1\", this); endfunction\n  endclass\n  class t1 extends uvm_test;\n    my_env env;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_phase(uvm_phase phase); env = my_env::type_id::create(\"env\", this); endfunction\n  endclass\nendpackage\n";
    let (a, _) = project("vfilter", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let (h, meta) = a.verification_tree(Some("t1"), &query("rx_agent"));
    let env = &h.roots[0].children[0];
    assert_eq!((env.name.as_str(), env.children.len(), meta.matches), ("env", 2, 2));
    let (h, _) = a.verification_tree(Some("t1"), &query("env.a1"));
    assert_eq!(h.roots[0].children[0].children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a1"]);
    let (files, meta) = a.compiled_files_tree(&query("tb_*.sv"));
    assert_eq!((files.len(), meta.matches), (1, 1));
    assert!(files[0].path.ends_with("tb_pkg.sv"));
}

#[test]
fn targets_find_every_occurrence_in_tree_order() {
    use dvi_analysis::ide::views::Target;
    let (a, _) = project("dtarget", &[("d.sv", DESIGN)]);
    let target = |definition: &str, instance: Option<&str>| {
        let mut q = TreeQuery::new(1000);
        q.target = Some(Target { definition: Some(definition.into()), instance: instance.map(Into::into), class: None });
        let (t, meta) = a.design_tree(Some("top"), &q);
        (paths(&t), meta.matches)
    };
    // Every fifo; the u_tx_fifo instance in core; core itself.
    let (p, n) = target("fifo", None);
    assert_eq!(n, 6);
    assert!(p.contains(&"top.g[1].f".to_string()) && p.contains(&"top.c1.u_tx_fifo".to_string()));
    assert_eq!(target("core", Some("u_tx_fifo")), (vec!["top".into(), "top.c0".into(), "top.c0.u_tx_fifo".into(), "top.c1".into(), "top.c1.u_tx_fifo".into()], 2));
    assert_eq!(target("core", None).1, 2);
}

/// Timing on a large design (run with `cargo test --release -p dvi-analysis --test views -- --ignored --nocapture`).
#[test]
#[ignore]
fn large_trees_timing() {
    // 30 x 30 x 30 leaves under three levels: 27,930 instances.
    let mut text = String::from("module leaf; endmodule\n");
    for (name, child) in [("l2", "leaf"), ("l1", "l2"), ("top", "l1")] {
        text.push_str(&format!("module {name};\n"));
        for i in 0..30 {
            text.push_str(&format!("  {child} u_{child}_{i}();\n"));
        }
        text.push_str("endmodule\n");
    }
    // 1,000 headers included in a chain of 10 levels, 100 wide.
    let mut files: Vec<(String, String)> = vec![("d.sv".into(), text)];
    let mut inc = String::new();
    for w in 0..100 {
        inc.push_str(&format!("`include \"h{w}_0.svh\"\n"));
        for l in 0..10 {
            let next = if l < 9 { format!("`include \"h{w}_{}.svh\"\n", l + 1) } else { String::new() };
            files.push((format!("h{w}_{l}.svh"), format!("// header\n{next}")));
        }
    }
    files.push(("inc.sv".into(), format!("{inc}module inc_top; endmodule\n")));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let (a, _) = project("large", &refs);
    let time = |what: &str, f: &dyn Fn() -> String| {
        let t0 = std::time::Instant::now();
        let r = f();
        println!("{what}: {:?} ({r})", t0.elapsed());
    };
    time("design, 20,000 nodes", &|| {
        let (t, m) = a.design_tree(Some("top"), &TreeQuery::new(20_000));
        format!("{} nodes, {:?}", paths(&t).len(), m.incomplete)
    });
    time("design, filter u_leaf_7", &|| {
        let (t, m) = a.design_tree(Some("top"), &query("u_leaf_7"));
        format!("{} nodes, {} matches, {:?}", paths(&t).len(), m.matches, m.incomplete)
    });
    time("design, filter *", &|| {
        let (t, m) = a.design_tree(Some("top"), &query("*"));
        format!("{} nodes, {} matches, {:?}", paths(&t).len(), m.matches, m.incomplete)
    });
    time("design, target leaf (every occurrence)", &|| {
        let mut q = TreeQuery::new(100_000);
        q.target = Some(dvi_analysis::ide::views::Target { definition: Some("leaf".into()), instance: None, class: None });
        let (_, m) = a.design_tree(Some("top"), &q);
        format!("{} matches, {:?}", m.matches, m.incomplete)
    });
    time("design, reveal beyond the limit", &|| {
        let mut q = TreeQuery::new(20_000);
        q.reveal = vec!["top".into(), "u_l1_29".into(), "u_l2_29".into(), "u_leaf_29".into()];
        let (t, _) = a.design_tree(Some("top"), &q);
        format!("{} nodes, found {}", paths(&t).len(), paths(&t).contains(&"top.u_l1_29.u_l2_29.u_leaf_29".to_string()))
    });
    time("files, all", &|| format!("{} roots", a.compiled_files_tree(&TreeQuery::new(20_000)).0.len()));
    time("files, filter h5_9", &|| {
        let (f, m) = a.compiled_files_tree(&query("h5_9"));
        format!("{} roots, {} matches", f.len(), m.matches)
    });
    let json = serde_json::to_string(&a.design_tree(Some("top"), &TreeQuery::new(20_000)).0).unwrap();
    println!("design JSON at 20,000 nodes: {} KB", json.len() / 1024);
}

#[test]
fn siblings_with_one_name_are_numbered_and_limits_hold() {
    // Both branches of an if-generate are named g: only the first is elaborated; shown with
    // the inactive ones, the second is numbered.
    let text = "module leaf; endmodule\nmodule top;\n  if (1) begin : g\n    leaf f();\n  end else begin : g\n    leaf f();\n  end\nendmodule\n";
    let (a, _) = project("dupnames", &[("d.sv", text)]);
    let names = |q: &TreeQuery| -> Vec<(String, Option<u32>, Option<String>)> { a.design_tree(Some("top"), q).0[0].children.iter().map(|c| (c.name.clone(), c.dup, c.presence.clone())).collect() };
    assert_eq!(names(&TreeQuery::new(1000)), [("g".to_string(), None, None)]);
    let mut inactive = TreeQuery::new(1000);
    inactive.inactive = true;
    assert_eq!(names(&inactive), [("g".to_string(), None, None), ("g".to_string(), Some(2), Some("not elaborated: condition false".to_string()))]);
    // The second one can be revealed by its numbered name.
    let mut q = TreeQuery::new(1);
    q.inactive = true;
    q.reveal = vec!["top".into(), "g#2".into(), "f".into()];
    let (t, _) = a.design_tree(Some("top"), &q);
    assert!(t[0].children.iter().any(|c| c.dup == Some(2) && c.children.len() == 1), "{t:?}");
    // Compiled files keep to the node limit.
    let mut files = vec![("top.sv".to_string(), (0..50).map(|i| format!("`include \"h{i}.svh\"\n")).collect::<String>())];
    files.extend((0..50).map(|i| (format!("h{i}.svh"), String::new())));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let (a, _) = project("filelimit", &refs);
    let (f, meta) = a.compiled_files_tree(&TreeQuery::new(10));
    assert_eq!((f[0].children.len(), meta.incomplete, f[0].truncated), (9, Some("nodes"), true));
}

#[test]
fn the_match_limit_holds_for_roots() {
    // Three tops that all match.
    let (a, _) = project("rootlimit", &[("d.sv", "module ta; endmodule\nmodule tb; endmodule\nmodule tc; endmodule\n")]);
    let mut q = query("t");
    q.max_matches = 2;
    let (t, meta) = a.design_tree(None, &q);
    assert_eq!((t.len(), meta.matches, meta.incomplete), (2, 2, Some("matches")));
}

#[test]
fn tests_of_one_name_and_empty_parameter_values() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = "package p;\n  import uvm_pkg::*;\n  class env_a extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class env_b extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class base #(type E = env_a, type F = env_b) extends uvm_test;\n    E env;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_phase(uvm_phase phase); env = new(\"env\", this); endfunction\n  endclass\n  class same_test extends base#(, env_b);\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\nendpackage\npackage q;\n  import uvm_pkg::*;\n  class same_test extends uvm_test; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let (a, _) = project("samenames", &[("uvm_pkg.sv", uvm), ("tb.sv", tb)]);
    let (h, _) = a.verification_tree(Some("*"), &TreeQuery::new(100));
    let roots: Vec<(String, Option<u32>)> = h.roots.iter().filter(|r| r.class == "same_test").map(|r| (r.class.clone(), r.dup)).collect();
    assert_eq!(roots, [("same_test".to_string(), None), ("same_test".to_string(), Some(2))]);
    // The empty value keeps the default.
    assert_eq!(h.roots.iter().find(|r| r.class == "same_test" && r.dup.is_none()).unwrap().children[0].class, "env_a");
}

#[test]
fn an_empty_verification_hierarchy_says_why() {
    // The test extends uvm_test, but nothing imports uvm_pkg where it is declared.
    let uvm = "package uvm_pkg;
  class uvm_component; function new(string name, uvm_component parent); endfunction endclass
  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
endpackage
";
    let (a, _) = project("nouvmimport", &[("uvm_pkg.sv", uvm), ("t.sv", "class my_test extends uvm_test;
endclass
")]);
    let (h, _) = a.verification_tree(None, &TreeQuery::new(100));
    assert!(h.tests.is_empty());
    let hint = h.hint.unwrap_or_default();
    assert!(hint.contains("1 class extends uvm_test") && hint.contains("uvm_pkg is not imported"), "{hint}");
    // With the import there is a test and no hint.
    let (a, _) = project("withuvmimport", &[("uvm_pkg.sv", uvm), ("t.sv", "import uvm_pkg::*;
class my_test extends uvm_test;
endclass
")]);
    let (h, _) = a.verification_tree(None, &TreeQuery::new(100));
    assert_eq!((h.tests.len(), h.hint), (1, None));
}

#[test]
fn the_hint_names_only_what_it_checked() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    // A misspelled base class is another problem: no hint about imports.
    let (a, _) = project("misspelledbase", &[("uvm_pkg.sv", uvm), ("t.sv", "import uvm_pkg::*;\nclass t extends uvm_tset;\nendclass\n")]);
    assert_eq!(a.verification_tree(None, &TreeQuery::new(100)).0.hint, None);
    // No UVM library in a build: the build file needs -uvm.
    let (a, _) = project("nouvm", &[("t.sv", "class t extends uvm_test;\nendclass\n")]);
    let hint = a.verification_tree(None, &TreeQuery::new(100)).0.hint.unwrap_or_default();
    assert!(hint.contains("add -uvm to the build file"), "{hint}");
}

const ELAB_SRC: &str = "package p;
  localparam int BASE = 4;
  function automatic int log2(input int v);
    int r;
    r = 0;
    while ((1 << r) < v) r++;
    return r;
  endfunction
endpackage
module leaf #(parameter int W = 8, parameter MODE = \"fast\") ();
endmodule
module mid #(parameter int N = 2, parameter int W = p::BASE * 2) ();
  localparam int AW = p::log2(N);
  localparam logic [3:0] ONES = '1;
  if (N > 2) begin : big
    leaf #(.W(W)) u_big ();
  end else begin : lite
    leaf #(W, \"slow\") u_lite ();
  end
  case (AW)
    0: leaf u_zero ();
    1, 2: leaf u_few ();
    default: leaf u_many ();
  endcase
  for (genvar i = N - 1; i >= 0; i -= 1) begin : g
    leaf #(.W(i)) u ();
  end
  leaf u_arr [N-1:0] ();
  if (UNKNOWN_P) begin : maybe
    leaf u_maybe ();
  end
endmodule
module top;
  mid #(.N(4)) u_a ();
  mid u_b ();
  defparam u_b.W = 5;
endmodule
";

/// Each node as `path [presence] {array} P=value...` (values, or `?` and why), depth first.
fn elaborated(nodes: &[HierNode]) -> Vec<String> {
    fn walk(n: &HierNode, prefix: &str, out: &mut Vec<String>) {
        let p = if prefix.is_empty() { n.name.clone() } else { format!("{prefix}.{}", n.name) };
        let mut line = p.clone();
        if let Some(x) = &n.presence {
            line.push_str(&format!(" [{x}]"));
        }
        if let Some(x) = &n.array {
            line.push_str(&format!(" {{{x}}}"));
        }
        for x in &n.params {
            line.push_str(&format!(" {}={}", x.name, x.value.clone().unwrap_or_else(|| format!("?({})", x.why.clone().unwrap_or_default()))));
        }
        out.push(line);
        for c in &n.children {
            walk(c, &p, out);
        }
    }
    let mut out = Vec::new();
    for n in nodes {
        walk(n, "", &mut out);
    }
    out
}

#[test]
fn parameters_and_generate_blocks_are_elaborated_per_instance() {
    let (a, root) = project("elab", &[("e.sv", ELAB_SRC)]);
    let (t, _) = a.design_tree(Some("top"), &TreeQuery::new(1000));
    let undetermined = "undetermined: condition not evaluated: `UNKNOWN_P` is not resolved";
    let want = [
        "top".to_string(),
        "top.u_a N=4 W=8 AW=2 ONES=15".into(),
        "top.u_a.big".into(),
        "top.u_a.big.u_big W=8 MODE=\"fast\"".into(),
        "top.u_a.genblk2".into(),
        "top.u_a.genblk2.u_few W=8 MODE=\"fast\"".into(),
        "top.u_a.g[3]".into(),
        "top.u_a.g[3].u W=3 MODE=\"fast\"".into(),
        "top.u_a.g[2]".into(),
        "top.u_a.g[2].u W=2 MODE=\"fast\"".into(),
        "top.u_a.g[1]".into(),
        "top.u_a.g[1].u W=1 MODE=\"fast\"".into(),
        "top.u_a.g[0]".into(),
        "top.u_a.g[0].u W=0 MODE=\"fast\"".into(),
        "top.u_a.u_arr {[3:0] · 4 instances} W=8 MODE=\"fast\"".into(),
        format!("top.u_a.maybe [{undetermined}]"),
        "top.u_a.maybe.u_maybe W=8 MODE=\"fast\"".into(),
        "top.u_b N=2 W=5 AW=1 ONES=15".into(),
        "top.u_b.lite".into(),
        "top.u_b.lite.u_lite W=5 MODE=\"slow\"".into(),
        "top.u_b.genblk2".into(),
        "top.u_b.genblk2.u_few W=8 MODE=\"fast\"".into(),
        "top.u_b.g[1]".into(),
        "top.u_b.g[1].u W=1 MODE=\"fast\"".into(),
        "top.u_b.g[0]".into(),
        "top.u_b.g[0].u W=0 MODE=\"fast\"".into(),
        "top.u_b.u_arr {[1:0] · 2 instances} W=8 MODE=\"fast\"".into(),
        format!("top.u_b.maybe [{undetermined}]"),
        "top.u_b.maybe.u_maybe W=8 MODE=\"fast\"".into(),
    ];
    assert_eq!(
        elaborated(&t),
        want,
        "named and ordered overrides, a defparam, a package constant, a function with a loop, '1, if/else, case with two labels, \
         a descending loop, an instance array, an unknown condition shown as undetermined"
    );
    let origins: Vec<(String, dvi_analysis::ide::elab::Origin)> = t[0].children[1].params.iter().map(|p| (p.name.clone(), p.origin)).collect();
    use dvi_analysis::ide::elab::Origin;
    assert_eq!(origins, [("N".to_string(), Origin::Default), ("W".into(), Origin::Defparam), ("AW".into(), Origin::Local), ("ONES".into(), Origin::Local)]);
    // Hover: a parameter's values and a signal's width, grouped, from the elaborated design.
    let f = a.vfs.lookup(&root.join("e.sv")).unwrap();
    let off = |after: &str, needle: &str| (ELAB_SRC.find(after).unwrap() + ELAB_SRC[ELAB_SRC.find(after).unwrap()..].find(needle).unwrap()) as u32;
    let (h, _) = a.hover(f, off("module mid", "W = p::BASE")).unwrap();
    assert!(h.contains("**Values in the design hierarchy** (2 instances from `top`)\n- `8` in `top.u_a`\n- `5` in `top.u_b`"), "{h}");
    let (h, _) = a.hover(f, off("module mid", "AW =")).unwrap();
    assert!(h.contains("- `2` in `top.u_a`\n- `1` in `top.u_b`"), "{h}");
    let (h, _) = a.hover(f, off("module leaf", "W = 8")).unwrap();
    assert!(h.contains("`top.u_a.maybe.u_maybe` (if it exists)"), "an instance under an undetermined branch: {h}");
}

#[test]
fn casts_to_user_types_and_package_types_are_evaluated() {
    let src = "package q;\n  typedef enum logic [1:0] {A, B, C} kind_t;\n  typedef logic [7:0] byte_t;\nendpackage\nmodule leaf #(parameter q::kind_t K = q::A, parameter q::byte_t V = 3) ();\nendmodule\nmodule top;\n  leaf #(.K(q::kind_t'(2)), .V(q::byte_t'(300))) u ();\n  if (q::kind_t'(1) == q::B) begin : g\n    leaf u2 ();\n  end\nendmodule\n";
    let (a, _) = project("elabgaps", &[("g.sv", src)]);
    let (t, _) = a.design_tree(Some("top"), &TreeQuery::new(1000));
    assert_eq!(elaborated(&t), ["top", "top.u K=2 V=44", "top.g", "top.g.u2 K=0 V=3"], "300 cast to an 8-bit type is 44");
}

#[test]
fn elaboration_review_cases() {
    // (case, source, the first lines of its elaborated tree)
    let cases: &[(&str, &str, &[&str])] = &[
        ("an unsigned context reaches a nested shift", "module top;\n  localparam P = (4'shf >>> 1) + 4'h0;\nendmodule\n", &["top P=7"]),
        ("overrides and defparams are assignments to the parameter's type", "module m #(parameter logic [7:0] P = 0); endmodule\nmodule top;\n  m #(.P('1)) u();\n  m v();\n  defparam v.P = '1;\nendmodule\n", &["top", "top.u P=255", "top.v P=255"]),
        ("a case's items in their common type", "module leaf; endmodule\nmodule top;\n  case ('1)\n    4'hf:   leaf good();\n    default: leaf bad();\n  endcase\nendmodule\n", &["top", "top.genblk1", "top.genblk1.good"]),
        ("a macro is spliced, not parenthesized", "`define SUM 1 + 2\nmodule top;\n  localparam P = `SUM * 3;\nendmodule\n", &["top P=7"]),
        ("types inherited across commas", "module top #(\n  parameter logic [3:0] A = 0, B = 31\n)(input logic [7:0] a, b);\n  function automatic int f(input int x, y);\n    return y;\n  endfunction\n  localparam F = f(0, 6);\n  localparam BW = $bits(b);\nendmodule\n", &["top A=0 B=15 F=6 BW=8"]),
        ("an enum's base type", "module top;\n  typedef enum logic [3:0] { A = 4'hf } t;\n  localparam P = ~A;\n  localparam Q = $bits(A);\nendmodule\n", &["top P=0 Q=4"]),
        ("no select of several packed dimensions", "module top;\n  parameter logic [1:0][3:0] P = 8'hab;\n  localparam Q = P[0];\nendmodule\n", &["top P=171 Q=?(a select of a name declared with another range or several dimensions)"]),
        ("a large power by squaring", "module top;\n  localparam logic [7:0] P = 3 ** 129;\nendmodule\n", &["top P=3"]),
        ("a power is left-associative", "module top;\n  localparam P = 2 ** 3 ** 2;\nendmodule\n", &["top P=64"]),
        (
            "an unassigned four-state local is no 0",
            "module leaf; endmodule\nmodule top;\n  function automatic int f();\n    integer x;\n    return x;\n  endfunction\n  localparam P = f();\n  if (P == 0) begin : selected\n    leaf u();\n  end\nendmodule\n",
            &["top P=?(an x value (a four-state variable not yet assigned))", "top.selected [undetermined: condition not evaluated: an x value (a four-state variable not yet assigned)]", "top.selected.u"],
        ),
        ("an assignment to the function's name extends with the value's sign", "module top;\n  function automatic logic [15:0] f();\n    f = 8'shff;\n  endfunction\n  localparam P = f();\nendmodule\n", &["top P=65535"]),
        ("a signed parameter without a range keeps the value's width", "module top;\n  parameter signed P = 8'h80;\nendmodule\n", &["top P=-128"]),
        ("a defparam in a module not instantiated under the top", "module leaf #(parameter P = 0); endmodule\nmodule unused;\n  defparam top.u.P = 7;\nendmodule\nmodule top;\n  leaf u();\nendmodule\n", &["top", "top.u P=0"]),
        ("the implicit block of a body without begin", "module leaf #(parameter P = 0); endmodule\nmodule top;\n  if (1) leaf u();\n  defparam genblk1.u.P = 5;\nendmodule\n", &["top", "top.genblk1", "top.genblk1.u P=5"]),
        ("a long comparison chain", "module top;\n  localparam P =\n    1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 ==\n    1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 ==\n    1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 ==\n    1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1 == 1;\nendmodule\n", &["top P=1"]),
        ("a body parameter is local with a parameter port list", "module m #(parameter A=1);\n  parameter B=2;\nendmodule\nmodule top;\n  m #(.B(9)) u();\nendmodule\n", &["top", "top.u A=1 B=?(`B` is local: it cannot be overridden)"]),
        ("a loop's shift by 32 gives 0", "module leaf; endmodule\nmodule top;\n  for (genvar i=1; i!=0; i<<=32) begin : g\n    leaf u();\n  end\nendmodule\n", &["top", "top.g[1]", "top.g[1].u"]),
        (
            "a macro's names are the invocation's",
            "`define WP1 (W + 1)\nmodule a #(parameter W = 2);\n  localparam P = `WP1;\nendmodule\nmodule b #(parameter W = 5);\n  localparam P = `WP1;\nendmodule\nmodule top;\n  a u1();\n  b u2();\n  a #(.W(7)) u3();\nendmodule\n",
            &["top", "top.u1 W=2 P=3", "top.u2 W=5 P=6", "top.u3 W=7 P=8"],
        ),
        (
            "a macro's name is the module's, not the compilation unit's",
            "`define WP1 (W + 1)\nparameter W = 100;\nmodule a #(parameter W = 2);\n  localparam P = `WP1;\nendmodule\nmodule top;\n  a u1();\nendmodule\n",
            &["top", "top.u1 W=2 P=3"],
        ),
        ("a macro in a constant function names its argument", "`define INC (x + 1)\nmodule top;\n  function automatic int f(input int x);\n    return `INC;\n  endfunction\n  localparam P = f(4);\nendmodule\n", &["top P=5"]),
        ("a conditional directly in another shares its block", "module leaf; endmodule\nmodule top;\n  if (1) if (0) leaf a(); else leaf b();\n  if (1) begin leaf c(); end\nendmodule\n", &["top", "top.genblk1", "top.genblk1.b", "top.genblk2", "top.genblk2.c"]),
    ];
    for (what, src, want) in cases {
        let (a, _) = project(&format!("elabrev{}", what.len()), &[("t.sv", src)]);
        let (t, _) = a.design_tree(Some("top"), &TreeQuery::new(1000));
        let got = elaborated(&t);
        assert_eq!(&got[..want.len().min(got.len())], *want, "{what}");
    }
}

#[test]
fn elaboration_limits_recursion_and_hover_lists() {
    // A recursion that counts down is followed to 64 instances, the cut said.
    let src = "module r #(parameter N=100);\n  if (N>0) r #(.N(N-1)) u();\nendmodule\nmodule top;\n  r u();\nendmodule\n";
    let (a, root) = project("elabrec", &[("t.sv", src)]);
    let (t, _) = a.design_tree(Some("top"), &TreeQuery::new(10_000));
    let mut n = &t[0].children[0];
    let mut depth = 1;
    while let Some(g) = n.children.first() {
        n = &g.children[0];
        depth += 1;
    }
    assert_eq!((depth, n.truncated, n.presence.as_deref()), (64, true, Some("not followed: 64 levels of instances")));
    let f = a.vfs.lookup(&root.join("t.sv")).unwrap();
    let (h, _) = a.hover(f, src.find("N=100").unwrap() as u32).unwrap();
    assert!(h.contains("not elaborated to its end"), "{h}");
    // Generated scopes count against the visit budget, with or without instances in them.
    let src = "module top;\n  for (genvar i=0; i<1024; i++) begin : a\n    for (genvar j=0; j<1024; j++) begin : b\n    end\n  end\nendmodule\n";
    let (a, _) = project("elabbudget", &[("t.sv", src)]);
    let mut q = TreeQuery::new(1000);
    q.max_visits = 5_000;
    let (_, meta) = a.design_tree(Some("top"), &q);
    assert_eq!(meta.incomplete, Some("visits"));
    // Hover lists 10 values at most, 20 paths, and counts the rest; loop copies are levels of
    // the path for defparams too.
    let src = "module leaf #(parameter P=0); endmodule\nmodule top;\n  leaf u();\n  for (genvar i=0; i<30; i++) begin : a\n    leaf #(.P(i)) u();\n  end\n  defparam u.P = 77;\nendmodule\n";
    let (a, root) = project("elabhover", &[("t.sv", src)]);
    let f = a.vfs.lookup(&root.join("t.sv")).unwrap();
    let (h, _) = a.hover(f, src.find("P=0").unwrap() as u32).unwrap();
    let rows: Vec<&str> = h.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(rows.len(), 11, "{h}");
    assert_eq!(rows[0], "- `77` in `top.u`", "the defparam sets top.u only: {h}");
    assert_eq!(rows[1], "- `0` in `top.a[0].u`");
    assert_eq!(rows[10], "- and 21 other values in 21 instances");
}
