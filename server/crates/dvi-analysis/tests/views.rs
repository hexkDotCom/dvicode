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
        build.push_str(f);
        build.push('\n');
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
