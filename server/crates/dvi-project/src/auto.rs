//! Auto-configuration: finds the sources of a project without a build file.
//!
//! Every `.sv`/`.v` file that no other file includes is a top file; directories with sources
//! are include directories; files defining packages are compiled before the files that use
//! them, and files that mostly define macros come first.

use crate::progress::{Phase, Progress};
use crate::Invocation;
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
    ft.is_file() || (ft.is_symlink() && std::fs::metadata(p).is_ok_and(|m| m.is_file()))
}

/// The patterns of the project's top-level `.gitignore`: build output, scratch files and
/// downloaded tools usually live there. Negations (`!pattern`) are not supported and skipped.
struct GitIgnore(Vec<(String, bool)>);

impl GitIgnore {
    fn load(root: &Path) -> GitIgnore {
        let text = std::fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
        let mut pats = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') || l.starts_with('!') {
                continue;
            }
            let dir_only = l.ends_with('/');
            let l = l.trim_end_matches('/');
            if l.is_empty() {
                continue;
            }
            // A slash other than a trailing one anchors the pattern at the project folder.
            let pat = if l.contains('/') { format!("/{}", l.trim_start_matches('/')) } else { l.to_string() };
            pats.push((pat, dir_only));
        }
        GitIgnore(pats)
    }

    /// `rel`: path relative to the project folder.
    fn ignores(&self, rel: &Path, is_dir: bool) -> bool {
        self.0.iter().any(|(p, dir_only)| (is_dir || !dir_only) && crate::glob::matches_path(p, rel))
    }
}

/// The project's sources; counts them on `progress`, and stops early when it is cancelled.
pub fn collect_sources_with(root: &Path, progress: &Progress) -> Vec<PathBuf> {
    let ignore = GitIgnore::load(root);
    let rel = |p: &Path| p.strip_prefix(root).map(|r| r.to_path_buf()).unwrap_or_default();
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if progress.is_cancelled() {
            return out;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) && !ignore.ignores(&rel(&p), true) {
                    stack.push(p);
                }
            } else if is_regular_file(&ft, &p) && is_source(&p) && !ignore.ignores(&rel(&p), false) {
                out.push(p);
                progress.step();
                if out.len() >= MAX_FILES {
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

struct FileFacts {
    includes: Vec<String>,
    /// Include operands with a directory part (`common_cells/registers.svh`).
    include_paths: Vec<String>,
    packages: Vec<String>,
    uses: FxHashSet<String>,
    defines: usize,
    items: usize,
}

fn scan(text: &str) -> FileFacts {
    let toks = lex(text, 0);
    let mut f = FileFacts { includes: Vec::new(), include_paths: Vec::new(), packages: Vec::new(), uses: FxHashSet::default(), defines: 0, items: 0 };
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            TokKind::Directive if t.text == "include" => {
                if let Some(n) = toks.get(i + 1).filter(|n| n.kind == TokKind::String) {
                    let p = n.text.trim_matches('"');
                    let base = p.rsplit(['/', '\\']).next().unwrap_or(p);
                    f.includes.push(base.to_string());
                    if base.len() < p.len() {
                        f.include_paths.push(p.replace('\\', "/"));
                    }
                }
            }
            TokKind::Directive if t.text == "define" => f.defines += 1,
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
                std::fs::read(&p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
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
    for inc in include_paths {
        let parts: Vec<&str> = inc.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
        if parts.is_empty() || parts.contains(&"..") || inc.starts_with('/') {
            continue;
        }
        let Some(candidates) = files_by_name.get(std::ffi::OsStr::new(parts[parts.len() - 1])) else { continue };
        for p in candidates {
            let mut comps: Vec<_> = p.components().collect();
            if comps.len() <= parts.len() || !comps.iter().rev().zip(parts.iter().rev()).all(|(c, s)| c.as_os_str() == *s) {
                continue;
            }
            comps.truncate(comps.len() - parts.len());
            let base: PathBuf = comps.iter().collect();
            if seen_dirs.insert(base.clone()) {
                incdirs.push(base);
            }
        }
    }
    // Top files.
    let tops: Vec<usize> = facts
        .iter()
        .enumerate()
        .filter(|(_, (p, _))| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            !is_header(p) && !included.contains(&name)
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
    let mut pkg_tops: Vec<usize> = tops.iter().copied().filter(|t| pkg_file.values().any(|v| v == t)).collect();
    pkg_tops.sort();
    for t in pkg_tops {
        visit(t, &top_uses, &pkg_file, &mut placed, &mut visiting, &mut order);
    }
    for &t in &tops {
        visit(t, &top_uses, &pkg_file, &mut placed, &mut visiting, &mut order);
    }
    let mut inv = Invocation { mode: "default".into(), auto: true, ..Default::default() };
    // UVM used but not part of the project.
    let uses_uvm = top_uses.values().any(|u| u.contains("uvm_pkg")) || included.contains("uvm_macros.svh");
    if uses_uvm && !pkg_file.contains_key("uvm_pkg") {
        // The same order as `-uvm`: UVM_HOME wins over the dvicode.uvmHome setting.
        let home = env
            .get("UVM_HOME")
            .cloned()
            .or_else(|| std::env::var("UVM_HOME").ok())
            .or_else(|| env.get("DVICODE_UVM_HOME").cloned())
            .or_else(|| std::env::var("DVICODE_UVM_HOME").ok())
            .filter(|h| !h.is_empty());
        if let Some(h) = home {
            let h = PathBuf::from(h);
            let src = if h.join("src").is_dir() { h.join("src") } else { h };
            if src.join("uvm_pkg.sv").is_file() {
                inv.incdirs.push(src.clone());
                inv.files.push(src.join("uvm_pkg.sv"));
            }
        }
    }
    inv.files.extend(order.into_iter().map(|i| facts[i].0.clone()));
    inv.incdirs.extend(incdirs);
    inv
}
