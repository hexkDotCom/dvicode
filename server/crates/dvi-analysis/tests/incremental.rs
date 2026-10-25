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
        std::fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
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
fn macros_flow_to_later_files_of_the_same_invocation() {
    let b = "module b;\n  logic [`W-1:0] x;\n  sub u(.a(x[0]));\nendmodule\n";
    let (a, root) = project("macrosflow", &[("defs.svh", DEFS), ("a.sv", A), ("b.sv", b), ("c.sv", "module c; endmodule\n")]);
    // b.sv needs two library files; c.sv is in an invocation of its own.
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/sub.v"), "module sub(input a);\n  leaf l(.x(a));\nendmodule\n").unwrap();
    std::fs::write(root.join("lib/leaf.v"), "module leaf(input x);\nendmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "-y lib\n+libext+.v\na.sv\nb.sv\n+dvicode_init\nc.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(a.vfs.clone(), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let f = |name: &str| a.vfs.file_id(&root.join(name));
    assert!(a.macros_flow(f("a.sv"), f("b.sv")));
    assert!(a.macros_flow(f("defs.svh"), f("b.sv")), "a header of an earlier file");
    assert!(!a.macros_flow(f("b.sv"), f("a.sv")), "a later file");
    assert!(!a.macros_flow(f("b.sv"), f("defs.svh")));
    assert!(!a.macros_flow(f("a.sv"), f("c.sv")), "another invocation");
    assert!(!a.macros_flow(f("a.sv"), f("a.sv")));
    // Library units come after the invocation's files, in an order their numbers do not tell.
    assert!(a.macros_flow(f("b.sv"), f("lib/sub.v")));
    assert!(!a.macros_flow(f("lib/sub.v"), f("b.sv")));
    assert!(a.macros_flow(f("lib/sub.v"), f("lib/leaf.v")) && a.macros_flow(f("lib/leaf.v"), f("lib/sub.v")));
    assert!(!a.macros_flow(f("lib/sub.v"), f("lib/sub.v")));
    assert!(!a.macros_flow(f("lib/sub.v"), f("c.sv")));
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

/// Definition lines and files a reference leads to, as text.
fn targets(a: &Analysis, root: &Path, file: &str, needle: &str) -> Vec<String> {
    definition_lines(a, root, file, needle).into_iter().map(|(f, l)| format!("{f}:{l}")).collect()
}

#[test]
fn open_files_outside_the_build_rank_by_path_not_by_opening_order() {
    // Two open files declare the same function at compilation-unit level.
    let top = "module top;\n  initial helper();\nendmodule\n";
    let mut seen = Vec::new();
    for (i, order) in [["a_scratch.sv", "b_scratch.sv"], ["b_scratch.sv", "a_scratch.sv"]].iter().enumerate() {
        let (a, root) = project(&format!("adhocorder{i}"), &[("top.sv", top)]);
        let mut cur = a;
        for f in order {
            let id = edit(&cur, &root, f, "function automatic void helper(); endfunction\n");
            cur = cur.sync_open_files(&[id]);
        }
        seen.push(targets(&cur, &root, "top.sv", "helper"));
    }
    assert_eq!(seen[0], vec!["a_scratch.sv:1".to_string()]);
    assert_eq!(seen[0], seen[1]);
}

#[test]
fn an_open_header_included_by_another_open_file_keeps_its_own_unit() {
    for (i, first_header) in [true, false].iter().enumerate() {
        let (a, root) = project(&format!("nested{i}"), &[("top.sv", "module top; endmodule\n")]);
        let h = edit(&a, &root, "h.svh", "  int from_h;\n");
        let x = edit(&a, &root, "x.sv", "module x;\n`include \"h.svh\"\nendmodule\n");
        let open = if *first_header { [h, x] } else { [x, h] };
        let b = a.sync_open_files(&open);
        assert!(b.is_adhoc_root(h) && b.is_adhoc_root(x), "order {i}");
        // Its own unit comes first for its problems and navigation.
        assert_eq!(b.unit(b.units_for(h)[0]).root, h);
    }
}

#[test]
fn library_units_follow_the_instances_that_need_them() {
    let root = std::env::temp_dir().join(format!("dvicode-incr-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    std::fs::write(root.join("lib/sub.v"), "module sub(input a);\n  leaf l(.x(a));\nendmodule\n").unwrap();
    std::fs::write(root.join("lib/leaf.v"), "module leaf(input x);\nendmodule\n").unwrap();
    let with = "module top;\n  logic s;\n  sub u(.a(s));\nendmodule\n";
    std::fs::write(root.join("top.sv"), with).unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "-y lib\n+libext+.v\ntop.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let names = |a: &Analysis| {
        let mut v: Vec<String> = a.project_files().iter().map(|f| a.path(*f).file_name().unwrap().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    };
    assert_eq!(names(&a), vec!["leaf.v", "sub.v", "top.sv"]);
    // The instance goes: so do sub.v and, through it, leaf.v.
    let top = edit(&a, &root, "top.sv", "module top;\n  logic s;\nendmodule\n");
    let b = a.update(&[top]);
    assert_eq!(names(&b), vec!["top.sv"]);
    // And comes back.
    edit(&b, &root, "top.sv", with);
    let c = b.update(&[top]);
    assert_eq!(names(&c), vec!["leaf.v", "sub.v", "top.sv"]);
    assert_eq!(targets(&c, &root, "top.sv", "sub u"), vec!["sub.v:1".to_string()]);
    // An open library file that is needed has no unit of its own; once it is not, it does.
    let leaf = edit(&c, &root, "lib/leaf.v", "module leaf(input x);\nendmodule\n");
    let d = c.update(&[leaf]).sync_open_files(&[leaf]);
    assert!(!d.is_adhoc_root(leaf));
    edit(&d, &root, "top.sv", "module top;\nendmodule\n");
    let e = d.update(&[top]).sync_open_files(&[leaf]);
    assert!(e.is_adhoc_root(leaf));
    assert_eq!(names(&e), vec!["leaf.v", "top.sv"]);
}

#[test]
fn library_units_keep_their_compile_order_after_an_update() {
    let root = std::env::temp_dir().join(format!("dvicode-incr-liborder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    std::fs::write(root.join("lib/a_mod.v"), "`define A_DONE\nmodule a_mod;\nendmodule\n").unwrap();
    std::fs::write(root.join("lib/b_mod.v"), "module b_mod;\nendmodule\n").unwrap();
    std::fs::write(root.join("top.sv"), "module top;\n  a_mod ua();\nendmodule\n").unwrap();
    std::fs::write(root.join("x.sv"), "`ifndef A_DONE\nmodule x; initial undeclared_q = 1; endmodule\n`endif\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "-y lib\n+libext+.v\ntop.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    // b_mod.v now comes first; a_mod.v, compiled after it with the same macros, is kept from
    // before (with the older id).
    let top = edit(&a, &root, "top.sv", "module top;\n  b_mod ub();\n  a_mod ua();\nendmodule\n");
    let b = a.update(&[top]);
    let full = Analysis::build(b.vfs.clone(), b.setup.clone());
    let order = |x: &Analysis| x.library_order.iter().map(|u| x.path(x.unit(*u).root).file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>();
    assert_eq!(order(&b), vec!["b_mod.v", "a_mod.v"]);
    assert_eq!(order(&b), order(&full));
    // A file outside the build sees the macros at the end of the build: after a_mod.v.
    let x = b.vfs.file_id(&root.join("x.sv"));
    for s in [b.add_adhoc(x), full.add_adhoc(x)] {
        assert!(s.file_diagnostics(x).is_empty(), "{:?}", s.file_diagnostics(x).iter().map(|d| &d.message).collect::<Vec<_>>());
    }
}

#[test]
fn a_cancelled_update_gives_no_snapshot() {
    let (a, root) = project("cancelled", &[("defs.svh", DEFS), ("a.sv", A), ("b.sv", B)]);
    let defs = edit(&a, &root, "defs.svh", "`define W 16\n");
    let progress = dvi_project::Progress::new();
    progress.cancel();
    assert!(a.update_with(&[defs], &progress).is_none());
    // One that goes on compiles the edit.
    let fa = a.vfs.file_id(&root.join("a.sv"));
    let unit = |x: &Analysis| x.unit(x.units_of_file(fa)[0]).clone();
    let next = a.update_with(&[defs], &dvi_project::Progress::new()).expect("not cancelled");
    assert!(!Arc::ptr_eq(&unit(&a).parsed, &unit(&next).parsed));
    assert!(next.interface_changed(&a), "W changed");
}

#[test]
fn a_header_that_every_file_includes_updates_like_a_full_build() {
    let header = |w: u32, above: &str| format!("{above}`define W {w}\n  typedef logic [`W-1:0] t_t;\n");
    let file = |i: u32| format!("`include \"h.svh\"\nmodule m{i};\n  logic [`W-1:0] x{i};\nendmodule\n");
    let files = [
        ("h.svh".to_string(), header(8, "")),
        ("f0.sv".to_string(), file(0)),
        ("f1.sv".to_string(), file(1)),
        // Defines W itself, and uses it before and after.
        ("d.sv".to_string(), "module d;\n  logic [`W-1:0] y0;\n`define W 4\n  logic [`W-1:0] y1;\nendmodule\n".to_string()),
        ("f2.sv".to_string(), file(2)),
        // Includes nothing: sees what the last file left.
        ("c.sv".to_string(), "module c;\n  logic [`W-1:0] z;\nendmodule\n".to_string()),
    ];
    let refs: Vec<(&str, &str)> = files.iter().map(|(f, t)| (f.as_str(), t.as_str())).collect();
    let (mut a, root) = project("everyfile", &refs);
    let uses = [("f0.sv", "x0"), ("f1.sv", "x1"), ("d.sv", "y0"), ("d.sv", "y1"), ("f2.sv", "x2"), ("c.sv", "z;")];
    // What each use of W expands to and where it leads, in `x` and in a full build.
    let seen = |x: &Analysis| -> Vec<String> {
        uses.iter()
            .map(|(f, v)| {
                let fid = x.vfs.file_id(&root.join(f));
                let text = x.file_text(fid).unwrap();
                let hover = x.hover(fid, text.find(v).unwrap() as u32 + 1).map(|h| h.0).unwrap_or_default();
                let before = &text[..text.find(v).unwrap()];
                let w = before.rfind("`W").unwrap() as u32 + 1;
                let def: Vec<String> =
                    x.goto_definition(fid, w).iter().map(|l| format!("{}:{}", l.uri.path().rsplit('/').next().unwrap(), l.range.start.line + 1)).collect();
                format!("{f} {v}: {} -> {def:?}", hover.lines().find(|l| l.contains("logic")).unwrap_or(""))
            })
            .collect()
    };
    let h = a.vfs.file_id(&root.join("h.svh"));
    let f1 = a.vfs.file_id(&root.join("f1.sv"));
    let guarded = "`ifndef H\n`define H\n`define W 32\n  typedef logic [`W-1:0] t_t;\n`endif\n".to_string();
    let steps: Vec<(Vec<FileId>, String, Option<String>)> = vec![
        (vec![h], header(8, ""), None),
        (vec![h], header(16, ""), None),
        (vec![h], header(16, "// widths\n\n"), None),
        (vec![h], header(8, ""), None),
        (vec![h, f1], header(12, "\n"), Some(format!("// f1\n{}", file(1)))),
        (vec![h], guarded, None),
        (vec![h], header(8, ""), None),
    ];
    for (i, (changed, text, f1_text)) in steps.into_iter().enumerate() {
        edit(&a, &root, "h.svh", &text);
        if let Some(t) = f1_text {
            edit(&a, &root, "f1.sv", &t);
        }
        a = a.update(&changed);
        let full = Analysis::build(a.vfs.clone(), a.setup.clone());
        assert_eq!(seen(&a), seen(&full), "after edit {i}");
    }
    let last = seen(&a);
    assert!(last[0].contains("[8-1:0]") && last[3].contains("[4-1:0]") && last[5].contains("[8-1:0]"), "{last:?}");
}

#[test]
fn an_update_from_a_pin_compiles_the_editor_texts_of_that_moment() {
    let (a, root) = project("pinned", &[("defs.svh", DEFS), ("a.sv", A), ("b.sv", &format!("`include \"defs.svh\"\n{B}"))]);
    let h = a.vfs.set_overlay_versioned(&root.join("defs.svh"), Arc::from("`define W 16\n"), Some(1));
    let pin = Arc::new(a.vfs.pin());
    // The editor sends a newer text while the update runs.
    a.vfs.set_overlay_versioned(&root.join("defs.svh"), Arc::from("`define W 32\n"), Some(2));
    let mut base = a.clone();
    base.pin = Some(pin);
    let next = base.update_with(&[h], &dvi_project::Progress::new()).unwrap();
    // Every unit that includes the header has the pinned text, with its version.
    let units = next.units_of_file(h).to_vec();
    assert_eq!(units.len(), 2);
    for u in units {
        let s = next.unit(u).sources.get(&h).unwrap().clone();
        assert_eq!((&*s.text, s.lsp_version), ("`define W 16\n", Some(1)));
    }
}

/// The SKIPPED_CODE notice of `file`: its line and message.
fn skipped_notice(a: &Analysis, root: &Path, file: &str) -> Option<(u32, String)> {
    let f = a.vfs.lookup(&root.join(file))?;
    let text = a.file_text(f)?;
    a.file_diagnostics(f)
        .into_iter()
        .find(|d| d.message.starts_with("SKIPPED_CODE"))
        .map(|d| (text[..d.span.start as usize].matches('\n').count() as u32, d.message))
}

#[test]
fn skipped_code_uses_are_counted_again_only_where_an_edit_can_change_them() {
    // No build file: env_pkg's `ifdef region hides NUM_AGENTS, which user.sv uses twice.
    let files = [
        ("env_pkg.sv", "package env_pkg;\n`ifdef FULL_ENV\n  parameter int NUM_AGENTS = 4;\n`endif\n  parameter int VISIBLE = 1;\nendpackage\n"),
        ("user.sv", "class user_seq;\n  int slots[env_pkg::NUM_AGENTS];\nendclass\nmodule user_top;\n  import env_pkg::*;\n  int n = NUM_AGENTS;\nendmodule\n"),
        ("other.sv", "module other;\n  initial begin\n  end\nendmodule\n"),
    ];
    let (_, root) = project("hiddenreuse", &files);
    std::fs::remove_file(root.join(".dvicode/default.build")).unwrap();
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let (line, msg) = skipped_notice(&a, &root, "env_pkg.sv").expect("a notice");
    assert!(line == 1 && msg.contains("2 uses"), "{line} {msg}");

    // A third use, in a block: nothing other files see changes, so only other.sv is counted
    // again, the others' uses are taken over.
    let other = edit(&a, &root, "other.sv", "module other;\n  initial begin\n    $display(env_pkg::NUM_AGENTS);\n  end\nendmodule\n");
    let mut b = a.update_with(&[other], &dvi_project::Progress::new()).unwrap();
    assert!(!b.interface_changed(&a));
    b.reuse_hidden_uses(&a);
    assert!(b.hidden_seed.as_ref().is_some_and(|s| s.len() == 2), "env_pkg.sv and user.sv taken over");
    let (line, msg) = skipped_notice(&b, &root, "env_pkg.sv").unwrap();
    assert!(line == 1 && msg.contains("3 uses"), "{line} {msg}");

    // A comment above the `ifdef moves the region: every file is counted again.
    let pkg = edit(&b, &root, "env_pkg.sv", &format!("// moved\n{}", files[0].1));
    let mut c = b.update_with(&[pkg], &dvi_project::Progress::new()).unwrap();
    assert!(!c.interface_changed(&b));
    c.reuse_hidden_uses(&b);
    assert!(c.hidden_seed.is_none());
    let (line, msg) = skipped_notice(&c, &root, "env_pkg.sv").unwrap();
    assert!(line == 2 && msg.contains("3 uses"), "{line} {msg}");
}

/// Every project file's problems, by file name.
fn all_problems(a: &Analysis) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = a
        .project_files()
        .into_iter()
        .flat_map(|f| {
            let name = a.path(f).file_name().unwrap().to_string_lossy().to_string();
            a.file_diagnostics(f).into_iter().map(move |d| (name.clone(), d.message))
        })
        .collect();
    v.sort();
    v
}

/// Auto-configures `files`, warms the skipped-code counts, makes `edit` (file, new text) as
/// the server does (reusing other files' counts when nothing they see changed), and checks
/// that every file's problems are those of a full build of the new texts. Returns whether
/// the edit changed what other files see.
fn edit_like_the_server(name: &str, files: &[(&str, &str)], edited: (&str, &str)) -> bool {
    let (_, root) = project(name, files);
    std::fs::remove_file(root.join(".dvicode/default.build")).unwrap();
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let before = all_problems(&a);
    let f = edit(&a, &root, edited.0, edited.1);
    let mut b = a.update_with(&[f], &dvi_project::Progress::new()).unwrap();
    let changed = b.interface_changed(&a);
    if !changed {
        b.reuse_hidden_uses(&a);
    }
    let full = Analysis::build(a.vfs.clone(), a.setup.clone());
    assert_eq!(all_problems(&b), all_problems(&full), "before the edit: {before:?}");
    changed
}

#[test]
fn connections_that_pick_another_copy_change_what_other_files_see() {
    // Two copies of D (no build file); the named connection picks the one with port x, whose
    // removed region may declare `secret`.
    let files = [
        ("a/d.sv", "module D(input logic x);\n`ifdef NEVER\n  logic secret;\n`endif\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("top.sv", "module top;\n  D d(.x());\nendmodule\n"),
        ("observer.sv", "module observer;\n  initial $display(top.d.secret);\nendmodule\n"),
    ];
    assert!(edit_like_the_server("ifaceconn", &files, ("top.sv", "module top;\n  D d(.y());\nendmodule\n")));
}

#[test]
fn a_removed_region_moving_out_of_a_function_changes_what_other_files_see() {
    let files = [
        ("p.sv", "package p;\nfunction void f();\n// pad here\n`ifdef NEVER\nint secret;\n`endif\nendfunction\nendpackage\n"),
        ("use.sv", "module m;\n  initial $display(p::secret);\nendmodule\n"),
    ];
    let swapped = "package p;\nfunction void f();\nendfunction\n`ifdef NEVER\nint secret;\n`endif\n// pad here\nendpackage\n";
    assert!(edit_like_the_server("ifaceregion", &files, ("p.sv", swapped)));
}

#[test]
fn a_bind_to_another_target_changes_what_other_files_see() {
    let files = [
        ("k.sv", "module K;\n`ifdef NEVER\n  logic secret;\n`endif\nendmodule\n"),
        ("mn.sv", "module M;\nendmodule\nmodule N;\nendmodule\n"),
        ("bind.sv", "module binds;\nendmodule\nbind M K b();\n"),
        ("observer.sv", "module observer;\n  M m();\n  initial $display(m.b.secret);\nendmodule\n"),
    ];
    assert!(edit_like_the_server("ifacebind", &files, ("bind.sv", "module binds;\nendmodule\nbind N K b();\n")));
}

#[test]
fn an_edit_inside_a_body_keeps_other_files_counts() {
    let files = [
        ("p.sv", "package p;\n`ifdef NEVER\nint secret;\n`endif\nendpackage\n"),
        ("use.sv", "module m;\n  initial $display(p::secret);\nendmodule\n"),
        ("other.sv", "module other;\n  initial begin\n  end\nendmodule\n"),
    ];
    assert!(!edit_like_the_server("ifacebody", &files, ("other.sv", "module other;\n  initial begin\n    $display(p::secret);\n  end\nendmodule\n")));
}

#[test]
fn splitting_an_instantiation_statement_changes_what_other_files_see() {
    // `D a(.x()), b(.y());` must fit one copy for both: none does, so both are the nearest.
    let files = [
        ("a/d.sv", "module D(input logic x);\n`ifdef NEVER\n  logic secret;\n`endif\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("a/top.sv", "module top;\n  D a(.x()), b(.y());\nendmodule\n"),
        ("observer.sv", "module observer;\n  initial $display(top.b.secret);\nendmodule\n"),
    ];
    assert!(edit_like_the_server("ifacestmt", &files, ("a/top.sv", "module top;\n  D a(.x());\n  D b(.y());\nendmodule\n")));
}

#[test]
fn a_variable_of_a_named_block_is_seen_by_other_files() {
    let files = [
        ("c.sv", "class C;\n`ifdef NEVER\n  int secret;\n`endif\nendclass\nclass E;\nendclass\n"),
        ("top.sv", "module top;\n  initial begin : B\n    C obj;\n  end\nendmodule\n"),
        ("observer.sv", "module observer;\n  initial $display(top.B.obj.secret);\nendmodule\n"),
    ];
    assert!(edit_like_the_server("ifaceblock", &files, ("top.sv", "module top;\n  initial begin : B\n    E obj;\n  end\nendmodule\n")));
}

#[test]
fn renaming_a_connection_of_a_design_element_defined_once_changes_nothing_others_see() {
    let files = [
        ("d.sv", "module D(input logic request);\nendmodule\n"),
        ("top.sv", "module top;\n  D d(.request());\nendmodule\n"),
    ];
    let (_, root) = project("ifaceonce", &files);
    std::fs::remove_file(root.join(".dvicode/default.build")).unwrap();
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let f = edit(&a, &root, "top.sv", "module top;\n  D d(.re());\nendmodule\n");
    let b = a.update_with(&[f], &dvi_project::Progress::new()).unwrap();
    assert!(!b.interface_changed(&a));
}

#[test]
fn an_unsaved_file_in_a_library_folder_is_found_without_a_build() {
    // -y preferred -y fallback: only fallback/X.v is on disk, then preferred/X.v is opened
    // unsaved; the next update finds it first, as a build would.
    let files = [("top.sv", "module top;\n  X x();\nendmodule\n")];
    let (_, root) = project("libunsaved", &files);
    std::fs::create_dir_all(root.join("preferred")).unwrap();
    std::fs::create_dir_all(root.join("fallback")).unwrap();
    std::fs::write(root.join("fallback/X.v"), "module X;\n  logic from_fallback;\nendmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "-y preferred\n-y fallback\n+libext+.v\ntop.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let lib = |a: &Analysis| -> Vec<String> {
        let mut v: Vec<String> = a.units.iter().flatten().filter(|u| u.library).map(|u| a.path(u.root).parent().unwrap().file_name().unwrap().to_string_lossy().to_string()).collect();
        v.sort();
        v
    };
    assert_eq!(lib(&a), ["fallback"]);
    let p = edit(&a, &root, "preferred/X.v", "module X;\n  logic from_preferred;\nendmodule\n");
    let b = a.update_with(&[p], &dvi_project::Progress::new()).unwrap();
    assert_eq!(lib(&b), ["preferred"]);
    // A body edit after that keeps it.
    let t = edit(&b, &root, "top.sv", "module top;\n  X x();\n  initial begin end\nendmodule\n");
    let c = b.update_with(&[t], &dvi_project::Progress::new()).unwrap();
    assert_eq!(lib(&c), ["preferred"]);
}

#[test]
fn moving_an_instance_to_another_statement_changes_what_other_files_see() {
    let files = [
        ("a/d.sv", "module D(input logic x);\n`ifdef NEVER\n  logic secret;\n`endif\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("a/top.sv", "module top;\n  D a(.x()), b(.x());\n  D c(.y());\nendmodule\n"),
        ("observer.sv", "module observer;\n  initial $display(top.b.secret);\nendmodule\n"),
    ];
    assert!(edit_like_the_server("ifacemove", &files, ("a/top.sv", "module top;\n  D a(.x());\n  D b(.y()), c(.y());\nendmodule\n")));
}

#[test]
fn statements_in_two_headers_are_told_apart() {
    // Both statements start at offset 0 of their header.
    let files = [
        ("a/d.sv", "module D(input logic x);\n`ifdef NEVER\n  logic secret;\n`endif\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("a/first.svh", "D a(.x()), b(.y());\n"),
        ("a/second.svh", "D c(.x());\n"),
        ("a/top.sv", "module top;\n`include \"first.svh\"\n`include \"second.svh\"\nendmodule\n"),
        ("observer.sv", "module observer;\n  initial $display(top.c.secret);\nendmodule\n"),
    ];
    assert!(edit_like_the_server("ifaceheaders", &files, ("a/second.svh", "D c(.y());\n")));
}

#[test]
fn an_instance_without_connections_follows_its_statement() {
    let files = [
        ("a/d.sv", "module D(input logic x);\n`ifdef NEVER\n  logic secret;\n`endif\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("a/top.sv", "module top;\n  D a(.x()), b();\n  D c(.y());\nendmodule\n"),
        ("observer.sv", "module observer;\n  initial $display(top.b.secret);\nendmodule\n"),
    ];
    assert!(edit_like_the_server("ifacesibling", &files, ("a/top.sv", "module top;\n  D a(.x());\n  D b(), c(.y());\nendmodule\n")));
}

#[test]
fn a_statement_moved_to_another_scope_changes_what_other_files_see() {
    let files = [
        ("a/d.sv", "module D(input logic x);\n`ifdef NEVER\n  logic secret;\n`endif\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("a/hx.svh", "D u(.x());\n"),
        ("a/hy.svh", "D u(.y());\n"),
        ("a/ab.sv", "module A;\n`include \"hx.svh\"\nendmodule\nmodule B;\n`include \"hy.svh\"\nendmodule\n"),
        ("observer.sv", "module observer;\n  initial $display(A.u.secret);\nendmodule\n"),
    ];
    assert!(edit_like_the_server("ifacescope", &files, ("a/ab.sv", "module A;\n`include \"hy.svh\"\nendmodule\nmodule B;\n`include \"hx.svh\"\nendmodule\n")));
}

#[test]
fn connecting_a_sibling_the_same_way_changes_nothing_others_see() {
    let files = [
        ("a/d.sv", "module D(input logic x);\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("a/top.sv", "module top;\n  D a(.x()), b();\nendmodule\n"),
    ];
    assert!(!edit_like_the_server("ifacesame", &files, ("a/top.sv", "module top;\n  D a(.x()), b(.x());\nendmodule\n")));
}

#[test]
fn a_header_included_in_several_scopes_counts_each() {
    let files = [
        ("a/d.sv", "module D(input logic x);\n`ifdef NEVER\n  logic secret;\n`endif\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("a/hx.svh", "D u(.x());\n"),
        ("a/hy.svh", "D u(.y());\n"),
        ("a/m.sv", "module AnchorX;\n`include \"hx.svh\"\nendmodule\nmodule AnchorY;\n`include \"hy.svh\"\nendmodule\nmodule B;\n`include \"hx.svh\"\nendmodule\nmodule C;\n`include \"hy.svh\"\nendmodule\n"),
        ("observer.sv", "module observer;\n  initial $display(B.u.secret);\nendmodule\n"),
    ];
    let swapped = "module AnchorX;\n`include \"hx.svh\"\nendmodule\nmodule AnchorY;\n`include \"hy.svh\"\nendmodule\nmodule B;\n`include \"hy.svh\"\nendmodule\nmodule C;\n`include \"hx.svh\"\nendmodule\n";
    assert!(edit_like_the_server("ifacerepeat", &files, ("a/m.sv", swapped)));
}

#[test]
fn splitting_a_statement_without_connections_changes_nothing_others_see() {
    let files = [
        ("a/d.sv", "module D(input logic x);\nendmodule\n"),
        ("b/d.sv", "module D(input logic y);\nendmodule\n"),
        ("a/top.sv", "module top;\n  D a(), b();\nendmodule\n"),
    ];
    assert!(!edit_like_the_server("ifacesplit", &files, ("a/top.sv", "module top;\n  D a();\n  D b();\nendmodule\n")));
}

#[test]
fn closing_an_unsaved_empty_library_file_brings_back_the_one_on_disk() {
    let files = [("top.sv", "module top;\n  X x();\nendmodule\n")];
    let (_, root) = project("libclose", &files);
    std::fs::create_dir_all(root.join("preferred")).unwrap();
    std::fs::create_dir_all(root.join("fallback")).unwrap();
    std::fs::write(root.join("fallback/X.v"), "module X;\nendmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "-y preferred\n-y fallback\n+libext+.v\ntop.sv\n").unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let lib = |a: &Analysis| -> Vec<String> {
        let mut v: Vec<String> = a.units.iter().flatten().filter(|u| u.library).map(|u| a.path(u.root).parent().unwrap().file_name().unwrap().to_string_lossy().to_string()).collect();
        v.sort();
        v
    };
    // An empty unsaved preferred/X.v is found first, then closed without saving.
    let p = edit(&a, &root, "preferred/X.v", "");
    let b = a.update_with(&[p], &dvi_project::Progress::new()).unwrap();
    assert_eq!(lib(&b), ["preferred"]);
    b.vfs.clear_overlay(&root.join("preferred/X.v"));
    let c = b.update_with(&[p], &dvi_project::Progress::new()).unwrap();
    assert_eq!(lib(&c), ["fallback"]);
}

/// Trie nodes the macro tables of all units hold (tables share what they did not change).
fn table_nodes(a: &Analysis) -> usize {
    dvi_syntax::preproc::MacroTable::distinct_nodes(a.units.iter().flatten().flat_map(|u| [&u.macros_before, &u.macros_after]))
}

/// Every unit's macro tables are as a full build of the same texts (with `open` open) makes
/// them: the same names, each with the same text and at the same place.
fn tables_like_a_full_build(b: &Analysis, open: &[FileId]) {
    let full = Analysis::build(b.vfs.clone(), b.setup.clone()).sync_open_files(open);
    let mut compared = 0;
    for u in b.units.iter().flatten() {
        let v = full.units.iter().flatten().find(|v| v.root == u.root && v.kind == u.kind && v.library == u.library && v.invocation == u.invocation).unwrap();
        for (t, w) in [(&u.macros_before, &v.macros_before), (&u.macros_after, &v.macros_after)] {
            assert!(t.same_definitions(w), "{}", b.path(u.root).display());
            for (name, d) in t.iter() {
                assert_eq!(d.name_span, w.get(name).unwrap().name_span, "{name} in {}", b.path(u.root).display());
            }
        }
        compared += 1;
    }
    assert_eq!(compared, full.units.iter().flatten().count());
}

const HEADER_MACROS: usize = 1000;
const FILES: usize = 200;

/// A header of `HEADER_MACROS` macros, then `FILES` files (`file_text`: each using one of the
/// header's), a top that instantiates a library module using one, and an open file outside the
/// build using one. Returns the build (with the open file), its root and the open file.
fn header_project(name: &str, file_text: fn(usize) -> String) -> (Analysis, PathBuf, FileId) {
    let root = std::env::temp_dir().join(format!("dvicode-incr-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    std::fs::write(root.join("defs.sv"), header_text()).unwrap();
    let mut build = String::from("-y lib\n+libext+.v\ndefs.sv\n");
    for i in 0..FILES {
        let f = format!("f{i:03}.sv");
        std::fs::write(root.join(&f), file_text(i)).unwrap();
        build.push_str(&f);
        build.push('\n');
    }
    std::fs::write(root.join("top.sv"), "module top;\n  L l();\nendmodule\n").unwrap();
    build.push_str("top.sv\n");
    std::fs::write(root.join("lib/L.v"), "module L;\n  localparam int Q = `HDR_3(1);\nendmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let extra = edit(&a, &root, "extra.sv", "module extra;\n  localparam int E = `HDR_5(2);\nendmodule\n");
    let a = a.sync_open_files(&[extra]);
    assert!(a.units.iter().flatten().any(|u| u.library), "lib/L.v is compiled");
    assert!(a.is_adhoc_root(extra));
    (a, root, extra)
}

fn header_text() -> String {
    (0..HEADER_MACROS).map(|i| format!("`define HDR_{i}(x) ((x) + {i})\n")).collect()
}

fn file_text(i: usize) -> String {
    format!("`define F{i} {i}\nmodule m{i};\n  localparam int P = `F{i} + `HDR_{i}(1);\nendmodule\n")
}

/// Trie nodes of the header's own table.
fn header_nodes(a: &Analysis, root: &Path) -> usize {
    let defs = a.vfs.file_id(&root.join("defs.sv"));
    dvi_syntax::preproc::MacroTable::distinct_nodes([&a.unit(a.units_for(defs)[0]).macros_after])
}

/// At most a path of new nodes per unit and table, besides the header's.
fn node_bound(header: usize) -> usize {
    header + (FILES + 10) * 13
}

#[test]
fn macro_tables_share_what_files_do_not_change_and_follow_moved_definitions() {
    let (a, root, extra) = header_project("sharedtables", file_text);
    let header_nodes = header_nodes(&a, &root);
    // Each file adds one macro: a path of new nodes, not a copy of the 1,000 others.
    assert!(table_nodes(&a) <= node_bound(header_nodes), "{} nodes for a header of {header_nodes}", table_nodes(&a));

    // A line above the header's definitions: they all move, and every later table follows.
    let defs = edit(&a, &root, "defs.sv", &format!("// moved\n{}", header_text()));
    let work = dvi_syntax::preproc::macro_table_work();
    let b = a.update(&[defs]);
    assert!(!b.interface_changed(&a), "the definitions only moved");
    let work = dvi_syntax::preproc::macro_table_work() - work;
    assert!(table_nodes(&b) <= node_bound(2 * header_nodes), "{} nodes after the move", table_nodes(&b));
    assert!(work as usize <= 2 * node_bound(header_nodes), "{work} nodes visited");
    assert_eq!(definition_lines(&b, &root, "f150.sv", "`HDR_150"), vec![("defs.sv".into(), 152)]);
    assert_eq!(definition_lines(&b, &root, "lib/L.v", "`HDR_3"), vec![("defs.sv".into(), 5)]);
    assert_eq!(definition_lines(&b, &root, "extra.sv", "`HDR_5"), vec![("defs.sv".into(), 7)]);
    tables_like_a_full_build(&b, &[extra]);
}

#[test]
fn moving_a_header_and_every_file_at_once_costs_as_much_as_the_changes() {
    // As a checkout that changes a header and every file: one update.
    let (a, root, extra) = header_project("batchmove", file_text);
    let header_nodes = header_nodes(&a, &root);
    let mut files = vec![edit(&a, &root, "defs.sv", &format!("// moved\n{}", header_text()))];
    for i in 0..FILES {
        files.push(edit(&a, &root, &format!("f{i:03}.sv"), &format!("// moved too\n{}", file_text(i))));
    }
    let work = dvi_syntax::preproc::macro_table_work();
    let b = a.update(&files);
    assert!(!b.interface_changed(&a), "the definitions only moved");
    let work = dvi_syntax::preproc::macro_table_work() - work;
    assert!(table_nodes(&b) <= node_bound(2 * header_nodes), "{} nodes after the move", table_nodes(&b));
    assert!(work as usize <= 3 * node_bound(header_nodes), "{work} nodes visited");
    assert_eq!(definition_lines(&b, &root, "f150.sv", "`HDR_150"), vec![("defs.sv".into(), 152)]);
    assert_eq!(definition_lines(&b, &root, "f151.sv", "`F151"), vec![("f151.sv".into(), 2)]);
    assert_eq!(definition_lines(&b, &root, "extra.sv", "`HDR_5"), vec![("defs.sv".into(), 7)]);
    tables_like_a_full_build(&b, &[extra]);
    // Then a macro of the header changes: every later file is compiled again, from the tables
    // the batch left.
    edit(&b, &root, "defs.sv", &format!("// moved\n{}", header_text().replacen("((x) + 0)", "((x) - 0)", 1)));
    let work = dvi_syntax::preproc::macro_table_work();
    let c = b.update(&files[..1]);
    assert!(c.interface_changed(&b));
    let work = dvi_syntax::preproc::macro_table_work() - work;
    assert!(work as usize <= 3 * node_bound(header_nodes), "{work} nodes visited after the batch");
    tables_like_a_full_build(&c, &[extra]);
}

#[test]
fn a_file_that_redefines_a_moved_macro_identically_is_where_later_files_find_it() {
    // p.sv defines and undefines a name: its tables before and after hold the same
    // definitions but are different tables, and must stay so once pointed at moved ones.
    let files = [
        ("defs.svh", "`define A 1\n"),
        ("h.sv", "`include \"defs.svh\"\nmodule h; endmodule\n"),
        ("p.sv", "`define TMP 1\n`undef TMP\nmodule p; endmodule\n"),
        ("q.sv", "module q;\n  localparam int X = `A;\nendmodule\n"),
    ];
    let (a, root) = project("redefinemoved", &files);
    let defs = edit(&a, &root, "defs.svh", "\n`define A 1\n");
    let b = a.update(&[defs]);
    assert_eq!(definition_lines(&b, &root, "q.sv", "`A"), vec![("defs.svh".into(), 2)]);
    tables_like_a_full_build(&b, &[]);
    // Moved again, and p.sv now defines A the same way: q.sv finds p.sv's.
    edit(&b, &root, "defs.svh", "\n\n`define A 1\n");
    let p = edit(&b, &root, "p.sv", "`define TMP 1\n`undef TMP\nmodule p; endmodule\n`define A 1\n");
    let c = b.update(&[defs, p]);
    assert_eq!(definition_lines(&c, &root, "q.sv", "`A"), vec![("p.sv".into(), 4)]);
    tables_like_a_full_build(&c, &[]);
}

/// A file that defines nothing: the files share the header's table.
fn plain_file_text(i: usize) -> String {
    format!("module m{i};\n  localparam int P = `HDR_{i}(1);\nendmodule\n")
}

#[test]
fn a_batch_where_files_redefine_moved_header_macros_costs_as_much_as_the_changes() {
    // Each file redefines a macro of the header the same way (another one each, or all the
    // same one), as the header's definitions move: each file then is where the next ones find
    // it.
    for same in [false, true] {
        let (a, root, extra) = header_project(if same { "redefinesame" } else { "redefineeach" }, plain_file_text);
        let header_nodes = header_nodes(&a, &root);
        let mut files = vec![edit(&a, &root, "defs.sv", &format!("// moved\n{}", header_text()))];
        for i in 0..FILES {
            let m = if same { 0 } else { i };
            files.push(edit(&a, &root, &format!("f{i:03}.sv"), &format!("{}`define HDR_{m}(x) ((x) + {m})\n", plain_file_text(i))));
        }
        let work = dvi_syntax::preproc::macro_table_work();
        let b = a.update(&files);
        b.interface_changed(&a);
        let work = dvi_syntax::preproc::macro_table_work() - work;
        assert!(table_nodes(&b) <= node_bound(2 * header_nodes), "{} nodes", table_nodes(&b));
        assert!(work as usize <= 3 * node_bound(header_nodes), "{work} nodes visited (same: {same})");
        if same {
            assert_eq!(definition_lines(&b, &root, "extra.sv", "`HDR_5"), vec![("defs.sv".into(), 7)]);
            assert_eq!(definition_lines(&b, &root, "f000.sv", "`HDR_0"), vec![("defs.sv".into(), 2)]);
        } else {
            assert_eq!(definition_lines(&b, &root, "extra.sv", "`HDR_5"), vec![("f005.sv".into(), 4)]);
            assert_eq!(definition_lines(&b, &root, "f150.sv", "`HDR_150"), vec![("defs.sv".into(), 152)]);
        }
        tables_like_a_full_build(&b, &[extra]);
    }
}

#[test]
fn what_an_open_file_moves_no_other_open_file_sees() {
    let files = [("defs.svh", "`define A 1\n"), ("h.sv", "`include \"defs.svh\"\nmodule h; endmodule\n")];
    for (i, a_first) in [true, false].into_iter().enumerate() {
        let (a, root) = project(&format!("adhocmoves{i}"), &files);
        let x = edit(&a, &root, "x.sv", "module x;\n  localparam int X = `A;\nendmodule\n");
        let y = edit(&a, &root, "y.sv", "module y;\n  localparam int Y = `A;\nendmodule\n");
        let open = if a_first { [x, y] } else { [y, x] };
        let a = a.sync_open_files(&open);
        // The header's definition moves, and x.sv defines A again the same way at its end.
        let defs = edit(&a, &root, "defs.svh", "\n`define A 1\n");
        edit(&a, &root, "x.sv", "module x;\n  localparam int X = `A;\nendmodule\n`define A 1\n");
        let b = a.update(&[defs, x]);
        assert_eq!(definition_lines(&b, &root, "y.sv", "`A"), vec![("defs.svh".into(), 2)], "order {i}");
        assert_eq!(definition_lines(&b, &root, "x.sv", "`A"), vec![("defs.svh".into(), 2)], "order {i}");
        tables_like_a_full_build(&b, &open);
    }
}

#[test]
fn moving_the_headers_of_many_invocations_at_once_costs_as_much_as_the_changes() {
    // Each invocation: a header, a file that defines nothing, and a library module (in a
    // folder of its own: library files are looked for in the invocations in order), all
    // sharing the header's table.
    const N: usize = 200;
    let root = std::env::temp_dir().join(format!("dvicode-incr-invocations-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    let mut build = String::new();
    for i in 0..N {
        std::fs::write(root.join(format!("h{i}.sv")), format!("`define M{i} {i}\n")).unwrap();
        std::fs::write(root.join(format!("t{i}.sv")), format!("module t{i};\n  L{i} l();\nendmodule\n")).unwrap();
        std::fs::create_dir_all(root.join(format!("lib{i}"))).unwrap();
        std::fs::write(root.join(format!("lib{i}/L{i}.v")), format!("module L{i};\n  localparam int Q = `M{i};\nendmodule\n")).unwrap();
        build.push_str(&format!("+dvicode_init\n-y lib{i}\n+libext+.v\nh{i}.sv\nt{i}.sv\n"));
    }
    std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    assert_eq!(a.units.iter().flatten().filter(|u| u.library).count(), N);
    let files: Vec<FileId> = (0..N).map(|i| edit(&a, &root, &format!("h{i}.sv"), &format!("\n`define M{i} {i}\n"))).collect();
    let work = dvi_syntax::preproc::macro_table_work();
    let b = a.update(&files);
    let work = dvi_syntax::preproc::macro_table_work() - work;
    assert!(work as usize <= 10 * N, "{work} nodes visited");
    assert_eq!(definition_lines(&b, &root, "lib150/L150.v", "`M150"), vec![("h150.sv".into(), 2)]);
    tables_like_a_full_build(&b, &[]);
}

#[test]
fn changing_a_header_macro_compiles_the_later_files_again_at_the_cost_of_their_changes() {
    // HDR_0 expands differently: every later file is compiled again with the new table.
    for (name, file_text) in [("changeplain", plain_file_text as fn(usize) -> String), ("changedefining", file_text)] {
        let (a, root, extra) = header_project(name, file_text);
        let header_nodes = header_nodes(&a, &root);
        let defs = edit(&a, &root, "defs.sv", &header_text().replacen("((x) + 0)", "((x) - 0)", 1));
        let work = dvi_syntax::preproc::macro_table_work();
        let b = a.update(&[defs]);
        assert!(b.interface_changed(&a));
        let work = dvi_syntax::preproc::macro_table_work() - work;
        assert!(work as usize <= 3 * node_bound(header_nodes), "{work} nodes visited ({name})");
        assert!(table_nodes(&b) <= node_bound(2 * header_nodes), "{} nodes", table_nodes(&b));
        tables_like_a_full_build(&b, &[extra]);
        // And with the header's definitions moved too, and a file in the middle edited.
        let mid = edit(&b, &root, "f100.sv", &format!("// edited\n{}", file_text(100)));
        edit(&b, &root, "defs.sv", &format!("// moved\n{}", header_text().replacen("((x) + 1)", "((x) - 1)", 1)));
        let c = b.update(&[defs, mid]);
        assert_eq!(definition_lines(&c, &root, "f150.sv", "`HDR_150"), vec![("defs.sv".into(), 152)]);
        tables_like_a_full_build(&c, &[extra]);
    }
}

#[test]
fn files_after_a_changed_macro_see_where_the_header_definitions_moved_in_the_same_update() {
    // One update: the header's definitions move, and a later file changes its own macro, so
    // the files after it are compiled again, from its table.
    let (a, root, extra) = header_project("moveandchange", file_text);
    let defs = edit(&a, &root, "defs.sv", &format!("// moved\n{}", header_text()));
    let f50 = edit(&a, &root, "f050.sv", &file_text(50).replacen("`define F50 50", "`define F50 51", 1));
    let b = a.update(&[defs, f50]);
    assert_eq!(definition_lines(&b, &root, "f150.sv", "`HDR_150"), vec![("defs.sv".into(), 152)]);
    assert_eq!(definition_lines(&b, &root, "extra.sv", "`HDR_5"), vec![("defs.sv".into(), 7)]);
    tables_like_a_full_build(&b, &[extra]);
}
