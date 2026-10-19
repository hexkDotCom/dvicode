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
