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
    assert_eq!(all, ["top", "top.c0", "top.c0.u_rx_fifo", "top.c0.u_tx_fifo", "top.c1", "top.c1.u_rx_fifo", "top.c1.u_tx_fifo", "top.g", "top.g.f"]);
    let (t, meta) = a.design_tree(Some("top"), &query("rx"));
    assert_eq!(paths(&t), ["top", "top.c0", "top.c0.u_rx_fifo", "top.c1", "top.c1.u_rx_fifo"]);
    assert_eq!((meta.matches, meta.incomplete), (2, None));
    // `*` stays within one level; the module name matches too (without its children).
    assert_eq!(paths(&a.design_tree(Some("top"), &query("c1.*fifo")).0), ["top", "top.c1", "top.c1.u_rx_fifo", "top.c1.u_tx_fifo"]);
    assert_eq!(paths(&a.design_tree(Some("top"), &query("CORE")).0), ["top", "top.c0", "top.c1"]);
    // Named generate blocks are levels of the path.
    assert_eq!(paths(&a.design_tree(Some("top"), &query("g.f")).0), ["top", "top.g", "top.g.f"]);
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
    let (files, meta) = a.compiled_files_filtered(Filter::new("tb_*.sv").as_ref());
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
    assert_eq!(n, 5);
    assert!(p.contains(&"top.g.f".to_string()) && p.contains(&"top.c1.u_tx_fifo".to_string()));
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
    time("files, all", &|| format!("{} roots", a.compiled_files_filtered(None).0.len()));
    time("files, filter h5_9", &|| {
        let (f, m) = a.compiled_files_filtered(Filter::new("h5_9").as_ref());
        format!("{} roots, {} matches", f.len(), m.matches)
    });
    let json = serde_json::to_string(&a.design_tree(Some("top"), &TreeQuery::new(20_000)).0).unwrap();
    println!("design JSON at 20,000 nodes: {} KB", json.len() / 1024);
}
