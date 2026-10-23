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
