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
