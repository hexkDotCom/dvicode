//! DVICode Lint on small projects: the rules, configuration, ownership and waivers.

use dvi_analysis::lint::config::{ConfigSet, Defaults, DiskEnv};
use dvi_analysis::lint::{Level, Ruleset};
use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-lint-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    std::fs::write(root.join("uvm_macros.svh"), UVM_MACROS).unwrap();
    std::fs::write(root.join("uvm_pkg.sv"), UVM_PKG).unwrap();
    let mut build = String::from("uvm_pkg.sv\n");
    for (f, text) in files {
        let p = root.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        if f.ends_with(".sv") {
            build.push_str(f);
            build.push('\n');
        } else if f.ends_with(".json") {
            // Configuration files are not sources.
        }
    }
    std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    (a, root)
}

const UVM_MACROS: &str = r#"`ifndef UVM_MACROS_SVH
`define UVM_MACROS_SVH
`define uvm_object_utils(T) typedef uvm_object_registry#(T,`"T`") type_id;
`define uvm_object_utils_begin(T) `uvm_object_utils(T)
`define uvm_object_utils_end
`define uvm_object_param_utils(T) typedef uvm_object_registry#(T) type_id;
`define uvm_object_abstract_utils(T) typedef uvm_abstract_object_registry#(T,`"T`") type_id;
`define uvm_component_utils(T) typedef uvm_component_registry#(T,`"T`") type_id;
`define uvm_component_param_utils(T) typedef uvm_component_registry#(T) type_id;
`define uvm_component_abstract_utils(T) typedef uvm_abstract_component_registry#(T,`"T`") type_id;
`define uvm_info(ID, MSG, V) begin end
`endif
"#;

const UVM_PKG: &str = r#"`include "uvm_macros.svh"
package uvm_pkg;
  class uvm_object; function new(string name = ""); endfunction endclass
  class uvm_object_registry #(type T = uvm_object, string Tname = "<unknown>"); endclass
  class uvm_abstract_object_registry #(type T = uvm_object, string Tname = "<unknown>"); endclass
  class uvm_component_registry #(type T = uvm_object, string Tname = "<unknown>"); endclass
  class uvm_abstract_component_registry #(type T = uvm_object, string Tname = "<unknown>"); endclass
  class uvm_component extends uvm_object; function new(string name, uvm_component parent); super.new(name); endfunction endclass
  class uvm_sequence_item extends uvm_object; function new(string name = ""); super.new(name); endfunction endclass
  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  virtual class uvm_port_base #(type IF = uvm_object) extends IF;
    function new(string name, uvm_component parent); endfunction
  endclass
  class uvm_analysis_port #(type T = int) extends uvm_port_base #(T); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
endpackage
"#;

/// Lints every project file with `rulesets`; findings as `file:line rule[/subcheck]` (lines from
/// 1), suppressed ones marked `(waived)`.
fn lint(a: &Analysis, root: &PathBuf, rulesets: &[Ruleset]) -> Vec<String> {
    let set = ConfigSet::new(root, Defaults { rulesets: rulesets.to_vec(), ..Defaults::default() }, None);
    let mut out = Vec::new();
    let mut files = a.project_files();
    files.sort_by_key(|f| a.path(*f));
    for f in files {
        let cfg = a.lint_config(&set, &DiskEnv, f);
        let r = a.lint_file(f, &cfg, None);
        let name = a.path(f).file_name().unwrap().to_string_lossy().to_string();
        let li = a.file_lines(f).unwrap();
        for x in r.findings {
            let line = li.position(&a.file_text(f).unwrap(), x.span.start).0 + 1;
            let sub = x.subcheck.map(|s| format!("/{s}")).unwrap_or_default();
            let w = if x.suppressed.is_some() { " (waived)" } else { "" };
            out.push(format!("{name}:{line} {}{sub}{w}", x.rule));
        }
    }
    out
}

#[test]
fn randomize_result_discarded() {
    let tb = r#"`include "uvm_macros.svh"
package tb_pkg;
  import uvm_pkg::*;
  class item extends uvm_sequence_item;
    rand int a;
    `uvm_object_utils(item)
    function new(string name = "item"); super.new(name); endfunction
    function void self_check();
      randomize();
      this.randomize() with { a > 0; };
      if (!randomize()) $display("x");
    endfunction
  endclass
  class env extends uvm_env;
    item it;
    item arr[4];
    `uvm_component_utils(env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function item get_item(); return it; endfunction
    task run_phase(uvm_phase phase);
      int x;
      bit ok;
      it.randomize();
      it.randomize() with { a < 10; };
      arr[1].randomize();
      get_item().randomize();
      void'(it.randomize());
      ok = it.randomize();
      if (it.randomize()) x = 1;
      assert (it.randomize());
      lbl: it.randomize();
      #10 it.randomize();
      std::randomize(x) with { x < 5; };
      `uvm_info("ID", "msg", 0)
    endtask
  endclass
endpackage
module top;
  int v;
  initial begin
    std::randomize(v);
    if (std::randomize(v)) v = 1;
  end
endmodule
"#;
    let (a, root) = project("rnd", &[("tb.sv", tb)]);
    let got = lint(&a, &root, &[Ruleset::Recommended]);
    let rnd: Vec<&String> = got.iter().filter(|s| s.contains("randomize-result-discarded")).collect();
    let lines: Vec<String> = rnd.iter().map(|s| s.split(' ').next().unwrap().to_string()).collect();
    assert_eq!(lines, ["tb.sv:9", "tb.sv:10", "tb.sv:23", "tb.sv:24", "tb.sv:25", "tb.sv:26", "tb.sv:31", "tb.sv:32", "tb.sv:33", "tb.sv:41"], "{got:?}");
}

#[test]
fn registration_mismatch() {
    let tb = r#"`include "uvm_macros.svh"
package tb_pkg;
  import uvm_pkg::*;
  class good_item extends uvm_sequence_item;
    `uvm_object_utils(good_item)
    function new(string name = ""); super.new(name); endfunction
  endclass
  class copy_item extends uvm_sequence_item;
    `uvm_object_utils(good_item)
    function new(string name = ""); super.new(name); endfunction
  endclass
  class my_env extends uvm_env;
    `uvm_object_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class cfg extends uvm_object;
    `uvm_component_utils(cfg)
    function new(string name = ""); super.new(name); endfunction
  endclass
  class p_item #(int W = 8) extends uvm_sequence_item;
    `uvm_object_utils(p_item)
    function new(string name = ""); super.new(name); endfunction
  endclass
  class pp_item #(int W = 8) extends uvm_sequence_item;
    typedef pp_item #(W) this_t;
    `uvm_object_param_utils(this_t)
    function new(string name = ""); super.new(name); endfunction
  endclass
  virtual class base_env extends uvm_env;
    `uvm_component_utils(base_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  virtual class abs_env extends uvm_env;
    `uvm_component_abstract_utils(abs_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class lost extends missing_base;
    `uvm_object_utils(lost)
  endclass
endpackage
"#;
    let (a, root) = project("reg", &[("tb.sv", tb)]);
    let got: Vec<String> = lint(&a, &root, &[Ruleset::Recommended]).into_iter().filter(|s| s.contains("registration")).collect();
    assert_eq!(
        got,
        [
            "tb.sv:9 uvm-registration-mismatch/other-class",
            "tb.sv:13 uvm-registration-mismatch/family",
            "tb.sv:17 uvm-registration-mismatch/family",
            "tb.sv:21 uvm-registration-mismatch/param",
            "tb.sv:30 uvm-registration-mismatch/abstract",
        ]
    );
}

#[test]
fn waivers_and_suppressions() {
    let tb = r#"package tb_pkg;
  class c;
    rand int a;
    function void f();
      // LNG-10 waived: smoke test, values do not matter
      randomize();
      // randomize-result-discarded waived:
      randomize();
      // LNG-99 waived: no such rule
      randomize();
      // dvicode-lint: disable=randomize-result-discarded legacy block
      randomize();
      // dvicode-lint: enable=randomize-result-discarded
      randomize();
    endfunction
  endclass
endpackage
"#;
    let (a, root) = project("waive", &[("tb.sv", tb)]);
    let got = lint(&a, &root, &[Ruleset::Recommended]);
    assert_eq!(
        got,
        [
            "tb.sv:6 randomize-result-discarded (waived)",
            "tb.sv:7 waiver-without-reason",
            "tb.sv:8 randomize-result-discarded (waived)",
            "tb.sv:9 waiver-unknown-id",
            "tb.sv:10 randomize-result-discarded",
            "tb.sv:12 randomize-result-discarded (waived)",
            "tb.sv:14 randomize-result-discarded",
        ]
    );
}

#[test]
fn configuration_and_ownership() {
    let tb = "package tb_pkg;\n  class c; rand int a; function void f(); randomize(); endfunction endclass\nendpackage\n";
    let lint_json = r#"{
  "comment": "test",
  "rulesets": ["hexk"],
  "rules": { "LNG-10": { "level": "error", "comment": "strict" } },
  "exclude": [{ "path": "vendor", "reason": "third party" }]
}"#;
    let (a, root) = project("cfg", &[("tb.sv", tb), ("vendor/v.sv", tb.replace("tb_pkg", "v_pkg").as_str()), (".dvicode/lint.json", lint_json)]);
    let set = ConfigSet::new(&root, Defaults::default(), None);
    let mut seen = Vec::new();
    for f in a.project_files() {
        let cfg = a.lint_config(&set, &DiskEnv, f);
        let name = a.path(f).file_name().unwrap().to_string_lossy().to_string();
        let r = a.lint_file(f, &cfg, None);
        seen.push((name, cfg.owned.is_owned(), r.findings.iter().map(|x| (x.rule, x.level)).collect::<Vec<_>>()));
    }
    let get = |n: &str| seen.iter().find(|s| s.0 == n).unwrap().clone();
    assert_eq!(get("tb.sv").2, [("randomize-result-discarded", Level::Error)]);
    assert!(!get("uvm_pkg.sv").1, "the UVM library is never owned");
    assert!(!get("uvm_macros.svh").1, "nor what it includes");
    assert!(!get("v.sv").1);
    assert!(set.loaded().iter().all(|c| c.problems.is_empty()));
}

#[test]
fn trailing_whitespace_in_hexk_only() {
    let tb = "package tb_pkg; \n  string s = \"a \\\n b\";\nendpackage\t\n";
    let (a, root) = project("ws", &[("tb.sv", tb)]);
    assert!(lint(&a, &root, &[Ruleset::Recommended]).is_empty());
    assert_eq!(lint(&a, &root, &[Ruleset::Hexk]), ["tb.sv:1 trailing-whitespace", "tb.sv:4 trailing-whitespace"]);
}

