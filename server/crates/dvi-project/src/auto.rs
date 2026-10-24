//! Auto-configuration: finds the sources of a project without a build file.
//!
//! Every `.sv`/`.v` file that no other file includes is a top file; directories with sources
//! are include directories; files defining packages are compiled before the files that use
//! them, and files that mostly define macros come first.

use crate::progress::{Phase, Progress};
use crate::{fsx, Invocation};
use dvi_syntax::lexer::lex;
use dvi_syntax::token::TokKind;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 50_000;
const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", "work", "INCA_libs", "xcelium.d", "csrc", "simv.daidir", "out",
    "__pycache__", "sim_build", "obj_dir", "transcript", "DVEfiles", "verdiLog", "cov_work",
];

fn is_source(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("sv" | "v" | "svh" | "vh" | "svi" | "svp" | "vp" | "sva" | "vlib")
    )
}

fn is_header(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("svh" | "vh" | "svi")
    )
}

/// A file, or a link to one: a link to a device (`x.sv -> /dev/zero`) would be read forever.
fn is_regular_file(ft: &std::fs::FileType, p: &Path) -> bool {
    ft.is_file() || (ft.is_symlink() && fsx::is_file(p))
}

/// The patterns of the project's top-level `.gitignore`: build output, scratch files and
/// downloaded tools usually live there. Negations (`!pattern`) are not supported and skipped.
/// The project's top-level `.gitignore`, in order.
struct GitIgnore(Vec<IgnorePattern>);

struct IgnorePattern {
    pat: String,
    /// Written with a trailing `/`.
    dir_only: bool,
    /// `!pattern`: re-includes.
    negate: bool,
    /// `dir/**`: what is inside `dir`, not `dir` itself (so `!dir/keep.sv` can re-include).
    contents: bool,
}

impl GitIgnore {
    fn load(root: &Path) -> GitIgnore {
        // A regular file of sensible size: a checked-out link to /dev/zero would never end.
        let text = crate::read_input(&root.join(".gitignore"), 16 << 20).unwrap_or_default();
        GitIgnore::parse(&String::from_utf8_lossy(&text))
    }

    fn parse(text: &str) -> GitIgnore {
        let mut pats = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            // `!pattern` re-includes what an earlier line ignored; `\!` and `\#` are literal.
            let (negate, l) = match l.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, l.strip_prefix('\\').filter(|r| r.starts_with('!') || r.starts_with('#')).unwrap_or(l)),
            };
            let (contents, l) = match l.strip_suffix("/**") {
                // `/**`: everything in the project folder.
                Some("") => (false, "**"),
                Some(dir) => (true, dir),
                None => (false, l),
            };
            let dir_only = l.ends_with('/');
            let l = l.trim_end_matches('/');
            if l.is_empty() {
                continue;
            }
            // A slash other than a trailing one anchors the pattern at the project folder.
            let pat = if l.contains('/') || contents { format!("/{}", l.trim_start_matches('/')) } else { l.to_string() };
            pats.push(IgnorePattern { pat, dir_only, negate, contents });
        }
        if !pats.is_empty() {
            let shown: Vec<String> = pats.iter().map(|p| format!("{}{}{}{}", if p.negate { "!" } else { "" }, p.pat, if p.dir_only { "/" } else { "" }, if p.contents { "/**" } else { "" })).collect();
            log::debug!("auto-configuration: skipping what {} patterns of .gitignore match: {}", pats.len(), shown.join(" "));
        }
        GitIgnore(pats)
    }

    /// `rel`: path relative to the project folder. The last line that matches decides, as in
    /// Git; a file in an ignored folder is not seen at all (the folder is not walked).
    fn ignores(&self, rel: &Path, is_dir: bool) -> bool {
        self.0
            .iter()
            .rev()
            .find(|p| {
                if p.contents {
                    // A folder above `rel` (not `rel` itself) matches.
                    rel.ancestors().skip(1).filter(|a| !a.as_os_str().is_empty()).any(|a| crate::glob::matches_path(&p.pat, a))
                } else {
                    (is_dir || !p.dir_only) && crate::glob::matches_path(&p.pat, rel)
                }
            })
            .is_some_and(|p| !p.negate)
    }
}

/// The project's sources; counts them on `progress`, and stops early when it is cancelled.
pub fn collect_sources_with(root: &Path, progress: &Progress) -> Vec<PathBuf> {
    log::debug!("auto-configuration: scanning {} for sources", root.display());
    let ignore = GitIgnore::load(root);
    let rel = |p: &Path| p.strip_prefix(root).map(|r| r.to_path_buf()).unwrap_or_default();
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut dirs = 0usize;
    while let Some(dir) = stack.pop() {
        if progress.is_cancelled() {
            return out;
        }
        dirs += 1;
        let rd = match fsx::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                log::debug!("auto-configuration: cannot read folder {}: {e}", dir.display());
                continue;
            }
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let why = if name.starts_with('.') {
                    Some("hidden folder")
                } else if SKIP_DIRS.contains(&name.as_str()) {
                    Some("build output or tool folder")
                } else if ignore.ignores(&rel(&p), true) {
                    Some("matched by .gitignore")
                } else {
                    None
                };
                match why {
                    None => stack.push(p),
                    Some(why) if name != ".git" => log::debug!("auto-configuration: skipped {} ({why})", p.display()),
                    Some(_) => {}
                }
            } else if ft.is_symlink() && fsx::is_dir(&p) {
                log::debug!("auto-configuration: not followed: {} (symbolic link to a folder; list its sources in a build file)", p.display());
            } else if is_regular_file(&ft, &p) && is_source(&p) {
                if ignore.ignores(&rel(&p), false) {
                    log::debug!("auto-configuration: skipped {} (matched by .gitignore)", p.display());
                    continue;
                }
                log::trace!("auto-configuration: found {}", p.display());
                out.push(p);
                progress.step();
                if out.len() >= MAX_FILES {
                    log::info!("auto-configuration: stopped after {MAX_FILES} source files; describe large projects with a build file");
                    return out;
                }
            }
        }
    }
    log::debug!("auto-configuration: {} source files in {dirs} folders", out.len());
    out.sort();
    out
}

struct FileFacts {
    includes: Vec<String>,
    /// Include operands with a directory part (`common_cells/registers.svh`).
    include_paths: Vec<String>,
    /// Include operands without one (`registers.svh`).
    bare_includes: Vec<String>,
    packages: Vec<String>,
    uses: FxHashSet<String>,
    defines: usize,
    items: usize,
    /// Statements or processes outside any design element, package or class (`initial`,
    /// `always`, `assign`): a piece of a module body, written to be included in one.
    fragment: bool,
}

/// The include directories that `` `include "dir/name.svh" `` operands in `sources` need, given
/// where the files they include are (`included`, from a compile log): `/p/inc` for an operand
/// `pkg/defs.svh` found at `/p/inc/pkg/defs.svh`.
pub(crate) fn include_bases(sources: &[PathBuf], included: &[PathBuf]) -> Vec<PathBuf> {
    let operands: FxHashSet<String> = sources
        .par_iter()
        .flat_map_iter(|p| {
            let text = fsx::read(p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            scan(&text).include_paths
        })
        .collect();
    let mut by_name: FxHashMap<&str, Vec<String>> = FxHashMap::default();
    let names: Vec<String> = included.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
    for n in &names {
        by_name.entry(n.rsplit('/').next().unwrap_or(n)).or_default().push(n.clone());
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for op in &operands {
        let op = op.trim_start_matches("./");
        let Some(paths) = by_name.get(op.rsplit('/').next().unwrap_or(op)) else { continue };
        for p in paths {
            if let Some(base) = p.strip_suffix(op).and_then(|b| b.strip_suffix('/')) {
                let base = PathBuf::from(base);
                if !out.contains(&base) {
                    out.push(base);
                }
            }
        }
    }
    out
}

fn scan(text: &str) -> FileFacts {
    let toks = lex(text, 0);
    let mut f = FileFacts { includes: Vec::new(), include_paths: Vec::new(), bare_includes: Vec::new(), packages: Vec::new(), uses: FxHashSet::default(), defines: 0, items: 0, fragment: false };
    // How deep in design elements, packages and classes; the body of a `define is skipped.
    let mut depth = 0usize;
    let mut in_define = false;
    for (i, t) in toks.iter().enumerate() {
        if in_define && t.nl_before() {
            in_define = false;
        }
        if !in_define && t.kind == TokKind::Keyword {
            let prev = i.checked_sub(1).map(|j| toks[j].text.as_str());
            match t.text.as_str() {
                // Not `typedef class c;` nor the `interface` of `interface class`.
                "class" if prev != Some("typedef") => depth += 1,
                "interface" if toks.get(i + 1).is_some_and(|n| n.text == "class") => {}
                "module" | "macromodule" | "interface" | "program" | "package" | "checker" if prev != Some("extern") => depth += 1,
                "endmodule" | "endinterface" | "endprogram" | "endpackage" | "endchecker" | "endclass" => depth = depth.saturating_sub(1),
                "initial" | "always" | "always_comb" | "always_ff" | "always_latch" | "final" | "assign" if depth == 0 => f.fragment = true,
                _ => {}
            }
        }
        match t.kind {
            TokKind::Directive if t.text == "include" => {
                if let Some(n) = toks.get(i + 1).filter(|n| n.kind == TokKind::String) {
                    let p = n.text.trim_matches('"');
                    let base = p.rsplit(['/', '\\']).next().unwrap_or(p);
                    f.includes.push(base.to_string());
                    if base.len() < p.len() {
                        f.include_paths.push(p.replace('\\', "/"));
                    } else {
                        f.bare_includes.push(base.to_string());
                    }
                }
            }
            TokKind::Directive if t.text == "define" => {
                f.defines += 1;
                in_define = true;
            }
            TokKind::Keyword if t.text == "package" => {
                if let Some(n) = toks.get(i + 1).filter(|n| n.kind == TokKind::Ident) {
                    f.packages.push(n.text.to_string());
                }
                f.items += 1;
            }
            TokKind::Keyword if matches!(t.text.as_str(), "module" | "interface" | "program" | "class" | "function" | "task") => f.items += 1,
            TokKind::Ident if toks.get(i + 1).is_some_and(|n| n.is_punct("::")) => {
                f.uses.insert(t.text.to_string());
            }
            _ => {}
        }
    }
    f
}

pub fn auto_config(root: &Path, env: &FxHashMap<String, String>) -> Invocation {
    auto_config_with(root, env, &Progress::new())
}

/// [`auto_config`], reporting on `progress`; returns early (an incomplete result the caller
/// throws away) when it is cancelled.
pub fn auto_config_with(root: &Path, env: &FxHashMap<String, String>, progress: &Progress) -> Invocation {
    progress.begin(Phase::Scanning, 0);
    let files = collect_sources_with(root, progress);
    progress.begin(Phase::Examining, files.len());
    // In parallel: on network and WSL drives most of the time is waiting for the reads.
    let facts: Vec<(PathBuf, FileFacts)> = files
        .into_par_iter()
        .map(|p| {
            let text = if progress.is_cancelled() {
                String::new()
            } else {
                crate::read_input(&p, crate::MAX_SOURCE).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
            };
            let f = scan(&text);
            progress.step();
            (p, f)
        })
        .collect();
    let included: FxHashSet<String> = facts.iter().flat_map(|(_, f)| f.includes.iter().cloned()).collect();
    let mut incdirs: Vec<PathBuf> = Vec::new();
    let mut seen_dirs = FxHashSet::default();
    for (p, _) in &facts {
        if let Some(d) = p.parent() {
            if seen_dirs.insert(d.to_path_buf()) {
                incdirs.push(d.to_path_buf());
            }
        }
    }
    // `include "common_cells/registers.svh"` needs the directory that contains `common_cells/`:
    // add it for every project file whose path ends with the include operand.
    let mut include_paths: Vec<&String> = facts.iter().flat_map(|(_, f)| f.include_paths.iter()).collect();
    include_paths.sort();
    include_paths.dedup();
    let mut files_by_name: FxHashMap<&std::ffi::OsStr, Vec<&PathBuf>> = FxHashMap::default();
    for (p, _) in &facts {
        if let Some(n) = p.file_name() {
            files_by_name.entry(n).or_default().push(p);
        }
    }
    // What an include names: the files whose paths end with its operand (`rtl/common.sv` is not
    // `tb/common.sv`); by name only for an operand without a folder or one not found so.
    let mut included_files: FxHashSet<&PathBuf> = FxHashSet::default();
    let mut included_names: FxHashSet<String> = FxHashSet::default();
    for (_, f) in &facts {
        included_names.extend(f.bare_includes.iter().cloned());
    }
    for inc in include_paths {
        let parts: Vec<&str> = inc.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
        let name = inc.rsplit('/').next().unwrap_or(inc);
        if parts.is_empty() || parts.contains(&"..") || inc.starts_with('/') {
            included_names.insert(name.to_string());
            continue;
        }
        let Some(candidates) = files_by_name.get(std::ffi::OsStr::new(parts[parts.len() - 1])) else {
            included_names.insert(name.to_string());
            continue;
        };
        let mut found = false;
        for p in candidates {
            let mut comps: Vec<_> = p.components().collect();
            if comps.len() <= parts.len() || !comps.iter().rev().zip(parts.iter().rev()).all(|(c, s)| c.as_os_str() == *s) {
                continue;
            }
            found = true;
            included_files.insert(*p);
            comps.truncate(comps.len() - parts.len());
            let base: PathBuf = comps.iter().collect();
            if seen_dirs.insert(base.clone()) {
                incdirs.push(base);
            }
        }
        if !found {
            included_names.insert(name.to_string());
        }
    }
    // Top files.
    let tops: Vec<usize> = facts
        .iter()
        .enumerate()
        .filter(|(_, (p, _))| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            !is_header(p) && !included_names.contains(&name) && !included_files.contains(p)
        })
        // A piece of a module body is compiled only where it is included, like a header.
        .filter(|(_, (p, f))| {
            if f.fragment {
                log::debug!("auto-configuration: {} has statements outside any module: compiled only where it is included", p.display());
            }
            !f.fragment
        })
        .map(|(i, _)| i)
        .collect();
    // Package providers.
    let mut pkg_file: FxHashMap<String, usize> = FxHashMap::default();
    for &i in &tops {
        for pk in &facts[i].1.packages {
            pkg_file.entry(pk.clone()).or_insert(i);
        }
    }
    // Packages defined in included files belong to the including top: attribute them by
    // following include names transitively.
    let by_name: FxHashMap<String, usize> = facts
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), i))
        .collect();
    let mut top_uses: FxHashMap<usize, FxHashSet<String>> = FxHashMap::default();
    let mut top_defines: FxHashMap<usize, usize> = FxHashMap::default();
    let mut top_items: FxHashMap<usize, usize> = FxHashMap::default();
    for &t in &tops {
        let mut stack = vec![t];
        let mut visited = FxHashSet::default();
        while let Some(i) = stack.pop() {
            if !visited.insert(i) {
                continue;
            }
            let f = &facts[i].1;
            top_uses.entry(t).or_default().extend(f.uses.iter().cloned());
            *top_defines.entry(t).or_default() += f.defines;
            *top_items.entry(t).or_default() += f.items;
            for pk in &f.packages {
                pkg_file.entry(pk.clone()).or_insert(t);
            }
            for inc in &f.includes {
                if let Some(&j) = by_name.get(inc) {
                    stack.push(j);
                }
            }
        }
    }
    // Order: macro-only files, then a topological order over package dependencies.
    let mut order: Vec<usize> = Vec::new();
    let mut placed = FxHashSet::default();
    let mut macro_files: Vec<usize> = tops
        .iter()
        .copied()
        .filter(|t| top_defines.get(t).copied().unwrap_or(0) > 0 && top_items.get(t).copied().unwrap_or(0) == 0)
        .collect();
    macro_files.sort();
    for t in macro_files {
        placed.insert(t);
        order.push(t);
    }
    fn visit(
        t: usize,
        top_uses: &FxHashMap<usize, FxHashSet<String>>,
        pkg_file: &FxHashMap<String, usize>,
        placed: &mut FxHashSet<usize>,
        visiting: &mut FxHashSet<usize>,
        order: &mut Vec<usize>,
    ) {
        if placed.contains(&t) || !visiting.insert(t) {
            return;
        }
        if let Some(uses) = top_uses.get(&t) {
            let mut deps: Vec<usize> = uses.iter().filter_map(|u| pkg_file.get(u).copied()).filter(|d| *d != t).collect();
            deps.sort();
            for d in deps {
                visit(d, top_uses, pkg_file, placed, visiting, order);
            }
        }
        if placed.insert(t) {
            order.push(t);
        }
    }
    let mut visiting = FxHashSet::default();
    // Package files first, then the rest, each in path order.
    let providers: FxHashSet<usize> = pkg_file.values().copied().collect();
    let mut pkg_tops: Vec<usize> = tops.iter().copied().filter(|t| providers.contains(t)).collect();
    pkg_tops.sort();
    for t in pkg_tops {
        visit(t, &top_uses, &pkg_file, &mut placed, &mut visiting, &mut order);
    }
    for &t in &tops {
        visit(t, &top_uses, &pkg_file, &mut placed, &mut visiting, &mut order);
    }
    if log::log_enabled!(log::Level::Debug) {
        log_summary(&facts, &tops, &included, &order);
    }
    let mut inv = Invocation { mode: "default".into(), auto: true, ..Default::default() };
    // UVM used but not part of the project.
    let uses_uvm = top_uses.values().any(|u| u.contains("uvm_pkg")) || included.contains("uvm_macros.svh");
    if !uses_uvm {
        log::debug!("auto-configuration: the sources do not use UVM");
    } else if pkg_file.contains_key("uvm_pkg") {
        log::debug!("auto-configuration: UVM is part of the folder ({})", facts[pkg_file["uvm_pkg"]].0.display());
    }
    if uses_uvm && !pkg_file.contains_key("uvm_pkg") {
        // The same order as `-uvm`: UVM_HOME wins over the dvicode.uvmHome setting.
        let home = env
            .get("UVM_HOME")
            .cloned()
            .or_else(|| std::env::var("UVM_HOME").ok())
            .or_else(|| env.get("DVICODE_UVM_HOME").cloned())
            .or_else(|| std::env::var("DVICODE_UVM_HOME").ok())
            .filter(|h| !h.is_empty());
        match home {
            Some(h) => {
                let h = PathBuf::from(h);
                let src = if fsx::is_dir(h.join("src")) { h.join("src") } else { h };
                if fsx::is_file(src.join("uvm_pkg.sv")) {
                    log::debug!("auto-configuration: the sources use UVM; compiling it from {}", src.display());
                    inv.incdirs.push(src.clone());
                    inv.files.push(src.join("uvm_pkg.sv"));
                } else {
                    log::info!("auto-configuration: the sources use UVM, but there is no uvm_pkg.sv in {} (UVM_HOME or dvicode.uvmHome)", src.display());
                }
            }
            None => log::info!("auto-configuration: the sources use UVM, but neither UVM_HOME nor dvicode.uvmHome is set, so UVM is not compiled"),
        }
    }
    inv.files.extend(order.into_iter().map(|i| facts[i].0.clone()));
    inv.incdirs.extend(incdirs);
    log::debug!("auto-configuration: {} files to compile, {} include directories", inv.files.len(), inv.incdirs.len());
    inv
}

/// What auto-configuration found, for the log: where a project that is set up wrongly shows.
fn log_summary(facts: &[(PathBuf, FileFacts)], tops: &[usize], included: &FxHashSet<String>, order: &[usize]) {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let headers = facts.len() - tops.len();
    log::debug!("auto-configuration: {} files compiled on their own, {headers} only through `include", tops.len());
    // Headers by extension that nothing includes are never compiled.
    let unused: Vec<&PathBuf> = facts.iter().map(|(p, _)| p).filter(|p| is_header(p) && !included.contains(&name(p))).collect();
    if !unused.is_empty() {
        log::debug!("auto-configuration: {} headers are included by no file and not compiled, e.g. {}", unused.len(), unused.iter().take(5).map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
    }
    // Files that `include files whose name no file in the folder has.
    let names: FxHashSet<String> = facts.iter().map(|(p, _)| name(p)).collect();
    let mut missing: Vec<&String> = included.iter().filter(|i| !names.contains(*i) && i.as_str() != "uvm_macros.svh").collect();
    missing.sort();
    if !missing.is_empty() {
        log::debug!("auto-configuration: {} included files are not in the folder: {}", missing.len(), missing.iter().take(20).map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }
    // Packages defined more than once: each use picks the nearest copy.
    let mut pkgs: FxHashMap<&str, Vec<&PathBuf>> = FxHashMap::default();
    for (p, f) in facts {
        for pk in &f.packages {
            pkgs.entry(pk.as_str()).or_default().push(p);
        }
    }
    let mut dups: Vec<(&str, Vec<&PathBuf>)> = pkgs.into_iter().filter(|(_, v)| v.len() > 1).collect();
    dups.sort();
    if !dups.is_empty() {
        log::debug!("auto-configuration: {} packages are defined in more than one file; uses pick the nearest copy that fits", dups.len());
        for (pk, v) in &dups {
            log::debug!("auto-configuration:   package {pk}: {}", v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
        }
    }
    for (n, &i) in order.iter().enumerate() {
        log::trace!("auto-configuration: compile order {}: {}", n + 1, facts[i].0.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_of_a_module_body_is_a_fragment() {
        let frag = |t: &str| scan(t).fragment;
        assert!(frag("import p::*;\nlogic failed;\ninitial begin\n  failed = 0;\nend\n"));
        assert!(frag("assign a = b;\n"));
        assert!(!frag("module m;\n  initial x = 1;\n  assign a = b;\nendmodule\n"));
        assert!(!frag("typedef class c;\nclass c;\n  task run; endtask\nendclass\n"));
        assert!(!frag("interface class ic;\nendclass\nmodule m; always_comb x = 1; endmodule\n"));
        assert!(!frag("`define GO initial \\n  x = 1;\npackage p; endpackage\n"));
        assert!(!frag("package p;\n  class c; endclass\nendpackage\nprogram t; initial run(); endprogram\n"));
    }

    #[test]
    fn gitignore_exceptions_and_character_classes() {
        let g = GitIgnore::parse("# generated\n*.log\n!keep.log\nbuild*/\n!build_rtl/\nsim[0-9]/\n\\!literal\ntmp/\n!tmp/x.sv\n");
        let ig = |p: &str, dir: bool| g.ignores(Path::new(p), dir);
        assert!(ig("run.log", false));
        // `!` re-includes; the last matching line wins.
        assert!(!ig("keep.log", false));
        assert!(ig("build_x", true) && !ig("build_rtl", true));
        assert!(ig("sim3", true) && !ig("simx", true));
        // `\!` is a literal `!`.
        assert!(ig("!literal", false));
        // A file in an ignored folder is not re-included: the folder is skipped, as in Git.
        assert!(ig("tmp", true));
        // `dir/**` ignores what is in the folder, not the folder: an exception inside works.
        let g = GitIgnore::parse("generated/**\n!generated/keep.sv\n");
        let ig = |p: &str, dir: bool| g.ignores(Path::new(p), dir);
        assert!(!ig("generated", true));
        assert!(ig("generated/x.sv", false) && ig("generated/sub", true));
        assert!(!ig("generated/keep.sv", false));
        assert!(!ig("rtl/generated", true));
        let g = GitIgnore::parse("/**\n!top.sv\n");
        let ig = |p: &str, dir: bool| g.ignores(Path::new(p), dir);
        assert!(ig("x.sv", false) && ig("rtl", true) && !ig("top.sv", false));
    }
}
