//! Choosing the compile context of a file several units compile: the editor features follow
//! the chosen unit (and only it), the choice names its context durably (root, invocation
//! fingerprint, nth), and a choice the build no longer compiles falls back to the default.

use dvi_analysis::select::{ContextChoices, ContextKey};
use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use dvi_syntax::FileId;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn write_project(name: &str, build: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("dvicode-select-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    for (f, text) in files {
        let p = root.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
    root
}

fn build(root: &Path) -> Analysis {
    let cfg = dvi_project::load(root, None, &FxHashMap::default());
    Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(root, cfg, FxHashMap::default())))
}

fn rebuild(root: &Path, build_text: &str) -> Analysis {
    std::fs::write(root.join(".dvicode/default.build"), build_text).unwrap();
    build(root)
}

const HEADER: &str = "`ifdef X
  int only_x;
  initial only_x = 1;
`else
  int only_nox;
  initial only_nox = missing_nox;
`endif
  int both;
";

const TWO: &str = "+define+X\na.sv\n+dvicode_init+second\nb.sv\n";

fn two_roots(name: &str) -> (Analysis, PathBuf) {
    let root = write_project(
        name,
        TWO,
        &[("h.svh", HEADER), ("a.sv", "module ma;\n`include \"h.svh\"\nendmodule\n"), ("b.sv", "module mb;\n`include \"h.svh\"\nendmodule\n")],
    );
    (build(&root), root)
}

fn file(a: &Analysis, root: &Path, f: &str) -> FileId {
    a.vfs.lookup(&root.join(f)).unwrap()
}

fn off(text: &str, needle: &str) -> u32 {
    text.find(needle).unwrap_or_else(|| panic!("no {needle}")) as u32
}

/// Chooses for `f` the context of the unit whose root is `root_file`.
fn choose(a: &mut Analysis, root: &Path, f: &str, root_file: &str) -> Vec<FileId> {
    let h = file(a, root, f);
    let r = file(a, root, root_file);
    let uid = a.units_for(h).into_iter().find(|u| a.unit(*u).root == r).unwrap();
    let key = a.context_key(uid);
    let mut c = ContextChoices::default();
    c.0.insert(root.join(f), key);
    a.select_contexts(&c, 1)
}

fn undeclared(a: &Analysis, f: FileId) -> Vec<String> {
    let mut v: Vec<String> = a.file_diagnostics(f).into_iter().filter(|d| d.code == "UNDECLARED_IDENTIFIER").map(|d| d.message).collect();
    v.sort();
    v
}

/// The lines semantic tokens mark as inactive code (token type 14).
fn inactive_lines(a: &Analysis, f: FileId) -> Vec<u32> {
    let mut line = 0;
    let mut out = Vec::new();
    for t in a.semantic_tokens(f) {
        line += t.delta_line;
        if t.token_type == 14 {
            out.push(line);
        }
    }
    out
}

#[test]
fn editor_features_follow_the_chosen_context() {
    let (mut a, root) = two_roots("follow");
    let h = file(&a, &root, "h.svh");
    let (ua, ub) = (file(&a, &root, "a.sv"), file(&a, &root, "b.sv"));
    // By default: the first unit (a.sv, X defined), hover falling through to b.sv's.
    assert_eq!(a.unit(a.context_unit(h).unwrap()).root, ua);
    assert!(undeclared(&a, h).is_empty());
    assert_eq!(inactive_lines(&a, h), [4, 5]);
    let on_nox = off(HEADER, "only_nox;") + 2;
    assert!(a.hover(h, on_nox).is_some(), "falls through to the unit that compiles it");
    let on_x = off(HEADER, "only_x;") + 2;
    assert!(a.hover(h, on_x).is_some());

    let changed = choose(&mut a, &root, "h.svh", "b.sv");
    assert_eq!(changed, [h]);
    assert_eq!(a.unit(a.context_unit(h).unwrap()).root, ub);
    assert_eq!(undeclared(&a, h).len(), 1, "{:?}", undeclared(&a, h));
    assert!(undeclared(&a, h)[0].contains("missing_nox"));
    assert_eq!(inactive_lines(&a, h), [0, 1, 2], "the `ifdef line from its macro name on, and the branch");
    assert!(a.hover(h, on_nox).is_some());
    assert!(a.hover(h, on_x).is_none(), "no fall-through to another unit when a context is chosen");
    // Completion after the `endif sees the chosen branch's declaration.
    let at = off(HEADER, "  int both") + 2;
    let labels: Vec<String> = a.completions(h, at).into_iter().map(|c| c.label).collect();
    assert!(labels.iter().any(|l| l == "only_nox") && !labels.iter().any(|l| l == "only_x"), "{labels:?}");

    // Choosing the first context explicitly: no fall-through either.
    let changed = choose(&mut a, &root, "h.svh", "a.sv");
    assert_eq!(changed, [h]);
    assert!(a.hover(h, on_nox).is_none());
    assert!(undeclared(&a, h).is_empty());
    // No choice again: as by default.
    assert_eq!(a.select_contexts(&ContextChoices::default(), 2), [h]);
    assert!(a.hover(h, on_nox).is_some());
    // The roots themselves have one context each and are unaffected.
    assert_eq!(a.context_list(ua, None, 0, 10).total, 1);
}

#[test]
fn a_root_of_two_invocations_aggregates_only_by_default() {
    let r = "module r;\n`ifdef X\n  initial x_missing = 1;\n`else\n  initial nox_missing = 1;\n`endif\nendmodule\n";
    let root = write_project("aggregate", "+define+X\nr.sv\n+dvicode_init+second\nr.sv\n", &[("r.sv", r)]);
    let mut a = build(&root);
    let f = file(&a, &root, "r.sv");
    assert_eq!(a.units_for(f).len(), 2);
    let all = undeclared(&a, f);
    assert_eq!(all.len(), 2, "{all:?}");
    let second = a.units_for(f)[1];
    let mut c = ContextChoices::default();
    c.0.insert(root.join("r.sv"), a.context_key(second));
    a.select_contexts(&c, 1);
    let one = undeclared(&a, f);
    assert_eq!(one.len(), 1, "{one:?}");
    assert!(one[0].contains("nox_missing"));
}

#[test]
fn keys_survive_sections_inserted_before_and_tell_identical_sections_apart() {
    let (a, root) = two_roots("durable");
    let h = file(&a, &root, "h.svh");
    let ub = file(&a, &root, "b.sv");
    let kb = a.context_key(a.units_for(h)[1]);
    assert_eq!((kb.root.as_path(), kb.nth), (root.join("b.sv").as_path(), 0));
    let ka = a.context_key(a.units_for(h)[0]);
    assert_ne!(ka.invocation, kb.invocation);
    // A section added first: indices shift, the key still names b.sv's context.
    std::fs::write(root.join("z.sv"), "module z; endmodule\n").unwrap();
    let mut a2 = rebuild(&root, &format!("+dvicode_init+zero\nz.sv\n+dvicode_init+default\n{TWO}"));
    let h2 = file(&a2, &root, "h.svh");
    let u = a2.resolve_context(h2, &kb).expect("resolved after the insert");
    assert_eq!((a2.unit(u).root, a2.unit(u).invocation), (file(&a2, &root, "b.sv"), 2));
    let mut c = ContextChoices::default();
    c.0.insert(root.join("h.svh"), kb.clone());
    a2.select_contexts(&c, 1);
    assert_eq!(a2.context_unit(h2), Some(u));
    assert_eq!(a2.context_list(h2, None, 0, 0).state, "chosen");
    // A define of that invocation changed: another context now, the choice is unavailable and
    // the default is used.
    let mut a3 = rebuild(&root, "+define+X\na.sv\n+dvicode_init+second\n+define+Y\nb.sv\n");
    let h3 = file(&a3, &root, "h.svh");
    assert!(a3.resolve_context(h3, &kb).is_none());
    a3.select_contexts(&c, 1);
    assert_eq!(a3.unit(a3.context_unit(h3).unwrap()).root, file(&a3, &root, "a.sv"));
    let l = a3.context_list(h3, None, 0, 0);
    assert_eq!(l.state, "unavailable");
    assert!(l.unavailable.as_ref().unwrap().0.ends_with("/b.sv"));
    let rep = a3.compile_context(&root.join("h.svh"), Some(h3));
    assert_eq!(rep.choice, "unavailable");
    // Restored: it applies again.
    let mut a4 = rebuild(&root, TWO);
    let h4 = file(&a4, &root, "h.svh");
    a4.select_contexts(&c, 1);
    assert_eq!(a4.unit(a4.context_unit(h4).unwrap()).root, file(&a4, &root, "b.sv"));
    let _ = ub;
    // Two sections that preprocess alike, both compiling a.sv: nth tells them apart.
    let a5 = rebuild(&root, "a.sv\n+dvicode_init+default\na.sv\n");
    let h5 = file(&a5, &root, "h.svh");
    let units = a5.units_for(h5);
    assert_eq!(units.len(), 2);
    let keys: Vec<ContextKey> = units.iter().map(|u| a5.context_key(*u)).collect();
    assert_eq!((keys[0].nth, keys[1].nth), (0, 1));
    assert_eq!(keys[0].invocation, keys[1].invocation);
    for (u, k) in units.iter().zip(&keys) {
        assert_eq!(a5.resolve_context(h5, k), Some(*u));
    }
}

#[test]
fn the_report_and_the_list_show_the_choice() {
    let mut files: Vec<(String, String)> = vec![("h.svh".into(), HEADER.into())];
    let mut build_text = String::new();
    for i in 1..=5 {
        files.push((format!("a{i}.sv"), format!("module m{i};\n`include \"h.svh\"\nendmodule\n")));
        build_text.push_str(&format!("a{i}.sv\n"));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let root = write_project("list", &build_text, &refs);
    let mut a = build(&root);
    let h = file(&a, &root, "h.svh");
    let l = a.context_list(h, None, 0, 2);
    assert_eq!((l.total, l.matched, l.items.len(), l.state), (5, 5, 2, "default"));
    assert_eq!(l.items.iter().map(|i| i.ordinal).collect::<Vec<_>>(), [1, 2]);
    assert!(l.items[0].includer.as_ref().is_some_and(|p| p.uri.ends_with("/a1.sv") && p.line == 1));
    assert!(l.chosen.is_none() && l.default.as_ref().is_some_and(|d| d.ordinal == 1));
    choose(&mut a, &root, "h.svh", "a5.sv");
    let l = a.context_list(h, None, 2, 2);
    assert_eq!(l.items.iter().map(|i| (i.ordinal, i.chosen)).collect::<Vec<_>>(), [(3, false), (4, false)]);
    let c = l.chosen.as_ref().expect("the choice, whatever the page");
    assert!(c.ordinal == 5 && c.chosen && c.root.ends_with("/a5.sv"));
    let f = a.context_list(h, Some("A3"), 0, 10);
    assert_eq!((f.matched, f.items.len()), (1, 1));
    assert!(f.items[0].root.ends_with("/a3.sv"));
    let meta = a.context_list(h, None, 0, 0);
    assert!(meta.items.is_empty() && meta.total == 5 && meta.state == "chosen");
    assert_eq!(meta.chosen.as_ref().map(|c| c.ordinal), Some(5));
    // The report marks it, and describes it in detail.
    let rep = a.compile_context(&root.join("h.svh"), Some(h));
    assert_eq!(rep.choice, "chosen");
    assert_eq!(rep.contexts.iter().filter(|c| c.chosen).map(|c| c.ordinal).collect::<Vec<_>>(), [5]);
}

#[test]
fn a_choice_resolves_again_after_an_update() {
    let (mut a, root) = two_roots("update");
    let h = file(&a, &root, "h.svh");
    choose(&mut a, &root, "h.svh", "b.sv");
    let chosen = a.context_unit(h);
    // b.sv edited: its unit is compiled again (same id); the choice still applies.
    let b = a.vfs.set_overlay(&root.join("b.sv"), Arc::from("module mb;\n  int extra;\n`include \"h.svh\"\nendmodule\n"));
    let mut next = a.update_with(&[b], &dvi_project::Progress::new()).unwrap();
    let mut c = ContextChoices::default();
    c.0.insert(root.join("h.svh"), a.context_key(chosen.unwrap()));
    assert!(next.select_contexts(&c, 1).is_empty(), "the same unit");
    assert_eq!(next.context_unit(h), chosen);
    // b.sv no longer includes it: unavailable, the default is used, and the caches that depend on
    // the choice are dropped.
    a.vfs.set_overlay(&root.join("b.sv"), Arc::from("module mb;\nendmodule\n"));
    let mut last = next.update_with(&[b], &dvi_project::Progress::new()).unwrap();
    let _ = last.hidden_uses();
    assert_eq!(last.select_contexts(&c, 1), [h]);
    assert!(last.hidden_uses.get().is_none());
    assert_eq!(last.unit(last.context_unit(h).unwrap()).root, file(&last, &root, "a.sv"));
    assert_eq!(last.context_list(h, None, 0, 0).state, "unavailable");
}

#[test]
fn the_fingerprint_is_stable() {
    let inv = dvi_project::Invocation {
        mode: "default".into(),
        defines: vec![("X".into(), None), ("W".into(), Some("8".into()))],
        undefines: vec!["Y".into()],
        incdirs: vec![PathBuf::from("/p/inc")],
        ..Default::default()
    };
    // Clients keep it from one engine version to the next: this value must not change.
    assert_eq!(dvi_analysis::select::invocation_key(&inv), GOLDEN);
    let other = dvi_project::Invocation { defines: vec![("X".into(), Some(String::new())), ("W".into(), Some("8".into()))], ..inv.clone() };
    assert_ne!(dvi_analysis::select::invocation_key(&other), GOLDEN, "`+define+X` and `+define+X=` differ");
}

const GOLDEN: &str = "v1-434fccc030a8a427";

#[test]
fn alike_sections_are_named_by_what_is_compiled_before() {
    let root = write_project("alike", "a.sv
", &[("h.svh", HEADER), ("a.sv", "module ma;
`include \"h.svh\"
endmodule
"), ("z.sv", "module z; endmodule
")]);
    let a = build(&root);
    let h = file(&a, &root, "h.svh");
    let single = a.context_key(a.units_for(h)[0]);
    assert_eq!((single.nth, single.before.is_none()), (0, true));
    // Two sections that preprocess alike compile a.sv: the old key, made when there was one,
    // could mean either.
    let a2 = rebuild(&root, "a.sv
+dvicode_init+default
a.sv
");
    let h2 = file(&a2, &root, "h.svh");
    assert!(a2.resolve_context(h2, &single).is_none());
    let second = a2.context_key(a2.units_for(h2)[1]);
    assert_eq!((second.nth, second.before.is_some()), (1, true), "nothing before either: told apart by order only");
    // A section put before them: the second of the two still.
    let a3 = rebuild(&root, "+dvicode_init+zero
z.sv
+dvicode_init+default
a.sv
+dvicode_init+default
a.sv
");
    let h3 = file(&a3, &root, "h.svh");
    assert_eq!(a3.resolve_context(h3, &second).map(|u| a3.unit(u).invocation), Some(2));
    // A file before a.sv in the first: each has its own `before`, the second keeps nth 0.
    let a4 = rebuild(&root, "z.sv
a.sv
+dvicode_init+default
a.sv
");
    let h4 = file(&a4, &root, "h.svh");
    let k = a4.context_key(a4.units_for(h4)[1]);
    assert_eq!(k.nth, 0);
    assert_ne!(k.before, a4.context_key(a4.units_for(h4)[0]).before);
}

#[test]
fn config_db_calls_are_read_in_the_chosen_context() {
    let hdr = "`ifdef X\n  initial uvm_config_db#(int)::set(null, \"*\", \"fa\", 1);\n`else\n  initial uvm_config_db#(int)::set(null, \"*\", \"fb\", 1);\n`endif\n";
    let root = write_project(
        "configdb",
        TWO,
        &[("h.svh", hdr), ("a.sv", "module ma;\n`include \"h.svh\"\nendmodule\n"), ("b.sv", "module mb;\n`include \"h.svh\"\nendmodule\n")],
    );
    let mut a = build(&root);
    let h = file(&a, &root, "h.svh");
    let fields = |a: &Analysis| -> Vec<String> { a.config_index().calls.iter().filter(|c| c.method.file == h).filter_map(|c| c.field.clone()).collect() };
    assert_eq!(fields(&a), ["fa"]);
    choose(&mut a, &root, "h.svh", "b.sv");
    assert_eq!(fields(&a), ["fb"]);
    let at = off(hdr, "\"fb\"") + 1;
    assert!(a.config_hover(h, at).is_some());
    assert!(a.config_hover(h, off(hdr, "\"fa\"") + 1).is_none(), "the call of the other context");
}

#[test]
fn a_chosen_unit_that_is_gone_is_never_used() {
    // Two open files outside the build include the header: their ad-hoc units are its contexts.
    let root = write_project("adhoc", "z.sv\n", &[("h.svh", HEADER), ("x.sv", "module mx;\n`include \"h.svh\"\nendmodule\n"), ("y.sv", "`define X\nmodule my;\n`include \"h.svh\"\nendmodule\n"), ("z.sv", "module z; endmodule\n")]);
    let a = build(&root);
    let x = a.vfs.set_overlay(&root.join("x.sv"), Arc::from(std::fs::read_to_string(root.join("x.sv")).unwrap()));
    let y = a.vfs.set_overlay(&root.join("y.sv"), Arc::from(std::fs::read_to_string(root.join("y.sv")).unwrap()));
    let mut a = a.add_adhoc(x).add_adhoc(y);
    let h = file(&a, &root, "h.svh");
    assert_eq!(a.units_for(h).len(), 2);
    assert_eq!(a.context_list(h, None, 0, 10).items.iter().map(|i| i.root_role).collect::<Vec<_>>(), ["adhoc", "adhoc"]);
    choose(&mut a, &root, "h.svh", "y.sv");
    assert_eq!(a.unit(a.context_unit(h).unwrap()).root, y);
    assert!(undeclared(&a, h).is_empty(), "X is defined in y.sv's context");
    // y.sv closed: its unit goes, with the selection still naming it (no install between).
    let b = a.remove_adhoc(y);
    assert_eq!(b.unit(b.context_unit(h).unwrap()).root, x);
    assert_eq!(b.editor_units(h).len(), 1);
    assert!(b.hover(h, off(HEADER, "only_nox;") + 2).is_some());
    let _ = b.file_diagnostics(h);
}

#[test]
fn alike_sections_swapped_keep_their_compilation() {
    // root.sv after pre_x.sv (which defines X) in one section, after pre_y.sv in another: the
    // sections preprocess alike, the compilations of h.svh differ.
    let root = write_project(
        "swap",
        "pre_x.sv\nroot.sv\n+dvicode_init+default\npre_y.sv\nroot.sv\n",
        &[("h.svh", HEADER), ("pre_x.sv", "`define X\n"), ("pre_y.sv", "module y; endmodule\n"), ("root.sv", "module mr;\n`include \"h.svh\"\nendmodule\n")],
    );
    let a = build(&root);
    let h = file(&a, &root, "h.svh");
    let after_y = a.units_for(h)[1];
    assert_eq!(a.unit(after_y).invocation, 1);
    let key = a.context_key(after_y);
    assert!(key.before.is_some());
    let a2 = rebuild(&root, "pre_y.sv\nroot.sv\n+dvicode_init+default\npre_x.sv\nroot.sv\n");
    let h2 = file(&a2, &root, "h.svh");
    let u = a2.resolve_context(h2, &key).expect("found where it went");
    assert_eq!(a2.unit(u).invocation, 0, "the section after pre_y.sv, now first");
    let mut c = ContextChoices::default();
    c.0.insert(root.join("h.svh"), key);
    let mut a2 = a2;
    a2.select_contexts(&c, 1);
    assert!(undeclared(&a2, h2).iter().any(|m| m.contains("missing_nox")), "X is not defined there");
}

#[test]
fn symbols_and_folding_follow_the_chosen_context() {
    let (mut a, root) = two_roots("symbols");
    let h = file(&a, &root, "h.svh");
    let names = |a: &Analysis| -> Vec<String> {
        fn walk(s: &[lsp_types::DocumentSymbol], out: &mut Vec<String>) {
            for x in s {
                out.push(x.name.clone());
                walk(x.children.as_deref().unwrap_or(&[]), out);
            }
        }
        let mut out = Vec::new();
        walk(&a.document_symbols(h), &mut out);
        out
    };
    let folds = |a: &Analysis| a.folding_ranges(h).iter().map(|r| (r.start_line, r.end_line)).collect::<Vec<_>>();
    let (n0, f0) = (names(&a), folds(&a));
    choose(&mut a, &root, "h.svh", "b.sv");
    let (n1, f1) = (names(&a), folds(&a));
    assert!(n0.iter().any(|n| n == "only_x") && !n0.iter().any(|n| n == "only_nox"), "{n0:?}");
    assert!(n1.iter().any(|n| n == "only_nox") && !n1.iter().any(|n| n == "only_x"), "{n1:?}");
    assert_ne!(f0, f1, "the removed region folds");
}

#[test]
fn a_library_context_can_be_chosen_for_config_db_calls() {
    let hdr = "`ifdef X\n  initial uvm_config_db#(int)::set(null, \"*\", \"fa\", 1);\n`else\n  initial uvm_config_db#(int)::set(null, \"*\", \"fb\", 1);\n`endif\n";
    let root = write_project(
        "libctx",
        "top.sv\n-v lib.sv\n",
        &[("h.svh", hdr), ("top.sv", "module top;\n`include \"h.svh\"\n  libmod u();\nendmodule\n"), ("lib.sv", "`define X\nmodule libmod;\n`include \"h.svh\"\nendmodule\n")],
    );
    let mut a = build(&root);
    let h = file(&a, &root, "h.svh");
    assert_eq!(a.context_list(h, None, 0, 10).items.iter().map(|i| i.root_role).collect::<Vec<_>>(), ["top", "library"]);
    let fields = |a: &Analysis| -> Vec<String> { a.config_index().calls.iter().filter(|c| c.method.file == h).filter_map(|c| c.field.clone()).collect() };
    assert_eq!(fields(&a), ["fb"]);
    choose(&mut a, &root, "h.svh", "lib.sv");
    assert_eq!(fields(&a), ["fa"]);
}

#[test]
fn skipped_code_notices_are_those_of_the_chosen_context() {
    // No build file: pa.sv defines X before the header's class, pb.sv does not; user.sv uses a
    // member that pa's copy declares only without X.
    let hdr = "class hc;\n`ifdef X\n  int only_x;\n`else\n  int only_nox;\n`endif\nendclass\n";
    let root = write_project(
        "skipped",
        "",
        &[
            ("h.svh", hdr),
            // Macros flow from file to file here: X is not left for pb.sv.
            ("pa.sv", "`define X\npackage pa;\n`include \"h.svh\"\nendpackage\n`undef X\n"),
            ("pb.sv", "package pb;\n`include \"h.svh\"\nendpackage\n"),
            ("user.sv", "module u;\n  pa::hc o;\n  initial o.only_nox = 1;\nendmodule\n"),
        ],
    );
    let _ = std::fs::remove_file(root.join(".dvicode/default.build"));
    let cfg = dvi_project::load_auto(&root, &FxHashMap::default());
    let mut a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    let h = file(&a, &root, "h.svh");
    let notices = |a: &Analysis| a.file_diagnostics(h).into_iter().filter(|d| d.code == "SKIPPED_CODE").count();
    assert_eq!(a.unit(a.context_unit(h).unwrap()).root, file(&a, &root, "pa.sv"));
    assert_eq!(notices(&a), 1, "{:?}", a.file_diagnostics(h));
    // In pb.sv's context that code is compiled: no notice on it.
    choose(&mut a, &root, "h.svh", "pb.sv");
    assert_eq!(notices(&a), 0, "{:?}", a.file_diagnostics(h));
}
