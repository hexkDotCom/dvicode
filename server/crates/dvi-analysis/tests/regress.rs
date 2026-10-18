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
