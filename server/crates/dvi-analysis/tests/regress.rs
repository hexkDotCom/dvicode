//! Regressions found on real code (Questa UVM Framework, OVM).

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    project_in(&std::env::temp_dir(), name, files)
}

fn project_in(base: &std::path::Path, name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = base.join(format!("dvicode-regress-{name}-{}", std::process::id()));
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

#[test]
fn components_are_built_as_the_build_phase_runs() {
    // `super.build_phase()` is followed where it is called; without it the base class builds
    // nothing. The library's `build_phase` calls `build()`. Constructors call `super.new()`.
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = r#"package tb_pkg;
  import uvm_pkg::*;
  class e extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class base_test extends uvm_test;
    e env, mon, cov;
    function new(string name, uvm_component parent);
      super.new(name, parent);
      mon = e::type_id::create("mon", this);
    endfunction
    function void build_phase(uvm_phase phase);
      env = e::type_id::create("env", this);
    endfunction
  endclass
  class calls_super extends base_test;
    e extra;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      extra = e::type_id::create("extra", this);
      super.build_phase(phase);
    endfunction
  endclass
  class skips_super extends base_test;
    e extra;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      extra = e::type_id::create("extra", this);
    endfunction
  endclass
  class old_style extends uvm_test;
    e env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build();
      env = e::type_id::create("env", this);
    endfunction
  endclass
  class to_library extends old_style;
    e more;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      more = e::type_id::create("more", this);
    endfunction
  endclass
  class implicit_super extends base_test;
    function new(string name, uvm_component parent);
      cov = e::type_id::create("cov", this);
    endfunction
  endclass
  class branches extends uvm_test;
    e env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (mode) env = e::type_id::create("env", this);
      else env = e::type_id::create("env", this);
      make("a"); make("b");
      if (x) make2(); else make2();
    endfunction
    function void make(string n); e c; c = e::type_id::create(n, this); endfunction
    function void make2(); e d; d = e::type_id::create("d", this); endfunction
  endclass
endpackage
"#;
    let (a, _) = project("buildorder", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let names = |test: &str| -> Vec<String> {
        let h = a.verification_hierarchy(Some(test), 100);
        h.roots.first().map(|r| r.children.iter().map(|c| keyed(&c.name, c.dup)).collect()).unwrap_or_default()
    };
    fn keyed(n: &str, dup: Option<u32>) -> String {
        dup.map_or(n.to_string(), |d| format!("{n}#{d}"))
    }
    assert_eq!(names("base_test"), ["mon", "env"]);
    assert_eq!(names("calls_super"), ["mon", "extra", "env"]);
    assert_eq!(names("skips_super"), ["mon", "extra"]);
    assert_eq!(names("old_style"), ["env"]);
    assert_eq!(names("to_library"), ["env", "more"]);
    // `super.new()` runs before the derived constructor's own code.
    assert_eq!(names("implicit_super"), ["mon", "cov", "env"]);
    // Two branches make one component (one or the other); a helper given two names makes two;
    // one reached two ways is one.
    assert_eq!(names("branches"), ["env", "a", "b", "d"]);
}

#[test]
fn build_scan_review_cases() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    // Ten classes, each with a constructor; a base constructor creating with its argument.
    let mut chain = String::from("  class k0 extends uvm_test;\n    e first;\n    function new(string name, uvm_component parent, string child = \"first\");\n      super.new(name, parent);\n      first = e::type_id::create(child, this);\n    endfunction\n  endclass\n");
    for k in 1..10 {
        let args = if k == 1 { "super.new(name, parent, \"renamed\");" } else { "super.new(name, parent);" };
        chain.push_str(&format!("  class k{k} extends k{};\n    function new(string name, uvm_component parent); {args} endfunction\n  endclass\n", k - 1));
    }
    // Fan-out: f7 calls f6 twenty times under different conditions, and so on.
    let mut fan = String::from("    function void f0(); x = e::type_id::create(\"x\", this); endfunction\n");
    for k in 1..8 {
        let calls: String = (0..20).map(|j| format!("if (c{j}) f{}(); ", k - 1)).collect();
        fan.push_str(&format!("    function void f{k}(); {calls}endfunction\n"));
    }
    let tb = format!(
        r#"package tb_pkg;
  import uvm_pkg::*;
  class e extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class a_env extends e; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class b_env extends e; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class c_env extends e; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
{chain}
  class base_t extends uvm_test;
    e env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); build(); endfunction
    function void build(); env = a_env::type_id::create("env", this); endfunction
  endclass
  class derived_t extends base_t;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build(); env = b_env::type_id::create("env", this); endfunction
  endclass
  class replace_t extends uvm_test;
    e env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (mode) env = a_env::type_id::create("env", this);
      else env = b_env::type_id::create("env", this);
      env = c_env::type_id::create("env", this);
    endfunction
  endclass
  class forward_t extends uvm_test;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void make(string n); e c; c = e::type_id::create(n, this); endfunction
    function void wrapper(string n); make(n); endfunction
    function void build_phase(uvm_phase phase); wrapper("a"); wrapper("b"); endfunction
  endclass
  class fan_t extends uvm_test;
    e x, first;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
{fan}    function void build_phase(uvm_phase phase); first = e::type_id::create("first", this); f7(); endfunction
  endclass
endpackage
"#
    );
    let (a, _) = project("buildreview", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", &tb)]);
    let names = |test: &str| -> Vec<String> {
        let h = a.verification_hierarchy(Some(test), 100);
        h.roots.first().map(|r| r.children.iter().map(|c| format!("{}:{}", c.name, c.class)).collect()).unwrap_or_default()
    };
    // `build()` is virtual in UVM: the derived one runs.
    assert_eq!(names("derived_t"), ["env:b_env"]);
    // An unconditional creation replaces both branches before it.
    assert_eq!(names("replace_t"), ["env:c_env"]);
    // Literal names passed on through a helper.
    assert_eq!(names("forward_t"), ["a:e", "b:e"]);
    // The whole constructor chain runs, with the arguments of `super.new(...)`.
    assert_eq!(names("k9"), ["renamed:e"]);
    // The fan-out stops at a budget, keeping what was found, in reasonable time.
    let t0 = std::time::Instant::now();
    let h = a.verification_hierarchy(Some("fan_t"), 100);
    assert!(t0.elapsed().as_secs() < 10, "took {:?}", t0.elapsed());
    let r = &h.roots[0];
    assert_eq!(r.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["first", "x"]);
    assert!(r.truncated);
    assert!(r.children[1].conditions.len() <= 16);
}

#[test]
fn factory_overrides() {
    let uvm = "`define uvm_component_utils(T)\npackage uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_factory; endclass\n  uvm_factory factory;\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = r#"package tb_pkg;
  import uvm_pkg::*;
  class leaf extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class err_leaf extends leaf; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class worse_leaf extends err_leaf; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class drv extends uvm_env; `uvm_component_utils(drv) function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class err_drv extends drv; `uvm_component_utils(err_drv) function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class agent extends uvm_env;
    drv d;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); d = drv::type_id::create("d", this); endfunction
  endclass
  class env_a extends uvm_env;
    leaf l;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      l = leaf::type_id::create("l", this);
      set_type_override_by_type(leaf::get_type(), err_leaf::get_type());
    endfunction
  endclass
  class env_b extends uvm_env;
    leaf l, by_new;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      l = leaf::type_id::create("l", this);
      by_new = new("by_new", this);
    endfunction
  endclass
  class base_t extends uvm_test;
    env_a a; env_b b; agent agt1, agt2, agt3;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      a = env_a::type_id::create("a", this);
      b = env_b::type_id::create("b", this);
      agt1 = agent::type_id::create("agt1", this);
      agt2 = agent::type_id::create("agt2", this);
      agt3 = agent::type_id::create("agt3", this);
    endfunction
  endclass
  class t_type extends base_t;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      drv::type_id::set_type_override(err_drv::get_type());
      super.build_phase(phase);
    endfunction
  endclass
  class t_inst extends base_t;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      set_inst_override_by_type("agt1.d", drv::get_type(), err_drv::get_type());
      factory.set_inst_override_by_type(.original_type(drv::get_type()), .override_type(err_drv::get_type()), .full_inst_path("*.agt3.*"));
      super.build_phase(phase);
    endfunction
  endclass
  class t_cond extends base_t;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (inject) set_type_override("drv", "err_drv");
      set_type_override_by_type(leaf::get_type(), worse_leaf::get_type());
      set_type_override_by_type(leaf::get_type(), leaf::get_type(), 0);
      super.build_phase(phase);
    endfunction
  endclass
  class t_chain extends base_t;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      uvm_factory::get().set_type_override_by_type(drv::get_type(), err_drv::get_type());
      set_type_override_by_type(err_drv::get_type(), drv::get_type());
      super.build_phase(phase);
    endfunction
  endclass
endpackage
"#;
    let (a, _) = project("overrides", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let tree = |test: &str| -> Vec<String> {
        let h = a.verification_hierarchy(Some(test), 100);
        let mut out = Vec::new();
        fn walk(n: &dvi_analysis::ide::views::VerifNode, out: &mut Vec<String>) {
            for c in &n.children {
                let ov: Vec<String> = c.overrides.iter().map(|o| format!("{}{}>{}@{}{}", if o.applied { "" } else { "?" }, o.from, o.to, o.set_in, o.path.as_ref().map(|p| format!(" {p}")).unwrap_or_default())).collect();
                out.push(format!("{}:{}{}", c.path.trim_start_matches("uvm_test_top."), c.class, if ov.is_empty() { String::new() } else { format!(" [{}]", ov.join("; ")) }));
                walk(c, out);
            }
        }
        walk(&h.roots[0], &mut out);
        out
    };
    // env_a's override comes after its own leaf, and before env_b builds (name order).
    let base = ["a:env_a", "a.l:leaf", "agt1:agent", "agt1.d:drv", "agt2:agent", "agt2.d:drv", "agt3:agent", "agt3.d:drv", "b:env_b", "b.l:err_leaf [leaf>err_leaf@env_a::build_phase]", "b.by_new:leaf"];
    let sorted = |v: Vec<String>| {
        let mut v = v;
        v.sort();
        v
    };
    let mut want: Vec<String> = base.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(sorted(tree("base_t")), want);
    let t = tree("t_type");
    assert!(t.contains(&"agt2.d:err_drv [drv>err_drv@t_type::build_phase]".to_string()), "{t:?}");
    let t = tree("t_inst");
    assert!(t.contains(&"agt1.d:err_drv [drv>err_drv@t_inst::build_phase uvm_test_top.agt1.d]".to_string()), "{t:?}");
    assert!(t.contains(&"agt2.d:drv".to_string()), "{t:?}");
    assert!(t.contains(&"agt3.d:err_drv [drv>err_drv@t_inst::build_phase *.agt3.*]".to_string()), "{t:?}");
    let t = tree("t_cond");
    // Set under a condition: listed, not applied. `replace = 0` keeps the earlier override.
    assert!(t.contains(&"agt1.d:drv [?drv>err_drv@t_cond::build_phase]".to_string()), "{t:?}");
    assert!(t.contains(&"a.l:worse_leaf [leaf>worse_leaf@t_cond::build_phase]".to_string()), "{t:?}");
    // A loop builds the class where it closes, as UVM's factory does.
    let t = tree("t_chain");
    assert!(t.contains(&"agt1.d:drv [drv>err_drv@t_chain::build_phase; err_drv>drv@t_chain::build_phase]".to_string()), "{t:?}");
}

#[test]
fn factory_review_cases() {
    let uvm = "`define uvm_component_utils(T)\npackage uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_factory; endclass\n  uvm_factory factory;\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let mut chain = String::new();
    for k in 0..18 {
        chain.push_str(&format!("  class z{k} extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n"));
    }
    let chain_overrides: String = (0..17).map(|k| format!("      set_type_override_by_type(z{k}::get_type(), z{}::get_type());\n", k + 1)).collect();
    let tb = format!(
        r#"package tb_pkg;
  import uvm_pkg::*;
  class a extends uvm_env; `uvm_component_utils(a) function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class b extends a; `uvm_component_utils(b) function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class c extends a; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class p #(int W = 8) extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class q #(int W = 8) extends p #(W); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
{chain}
  class installer extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); set_type_override_by_type(a::get_type(), b::get_type()); endfunction
  endclass
  class t_ctor extends uvm_test;
    installer i; a x;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      i = installer::type_id::create("i", this);
      x = a::type_id::create("x", this);
    endfunction
  endclass
  class t_cond_inst extends uvm_test;
    installer i; a x;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (enabled) i = installer::type_id::create("i", this);
      repeat (0) set_type_override_by_type(a::get_type(), c::get_type());
      x = a::type_id::create("x", this);
    endfunction
  endclass
  class t_args extends uvm_test;
    a x, y, w; p #(16) wide;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      set_type_override_by_type(a::get_type(), pick ? b::get_type() : c::get_type());
      set_type_override_by_type(p#(8)::get_type(), q#(8)::get_type());
      factory.set_inst_override_by_type(a::get_type(), b::get_type(), "special.y");
      x = a::type_id::create("x", this);
      y = a::type_id::create("y", this, "special");
      wide = p#(16)::type_id::create("wide", this);
      set_type_override("a", "c");
      w = a::type_id::create("w", this);
    endfunction
  endclass
  class t_possible extends uvm_test;
    a x;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (mode) set_type_override_by_type(a::get_type(), b::get_type());
      set_type_override_by_type(a::get_type(), c::get_type());
      x = a::type_id::create("x", this);
    endfunction
  endclass
  class t_merge extends uvm_test;
    a child;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void make_child(); child = a::type_id::create("child", this); endfunction
    function void build_phase(uvm_phase phase);
      if (mode) make_child();
      set_type_override_by_type(a::get_type(), b::get_type());
      if (!mode) make_child();
    endfunction
  endclass
  class t_long extends uvm_test;
    z0 x;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
{chain_overrides}      x = z0::type_id::create("x", this);
    endfunction
  endclass
endpackage
"#
    );
    let (an, _) = project("factoryreview", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", &tb)]);
    let kids = |test: &str| -> Vec<String> {
        let h = an.verification_hierarchy(Some(test), 100);
        h.roots[0]
            .children
            .iter()
            .map(|c| {
                let possible = c.overrides.iter().filter(|o| !o.applied).map(|o| o.to.clone()).collect::<Vec<_>>();
                let alts = c.alternatives.iter().map(|x| x.class.clone()).collect::<Vec<_>>();
                format!("{}:{}{}{}", c.name, c.class, if possible.is_empty() { String::new() } else { format!(" ?{}", possible.join("/")) }, if alts.is_empty() { String::new() } else { format!(" |{}", alts.join("/")) })
            })
            .collect()
    };
    // A constructor's override applies to what is made after it.
    assert_eq!(kids("t_ctor"), ["i:installer", "x:b"]);
    // A component made under a condition registers its override under that condition; so does
    // a loop that may not run.
    assert_eq!(kids("t_cond_inst"), ["i:installer", "x:a ?b"]);
    // A computed type handle is not an override; parameter values make it uncertain; the context
    // is the factory's path; by name, an unregistered class is not an override.
    assert_eq!(kids("t_args"), ["x:a", "y:b", "wide:p ?q", "w:a"]);
    // A conditional override replaced by a later certain one cannot win.
    assert_eq!(kids("t_possible"), ["x:c"]);
    // One helper creating before and after an override: two classes.
    assert_eq!(kids("t_merge"), ["child:a |b"]);
    // A long chain is followed to its end, as UVM does.
    assert_eq!(kids("t_long"), ["x:z17"]);
}

#[test]
fn build_review_cases_2() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let deep = format!("{}1{}", "(".repeat(5000), ")".repeat(5000));
    let tb = format!(
        r#"package tb_pkg;
  import uvm_pkg::*;
  class e extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class with_leaf extends uvm_env;
    e leaf;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); leaf = e::type_id::create("leaf", this); endfunction
  endclass
  class t_limits extends uvm_test;
    with_leaf a; e b;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      a = with_leaf::type_id::create("a", this);
      b = e::type_id::create("b", this);
    endfunction
  endclass
  class base_t extends uvm_test;
    e x;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void make_phase(); x = e::type_id::create("base_made", this); endfunction
    function void build_phase(uvm_phase phase); make_phase(); endfunction
  endclass
  class derived_t extends base_t;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void make_phase(); x = e::type_id::create("derived_made", this); endfunction
  endclass
  class t_misc extends uvm_test;
    e huge[8], deep_e, s;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void make_child(string n); e c; c = e::type_id::create(n, this); endfunction
    function void wrapper(string n); n = "actual"; make_child(n); endfunction
    function void build_phase(uvm_phase phase);
      for (longint i = 0; i <= 9223372036854775807; i++) huge[i] = e::type_id::create($sformatf("h%0d", i), this);
      repeat ({deep}) deep_e = e::type_id::create("deep_e", this);
      for (int i = 0; i < 4; i++) begin i++; s = e::type_id::create("s", this); end
      wrapper("original");
    endfunction
  endclass
endpackage
"#
    );
    let (a, _) = project("buildreview2", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", &tb)]);
    // Limits keep the depth-first prefix; a match beyond a dropped sibling still fits.
    let mut q = dvi_analysis::ide::views::TreeQuery::new(2);
    let (h, _) = a.verification_tree(Some("t_limits"), &q);
    let mut names = Vec::new();
    fn walk(n: &dvi_analysis::ide::views::VerifNode, out: &mut Vec<String>) {
        for c in &n.children {
            out.push(c.path.clone());
            walk(c, out);
        }
    }
    walk(&h.roots[0], &mut names);
    assert_eq!(names, ["uvm_test_top.a", "uvm_test_top.a.leaf"]);
    q = dvi_analysis::ide::views::TreeQuery::new(1);
    q.filter = dvi_analysis::ide::filter::Filter::new("b");
    let (h, _) = a.verification_tree(Some("t_limits"), &q);
    assert_eq!(h.roots[0].children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["b"]);
    // A helper whose name ends in `_phase` is not virtual for that.
    assert_eq!(a.verification_hierarchy(Some("derived_t"), 10).roots[0].children[0].name, "base_made");
    let h = a.verification_hierarchy(Some("t_misc"), 10);
    let by = |n: &str| h.roots[0].children.iter().find(|c| c.name == n).cloned();
    // Overflowing and deeply nested bounds are not counted (and do not crash); a loop whose
    // body changes its variable is not counted.
    assert_eq!(by("huge[i]").and_then(|c| c.count), Some("?".into()));
    assert_eq!(by("deep_e").and_then(|c| c.count), Some("?".into()));
    assert_eq!(by("s").and_then(|c| c.count), Some("?".into()));
    // An argument changed in the helper does not pass on the caller's value.
    assert!(by("original").is_none() && by("c").is_some(), "{:?}", h.roots[0].children.iter().map(|c| &c.name).collect::<Vec<_>>());
}

#[test]
fn objects_switch() {
    let uvm = r#"`define uvm_object_utils(T)
`define uvm_do(S) begin S = new(); S.start(m_sequencer, this); end
`define uvm_do_on(S, Q) begin S = new(); S.start(Q, this); end
package uvm_pkg;
  class uvm_object; function new(string name = ""); endfunction endclass
  class uvm_component extends uvm_object; function new(string name, uvm_component parent); endfunction endclass
  class uvm_sequence_item extends uvm_object; function new(string name = ""); endfunction endclass
  class uvm_sequence_base extends uvm_sequence_item; function new(string name = ""); endfunction endclass
  class uvm_sequence #(type REQ = int) extends uvm_sequence_base; function new(string name = ""); endfunction endclass
  class uvm_reg_block extends uvm_object; function new(string name = ""); endfunction endclass
  class uvm_reg extends uvm_object; function new(string name = ""); endfunction endclass
  class uvm_reg_adapter extends uvm_object; function new(string name = ""); endfunction endclass
  class uvm_event extends uvm_object; function new(string name = ""); endfunction endclass
  class uvm_object_wrapper; endclass
  class uvm_config_db #(type T = int); static function void set(uvm_component c, string i, string f, T v); endfunction endclass
  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_sequencer extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
endpackage
"#;
    let tb = r#"package tb_pkg;
  import uvm_pkg::*;
  class item extends uvm_sequence_item; function new(string name = ""); super.new(name); endfunction endclass
  class cfg_c extends uvm_object; function new(string name = ""); super.new(name); endfunction endclass
  class regs extends uvm_reg_block; function new(string name = ""); super.new(name); endfunction endclass
  class one_reg extends uvm_reg; function new(string name = ""); super.new(name); endfunction endclass
  class adapter_c extends uvm_reg_adapter; function new(string name = ""); super.new(name); endfunction endclass
  class leaf_seq extends uvm_sequence #(item);
    item req;
    function new(string name = ""); super.new(name); endfunction
    task body(); `uvm_do(req) endtask
  endclass
  class vseq extends uvm_sequence #(item);
    leaf_seq a, b;
    function new(string name = ""); super.new(name); endfunction
    task body();
      `uvm_do(a)
      `uvm_do_on(b, p_sequencer.other)
    endtask
  endclass
  class sqr_c extends uvm_sequencer; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class agent extends uvm_env;
    sqr_c sqr;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); sqr = sqr_c::type_id::create("sqr", this); endfunction
  endclass
  class env extends uvm_env;
    agent agt;
    regs model;
    one_reg r;
    adapter_c adapter;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      agt = agent::type_id::create("agt", this);
      model = regs::type_id::create("model");
      r = one_reg::type_id::create("r");
    endfunction
    function void connect_phase(uvm_phase phase);
      adapter = adapter_c::type_id::create("adapter");
    endfunction
  endclass
  class base_t extends uvm_test;
    env env_h;
    cfg_c cfg;
    uvm_event ev;
    vseq top_seq;
    leaf_seq other_seq;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (cfg == null) cfg = cfg_c::type_id::create("cfg");
      uvm_config_db#(cfg_c)::set(this, "env*", "cfg", cfg);
      ev = new("ev");
      env_h = env::type_id::create("environment", this);
      top_seq = vseq::type_id::create("top_seq");
      uvm_config_db#(uvm_object_wrapper)::set(this, "environment.agt.sqr.main_phase", "default_sequence", leaf_seq::type_id::get());
    endfunction
    task main_phase(uvm_phase phase);
      top_seq.start(env_h.agt.sqr);
      other_seq.start(null);
    endtask
  endclass
endpackage
"#;
    let (a, _) = project("objects", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let show = |objects: bool| -> Vec<String> {
        let mut q = dvi_analysis::ide::views::TreeQuery::new(1000);
        q.objects = objects;
        let (h, _) = a.verification_tree(Some("base_t"), &q);
        let mut out = Vec::new();
        fn walk(n: &dvi_analysis::ide::views::VerifNode, d: usize, out: &mut Vec<String>) {
            for c in &n.children {
                let starts: Vec<String> = c.starts.iter().map(|s| format!("{}{}@{}", if s.path { "" } else { "~" }, s.on, s.phase)).collect();
                let mut line = format!("{}{} : {} [{}]", "  ".repeat(d), c.name, c.class, c.kind);
                if !starts.is_empty() {
                    line.push_str(&format!(" on {}", starts.join(", ")));
                }
                if !c.notes.is_empty() {
                    line.push_str(&format!(" ({})", c.notes.join("; ")));
                }
                if c.allocated.is_some() {
                    line.push_str(" elsewhere");
                }
                out.push(line);
                walk(c, d + 1, out);
            }
        }
        walk(&h.roots[0], 0, &mut out);
        out
    };
    // Off: components only, as before.
    assert_eq!(show(false), ["environment : env [env]", "  agt : agent [env]", "    sqr : sqr_c [sequencer]"]);
    assert_eq!(
        show(true),
        [
            "environment : env [env]",
            "  agt : agent [env]",
            "    sqr : sqr_c [sequencer]",
            "      leaf_seq : leaf_seq [sequence] on uvm_test_top.environment.agt.sqr@main_phase (default sequence for main_phase (set in base_t))",
            "  model : regs [register model]",
            "  adapter : adapter_c [adapter]",
            "cfg : cfg_c [object] (set in config_db for uvm_test_top.env* as \"cfg\")",
            "top_seq : vseq [sequence] on uvm_test_top.environment.agt.sqr@main_phase",
            "  a : leaf_seq [sequence] on uvm_test_top.environment.agt.sqr@body",
            "  b : leaf_seq [sequence] on ~p_sequencer.other@body",
            "other_seq : leaf_seq [sequence] on ~null (no sequencer)@main_phase elsewhere",
        ]
    );
}

#[test]
fn objects_review_cases() {
    let uvm = r#"`define uvm_create_on(S, Q) begin S = new(); end
`define uvm_send(S) begin S.start(null); end
package uvm_pkg;
  class uvm_object; function new(string name = ""); endfunction endclass
  class uvm_component extends uvm_object; function new(string name, uvm_component parent); endfunction endclass
  class uvm_sequence_item extends uvm_object; function new(string name = ""); endfunction endclass
  class uvm_sequence_base extends uvm_sequence_item; function new(string name = ""); endfunction endclass
  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
endpackage
"#;
    let tb = r#"package tb_pkg;
  import uvm_pkg::*;
  class cfg_a extends uvm_object; function new(string name = ""); super.new(name); endfunction endclass
  class cfg_b extends cfg_a; function new(string name = ""); super.new(name); endfunction endclass
  class s1 extends uvm_sequence_base; function new(string name = ""); super.new(name); endfunction endclass
  class s2 extends uvm_sequence_base; function new(string name = ""); super.new(name); endfunction endclass
  class child_seq extends uvm_sequence_base; function new(string name = ""); super.new(name); endfunction endclass
  class parent_seq extends uvm_sequence_base;
    child_seq c;
    function new(string name = ""); super.new(name); endfunction
    task body();
      `uvm_create_on(c, p_sequencer.other)
      `uvm_send(c)
    endtask
  endclass
  class installer extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); set_type_override_by_type(cfg_a::get_type(), cfg_b::get_type()); endfunction
  endclass
  class t extends uvm_test;
    installer inst;
    cfg_a late_cfg, alt_cfg;
    uvm_sequence_base s;
    s1 early;
    parent_seq p;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      inst = installer::type_id::create("inst", this);
      if (mode) alt_cfg = cfg_a::type_id::create("alt_cfg");
      else alt_cfg = cfg_b::type_id::create("alt_cfg");
      early = s1::type_id::create("early");
      s1::type_id::set_type_override(s2::get_type());
    endfunction
    function void connect_phase(uvm_phase phase);
      late_cfg = cfg_a::type_id::create("late_cfg");
    endfunction
    task main_phase(uvm_phase phase);
      s = s1::type_id::create("one");
      s.start(q1);
      s = s2::type_id::create("two");
      s.start(q2);
      early.start(q3);
      p.start(q4);
    endtask
  endclass
endpackage
"#;
    let (a, _) = project("objectsreview", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let mut q = dvi_analysis::ide::views::TreeQuery::new(1000);
    q.objects = true;
    let (h, _) = a.verification_tree(Some("t"), &q);
    let mut out = Vec::new();
    fn walk(n: &dvi_analysis::ide::views::VerifNode, d: usize, out: &mut Vec<String>) {
        for c in &n.children {
            let starts: Vec<String> = c.starts.iter().map(|s| s.on.clone()).collect();
            let alts: Vec<String> = c.alternatives.iter().map(|x| x.class.clone()).collect();
            out.push(format!("{}{} : {}{}{}", "  ".repeat(d), c.name, c.class, if starts.is_empty() { String::new() } else { format!(" on {}", starts.join(", ")) }, if alts.is_empty() { String::new() } else { format!(" | {}", alts.join(", ")) }));
            walk(c, d + 1, out);
        }
    }
    walk(&h.roots[0], 0, &mut out);
    assert_eq!(
        out,
        [
            "inst : installer",
            // Alternatives of an object.
            "alt_cfg : cfg_a | cfg_b",
            // connect_phase runs after every build: the installer's override applies.
            "late_cfg : cfg_b",
            // Made before the override: still s1, started in main_phase.
            "early : s1 on q3",
            // Each start goes to what the variable holds then.
            "one : s2 on q1",
            "two : s2 on q2",
            "p : parent_seq on q4",
            // uvm_send sends on the sequencer uvm_create_on gave.
            "  c : child_seq on p_sequencer.other",
        ]
    );
}

#[test]
fn sequence_review_cases() {
    let uvm = "package uvm_pkg;\n  class uvm_object; function new(string name = \"\"); endfunction endclass\n  class uvm_component extends uvm_object; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_sequence_item extends uvm_object; function new(string name = \"\"); endfunction endclass\n  class uvm_sequence_base extends uvm_sequence_item; function new(string name = \"\"); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = r#"package tb_pkg;
  import uvm_pkg::*;
  class s1 extends uvm_sequence_base; function new(string name = ""); super.new(name); endfunction endclass
  class s2 extends uvm_sequence_base; function new(string name = ""); super.new(name); endfunction endclass
  class s3 extends uvm_sequence_base; function new(string name = ""); super.new(name); endfunction endclass
  class t extends uvm_test;
    uvm_sequence_base built, s, r;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (mode) built = s1::type_id::create("built");
      else built = s2::type_id::create("built");
    endfunction
    task main_phase(uvm_phase phase);
      built.start(q0);
      if (mode) begin
        s = s1::type_id::create("one");
        s.start(q1);
      end else begin
        s = s2::type_id::create("two");
        s.start(q2);
      end
      s1::type_id::set_type_override(s3::get_type());
      r = s1::type_id::create("late");
      r.start(q3);
      if (a) r = s2::type_id::create("maybe");
      if (b) r.start(q4);
    endtask
  endclass
endpackage
"#;
    let (a, _) = project("seqreview", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let mut q = dvi_analysis::ide::views::TreeQuery::new(1000);
    q.objects = true;
    let (h, _) = a.verification_tree(Some("t"), &q);
    let got: Vec<String> = h.roots[0].children.iter().map(|c| format!("{} : {} on {}", c.name, c.class, c.starts.iter().map(|s| s.on.clone()).collect::<Vec<_>>().join(", "))).collect();
    assert_eq!(
        got,
        [
            // Both classes a build-time branch can make, each started.
            "built : s1 on q0",
            "built : s2 on q0",
            // A start in a branch goes to that branch's allocation only.
            "one : s1 on q1",
            "two : s2 on q2",
            // An override registered at run time applies to what is made after it.
            "late : s3 on q3, q4",
            // Conditions that can both hold: the start may run it.
            "maybe : s2 on q4",
        ]
    );
}

#[test]
fn conditions_loops_alternatives_and_ports() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_port_base; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_analysis_port #(type T = int) extends uvm_port_base; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_driver extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = r#"`define NUM 3
package tb_pkg;
  import uvm_pkg::*;
  localparam int N = 4;
  class c extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class d_a extends uvm_driver; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class d_b extends uvm_driver; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class t extends uvm_test;
    c agents[N];
    c grid[2][0:2];
    c dyn[];
    c three[`NUM];
    c down[8];
    c e, sb, drv0;
    uvm_driver drv;
    uvm_analysis_port #(int) ap, ap2;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      ap = new("ap", this);
      ap2 = new("ap2", this);
      if (cfg.is_active == UVM_ACTIVE) drv0 = c::type_id::create("drv0", this);
      if (cfg.has_sb) build_sb();
      else if (cfg.other) build_sb();
      foreach (agents[i]) agents[i] = c::type_id::create($sformatf("a%0d", i), this);
      foreach (grid[i, j]) grid[i][j] = c::type_id::create($sformatf("g%0d%0d", i, j), this);
      foreach (dyn[i]) dyn[i] = c::type_id::create($sformatf("y%0d", i), this);
      for (int i = 0; i < `NUM; i++) three[i] = c::type_id::create($sformatf("t%0d", i), this);
      for (int i = N * 2 - 1; i >= 0; i--) down[i] = c::type_id::create($sformatf("d%0d", i), this);
      repeat (2) begin if (stop) break; e = c::type_id::create("e", this); end
      case (mode)
        0: drv = d_a::type_id::create("drv", this);
        default: drv = d_b::type_id::create("drv", this);
      endcase
    endfunction
    function void build_sb(); sb = c::type_id::create("sb", this); endfunction
  endclass
endpackage
"#;
    let (a, _) = project("condloops", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let show = |ports: bool| -> Vec<String> {
        let mut q = dvi_analysis::ide::views::TreeQuery::new(100);
        q.ports = ports;
        let (h, _) = a.verification_tree(Some("t"), &q);
        let r = &h.roots[0];
        let mut out = vec![format!("ports hidden {}", r.ports_hidden)];
        for n in &r.children {
            let conds: Vec<String> = n.conditions.iter().map(|w| w.join(" && ")).collect();
            let alts: Vec<String> = n.alternatives.iter().map(|x| format!("{} when {}", x.class, x.conditions.iter().map(|w| w.join(" && ")).collect::<Vec<_>>().join(" or "))).collect();
            out.push(format!("{} {} [{}] x{} {{{}}} <{}>", n.name, n.class, conds.join(" or "), n.count.as_deref().unwrap_or("-"), n.loops.join("; "), alts.join(", ")));
        }
        out
    };
    assert_eq!(
        show(false),
        [
            "ports hidden 2",
            "drv0 c [cfg.is_active == UVM_ACTIVE] x- {} <>",
            "sb c [cfg.has_sb or !(cfg.has_sb) && cfg.other] x- {} <>",
            "agents[i] c [] x4 {foreach (agents[i])} <>",
            "grid[i][j] c [] x6 {foreach (grid[i, j])} <>",
            "dyn[i] c [] x? {foreach (dyn[i])} <>",
            "three[i] c [] x3 {for (int i = 0; i < `NUM; i++)} <>",
            "down[i] c [] x8 {for (int i = N * 2 - 1; i >= 0; i--)} <>",
            "e c [] xup to 2 {repeat (2)} <>",
            "drv d_a [case mode: 0] x- {} <d_b when case mode: default>",
        ]
    );
    let shown = show(true);
    assert_eq!(&shown[..3], ["ports hidden 0", "ap uvm_analysis_port [] x- {} <>", "ap2 uvm_analysis_port [] x- {} <>"]);
}

#[test]
fn type_parameters_of_base_tests_are_bound() {
    // A base test creates `T_ENV env`; derived tests give T_ENV by name, by position, through
    // a typedef, or pass on their own parameter; without a value the default is used.
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_agent extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = "package tb_pkg;\n  import uvm_pkg::*;\n  class cfg_c; endclass\n  class my_agent extends uvm_agent; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class base_env extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class rx_env extends uvm_env;\n    my_agent agt;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_phase(uvm_phase phase); agt = my_agent::type_id::create(\"agt\", this); endfunction\n  endclass\n  class tx_env extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class base_tc #(type T_CFG = cfg_c, type T_ENV = base_env) extends uvm_test;\n    T_ENV env;\n    T_ENV env2;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_phase(uvm_phase phase);\n      env = T_ENV::type_id::create(\"env\", this);\n      env2 = new(\"env2\", this);\n    endfunction\n  endclass\n  class named_tc extends base_tc#(.T_CFG(cfg_c), .T_ENV(rx_env));\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\n  class positional_tc extends base_tc#(cfg_c, tx_env);\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\n  typedef base_tc#(.T_ENV(tb_pkg::tx_env)) tx_base_t;\n  class typedef_tc extends tx_base_t;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\n  class mid_tc #(type E = rx_env) extends base_tc#(.T_ENV(E));\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\n  class passed_on_tc extends mid_tc#(tx_env);\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\n  class default_tc extends base_tc;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\nendpackage\n";
    let (a, _) = project("typeparams", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let tree = |test: &str| -> Vec<String> {
        let h = a.verification_hierarchy(Some(test), 100);
        let mut out = Vec::new();
        fn walk(n: &dvi_analysis::ide::views::VerifNode, out: &mut Vec<String>) {
            for c in &n.children {
                out.push(format!("{}:{}:{}", c.path, c.class, c.kind));
                walk(c, out);
            }
        }
        if let Some(r) = h.roots.first() {
            walk(r, &mut out);
        }
        out
    };
    assert_eq!(tree("named_tc"), vec!["uvm_test_top.env:rx_env:env", "uvm_test_top.env.agt:my_agent:agent", "uvm_test_top.env2:rx_env:env", "uvm_test_top.env2.agt:my_agent:agent"]);
    assert_eq!(tree("positional_tc"), vec!["uvm_test_top.env:tx_env:env", "uvm_test_top.env2:tx_env:env"]);
    assert_eq!(tree("typedef_tc"), vec!["uvm_test_top.env:tx_env:env", "uvm_test_top.env2:tx_env:env"]);
    assert_eq!(tree("passed_on_tc"), vec!["uvm_test_top.env:tx_env:env", "uvm_test_top.env2:tx_env:env"]);
    // mid_tc's own default (rx_env) when nothing below it gives E.
    assert_eq!(tree("mid_tc")[0], "uvm_test_top.env:rx_env:env");
    assert_eq!(tree("default_tc"), vec!["uvm_test_top.env:base_env:env", "uvm_test_top.env2:base_env:env"]);
}

#[test]
fn packages_exporting_each_other_do_not_crash() {
    let (a, _) = project(
        "exportcycle",
        &[
            ("a.sv", "package a;\n  import b::*;\n  export b::*;\n  int x;\nendpackage\n"),
            ("b.sv", "package b;\n  import a::*;\n  export a::*;\n  int y;\nendpackage\n"),
            ("t.sv", "module t;\n  int v = a::missing;\n  int w = a::y;\nendmodule\n"),
        ],
    );
    let p = problems(&a);
    assert!(p.iter().any(|m| m.contains("missing")), "{p:?}");
    assert!(!p.iter().any(|m| m.contains("'y'")), "{p:?}");
}

#[test]
fn recursive_macro_inside_a_quote_stops() {
    // Each level would expand `R twice: without recursion detection the work doubles per level.
    let (a, _) = project("quoterec", &[("t.sv", "`define R `\"`R`R`\"\nmodule m;\n  string s = `R;\nendmodule\n")]);
    let _ = problems(&a);
}

#[test]
fn macro_calls_with_arguments_inside_a_quote() {
    let (a, _) = project(
        "quotecall",
        &[
            ("dir/vcs/a.svh", "int from_a;\n"),
            ("t.sv", "`define ID(x) x\n`define Q(t) `\"dir/`ID(t)/a.svh`\"\nmodule m;\n  `include `Q(vcs)\n  initial from_a = 1;\nendmodule\n"),
        ],
    );
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn instances_in_one_statement_choose_a_copy_that_fits_all() {
    // Auto mode, two copies of `leaf`: the nearer one has no `y`.
    let files = [
        ("top/leaf.sv", "module leaf(input x);\nendmodule\n"),
        ("lib/leaf.sv", "module leaf(input x, input y);\nendmodule\n"),
        ("top/top.sv", "module top;\n  logic x, y;\n  leaf a(.x(x)), b(.x(x), .y(y));\nendmodule\n"),
    ];
    let (_, root) = project("autoshared", &files);
    let _ = std::fs::remove_dir_all(root.join(".dvicode"));
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn foreach_over_a_path_through_arrays() {
    // Member checks after `lines[i]` need the select counted.
    let (a, _) = project(
        "foreachsel",
        &[(
            "t.sv",
            "class sub_c; int v; endclass\nclass tile_c; sub_c subs[4]; endclass\nclass line_c; tile_c tiles[2][3]; endclass\nclass frame_c;\n  line_c lines[8];\n  int i, j, k;\n  function void f();\n    foreach (lines[i].tiles[j][k].subs[l]) lines[i].tiles[j][k].subs[l].v = 0;\n    foreach (lines[i].tiles[j][k].nope[l]) ;\n  endfunction\nendclass\n",
        )],
    );
    let p = problems(&a);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].contains("nope"), "{p:?}");
}

#[test]
fn helper_calls_follow_dispatch_order_and_active_code() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let chain: String = (1..=7).map(|k| format!("    function void l{k}(); l{}(); endfunction\n", k + 1)).collect::<String>().replace("l8()", "h()");
    let tb = format!(
        "package tb_pkg;\n  import uvm_pkg::*;\n  class a_env extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class b_env extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class base_test extends uvm_test;\n    a_env env; b_env child; a_env deep; a_env off;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_env(); env = a_env::type_id::create(\"env\", this); endfunction\n    function void first(); child = b_env::type_id::create(\"first\", this); endfunction\n    function void build_off(); off = a_env::type_id::create(\"off\", this); endfunction\n{chain}    function void h(); mk(); endfunction\n    function void mk(); deep = a_env::type_id::create(\"deep\", this); endfunction\n    function void build_phase(uvm_phase phase);\n      build_env();\n      first();\n      child = b_env::type_id::create(\"second\", this);\n`ifdef ENABLE_OFF\n      build_off();\n`endif\n      l1();\n      h();\n    endfunction\n  endclass\n  class derived_test extends base_test;\n    b_env other;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_env(); other = b_env::type_id::create(\"other\", this); endfunction\n  endclass\nendpackage\n"
    );
    let (a, _) = project("helperorder", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", &tb)]);
    let names = |test: &str| -> Vec<String> {
        let h = a.verification_hierarchy(Some(test), 100);
        h.roots.first().map(|r| r.children.iter().map(|c| c.name.clone()).collect()).unwrap_or_default()
    };
    // Source order; `first` and `second` are two components; `off` is not compiled; `deep` is
    // reached through the direct call to h() although l7() reaches h() at the depth limit.
    assert_eq!(names("base_test"), vec!["env", "first", "second", "deep"]);
    // build_env is not virtual: the base's build_phase calls its own.
    assert_eq!(names("derived_test"), vec!["env", "first", "second", "deep"]);
}

#[test]
fn include_operand_inside_a_quoted_macro_does_not_loop() {
    let (a, _) = project("quoteinclude", &[("t.sv", "`define A `\"`B`\"\n`define B `include `A\nmodule m;\n  string s = `A;\nendmodule\n")]);
    let _ = problems(&a);
}

#[test]
fn exponential_macros_inside_a_quote_stop() {
    let mut text = String::from("`define A0 x\n");
    for k in 1..=30 {
        text.push_str(&format!("`define A{k} `A{} `A{}\n", k - 1, k - 1));
    }
    text.push_str("`define Q `\"`A30`\"\nmodule m;\n  string s = `Q;\nendmodule\n");
    let t0 = std::time::Instant::now();
    let (a, _) = project("quoteexp", &[("t.sv", &text)]);
    let _ = problems(&a);
    assert!(t0.elapsed().as_secs() < 30, "took {:?}", t0.elapsed());
}

#[test]
fn quoted_call_of_a_formal_macro_name() {
    let (a, _) = project(
        "quoteformal",
        &[
            ("dir/foo/a.svh", "int from_a;\n"),
            ("t.sv", "`define ID(x) x\n`define Q(M,x) `\"dir/`M(x)/a.svh`\"\nmodule m;\n  `include `Q(ID,foo)\n  initial from_a = 1;\nendmodule\n"),
        ],
    );
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn export_of_many_packages_reaches_the_last() {
    let mut files: Vec<(String, String)> = (1..=70).map(|k| (format!("p{k}.sv"), format!("package p{k};\n{}endpackage\n", if k == 70 { "  int x;\n" } else { "" }))).collect();
    let imports: String = (1..=70).map(|k| format!("  import p{k}::*;\n")).collect();
    files.push(("api.sv".into(), format!("package api;\n{imports}  export *::*;\nendpackage\n")));
    files.push(("t.sv".into(), "module t;\n  int v = api::x;\nendmodule\n".into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let (a, _) = project("manyexports", &refs);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn type_parameter_defaults_and_macro_values() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = "`define ENV env_b\npackage tb_pkg;\n  import uvm_pkg::*;\n  class env_a extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class env_b extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class body_tc extends uvm_test;\n    parameter type A = env_a, B = env_b;\n    B env;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_phase(uvm_phase phase); env = new(\"env\", this); endfunction\n  endclass\n  class base #(type T_ENV = env_a) extends uvm_test;\n    T_ENV env;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_phase(uvm_phase phase); env = new(\"env\", this); endfunction\n  endclass\n  class macro_tc extends base#(`ENV);\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\nendpackage\n";
    let (a, _) = project("typeparamdefaults", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let class_of = |test: &str| a.verification_hierarchy(Some(test), 100).roots[0].children[0].class.clone();
    assert_eq!(class_of("body_tc"), "env_b");
    // The macro's class, or nothing; never the default.
    assert_ne!(class_of("macro_tc"), "env_a");
}

#[test]
fn many_inactive_regions_in_a_build_phase() {
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let body = "`ifdef NEVER_DEFINED\n      helper();\n`endif\n      ;\n".repeat(10_000);
    let tb = format!("package tb_pkg;\n  import uvm_pkg::*;\n  class t extends uvm_test;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void helper(); endfunction\n    function void build_phase(uvm_phase phase);\n{body}    endfunction\n  endclass\nendpackage\n");
    let (a, _) = project("manyinactive", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", &tb)]);
    let t0 = std::time::Instant::now();
    let _ = a.verification_hierarchy(Some("t"), 100);
    assert!(t0.elapsed().as_secs() < 30, "took {:?}", t0.elapsed());
}

#[test]
fn components_made_by_a_factory_object() {
    let uvm = "package uvm_pkg;\n  class uvm_object; function new(string name = \"\"); endfunction endclass\n  class uvm_component extends uvm_object; function new(string name, uvm_component parent); super.new(name); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let tb = "package tb_pkg;\n  import uvm_pkg::*;\n  class my_env extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class other_env extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class env_factory extends uvm_object;\n    my_env cache[string];\n    function new(string name = \"\"); super.new(name); endfunction\n    // Direct: returns what it creates.\n    function my_env make_env(string name, uvm_component parent);\n      return my_env::type_id::create(name, parent);\n    endfunction\n    // Cached, with a computed name.\n    function my_env get_env(uvm_component _parent, int _idx = 0);\n      string id = $sformatf(\"env%0d\", _idx);\n      if (!cache.exists(id)) begin\n        my_env e;\n        e = my_env::type_id::create(id, _parent);\n        cache[id] = e;\n      end\n      return cache[id];\n    endfunction\n    // The parent argument is reassigned: not followed.\n    function my_env moved(string name, uvm_component parent);\n      parent = null;\n      return my_env::type_id::create(name, parent);\n    endfunction\n    // Two creations: not followed.\n    function other_env two(uvm_component p);\n      other_env a, b;\n      a = other_env::type_id::create(\"a\", p);\n      b = other_env::type_id::create(\"b\", p);\n      return a;\n    endfunction\n  endclass\n  class t extends uvm_test;\n    env_factory f;\n    my_env direct, cached, other_parent;\n    other_env both;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n    function void build_phase(uvm_phase phase);\n      f = env_factory::type_id::create(\"f\");\n      direct = f.make_env(\"direct_env\", this);\n      this.cached = this.f.get_env(._idx(1), ._parent(this));\n      other_parent = f.make_env(\"x\", null);\n      both = f.two(this);\n      other_parent = f.moved(\"m\", this);\n    endfunction\n  endclass\nendpackage\n";
    let (a, _) = project("factory", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", tb)]);
    let h = a.verification_hierarchy(Some("t"), 100);
    let kids: Vec<String> = h.roots[0].children.iter().map(|c| format!("{}:{}", c.name, c.class)).collect();
    assert_eq!(kids, ["direct_env:my_env", "cached:my_env"]);
}

#[test]
fn classes_nested_by_a_macro_without_endclass() {
    // The macro that would close each class is not defined, so each class nests in the one
    // before it; finding a base walked every enclosing class's base again (2^depth lookups).
    let uvm = "package uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    let instrs: String = (0..40).map(|i| format!("  `DEFINE_INSTR(i{i})\n")).collect();
    let tb = format!("`define DEFINE_INSTR(n) \\n  class n``_instr extends base_instr; \\n    `INSTR_BODY(n)\npackage tb_pkg;\n  import uvm_pkg::*;\n  class base_instr; endclass\n{instrs}  class t extends uvm_test;\n    function new(string name, uvm_component parent); super.new(name, parent); endfunction\n  endclass\nendpackage\n");
    let (a, _) = project("nestedbymacro", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", &tb)]);
    let t0 = std::time::Instant::now();
    let tests: Vec<String> = a.uvm_tests().iter().map(|t| a.decl(*t).name.to_string()).collect();
    assert_eq!(tests, ["t"]);
    let _ = problems(&a);
    assert!(t0.elapsed().as_secs() < 30, "took {:?}", t0.elapsed());
}

#[test]
fn class_bases_past_the_depth_limit_with_the_memo() {
    // `R`'s base needs `Helper`'s at the depth limit, where `Helper`'s own base is refused:
    // a base found earlier at a shallower depth (through `Warm`) must not be reused there.
    let chain: String = (0..28).map(|i| format!("  class C{i} extends C{}::T; endclass\n", i + 1)).collect();
    let src = format!("package p;\n  class Leaf;\n    typedef Leaf T;\n    class A extends C0::T; endclass\n  endclass\n  class HelperBase extends Leaf; endclass\n  class Helper extends HelperBase::T; endclass\n  class Warm extends Helper::T; endclass\n{chain}  class C28 extends Helper::T; endclass\n  class R extends Warm::A::T; endclass\nendpackage\n");
    let (a, _) = project("basedepth", &[("p.sv", &src)]);
    let p = a.global.packages["p"][0];
    let class = |n: &str| a.member_of(p, n).unwrap();
    assert_eq!(a.class_base(class("Helper")), Some(class("Leaf")));
    assert_eq!(a.class_base(class("R")), None);
}

#[test]
fn release_review_cases() {
    let uvm = "package uvm_pkg;\n  class uvm_object; function new(string name = \"\"); endfunction endclass\n  class uvm_component extends uvm_object; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_sequence_item extends uvm_object; function new(string name = \"\"); endfunction endclass\n  class uvm_sequence_base extends uvm_sequence_item; function new(string name = \"\"); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    // 600 environments of 600 leaves each: past the model budget.
    let fields: String = (0..600).map(|k| format!("    e e{k};\n")).collect();
    let made: String = (0..600).map(|k| format!("      e{k} = e::type_id::create(\"e{k}\", this);\n")).collect();
    let leaves: String = (0..600).map(|k| format!("    leaf l{k};\n")).collect();
    let leaf_made: String = (0..600).map(|k| format!("      l{k} = leaf::type_id::create(\"l{k}\", this);\n")).collect();
    let tb = format!(
        r#"package tb_pkg;
  import uvm_pkg::*;
  class leaf extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class e extends uvm_env;
{leaves}    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
{leaf_made}    endfunction
  endclass
  class big extends uvm_test;
{fields}    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
{made}    endfunction
  endclass
  class s extends uvm_sequence_base; function new(string name = ""); super.new(name); endfunction endclass
  class d extends s; function new(string name = ""); super.new(name); endfunction endclass
  class t extends uvm_test;
    s seq;
    leaf l[4];
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      set_inst_override_by_type("seq", s::get_type(), d::get_type());
      for (int i = 0; i < 4; ++i) l[i] = leaf::type_id::create($sformatf("l%0d", i), this);
    endfunction
    task run_phase(uvm_phase phase);
      seq = s::type_id::create("seq", this);
      seq.start(null);
    endtask
  endclass
endpackage
"#
    );
    let (a, _) = project("releasereview", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", &tb)]);
    let t0 = std::time::Instant::now();
    let (h, meta) = a.verification_tree(Some("big"), &dvi_analysis::ide::views::TreeQuery::new(1));
    assert!(t0.elapsed().as_secs() < 20, "took {:?}", t0.elapsed());
    assert!(meta.incomplete.is_some() && h.roots[0].truncated);
    let mut q = dvi_analysis::ide::views::TreeQuery::new(100);
    q.objects = true;
    let (h, _) = a.verification_tree(Some("t"), &q);
    let kids: Vec<String> = h.roots[0].children.iter().map(|c| format!("{}:{}:{}", c.name, c.class, c.count.clone().unwrap_or_default())).collect();
    // `++i` counts; an instance override at the component's path applies at run time.
    assert_eq!(kids, ["l[i]:leaf:4", "seq:d:"]);
}

#[test]
fn auto_config_excludes_the_file_an_include_names_not_its_namesakes() {
    // `include "rtl/common.sv"` is rtl/common.sv; tb/common.sv is still compiled.
    let files = [
        ("rtl/common.sv", "module rtl_block; endmodule\n"),
        ("tb/common.sv", "module tb_block; endmodule\n"),
        ("top.sv", "`include \"rtl/common.sv\"\nmodule top;\n  tb_block u();\n  rtl_block v();\nendmodule\n"),
    ];
    let (_, root) = project("autoinc", &files);
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let tops: Vec<String> = cfg.invocations.iter().flat_map(|i| i.files.iter()).map(|f| f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/")).collect();
    assert!(tops.contains(&"tb/common.sv".to_string()) && !tops.contains(&"rtl/common.sv".to_string()), "{tops:?}");
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    // One file includes common.sv both ways: the bare one is still an include.
    let files = [
        ("rtl/common.sv", "localparam int RTL_ONLY = 1;\n"),
        ("common.sv", "class C extends base; endclass\n"),
        ("top.sv", "package p;\n  class base; endclass\n`include \"rtl/common.sv\"\n`include \"common.sv\"\nendpackage\n"),
    ];
    let (_, root) = project("autoinc2", &files);
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn a_typedef_named_through_itself_ends() {
    // `T::X` resolves through `T`, which is this typedef: the recursion overflowed the stack.
    let files = [("a.sv", "typedef T::X T;\nmodule m;\n  T v;\n  initial v.a = 1;\nendmodule\npackage q;\n  typedef U::Y U;\n  U w;\nendpackage\n")];
    let (a, root) = project("typedefself", &files);
    let _ = problems(&a);
    let f = a.vfs.lookup(&root.join("a.sv")).unwrap();
    let off = files[0].1.find("T v").unwrap() as u32;
    assert!(a.hover(f, off).is_some());
}

#[test]
fn hover_cuts_a_long_header_on_a_character_boundary() {
    // The header shown is cut at 1500 bytes: never inside a character.
    for pad in 1480..1500 {
        let text = format!("module m #(/*{}é*/parameter int P=0)(); endmodule\n", "a".repeat(pad));
        let (a, root) = project(&format!("hoverutf{pad}"), &[("m.sv", text.as_str())]);
        let f = a.vfs.lookup(&root.join("m.sv")).unwrap();
        assert!(a.hover(f, 7).is_some(), "{pad}");
    }
}

#[test]
fn for_loops_declare_every_variable_of_their_type() {
    // `j` is declared too; without a declaration first, `k = 0` stays an assignment.
    let files = [("c.sv", "class C;\n  int k, n;\n  function void f();\n    for (int i = 0, j = 0; i < 2; i++, j++)\n      $display(i, j);\n    for (k = 0, n = 1; k < 2; k++) ;\n  endfunction\nendclass\n")];
    let (a, root) = project("formulti", &files);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    let f = a.vfs.lookup(&root.join("c.sv")).unwrap();
    let text = files[0].1;
    let j = a.goto_definition(f, text.find("i, j)").unwrap() as u32 + 3);
    assert_eq!(j.first().map(|l| l.range.start.line), Some(3));
    let k = a.goto_definition(f, text.find("k = 0").unwrap() as u32);
    assert_eq!(k.first().map(|l| l.range.start.line), Some(1));
}

#[test]
fn line_macros_count_lines_once_per_file() {
    // `__LINE__ is the line of its use, inside a macro too; 100,000 uses stay fast.
    let dir = std::env::temp_dir().join(format!("dvicode-line-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = String::from("`define AT `__LINE__\nmodule m;\n  int a = `__LINE__;\n  int b = `AT;\n");
    for k in 0..100_000 {
        text.push_str(&format!("  int x{k} = `__LINE__;\n"));
    }
    text.push_str("endmodule\n");
    let path = dir.join("m.sv");
    std::fs::write(&path, &text).unwrap();
    let vfs = Vfs::new();
    let f = dvi_syntax::SourceProvider::file_id(&vfs, &path);
    let t = std::time::Instant::now();
    let r = dvi_syntax::preproc::preprocess(&vfs, &Default::default(), f, Default::default());
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    let numbers: Vec<&str> = r.tokens.iter().filter(|t| t.kind == dvi_syntax::TokKind::Number).map(|t| t.text.as_str()).collect();
    assert_eq!(&numbers[..3], ["3", "4", "5"]);
    assert_eq!(numbers.last(), Some(&"100004"));
}

#[test]
fn non_ansi_ports_of_a_large_netlist_match_quickly() {
    // Each body declaration completes its header port by name, not by a scan of the module.
    let n = 50_000;
    let header: Vec<String> = (0..n).map(|k| format!("p{k}")).collect();
    let body: String = (0..n).map(|k| format!("  input p{k};\n")).collect();
    let text = format!("module m({});\n{body}  assign p7 = p8;\nendmodule\n", header.join(", "));
    let t = std::time::Instant::now();
    let (a, root) = project("netlistports", &[("m.sv", text.as_str())]);
    assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    let f = a.vfs.lookup(&root.join("m.sv")).unwrap();
    let d = a.goto_definition(f, text.find("p7 = p8").unwrap() as u32);
    assert_eq!(d.first().map(|l| l.range.start.line), Some(8));
}

#[test]
fn deeply_nested_conditions_and_generate_blocks_end() {
    // On a stack the size of the engine's worker threads.
    let run = || {
        let n = 100_000;
        let gen = format!("module g;\n{}  wire x;\n{}endmodule\n", "if (1) begin\n".repeat(n), "end\n".repeat(n));
        let u = dvi_syntax::parser::parse(&dvi_syntax::lexer::lex(&gen, 0), 0);
        let blocks = u.diags.iter().filter(|d| d.message.contains("nested too deeply")).count();
        let dir = std::env::temp_dir().join(format!("dvicode-deepcond-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.sv");
        std::fs::write(&path, format!("`ifdef {}X{}\n  wire hidden;\n`endif\nmodule c; endmodule\n", "(".repeat(n), ")".repeat(n))).unwrap();
        let vfs = Vfs::new();
        let f = dvi_syntax::SourceProvider::file_id(&vfs, &path);
        let r = dvi_syntax::preproc::preprocess(&vfs, &Default::default(), f, Default::default());
        (blocks, r.tokens.iter().any(|t| t.text == "hidden"))
    };
    let (blocks, hidden) = std::thread::Builder::new().stack_size(16 << 20).spawn(run).unwrap().join().expect("no stack overflow");
    assert_eq!((blocks, hidden), (1, false));
}

#[test]
fn library_hierarchies_deeper_than_eight_levels_are_found() {
    // top -> L0 -> L1 -> ... -> L11, each in its own library file.
    let root = std::env::temp_dir().join(format!("dvicode-regress-deeplib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    for k in 0..12 {
        let body = if k < 11 { format!("  L{} u();\n", k + 1) } else { String::new() };
        std::fs::write(root.join(format!("lib/L{k}.sv")), format!("module L{k};\n{body}endmodule\n")).unwrap();
    }
    std::fs::write(root.join("top.sv"), "module top;\n  L0 u();\nendmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "-y lib\n+libext+.sv\ntop.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let p = problems(&a);
    assert!(p.is_empty(), "{p:?}");
    assert!(a.workspace_symbols("L11", 10).iter().any(|s| s.name == "L11"));
}

#[test]
fn named_connections_name_ports_and_parameters_only() {
    let files = [("t.sv", "module M #(parameter int W = 1) (input logic a);\n  localparam int L = 2;\n  int internal_signal;\nendmodule\nmodule top;\n  M #(.W(2), .L(3)) u(.a(1'b0), .internal_signal(1));\nendmodule\n")];
    let (a, _) = project("namedconn", &files);
    let p = problems(&a);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(p.iter().any(|m| m.contains("'L' is a localparam")) && p.iter().any(|m| m.contains("Port 'internal_signal'") && m.contains("a variable inside it")), "{p:?}");
}

#[test]
fn codebase_review_hierarchy_bounds() {
    let uvm = "package uvm_pkg;\n  class uvm_object; function new(string name = \"\"); endfunction endclass\n  class uvm_component extends uvm_object; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";
    // Constants of constants: D names C 1,000 times, C names B 1,000 times...
    let sum = |x: &str| vec![x; 1000].join(" + ");
    let overrides: String = (0..1000).map(|_| "      set_type_override(\"leaf\", \"leaf2\");\n").collect();
    let steps: String = (0..20_000).map(|_| "      x = x + 1;\n").collect();
    let calls: String = (0..2_000).map(|_| "      scratch = helper(0);\n").collect();
    let tb = format!(
        r#"package tb_pkg;
  import uvm_pkg::*;
  localparam int A = 0;
  localparam int B = {b};
  localparam int C = {c};
  localparam int D = {d};
  class leaf extends uvm_env; `uvm_component_utils(leaf) function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class leaf2 extends leaf; `uvm_component_utils(leaf2) function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class e extends uvm_env;
    e child;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      if (get_depth() < 4) child = e::type_id::create("child", this);
    endfunction
  endclass
  class t extends uvm_test;
    leaf x;
    e top_e;
    int scratch;
    function int helper(int x);
{steps}      return x;
    endfunction
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
{overrides}{calls}      repeat (D) x = leaf::type_id::create("x", this);
      top_e = e::type_id::create("top_e", this);
    endfunction
  endclass
endpackage
"#,
        b = sum("A"),
        c = sum("B"),
        d = sum("C")
    );
    let (a, _) = project("reviewbounds", &[("uvm_pkg.sv", uvm), ("tb_pkg.sv", &tb)]);
    let t0 = std::time::Instant::now();
    let (h, meta) = a.verification_tree(Some("t"), &dvi_analysis::ide::views::TreeQuery::new(100));
    assert!(t0.elapsed().as_secs() < 30, "took {:?}", t0.elapsed());
    let kids: Vec<String> = h.roots[0].children.iter().map(|c| format!("{}:{}", c.name, c.class)).collect();
    assert_eq!(kids, ["x:leaf2", "top_e:e"]);
    // e builds an e inside itself: the inner one is shown, not followed, and said to be cut.
    let inner = &h.roots[0].children[1].children[0];
    assert!(inner.name == "child" && inner.truncated && meta.incomplete.is_some(), "{} {} {:?}", inner.name, inner.truncated, meta.incomplete);
}

#[test]
fn for_loops_with_qualified_types_and_long_generate_chains() {
    let chain = format!("module g;\n  if (1) begin end\n{}  wire w;\nendmodule\n", "  else if (1) begin end\n".repeat(100_000));
    let files = [
        ("p.sv", "package p;\n  typedef int index_t;\nendpackage\n"),
        ("m.sv", "module m;\n  initial\n    for (p::index_t i = 0, j = 0; i < 4; i++, j++)\n      $display(i, j);\nendmodule\n"),
    ];
    let (a, root) = project("forqual", &files);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    let f = a.vfs.lookup(&root.join("m.sv")).unwrap();
    let j = a.goto_definition(f, files[1].1.find("i, j)").unwrap() as u32 + 3);
    assert_eq!(j.first().map(|l| l.range.start.line), Some(2));
    // A long `else if` chain is a loop, not a recursion: no stack is needed for it.
    let u = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || dvi_syntax::parser::parse(&dvi_syntax::lexer::lex(&chain, 0), 0).diags.len())
        .unwrap()
        .join()
        .expect("no stack overflow");
    assert_eq!(u, 0);
}

#[test]
fn nettypes_declare_types() {
    let files = [
        ("p.sv", "package rp;\n  typedef struct { real v; } s_t;\n  function automatic s_t res(input s_t d[]); return d[0]; endfunction\nendpackage\npackage p;\n  nettype rp::s_t wire_t with rp::res;\n  nettype real plain_t;\nendpackage\n"),
        ("m.sv", "module m;\n  p::wire_t a;\n  p::plain_t b;\n  if (type(a) == type(p::wire_t)) begin : g end\nendmodule\n"),
    ];
    let (a, _) = project("nettype", &files);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn completion_in_a_file_included_inside_a_module() {
    // body.svh is included inside m: completion there sees m's names, after the function the
    // header declares and closes too.
    let body = "function int f(); return 1; endfunction\nassign local_signal = loc;\n";
    let files = [("top.sv", "module m;\n  int local_signal;\n  `include \"body.svh\"\nendmodule\n"), ("body.svh", body)];
    let (a, root) = project("fragcomplete", &files);
    let f = a.vfs.lookup(&root.join("body.svh")).unwrap();
    let off = (body.find("= loc").unwrap() + 5) as u32;
    let names: Vec<String> = a.completions(f, off).into_iter().map(|c| c.label).collect();
    assert!(names.iter().any(|n| n == "local_signal"), "{names:?}");
    // The nearest name before the cursor is inside a function that has ended.
    let body = "function int g();\n  int t;\n  t = 1;\n  return t;\nendfunction\nassign local_signal = ";
    let files = [("top.sv", "module m;\n  int local_signal;\n  `include \"body.svh\"\nendmodule\n"), ("body.svh", body)];
    let (a, root) = project("fragcomplete2", &files);
    let f = a.vfs.lookup(&root.join("body.svh")).unwrap();
    // Right after `endfunction`: the nearest name is `t`, in g.
    let off = (body.find("endfunction").unwrap() + "endfunction\n".len()) as u32;
    let names: Vec<String> = a.completions(f, off).into_iter().map(|c| c.label).collect();
    assert!(names.iter().any(|n| n == "local_signal") && !names.iter().any(|n| n == "t"), "{names:?}");
    // A header that only declares, the cursor before the first name of a header, and a header
    // with nothing in it yet (the scope where it is included).
    let files = [
        ("top.sv", "module m;\n  int local_signal;\n  `include \"decl.svh\"\n  `include \"use.svh\"\n  `include \"blank.svh\"\nendmodule\n"),
        ("decl.svh", "int header_var;\n"),
        ("use.svh", "\nassign local_signal = 1;\n"),
        ("blank.svh", "// to do\n"),
    ];
    let (a, root) = project("fragcomplete3", &files);
    for (file, off) in [("decl.svh", 16u32), ("use.svh", 0), ("blank.svh", 9)] {
        let f = a.vfs.lookup(&root.join(file)).unwrap();
        let names: Vec<String> = a.completions(f, off).into_iter().map(|c| c.label).collect();
        assert!(names.iter().any(|n| n == "local_signal"), "{file}: {names:?}");
    }
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn an_include_and_the_editor_spelling_a_file_differently_share_it() {
    // `include "defs.svh" finds Defs.svh where case does not matter; the editor's text of
    // Defs.svh is what the includer sees.
    let (a, root) = project("casefile", &[("Defs.svh", "`define READY 1\n"), ("top.sv", "`include \"defs.svh\"\n`ifndef READY\nmodule m; initial undeclared_q = 1; endmodule\n`endif\n")]);
    assert!(codes(&a, "top.sv").is_empty(), "{:?}", codes(&a, "top.sv"));
    let f = a.vfs.set_overlay(&root.join("Defs.svh"), Arc::from("`define NOT_READY 1\n"));
    let a2 = a.update(&[f]);
    assert_eq!(codes(&a2, "top.sv"), vec!["UNDECLARED_IDENTIFIER"]);
}

/// Under WSL, a Windows drive (`/mnt/c`), where case does not matter either: the same as
/// [`an_include_and_the_editor_spelling_a_file_differently_share_it`].
#[cfg(target_os = "linux")]
#[test]
fn spellings_of_one_file_on_a_windows_drive_under_wsl_share_it() {
    let Some(base) = ["/mnt/c/Users/Public", "/mnt/c/Windows/Temp", "/mnt/c/tmp"].iter().map(std::path::Path::new).find(|d| {
        let probe = d.join(format!("dvicode-probe-{}", std::process::id()));
        let ok = std::fs::write(&probe, "").is_ok() && std::fs::metadata(d.join(format!("DVICODE-PROBE-{}", std::process::id()))).is_ok();
        let _ = std::fs::remove_file(&probe);
        ok
    }) else {
        eprintln!("skipped: no Windows drive that ignores case here");
        return;
    };
    let (a, root) = project_in(base, "wslcase", &[("Defs.svh", "`define READY 1
"), ("top.sv", "`include \"defs.svh\"
`ifndef READY
module m; initial undeclared_q = 1; endmodule
`endif
")]);
    assert!(codes(&a, "top.sv").is_empty(), "{:?}", codes(&a, "top.sv"));
    let f = a.vfs.set_overlay(&root.join("Defs.svh"), Arc::from("`define NOT_READY 1
"));
    let a2 = a.update(&[f]);
    assert_eq!(codes(&a2, "top.sv"), vec!["UNDECLARED_IDENTIFIER"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(windows)]
#[test]
fn files_differing_in_case_stay_apart_in_a_case_sensitive_folder() {
    // A folder made case-sensitive holds two files: they are not one file under two spellings.
    let root = std::env::temp_dir().join(format!("dvicode-regress-casesens-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    std::fs::create_dir_all(root.join("inc")).unwrap();
    let made = std::process::Command::new("fsutil.exe").args(["file", "setCaseSensitiveInfo"]).arg(root.join("inc")).arg("enable").output();
    if !made.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: no case-sensitive folders here");
        return;
    }
    std::fs::write(root.join("inc/A.svh"), "typedef int A_t;\n").unwrap();
    std::fs::write(root.join("inc/a.svh"), "typedef int a_t;\n").unwrap();
    std::fs::write(root.join("top.sv"), "module top;\n`include \"A.svh\"\n`include \"a.svh\"\n  A_t x;\n  a_t y;\nendmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "+incdir+inc\ntop.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn completion_in_files_that_include_each_other_ends() {
    // a.sv includes b.svh, which includes a.sv: the preprocessor refuses the loop but records
    // both includes; completion in either looks for the scope where it is included, a bounded
    // number of times.
    let (a, root) = project("inccycle", &[("a.sv", "`include \"b.svh\"\n\n"), ("b.svh", "`include \"a.sv\"\n\n")]);
    for f in ["a.sv", "b.svh"] {
        let id = a.vfs.lookup(&root.join(f)).unwrap();
        let _ = a.completions(id, 1 + "`include \"b.svh\"".len() as u32);
    }
}

#[test]
fn general_review_cases() {
    // Net type keywords and enum base types nest types: a long run of them ends with a problem.
    let run = || {
        let n = 100_000;
        let nets = format!("module m; {}x; endmodule\n", "wire ".repeat(n));
        let enums = format!("package p; typedef {}{{A}} t; endpackage\n", "enum ".repeat(n));
        let parse = |t: &str| dvi_syntax::parser::parse(&dvi_syntax::lexer::lex(t, 0), 0).diags.iter().filter(|d| d.message.contains("nested too deeply")).count();
        (parse(&nets), parse(&enums))
    };
    let r = std::thread::Builder::new().stack_size(16 << 20).spawn(run).unwrap().join().expect("no stack overflow");
    assert_eq!(r, (1, 1));
    // A formal repeated in a `"...`" string, given a long actual: a bounded string.
    let text = format!("`define S(x) `\"{}`\"\nmodule m;\n  string s = `S({});\nendmodule\n", "x ".repeat(100_000), "a".repeat(100_000));
    let (a, _) = project("bigstring", &[("t.sv", &text)]);
    let _ = problems(&a);
    // An explicit import wins over a wildcard one written before it.
    let src = "package a; int x; endpackage\npackage b; int x; endpackage\nmodule m;\n  import a::*;\n  import b::x;\n  initial x = 1;\nendmodule\n";
    let (a, root) = project("explicitimport", &[("t.sv", src)]);
    let f = a.vfs.lookup(&root.join("t.sv")).unwrap();
    let off = src.find("x = 1").unwrap() as u32;
    assert_eq!(a.goto_definition(f, off).iter().map(|l| l.range.start.line).collect::<Vec<_>>(), vec![1]);
    // `type T = int, U = byte`: U is a type too.
    let u = dvi_syntax::parser::parse(&dvi_syntax::lexer::lex("class C #(type T = int, U = byte, int N = 1);\n  U value;\nendclass\n", 0), 0);
    let kinds: Vec<_> = ["T", "U", "N"].iter().map(|n| u.decls.iter().find(|d| d.name == *n).map(|d| d.kind)).collect();
    use dvi_syntax::ast::DeclKind;
    assert_eq!(kinds, [Some(DeclKind::TypeParam), Some(DeclKind::TypeParam), Some(DeclKind::Parameter)]);
}

#[test]
fn deeply_nested_constraints_and_structs_end() {
    // On a stack the size of the engine's worker threads.
    let run = || {
        let n = 100_000;
        let c = format!("class c; rand int x; constraint k {{ {}x == 1; }} endclass\n", "if (1) ".repeat(n));
        let s = format!("package p; typedef {} int a; {} t; endpackage\n", "struct { ".repeat(n), "} f;".repeat(n));
        let parse = |t: &str| dvi_syntax::parser::parse(&dvi_syntax::lexer::lex(t, 0), 0).diags.iter().filter(|d| d.message.contains("nested too deeply")).count();
        (parse(&c), parse(&s))
    };
    let r = std::thread::Builder::new().stack_size(16 << 20).spawn(run).unwrap().join().expect("no stack overflow");
    assert_eq!(r, (1, 1));
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn a_deleted_file_named_with_other_case_is_forgotten() {
    // The file system cannot name a deleted file: the change event's spelling, never met
    // before, still reaches the file's cached text where case does not matter.
    let (a, root) = project("casedelete", &[("top.sv", "module top; endmodule\n")]);
    let id = a.vfs.lookup(&root.join("top.sv")).unwrap();
    assert!(dvi_syntax::SourceProvider::text(&*a.vfs, id).is_some());
    std::fs::remove_file(root.join("top.sv")).unwrap();
    assert_eq!(a.vfs.invalidate(&root.join("TOP.SV")), Some(id));
    assert!(dvi_syntax::SourceProvider::text(&*a.vfs, id).is_none());
}

#[test]
fn bounds_keep_real_include_chains_and_constraint_braces() {
    // A header 34 includes deep, with nothing written in it, is still in the module.
    let mut files = vec![("top.sv".to_string(), "module m;\n  int local_signal;\n`include \"h1.svh\"\nendmodule\n".to_string())];
    for k in 1..34 {
        files.push((format!("h{k}.svh"), format!("`include \"h{}.svh\"\n", k + 1)));
    }
    files.push(("h34.svh".to_string(), "// to do\n".to_string()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let (a, root) = project("deepinc", &refs);
    let f = a.vfs.lookup(&root.join("h34.svh")).unwrap();
    let names: Vec<String> = a.completions(f, 9).into_iter().map(|c| c.label).collect();
    assert!(names.iter().any(|n| n == "local_signal"), "{names:?}");
    // At the depth limit a constraint group is skipped whole: the class goes on after it.
    let c = format!("class c;\n  rand int x;\n  constraint k {{ {}{{ x == 1; }} x == 2; }}\n  int tail;\nendclass\n", "if (1) ".repeat(998));
    let u = dvi_syntax::parser::parse(&dvi_syntax::lexer::lex(&c, 0), 0);
    let errors: Vec<&str> = u.diags.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn macro_highlights_stay_in_their_file() {
    // The macro is defined in a header and used in two files: highlights in a.sv are a.sv's
    // uses and its `ifdef, references are all of them.
    let defs = "`define W 8\n";
    let a_sv = "`include \"defs.svh\"\nmodule a;\n  logic [`W-1:0] x;\n`ifdef W\n  logic [`W:0] y;\n`endif\nendmodule\n";
    let b_sv = "`include \"defs.svh\"\nmodule b;\n  logic [`W-1:0] z;\nendmodule\n";
    let files = [("defs.svh", defs), ("a.sv", a_sv), ("b.sv", b_sv)];
    let (a, root) = project("macrohighlight", &files);
    let fa = a.vfs.lookup(&root.join("a.sv")).unwrap();
    let off = (a_sv.find("`W-1").unwrap() + 1) as u32;
    let spans = a.document_highlights(fa, off);
    assert_eq!(spans.len(), 3, "{spans:?}");
    assert!(spans.iter().all(|s| s.file == fa), "{spans:?}");
    let refs = a.references(fa, off, true);
    assert_eq!(refs.len(), 5, "{refs:?}");
}

#[test]
fn completion_stops_at_its_item_limit() {
    let mut text = String::new();
    for i in 0..3500 {
        text.push_str(&format!("module cell_{i}(input logic a); endmodule\n"));
    }
    // In another file: the cells come from the project's definitions, not the scope.
    let top = "module top;\n  cel\nendmodule\n";
    let files = [("cells.sv", text.as_str()), ("top.sv", top)];
    let (a, root) = project("completionlimit", &files);
    let f = a.vfs.lookup(&root.join("top.sv")).unwrap();
    let off = (top.find("  cel\n").unwrap() + 5) as u32;
    let items = a.completions(f, off);
    assert_eq!(items.len(), 3000);
    assert!(items.iter().all(|c| c.label.starts_with("cel")), "{:?}", items.iter().take(5).map(|c| &c.label).collect::<Vec<_>>());
}

#[test]
fn completion_keeps_the_exact_member_past_the_limit() {
    let mut class = String::from("class c;\n");
    for i in 0..3000 {
        class.push_str(&format!("  int foo_{i};\n"));
    }
    class.push_str("  int foo;\nendclass\nmodule m;\n  c obj;\n  initial obj.foo\nendmodule\n");
    let files = [("c.sv", class.as_str())];
    let (a, root) = project("completionexact", &files);
    let f = a.vfs.lookup(&root.join("c.sv")).unwrap();
    let off = (class.find("obj.foo\n").unwrap() + 7) as u32;
    let names: Vec<String> = a.completions(f, off).into_iter().map(|c| c.label).collect();
    assert!(names.iter().any(|n| n == "foo"), "{}", names.len());
}

#[test]
fn library_cells_are_compiled_in_reference_order() {
    // E is named first, so E.sv is compiled first and its define reaches C.sv.
    let files = [
        ("top.sv", "module top;\n  E e();\n  C c();\nendmodule\n"),
        ("lib/E.v", "`define ENABLE_C\nmodule E; endmodule\n"),
        ("lib/C.v", "`ifdef ENABLE_C\nmodule C; endmodule\n`endif\n"),
    ];
    for name in ["liborder", "liborder2"] {
        let root = std::env::temp_dir().join(format!("dvicode-regress-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".dvicode")).unwrap();
        for (f, text) in &files {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
        }
        std::fs::write(root.join(".dvicode/default.build"), "-y lib\ntop.sv\n").unwrap();
        let cfg = dvi_project::load(&root, None, &FxHashMap::default());
        let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
        assert!(problems(&a).is_empty(), "{:?}", problems(&a));
    }
}

#[test]
fn macro_definitions_compare_as_their_signatures_and_bodies() {
    use dvi_syntax::preproc::{preprocess, MacroTable, PpConfig};
    let vfs = Vfs::new();
    let def = |n: usize, text: &str| {
        let id = vfs.set_overlay(std::path::Path::new(&format!("/m{n}.svh")), Arc::from(text));
        preprocess(&vfs, &PpConfig::default(), id, MacroTable::new()).macros_after.get("M").unwrap().clone()
    };
    let texts = [
        "`define M 1\n",
        "`define M 2\n",
        "`define M() 1\n",
        "`define M(a) a\n",
        "`define M(b) a\n",
        "`define M(a=1) a\n",
        "`define M(a=2) a\n",
        "`define M(a = 1) a\n",
        "`define M(a=1+2) a\n",
        "`define M(a=1 +2) a\n",
        "`define M(a=`X) a\n",
        "`define M(a=X) a\n",
        "`define M(a, b=c) a\n",
        "`define M(a, b) a\n",
        "`define M(a=) a\n",
    ];
    let defs: Vec<_> = texts.iter().enumerate().map(|(i, t)| def(i, t)).collect();
    for (i, a) in defs.iter().enumerate() {
        for (j, b) in defs.iter().enumerate() {
            let old = a.body_text == b.body_text && a.signature() == b.signature();
            assert_eq!(a.same_as(b), old, "{} vs {}", texts[i].trim(), texts[j].trim());
        }
    }
}

#[test]
fn an_edit_is_local_to_a_line_only_when_nothing_else_changed() {
    use dvi_analysis::ide::completion::line_local_edit;
    let old = "a;\n  x = 1;\nb;\n";
    // The line changed, the others not: where it starts, and both versions.
    assert_eq!(line_local_edit(old, "a;\n  x = 12;\nb;\n", 1), Some((3, "  x = 1;", "  x = 12;")));
    assert_eq!(line_local_edit(old, "a;\n  x = f(y);\nb;\n", 1), Some((3, "  x = 1;", "  x = f(y);")));
    // A `;` more or less may end a scope's statement on the line.
    assert_eq!(line_local_edit(old, "a;\n  x = pr\nb;\n", 1), None);
    assert_eq!(line_local_edit(old, "a;\n\nb;\n", 1), None);
    // The first and the last line.
    // (the first line has no statement before it to be sure of)
    assert_eq!(line_local_edit(old, "ab;\n  x = 1;\nb;\n", 0), None);
    // An `else` gone may end a loop's statement on the line; a statement begun with a macro
    // use on an earlier line (an alias of a call) may reach into it.
    assert_eq!(line_local_edit("a;\n  if (f) t(); else\n  t();\n", "a;\n  if (f) t(); loo\n  t();\n", 1), None);
    assert_eq!(line_local_edit("a;\n  `ALIAS(\n    KEEP) inn;\n", "a;\n  `ALIAS(\n    SWAP) inn;\n", 2), None);
    // A macro call closed before the line (`uvm_info over several lines, no `;`) is not open.
    assert!(line_local_edit("a;\n  `uvm_info(\"T\",\n    $sformatf(\"%0d\", n), UVM_LOW)\n  x = pr\n", "a;\n  `uvm_info(\"T\",\n    $sformatf(\"%0d\", n), UVM_LOW)\n  x = pro\n", 3).is_some());
    assert_eq!(line_local_edit("a;\nb;", "a;\nbc;", 1), Some((3, "b;", "bc;")));
    // Another line changed too, a line added or removed, a line that is not there.
    assert_eq!(line_local_edit(old, "A;\n  x = 12;\nb;\n", 1), None);
    assert_eq!(line_local_edit(old, "a;\n  x = 1;\n  y\nb;\n", 1), None);
    assert_eq!(line_local_edit(old, "a;\nb;\n", 1), None);
    assert_eq!(line_local_edit(old, old, 9), None);
    // CRLF: the line is without its end.
    assert_eq!(line_local_edit("a;\r\nx;\r\n", "a;\r\nxy;\r\n", 1), Some((4, "x;\r", "xy;\r")));
    // The line before continues into it: it is part of that line (`ifdef \).
    assert_eq!(line_local_edit("`ifdef \\\n  ENABLED\n", "`ifdef \\\n  DISABLED\n", 1), None);
    // A statement being typed, and one with a comment after it.
    assert!(line_local_edit("a;\n  x = pr\n", "a;\n  x = pro\n", 1).is_some());
    assert!(line_local_edit("a;\n  x = 1; // one\n", "a;\n  x = 2; // one\n", 1).is_some());
    // Lines that open or close a scope, or change how later text is read.
    for l in ["  begin", "  end", "  for (int i = 0; i < 2; i++)", "  x = \"a", "  /* x", "  y */", "  `define Q", "  x = 1; \\", "  endfunction", "  randsequence"] {
        assert_eq!(line_local_edit(old, &format!("a;\n{l}\nb;\n"), 1), None, "{l}");
    }
    // A keyword inside a longer name is not one.
    assert!(line_local_edit(old, "a;\n  x = begin_count;\nb;\n", 1).is_some());
}

#[test]
fn only_the_left_of_a_continuous_assignment_declares_a_net() {
    // IEEE 1800 6.10: an undeclared name on the left is an implicit net; on the right it is
    // a use of a name that must be declared, unless it is an implicit net elsewhere.
    let t = "module t(input logic a);\n  assign w = missing;\n  assign x = w, y = gone;\n  assign {p, q} = {a, x};\n  assign z = p & y;\nendmodule\n";
    let (a, _) = project("assignsides", &[("t.sv", t)]);
    let p = problems(&a);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(p.iter().any(|m| m.contains("'missing' is not declared")), "{p:?}");
    assert!(p.iter().any(|m| m.contains("'gone' is not declared")), "{p:?}");
}

#[test]
fn attributes_tags_and_casts_on_the_right_of_an_assignment_are_not_names() {
    let t = "module m(input logic a, b, output logic y, output int z);\n  typedef union tagged {\n    logic [7:0] Valid;\n    void Invalid;\n  } result_t;\n  result_t r;\n  assign y = a + (* use_dsp = \"no\" *) b;\n  assign r = tagged Valid 8'h42;\n  assign z = int'(a) + 1;\nendmodule\n";
    let (a, _) = project("assignattr", &[("m.sv", t)]);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn type_operators_event_controls_and_open_attributes_in_assignments() {
    let t = "module m(input logic a, b, output logic y, z, output logic q);
  assign y = type(a) == type(b);
  always_comb q = @(*) a;
  assign z = a + (* keep b;
  wire w;
  assign q = w;
endmodule
module n;
  m u();
endmodule
";
    let (a, _) = project("assigntype", &[("m.sv", t)]);
    let p = problems(&a);
    // The open attribute ends at its `;`: the rest of the module, and the next, are read.
    assert_eq!(p, ["m.sv: UNDECLARED_IDENTIFIER: 'keep' is not declared"]);
}

#[test]
fn a_closed_attribute_read_again_after_an_open_one_is_still_skipped() {
    // The parameters of `t` are read again for `v`, after the open attribute in `u`.
    let t = "module t #(int P = 0) (input logic x);
endmodule
module m(input logic a);
  t #(.P(1 + (* keep=1 *) a))
    u(.x(a + (* broken)), v(.x(a));
endmodule
";
    let (a, _) = project("attrrescan", &[("m.sv", t)]);
    let p = problems(&a);
    assert!(!p.iter().any(|m| m.contains("'keep'")), "{p:?}");
}

#[test]
fn enum_constants_of_a_file_of_declarations_are_seen_by_the_files_after_it() {
    // A header listed as a source declares the enum outside any scope: its constants are
    // names of the compilation unit, like its typedef.
    let root = std::env::temp_dir().join(format!("dvicode-regress-unitenum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    std::fs::write(root.join("defs.svh"), "parameter W = 3;\ntypedef enum logic [W-1:0] { S_IDLE = 0, S_RUN = 1 } st_e;\n").unwrap();
    std::fs::write(root.join("m.sv"), "module m;\n  st_e s;\n  logic busy;\n  always_comb case (s) S_IDLE: ; default: ; endcase\n  assign busy = (s == S_RUN);\nendmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "defs.svh\nm.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn foreach_in_the_constraint_set_of_an_implication_declares_its_loop_variable() {
    let t = "class s_cfg; rand bit tiny; endclass\nclass cfg;\n  rand s_cfg surf[4];\n  rand bit mode;\n  extern constraint a_c;\n  constraint b_c { mode -> { foreach (surf[j]) { surf[j].tiny == 1'b1; } } }\n  constraint c_c { mode -> mode -> { foreach (surf[k]) surf[k].tiny == 1'b0; } }\nendclass\nconstraint cfg::a_c {\n  mode -> {\n    foreach (surf[i]) {\n      surf[i].tiny == 1'b1;\n    }\n  }\n}\n";
    let (a, _) = project("implforeach", &[("c.sv", t)]);
    assert!(problems(&a).is_empty(), "{:?}", problems(&a));
}

#[test]
fn implicit_nets_are_only_the_names_assigned_in_their_own_generate_block() {
    // `idx` selects on the left: a use, not a net. `w` is implicit in `g` only.
    let t = "module m(input logic a);\n  logic [3:0] v;\n  assign v[idx] = a;\n  if (1) begin : g\n    assign w = a;\n    assign x = w;\n  end\n  assign y = w;\n  assign top_net = a;\n  assign z = top_net;\nendmodule\n";
    let (a, _) = project("implicitscope", &[("m.sv", t)]);
    let mut p = problems(&a);
    p.sort();
    assert_eq!(p, ["m.sv: UNDECLARED_IDENTIFIER: 'idx' is not declared", "m.sv: UNDECLARED_IDENTIFIER: 'w' is not declared"], "{p:?}");
}

#[test]
fn index_names_in_port_connections_and_gate_terminals_are_uses() {
    let t = "module c(input logic x);\nendmodule\nmodule m;\n  logic [3:0] v;\n  c u(.x(v[idx]));\n  c w(v[jdx]);\n  and g(out_net, v[kdx], v[0]);\n  c z(.x(implicit_net));\nendmodule\n// A bind's names are the target's: not checked here.\nbind m c b_u(.x(v[target_idx]));\n";
    let (a, _) = project("portselects", &[("m.sv", t)]);
    let mut p = problems(&a);
    p.sort();
    assert_eq!(p, ["m.sv: UNDECLARED_IDENTIFIER: 'idx' is not declared", "m.sv: UNDECLARED_IDENTIFIER: 'jdx' is not declared", "m.sv: UNDECLARED_IDENTIFIER: 'kdx' is not declared"], "{p:?}");
}

#[test]
fn connection_selects_with_macros_iterators_parameters_bind_and_defparam() {
    let t = concat!(
        "`define CONN(n) {n, v[0]}\n",
        "module c #(parameter int P = 0) (input [1:0] p);\nendmodule\n",
        "module t;\nendmodule\n",
        "module m;\n",
        "  wire [3:0] v;\n",
        "  int q[2];\n",
        // A macro's brackets are not around its argument: an implicit net.
        "  c u1(.p(`CONN(implicit_a)));\n",
        // An array method's iterator.
        "  c u2(.p(v[q.sum(it) with (it)]));\n",
        // A parameter value declares nothing: both names are uses.
        "  c #(.P(px[pi])) u3();\n",
        // Names of a bind or defparam do not excuse the same names here.
        "  defparam u3.P = dp_only;\n",
        "  c u4(.p(v[dp_only]));\n",
        "endmodule\n",
        "bind m c b(.p(bind_only));\n",
        "module n;\n  wire [3:0] v;\n  c u5(.p(v[bind_only]));\nendmodule\n",
    );
    let (a, _) = project("selects2", &[("m.sv", t)]);
    let mut p = problems(&a);
    p.sort();
    assert_eq!(
        p,
        [
            "m.sv: UNDECLARED_IDENTIFIER: 'bind_only' is not declared",
            "m.sv: UNDECLARED_IDENTIFIER: 'dp_only' is not declared",
            "m.sv: UNDECLARED_IDENTIFIER: 'pi' is not declared",
            "m.sv: UNDECLARED_IDENTIFIER: 'px' is not declared",
        ],
        "{p:?}"
    );
}

#[test]
fn inlay_hints_name_positional_ports_parameters_and_arguments() {
    use dvi_analysis::ide::hints::HintKinds;
    let t = concat!(
        "module c #(parameter int W = 1, parameter int D = 2) (input logic clk, input logic [W-1:0] din, output logic dout);\nendmodule\n",
        "module top;\n",
        "  logic clk, x, y, z;\n",
        "  c #(8, 4) u1 (clk, x, y), u2 (.clk(clk), .din(x), .dout(z));\n",
        "  function automatic int add(int a, int b); return a + b; endfunction\n",
        "  initial begin z = add(1, x); z = add(a_val(), b); end\n",
        "  function automatic int a_val(); return 0; endfunction\n",
        "  int b;\n",
        "endmodule\n",
    );
    let (a, root) = project("hints", &[("top.sv", t)]);
    let f = a.vfs.lookup(&root.join("top.sv")).unwrap();
    let show = |kinds: HintKinds| -> Vec<String> {
        a.inlay_hints(f, 0, t.len() as u32, kinds).into_iter().map(|h| format!("{}{}", h.label, &t[h.offset as usize..].split(|c: char| c == ',' || c == ')').next().unwrap())).collect()
    };
    assert_eq!(show(HintKinds::default()), ["W:8", "D:4", "din:x", "dout:y", "a:1", "b:x", "a:a_val("], "clk is not hinted: it is the port's own name; b neither");
    assert_eq!(show(HintKinds { ports: false, parameters: true, arguments: false }), ["W:8", "D:4"]);
    // Only the part asked for.
    let line4 = t.find("  c #(8").unwrap() as u32;
    assert_eq!(a.inlay_hints(f, line4, line4 + 12, HintKinds::default()).len(), 2);
}

#[test]
fn inherited_methods_are_offered_with_their_signatures_and_pure_ones_implemented() {
    let t = concat!(
        "virtual class base;\n",
        "  pure virtual function int size();\n",
        "  virtual function void build(int n); n = n + 1; endfunction\n",
        "  virtual task run(); endtask\n",
        "  extern virtual function void report(string s);\n",
        "  function new(string name = \"\"); endfunction\n",
        "  local virtual function void hidden(); endfunction\n",
        "  static function void util(); endfunction\n",
        "endclass\n",
        "function void base::report(string s); $display(s); endfunction\n",
        "interface class ic;\n  pure virtual function bit check(int x);\nendclass\n",
        "class child extends base implements ic;\n",
        "  int k;\n",
        "  bu\n",
        "endclass\n",
    );
    let (a, root) = project("overrides", &[("c.sv", t)]);
    let f = a.vfs.lookup(&root.join("c.sv")).unwrap();
    let at = t.find("  bu\n").unwrap() as u32 + 4;
    let items = a.completions(f, at);
    let build = items.iter().find(|i| i.label == "build" && i.sort_text.as_deref() == Some("0build")).expect("override item");
    assert_eq!(build.insert_text.as_deref(), Some("virtual function void build(int n);\n\tsuper.build(n);\n\t$0\nendfunction"));
    // All that may be overridden, nearest first; not local or static ones.
    let class = {
        let u = a.unit(a.units_for(f)[0]);
        let d = u.parsed.decls.iter().position(|d| d.name == "child").unwrap();
        dvi_analysis::DeclRef { unit: u.id, decl: d as u32 }
    };
    let names: Vec<(String, bool)> = a.overridable_methods(class).iter().map(|o| (a.decl(o.method).name.to_string(), o.pure)).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(sorted, [("build".into(), false), ("check".into(), true), ("new".into(), false), ("report".into(), false), ("run".into(), false), ("size".into(), true)], "{names:?}");
    let text = |n: &str| a.override_text(&a.overridable_methods(class).into_iter().find(|o| a.decl(o.method).name == n).unwrap(), class, "  ", "
", false).unwrap();
    assert_eq!(text("run"), "virtual task run();\nendtask", "an empty base body: no super call");
    assert_eq!(text("report"), "virtual function void report(string s);\n  super.report(s);\nendfunction", "the body outside the class counts");
    assert_eq!(text("new"), "function new(string name = \"\");\n  super.new(name);\nendfunction");
    assert_eq!(text("size"), "virtual function int size();\nendfunction");
    // The action implements the two pure ones before `endclass`.
    let r = a.implement_pure_methods(f, at).expect("action");
    assert!(r.title.contains("2 pure virtual methods"), "{}", r.title);
    let (span, ins) = &r.edits[0];
    assert_eq!(span.start as usize, t.rfind("endclass").unwrap());
    assert!(ins.contains("  virtual function int size();\n  endfunction\n") && ins.contains("  virtual function bit check(int x);\n  endfunction\n"), "{ins}");
}

#[test]
fn inlay_hints_stay_off_where_the_binding_is_uncertain() {
    use dvi_analysis::ide::hints::HintKinds;
    let t = concat!(
        "`define PAIR 10, 20\n",
        "module p #(parameter int A = 1, localparam type L = int, parameter int B = 2) (input logic x);\nendmodule\n",
        "module q ({a, b}, d);\n  input a, b, d;\nendmodule\n",
        "import \"DPI-C\" function int cf(input int, input int count);\n",
        "class base;\n  function new(int size, int depth); endfunction\nendclass\n",
        "class kid extends base;\n  function new(); super.new(1, 2); endfunction\nendclass\n",
        "module top;\n",
        "  logic x, y, DATA;\n",
        "  function automatic int f(int a, int b, int c); return 0; endfunction\n",
        "  function automatic int g(int data, int b); return 0; endfunction\n",
        "  int r;\n",
        "  p #(3, 4) u1 (x);\n",
        "  q u2 (x, y);\n",
        "  p u3 (.*, y);\n",
        "  initial begin\n",
        "    r = f(`PAIR, 30);\n",
        "    r = cf(1, 2);\n",
        "    r = g(DATA, (* keep = 1, mark = 2 *) y);\n",
        "  end\n",
        "endmodule\n",
    );
    let (a, root) = project("hints2", &[("top.sv", t)]);
    let f = a.vfs.lookup(&root.join("top.sv")).unwrap();
    let got: Vec<String> = a
        .inlay_hints(f, 0, t.len() as u32, HintKinds::default())
        .into_iter()
        .map(|h| format!("{}{}", h.label, &t[h.offset as usize..].split([',', ')', ';']).next().unwrap()))
        .collect();
    // `A`, `B` (not the local type); nothing for q's concatenated port, the mixed list or the
    // macro; `count` but no empty label; `data` for `DATA` (names are case-sensitive), `b` past
    // the attribute; `size` and `depth` for `super.new`.
    assert_eq!(got, ["size:1", "depth:2", "A:3", "B:4", "count:2", "data:DATA", "b:(* keep = 1"], "{got:?}");
}

#[test]
fn overrides_follow_the_class_chain_and_its_parameters() {
    let t = concat!(
        "virtual class a_c;\n",
        "  pure virtual function void f();\n",
        "  virtual function string g(string s = \" void \"); return s; endfunction\n",
        "  function new(int n = 0); endfunction\n",
        "endclass\n",
        // `f` implemented without repeating `virtual`; no constructor of its own.
        "class b_c extends a_c;\n  function void f(); $display(1); endfunction\nendclass\n",
        "interface class i_c;\n  pure virtual function void f();\n  pure virtual function void h();\nendclass\n",
        "class c_c extends b_c implements i_c;\n  int k;\nendclass\n",
        "virtual class sub #(type T = int);\n  pure virtual function void write(T t);\nendclass\n",
        "class item; endclass\n",
        "class cov extends sub #(item); endclass\n",
    );
    let (a, _) = project("overrides2", &[("c.sv", t)]);
    let class = |name: &str| {
        let u = a.unit(a.units_for(a.project_files()[0])[0]);
        let d = u.parsed.decls.iter().position(|d| d.name == name).unwrap();
        dvi_analysis::DeclRef { unit: u.id, decl: d as u32 }
    };
    let c = class("c_c");
    let os = a.overridable_methods(c);
    let get = |n: &str| os.iter().find(|o| o.name == n).unwrap().clone();
    // `f` is b_c's (still virtual, not pure); the interface's `f` is defined by b_c; `h` is not.
    assert!(!get("f").pure && a.decl(get("f").owner).name == "b_c");
    assert!(get("h").pure);
    let text = |o: &dvi_analysis::ide::overrides::Overridable| a.override_text(o, c, "  ", "\n", false).unwrap();
    assert_eq!(text(&get("f")), "function void f();\n  super.f();\nendfunction");
    assert_eq!(text(&get("g")), "virtual function string g(string s = \" void \");\n  return super.g(s);\nendfunction", "returns a value");
    // b_c has no constructor: an implicit one, not a_c's.
    assert_eq!(text(&get("new")), "function new();\n  super.new();\nendfunction");
    let r = a.implement_pure_methods(a.project_files()[0], t.find("  int k;").unwrap() as u32).unwrap();
    assert_eq!(r.title, "Implement pure virtual method h");
    // A type parameter as the derived class binds it; a one-line class gets the definition.
    let cov = class("cov");
    let w = a.overridable_methods(cov).into_iter().find(|o| a.decl(o.method).name == "write").unwrap();
    assert_eq!(text(&w), "virtual function void write(item t);\nendfunction");
    let r = a.implement_pure_methods(a.project_files()[0], t.find("class cov").unwrap() as u32 + 3).unwrap();
    let (span, ins) = &r.edits[0];
    assert_eq!(span.start as usize, t.rfind("endclass").unwrap());
    assert!(ins.starts_with('\n') && ins.contains("virtual function void write(item t);"), "{ins:?}");
}

const RAL_MODEL: &str = r#"
virtual class uvm_reg_block; endclass
virtual class uvm_reg; endclass
class uvm_reg_field; endclass
virtual class uvm_mem; endclass
class uvm_reg_map; endclass
package cfg_pkg; parameter int SUB_BASE = 'h1000; endpackage
`define MEM_BASE (`HALF_MEM_BASE * 2)
`define HALF_MEM_BASE 'h200
`define ADDR_W 64
`define report(a, b) $display(a, b);
class ctrl_reg extends uvm_reg;
  rand uvm_reg_field en, mode;
  function new(string name = "ctrl_reg", int unsigned n_bits = 32); super.new(name, n_bits, 0); endfunction
  virtual function void build();
    en = uvm_reg_field::type_id::create("en");
    en.configure(this, 1, 0, "RW", 0, 1'b1, 1, 1, 0);
    mode = uvm_reg_field::type_id::create("mode");
    mode.configure(.parent(this), .size(3), .lsb_pos(4), .access("RO"), .volatile(1), .reset('1), .has_reset(1), .is_rand(0), .individually_accessible(0));
  endfunction
endclass
class wide_reg extends uvm_reg;
  function new(string name = "wide_reg"); super.new(name, 64, 0); endfunction
endclass
class buf_mem extends uvm_mem;
  function new(string name = "buf_mem"); super.new(name, 256, 32, "RW", 0); endfunction
endclass
class sub_block extends uvm_reg_block;
  rand ctrl_reg c;
  function new(string name = "sub_block"); super.new(name); endfunction
  virtual function void build();
    default_map = create_map("sub_map", 'h40, 4, UVM_LITTLE_ENDIAN);
    c = ctrl_reg::type_id::create("c",, get_full_name());
    c.configure(this, null, "");
    c.build();
    default_map.add_reg(c, 'h8, "RW");
  endfunction
endclass
class wide_block extends uvm_reg_block;
  rand wide_reg x;
  function new(string name = "wide_block"); super.new(name); endfunction
  virtual function void build();
    default_map = create_map("wide_map", 0, 8, UVM_LITTLE_ENDIAN, 0);
    x = new("x");
    default_map.add_reg(x, 'h2);
  endfunction
endclass
class top_block extends uvm_reg_block;
  rand ctrl_reg r[4];
  rand wide_block wb;
  rand wide_reg w;
  rand buf_mem mem;
  rand sub_block sub;
  uvm_reg_map word_map;
  function new(string name = "top_block"); super.new(name); endfunction
  virtual function void build();
    if (!$test$plusargs("quiet")) begin
      `report("top_block", "building")
    end
    this.default_map = create_map(.name("bus"), .base_addr('h1_0000), .n_bytes(4), .endian(UVM_LITTLE_ENDIAN));
    word_map = create_map("words", 0, 4, UVM_LITTLE_ENDIAN, 0);
    for (int i = 0; i < 4; i++) begin
      r[i] = (ctrl_reg::
              type_id::create($sformatf("r%0d", i)));
      r[i].configure(this);
      r[i].build();
      default_map.add_reg(r[i], i * 4, "RW");
    end
    w = new("w");
    default_map.add_reg(.rg(w), .offset(`ADDR_W'h20));
    word_map.add_reg(w, 'h8);
    mem = buf_mem::type_id::create("mem");
    mem.configure(this);
    default_map.add_mem(mem, `MEM_BASE, "RW");
    sub = sub_block::type_id::create("sub");
    sub.configure(this);
    sub.build();
    default_map.add_submap(sub.default_map, cfg_pkg::SUB_BASE);
    wb = wide_block::type_id::create("wb");
    wb.configure(this);
    wb.build();
    default_map.add_submap(wb.default_map, 'h2000);
    if (0) default_map.add_reg(r[0], 'h30);
    lock_model();
    default_map.add_reg(w, 'h80);
  endfunction
endclass
"#;

#[test]
fn a_register_model_is_built_as_its_construction_code_says() {
    let (a, _) = project("ral", &[("ral.sv", RAL_MODEL)]);
    let (blocks, tops) = a.register_blocks();
    let name = |d: &dvi_analysis::DeclRef| a.decl(*d).name.to_string();
    assert_eq!(blocks.iter().map(name).collect::<Vec<_>>(), ["sub_block", "top_block", "wide_block"]);
    assert_eq!(tops.iter().map(name).collect::<Vec<_>>(), ["top_block"]);
    let model = a.register_model(tops[0]);
    assert_eq!(model.cut, None);
    let (tree, truncated) = a.register_tree(&model, 1000);
    assert!(!truncated);
    let show = |n: &dvi_analysis::ide::registers::RegNode| -> Vec<String> {
        n.children.iter().map(|c| format!("{} {} {}{}", c.kind, c.name, c.address.clone().unwrap_or_default(), if c.notes.is_empty() { String::new() } else { format!(" ({})", c.notes.join("; ")) })).collect()
    };
    let bus = &tree.children[0];
    assert_eq!(bus.name, "bus");
    assert_eq!(
        show(bus),
        [
            "register r[0] 0x10000",
            "register r[1] 0x10004",
            "register r[2] 0x10008",
            "register r[3] 0x1000c",
            "register w 0x10020–0x10024",
            "register r[0] 0x10030 (added under a condition)",
            "memory mem 0x10400–0x107fc",
            "block sub 0x11000",
            "block wb 0x12000 (word addressing)",
        ],
        "after lock_model() nothing is added; the submap's own base ('h40) is not used"
    );
    assert_eq!(show(&bus.children[7]), ["register c 0x11008"]);
    // UVM 1.2: an 8-byte map's address 2 is 2 words of the 4-byte parent map, each taking 4
    // bytes there; the 64-bit register then takes two parent words.
    assert_eq!(show(&bus.children[8]), ["register x 0x12004–0x12008"]);
    let words = &tree.children[1];
    assert_eq!(show(words), ["register w 0x8–0x9 (word addressing)"].map(|s| s.replace(" (word addressing)", "")), "word addressing: one step per bus word");
    assert!(words.notes.iter().any(|n| n == "word addressing"));
    // Fields on demand.
    let r0 = bus.children[0].register.unwrap();
    let f: Vec<String> = a.register_fields(&model, r0).iter().map(|f| format!("{} {:?}+{:?} {:?} {:?}", f.name, f.lsb, f.size, f.access, f.reset)).collect();
    assert_eq!(f, ["en Some(0)+Some(1) Some(\"RW\") Some(\"0x1\")", "mode Some(4)+Some(3) Some(\"RO\") Some(\"0x7\")"]);
    // Address search.
    let hits = |x: u128| a.registers_at(&model, x).into_iter().map(|h| format!("{} {}", h.path.join("."), h.address)).collect::<Vec<_>>();
    assert_eq!(hits(0x11008), ["top_block.bus.sub.c 0x11008"]);
    assert_eq!(hits(0x10024), ["top_block.bus.w 0x10020–0x10024"]);
    assert_eq!(hits(0x12008), ["top_block.bus.wb.x 0x12004–0x12008"]);
    assert_eq!(hits(0x9), ["top_block.words.w 0x8–0x9"]);
    assert!(hits(0x10010).is_empty());
}

const RAL_EDGES: &str = r#"
virtual class uvm_reg_block; endclass
virtual class uvm_reg; endclass
class uvm_reg_field; endclass
class uvm_reg_map; endclass
package loop_pkg; parameter int A = loop_pkg::A; endpackage
class r32 extends uvm_reg;
  function new(string name = "r32"); super.new(name, 32, 0); endfunction
endclass
class r64 extends uvm_reg;
  function new(string name = "r64"); super.new(name, 64, 0); endfunction
endclass
class helper_reg extends uvm_reg;
  uvm_reg_field f, g;
  function new(string name = "helper_reg"); super.new(name, 32, 0); endfunction
  function void make_f(); f = uvm_reg_field::type_id::create("f"); endfunction
  virtual function void build(string access = "RO");
    make_f();
    f.configure(this, 4, 0, access, 0, 0, 1, 0, 0);
    g = uvm_reg_field::type_id::create("g");
    g.configure(this, 2, 'hffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff, "RW", 0, 0, 1, 0, 0);
  endfunction
endclass
class two_maps extends uvm_reg_block;
  rand r32 x;
  uvm_reg_map map_a, map_b;
  function new(string name = "two_maps"); super.new(name); endfunction
  virtual function void build();
    map_a = create_map("map_a", 0, 4, UVM_LITTLE_ENDIAN);
    map_b = create_map("map_b", 0, 4, UVM_LITTLE_ENDIAN);
    x = r32::type_id::create("x");
    map_a.add_reg(x, 'h4);
    map_b.add_reg(x, 'h8);
  endfunction
endclass
class lone extends uvm_reg_block;
  rand r32 y;
  function new(string name = "lone"); super.new(name); endfunction
  virtual function void build();
    default_map = create_map("lone_map", 'h9000, 4, UVM_LITTLE_ENDIAN);
    y = r32::type_id::create("y");
    default_map.add_reg(y, 0);
  endfunction
endclass
class edge_block extends uvm_reg_block;
  rand r32 arr[4:7];
  rand r32 odd[2], odd2[4], arr2[2:3];
  rand r64 wide, packed_w, w2;
  rand helper_reg h, unbuilt;
  rand two_maps tm;
  rand lone ln;
  uvm_reg_map m, fifo_map, fifo2;
  function new(string name = "edge_block"); super.new(name); endfunction
  function void scratch();
    uvm_reg_map m = create_map("scratch", 'h7000, 4, UVM_LITTLE_ENDIAN);
  endfunction
  function void scratch2();
    uvm_reg_map m;
    m = create_map("scratch2", 'h7100, 4, UVM_LITTLE_ENDIAN);
  endfunction
  virtual function void build();
    m = create_map("bus", 'h1000, 4, UVM_LITTLE_ENDIAN);
    default_map = m;
    fifo_map = create_map("fifo", 'h2000, 4, UVM_LITTLE_FIFO);
    fifo2 = create_map("maybe_fifo", 'h3000, 4, USE_FIFO ? UVM_LITTLE_FIFO : UVM_LITTLE_ENDIAN);
    scratch();
    scratch2();
    m.add_submap(m, 'h0);
    foreach (this.arr2[i]) begin
      arr2[i] = r32::type_id::create("arr2");
      m.add_reg(arr2[i], 'h60 + i * 4);
    end
    for (int i = 4; i > 0; i = i - 2 + 1) begin
      odd2[i] = r32::type_id::create("odd2");
      m.add_reg(odd2[i], 'h50);
    end
    w2 = new("w2");
    fifo2.add_reg(w2, 'h20);
    foreach (arr[i]) begin
      arr[i] = r32::type_id::create($sformatf("arr%0d", i));
      m.add_reg(arr[i], i * 4);
    end
    for (int i = 0; i * 2 < 4; i++) begin
      odd[i] = r32::type_id::create("odd");
      m.add_reg(odd[i], 'h40);
    end
    wide = new("wide");
    m.add_reg(wide, 'h80);
    packed_w = new("packed_w");
    fifo_map.add_reg(packed_w, 'h20);
    h = helper_reg::type_id::create("h");
    h.build();
    m.add_reg(h, 'h90);
    unbuilt = helper_reg::type_id::create("unbuilt");
    m.add_reg(unbuilt, 'h94);
    tm = two_maps::type_id::create("tm");
    tm.build();
    m.add_submap(tm.map_a, 'h100);
    m.add_submap(tm.map_b, 'h200);
    ln = lone::type_id::create("ln");
    ln.build();
    m.add_reg(arr[4], loop_pkg::A);
    arr[4] = ::type_id::create("r");
    for (int a = 0; a < 4096; a++) for (int b = 0; b < 4096; b++) for (int c = 0; c < 4096; c++) for (int d = 0; d < 4096; d++) begin end
  endfunction
endclass
"#;

#[test]
fn register_model_edge_cases_are_bounded_and_follow_uvm() {
    let (a, _) = project("raledges", &[("ral.sv", RAL_EDGES)]);
    let (blocks, _) = a.register_blocks();
    let top = *blocks.iter().find(|b| a.decl(**b).name == "edge_block").unwrap();
    let t0 = std::time::Instant::now();
    let model = a.register_model(top);
    assert!(t0.elapsed().as_secs() < 20, "nested empty loops are charged: {:?}", t0.elapsed());
    assert_eq!(model.cut, Some("work"), "four nested loops of 4096 stop at the work limit");
    let (tree, _) = a.register_tree(&model, 1000);
    let show = |n: &dvi_analysis::ide::registers::RegNode| -> Vec<String> { n.children.iter().map(|c| format!("{} {}", c.name, c.address.clone().unwrap_or_default())).collect() };
    let maps: Vec<&str> = tree.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(maps, ["bus", "fifo", "maybe_fifo", "scratch", "scratch2"], "the helpers' local maps are their own, not the member `m`");
    let bus = &tree.children[0];
    assert_eq!(
        show(bus),
        [
            "arr[4] 0x1010",
            "arr[5] 0x1014",
            "arr[6] 0x1018",
            "arr[7] 0x101c",
            "odd[*] 0x1040",
            "odd2[*] 0x1050",
            "arr2 0x1068",
            "arr2 0x106c",
            "wide 0x1080–0x1084",
            "h 0x1090",
            "unbuilt 0x1094",
            "tm (map_a) 0x1100",
            "tm (map_b) 0x1200",
            "arr[4] ",
        ],
        "foreach follows [4:7]; `i * 2 < 4` is not unrolled; a package constant defined by itself is not constant"
    );
    assert_eq!(show(&tree.children[1]), ["packed_w 0x2020"], "a FIFO map puts every word at the same address");
    assert_eq!(show(&tree.children[2]), ["w2 0x3020–0x3024"], "an endianness that is not a FIFO name is not FIFO");
    // Fields: a helper's member survives; a string argument's value; unbuilt: none; no overflow.
    let reg = |name: &str| bus.children.iter().find(|c| c.name == name).unwrap().register.unwrap();
    let f: Vec<String> = a.register_fields(&model, reg("h")).iter().map(|f| format!("{} {:?} {:?}", f.name, f.access, f.notes)).collect();
    assert_eq!(f, ["f Some(\"RO\") []", "g Some(\"RW\") [\"beyond the register's width\"]"]);
    assert!(a.register_fields(&model, reg("unbuilt")).is_empty(), "build() never called: no fields");
    // Address search: exact words, a byte inside a word, and only blocks the view shows.
    let hits = |x: u128| a.registers_at(&model, x).into_iter().map(|h| format!("{} {}{}", h.path.join("."), h.address, h.note.map(|n| format!(" ({n})")).unwrap_or_default())).collect::<Vec<_>>();
    assert_eq!(hits(0x1084), ["edge_block.bus.wide 0x1080–0x1084"]);
    assert_eq!(hits(0x1081), ["edge_block.bus.wide 0x1080–0x1084 (byte 1 of the word at 0x1080)"]);
    assert_eq!(hits(0x1208), ["edge_block.bus.tm (map_b).x 0x1208"]);
    assert!(hits(0x9000).is_empty(), "a block whose map is not mounted is not in the view");
    // The node limit keeps a revealed path and marks what it cut.
    let (mut t, _) = a.register_tree(&model, usize::MAX);
    let cut = dvi_analysis::ide::registers::limit_tree(&mut t, 2, &["bus".to_string(), "unbuilt".to_string()]);
    assert!(cut);
    let bus = &t.children[0];
    assert_eq!(bus.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["arr[4]", "unbuilt"]);
    assert!(bus.truncated && t.truncated);
}

const CHECKS_SRC: &str = r#"`define CHECK(name, prop) name: assert property (@(posedge clk) prop);
module chk(input logic clk, rst_n, req, ack);
  property p_handshake;
    @(posedge clk) disable iff (!rst_n) req |-> ##[1:3] ack;
  endproperty
  a_handshake: assert property (p_handshake);
  c_req: cover property (@(posedge clk) req);
endmodule

module dut(input logic clk, rst_n, input logic req, output logic ack, input logic [7:0] data);
  logic busy;
  logic [3:0] bus;
  logic [1:0] idx;
  logic dbg;
  default clocking cb @(posedge clk); endclocking
  default disable iff (!rst_n);
  a_busy: assert property (busy |-> !req) else $error("req while busy %0d", req, dbg);
  assert property (@(posedge clk) req |=> busy);
  always_comb begin
    a_imm: assert (!(req && ack)) else $display("req %0d", req);
  end
  initial assert final (data != 0);
  `CHECK(a_mac, req |-> ##1 ack)
  covergroup cg_data @(posedge clk);
    cp_data: coverpoint data iff (rst_n) {
      bins low = {[0:127]};
      bins high = {[128:255]};
      option.weight = req;
    }
    coverpoint req;
    x_req_data: cross cp_data, req iff (!busy);
  endgroup
  sequence s_req;
    req ##1 ack;
  endsequence
  property p_unused;
    @(posedge clk) req;
  endproperty
  chk u_chk (.clk, .rst_n(rst_n), .req(req), .ack(ack));
  chk u_pos (clk, rst_n, req, ack);
  chk u_star (.*);
  chk u_expr (.clk(clk), .rst_n(rst_n), .req(req & busy), .ack(ack));
  chk u_idx (.clk(clk), .rst_n(rst_n), .req(bus[idx]), .ack(ack));
endmodule

module top;
  bind dut chk u_bound (.clk(clk), .rst_n(rst_n), .req(req), .ack(ack));
endmodule

class tx;
  rand bit [7:0] addr;
endclass

class cov;
  tx tr;
  covergroup cg_tr;
    cp_addr: coverpoint tr.addr;
  endgroup
  covergroup cg_arg with function sample(bit [7:0] a);
    cp_a: coverpoint a;
  endgroup
  function new();
    cg_tr = new();
    cg_arg = new();
  endfunction
  function void sample_it();
    cg_arg.sample(tr.addr);
  endfunction
endclass
"#;

#[test]
fn assertions_and_coverage_of_a_signal_are_found_through_helpers_ports_and_arguments() {
    let (a, root) = project("checksof", &[("a.sv", CHECKS_SRC)]);
    let f = a.vfs.lookup(&root.join("a.sv")).unwrap();
    // The first `needle` after the first `after`.
    let at = |after: &str, needle: &str| -> u32 {
        let base = CHECKS_SRC.find(after).unwrap();
        (base + CHECKS_SRC[base..].find(needle).unwrap()) as u32
    };
    fn render(h: &dvi_analysis::ide::checks::CheckHit, depth: usize, out: &mut Vec<String>) {
        let label = h.label.clone().unwrap_or_else(|| format!("line {}", h.line.unwrap_or(0) + 1));
        let notes = if h.notes.is_empty() { String::new() } else { format!(" ({})", h.notes.join("; ")) };
        out.push(format!("{}{} {} [{}] {}{}", "  ".repeat(depth), h.group, h.kind, h.roles.join(","), label, notes));
        for c in &h.children {
            render(c, depth + 1, out);
        }
    }
    let show = |off: u32| -> Vec<String> {
        let r = a.checks_at(f, off).expect("a searchable declaration");
        assert!(r.limits.is_empty(), "{:?}", r.limits);
        let mut out = Vec::new();
        for h in &r.hits {
            render(h, 0, &mut out);
        }
        out
    };
    let dut = "module dut(";
    let port_chain = |port: &str, role: &str| -> Vec<String> {
        ["u_chk", "u_pos", "u_star (.*)", "u_expr (connected in an expression)", "u_bound (bind)"]
            .iter()
            .flat_map(|u| [format!("connections port [connection] {port} of {u}"), format!("  covers cover [{role}] c_req"), format!("  helpers property [{role}] p_handshake"), format!("    checks assert [checked] a_handshake")])
            .collect()
    };
    let mut req = vec![
        "checks assert [checked] a_busy",
        "checks assert [checked] line 18",
        "checks assert [checked] a_imm",
        "checks assert [checked] a_mac",
        "coverage coverpoint [checked] coverpoint of cg_data",
        "coverage cross [cross item] x_req_data",
        "helpers sequence [checked] s_req (declaration only: no assertion uses it)",
        "helpers property [checked] p_unused (declaration only: no assertion uses it)",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    req.extend(port_chain("req", "checked"));
    assert_eq!(show(at(dut, "req")), req, "the action block's `req` and `option.weight = req` are not uses; `bus[idx]` is no connection of req");
    let top = |off: u32| show(off).into_iter().filter(|l| !l.starts_with(' ')).collect::<Vec<_>>();
    assert_eq!(
        top(at(dut, "rst_n")),
        [
            "checks default disable [reset] line 16 (applies to 3 concurrent assertions without their own disable iff)",
            "coverage coverpoint [gate] cp_data",
            "connections port [connection] rst_n of u_chk",
            "connections port [connection] rst_n of u_pos",
            "connections port [connection] rst_n of u_star (.*)",
            "connections port [connection] rst_n of u_expr",
            "connections port [connection] rst_n of u_idx",
            "connections port [connection] rst_n of u_bound (bind)",
        ]
    );
    assert_eq!(
        top(at(dut, "clk"))[..5],
        [
            "checks default clocking [clock] cb (applies to 1 concurrent assertion without their own clock)",
            "checks assert [clock] line 18",
            "checks assert [clock] a_mac",
            "coverage covergroup event [clock] covergroup event of cg_data",
            "helpers property [clock] p_unused (declaration only: no assertion uses it)",
        ]
    );
    assert_eq!(show(at(dut, "data")), ["checks assert [checked] line 22", "coverage coverpoint [checked] cp_data", "  coverage cross [cross item] x_req_data"]);
    assert!(show(at("logic dbg", "dbg")).is_empty(), "only in an action block");
    assert_eq!(show(at("class tx", "addr")), ["coverage coverpoint [checked] cp_addr", "connections argument [argument] a of cg_arg.sample()", "  coverage coverpoint [checked] cp_a"]);
    assert_eq!(show(at("module chk(", "req")), ["covers cover [checked] c_req", "helpers property [checked] p_handshake", "  checks assert [checked] a_handshake"]);
    // Forms, details and the macro call.
    let r = a.checks_at(f, at(dut, "req")).unwrap();
    let find = |label: &str| r.hits.iter().find(|h| h.label.as_deref() == Some(label)).unwrap();
    assert_eq!((find("a_busy").form, find("a_imm").form), (Some("concurrent"), Some("immediate")));
    assert_eq!(find("a_mac").detail, "`CHECK(a_mac, req |-> ##1 ack)");
    let d = a.checks_at(f, at(dut, "data")).unwrap();
    assert_eq!(d.hits[0].form, Some("deferred"));
    assert_eq!((d.target.name.as_str(), d.target.kind.as_str()), ("data", "port of dut"));
}

const CONFIG_UVM: &str = r#"
package uvm_pkg;
  class uvm_object; endclass
  class uvm_component extends uvm_object; function new(string name, uvm_component parent); endfunction endclass
  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_agent extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_root extends uvm_component; static function uvm_root get(); return null; endfunction endclass
  class uvm_config_db #(type T = int);
    static function void set(uvm_component cntxt, string inst_name, string field_name, T value); endfunction
    static function bit get(uvm_component cntxt, string inst_name, string field_name, inout T value); return 0; endfunction
  endclass
endpackage
"#;

const CONFIG_TB: &str = r#"
interface bus_if; endinterface
package p;
  typedef virtual bus_if bus_vif;
endpackage
package tb_pkg;
  import uvm_pkg::*;
  class base_cfg extends uvm_object; endclass
  class my_cfg extends base_cfg; endclass
  class my_agent extends uvm_agent;
    my_cfg cfg; base_cfg bcfg; int depth;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      void'(uvm_config_db #(my_cfg)::get(this, "", "cfg", cfg)); // G1
      void'(uvm_config_db #(base_cfg)::get(this, "", "cfg", bcfg)); // G2
      void'(uvm_config_db #(int)::get(this, "", "depth", depth)); // G3
      void'(uvm_config_db #(int)::get(get_parent(), "", "depth", depth)); // G4
      void'(uvm_config_db #(bit [7:0])::get(this, "", "b8", b8)); // G5
      void'(uvm_config_db #(virtual bus_if)::get(this, "", "vif", vif)); // G6
      void'(uvm_config_db #(my_cfg)::get(this, "", "cfg", cfg)); // G7
    endfunction
    bit [7:0] b8;
    virtual bus_if vif;
  endclass
  class pagent #(type T = int) extends uvm_agent;
    T t;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      void'(uvm_config_db #(T)::get(this, "", "depth", t)); // G8
    endfunction
  endclass
  class lone_agent extends uvm_agent;
    my_cfg cfg;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      void'(uvm_config_db #(my_cfg)::get(this, "", "cfg", cfg)); // G9
    endfunction
  endclass
  class my_env extends uvm_env;
    my_agent agt, other;
    my_cfg cfg;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      agt = my_agent::type_id::create("agt", this);
      other = my_agent::type_id::create("other", this);
      uvm_config_db #(my_cfg)::set(this, "agt", "cfg", cfg); // S1
      uvm_config_db #(my_cfg)::set(this, "*", "cfg", cfg); // S2
      uvm_config_db #(my_cfg)::set(this, "agt", "*", cfg); // S3
      uvm_config_db #(my_cfg)::set(this, "/ag.*/", "cfg", cfg); // S4
      uvm_config_db #(bit[7:0])::set(this, "*", "b8", 1); // S9
      uvm_config_db #(p::bus_vif)::set(this, "*", "vif", null); // S10
      uvm_config_db #(my_cfg)::set(this, "*", name_of_field, cfg); // S11
`ifdef OFF
      uvm_config_db #(my_cfg)::set(null, "*", "cfg", cfg); // S12
`endif
    endfunction
    string name_of_field;
  endclass
  class my_test extends uvm_test;
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      env = my_env::type_id::create("env", this);
      uvm_config_db #(int)::set(this, "env.ag?", "depth", 4); // S5
      uvm_config_db #(int)::set(null, "/uvm_test_top\..*/", "depth", 5); // S6
      uvm_config_db #(int)::set(uvm_root::get(), "uvm_test_top.env.+", "depth", 6); // S7
      uvm_config_db #(int)::set(.cntxt(this), .inst_name("env.other"), .field_name("depth"), .value(7)); // S8
    endfunction
  endclass
  class test_b extends uvm_test;
    lone_agent la;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      la = lone_agent::type_id::create("la", this);
    endfunction
  endclass
endpackage
"#;

#[test]
fn config_db_sets_are_matched_to_gets_as_uvm_does() {
    use dvi_analysis::ide::configdb::{glob_match, scope_of, Status};
    assert!(glob_match("uvm_test_top.env.*", "uvm_test_top.env.agt.drv"), "* crosses dots");
    assert!(glob_match("a?c", "abc") && !glob_match("a?c", "ac"));
    assert!(glob_match("env.+", "env.x") && !glob_match("env.+", "env."));
    assert!(!glob_match("env", "env.agt"), "anchored");
    assert_eq!((scope_of("", "x"), scope_of("top", ""), scope_of("top", "a")), ("x".into(), "top".into(), "top.a".into()));
    let (a, root) = project("configdb", &[("uvm_pkg.sv", CONFIG_UVM), ("tb_pkg.sv", CONFIG_TB)]);
    let f_of = |a: &dvi_analysis::Analysis, n: &str| a.vfs.lookup(&root.join(n)).unwrap();
    let off_of = |tag: &str| -> u32 {
        let line_start: usize = CONFIG_TB.lines().take_while(|l| !l.ends_with(&format!("// {tag}"))).map(|l| l.len() + 1).sum();
        (line_start + CONFIG_TB[line_start..].find("::get").unwrap() + 3) as u32
    };
    let idx = a.config_index();
    let call = |tag: &str| {
        let line = CONFIG_TB.lines().position(|l| l.ends_with(&format!("// {tag}"))).unwrap() as u32;
        idx.calls.iter().find(|c| a.to_range(c.method).unwrap().start.line == line).unwrap_or_else(|| panic!("{tag}"))
    };
    let st = |s: &str, g: &str| {
        let p = a.config_pair(call(s), call(g));
        (p.field, p.ty, p.scope)
    };
    use Status::*;
    assert_eq!(st("S1", "G1"), (Match, Match, Match));
    assert_eq!(a.config_pair(call("S1"), call("G1")).precedence, Some(998), "during build, set from a context at depth 2");
    assert_eq!(st("S2", "G1"), (Match, Match, Match));
    assert_eq!(st("S3", "G1").0, Mismatch, "a field of \"*\" provides no field \"cfg\"");
    assert_eq!(st("S4", "G1").2, Mismatch, "after a context, /.../ is no regular expression");
    assert_eq!(st("S1", "G2").1, Mismatch, "a set of the derived class does not satisfy a get of the base");
    assert_eq!(st("S5", "G3"), (Match, Match, Match), "env.ag? matches env.agt");
    assert_eq!(st("S6", "G3").2, Unknown, "a regular expression is not evaluated");
    assert_eq!(st("S7", "G3").2, Match, "the root context and +");
    assert_eq!(st("S8", "G3").2, Match, "named arguments; one of the two places of my_agent");
    assert_eq!(st("S5", "G4").2, Unknown, "get_parent() is not placed");
    assert_eq!(call("S8").inst.as_deref(), Some("env.other"));
    assert_eq!(st("S9", "G5").1, Match, "bit[7:0] is bit [7:0]");
    assert_eq!(st("S10", "G6").1, Match, "a package typedef of the virtual interface");
    assert_eq!(st("S1", "G7").0, Match, "an octal escape: \\143fg is cfg");
    assert_eq!(st("S5", "G8").1, Unknown, "a type parameter is not a type");
    assert_eq!(st("S11", "G1").0, Unknown, "a computed field");
    assert!(a.config_hover(f_of(&a, "tb_pkg.sv"), off_of("G1")).unwrap().0.contains("name_of_field"), "a computed field is a candidate in the hover");
    assert_eq!(st("S2", "G9").2, Mismatch, "my_env and lone_agent are never in the same test");
    assert!(!idx.calls.iter().any(|c| CONFIG_TB.lines().nth(a.to_range(c.method).unwrap().start.line as usize).unwrap().ends_with("// S12")), "inactive code");
    // Completion of field names inside a field string, same type first.
    assert!(a.config_field_completions("uvm_config_db #(p::bus_vif)::get(this, \"\", \"").is_some(), "a type with ::");
    assert!(a.config_field_completions("uvm_config_db #(int)::get(this, \"\", $sformatf(\"").is_none(), "a string inside the argument");
    assert!(a.config_field_completions("uvm_config_db #(int)::get(.field_name(\"").is_some(), "a named argument");
    let items = a.config_field_completions("      void'(uvm_config_db #(int)::get(this, \"\", \"de").unwrap();
    assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>()[..2], ["depth", "*"]);
    assert!(a.config_field_completions("uvm_config_db #(int)::get(this, \"de").is_none(), "not the field argument");
}

const CHECKS_EDGES: &str = r#"
`define BOTH(s) assert property (@(posedge clk) s); cover property (@(posedge clk) s);
package pk; bit q; endpackage
module c2(input logic a, input logic b);
  logic clk;
  a_a: assert property (@(posedge clk) a);
  a_b: assert property (@(posedge clk) b);
endmodule
module na(p);
  input bit p;
  logic clk;
  a_p: assert property (@(posedge clk) p);
endmodule
module cw(input logic w);
  logic clk;
  a_w: assert property (@(posedge clk) w);
endmodule
module bound(input logic s, input logic t);
  logic clk;
  a_s: assert property (@(posedge clk) s);
  a_t: assert property (@(posedge clk) t);
endmodule
module m;
  logic clk, c1, c2, sig, x, y, en, z, w, q, v;
  `BOTH(sig)
  a_two: assert property (@(posedge c1) sig |-> @(posedge c2) x);
  clocking cb @(posedge clk); endclocking
  default clocking cb;
  a_dflt: assert property (z);
  covergroup g @(posedge c1);
    cp: coverpoint w { bins one = {1} iff (en); }
  endgroup
  c2 u (.a(v), .b(v));
  c2 u1 (.a(x), .b()), u2 (.a(y), .b());
  na u_na (z);
  cw u_top (.*);
  if (1) begin : gen
    logic w;
    cw u_star (.*);
  end
endmodule
bind m bound u_b (.s(pk::q), .t(q));
bind m bound u_b2 (.s(z), .t(z));
module k;
  logic clk;
  bit s1, s2, s3;
  covergroup cgs with function sample(bit v);
    cp_v: coverpoint v;
  endgroup
  covergroup cgn(ref bit r) @(posedge clk);
    cp_r: coverpoint r;
  endgroup
  cgs cg = new();
  cgn cn = new(s3);
  initial begin
    cg.sample(.v(s1));
    cg.sample((s2));
  end
endmodule
"#;

#[test]
fn assertion_search_keeps_items_ports_and_arguments_apart() {
    let (a, root) = project("checksedges", &[("e.sv", CHECKS_EDGES)]);
    let f = a.vfs.lookup(&root.join("e.sv")).unwrap();
    // The declaration of `name` in the module `module`.
    let decl = |module: &str, name: &str| -> u32 {
        let base = CHECKS_EDGES.find(&format!("module {module}")).unwrap();
        let line = base + CHECKS_EDGES[base..].find("logic").unwrap_or(0);
        let k = line + CHECKS_EDGES[line..].find(&format!(" {name}")).unwrap() + 1;
        k as u32
    };
    fn flat(h: &dvi_analysis::ide::checks::CheckHit, depth: usize, out: &mut Vec<String>) {
        out.push(format!("{}{} {} [{}]", "  ".repeat(depth), h.kind, h.label.clone().unwrap_or_default(), h.roles.join(",")));
        for c in &h.children {
            flat(c, depth + 1, out);
        }
    }
    let show = |off: u32| -> Vec<String> {
        let r = a.checks_at(f, off).expect("searchable");
        let mut out = Vec::new();
        for h in &r.hits {
            flat(h, 0, &mut out);
        }
        out
    };
    let sig = show(decl("m", "sig"));
    assert!(sig.contains(&"assert  [checked]".to_string()) && sig.contains(&"cover  [checked]".to_string()), "one macro, two items: {sig:?}");
    assert!(sig.contains(&"assert a_two [checked]".to_string()), "{sig:?}");
    assert!(show(decl("m", "c2")).contains(&"assert a_two [clock]".to_string()), "a clock inside the property");
    assert!(show(decl("m", "clk")).iter().any(|l| l.starts_with("default clocking ") && l.ends_with("[clock]")), "default clocking cb; selects cb's clock");
    assert_eq!(show(decl("m", "en")), ["coverpoint cp [gate]"], "a bin's iff is a gate");
    assert_eq!(show(decl("m", "v")), ["port a of u [connection]", "  assert a_a [checked]", "port b of u [connection]", "  assert a_b [checked]"]);
    assert_eq!(show(decl("m", "x")), ["assert a_two [checked]", "port a of u1 [connection]", "  assert a_a [checked]"], "the first instance of the statement");
    let z = show(decl("m", "z"));
    assert!(z.contains(&"port p of u_na [connection]".to_string()), "non-ANSI positional: {z:?}");
    assert!(z.contains(&"port s of u_b2 (bind) [connection]".to_string()) && z.contains(&"port t of u_b2 (bind) [connection]".to_string()), "every port of the bind: {z:?}");
    assert_eq!(show(decl("m", "w")), ["coverpoint cp [checked]", "port w of u_top (.*) [connection]", "  assert a_w [checked]"], "gen.w is what u_star connects");
    assert_eq!(show(decl("m", "q")), ["port t of u_b (bind) [connection]", "  assert a_t [checked]"], "pk::q is not m.q");
    let k = |name: &str| -> u32 {
        let base = CHECKS_EDGES.find("module k").unwrap();
        (base + CHECKS_EDGES[base..].find(&format!(" {name},")).or_else(|| CHECKS_EDGES[base..].find(&format!(" {name};"))).unwrap() + 1) as u32
    };
    assert_eq!(show(k("s1")), ["argument v of cgs.sample() [argument]", "  coverpoint cp_v [checked]"], "a named argument");
    assert_eq!(show(k("s2")), ["argument v of cgs.sample() [argument]", "  coverpoint cp_v [checked]"], "an argument in parentheses");
    assert_eq!(show(k("s3")), ["argument r of cgn.new() [argument]", "  coverpoint cp_r [checked]"], "new() of a declaration");
}

const INSTANCES: &str = r#"interface bus_if; logic v; endinterface
module c(input logic clk, input logic rst_n = 1'b1, bus_if bus, output logic [7:0] q, input logic en);
endmodule
module na(a, b);
  input a;
  output b;
endmodule
module pp #(parameter int W = 8, parameter int D = 2) (input logic x);
endmodule
module top;
  logic clk, en, x;
  logic [7:0] q;
  bus_if bus();
  c u_named (
    .clk(clk),
    .q(q) // the output
  );
  c u_star (.*, .q());
  na u_ord (x /* first */, en);
  na u_hole (x, );
  pp #(16, 4) u_p (.x(x));
  na u_dup (.a(x), .a(en));
  na u_mix (x, .b(en));
  na u_many (x, en, clk);
  c u_full (.clk(clk), .q(q), .bus(bus), .en(en));
  pp #(.W(1), .D(2),) u_trail (.x(x),);
endmodule
module dflt(input logic a = 1'b1, input logic b);
endmodule
module sel(a[0]);
  input [1:0] a;
endmodule
module esc(input \p.q );
endmodule
module w8(input logic [7:0] p);
endmodule
module mi(interface a, b);
endmodule
primitive udp(o, i);
  output o;
  input i;
  table
    0 : 0;
    1 : 1;
  endtable
endprimitive
module top2;
  logic b, x, o;
  logic [15:0] p;
  bus_if bus();
  dflt u_blank (, b);
  sel u_sel (x);
  esc u_esc (x);
  w8 u_w8 (.*);
  mi u_mi (.a(bus));
  udp #(1) u_udp (o, x);
endmodule
"#;

/// `text` with `edits` (spans into it) applied.
fn apply(text: &str, edits: &[(dvi_syntax::Span, String)]) -> String {
    let mut e = edits.to_vec();
    e.sort_by_key(|x| std::cmp::Reverse(x.0.start));
    let mut out = text.to_string();
    for (sp, t) in e {
        out.replace_range(sp.start as usize..sp.end as usize, &t);
    }
    out
}

#[test]
fn instance_connections_are_repaired_without_changing_their_meaning() {
    let (a, root) = project("instances", &[("top.sv", INSTANCES)]);
    let f = a.vfs.lookup(&root.join("top.sv")).unwrap();
    let at = |needle: &str| INSTANCES.find(needle).unwrap() as u32 + 3;
    let actions = |needle: &str| a.instance_actions(f, at(needle)).into_iter().map(|r| (r.title.clone(), apply(INSTANCES, &r.edits))).collect::<Vec<_>>();
    // Connect missing ports: the defaulted one left out, the list's layout kept.
    let named = actions("c u_named");
    assert_eq!(named.len(), 1, "{named:?}");
    assert_eq!(named[0].0, "Connect 2 missing ports of u_named (some left out: default value)");
    assert!(named[0].1.contains("    .q(q),\n    .bus(bus),\n    .en(en) // the output\n  );"), "{}", named[0].1);
    // Expand .*: the defaulted port with no signal is left out, as .* does.
    let star = actions("c u_star");
    assert_eq!(star[0].0, "Expand .* of u_star");
    assert!(star[0].1.contains("c u_star (.clk(clk), .bus(bus), .en(en), .q());"), "{}", star[0].1);
    // Ordered to named: comments kept, an empty hole explicit.
    let ord = actions("na u_ord");
    assert_eq!(ord[0].0, "Use named port connections for u_ord");
    assert!(ord[0].1.contains("na u_ord (.a(x) /* first */, .b(en));"), "{}", ord[0].1);
    let hole = actions("na u_hole");
    assert!(hole.first().is_some_and(|h| h.1.contains("na u_hole (.a(x), .b());")), "{hole:?}");
    let pp = actions("pp #(16");
    assert_eq!(pp.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["Use named parameter values for pp"]);
    assert!(pp[0].1.contains("pp #(.W(16), .D(4)) u_p (.x(x));"));
    assert!(actions("c u_full").is_empty(), "nothing to repair");
    assert!(actions("na u_many").is_empty(), "more items than ports: not rewritten");
    assert!(actions("dflt u_blank").is_empty(), "an empty item keeps a default that .a() would not");
    assert!(actions("sel u_sel").is_empty(), "a part of a port has no name");
    let esc = actions("esc u_esc");
    assert!(esc.first().is_some_and(|e| e.1.contains("esc u_esc (.\\p.q (x));")), "{esc:?}");
    assert!(actions("w8 u_w8").is_empty(), ".* with another type is not expanded (it is an error there)");
    assert!(actions("mi u_mi").is_empty(), "b is an interface port (inherited) with no interface to connect");
    // Every rewrite parses again without new problems.
    for (title, text) in named.iter().chain(&star).chain(&ord).chain(&pp) {
        let (b, _) = project("instances_after", &[("top.sv", text)]);
        let before = problems(&a).len();
        let after = problems(&b).len();
        assert_eq!(after, before, "{title}: {:?}", problems(&b));
    }
    // Problems that are certain.
    let p = problems(&a);
    assert_eq!(
        p,
        [
            "top.sv: The port 'a' is connected more than once",
            "top.sv: Ordered and named port connections cannot be mixed in one list",
            "top.sv: 3 ordered port connections, but only 2 ports",
        ]
    );
    // Completion of names at an item's start: connected ones last.
    let end = INSTANCES.find("endmodule\n\"").unwrap_or(INSTANCES.rfind("endmodule").unwrap()) as u32;
    let labels = |line: &str| a.connection_completions(f, end, line).map(|v| {
        let mut v: Vec<(String, String)> = v.into_iter().map(|i| (i.sort_text.unwrap(), i.label)).collect();
        v.sort();
        v.into_iter().map(|x| x.1).collect::<Vec<_>>()
    });
    assert_eq!(labels("  c u_x (.clk(clk), .").unwrap(), ["rst_n", "bus", "q", "en", "clk"]);
    assert_eq!(labels("  pp #(.W(1), .").unwrap(), ["D", "W"]);
    assert!(labels("  c u_x (.clk(obj.").is_none(), "a member, not a port");
    assert_eq!(labels("  c u0 (.clk(clk)), u1 (.").unwrap()[0], "clk", "a later instance of the statement");
}

const IMPACT_UVM: &str = "`define uvm_component_utils(T)\npackage uvm_pkg;\n  class uvm_component; function new(string name, uvm_component parent); endfunction endclass\n  class uvm_factory; endclass\n  uvm_factory factory;\n  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\n  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass\nendpackage\n";

const IMPACT_TB: &str = r#"package tb_pkg;
  import uvm_pkg::*;
  class leaf extends uvm_env; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class err_leaf extends leaf; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class drv extends uvm_env; `uvm_component_utils(drv) function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class err_drv extends drv; `uvm_component_utils(err_drv) function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class agent extends uvm_env;
    drv d;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); d = drv::type_id::create("d", this); endfunction
  endclass
  class env_a extends uvm_env;
    leaf l;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      l = leaf::type_id::create("l", this);
      set_type_override_by_type(leaf::get_type(), err_leaf::get_type()); // LATE
    endfunction
  endclass
  class env_b extends uvm_env;
    leaf l, by_new;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      l = leaf::type_id::create("l", this);
      by_new = new("by_new", this);
    endfunction
  endclass
  class the_test extends uvm_test;
    env_a a; env_b b; agent agt1, agt2;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      set_inst_override_by_type("agt1.d", drv::get_type(), err_drv::get_type()); // INST
      factory.set_inst_override_by_type(drv::get_type(), err_drv::get_type(), "*.agt2.*"); // ABS
      a = env_a::type_id::create("a", this);
      b = env_b::type_id::create("b", this);
      agt1 = agent::type_id::create("agt1", this);
      agt2 = agent::type_id::create("agt2", this);
    endfunction
  endclass
endpackage
"#;

#[test]
fn an_override_call_says_what_it_changes_in_the_modeled_build() {
    let (a, root) = project("impact", &[("uvm_pkg.sv", IMPACT_UVM), ("tb_pkg.sv", IMPACT_TB)]);
    let f = a.vfs.lookup(&root.join("tb_pkg.sv")).unwrap();
    let hover = |tag: &str| -> String {
        let line_start: usize = IMPACT_TB.lines().take_while(|l| !l.ends_with(&format!("// {tag}"))).map(|l| l.len() + 1).sum();
        let k = line_start + IMPACT_TB[line_start..].find("set_").unwrap() + 2;
        a.hover(f, k as u32).unwrap().0
    };
    let late = hover("LATE");
    assert!(late.contains("Changes (1):\n- `uvm_test_top.b.l` (leaf → err_leaf)"), "a sibling built later: {late}");
    assert!(late.contains("- `uvm_test_top.a.l`: registered after it was made"), "{late}");
    assert!(late.contains("- `uvm_test_top.b.by_new`: made with `new`"), "{late}");
    let inst = hover("INST");
    assert!(inst.contains("Changes (1):\n- `uvm_test_top.agt1.d` (drv → err_drv)"), "{inst}");
    assert!(inst.contains("- `uvm_test_top.agt2.d`: another override comes first: drv → err_drv"), "a relative pattern gives no certain path reason: {inst}");
    let abs = hover("ABS");
    assert!(abs.contains("Changes (1):
- `uvm_test_top.agt2.d`"), "{abs}");
    assert!(abs.contains("- `uvm_test_top.agt1.d`: its path `uvm_test_top.agt1.d` does not match `*.agt2.*`"), "{abs}");
    assert!(inst.contains("as modeled"), "the scope is said");
}

#[test]
fn a_missing_semicolon_after_an_assignment_does_not_take_the_next_declaration() {
    let t = "module m(input logic x);\n  assign a = 0\n  wire b;\n  assign c = b;\n  assign d = 0\n  type(x) e;\n  assign f = e;\nendmodule\n";
    let (a, _) = project("assignsemi", &[("m.sv", t)]);
    let p = problems(&a);
    assert_eq!(p.iter().filter(|m| m.contains("Expected ';'")).count(), 2, "{p:?}");
    assert!(!p.iter().any(|m| m.contains("'b' is not declared") || m.contains("'e' is not declared")), "{p:?}");
}

const DRIVERS_SRC: &str = r#"
module child(input logic a, output logic y, inout wire z);
  assign y = a;
endmodule

module leaf(input logic d, output logic q);
  always_ff @(posedge d) q <= d;
endmodule

module top;
  logic clk, s, t, u, v, w;
  wire g;
  logic [7:0] bus, idx;
  int n;
  string str;
  logic p = 1'b0;
  child u_c (.a(s), .y(t), .z());
  child u_p (s, u, );
  leaf u_l (.d(t), .q(v));
  child u_e (.a(s & w), .y(w), .z());
  and g1 (g, s, w);
  assign s = clk;
  task get(output int o);
    o = 1;
  endtask
  always_comb bus[idx] = n;
  initial begin
    n++;
    get(n);
    void'($sscanf(str, "%d", n));
    force w = 1'b1;
    release w;
    if (n <= 3) n += 2;
  end
endmodule
"#;

/// The sites of the declaration at `off`, one line each (children indented), and the limits and
/// notes.
fn access_lines(a: &Analysis, f: dvi_syntax::FileId, off: u32, writes: bool) -> (Vec<String>, Vec<&'static str>, Vec<String>) {
    fn render(s: &dvi_analysis::ide::drivers::AccessSite, depth: usize, out: &mut Vec<String>) {
        let ctx = if s.context.is_empty() { String::new() } else { format!(" @{}", s.context) };
        let notes = if s.notes.is_empty() { String::new() } else { format!(" ({})", s.notes.join("; ")) };
        let process = s.process.map(|p| format!(" in {p}")).unwrap_or_default();
        out.push(format!("{}{} {} line {}{process}{ctx}{notes}", "  ".repeat(depth), s.group, s.label, s.line.unwrap_or(0) + 1));
        for c in &s.children {
            render(c, depth + 1, out);
        }
    }
    let r = a.accesses_at(f, off, writes).expect("a signal");
    let mut out = Vec::new();
    for s in &r.sites {
        render(s, 0, &mut out);
    }
    (out, r.limits, r.notes)
}

/// The offset of the first whole word `needle` after `after` in `src`.
fn word_at(src: &str, after: &str, needle: &str) -> u32 {
    let base = src.find(after).unwrap();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let (i, _) = src[base..].match_indices(needle).find(|(i, _)| !word(src[..base + i].chars().last()) && !word(src[base + i + needle.len()..].chars().next())).unwrap();
    (base + i) as u32
}

#[test]
fn drivers_and_loads_of_a_signal_follow_ports_both_ways() {
    let (a, root) = project("drivers", &[("a.sv", DRIVERS_SRC)]);
    let f = a.vfs.lookup(&root.join("a.sv")).unwrap();
    let show = |after: &str, name: &str, writes: bool| -> Vec<String> {
        let (lines, limits, notes) = access_lines(&a, f, word_at(DRIVERS_SRC, after, name), writes);
        assert!(limits.is_empty() && notes.is_empty(), "{limits:?} {notes:?}");
        lines
    };
    let top = "logic clk";
    assert_eq!(show(top, "s", true), ["writes continuous line 22"], "a gate's input is no write");
    assert_eq!(
        show(top, "s", false),
        [
            "connections a of u_c (input) line 17",
            "  reads read line 3 @u_c",
            "connections a of u_p (input) line 18",
            "  reads read line 3 @u_p",
            "connections a of u_e (input) line 20 (connected in an expression: not followed)",
            "reads gate input line 21",
        ]
    );
    assert_eq!(show(top, "t", true), ["connections y of u_c (output) line 17", "  writes continuous line 3 @u_c"]);
    assert_eq!(show(top, "t", false), ["connections d of u_l (input) line 19", "  reads read line 7 in always_ff @u_l", "  reads read line 7 in always_ff @u_l"]);
    assert_eq!(
        show(top, "n", true),
        ["writes increment line 28 in initial", "writes argument of get line 29", "writes argument of $sscanf line 30", "writes compound line 33 in initial"],
        "an output formal and `$sscanf`'s third argument write; `n <= 3` in a condition does not"
    );
    assert_eq!(show(top, "n", false), ["reads read line 26 in always_comb", "reads increment line 28 in initial", "reads read line 33 in initial", "reads compound line 33 in initial"]);
    assert_eq!(show(top, "w", true), ["connections y of u_e (output) line 20", "  writes continuous line 3 @u_e", "writes force line 31 in initial", "control release line 32 in initial"]);
    assert_eq!(show(top, "bus", true), ["writes blocking line 26 in always_comb"]);
    assert!(show(top, "idx", true).is_empty(), "an index on the left side is read");
    assert_eq!(show(top, "idx", false), ["reads read line 26 in always_comb"]);
    assert_eq!(show(top, "g", true), ["writes gate output line 21"]);
    assert_eq!(show(top, "p", true), ["writes variable initializer line 16"]);
    // From a port: up to each instantiation, one branch each, and down again elsewhere.
    assert_eq!(
        show("module child(", "a", true),
        ["connections a of u_c in top line 17", "  writes continuous line 22 @top", "connections a of u_p in top line 18", "  writes continuous line 22 @top", "connections a of u_e in top line 20 (connected to an expression: s & w)"]
    );
    assert_eq!(
        show("module child(", "y", false),
        [
            "connections y of u_c in top line 17",
            "  connections d of u_l (input) line 19 @top",
            "    reads read line 7 in always_ff @top.u_l",
            "    reads read line 7 in always_ff @top.u_l",
            "connections y of u_p in top line 18",
            "connections y of u_e in top line 20",
            "  reads gate input line 21 @top",
        ],
        "the search that came up through u_c does not go down into u_c again"
    );
}

const DRIVERS_EDGE_SRC: &str = r#"
`define SET(x, v) x = v;
module m3(input logic i, output logic o); assign o = i; endmodule
module m2(input logic i, output logic o); m3 u3 (.i, .o); endmodule
module m1(input logic i, output logic o); m2 u2 (.*); endmodule
module m0(input logic i, output logic o); m1 u1 (.i(i), .o(o)); endmodule
module chk(input logic c); endmodule
module tb;
  logic a, b, c, d;
  m0 u0 (.i(a), .o(b));
  generate if (1) begin : g
    m3 u_g (.i(c), .o(d));
  end endgenerate
  unknown_mod u_x (c, d);
  initial `SET(c, 1)
  task t(output logic r); r = a; endtask
endmodule
bind tb chk u_bind (.c(c));
class obj;
  int f;
  function void set(); f = 1; endfunction
endclass
module user;
  obj h;
  initial begin h = new; h.f = 2; $display(h.f); end
endmodule
"#;

#[test]
fn drivers_and_loads_through_implicit_connections_macros_and_classes() {
    let (a, root) = project("drivers_edge", &[("a.sv", DRIVERS_EDGE_SRC)]);
    let f = a.vfs.lookup(&root.join("a.sv")).unwrap();
    let show = |after: &str, name: &str, writes: bool| access_lines(&a, f, word_at(DRIVERS_EDGE_SRC, after, name), writes);
    let tb = "logic a";
    let bind = vec!["`bind` instances in tb are not followed".to_string()];
    assert_eq!(
        show(tb, "b", true),
        (
            vec!["connections o of u0 (output) line 10", "  connections o of u1 (output) line 6 @u0", "    connections o of u2 (.*) (output) line 5 @u0.u1", "      connections o of u3 (output) line 4 @u0.u1.u2"].into_iter().map(String::from).collect::<Vec<_>>(),
            vec!["depth"],
            bind.clone()
        ),
        "named, `.*` and `.o` connections; the fourth level is past the depth limit"
    );
    assert_eq!(show(tb, "c", true).0, ["connections connection of u_x line 14 (module not found: not followed)", "writes blocking line 15 in initial"], "the macro's assignment");
    assert_eq!(show(tb, "c", false), (vec!["connections i of g.u_g (input) line 12".to_string(), "  reads read line 3 @g.u_g".into(), "connections connection of u_x line 14 (module not found: not followed)".into()], vec![], bind.clone()), "in a generate block; the `.i` of m2 is no read of m3's port");
    assert_eq!(show("output logic r", "r", true).0, ["writes blocking line 16 in task"]);
    let field = vec!["a class field: the sites are those of every object of the class".to_string()];
    assert_eq!(show("int f", "f", true), (vec!["writes blocking line 21 in function".to_string(), "writes blocking line 25 in initial".into()], vec![], field.clone()));
    assert_eq!(show("int f", "f", false), (vec!["reads argument of $display line 25".to_string()], vec![], field));
    assert_eq!(show("obj h", "h", true).0, ["writes blocking line 25 in initial"], "`h.f = 2` writes the object's field, not the handle");
}

#[test]
fn drivers_and_loads_review_cases() {
    let cases: &[(&str, &[(&str, &str, bool)])] = &[
        ("module m;\n  string text;\n  int fd, count;\n  initial count = $fgets(text, fd);\nendmodule\n", &[("int fd", "fd", true), ("int fd", "fd", false), ("string text", "text", true)]),
        ("module m;\n  int x;\n  task bump(inout int v);\n    v = v + 1;\n  endtask\n  initial bump(x);\nendmodule\n", &[("int x", "x", true), ("int x", "x", false)]),
        ("class C;\n  task get(output int v);\n    v = 1;\n  endtask\nendclass\nmodule m;\n  C c;\n  int x;\n  initial begin\n    c = new;\n    c.get(x);\n  end\nendmodule\n", &[("int x", "x", true), ("int x", "x", false)]),
        ("module child(output logic y);\n  initial y = 1;\nendmodule\nmodule top;\n  wire [1:0] q;\n  child u[1:0](.y(q));\nendmodule\n", &[("wire [1:0] q", "q", true), ("wire [1:0] q", "q", false)]),
        ("module child(output wire [1:0] y);\n  assign y = 2'b00;\nendmodule\nmodule top;\n  wire a, b;\n  child u(.y({a, b}));\nendmodule\n", &[("wire a", "a", true), ("wire a", "a", false)]),
        ("module m;\n  int i, x, y;\n  initial begin\n    for (i = 0; i < 4; i++) ;\n    x = y++;\n    for (int j = 0; j < 2; j += 1) ;\n  end\nendmodule\n", &[("int i", "i", true), ("int i", "i", false), ("int i", "y", true), ("int j", "j", true)]),
        ("`define SET_BIT(a, i) a[i] = 1'b1\nmodule m;\n  logic [7:0] bus;\n  int index;\n  initial `SET_BIT(bus, index);\nendmodule\n", &[("int index", "index", true), ("int index", "index", false), ("logic [7:0] bus", "bus", true)]),
        ("module m(input a, b, output wire y, z, w);\n  and g0(y, a, b), g1(z, a, b);\n  and #1 g2(w, a, b);\nendmodule\n", &[("output wire y", "z", true), ("output wire y", "w", true), ("output wire y", "z", false)]),
        ("module m(q);\n  output wire q;\nendmodule\n", &[("output wire q", "q", false)]),
        ("module child(output logic y);\n  initial y = 1;\nendmodule\nmodule top;\n  wire q;\n  if (1) begin : g1\n    child u(.y(q));\n  end\n  if (1) begin : g2\n    child u(.y(q));\n  end\nendmodule\n", &[("wire q", "q", true)]),
    ];
    let mut got = Vec::new();
    for (i, (src, queries)) in cases.iter().enumerate() {
        let (a, root) = project(&format!("drvrev{i}"), &[("a.sv", src)]);
        let f = a.vfs.lookup(&root.join("a.sv")).unwrap();
        for (after, name, writes) in queries.iter() {
            let (lines, limits, notes) = access_lines(&a, f, word_at(src, after, name), *writes);
            assert!(limits.is_empty() && notes.is_empty(), "{limits:?} {notes:?}");
            got.push(format!("{i} {name} {}: {}", if *writes { "writes" } else { "reads" }, lines.join(" | ")));
        }
    }
    assert_eq!(
        got,
        [
            "0 fd writes: ",
            "0 fd reads: reads argument of $fgets line 4",
            "0 text writes: writes argument of $fgets line 4",
            "1 x writes: writes argument of bump line 6",
            "1 x reads: reads argument of bump line 6",
            "2 x writes: writes argument of get line 11",
            "2 x reads: ",
            "3 q writes: connections y of u (output) line 6 |   writes blocking line 2 in initial @u",
            "3 q reads: ",
            "4 a writes: connections y of u (output) line 6 (connected in an expression: not followed)",
            "4 a reads: ",
            "5 i writes: writes blocking line 4 in initial | writes increment line 4 in initial",
            "5 i reads: reads read line 4 in initial | reads increment line 4 in initial",
            "5 y writes: writes increment line 5 in initial",
            "5 j writes: writes compound line 6 in initial | writes variable initializer line 6 in initial",
            "6 index writes: ",
            "6 index reads: reads read line 5 in initial",
            "6 bus writes: writes blocking line 5 in initial",
            "7 z writes: writes gate output line 2",
            "7 w writes: writes gate output line 3",
            "7 z reads: ",
            "8 q reads: ",
            "9 q writes: connections y of g1.u (output) line 7 |   writes blocking line 2 in initial @g1.u | connections y of g2.u (output) line 10 |   writes blocking line 2 in initial @g2.u",
        ],
        "$fgets writes only its first argument; an inout actual is read and written; a method call's output argument; an array of instances; \
         a concatenation on an output port; for headers and nested increments; a macro's index; later gates and delays; a non-ANSI header; generate scopes in paths"
    );
}

#[test]
fn drivers_and_loads_scale_linearly_and_survive_malformed_nesting() {
    let n = 20_000;
    let mut calls = String::from("module m;\n  logic s;\n  function void consume(input logic v); endfunction\n");
    for i in 0..n {
        calls.push_str(&format!("  logic v{i};\n"));
    }
    calls.push_str("  initial begin\n");
    for _ in 0..n {
        calls.push_str("    consume(s);\n");
    }
    calls.push_str("  end\nendmodule\n");
    let mut cat = String::from("module c;\n  logic [19999:0] bus;\n  initial {");
    cat.push_str(&(0..n).map(|i| format!("bus[{i}]")).collect::<Vec<_>>().join(", "));
    cat.push_str("} = '0;\nendmodule\n");
    let deep = 5_000;
    let nested = format!("module d(input p); endmodule\nmodule e;\n  logic s;\n  d u(.p({}s{}));\nendmodule\n", ".a(".repeat(deep), ")".repeat(deep));
    // Instances found through an index, lists cached; the left side classified in one pass
    // (20,000 names in a concatenation); a 5,000-deep `.a(` nest climbed with a bound.
    let (a, root) = project("drvscale", &[("a.sv", &calls), ("b.sv", &cat), ("c.sv", &nested)]);
    let fa = a.vfs.lookup(&root.join("a.sv")).unwrap();
    let (l, lim, _) = access_lines(&a, fa, word_at(&calls, "logic s", "s"), true);
    assert!(l.is_empty() && lim.is_empty(), "{lim:?}");
    let (_, lim, _) = access_lines(&a, fa, word_at(&calls, "logic s", "s"), false);
    assert_eq!(lim, ["sites"]);
    let fb = a.vfs.lookup(&root.join("b.sv")).unwrap();
    let (l, lim, _) = access_lines(&a, fb, word_at(&cat, "logic [19999:0]", "bus"), true);
    assert!(l.len() == 2000 && l.iter().all(|x| x == "writes blocking line 3 in initial"), "each name of the concatenation is a write: {}", l.len());
    assert_eq!(lim, ["sites"]);
    let fc = a.vfs.lookup(&root.join("c.sv")).unwrap();
    let (l, _, _) = access_lines(&a, fc, word_at(&nested, "logic s", "s"), false);
    assert_eq!(l.len(), 1);
}

// ------------------------------------------------------------------ general review 1

#[test]
fn register_models_survive_deep_expressions_unfinished_loops_and_follow_widths() {
    std::thread::Builder::new().stack_size(16 << 20).spawn(register_case).unwrap().join().unwrap();
}

fn register_case() {
    let deep = format!("{}0{}", "1+(".repeat(20_000), ")".repeat(20_000));
    let src = format!(
        "virtual class uvm_reg_block; endclass\nvirtual class uvm_reg; endclass\nvirtual class uvm_mem; endclass\nclass uvm_reg_map; endclass\nclass r_t extends uvm_reg;\n  function new(string name = \"r_t\"); super.new(name, 32, 0); endfunction\nendclass\nclass m_t extends uvm_mem;\n  function new(string name = \"m_t\"); super.new(name, 4, 32, \"RW\", 0); endfunction\nendclass\nclass top_block extends uvm_reg_block;\n  rand r_t a;\n  m_t mem;\n  function new(string name = \"top_block\"); super.new(name); endfunction\n  virtual function void build();\n    int unsigned off = 32'hffff_ffff + 32'd1;\n    if (0) lock_model();\n    default_map = create_map(\"map\", 0, 4, UVM_LITTLE_ENDIAN);\n    a = r_t::type_id::create(\"a\");\n    a.configure(this, null, \"\");\n    default_map.add_reg(a, off, \"RW\");\n    mem = m_t::type_id::create(\"mem\");\n    mem.configure(this, \"\");\n    default_map.add_mem(mem, 'h100, \"RW\");\n    for (\n  endfunction\nendclass\n"
    );
    // The deep expression in a file of its own (the parser stops a file nested too deeply).
    let deep_src = format!("class deep_block extends uvm_reg_block;\n  function new(string name = \"deep_block\"); super.new(name); endfunction\n  virtual function void build();\n    default_map = create_map(\"map\", {deep}, 4, UVM_LITTLE_ENDIAN);\n  endfunction\nendclass\n");
    let (a, _) = project("review1ral", &[("ral.sv", &src), ("deep.sv", &deep_src)]);
    let (blocks, _) = a.register_blocks();
    let block = |n: &str| *blocks.iter().find(|b| a.decl(**b).name == n).unwrap();
    // A deep expression: no crash, just no value.
    let _ = a.register_model(block("deep_block"));
    let model = a.register_model(block("top_block"));
    let hits = |x: u128| a.registers_at(&model, x).into_iter().map(|h| h.path.join(".")).collect::<Vec<_>>();
    assert_eq!(hits(0), ["top_block.map.a"], "an int unsigned local is cut to 32 bits; a lock in a branch does not stop the model; an unfinished `for (` stops quietly");
    assert_eq!(hits(0x10d), ["top_block.map.mem"], "a byte of the memory's last word");
}

#[test]
fn constant_functions_literals_strings_and_bits_are_evaluated_honestly() {
    // On a stack the size of the engine's worker threads (deep nesting is parsed there).
    std::thread::Builder::new().stack_size(16 << 20).spawn(constant_functions_case).unwrap().join().unwrap();
}

fn constant_functions_case() {
    let nested = format!("{}return 1;{}", "begin ".repeat(5_000), " end".repeat(5_000));
    let src = format!(
        "module leaf; endmodule\nmodule top;\n  function automatic int f();\n    {nested}\n  endfunction\n  localparam P = f();\n  localparam Q = 'h1_0000_0000;\n  parameter string S = \"\\141\";\n  int arr[4];\n  localparam N = $bits(arr);\n  if (Q != 0) begin : wide\n    leaf u();\n  end\n  if (S == \"a\") begin : decoded\n    leaf v();\n  end\nendmodule\n"
    );
    let (a, _) = project("review1elab", &[("t.sv", &src)]);
    let (t, _) = a.design_tree(Some("top"), &dvi_analysis::ide::views::TreeQuery::new(100));
    let p = |n: &str| t[0].params.iter().find(|x| x.name == n).map(|x| (x.value.clone(), x.why.clone())).unwrap();
    assert_eq!(p("P").0, None, "nested too deeply: not evaluated, no crash");
    assert_eq!(p("Q").0.as_deref(), Some("4294967296"), "an unsized based number keeps its width");
    assert_eq!(p("S").0.as_deref(), Some("\"a\""), "escapes decoded");
    assert_eq!(p("N").1.as_deref(), Some("`$bits` of an unpacked array"));
    let names: Vec<&str> = t[0].children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["wide", "decoded"]);
}

#[test]
fn override_text_keeps_specializations_escapes_and_package_names() {
    let src = "package p;\n  typedef int value_t;\n  class box #(type T = int); endclass\n  virtual class base #(type T = int);\n    pure virtual function value_t f();\n    virtual function void g(T \\a.b , int y); endfunction\n    virtual function int h; input int x; return x; endfunction\n  endclass\nendpackage\nclass child extends p::base #(.T(p::box#(int)));\nendclass\n";
    let (a, _) = project("review1ovr", &[("t.sv", src)]);
    let class = {
        let u = a.unit(a.units_for(a.project_files()[0])[0]);
        let d = u.parsed.decls.iter().position(|d| d.name == "child").unwrap();
        dvi_analysis::DeclRef { unit: u.id, decl: d as u32 }
    };
    let os = a.overridable_methods(class);
    let text = |n: &str| os.iter().find(|o| o.name == n).and_then(|o| a.override_text(o, class, "  ", "\n", false));
    assert_eq!(text("f").as_deref(), Some("virtual function p::value_t f();\nendfunction"), "a package's type the class does not see: qualified");
    assert_eq!(text("g").as_deref(), Some("virtual function void g(p::box#(int) \\a.b , int y);\nendfunction"), "the named specialization whole; an escaped argument");
    assert_eq!(text("h"), None, "a non-ANSI header: not offered");
}

#[test]
fn instance_lists_assertions_and_drivers_review_cases() {
    // `u(,,)`: no "Connect missing ports".
    let src = "module m(input a, input b, input c); endmodule\nmodule top;\n  logic x, y;\n  m u(,,);\nendmodule\n";
    let (a, root) = project("review1inst", &[("t.sv", src)]);
    let f = a.vfs.lookup(&root.join("t.sv")).unwrap();
    let titles: Vec<String> = a.instance_actions(f, src.find("u(,,)").unwrap() as u32).into_iter().map(|r| r.title).collect();
    assert!(!titles.iter().any(|t| t.starts_with("Connect")), "{titles:?}");
    // An assertion found through the 65th instance of one statement.
    let insts: String = (1..65).map(|i| format!(", u{i}(y)")).collect();
    let src = format!("module c(input logic p);\n  a_p: assert property (p);\nendmodule\nmodule top;\n  logic x, y;\n  c u0(x){insts};\nendmodule\n");
    let (a, root) = project("review1checks", &[("t.sv", &src)]);
    let f = a.vfs.lookup(&root.join("t.sv")).unwrap();
    let r = a.checks_at(f, src.find("logic x").unwrap() as u32 + 6).unwrap();
    assert_eq!(r.hits.len(), 1, "{:?}", r.hits.iter().map(|h| h.label.clone()).collect::<Vec<_>>());
    // Increments in an expression statement and a condition; a write through another instance.
    let src = "module child(output logic y); endmodule\nmodule top;\n  int i;\n  logic a, b;\n  child u0(a), u1(b);\n  assign u1.y = 1'b1;\n  initial begin\n    $display(i++);\n    if (i-- > 0) ;\n  end\nendmodule\n";
    let (a, root) = project("review1drv", &[("t.sv", src)]);
    let f = a.vfs.lookup(&root.join("t.sv")).unwrap();
    let (l, _, _) = access_lines(&a, f, word_at(src, "int i", "i"), true);
    assert_eq!(l, ["writes increment line 8 in initial", "writes increment line 9 in initial"]);
    let (l, _, _) = access_lines(&a, f, word_at(src, "logic a", "a"), true);
    assert_eq!(l, ["connections y of u0 (output) line 5"], "u1.y is not u0's");
}




// ------------------------------------------------------------------ general review 3

#[test]
fn register_fields_overlap_by_interval_and_deep_conditions_do_not_recurse() {
    std::thread::Builder::new().stack_size(16 << 20).spawn(register_overlap_case).unwrap().join().unwrap();
}

fn register_overlap_case() {
    let ifs = "if (1) ".repeat(100_000);
    let src = format!(
        "virtual class uvm_reg_block; endclass\nvirtual class uvm_reg; endclass\nclass uvm_reg_field; endclass\nclass uvm_reg_map; endclass\nclass r_t extends uvm_reg;\n  rand uvm_reg_field a, b, c, e, f;\n  function new(string name = \"r_t\"); super.new(name, 32, 0); endfunction\n  virtual function void build();\n    a = uvm_reg_field::type_id::create(\"a\"); a.configure(this, 4, 0, \"RW\", 0, 0, 1, 1, 0);\n    b = uvm_reg_field::type_id::create(\"b\"); b.configure(this, 1, 2, \"RW\", 0, 0, 1, 1, 0);\n    c = uvm_reg_field::type_id::create(\"c\"); c.configure(this, 1, 8, \"RW\", 0, 0, 1, 1, 0);\n    e = uvm_reg_field::type_id::create(\"e\"); e.configure(this, 2, 9, \"RW\", 0, 0, 1, 1, 0);\n    f = uvm_reg_field::type_id::create(\"f\"); f.configure(this, 1, 10, \"RW\", 0, 0, 1, 1, 0);\n  endfunction\nendclass\nclass top_block extends uvm_reg_block;\n  rand r_t r;\n  function new(string name = \"top_block\"); super.new(name); endfunction\n  virtual function void build();\n    default_map = create_map(\"map\", 0, 4, UVM_LITTLE_ENDIAN);\n    for (int i = 0; i < 1; i++) {ifs};\n    r = r_t::type_id::create(\"r\");\n    r.configure(this, null, \"\");\n    r.build();\n    default_map.add_reg(r, 'h4, \"RW\");\n  endfunction\nendclass\n"
    );
    let (a, _) = project("review3ral", &[("ral.sv", &src)]);
    let (blocks, _) = a.register_blocks();
    let model = a.register_model(*blocks.iter().find(|b| a.decl(**b).name == "top_block").unwrap());
    let r = model.regs.iter().position(|r| r.name == "r").expect("the register after the deep loop");
    let f: Vec<String> = a.register_fields(&model, r).iter().map(|f| format!("{} {}", f.name, f.notes.iter().any(|n| n == "overlaps another field"))).collect();
    assert_eq!(f, ["a true", "b true", "c false", "e true", "f true"]);
}

#[test]
fn override_text_reads_a_specialization_with_space_after_the_hash() {
    let src = "virtual class base #(type T = int);\n  pure virtual function void f(T x);\nendclass\nclass child extends base # (byte);\nendclass\n";
    let (a, _) = project("review3ovr", &[("t.sv", src)]);
    let class = {
        let u = a.unit(a.units_for(a.project_files()[0])[0]);
        let d = u.parsed.decls.iter().position(|d| d.name == "child").unwrap();
        dvi_analysis::DeclRef { unit: u.id, decl: d as u32 }
    };
    let os = a.overridable_methods(class);
    let text = os.iter().find(|o| o.name == "f").and_then(|o| a.override_text(o, class, "  ", "\n", false));
    assert_eq!(text.as_deref(), Some("virtual function void f(byte x);\nendfunction"));
}

#[test]
fn rename_refuses_a_shared_header_use_that_means_another_declaration_elsewhere() {
    let pkgs = "package p;\n  int value;\nendpackage\npackage q;\n  int value;\nendpackage\n";
    let hdr = "function int f();\n  return value;\nendfunction\n";
    let ma = "module a;\n  import p::*;\n  `include \"f.svh\"\nendmodule\n";
    let mb = "module b;\n  import q::*;\n  `include \"f.svh\"\nendmodule\n";
    let (a, root) = project("review3rename", &[("pkgs.sv", pkgs), ("f.svh", hdr), ("a.sv", ma), ("b.sv", mb)]);
    let f = a.vfs.lookup(&root.join("pkgs.sv")).unwrap();
    let e = a.rename(f, pkgs.find("value").unwrap() as u32, "v2").unwrap_err();
    assert!(e.contains("another declaration"), "{e}");
    // Only p's own uses: renamed.
    let solo = "package p;\n  int value;\n  function int g(); return value; endfunction\nendpackage\n";
    let (a, root) = project("review3rename2", &[("p.sv", solo)]);
    let f = a.vfs.lookup(&root.join("p.sv")).unwrap();
    assert!(a.rename(f, solo.find("value").unwrap() as u32, "v2").is_ok());
}

#[test]
fn rename_of_a_virtual_method_renames_its_overrides() {
    let src = "class B;\n  virtual function int f(); return 1; endfunction\nendclass\nclass D extends B;\n  virtual function int f(); return 2; endfunction\nendclass\nmodule top;\n  initial begin\n    B b;\n    D d;\n    d = new;\n    b = d;\n    $display(b.f(), d.f());\n  end\nendmodule\n";
    let (a, root) = project("review3virt", &[("t.sv", src)]);
    let f = a.vfs.lookup(&root.join("t.sv")).unwrap();
    let edit = a.rename(f, src.find("f()").unwrap() as u32, "g").unwrap();
    let Some(lsp_types::DocumentChanges::Edits(docs)) = edit.document_changes else { panic!() };
    assert_eq!(docs[0].edits.len(), 4, "B::f, D::f and both calls");
}

#[test]
fn rename_refuses_macro_and_named_argument_uses_that_mean_something_else_elsewhere() {
    let hdr = "localparam X = `M;\n";
    let ma = "`define M 1\nmodule a;\n  `include \"h.svh\"\nendmodule\n";
    let mb = "`undef M\n`define M 2\nmodule b;\n  `include \"h.svh\"\nendmodule\n";
    let (a, root) = project("review3macro", &[("h.svh", hdr), ("a.sv", ma), ("b.sv", mb)]);
    let f = a.vfs.lookup(&root.join("a.sv")).unwrap();
    let e = a.rename(f, ma.find("M 1").unwrap() as u32, "N").unwrap_err();
    assert!(e.contains("another definition"), "{e}");
    let hdr = "initial f(.value(1));\n";
    let ma = "module a;\n  function void f(int value); endfunction\n  `include \"h.svh\"\nendmodule\n";
    let mb = "module b;\n  function void f(int value); endfunction\n  `include \"h.svh\"\nendmodule\n";
    let (a, root) = project("review3named", &[("h.svh", hdr), ("a.sv", ma), ("b.sv", mb)]);
    let f = a.vfs.lookup(&root.join("a.sv")).unwrap();
    let e = a.rename(f, ma.find("value").unwrap() as u32, "v2").unwrap_err();
    assert!(e.contains("another declaration"), "{e}");
}

#[test]
fn override_text_keeps_formals_and_string_arguments() {
    let src = "virtual class base #(string S = \"\", int N = 1);\n  pure virtual function void f(int N, bit [N-1:0] x);\n  pure virtual function void g(bit [N-1:0] y);\nendclass\nclass child extends base #(\"a,b\", 8);\nendclass\n";
    let (a, _) = project("review3ovr2", &[("t.sv", src)]);
    let class = {
        let u = a.unit(a.units_for(a.project_files()[0])[0]);
        let d = u.parsed.decls.iter().position(|d| d.name == "child").unwrap();
        dvi_analysis::DeclRef { unit: u.id, decl: d as u32 }
    };
    let os = a.overridable_methods(class);
    let text = |n: &str| os.iter().find(|o| o.name == n).and_then(|o| a.override_text(o, class, "  ", "\n", false));
    assert_eq!(text("f").as_deref(), Some("virtual function void f(int N, bit [N-1:0] x);\nendfunction"), "a formal named as the parameter");
    assert_eq!(text("g").as_deref(), Some("virtual function void g(bit [8-1:0] y);\nendfunction"), "the string's comma does not split it");
}

#[test]
fn register_helpers_under_a_condition_and_else_branches_in_loops() {
    let src = "virtual class uvm_reg_block; endclass\nvirtual class uvm_reg; endclass\nclass uvm_reg_map; endclass\nclass r_t extends uvm_reg;\n  function new(string name = \"r_t\"); super.new(name, 32, 0); endfunction\nendclass\nclass top_block extends uvm_reg_block;\n  rand r_t r[2];\n  rand r_t z;\n  bit pre;\n  function new(string name = \"top_block\"); super.new(name); endfunction\n  function void freeze(); lock_model(); endfunction\n  virtual function void build();\n    default_map = create_map(\"map\", 0, 4, UVM_LITTLE_ENDIAN);\n    if (pre) freeze();\n    for (int i = 0; i < 2; i++)\n      if (i == 0) r[i] = r_t::type_id::create(\"ra\");\n      else r[i] = r_t::type_id::create(\"rb\");\n    z = r_t::type_id::create(\"z\");\n    z.configure(this, null, \"\");\n    default_map.add_reg(z, 'h8, \"RW\");\n  endfunction\nendclass\n";
    let (a, _) = project("review3helper", &[("ral.sv", src)]);
    let (blocks, _) = a.register_blocks();
    let model = a.register_model(blocks[0]);
    let names: Vec<&str> = model.regs.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"z"), "a lock in a helper called under a condition does not stop the model: {names:?}");
    assert!(!names.iter().any(|n| n.contains('*')), "the else branch is in the loop: {names:?}");
}

#[test]
fn a_macro_preview_uses_the_definition_of_the_call() {
    let src = "`define V 1\nmodule m;\n  localparam A = `V;\nendmodule\n`undef V\n`define V 2\n";
    let (a, _) = project("review3pp", &[("t.sv", src)]);
    let uid = a.units_for(a.project_files()[0])[0];
    let idx = a.unit(uid).macro_uses.iter().position(|m| m.name == "V").unwrap();
    assert_eq!(a.expand_macro_use(uid, idx, 1).as_deref(), Some("1"));
}

#[test]
fn rename_review_round_three_cases() {
    // A shared call through a base handle in one module and a derived one in another: both are
    // the family renamed, not a conflict.
    let cls = "class B;\n  virtual function void f(); endfunction\nendclass\nclass D extends B;\n  virtual function void f(); endfunction\nendclass\n";
    let hdr = "initial obj.f();\n";
    let ma = "module a;\n  B obj;\n  `include \"h.svh\"\nendmodule\n";
    let mb = "module b;\n  D obj;\n  `include \"h.svh\"\nendmodule\n";
    let (a, root) = project("review3fam", &[("c.sv", cls), ("h.svh", hdr), ("a.sv", ma), ("b.sv", mb)]);
    let f = a.vfs.lookup(&root.join("c.sv")).unwrap();
    assert!(a.rename(f, cls.find("f()").unwrap() as u32, "g").is_ok());
    // `ifdef M in a shared header, M defined differently by its two includers.
    let hdr = "`ifdef M\nlocalparam X = 1;\n`endif\n";
    let ma = "`define M\nmodule a;\n  `include \"h.svh\"\nendmodule\n";
    let mb = "`undef M\n`define M\nmodule b;\n  `include \"h.svh\"\nendmodule\n";
    let (a, root) = project("review3ifdef", &[("h.svh", hdr), ("a.sv", ma), ("b.sv", mb)]);
    let f = a.vfs.lookup(&root.join("a.sv")).unwrap();
    let e = a.rename(f, ma.find("M\n").unwrap() as u32, "N").unwrap_err();
    assert!(e.contains("another definition"), "{e}");
}

#[test]
fn review_round_three_generated_text_and_values() {
    let src = "package p;\n  typedef int T;\nendpackage\nvirtual class B #(type T = byte);\n  pure virtual function void f(p::T x, T y);\nendclass\nclass D extends B #(int);\nendclass\n";
    let (a, _) = project("review3qual", &[("t.sv", src)]);
    let class = {
        let u = a.unit(a.units_for(a.project_files()[0])[0]);
        let d = u.parsed.decls.iter().position(|d| d.name == "D").unwrap();
        dvi_analysis::DeclRef { unit: u.id, decl: d as u32 }
    };
    let os = a.overridable_methods(class);
    let text = os.iter().find(|o| o.name == "f").and_then(|o| a.override_text(o, class, "  ", "\n", false));
    assert_eq!(text.as_deref(), Some("virtual function void f(p::T x, int y);\nendfunction"), "a qualified name is another scope's");
    assert_eq!(dvi_analysis::ide::instances::ident("wire"), "\\wire ", "a keyword as a name is escaped");
    assert_eq!(dvi_analysis::ide::instances::ident("w1"), "w1");
    // Replications of different operands evaluate each one.
    let src = "module top;\n  localparam bit [1:0] A = 1;\n  localparam bit [1:0] B = 2;\n  localparam P = {2{A}} | {2{B}};\nendmodule\n";
    let (a, _) = project("review3repl", &[("t.sv", src)]);
    let (t, _) = a.design_tree(Some("top"), &dvi_analysis::ide::views::TreeQuery::new(100));
    let p = t[0].params.iter().find(|x| x.name == "P").and_then(|x| x.value.clone());
    assert_eq!(p.as_deref(), Some("15"));
}

#[test]
fn review_round_three_registers_and_drivers() {
    let src = "virtual class uvm_reg_block; endclass\nvirtual class uvm_reg; endclass\nclass uvm_reg_map; endclass\nclass r_t extends uvm_reg;\n  function new(string name = \"r_t\"); super.new(name, 32, 0); endfunction\nendclass\nclass base_block extends uvm_reg_block;\n  function new(string name = \"base_block\"); super.new(name); endfunction\n  function void freeze(); lock_model(); endfunction\nendclass\nclass top_block extends base_block;\n  rand r_t z;\n  bit pre;\n  function new(string name = \"top_block\"); super.new(name); endfunction\n  virtual function void build();\n    default_map = create_map(\"map\", 0, 4, UVM_LITTLE_ENDIAN);\n    if (pre) super.freeze();\n    z = r_t::type_id::create(\"z\");\n    z.configure(this, null, \"\");\n    default_map.add_reg(z, 'h8, \"RW\");\n  endfunction\nendclass\n";
    let (a, _) = project("review3super", &[("ral.sv", src)]);
    let (blocks, _) = a.register_blocks();
    let top = *blocks.iter().find(|b| a.decl(**b).name == "top_block").unwrap();
    let model = a.register_model(top);
    assert!(model.regs.iter().any(|r| r.name == "z"), "a lock in a super helper under a condition does not stop the model");
    let src = "class C;\n  int x;\nendclass\nmodule top;\n  C c;\n  function void fill(output int v); v = 1; endfunction\n  initial fill(c.x);\nendmodule\n";
    let (a, root) = project("review3drv", &[("t.sv", src)]);
    let f = a.vfs.lookup(&root.join("t.sv")).unwrap();
    let (writes, _, _) = access_lines(&a, f, word_at(src, "C c", "c"), true);
    assert!(writes.is_empty(), "the call writes c.x, not the handle: {writes:?}");
    let (reads, _, _) = access_lines(&a, f, word_at(src, "C c", "c"), false);
    assert!(reads.iter().any(|l| l.contains("argument")), "the handle is read: {reads:?}");
}

#[test]
fn macro_actuals_are_expanded_before_pasting_and_quoted_when_they_hold_backtick_quotes() {
    // GitHub issue 13: both compile with VCS.
    let src = "`define TC        demo\n`define CFG(tc)   tc``_cfg\n`define LOG(MSG)  $display(\"%s\", MSG)\n\nclass `TC; endclass\nclass `CFG(direct); endclass\nclass `CFG(`TC); endclass\n\nmodule m;\n  class demo2; function void run(); endfunction endclass\n  initial begin\n    `LOG(\"`TC run(): Finished!\");\n    `LOG(`\"`TC run(): Finished!`\");\n  end\nendmodule\n";
    let (a, _) = project("issue13", &[("t.sv", src)]);
    assert!(codes(&a, "t.sv").is_empty(), "{:?}", codes(&a, "t.sv"));
    let u = a.unit(a.units_for(a.project_files()[0])[0]);
    assert!(u.parsed.decls.iter().any(|d| d.name == "demo_cfg"), "`CFG(`TC) declares demo_cfg");
    let uid = u.id;
    let idx = u.macro_uses.iter().rposition(|m| m.name == "LOG").unwrap();
    let e = a.expand_macro_use(uid, idx, usize::MAX).unwrap();
    assert!(e.contains("\"demo run(): Finished!\""), "{e}");
}

#[test]
fn backtick_quotes_keep_slashes_paste_and_escaped_names() {
    let src = "`define S(x) `\"x // not a comment`\"\n`define VIF(x) `\"x``_vif`\"\n`define T(type) `\"type`\"\n`define E(x) int x ;\nmodule m;\n  string a = `S(p);\n  string b = `VIF(bus);\n  string c = `T(int);\n  `E(\\a.b )\n  initial \\a.b = 1;\nendmodule\n";
    let (a, _) = project("ppquote", &[("t.sv", src)]);
    assert!(codes(&a, "t.sv").is_empty(), "{:?}", codes(&a, "t.sv"));
    let uid = a.units_for(a.project_files()[0])[0];
    let exp = |name: &str| {
        let idx = a.unit(uid).macro_uses.iter().position(|m| m.name == name).unwrap();
        a.expand_macro_use(uid, idx, usize::MAX).unwrap()
    };
    assert_eq!(exp("S"), "\"p // not a comment\"", "`//` inside `\"...`\" is text");
    assert_eq!(exp("VIF"), "\"bus_vif\"", "`` inside `\"...`\" pastes");
    assert_eq!(exp("T"), "\"int\"", "a keyword-named formal inside `\"...`\"");
    assert_eq!(exp("E"), "int \\a.b ;", "an escaped name keeps its backslash and space");
}

#[test]
fn a_guarded_include_cycle_is_not_recursive_and_an_unguarded_one_is() {
    let a14 = "`ifndef A_SVH\n`define A_SVH\nint a1;\n`include \"b.svh\"\n`endif\n";
    let b14 = "`ifndef B_SVH\n`define B_SVH\nint b1;\n`include \"a.svh\"\n`endif\n";
    let top = "module m;\n  `include \"a.svh\"\n  initial begin a1 = 1; b1 = 1; end\nendmodule\n";
    let (a, _) = project("ppguard", &[("a.svh", a14), ("b.svh", b14), ("t.sv", top)]);
    assert!(codes(&a, "t.sv").is_empty(), "{:?}", codes(&a, "t.sv"));
    let all: Vec<&str> = a.project_files().into_iter().flat_map(|f| a.file_diagnostics(f)).map(|d| d.code).collect();
    assert!(!all.contains(&"RECURSIVE_INCLUDE"), "{all:?}");
    let c = "int c1;\n`include \"d.svh\"\n";
    let d = "int d1;\n`include \"c.svh\"\n";
    let top = "module m;\n  `include \"c.svh\"\nendmodule\n";
    let (a, _) = project("ppcycle", &[("c.svh", c), ("d.svh", d), ("t.sv", top)]);
    let all: Vec<&str> = a.project_files().into_iter().flat_map(|f| a.file_diagnostics(f)).map(|d| d.code).collect();
    assert!(all.contains(&"RECURSIVE_INCLUDE"), "an unguarded cycle is still reported: {all:?}");
}
