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
`define uvm_sequence_utils(T, S) `uvm_object_utils(T)
`define uvm_component_utils_begin(T) `uvm_component_utils(T)
`define uvm_component_utils_end
`define uvm_field_int(F, FL)
`define uvm_do(S) begin end
`define uvm_declare_p_sequencer(T) T p_sequencer;
`define uvm_fatal(ID, MSG) begin end
`endif
"#;

const UVM_PKG: &str = r#"`include "uvm_macros.svh"
package uvm_pkg;
  class uvm_object;
    protected string m_name;
    function new(string name = ""); m_name = name; endfunction
    function string get_name(); return m_name; endfunction
  endclass
  class uvm_object_registry #(type T = uvm_object, string Tname = "<unknown>");
    static function T create(string name = "", uvm_component parent = null, string contxt = ""); endfunction
  endclass
  class uvm_abstract_object_registry #(type T = uvm_object, string Tname = "<unknown>"); endclass
  class uvm_component_registry #(type T = uvm_object, string Tname = "<unknown>");
    static function T create(string name, uvm_component parent, string contxt = ""); endfunction
  endclass
  class uvm_abstract_component_registry #(type T = uvm_object, string Tname = "<unknown>"); endclass
  class uvm_phase;
    function void raise_objection(uvm_object obj, string description = "", int count = 1); endfunction
    function void drop_objection(uvm_object obj, string description = "", int count = 1); endfunction
  endclass
  class uvm_report_object extends uvm_object;
    function new(string name = ""); super.new(name); endfunction
    virtual function void uvm_report_info(string id, string message, int verbosity = 0, string filename = "", int line = 0); endfunction
  endclass
  class uvm_component extends uvm_report_object;
    function new(string name, uvm_component parent); super.new(name); endfunction
    virtual function void build_phase(uvm_phase phase); endfunction
    virtual function void connect_phase(uvm_phase phase); endfunction
    virtual task run_phase(uvm_phase phase); endtask
    virtual function void set_config_int(string inst, string field, int value); endfunction
  endclass
  function void global_stop_request(); endfunction
  class uvm_transaction extends uvm_object; function new(string name = ""); super.new(name); endfunction endclass
  class uvm_sequence_item extends uvm_transaction; function new(string name = ""); super.new(name); endfunction endclass
  typedef enum { UVM_NONE, UVM_LOW, UVM_MEDIUM, UVM_DEFAULT } uvm_verbosity;
  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_sequence_base extends uvm_sequence_item;
    function new(string name = ""); super.new(name); endfunction
    virtual task pre_do(bit is_item); endtask
  endclass
  class uvm_tlm_analysis_fifo #(type T = int) extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_config_db #(type T = int);
    static function void set(uvm_component cntxt, string inst_name, string field_name, T value); endfunction
    static function bit get(uvm_component cntxt, string inst_name, string field_name, inout T value); endfunction
  endclass
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

/// Every rule has a page; its bad examples give its finding and its good ones none.
#[test]
fn rule_pages_and_their_examples() {
    use dvi_analysis::lint::{docs, rules};
    for r in rules::all() {
        let id = r.meta().id;
        let page = docs::page(id).unwrap_or_else(|| panic!("{id} has no page"));
        let ex = docs::examples(page);
        assert!(ex.iter().any(|(b, _)| *b) && ex.iter().any(|(b, _)| !*b), "{id}: a bad and a good example");
        for (i, (bad, code)) in ex.iter().enumerate() {
            let files = docs::example_files(code);
            let refs: Vec<(&str, &str)> = files.iter().map(|(n, c)| (n.as_str(), c.as_str())).collect();
            let (a, root) = project(&format!("doc-{id}-{i}"), &refs);
            let got: Vec<String> = lint(&a, &root, &[Ruleset::Hexk]).into_iter().filter(|s| files.iter().any(|(n, _)| s.starts_with(&format!("{}:", n.rsplit('/').next().unwrap())))).collect();
            let hit = got.iter().any(|s| s.contains(&format!(" {id}")) && !s.ends_with("(waived)"));
            assert_eq!(hit, *bad, "{id} example {i} ({}): {got:?}\n{code}", if *bad { "bad" } else { "good" });
        }
    }
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
fn suppressions_per_rule_and_subroutine_waivers() {
    let tb = r#"package tb_pkg;
  class c;
    rand int a;
    // dvicode-lint: disable=randomize-result-discarded,trailing-whitespace legacy
    function void f();
      randomize();
      // dvicode-lint: enable=randomize-result-discarded
      randomize();@@
    endfunction
    // LNG-10 waived: generated test vectors, failures do not matter
    function void g();
      randomize();
      randomize();
    endfunction
    function void h();
      randomize();
    endfunction
  endclass
endpackage
"#;
    // A space at the end of line 8 (an editor would strip it from this file).
    let tb = tb.replace("@@", " ");
    let (a, root) = project("supp", &[("tb.sv", &tb)]);
    let got: Vec<String> = lint(&a, &root, &[Ruleset::Hexk]).into_iter().filter(|s| s.contains("randomize") || s.contains("whitespace")).collect();
    assert_eq!(
        got,
        [
            "tb.sv:6 randomize-result-discarded (waived)",
            "tb.sv:8 randomize-result-discarded",
            "tb.sv:8 trailing-whitespace (waived)",
            "tb.sv:12 randomize-result-discarded (waived)",
            "tb.sv:13 randomize-result-discarded (waived)",
            "tb.sv:16 randomize-result-discarded",
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
        seen.push((name, cfg.owned.is_owned(), r.findings.iter().filter(|x| x.rule.starts_with("randomize")).map(|x| (x.rule, x.level)).collect::<Vec<_>>()));
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
    let got: Vec<String> = lint(&a, &root, &[Ruleset::Hexk]).into_iter().filter(|s| s.contains("whitespace")).collect();
    assert_eq!(got, ["tb.sv:1 trailing-whitespace", "tb.sv:4 trailing-whitespace"]);
}


#[test]
fn randomize_in_assert() {
    let tb = r#"package tb_pkg;
  class c;
    rand int a;
    function void f();
      assert (randomize());
      assert (this.randomize() with { a > 0; }) else $error("x");
      assume (std::randomize(a));
      cover (randomize());
      if (!randomize()) $error("ok");
      assert #0 (randomize());
    endfunction
  endclass
endpackage
"#;
    let (a, root) = project("rasrt", &[("tb.sv", tb)]);
    let got: Vec<String> = lint(&a, &root, &[Ruleset::Recommended]).into_iter().filter(|s| s.contains("in-assert")).collect();
    assert_eq!(got, ["tb.sv:5 randomize-in-assert", "tb.sv:6 randomize-in-assert", "tb.sv:7 randomize-in-assert", "tb.sv:10 randomize-in-assert"]);
}

#[test]
fn factory_incompatible_constructor() {
    let tb = r#"`include "uvm_macros.svh"
package tb_pkg;
  import uvm_pkg::*;
  class good_env extends uvm_env;
    `uvm_component_utils(good_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class one_arg_env extends uvm_env;
    `uvm_component_utils(one_arg_env)
    function new(string name); super.new(name, null); endfunction
  endclass
  class implicit_env extends uvm_env;
    `uvm_component_utils(implicit_env)
  endclass
  class extra_env extends uvm_env;
    `uvm_component_utils(extra_env)
    function new(string name, uvm_component parent, int n); super.new(name, parent); endfunction
  endclass
  class good_item extends uvm_sequence_item;
    `uvm_object_utils(good_item)
    function new(string name = "good_item"); super.new(name); endfunction
  endclass
  class no_default_item extends uvm_sequence_item;
    `uvm_object_utils(no_default_item)
    function new(string name); super.new(name); endfunction
  endclass
  class implicit_item extends uvm_sequence_item;
    `uvm_object_utils(implicit_item)
  endclass
  class proto_item extends uvm_sequence_item;
    `uvm_object_utils(proto_item)
    extern function new(string name = "proto_item");
  endclass
  function proto_item::new(string name = "proto_item"); super.new(name); endfunction
  virtual class abs_env extends uvm_env;
    `uvm_component_abstract_utils(abs_env)
    function new(string name); super.new(name, null); endfunction
  endclass
endpackage
"#;
    let (a, root) = project("ctor", &[("tb.sv", tb)]);
    let got: Vec<String> = lint(&a, &root, &[Ruleset::Recommended]).into_iter().filter(|s| s.contains("constructor")).collect();
    assert_eq!(
        got,
        [
            "tb.sv:10 factory-incompatible-constructor/component",
            "tb.sv:12 factory-incompatible-constructor/component",
            "tb.sv:17 factory-incompatible-constructor/component",
            "tb.sv:25 factory-incompatible-constructor/object",
            "tb.sv:27 factory-incompatible-constructor/object",
        ]
    );
}

#[test]
fn uvm_override_signature() {
    let tb = r#"`include "uvm_macros.svh"
package tb_pkg;
  import uvm_pkg::*;
  class good_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); endfunction
    task run_phase(uvm_pkg::uvm_phase phase); endtask
  endclass
  class bad_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    task build_phase(uvm_phase phase); endtask
    function void connect_phase(); endfunction
    function int run_phase(uvm_phase phase); endfunction
  endclass
  class not_uvm;
    task build_phase(); endtask
  endclass
endpackage
"#;
    let (a, root) = project("ovsig", &[("tb.sv", tb)]);
    let got: Vec<String> = lint(&a, &root, &[Ruleset::Recommended]).into_iter().filter(|s| s.contains("override")).collect();
    assert_eq!(got, ["tb.sv:11 uvm-override-signature/kind", "tb.sv:12 uvm-override-signature/arguments", "tb.sv:13 uvm-override-signature/kind"]);
}

#[test]
fn uvm_deprecated_api() {
    let tb = r#"`include "uvm_macros.svh"
package tb_pkg;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      set_config_int("*", "n", 1);
      global_stop_request();
    endfunction
  endclass
  class my_seq extends uvm_sequence_item;
    `uvm_sequence_utils(my_seq, my_sqr)
    function void set_config_int(string a, string b, int c); endfunction
    function void f(); set_config_int("a", "b", 1); endfunction
  endclass
endpackage
"#;
    let (a, root) = project("depr", &[("tb.sv", tb)]);
    let got: Vec<String> = lint(&a, &root, &[Ruleset::Recommended]).into_iter().filter(|s| s.contains("deprecated")).collect();
    assert_eq!(got, ["tb.sv:7 uvm-deprecated-api", "tb.sv:8 uvm-deprecated-api", "tb.sv:12 uvm-deprecated-api"]);
}


/// The cases of the L2 review: each line names the finding expected (or none).
#[test]
fn hexk_review_cases() {
    let tb = r#"`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class t1 extends uvm_env;
    `uvm_component_utils(t1)
    bit enabled;
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (enabled)
        env = my_env::type_id::create("env", this);
    endfunction
    function void connect_phase(uvm_phase phase);
      if (touch()) begin end
      super.connect_phase(phase);
    endfunction
    function bit touch(); return 1; endfunction
  endclass
  class t2 extends uvm_env;
    `uvm_component_utils(t2)
    my_env env = new("env", null);
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      my_env e2 = my_env::type_id::create("e2", this);
    endfunction
  endclass
  class settings;
    int goal;
  endclass
  class user;
    settings option;
    function void f(); option.goal = 90; endfunction
    function int randomize_all(); return 1; endfunction
    function void g(); void'(randomize_all()); endfunction
  endclass
  function bit inspect(uvm_object o); return 1; endfunction
  class t3 extends uvm_env;
    `uvm_component_utils(t3)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      bit ok;
      super.build_phase(phase);
      ok = inspect(my_env::type_id::create("item", this));
      uvm_config_db#(int)::set(.cntxt(this), .inst_name("sqr.main_phase"), .field_name("default_sequence"), .value(0));
    endfunction
    task run_phase(uvm_phase phase);
      phase.raise_objection(.obj(this), .description());
    endtask
  endclass
endpackage
module m;
  bit [1:0] a, b;
  covergroup cg;
    cp_a: coverpoint a iff (a inside {1, 2}) { option.weight = 0; }
    cp_b: coverpoint b;
    x: cross cp_a, cp_b { option.weight = 0; }
  endgroup
endmodule
"#;
    let (a, root) = project("review2", &[("tb.sv", tb)]);
    let want = ["missing-super-phase-call", "component-created-with-new", "create-name-matches", "default-sequence", "objection-without-description", "coverage-weight-goal", "randomize-void-cast"];
    let got: Vec<String> = lint(&a, &root, &[Ruleset::Hexk]).into_iter().filter(|s| want.iter().any(|w| s.contains(&format!(" {w}")))).collect();
    assert_eq!(
        got,
        [
            "tb.sv:13 missing-super-phase-call",
            "tb.sv:17 missing-super-phase-call",
            "tb.sv:25 component-created-with-new",
            "tb.sv:27 missing-super-phase-call",
            "tb.sv:48 default-sequence",
            "tb.sv:51 objection-without-description",
            "tb.sv:60 coverage-weight-goal",
        ]
    );
}
