//! The Structure view: groups, inherited members, processes, includes and defines, code
//! `ifdef` leaves out, the filter.

use dvi_analysis::ide::filter::Filter;
use dvi_analysis::ide::structure::StructNode;
use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-structure-{name}-{}", std::process::id()));
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

/// The tree as indented lines: `name [kind] detail {mark}`.
fn lines(nodes: &[StructNode]) -> Vec<String> {
    fn walk(n: &[StructNode], d: usize, out: &mut Vec<String>) {
        for x in n {
            let mut s = format!("{}{} [{}]", "  ".repeat(d), x.name, x.kind);
            if let Some(det) = &x.detail {
                s.push_str(&format!(" {det}"));
            }
            if let Some(m) = &x.mark {
                s.push_str(&format!(" {{{m}}}"));
            }
            if let Some(f) = &x.inherited_from {
                s.push_str(&format!(" <{f}>"));
            }
            out.push(s);
            walk(&x.children, d + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(nodes, 0, &mut out);
    out
}

fn structure(a: &Analysis, root: &std::path::Path, file: &str, filter: Option<&str>) -> Vec<String> {
    let f = a.vfs.lookup(&root.join(file)).expect("file");
    let filter = filter.and_then(Filter::new);
    lines(&a.structure(f, filter.as_ref(), 10_000).nodes)
}

#[test]
fn modules_are_grouped_with_their_processes() {
    let m = "`define WIDTH 8\n`include \"defs.svh\"\nmodule top #(parameter int N = 2) (input logic clk, output logic [7:0] y);\n  logic a, b;\n  fifo u_fifo (.clk(clk));\n  always_ff @(posedge clk) begin : regs\n    a <= b;\n  end\n  assign y = {a, b};\n  initial $display(\"hi\");\n  function int f(); return 1; endfunction\n  p1: assert property (@(posedge clk) a |-> b);\n  generate if (N > 1) begin : g_more\n    logic c;\n    always_comb c = a;\n  end endgenerate\n`ifdef NEVER\n  logic hidden;\n`endif\nendmodule\nmodule fifo(input logic clk); endmodule\n";
    let (a, root) = project("module", &[("defs.svh", "typedef int my_int;\n"), ("top.sv", m)]);
    let t = structure(&a, &root, "top.sv", None);
    assert_eq!(
        t,
        [
            "`WIDTH [define]",
            "`include \"defs.svh\" [include]",
            "top [module]",
            "  Parameters [group]",
            "    N [parameter] int",
            "  Ports [group]",
            "    clk [port] input logic",
            "    y [port] output logic [7:0]",
            "  Signals [group]",
            "    a [signal] logic",
            "    b [signal] logic",
            "  Instances [group]",
            "    u_fifo [instance] fifo",
            "  Processes [group]",
            "    always_ff @(posedge clk) : regs [process]",
            "    assign y [process]",
            "    initial [process]",
            "  Generate blocks [group]",
            "    g_more [generate]",
            "      Signals [group]",
            "        c [signal] logic",
            "      Processes [group]",
            "        always_comb [process]",
            "  Functions and tasks [group]",
            "    f [function] int",
            "  Assertions [group]",
            "    p1 [assertion]",
            "fifo [module]",
            "  Ports [group]",
            "    clk [port] input logic",
            "Inactive code [group]",
            "  lines 17–19 [inactive] `ifdef NEVER",
        ]
    );
}

#[test]
fn classes_show_what_they_inherit() {
    let p = "package p;\n  class base;\n    int count;\n    local int secret;\n    function new(); endfunction\n    virtual function void build_phase(int phase); endfunction\n    function void plain(); endfunction\n  endclass\n  class mid extends base;\n    virtual function void build_phase(int phase); endfunction\n  endclass\n  class top_c extends mid;\n    rand bit [3:0] mode;\n    constraint c_mode { mode < 4; }\n    function void build_phase(int phase); endfunction\n    function void plain(); endfunction\n    extern function void later();\n  endclass\n  function void top_c::later(); endfunction\nendpackage\n";
    let (a, root) = project("class", &[("p.sv", p)]);
    let t = structure(&a, &root, "p.sv", None);
    let top: Vec<&String> = t.iter().skip_while(|l| !l.starts_with("    top_c")).collect();
    assert_eq!(
        top,
        [
            "    top_c [class] extends mid",
            "      Fields [group]",
            "        mode [field] rand bit [3:0]",
            "      Constraints [group]",
            "        c_mode [constraint]",
            "      Methods [group]",
            "        build_phase [phase] phase void {overrides mid::build_phase}",
            "        plain [method] void {hides base::plain}",
            "        later [method] void",
            "      Inherited from mid [group]",
            "        build_phase [phase] phase virtual void {overridden in top_c} <mid>",
            "      Inherited from base [group]",
            "        count [field] int <base>",
            "        build_phase [phase] phase virtual void {overridden in mid} <base>",
            "        plain [method] void {hidden in top_c} <base>",
        ]
    );
    // The filter keeps what matches, with its groups and ancestors.
    let f = structure(&a, &root, "p.sv", Some("mode"));
    assert_eq!(f, ["p [package]", "  Classes [group]", "    top_c [class] extends mid", "      Fields [group]", "        mode [field] rand bit [3:0]", "      Constraints [group]", "        c_mode [constraint]"]);
}
