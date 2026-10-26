//! The compile-context inspector: contexts with their recorded include chains, the first
//! context's invocation, macros, includes (with the step that found them and shadowed copies),
//! removed lines, duplicates and problems; and the hover on an include.

use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A project with its own build file.
fn project(name: &str, build: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-context-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    for (f, text) in files {
        let p = root.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    (a, root)
}

fn name(uri: &str) -> &str {
    uri.rsplit('/').next().unwrap_or(uri)
}

const BUILD: &str = "+incdir+shared\n+define+MODE_A\n+dvicode_skip_compile+*skipme*\ntop_a.sv\nother.sv\nskipme.sv\n+dvicode_init+second\n+incdir+shared\ntop_b.sv\n";

fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("shared/common.svh", "`ifdef MODE_A\n  int a;\n`else\n  int b;\n  int c;\n`endif\n`include \"leaf.svh\"\n`include \"missing.svh\"\n"),
        ("shared/leaf.svh", "int leaf_x;\n"),
        // The same name in the project folder: later in the search, shadowed.
        ("leaf.svh", "int leaf_y;\n"),
        ("top_a.sv", "module top_a;\n`include \"common.svh\"\nendmodule\nmodule fifo; endmodule\n"),
        ("other.sv", "module fifo; endmodule\n"),
        ("skipme.sv", "module skipped; endmodule\n"),
        ("top_b.sv", "`define HDR \"common.svh\"\nmodule top_b;\n`include `HDR\nendmodule\n"),
    ]
}

#[test]
fn a_shared_header_shows_both_contexts_and_the_first_in_detail() {
    let (a, root) = project("shared", BUILD, &files());
    let p = root.join("shared/common.svh");
    let c = a.compile_context(&p, a.vfs.lookup(&p));
    assert!(c.known);
    assert_eq!(c.contexts_total, 2);
    let roots: Vec<(&str, usize, &str)> = c.contexts.iter().map(|x| (name(&x.root), x.invocation, x.root_role)).collect();
    assert_eq!(roots, [("top_a.sv", 0, "top"), ("top_b.sv", 1, "top")]);
    assert_eq!(c.contexts[0].order, Some((1, 2)), "top_a.sv is the first of the two top files left (skipme.sv is skipped)");
    // The chain is the recorded include: top_a.sv line 2; top_b.sv's comes from a macro.
    let chain = |i: usize| c.contexts[i].chain.iter().map(|p| (name(&p.uri).to_string(), p.line)).collect::<Vec<_>>();
    assert_eq!(chain(0), [("top_a.sv".to_string(), 1)]);
    assert_eq!(chain(1), [("top_b.sv".to_string(), 2)]);
    let d = c.detail.as_ref().unwrap();
    let inv = d.invocation.as_ref().unwrap();
    assert_eq!((inv.index, inv.defines.clone()), (0, vec![("MODE_A".to_string(), None)]));
    // MODE_A is tested and defined on the command line in the first context.
    let m = d.macros.iter().find(|m| m.name == "MODE_A").unwrap();
    assert!(m.tested && !m.used && m.seen.len() == 1 && m.seen[0].defined && m.seen[0].command_line, "{m:?}");
    // The `else branch (its two lines, not the directives) is removed.
    assert_eq!(d.inactive_lines, 2);
    // leaf.svh found in the header's own folder; the project folder's copy is later in the search.
    let leaf = d.includes.iter().find(|i| i.text == "leaf.svh").unwrap();
    assert_eq!((leaf.step, leaf.entered, leaf.resolved.as_deref().map(name)), (Some("folder"), true, Some("leaf.svh")));
    assert!(leaf.resolved.as_ref().unwrap().contains("/shared/"));
    assert_eq!(leaf.shadowed.len(), 1, "{leaf:?}");
    assert!(!leaf.shadowed[0].contains("/shared/"));
    let missing = d.includes.iter().find(|i| i.text == "missing.svh").unwrap();
    assert!(missing.resolved.is_none() && missing.step.is_none() && !missing.entered);
    assert!(d.preprocessing.iter().any(|p| p.code == "INCLUDE_NOT_FOUND" && p.line == 7), "{:?}", d.preprocessing);
}

#[test]
fn the_second_invocation_sees_other_macros() {
    let (a, root) = project("second", BUILD, &files());
    let p = root.join("top_b.sv");
    let c = a.compile_context(&p, a.vfs.lookup(&p));
    assert_eq!(c.contexts_total, 1);
    assert!(c.contexts[0].is_root && c.contexts[0].chain.is_empty());
    let d = c.detail.unwrap();
    assert_eq!(d.invocation.as_ref().map(|i| (i.index, i.mode.as_str())), Some((1, "second")));
    let hdr = d.macros.iter().find(|m| m.name == "HDR").unwrap();
    assert!(hdr.used && hdr.seen[0].defined && !hdr.seen[0].command_line);
    assert_eq!(hdr.seen[0].at.as_ref().map(|p| (name(&p.uri), p.line)), Some(("top_b.sv", 0)));
    // The include written through a macro is found in the incdir.
    let inc = &d.includes[0];
    assert_eq!((inc.text.as_str(), inc.step, inc.incdir.as_ref().map(|x| x.0)), ("common.svh", Some("incdir"), Some(1)));
}

#[test]
fn duplicates_skips_and_files_not_compiled() {
    let (a, root) = project("dups", BUILD, &files());
    let p = root.join("top_a.sv");
    let c = a.compile_context(&p, a.vfs.lookup(&p));
    let d = c.detail.unwrap();
    assert_eq!(d.duplicates.len(), 1, "{:?}", d.duplicates);
    let x = &d.duplicates[0];
    assert_eq!((x.kind, x.name.as_str(), x.others.len(), x.rule), ("module", "fifo", 1, "first compiled"));
    assert_eq!(name(&x.others[0].uri), "other.sv");
    assert_eq!(x.default.as_ref().map(|p| name(&p.uri)), Some("top_a.sv"), "top_a.sv is compiled first");
    // A skipped file: listed, matched by the pattern, compiled nowhere.
    let s = root.join("skipme.sv");
    let c = a.compile_context(&s, a.vfs.lookup(&s));
    assert_eq!(c.skip_matches, vec![(0, "*skipme*".to_string())]);
    assert!(c.contexts.is_empty() && c.detail.is_none());
    // A file the engine never read.
    let n = root.join("nowhere.sv");
    let c = a.compile_context(&n, a.vfs.lookup(&n));
    assert!(!c.known && c.contexts.is_empty());
}

#[test]
fn hover_on_an_include_says_what_found_it() {
    let (a, root) = project("hover", BUILD, &files());
    let at = |f: &str, needle: &str| {
        let text = std::fs::read_to_string(root.join(f)).unwrap();
        (a.vfs.lookup(&root.join(f)).unwrap(), text.find(needle).unwrap() as u32 + 1)
    };
    let (f, off) = at("top_a.sv", "\"common.svh\"");
    let (h, _) = a.hover(f, off).unwrap();
    assert!(h.contains("include directory 1 of 1") && h.contains("common.svh"), "{h}");
    // Through a macro: the include, not the macro.
    let (f, off) = at("top_b.sv", "`HDR\n");
    let (h, _) = a.hover(f, off).unwrap();
    assert!(h.contains("reads") && h.contains("include directory"), "{h}");
    // Not found: where it was looked for.
    let (f, off) = at("shared/common.svh", "\"missing.svh\"");
    let (h, _) = a.hover(f, off).unwrap();
    assert!(h.contains("not found") && h.contains("1 include directory"), "{h}");
}

#[test]
fn deep_include_chains_are_recorded_exactly() {
    // root includes A (which includes H only with X defined), then defines X and includes B,
    // which includes A again: H is reached through root → B → A → H.
    let files = [
        ("a.svh", "`ifdef X\n`include \"h.svh\"\n`endif\n"),
        ("b.svh", "`include \"a.svh\"\n"),
        ("h.svh", "int h;\n"),
        ("root.sv", "`include \"a.svh\"\n`define X\n`include \"b.svh\"\nmodule root; endmodule\n"),
    ];
    let (a, root) = project("chain", "root.sv\n", &files);
    let p = root.join("h.svh");
    let c = a.compile_context(&p, a.vfs.lookup(&p));
    let chain: Vec<(String, u32)> = c.contexts[0].chain.iter().map(|p| (name(&p.uri).to_string(), p.line)).collect();
    assert_eq!(chain, [("root.sv".to_string(), 2), ("b.svh".to_string(), 0), ("a.svh".to_string(), 1)]);
    let _ = Path::new("");
}

#[test]
fn review_cases_of_the_report() {
    let files = [
        // A compilation-unit class in two files: each unit's use gets its own.
        ("a.sv", "class C; endclass\nmodule a; endmodule\n"),
        // A forward declaration and an out-of-block body are no second definition.
        ("b.sv", "typedef class C;\nclass C;\n  extern function void f();\nendclass\nfunction void C::f(); endfunction\nfunction void f(); endfunction\n"),
        // An `ifdef region whose condition is false: one removed line, not the directive's.
        ("c.sv", "`define Y 1\n`ifdef X\nint a;\n`endif\n`undef Y\n`ifdef Y\n`endif\nmodule c;\n`ifdef Z\n"),
        // An unguarded header defining a module, included twice.
        ("twice.svh", "module twice; endmodule\n"),
        ("d.sv", "`include \"twice.svh\"\n`include \"twice.svh\"\nmodule twice; endmodule\n"),
    ];
    let (a, root) = project("review", "a.sv\nb.sv\nc.sv\nd.sv\n", &files);
    let ctx = |f: &str| {
        let p = root.join(f);
        a.compile_context(&p, a.vfs.lookup(&p)).detail.unwrap()
    };
    let b = ctx("b.sv");
    let c_dup: Vec<_> = b.duplicates.iter().map(|x| (x.name.as_str(), x.others.len(), x.rule, x.default.as_ref().map(|p| (name(&p.uri).to_string(), p.line)))).collect();
    assert_eq!(c_dup, [("C", 1, "this compilation unit's own", Some(("b.sv".to_string(), 1)))], "the forward typedef and C::f are no duplicates");
    let c = ctx("c.sv");
    assert_eq!(c.inactive_lines, 1);
    let y = c.macros.iter().find(|m| m.name == "Y").unwrap();
    assert!(y.undefined && y.tested, "{y:?}");
    let x = c.macros.iter().find(|m| m.name == "X").unwrap();
    assert!(x.tested && !x.undefined && !x.used);
    assert!(c.preprocessing.iter().any(|p| p.code == "MISSING_ENDIF" || p.message.contains("endif")), "{:?}", c.preprocessing);
    let tw = ctx("twice.svh");
    assert_eq!(tw.duplicates.len(), 1, "one row for the header's module: {:?}", tw.duplicates);
    assert_eq!(dvi_analysis::ide::context::md_code("a ``b` c"), "``` a ``b` c ```");
    assert_eq!(dvi_analysis::ide::context::md_code("plain"), "`plain`");
}

#[test]
fn hover_on_a_missing_absolute_include() {
    let abs = if cfg!(windows) { "C:/no/such/dir/x.svh" } else { "/no/such/dir/x.svh" };
    let text = format!("`include \"{abs}\"\nmodule m; endmodule\n");
    let (a, root) = project("absinc", "m.sv\n", &[("m.sv", &text)]);
    let f = a.vfs.lookup(&root.join("m.sv")).unwrap();
    let (h, _) = a.hover(f, text.find("x.svh").unwrap() as u32).unwrap();
    assert!(h.contains("absolute path: nothing else is searched"), "{h}");
}
