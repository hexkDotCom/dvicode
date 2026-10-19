//! Incremental updates that went wrong in long editing sessions (found by
//! `dvicode-server check --fuzz-edits`, which compares every update with a full build).

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use dvi_syntax::{FileId, SourceProvider};
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-incr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    let mut build = String::new();
    for (f, text) in files {
        std::fs::write(root.join(f), text).unwrap();
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

/// New editor contents for `file`, and the file's id.
fn edit(a: &Analysis, root: &Path, file: &str, text: &str) -> FileId {
    a.vfs.set_overlay(&root.join(file), Arc::from(text))
}

/// Lines (1-based) that go to definition offers at the first `needle` in `file`.
fn definition_lines(a: &Analysis, root: &Path, file: &str, needle: &str) -> Vec<(String, u32)> {
    let f = a.vfs.file_id(&root.join(file));
    let off = a.file_text(f).unwrap().find(needle).unwrap_or_else(|| panic!("no {needle} in {file}")) as u32 + 1;
    a.goto_definition(f, off)
        .iter()
        .map(|l| (l.uri.path().rsplit('/').next().unwrap().to_string(), l.range.start.line + 1))
        .collect()
}

const DEFS: &str = "`define W 8\n";
const A: &str = "`include \"defs.svh\"\nmodule a; endmodule\n";
const B: &str = "module b;\n  logic [`W-1:0] x;\nendmodule\n";

#[test]
fn a_moved_define_is_found_at_its_new_place_from_later_files() {
    let (a, root) = project("moveddefine", &[("defs.svh", DEFS), ("a.sv", A), ("b.sv", B)]);
    assert_eq!(definition_lines(&a, &root, "b.sv", "`W"), vec![("defs.svh".into(), 1)]);
    // Two lines above the `define: the macro expands the same, so b.sv is not compiled again,
    // but its use of W must lead to the new line.
    let defs = edit(&a, &root, "defs.svh", "// widths\n\n`define W 8\n");
    let a2 = a.update(&[defs]);
    assert_eq!(definition_lines(&a2, &root, "b.sv", "`W"), vec![("defs.svh".into(), 3)]);
    let fb = a2.vfs.file_id(&root.join("b.sv"));
    let w = a2.file_text(fb).unwrap().find("`W").unwrap() as u32 + 1;
    let refs: Vec<u32> = a2.references(fb, w, true).iter().map(|l| l.range.start.line + 1).collect();
    assert_eq!(refs, vec![3, 2], "the definition and the use in b.sv");
    // b.sv edited later is compiled with the macro state it was pointed at.
    let b = edit(&a2, &root, "b.sv", &format!("// b\n{B}"));
    let a3 = a2.update(&[b]);
    assert_eq!(definition_lines(&a3, &root, "b.sv", "`W"), vec![("defs.svh".into(), 3)]);
    // Both edited at once.
    edit(&a3, &root, "defs.svh", "`define W 8\n");
    edit(&a3, &root, "b.sv", &format!("\n\n{B}"));
    let a4 = a3.update(&[defs, b]);
    assert_eq!(definition_lines(&a4, &root, "b.sv", "`W"), vec![("defs.svh".into(), 1)]);
}

#[test]
fn open_files_keep_or_lose_their_own_unit_as_includes_change() {
    let (a, root) = project("openheader", &[("h.svh", "int from_h;\n"), ("a.sv", "module a;\n`include \"h.svh\"\nendmodule\n")]);
    let h = edit(&a, &root, "h.svh", "int from_h;\n");
    let a1 = a.update(&[h]).sync_open_files(&[h]);
    assert!(!a1.is_adhoc_root(h));
    // a.sv no longer includes the open header: the header is still analysed, on its own.
    let fa = edit(&a1, &root, "a.sv", "module a;\nendmodule\n");
    let a2 = a1.update(&[fa]).sync_open_files(&[h]);
    assert!(a2.is_adhoc_root(h), "the open header has no unit");
    assert_eq!(a2.units_of_file(h).len(), 1);
    // Included again: back to the project's unit only.
    edit(&a2, &root, "a.sv", "module a;\n`include \"h.svh\"\nendmodule\n");
    let a3 = a2.update(&[fa]).sync_open_files(&[h]);
    assert!(!a3.is_adhoc_root(h));
    assert_eq!(a3.units_of_file(h).len(), 1);
    assert!(a3.interface_changed(&a2), "dropping the file's own unit changes what other files see");
}

#[test]
fn files_compiled_again_with_an_edited_header_are_reported() {
    let (a, root) = project(
        "recompiled",
        &[("h.svh", "  typedef enum { A, B } e;\n"), ("p.sv", "package p;\n`include \"h.svh\"\n  int x;\nendpackage\n"), ("q.sv", "module q; endmodule\n")],
    );
    let h = edit(&a, &root, "h.svh", "  typedef enum { A, B } e2;\n");
    let a2 = a.update(&[h]);
    let mut files: Vec<String> = a2.recompiled_files(&a).iter().map(|f| a2.path(*f).file_name().unwrap().to_string_lossy().into_owned()).collect();
    files.sort();
    assert_eq!(files, vec!["h.svh", "p.sv"]);
    // A header that is no longer included is reported too: its problems are gone.
    let p = edit(&a2, &root, "p.sv", "package p;\n  int x;\nendpackage\n");
    let a3 = a2.update(&[p]);
    let mut files: Vec<String> = a3.recompiled_files(&a2).iter().map(|f| a3.path(*f).file_name().unwrap().to_string_lossy().into_owned()).collect();
    files.sort();
    assert_eq!(files, vec!["h.svh", "p.sv"]);
    // Units only pointed at a moved macro definition were not compiled again.
    let (a, root) = project("relocated", &[("defs.svh", DEFS), ("a.sv", A), ("b.sv", B)]);
    let defs = edit(&a, &root, "defs.svh", "\n`define W 8\n");
    let a2 = a.update(&[defs]);
    let mut files: Vec<String> = a2.recompiled_files(&a).iter().map(|f| a2.path(*f).file_name().unwrap().to_string_lossy().into_owned()).collect();
    files.sort();
    assert_eq!(files, vec!["a.sv", "defs.svh"]);
}
