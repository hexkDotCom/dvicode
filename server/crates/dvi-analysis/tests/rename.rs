//! Rename and quick fixes, applied and compiled again (found by `dvicode-server check
//! --write-check` on real projects).

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use dvi_syntax::{FileId, LineIndex, SourceProvider};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn project_at(root: &Path, files: &[(&str, &str)], build: &str) -> Analysis {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    for (f, text) in files {
        let p = root.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
    let cfg = dvi_project::load(root, None, &FxHashMap::default());
    assert!(cfg.problems.is_empty(), "{:?}", cfg.problems);
    Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(root, cfg, FxHashMap::default())))
}

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-rename-{name}-{}", std::process::id()));
    let build: String = files.iter().filter(|(f, _)| f.ends_with(".sv")).map(|(f, _)| format!("{f}\n")).collect();
    (project_at(&root, files, &build), root)
}

/// (file, offset) of the first `needle` in `file`, plus `skip` bytes.
fn at(a: &Analysis, root: &Path, file: &str, needle: &str, skip: u32) -> (FileId, u32) {
    let f = a.vfs.file_id(&root.join(file));
    let off = a.file_text(f).unwrap().find(needle).unwrap_or_else(|| panic!("no {needle} in {file}")) as u32;
    (f, off + skip)
}

/// The texts after applying a workspace edit, by file name.
fn apply(a: &Analysis, edit: lsp_types::WorkspaceEdit) -> BTreeMap<String, String> {
    let Some(lsp_types::DocumentChanges::Edits(docs)) = edit.document_changes else { panic!("expected document changes") };
    let mut out = BTreeMap::new();
    for d in docs {
        let path = d.text_document.uri.to_file_path().unwrap();
        let text = a.vfs.text(a.vfs.lookup(&path).unwrap()).unwrap();
        let li = LineIndex::new(&text);
        let mut edits: Vec<(usize, usize, String)> = d
            .edits
            .into_iter()
            .map(|e| {
                let t = match e {
                    lsp_types::OneOf::Left(t) => t,
                    lsp_types::OneOf::Right(a) => a.text_edit,
                };
                (li.offset(&text, t.range.start.line, t.range.start.character) as usize, li.offset(&text, t.range.end.line, t.range.end.character) as usize, t.new_text)
            })
            .collect();
        edits.sort();
        let mut s = text.to_string();
        for (b, e, t) in edits.into_iter().rev() {
            s.replace_range(b..e, &t);
        }
        out.insert(path.file_name().unwrap().to_string_lossy().into_owned(), s);
    }
    out
}

/// Problem codes of every file after replacing texts.
fn codes_after(a: &Analysis, root: &Path, texts: &BTreeMap<String, String>) -> Vec<&'static str> {
    for (f, t) in texts {
        a.vfs.set_overlay(&root.join(f), Arc::from(t.as_str()));
    }
    let b = Analysis::build(a.vfs.clone(), a.setup.clone());
    let mut v: Vec<&'static str> = b.project_files().into_iter().flat_map(|f| b.file_diagnostics(f)).map(|d| d.code).collect();
    v.sort();
    v
}

const SUB: &str = "module sub(input logic clk_i, output logic q_o);\n  assign q_o = clk_i;\nendmodule\n";
const TOP: &str = "module top;\n  logic clk_i, q;\n  sub u(.clk_i, .q_o(q));\nendmodule\n";

#[test]
fn an_implicit_port_connection_becomes_explicit() {
    let (a, root) = project("implicit", &[("sub.sv", SUB), ("top.sv", TOP)]);
    // The local signal: `.clk_i` -> `.clk_i(clock)`.
    let (f, off) = at(&a, &root, "top.sv", "clk_i,", 0);
    let texts = apply(&a, a.rename(f, off, "clock").unwrap());
    assert_eq!(texts["top.sv"], "module top;\n  logic clock, q;\n  sub u(.clk_i(clock), .q_o(q));\nendmodule\n");
    assert!(!texts.contains_key("sub.sv"));
    assert!(codes_after(&a, &root, &texts).is_empty());
    // The port: `.clk_i` -> `.clk(clk_i)`.
    let (a, root) = project("implicit2", &[("sub.sv", SUB), ("top.sv", TOP)]);
    let (f, off) = at(&a, &root, "sub.sv", "clk_i", 0);
    let texts = apply(&a, a.rename(f, off, "clk").unwrap());
    assert_eq!(texts["top.sv"], "module top;\n  logic clk_i, q;\n  sub u(.clk(clk_i), .q_o(q));\nendmodule\n");
    assert_eq!(texts["sub.sv"], SUB.replace("clk_i", "clk"));
    assert!(codes_after(&a, &root, &texts).is_empty());
}

#[test]
fn a_member_after_a_dot_is_not_a_port_connection() {
    let p = "package p;\n  class item;\n    int addr;\n  endclass\n  function automatic int f(item it);\n    return it.addr;\n  endfunction\nendpackage\n";
    let (a, root) = project("member", &[("p.sv", p)]);
    let (f, off) = at(&a, &root, "p.sv", "addr;", 0);
    let texts = apply(&a, a.rename(f, off, "address").unwrap());
    assert_eq!(texts["p.sv"], p.replace("addr", "address"));
}

#[test]
fn macro_rename_reaches_other_macros_bodies_and_arguments() {
    let defs = "`define CHECK(c) if (!(c)) $error(\"bad\")\n`define CHECK_EQ(a, b) `CHECK((a) == (b))\n`define VAL 1\n";
    let t = "`include \"defs.svh\"\nmodule t;\n  initial `CHECK_EQ(`VAL, 1);\n  initial `CHECK(`VAL);\nendmodule\n";
    let (a, root) = project("macrobody", &[("defs.svh", defs), ("t.sv", t)]);
    // A use in the arguments of another macro is navigable.
    let (f, off) = at(&a, &root, "t.sv", "`VAL", 1);
    let d: Vec<u32> = a.goto_definition(f, off).iter().map(|l| l.range.start.line + 1).collect();
    assert_eq!(d, vec![3]);
    let texts = apply(&a, a.rename(f, off, "VALUE").unwrap());
    assert_eq!(texts["t.sv"], t.replace("`VAL", "`VALUE"));
    assert_eq!(texts["defs.svh"], defs.replace("define VAL", "define VALUE"));
    // `CHECK is also used inside `CHECK_EQ: its body is renamed too.
    let (f, off) = at(&a, &root, "t.sv", "`CHECK(", 1);
    let texts = apply(&a, a.rename(f, off, "ASSERT").unwrap());
    assert_eq!(texts["defs.svh"], defs.replace("CHECK(c)", "ASSERT(c)").replace("`CHECK((a)", "`ASSERT((a)"));
    assert_eq!(texts["t.sv"], t.replace("`CHECK(`", "`ASSERT(`"));
    assert!(codes_after(&a, &root, &texts).is_empty());
}

#[test]
fn macro_rename_reaches_conditional_directives() {
    let h = "`ifndef H_SVH\n`define H_SVH\n`define FEATURE\n`endif\n";
    let t = "`include \"h.svh\"\n`include \"h.svh\"\nmodule t;\n`ifdef FEATURE\n  int with_feature;\n`elsif OTHER\n`endif\n`undef FEATURE\nendmodule\n";
    let (a, root) = project("ifdefs", &[("h.svh", h), ("t.sv", t)]);
    // From `ifdef to the definition, and back.
    let (f, off) = at(&a, &root, "t.sv", "FEATURE", 0);
    let d: Vec<u32> = a.goto_definition(f, off).iter().map(|l| l.range.start.line + 1).collect();
    assert_eq!(d, vec![3]);
    let texts = apply(&a, a.rename(f, off, "HAS_FEATURE").unwrap());
    assert_eq!(texts["t.sv"], t.replace("FEATURE", "HAS_FEATURE"));
    assert_eq!(texts["h.svh"], h.replace("FEATURE", "HAS_FEATURE"));
    // An include guard: the `ifndef runs before the `define.
    let (f, off) = at(&a, &root, "h.svh", "H_SVH", 0);
    let texts = apply(&a, a.rename(f, off, "GUARD").unwrap());
    assert_eq!(texts["h.svh"], h.replace("H_SVH", "GUARD"));
    assert!(codes_after(&a, &root, &texts).is_empty());
}

#[test]
fn renames_that_would_break_code_are_refused() {
    // A name used inside a macro body: the macro cannot be changed for one of its uses.
    let t = "`define CLK_EDGE @(posedge clk)\nmodule t;\n  logic clk;\n  initial `CLK_EDGE;\nendmodule\n";
    let (a, root) = project("refuse", &[("t.sv", t)]);
    let (f, off) = at(&a, &root, "t.sv", "clk;", 0);
    let e = a.rename(f, off, "clock").unwrap_err();
    assert!(e.contains("inside the macro `CLK_EDGE"), "{e}");
    // A macro defined in two places and used inside another macro: either could be meant.
    let (a, root) = project(
        "refuse2",
        &[
            ("d1.svh", "`define W 1\n`define W2 (`W * 2)\n"),
            ("a.sv", "`include \"d1.svh\"\nmodule a;\n  int x = `W2;\nendmodule\n"),
            ("b.sv", "`undef W\n`define W 3\nmodule b;\n  int y = `W;\nendmodule\n"),
        ],
    );
    let (f, off) = at(&a, &root, "b.sv", "`W;", 1);
    let e = a.rename(f, off, "V").unwrap_err();
    assert!(e.contains("more than one place"), "{e}");
    // A macro called through a pasted name (`dv_``SEV_): the pasted text cannot follow.
    let t = "`define dv_fatal(m) $fatal(1, m)\n`define dv_error(m) $error(m)\n`define CHECK(c, SEV_) if (!(c)) `dv_``SEV_(\"bad\")\nmodule t;\n  initial `CHECK(1, fatal);\n  initial `dv_error(\"x\");\nendmodule\n";
    let (a, root) = project("pasted", &[("t.sv", t)]);
    let (f, off) = at(&a, &root, "t.sv", "dv_fatal", 0);
    let e = a.rename(f, off, "dv_die").unwrap_err();
    assert!(e.contains("token pasting"), "{e}");
    let (f, off) = at(&a, &root, "t.sv", "`dv_error(\"x", 1);
    assert!(a.rename(f, off, "dv_err").is_ok(), "never pasted");
    // Without a use inside another macro, each definition renames its own uses.
    let (a, root) = project(
        "refuse3",
        &[("a.sv", "`define W 1\nmodule a;\n  int x = `W;\nendmodule\n"), ("b.sv", "`undef W\n`define W 3\nmodule b;\n  int y = `W;\nendmodule\n")],
    );
    let (f, off) = at(&a, &root, "b.sv", "`W;", 1);
    let texts = apply(&a, a.rename(f, off, "V").unwrap());
    assert_eq!(texts.keys().collect::<Vec<_>>(), vec!["b.sv"]);
    assert_eq!(texts["b.sv"], "`undef W\n`define V 3\nmodule b;\n  int y = `V;\nendmodule\n");
    // A port connected with `.*`, and the signal it takes: the connection names neither.
    let t = "module sub(input logic a, input logic c);\nendmodule\nmodule top;\n  logic a, b, c;\n  sub u (.*);\n  sub v (.a(b), .c(b));\nendmodule\nmodule other;\n  logic a;\nendmodule\n";
    let (a, root) = project("dotstar", &[("t.sv", t)]);
    for (needle, k) in [("a, input", 0), ("a, b, c", 0)] {
        let (f, off) = at(&a, &root, "t.sv", needle, k);
        let e = a.rename(f, off, "a2").unwrap_err();
        assert!(e.contains("`.*` of the instance at") && e.contains("t.sv:5"), "{e}");
    }
    // A signal no port takes, and one of the same name elsewhere, rename.
    let (f, off) = at(&a, &root, "t.sv", "b, c", 0);
    assert!(a.rename(f, off, "b2").is_ok());
    let (f, off) = at(&a, &root, "t.sv", "a;\nendmodule\n", 0);
    assert!(a.rename(f, off, "a2").is_ok());
}

#[test]
fn rename_never_changes_files_outside_the_workspace() {
    let base = std::env::temp_dir().join(format!("dvicode-rename-outside-{}", std::process::id()));
    let root = base.join("ws");
    std::fs::create_dir_all(base.join("lib")).unwrap();
    std::fs::write(base.join("lib/lib_pkg.sv"), "package lib_pkg;\n  typedef int word_t;\nendpackage\n").unwrap();
    let a = project_at(&root, &[("top.sv", "module top;\n  lib_pkg::word_t w;\n  int mine;\n  initial mine = w;\nendmodule\n")], "../lib/lib_pkg.sv\ntop.sv\n");
    let (f, off) = at(&a, &root, "top.sv", "word_t", 0);
    assert!(a.prepare_rename(f, off).is_none(), "declared in a library outside the workspace");
    assert!(a.rename(f, off, "w32_t").is_err());
    let (f, off) = at(&a, &root, "top.sv", "mine;", 0);
    assert!(a.rename(f, off, "own").is_ok());
    // Open in the editor, the library file may be changed.
    let lib = base.join("lib/lib_pkg.sv");
    let id = a.vfs.set_overlay(&lib, a.vfs.text(a.vfs.lookup(&lib).unwrap()).unwrap());
    let a = a.update(&[id]);
    let (f, off) = at(&a, &root, "top.sv", "word_t", 0);
    assert!(a.rename(f, off, "w32_t").is_ok());
}

/// Titles of the quick fixes at `needle` in `file`.
fn fixes(a: &Analysis, root: &Path, file: &str, needle: &str) -> Vec<String> {
    let (f, off) = at(a, root, file, needle, 0);
    a.quick_fixes(f, off, off + 1).into_iter().map(|q| q.title).collect()
}

#[test]
fn quick_fixes_where_a_type_stands_in_a_value_position() {
    let pkg = "package items_pkg;\n  class my_item;\n  endclass\n  typedef my_item item_q[$];\nendpackage\n";
    let t = "module t;\n  import items_pkg::*;\n  mailbox #(my_itm) mb;\nendmodule\nmodule u;\n  mailbox #(item_q) mb;\nendmodule\nmodule v;\n  item_pkg::my_item x;\nendmodule\n";
    let (a, root) = project("qfix", &[("p.sv", pkg), ("t.sv", t)]);
    // A misspelled class as a parameter value.
    assert!(fixes(&a, &root, "t.sv", "my_itm").contains(&"Did you mean 'my_item'?".to_string()), "{:?}", fixes(&a, &root, "t.sv", "my_itm"));
    // A typedef from a package that is not imported.
    assert!(fixes(&a, &root, "t.sv", "item_q").contains(&"Import package 'items_pkg' (items_pkg::*)".to_string()), "{:?}", fixes(&a, &root, "t.sv", "item_q"));
    // A misspelled package before `::`.
    assert_eq!(fixes(&a, &root, "t.sv", "item_pkg::"), vec!["Did you mean 'items_pkg'?".to_string()]);
}
