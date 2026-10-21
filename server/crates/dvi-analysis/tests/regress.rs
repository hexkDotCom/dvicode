//! Regressions found on real code (Questa UVM Framework, OVM).

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-regress-{name}-{}", std::process::id()));
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
fn package_exports_are_visible_through_imports() {
    let (a, _) = project(
        "export",
        &[
            ("a_pkg.sv", "package a_pkg;\n  typedef enum {ACTIVE, PASSIVE} mode_t;\n  parameter string NAME = \"x\";\nendpackage\n"),
            ("b_pkg.sv", "package b_pkg;\n  import a_pkg::*;\n  export a_pkg::mode_t;\n  export a_pkg::NAME;\n  export a_pkg::ACTIVE;\nendpackage\n"),
            ("c_pkg.sv", "package c_pkg;\n  import a_pkg::*;\n  export *::*;\nendpackage\n"),
            ("use.sv", "module use_it;\n  import b_pkg::*;\n  mode_t m = ACTIVE;\n  string s = NAME;\nendmodule\nmodule use_all;\n  import c_pkg::*;\n  mode_t m = PASSIVE;\nendmodule\n"),
        ],
    );
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn import_inside_a_block_and_dpi_export() {
    let (a, _) = project(
        "blockimport",
        &[
            ("p.sv", "package p;\n  function automatic int f(); return 1; endfunction\nendpackage\n"),
            (
                "t.sv",
                "module t;\n  export \"DPI-C\" function g;\n  function void g(); endfunction\n  initial begin\n    import p::f;\n    int x;\n    x = f();\n  end\nendmodule\n",
            ),
        ],
    );
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn verilog_xl_protected_regions_are_skipped() {
    let (a, _) = project(
        "protected",
        &[(
            "enc.sv",
            "class visible;\n`protected\n  MTI!#garbage;;; \"unterminated ( [ {\n  endclass endmodule `define X\n`endprotected\nendclass\n",
        )],
    );
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn escaped_quotes_inside_macro_strings() {
    // SVUnit's FAIL_UNLESS_STR_EQUAL builds its message with `"\"%s\" != \"%s\"`".
    let (a, _) = project(
        "macrostr",
        &[(
            "t.sv",
            r#"`define CHECK_EQ(a,b) \
  begin \
    string sa; \
    sa = a; \
    if (sa.compare(b) != 0) $display($sformatf(`"\"%s\" != \"%s\" in a\\b`", sa, b)); \
  end
module t;
  initial `CHECK_EQ("x", "y")
  initial begin int n; n = 1; end
endmodule
"#,
        )],
    );
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn paste_before_punctuation_keeps_tokens_apart() {
    // SVUnit's SVTEST ends with `task automatic SVTEST_``_NAME_``();`.
    let (a, _) = project(
        "pasteparen",
        &[(
            "t.sv",
            "`define T(n) task automatic run_``n``(); \\\n  $display(`\"n`\");\nmodule t;\n  `T(a)\n  endtask\n  initial run_a();\nendmodule\n",
        )],
    );
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn self_referencing_base_class_does_not_crash() {
    let (a, _) = project("selfbase", &[("t.sv", "class C extends C::T;\nendclass\nmodule m;\n  C c;\n  initial c.x = 1;\nendmodule\n")]);
    // Problems are expected; reaching here without a stack overflow is the point.
    let _ = problems(&a);
}

#[test]
fn deeply_nested_calls_do_not_crash() {
    let n = 50_000;
    let text = format!("module m;\n  function int f(int a); return a; endfunction\n  int x = {}1{};\nendmodule\n", "f(".repeat(n), ")".repeat(n));
    let (a, _) = project("deepcalls", &[("t.sv", &text)]);
    let p = problems(&a);
    assert!(p.iter().any(|m| m.contains("nested too deeply")), "{p:?}");
}

#[test]
fn crlf_string_continuation() {
    let (a, _) = project("crlfstr", &[("t.sv", "module m;\r\n  string s = \"abc \\\r\ndef\";\r\nendmodule\r\n")]);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn quick_fixes_keep_crlf_line_endings() {
    use dvi_syntax::SourceProvider;
    let (a, root) = project("crlffix", &[("t.sv", "module t (input logic clk);\r\n  initial undeclared_v = 1;\r\nendmodule\r\n")]);
    let f = a.vfs.file_id(&root.join("t.sv"));
    let fixes = a.quick_fixes(f, 0, u32::MAX);
    let declare = fixes.iter().find(|q| q.title.starts_with("Declare")).expect("a declare fix");
    assert!(declare.edits.iter().all(|(_, t)| !t.contains('\n') || t.contains("\r\n")), "{:?}", declare.edits);
}

#[test]
fn builds_report_progress_and_stop_when_cancelled() {
    use dvi_project::{Phase, Progress};
    let (_, root) = project("progress", &[("a.sv", "module a; endmodule\n"), ("b.sv", "module b; endmodule\n")]);
    let setup = || {
        let cfg = dvi_project::load(&root, None, &FxHashMap::default());
        Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default()))
    };
    // A build that runs to the end has counted every top file.
    let p = Progress::new();
    let a = Analysis::build_with(Arc::new(Vfs::new()), setup(), &p).expect("not cancelled");
    assert_eq!(p.snapshot().0, Phase::Indexing);
    assert_eq!(a.status().top_files, 2);
    // A cancelled one gives nothing back.
    let p = Progress::new();
    p.cancel();
    assert!(Analysis::build_with(Arc::new(Vfs::new()), setup(), &p).is_none());
}

fn codes(a: &Analysis, file: &str) -> Vec<&'static str> {
    use dvi_syntax::SourceProvider;
    let f = a.project_files().into_iter().find(|f| a.vfs.path(*f).ends_with(file)).unwrap();
    let _ = a.vfs.text(f);
    a.file_diagnostics(f).iter().map(|d| d.code).collect()
}

#[test]
fn macro_changes_reach_later_files_incrementally() {
    let b_text = "`ifndef FLAG
module m; initial undeclared_q = 1; endmodule
`else
module m; endmodule
`endif
";
    let (a, root) = project("macroprop", &[("a.sv", "`define FLAG
"), ("b.sv", b_text)]);
    assert!(codes(&a, "b.sv").is_empty(), "{:?}", codes(&a, "b.sv"));
    // Remove FLAG in a.sv: the other branch of b.sv becomes active.
    let fa = a.vfs.set_overlay(&root.join("a.sv"), Arc::from("`define OTHER
"));
    let a2 = a.update(&[fa]);
    assert_eq!(codes(&a2, "b.sv"), vec!["UNDECLARED_IDENTIFIER"]);
    // And back.
    a2.vfs.set_overlay(&root.join("a.sv"), Arc::from("`define FLAG
"));
    let a3 = a2.update(&[fa]);
    assert!(codes(&a3, "b.sv").is_empty(), "{:?}", codes(&a3, "b.sv"));
}

#[test]
fn library_modules_are_found_while_editing() {
    let root = std::env::temp_dir().join(format!("dvicode-regress-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    std::fs::write(root.join("lib/sub.v"), "module sub(input a); endmodule\n").unwrap();
    std::fs::write(root.join("top.sv"), "module top; endmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "-y lib\n+libext+.v\ntop.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let ft = a.vfs.set_overlay(&root.join("top.sv"), std::sync::Arc::from("module top;\n  sub u(.a(1'b0));\nendmodule\n"));
    let a2 = a.update(&[ft]);
    assert!(codes(&a2, "top.sv").is_empty(), "{:?}", codes(&a2, "top.sv"));
}

#[test]
fn include_operand_macros_are_expanded() {
    let (a, _) = project(
        "incmacro",
        &[
            ("inc/defs.svh", "`define FROM_DEFS 1\n"),
            ("inc/other.svh", "`define FROM_OTHER 1\n"),
            (
                "top.sv",
                "`define HEADER \"inc/defs.svh\"\n`define ALIAS `HEADER\n`define PATH(f) `\"inc/f.svh`\"\n`include `ALIAS\n`include `PATH(other)\nmodule top;\n  int a = `FROM_DEFS + `FROM_OTHER;\nendmodule\n",
            ),
        ],
    );
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn non_ansi_port_header_is_renamed_too() {
    use dvi_syntax::SourceProvider;
    let (a, root) = project("nonansi", &[("m.sv", "module m(a, b);\n  input a;\n  output b;\n  assign b = a;\nendmodule\n")]);
    let f = a.vfs.file_id(&root.join("m.sv"));
    let text = a.vfs.text(f).unwrap();
    let off = text.find("input a").unwrap() as u32 + 6;
    let mut lines: Vec<u32> = a.references(f, off, true).iter().map(|l| l.range.start.line + 1).collect();
    lines.sort();
    assert_eq!(lines, vec![1, 2, 4]);
    // Navigation from the header reaches the declaration.
    let hdr = text.find("(a,").unwrap() as u32 + 1;
    let d: Vec<u32> = a.goto_definition(f, hdr).iter().map(|l| l.range.start.line + 1).collect();
    assert_eq!(d, vec![2]);
}

#[test]
fn macro_rename_keeps_the_backtick() {
    use dvi_syntax::SourceProvider;
    let (a, root) = project("macrorename", &[("t.sv", "`define OLD(x) (x)\nmodule t;\n  int a = `OLD(1) + `OLD(2);\nendmodule\n")]);
    let f = a.vfs.file_id(&root.join("t.sv"));
    let text = a.vfs.text(f).unwrap();
    let li = a.vfs.line_index(f).unwrap();
    let off = text.find("`OLD(1)").unwrap() as u32 + 2;
    let edit = a.rename(f, off, "NEW").unwrap();
    let edits = all_edits(edit);
    assert_eq!(edits.len(), 3);
    for (_, r) in edits {
        let s = li.offset(&text, r.start.line, r.start.character) as usize;
        let t = li.offset(&text, r.end.line, r.end.character) as usize;
        assert_eq!(&text[s..t], "OLD");
    }
}

fn all_edits(edit: lsp_types::WorkspaceEdit) -> Vec<(Option<i32>, lsp_types::Range)> {
    let Some(lsp_types::DocumentChanges::Edits(docs)) = edit.document_changes else { panic!("expected document changes") };
    docs.into_iter()
        .flat_map(|d| {
            let v = d.text_document.version;
            d.edits.into_iter().map(move |e| match e {
                lsp_types::OneOf::Left(t) => (v, t.range),
                lsp_types::OneOf::Right(t) => (v, t.text_edit.range),
            })
        })
        .collect()
}

#[test]
fn snapshots_keep_their_own_text_and_version() {
    use dvi_syntax::SourceProvider;
    let (a0, root) = project("snapshot", &[("s.sv", "module s;
  int count;
  initial count = 1;
endmodule
")]);
    let path = root.join("s.sv");
    let f = a0.vfs.file_id(&path);
    // The editor opens the file (version 7) and the analysis catches up.
    a0.vfs.set_overlay_versioned(&path, Arc::from("module s;
  int count;
  initial count = 1;
endmodule
"), Some(7));
    let a = a0.update(&[f]);
    let off = a.file_text(f).unwrap().find("count = 1").unwrap() as u32;
    // Then the editor changes the text (version 8) before this snapshot answers.
    a.vfs.set_overlay_versioned(&path, Arc::from("// a new first line
module s;
  int count;
  initial count = 1;
endmodule
"), Some(8));
    let d = a.goto_definition(f, off);
    assert_eq!(d[0].range.start.line, 1, "position from the snapshot's text, not the new one");
    let edits = all_edits(a.rename(f, off, "total").unwrap());
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|(v, _)| *v == Some(7)), "edits carry the snapshot's version: {edits:?}");
}

#[test]
fn interface_changes_are_detected() {
    use dvi_syntax::SourceProvider;
    let (a, root) = project("iface", &[("p.sv", "package p;\n  function automatic int f();\n    return 1;\n  endfunction\nendpackage\n")]);
    let f = a.vfs.file_id(&root.join("p.sv"));
    // A local change inside a function body: other files cannot see it.
    a.vfs.set_overlay(&root.join("p.sv"), Arc::from("package p;\n  function automatic int f();\n    int k = 2;\n    return k;\n  endfunction\nendpackage\n"));
    let b = a.update(&[f]);
    assert!(!b.interface_changed(&a));
    // A new package item: other files can see it.
    b.vfs.set_overlay(&root.join("p.sv"), Arc::from("package p;\n  int g;\n  function automatic int f();\n    int k = 2;\n    return k;\n  endfunction\nendpackage\n"));
    let c = b.update(&[f]);
    assert!(c.interface_changed(&b));
}

// ---- Found by scripts/corpus (OpenTitan, Ibex, core-v-verif, CVA6, PULP) -----------------

fn clean(name: &str, files: &[(&str, &str)]) {
    let (a, _) = project(name, files);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn packed_dimensions_after_a_type_name_in_lists() {
    // CVA6 `input scoreboard_entry_t [N-1:0] issue_sbe`, PULP `parameter byte_t [N-1:0] RegRstVal`,
    // OpenTitan `function ... (input sha_word64_t [7:0] h_i)` and `parameter token_idx_e [N][N] M`.
    clean(
        "typeddims",
        &[(
            "t.sv",
            "package p;
  typedef logic [7:0] byte_t;
  typedef logic [3:0] addr_t;
  parameter byte_t [1:0][3:0] M = '0;
  localparam addr_t [1:0] L = '0;
  function automatic byte_t [3:0] f(input byte_t [3:0] h_i, byte_t b);
    return h_i;
  endfunction
endpackage
module t import p::*; #(parameter byte_t [3:0] RST = '0) (
  input addr_t [1:0] addr_i,
  output byte_t [1:0] data_o,
  input logic plain [2]
);
  assign data_o = {addr_i[0], addr_i[1]};
endmodule
module u(a, b);
  input p::addr_t [1:0] a;
  output p::byte_t b;
endmodule
",
        )],
    );
}

#[test]
fn case_item_colon_followed_by_a_comment() {
    // `3'b000://comment` is not the `:/` of a `dist` (CVA6's vendored C910 FPU).
    clean(
        "colonslash",
        &[("t.sv", "module t(input logic [2:0] rm, output logic y);\n  always_comb begin\n    case (rm)\n      3'b000://round to nearest\n      begin\n        y = 1'b0;\n      end\n      3'b001:/* c */ y = 1'b1;\n      default: y = 1'b1;\n    endcase\n  end\nendmodule\n")],
    );
}

#[test]
fn macro_parameter_list_on_several_lines() {
    // OpenTitan cip_base_vseq__tl_errors.svh.
    clean(
        "mdefnl",
        &[(
            "t.sv",
            r#"`define CASE(task_name_, with_c_,
             seq_t_ = int,
             seqr_t)       \
  initial $display(task_name_, with_c_, seqr_t);
module t;
  `CASE(1, 2, , 4)
endmodule
"#,
        )],
    );
}

#[test]
fn macro_pastes_a_comment_together() {
    // common_cells registers.svh: `/``* synopsys sync_set_reset `"__clear`" *``/`.
    clean(
        "pastecomment",
        &[(
            "t.sv",
            r#"`define FFX(__q, __d, __clk) \
  /``* synopsys sync_set_reset `"__clk`" *``/ \
  always_ff @(posedge (__clk)) begin \
    __q <= __d; \
  end
module t(input logic clk, d, output logic q);
  `FFX(q, d, clk)
endmodule
"#,
        )],
    );
}

#[test]
fn paste_after_a_space_and_double_paste() {
    // riscv-dv `covergroup ``INSTR_NAME``_cg`, core-v-verif `rvfi_csr_``name````idx````suffix``_if`.
    clean(
        "pastespace",
        &[(
            "t.sv",
            r#"class instr; int rs1; endclass
`define CG_BEGIN(NAME, CLS = instr) \
  covergroup ``NAME``_cg with function sample(CLS i);
`define R_CG_BEGIN(NAME) \
  `CG_BEGIN(NAME) \
    cp_rs1 : coverpoint i.rs1;
class cov;
  `R_CG_BEGIN(add)
  endgroup
  function new(); add_cg = new(); endfunction
endclass
interface cif(input logic clk); logic en; endinterface
`define IDX_INST(n, s, idx) \
  cif r_``n````idx````s``_i (.clk(clk));
module t(input logic clk);
  `IDX_INST(cnt,,3)
  initial $display(r_cnt3_i.en);
endmodule
"#,
        )],
    );
}

#[test]
fn covergroup_coverpoint_on_a_concatenation() {
    // OpenTitan push_pull_agent_cov.sv: `coverpoint {valid, ready};`.
    clean(
        "cpconcat",
        &[(
            "t.sv",
            "package p;
covergroup vr_cg(string name, string path) with function sample(bit valid, bit ready);
  option.per_instance = 1;
  option.name = {path, \"::\", name};
  cp_vr: coverpoint {valid, ready};
endgroup : vr_cg
covergroup ra_cg(string name) with function sample(bit req, bit ack);
  cp_ra: coverpoint {req, ack} {
    ignore_bins ack_wo_req = {2'b01};
  }
endgroup : ra_cg
endpackage
",
        )],
    );
}

#[test]
fn named_assertions_are_scope_members() {
    // Ibex core_ibex_tb_top: `DV_ASSERT_CTRL("x", core_ibex_tb_top.NoAlertsTriggered)`.
    clean(
        "assertlabel",
        &[("t.sv", "module t(input logic clk, a);\n  NoA: assert property (@(posedge clk) !a);\n  CovA: cover property (@(posedge clk) a);\n  initial $assertoff(0, t.NoA);\n  initial $assertoff(0, NoA, CovA);\nendmodule\n")],
    );
}

#[test]
fn bind_parameters_and_instances() {
    // CV32E40P `bind cv32e40p_prefetch_controller: path ... #(.DEPTH(DEPTH))`, OpenTitan aes
    // `bind aes aes_cov_if u_aes_cov_if (...)` used as `dut.u_aes_cov_if`.
    let files = [(
        "t.sv",
        "interface cov_if(input logic clk); logic en; endinterface
module dut #(parameter int DEPTH = 4) (input logic clk);
endmodule
module sva #(parameter int DEPTH = 1) (input logic clk);
endmodule
module binds;
  bind dut sva #(.DEPTH(DEPTH)) u_sva (.clk(clk));
  bind dut cov_if u_cov_if (.clk);
  bind dut: tb.d sva #(.DEPTH(DEPTH)) u_sva2 (.*);
endmodule
module tb;
  logic clk;
  dut #(.DEPTH(8)) d (.clk);
  // OpenTitan csrng tb: `bind dut csrng_assert_if csrng_assert_if (...)` names an instance.
  bind d cov_if u_by_inst (.clk);
  initial $display(d.u_cov_if.en, d.u_by_inst.en, w.d.u_by_path.en);
  wrap w ();
  // OpenTitan edn tb: `bind dut.u_edn_core edn_assert_if edn_assert_if ();`.
  bind w.d cov_if u_by_path (.clk);
endmodule
module wrap;
  logic clk;
  dut d (.clk);
endmodule
",
    )];
    let (a, _) = project("bind", &files);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    // In the design hierarchy, the module holding the `bind`s has no instances of its own.
    let h = a.design_hierarchy(Some("binds"), 100);
    assert!(h[0].children.is_empty(), "{:?}", h[0].children.iter().map(|c| &c.name).collect::<Vec<_>>());
    // The bound instance is not a name in the module that contains the `bind`.
    let (a, _) = project("bindscope", &[files[0], ("u.sv", "module user;\n  binds b();\n  initial $display(b.u_cov_if.en);\nendmodule\n")]);
    let p = problems(&a);
    assert!(p.len() == 1 && p[0].starts_with("u.sv") && p[0].contains("'u_cov_if' is not declared"), "{p:?}");
}

#[test]
fn based_number_digits_from_a_macro() {
    // Ibex: `parameter int unsigned DmBaseAddr = 32'h`DM_ADDR;` with +define+DM_ADDR=1A11_0000.
    clean("basedmacro", &[("t.sv", "`define ADDR 1A11_0000\nmodule t;\n  parameter int unsigned A = 32'h`ADDR;\n  parameter int unsigned B = 'h`ADDR;\n  parameter int unsigned C = 8'h`ADDR + 1;\nendmodule\n")]);
}

#[test]
fn foreach_over_a_root_path() {
    // core-v-verif step_compare: `foreach (`CV32E40P_RM_RVVI_STATE.csr[index])` with `$root.`.
    clean(
        "foreachroot",
        &[("t.sv", "`define ST $root.top.u.state\nmodule sub; int state[4]; endmodule\nmodule top;\n  sub u();\n  initial foreach (`ST[i]) $display(i);\nendmodule\n")],
    );
}

#[test]
fn unit_level_import_reaches_later_files() {
    // core-v-verif: uvmt_cv32e40p_step_compare.sv has `import uvm_pkg::*;` outside any module;
    // the next file of the same compilation uses UVM_DEBUG.
    clean(
        "unitimport",
        &[
            ("a.sv", "package lv_pkg; typedef enum {LOW, HIGH} level_e; endpackage\nimport lv_pkg::*;\nmodule a; level_e l = LOW; endmodule\n"),
            ("b.sv", "module b; level_e l = HIGH; endmodule\n"),
        ],
    );
    // Not in earlier files.
    let (a, _) = project("unitimport2", &[("a.sv", "module b; int x = HIGH; endmodule\n"), ("b.sv", "package lv_pkg; typedef enum {LOW, HIGH} level_e; endpackage\nimport lv_pkg::*;\n")]);
    let p = problems(&a);
    assert!(p.len() == 1 && p[0].starts_with("a.sv") && p[0].contains("'HIGH' is not declared"), "{p:?}");
}

// ---- Found on closed-source projects; reduced to invented examples -----------------------

#[test]
fn names_on_unnamed_instance_terminals_are_implicit_nets() {
    // Standard-cell libraries: `chk_buf (a_chk, a, 1'b1);` instantiates a UDP without a name,
    // and `a_chk` is used again only in the specify block.
    clean(
        "unnamedudp",
        &[(
            "cells.sv",
            "primitive chk_buf (o, i, en);\n  output o; input i, en;\n  table 0 1 : 0; 1 1 : 1; endtable\nendprimitive\n\
             module leaf (input a, output y); assign y = a; endmodule\n\
             module cel1 (q, a);\n  input a; output q;\n  reg notifier;\n  buf (q, a);\n  chk_buf (a_chk, a, 1'b1);\n  chk_buf #1 (b_chk, a, 1'b0), (c_chk, a, 1'b1);\n  leaf (a, leaf_y);\n\
             \x20 specify\n    $setuphold (posedge a &&& a_chk, a, 0, 0, notifier);\n  endspecify\nendmodule\n",
        )],
    );
    // The primitive is a design element: navigation reaches it, and a missing one is reported.
    use dvi_syntax::SourceProvider;
    let (a, root) = project("unnamedudp2", &[("m.sv", "primitive p1 (o, i);\n  output o; input i;\n  table 0 : 0; 1 : 1; endtable\nendprimitive\nmodule m;\n  p1 (x, y);\n  p2 (x, y);\nendmodule\n")]);
    let f = a.vfs.file_id(&root.join("m.sv"));
    let off = a.vfs.text(f).unwrap().find("p1 (x").unwrap() as u32;
    assert_eq!(a.goto_definition(f, off).iter().map(|l| l.range.start.line).collect::<Vec<_>>(), vec![0]);
    let p = problems(&a);
    assert!(p.len() == 1 && p[0].contains("'p2' is not defined"), "{p:?}");
}

#[test]
fn macro_formal_after_a_backtick_names_the_macro_to_call() {
    // `` `define APPLY(MACRO, ARG) `MACRO(ARG) ``: simulators substitute MACRO, then expand it.
    clean(
        "macroformal",
        &[(
            "t.sv",
            "`define DECL(N) int N;\n`define APPLY0(MACRO, ARG) `MACRO(ARG)\n`define APPLY_N(MACRO, ARG, N) `APPLY``N(MACRO, ARG)\n\
             module t;\n  `APPLY0(DECL, first)\n  `APPLY_N(DECL, second, 0)\n  initial first = second;\nendmodule\n",
        )],
    );
}

#[test]
fn names_near_encrypted_code_are_not_reported() {
    // Vendor IP: the declarations of a module are encrypted, the code using them is not.
    clean(
        "encrypted",
        &[(
            "ip.sv",
            "module ip (input clk);\n`pragma protect begin_protected\n`pragma protect data_block\nAbCdEfGh0123+/==\n`pragma protect end_protected\n\
             \x20 always @(posedge clk) for (i = 0; i < 4; i = i + 1) hidden_r[i] <= 1'b0;\nendmodule\n",
        )],
    );
    // Other modules are still checked.
    let (a, _) = project("encrypted2", &[("ip.sv", "module ip;\n`pragma protect begin_protected\nxyz\n`pragma protect end_protected\nendmodule\nmodule other;\n  initial undeclared_x = 1;\nendmodule\n")]);
    let p = problems(&a);
    assert!(p.len() == 1 && p[0].contains("'undeclared_x'"), "{p:?}");
}

#[test]
fn inline_constraint_foreach_sees_the_randomized_object() {
    use dvi_syntax::SourceProvider;
    let text = "`define check(EXPR) if (!(EXPR)) $error(\"failed\");\nclass cfg_c;\n  rand int mults[4];\nendclass\nmodule t;\n  cfg_c c = new;\n  initial begin\n\
                \x20   `check(c.randomize() with { foreach (mults[i]) mults[i] inside {[1:3]}; })\n    if (!c.randomize() with { foreach (mults[i]) mults[i] < 5; }) $error(\"x\");\n  end\nendmodule\n";
    let (a, root) = project("withforeach", &[("t.sv", text)]);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    let f = a.vfs.file_id(&root.join("t.sv"));
    let off = text.find("foreach (mults").unwrap() as u32 + 9;
    assert_eq!(a.goto_definition(f, off).iter().map(|l| l.range.start.line).collect::<Vec<_>>(), vec![2]);
}

#[test]
fn macro_body_with_define_lines_and_a_multiline_argument() {
    // A macro that wraps its argument in a module twice, with `define/`undef lines and pasted
    // comments in its body, called with an argument that has `ifdef in it.
    clean(
        "definebody",
        &[(
            "t.sv",
            "`define alt_name(M) `ifdef ALT_``M``_ON ``M``_alt `else M `endif\n\
             `define wrap_defn(M, BODY) \\\n  module ``M``_wrap #(bit ALT = 0) (); \\\n    /``* pragma coverage off ``*/ \\\n    if (ALT) begin : body \\\n      `define ALT_``M``_ON \\\n      BODY \\\n      `undef ALT_``M``_ON \\\n    end \\\n\
             \x20   else begin : body \\\n      BODY \\\n    end \\\n    /``* pragma coverage on ``*/ \\\n  endmodule : ``M``_wrap\n\
             module dut; logic clk; endmodule\n\
             `wrap_defn(dut,\n  logic seen;\n`ifdef NOT_DEFINED\n  logic never;\n`endif\n  initial seen = 1;\n)\n",
        )],
    );
}

#[test]
fn solve_before_hard() {
    // VCS extension.
    clean("solvehard", &[("t.sv", "class pkt;\n  rand bit a; rand bit [3:0] b;\n  constraint c { solve a before b hard; solve a, b before c2; if (a) b == 0; }\n  rand bit c2;\nendclass\n")]);
}

#[test]
fn dpi_import_arguments_without_names() {
    clean(
        "dpinames",
        &[("t.sv", "module t;\n  import \"DPI-C\" pure function real fabs_r(input real);\n  import \"DPI-C\" function int add2(input int, input int b);\n  import \"DPI-C\" function void put(input bit [3:0], output bit);\n  real r = fabs_r(-1.0);\nendmodule\n")],
    );
    // Elsewhere a name is still required.
    let (a, _) = project("dpinames2", &[("t.sv", "module t;\n  function int f(input int);\n    return 0;\n  endfunction\nendmodule\n")]);
    let p = problems(&a);
    assert!(p.iter().any(|m| m.contains("Expected argument name")), "{p:?}");
}

#[test]
fn auto_config_does_not_know_the_defines_of_conditional_package_members() {
    // A package whose members are inside `ifdef FULL_ENV, defined only on the simulator
    // command line: without a build file, using them is not an error.
    let files = [
        ("env_pkg.sv", "package env_pkg;\n`ifdef FULL_ENV\n  parameter int NUM_AGENTS = 4;\n`endif\n  parameter int VISIBLE = 1;\nendpackage\n"),
        ("user.sv", "class user_seq;\n  int slots[env_pkg::NUM_AGENTS];\n  int other[env_pkg::VISIBLE];\nendclass\nmodule user_top;\n  import env_pkg::*;\n  int n = NUM_AGENTS;\nendmodule\n"),
    ];
    let (_, root) = project("autocond", &files);
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    // One notice instead, on the `ifdef, naming the macro and the uses it hides.
    let p = problems(&a);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].starts_with("env_pkg.sv: SKIPPED_CODE: `FULL_ENV` is not defined") && p[0].contains("2 uses") && p[0].contains("+define+FULL_ENV"), "{p:?}");
    let f = a.project_files().into_iter().find(|f| a.path(*f).ends_with("env_pkg.sv")).unwrap();
    let d = a.file_diagnostics(f).into_iter().next().unwrap();
    assert_eq!(d.severity, dvi_syntax::Severity::Info);
    assert_eq!(&*a.file_text(f).unwrap(), &files[0].1[..]);
    assert_eq!(&files[0].1[d.span.start as usize..d.span.end as usize], "FULL_ENV");
    // With a build file the defines are known, and the members are missing.
    let (a, _) = project("autocond2", &files);
    let p = problems(&a);
    assert!(p.len() == 2 && p.iter().all(|m| m.contains("'NUM_AGENTS'")), "{p:?}");
}

#[test]
fn the_else_of_an_ifndef_names_the_macro_to_define() {
    // `else of `ifndef LITE: skipped because LITE is not defined.
    let files = [
        ("p.sv", "package p;\n`ifndef LITE\n  int a;\n`else\n  int b;\n`endif\nendpackage\n"),
        ("m.sv", "module m;\n  initial p::b = 1;\nendmodule\n"),
    ];
    let (_, root) = project("autoelse", &files);
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let p = problems(&a);
    assert!(p.len() == 1 && p[0].contains("`LITE` is not defined") && p[0].contains("1 use ") && p[0].contains("+define+LITE"), "{p:?}");
    // Nothing used from it: no notice.
    let (_, root) = project("autoelse2", &[files[0], ("m.sv", "module m;\n  initial p::a = 1;\nendmodule\n")]);
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

// ---- Review of the fixes above ------------------------------------------------------------

#[test]
fn macro_default_after_a_continuation_inside_an_expansion() {
    // The default value of a macro defined by a macro ends at the continued line; no panic.
    let (a, _) = project("contdefault", &[("t.sv", "`define OUTER \\\n  `define INNER(x = \\\n    1) x\n`OUTER\nmodule t; endmodule\n")]);
    let _ = problems(&a);
}

#[test]
fn directives_are_not_macro_formals() {
    // A formal named like a directive does not turn `define into a call.
    clean("formaldirective", &[("t.sv", "`define MAKE(define) `define FLAG 1\n`MAKE(other)\nmodule m;\n  initial $display(`FLAG);\nendmodule\n")]);
}

#[test]
fn macro_named_by_an_expanded_actual() {
    // `M(N) with M = `SELECT: the actual is expanded first, then the macro it names is called.
    clean(
        "formalexpanded",
        &[("t.sv", "`define SELECT DECL\n`define DECL(N) int N;\n`define APPLY(M, N) `M(N)\nmodule m;\n  `APPLY(`SELECT, n)\n  initial n = 0;\nendmodule\n")],
    );
}

#[test]
fn hidden_package_members_open_only_wildcard_imports_and_direct_scopes() {
    let auto = |name: &str, files: &[(&str, &str)]| {
        let (_, root) = project(name, files);
        let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
        let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
        problems(&a)
    };
    // An explicit import of a known member brings in nothing else.
    let p = auto(
        "hiddenexplicit",
        &[("p.sv", "package p;\n  int known;\n`ifdef OPTIONAL\n  int extra;\n`endif\nendpackage\n"), ("m.sv", "module m;\n  import p::known;\n  initial typo = known;\nendmodule\n")],
    );
    assert!(p.len() == 1 && p[0].contains("'typo'"), "{p:?}");
    // Code removed inside a method declares locals, not class members.
    let p = auto(
        "hiddenmethod",
        &[("c.sv", "class C;\n  function void f();\n`ifdef OPTIONAL\n    int temporary;\n`endif\n  endfunction\nendclass\nmodule m;\n  C c;\n  initial c.typo = 1;\nendmodule\n")],
    );
    assert!(p.len() == 1 && p[0].contains("'typo' is not a member"), "{p:?}");
}

#[test]
fn unnamed_dpi_arguments_count_in_signatures() {
    use dvi_syntax::SourceProvider;
    let text = "module t;\n  import \"DPI-C\" function int add(input int, input int b);\n  int r;\n  initial r = add(1, 2);\nendmodule\n";
    let (a, root) = project("dpisig", &[("t.sv", text)]);
    let f = a.vfs.file_id(&root.join("t.sv"));
    let off = text.find("add(1, 2").unwrap() as u32 + 7;
    let sh = a.signature_help(f, off).expect("signature help");
    let sig = &sh.signatures[0];
    assert_eq!(sig.parameters.as_ref().map(|p| p.len()), Some(2), "{}", sig.label);
    assert_eq!(sh.active_parameter, Some(1));
    let _ = a.vfs.text(f);
}

#[test]
fn a_variable_named_hard_in_solve_before() {
    use dvi_syntax::SourceProvider;
    let text = "class C;\n  rand int a, hard;\n  constraint c { solve a before hard; }\nendclass\n";
    let (a, root) = project("solvevar", &[("t.sv", text)]);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    let f = a.vfs.file_id(&root.join("t.sv"));
    let off = text.find("before hard").unwrap() as u32 + 8;
    assert_eq!(a.goto_definition(f, off).iter().map(|l| l.range.start.line).collect::<Vec<_>>(), vec![1]);
    let _ = a.vfs.text(f);
}

#[test]
fn unnamed_instance_delay_names_a_parameter() {
    let udp = "primitive p (o, i);\n  output o; input i;\n  table 0 : 0; 1 : 1; endtable\nendprimitive\n";
    clean("udpdelay", &[("t.sv", &format!("{udp}module m;\n  localparam D = 1;\n  wire a, y;\n  p #D (y, a);\nendmodule\n"))]);
    let (a, _) = project("udpdelay2", &[("t.sv", &format!("{udp}module m;\n  wire a, y;\n  p #NO_SUCH (y, a);\nendmodule\n"))]);
    let p = problems(&a);
    assert!(p.len() == 1 && p[0].contains("'NO_SUCH'"), "{p:?}");
}

#[test]
fn conditional_enum_items_are_package_members() {
    // Enum items are visible in the package: code removed inside the enum may declare one.
    let (_, root) = project(
        "hiddenenum",
        &[
            ("p.sv", "package p;\n  typedef enum { A,\n`ifdef OPTIONAL\n    B,\n`endif\n    C } e_t;\nendpackage\n"),
            ("m.sv", "module m;\n  import p::*;\n  e_t e;\n  initial e = B;\nendmodule\n"),
        ],
    );
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    // Not an error: only the notice on the `ifdef.
    let p = problems(&a);
    assert!(p.len() == 1 && p[0].starts_with("p.sv: SKIPPED_CODE: `OPTIONAL` is not defined"), "{p:?}");
}

#[test]
fn macro_named_by_an_actual_expanded_through_a_function_macro() {
    clean(
        "formalexpanded2",
        &[("t.sv", "`define ID(X) X\n`define SELECT `ID(DECL)\n`define DECL(N) int N;\n`define APPLY(M, N) `M(N)\nmodule m;\n  `APPLY(`SELECT, n)\n  initial n = 0;\nendmodule\n")],
    );
    // A macro that names itself through the actual stops at the depth limit.
    let (a, _) = project("formalrecursive", &[("t.sv", "`define APPLY(M, N) `M(N)\n`define SELF `APPLY(`SELF, x)\nmodule m;\n  `SELF\nendmodule\n")]);
    let _ = problems(&a);
}

#[test]
fn a_variable_named_hard_after_a_comma_in_solve_before() {
    use dvi_syntax::SourceProvider;
    let text = "class C;\n  rand int a, b, hard;\n  constraint c { solve a before b, hard; solve a before b hard; }\nendclass\n";
    let (a, root) = project("solvecomma", &[("t.sv", text)]);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    let f = a.vfs.file_id(&root.join("t.sv"));
    // Both uses of the variable are references of its declaration.
    let decl = text.find("hard;").unwrap() as u32;
    let mut uses: Vec<(u32, u32)> = a.references(f, decl, false).iter().map(|l| (l.range.start.line, l.range.start.character)).collect();
    uses.sort();
    let col = text.lines().nth(2).unwrap().find("b, hard").unwrap() as u32 + 3;
    assert_eq!(uses, vec![(2, col)], "{uses:?}");
    let _ = a.vfs.text(f);
}

#[test]
fn macros_inside_backtick_quotes_are_expanded() {
    // A vendor library builds include paths as `"dir/`VENDOR/name`" (IEEE 1800 22.5.1: the
    // expansion of `" includes the expansion of embedded macros).
    clean(
        "quotedmacro",
        &[
            ("lib/vcs/a.svh", "int from_vcs;\n"),
            ("m.sv", "`define VENDOR vcs\n`define LIB(x) `\"lib/`VENDOR/x`\"\nmodule m;\n`include `LIB(a.svh)\n  initial from_vcs = 1;\nendmodule\n"),
        ],
    );
    // An undefined macro stays as written.
    let (a, _) = project("quotedmacro2", &[("m.sv", "`define LIB(x) `\"lib/`NOPE/x`\"\nmodule m;\n  string s = `LIB(a);\nendmodule\n")]);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn modport_subroutine_prototypes_with_arguments() {
    // A vendor interface's modport lists `export task name(args)` prototypes.
    clean(
        "modportproto",
        &[(
            "i.sv",
            "interface bus_if;\n  logic a;\n  modport mst(\n    export task start_exp(),\n    export task log_exp(ref int n, input string s),\n    import function int get(int x),\n    output a\n  );\nendinterface\n",
        )],
    );
}

#[test]
fn a_module_whose_header_is_encrypted() {
    // `module m` followed by an encrypted region: its `;`, ports and parameters are in the
    // ciphertext, so neither the header nor the instance connections are errors.
    let enc = "module enc_mod\n`pragma protect begin_protected\n`pragma protect data_method = \"aes128-cbc\"\n`pragma protect encoding = (enctype = \"base64\")\n`pragma protect data_block\nQUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo=\n`pragma protect end_protected\nendmodule\n";
    let top = "module top;\n  logic x;\n  enc_mod #(.WIDTH(8)) u (.clk(x), .data(x));\nendmodule\n";
    clean("encheader", &[("enc.sv", enc), ("top.sv", top)]);
    // Without the encrypted region a missing `;` is still an error.
    let (a, _) = project("encheader2", &[("m.sv", "module plain\n  logic x;\nendmodule\n")]);
    assert!(problems(&a).iter().any(|p| p.contains("Expected ';'")), "{:?}", problems(&a));
}

#[test]
fn a_module_header_cut_by_an_encrypted_region_then_an_include() {
    // A vendor checker: `module m`, the header in `protected text, then an include that starts
    // with an import.
    clean(
        "encinclude",
        &[
            ("disp.svi", "  import pkg_a::*;\n  int shown;\n"),
            ("pkg.sv", "package pkg_a;\n  int x;\nendpackage\n"),
            ("chk.sv", "module chk\n`protected\nQUJDREVGR0hJSktM\n`endprotected\n`include \"disp.svi\"\nendmodule\n"),
        ],
    );
}

#[test]
fn foreach_over_an_array_reached_through_several_selects() {
    // `foreach (f.lines[i].tiles[j][k].subs[l])`: i, j and k come from outer loops; l is new.
    clean(
        "foreachpath",
        &[(
            "t.sv",
            "class tile; int subs[4]; endclass\nclass line_c; tile tiles[2][2]; endclass\nclass frame; line_c lines[3]; endclass\nmodule t;\n  frame f;\n  initial foreach (f.lines[i]) foreach (f.lines[i].tiles[j, k]) foreach (f.lines[i].tiles[j][k].subs[l]) f.lines[i].tiles[j][k].subs[l] = l;\nendmodule\n",
        )],
    );
}

#[test]
fn member_labels_in_braces_without_an_apostrophe() {
    // `x = {valid: 1, ready: 0}`: accepted by VCS as an assignment pattern; the labels are struct
    // members, not variables. Names in a ternary inside braces are still checked.
    let (a, _) = project(
        "bracelabels",
        &[(
            "t.sv",
            "module t;\n  typedef struct packed { logic valid; logic ready; } s_t;\n  s_t s;\n  logic c;\n  initial begin\n    s = {valid: 1, ready: 0};\n    s = {c ? 1'b1 : nope, 1'b0};\n  end\nendmodule\n",
        )],
    );
    let p = problems(&a);
    assert!(p.len() == 1 && p[0].contains("'nope'"), "{p:?}");
}

#[test]
fn auto_config_resolves_names_defined_more_than_once_to_the_nearest_fitting_copy() {
    // Two tops, each with its own generated copy of a register package and block; a nearer
    // standalone stub of a package that lacks what the code uses; a deprecated copy of a
    // module, nearer than the real one, without the ports the code connects.
    let files = [
        ("top_a/reg_pkg.sv", "package reg_pkg;\n  parameter int NUM_A = 2;\nendpackage\n"),
        ("top_a/reg_top.sv", "module reg_top (input logic clk_i, input logic [reg_pkg::NUM_A-1:0] a_i);\nendmodule\n"),
        ("top_a/top.sv", "module top_a;\n  logic clk;\n  logic [1:0] a;\n  reg_top u_reg (.clk_i(clk), .a_i(a));\nendmodule\n"),
        ("top_b/reg_pkg.sv", "package reg_pkg;\n  parameter int NUM_B = 3;\nendpackage\n"),
        ("top_b/reg_top.sv", "module reg_top (input logic clk_i, input logic [reg_pkg::NUM_B-1:0] b_i);\nendmodule\n"),
        ("top_b/top.sv", "module top_b;\n  import reg_pkg::*;\n  logic clk;\n  logic [NUM_B-1:0] b;\n  reg_top u_reg (.clk_i(clk), .b_i(b));\nendmodule\n"),
        ("core/include/riscv_pkg.sv", "package riscv;\n  parameter int LD_FAULT = 5;\n  parameter int XLEN = 64;\nendpackage\n"),
        ("core/pmp/include/riscv.sv", "package riscv;\n  parameter int XLEN = 64;\nendpackage\n"),
        ("core/pmp/src/pmp_if.sv", "module pmp_if;\n  int c = riscv::LD_FAULT;\nendmodule\n"),
        ("core/cvfpu/deprecated/sram.sv", "module sram (input logic clk_i);\nendmodule\n"),
        ("common/util/sram.sv", "module sram #(parameter int USER_WIDTH = 1) (input logic clk_i, input logic rst_ni);\nendmodule\n"),
        ("core/cache/nbdcache.sv", "module nbdcache;\n  logic clk, rst;\n  sram #(.USER_WIDTH(2)) u_sram (.clk_i(clk), .rst_ni(rst));\nendmodule\n"),
    ];
    let (_, root) = project("autodup", &files);
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    // Go to definition lands in the copy that was used.
    let at = |file: &str, text: &str| {
        let f = a.project_files().into_iter().find(|f| a.path(*f).ends_with(file)).unwrap();
        let off = files.iter().find(|(n, _)| *n == file).unwrap().1.find(text).unwrap() as u32;
        let d: Vec<String> = a.goto_definition(f, off).iter().map(|l| l.uri.path().rsplit("autodup-").next().unwrap().split_once('/').unwrap().1.to_string()).collect();
        d
    };
    assert_eq!(at("top_b/top.sv", "reg_top u_reg"), vec!["top_b/reg_top.sv"]);
    assert_eq!(at("top_a/reg_top.sv", "reg_pkg::NUM_A"), vec!["top_a/reg_pkg.sv"]);
    assert_eq!(at("core/pmp/src/pmp_if.sv", "riscv::"), vec!["core/include/riscv_pkg.sv"]);
    assert_eq!(at("core/cache/nbdcache.sv", "sram #"), vec!["common/util/sram.sv"]);
    // With a build file the first compiled copy wins, as in a simulator.
    let (a, _) = project("autodup2", &files);
    assert!(!problems(&a).is_empty());
}

#[test]
fn auto_config_reports_macros_defined_nowhere_once_per_file() {
    // `WIDTH comes from the simulator's command line; `LOCAL is defined in the folder, but after
    // its use.
    let files = [
        ("a.sv", "module a;\n  logic [`WIDTH-1:0] x;\n  logic [`WIDTH-1:0] y;\n  int z = `LOCAL;\nendmodule\n"),
        ("b.sv", "module b;\n  logic [`WIDTH-1:0] w;\nendmodule\n`define LOCAL 1\n"),
    ];
    let (_, root) = project("automacro", &files);
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let mut diags: Vec<(String, dvi_syntax::Severity, String)> = Vec::new();
    for f in a.project_files() {
        for d in a.file_diagnostics(f) {
            diags.push((a.path(f).file_name().unwrap().to_string_lossy().into_owned(), d.severity, d.message));
        }
    }
    diags.sort_by(|x, y| x.0.cmp(&y.0));
    let width: Vec<_> = diags.iter().filter(|d| d.2.contains("`WIDTH")).collect();
    assert_eq!(width.len(), 2, "{diags:?}");
    assert!(width.iter().all(|d| d.1 == dvi_syntax::Severity::Info && d.2.contains("+define+WIDTH")), "{diags:?}");
    assert!(width[0].0 == "a.sv" && width[0].2.contains("Its 2 uses in this file"), "{diags:?}");
    assert!(width[1].0 == "b.sv" && width[1].2.contains("Its use in this file"), "{diags:?}");
    // Defined in the folder: still an error.
    assert!(diags.iter().any(|d| d.2 == "Macro `LOCAL is not defined" && d.1 == dvi_syntax::Severity::Error), "{diags:?}");
    // With a build file every use is an error.
    let (a, _) = project("automacro2", &files);
    assert_eq!(problems(&a).iter().filter(|m| m.contains("`WIDTH is not defined")).count(), 3);
}

#[test]
fn components_created_in_helpers_called_by_build_phase() {
    // Large testbenches build in helpers: `build_phase` calls `build_envs()` and
    // `this.build_agents()`, which a derived test may override (virtual dispatch).
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_agent extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = "package tb_pkg;\n  import uvm_pkg::*;\n  class my_agent extends uvm_agent; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class my_env extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class other_env extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class base_test extends uvm_test;\n    my_env env; my_agent agt;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    virtual function void build_envs(); env = my_env::type_id::create(\"env\", this); endfunction\n    function void build_agents(); this.agt = my_agent::type_id::create(\"agt\", this); endfunction\n    function void build_phase(uvm_phase phase); build_envs(); this.build_agents(); endfunction\n  endclass\n  class other_test extends base_test;\n    other_env oenv;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    virtual function void build_envs(); oenv = other_env::type_id::create(\"oenv\", this); endfunction\n  endclass\nendpackage\n";
    let (a, _) = project("buildhelpers", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let names = |test: &str| -> Vec<String> {
        let h = a.verification_hierarchy(Some(test), 100);
        h.roots.first().map(|r| r.children.iter().map(|c| format!("{}:{}", c.name, c.class)).collect()).unwrap_or_default()
    };
    assert_eq!(names("base_test"), vec!["env:my_env", "agt:my_agent"]);
    // The derived test's override of the helper is the one called.
    assert_eq!(names("other_test"), vec!["oenv:other_env", "agt:my_agent"]);
}
