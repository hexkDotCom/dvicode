//! Where the links of a simulator log lead: a source file named by a path that may be from
//! another machine (`/nfs/project/rtl/x.sv` in a log written on a farm), and a UVM component
//! named by its path (`uvm_test_top.env.agents[0].drv`).

use super::views::{keyed, TreeQuery, VerifNode};
use crate::world::Analysis;
use dvi_project::fsx;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The component a log names, as the Verification Hierarchy shows it.
#[derive(Serialize, Debug, Clone)]
pub struct ComponentHit {
    /// The test whose tree was searched.
    pub test: String,
    /// The node names from the root (the test class) to the deepest node matched, as the view
    /// keys them.
    pub names: Vec<String>,
    /// Path segments matched (`uvm_test_top` included), of `segments`.
    pub matched: usize,
    pub segments: usize,
    /// The class of the node reached.
    pub class: String,
    pub uri: Option<String>,
    pub line: u32,
}

/// The components of a path as a log writes it (`/` or `\`), without `.`, `..` and a drive.
fn written_parts(p: &str) -> Vec<&str> {
    p.split(['/', '\\']).filter(|c| !c.is_empty() && *c != "." && *c != ".." && !(c.len() == 2 && c.ends_with(':'))).collect()
}

impl Analysis {
    /// The files a log's `path` may name: the path itself when it exists, relative to `base`
    /// (the log's folder) or to the project; else the files the project compiles whose paths
    /// end with the most components of it (at least its file name). Several when they are
    /// equally good.
    pub fn find_files(&self, path: &str, base: Option<&Path>) -> Vec<PathBuf> {
        let p = Path::new(path);
        if p.is_absolute() && fsx::is_file(p) {
            return vec![p.to_path_buf()];
        }
        if !p.is_absolute() {
            for dir in base.into_iter().chain([self.setup.root.as_path()]) {
                let q = dir.join(p);
                if fsx::is_file(&q) {
                    return vec![dvi_syntax::preproc::normalize(&q)];
                }
            }
        }
        let want = written_parts(path);
        let Some(name) = want.last() else { return Vec::new() };
        let mut best = 0usize;
        let mut out: Vec<PathBuf> = Vec::new();
        for f in self.project_files() {
            let fp = self.path(f);
            let fold = fsx::ignores_case(&fp);
            let same = |a: &str, b: &str| if fold { a.eq_ignore_ascii_case(b) } else { a == b };
            let have: Vec<String> = fp.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            if !have.last().is_some_and(|l| same(l, name)) {
                continue;
            }
            let n = want.iter().rev().zip(have.iter().rev()).take_while(|(a, b)| same(a, b)).count();
            if n > best {
                best = n;
                out.clear();
            }
            if n == best && !out.contains(&fp) {
                out.push(fp);
            }
        }
        out.sort();
        out
    }

    /// The deepest component of `test`'s Verification Hierarchy (else the configured test's,
    /// else the deepest in any test's) along a UVM path from a log. A segment `name[3]` matches
    /// a child named so, else one named `name[...]` (an array made in a loop is one node).
    pub fn component_at(&self, test: Option<&str>, path: &str) -> Option<ComponentHit> {
        let segs: Vec<&str> = path.split('.').collect();
        if segs.first() != Some(&"uvm_test_top") {
            return None;
        }
        let tests: Vec<String> = self.uvm_tests().iter().map(|t| self.decl(*t).name.to_string()).collect();
        let configured = self.setup.config.uvm_testname();
        let wanted: Vec<String> = match (test.filter(|t| tests.iter().any(|x| x == t)), configured.filter(|c| tests.contains(c))) {
            (Some(t), _) => vec![t.to_string()],
            (None, Some(c)) => vec![c],
            (None, None) => tests,
        };
        let mut best: Option<ComponentHit> = None;
        for t in wanted {
            if self.cancelled() {
                break;
            }
            let (h, _) = self.verification_tree(Some(&t), &TreeQuery::new(100_000));
            let Some(root) = h.roots.first() else { continue };
            let mut names = vec![keyed(&root.class, root.dup)];
            let mut node: &VerifNode = root;
            let mut matched = 1;
            for seg in &segs[1..] {
                let stem = seg.split_once('[').map(|(s, _)| s);
                let next = node.children.iter().find(|c| c.name == *seg).or_else(|| stem.and_then(|s| node.children.iter().find(|c| c.name.strip_prefix(s).is_some_and(|r| r.starts_with('[')))));
                let Some(c) = next else { break };
                names.push(keyed(&c.name, c.dup));
                node = c;
                matched += 1;
            }
            if best.as_ref().is_none_or(|b| matched > b.matched) {
                best = Some(ComponentHit { test: t, names, matched, segments: segs.len(), class: node.class.clone(), uri: node.def_uri.clone(), line: node.def_line });
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::written_parts;

    #[test]
    fn written_parts_drop_dots_and_drives() {
        assert_eq!(written_parts("../tb/./sb.sv"), ["tb", "sb.sv"]);
        assert_eq!(written_parts("C:\\work\\rtl\\a.sv"), ["work", "rtl", "a.sv"]);
        assert_eq!(written_parts("/nfs/p//x.svh"), ["nfs", "p", "x.svh"]);
    }
}
