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
    /// The test the log names, when the build has no such test (another one was searched).
    pub unknown_test: Option<String>,
    /// The tree was cut by its size before the rest of the path: not known to be missing.
    pub incomplete: bool,
}

/// Whether `name` is one a `$sformatf` with `format` can make: `%d` (with a width or `0`)
/// any digits, `%s` any text, `%h`/`%x` hex digits, `%o` and `%b` theirs, `%%` a `%`.
fn format_makes(format: &str, name: &str) -> bool {
    fn go(f: &[u8], n: &[u8], budget: &mut u32) -> bool {
        *budget = budget.saturating_sub(1);
        if *budget == 0 {
            return false;
        }
        let Some((&c, rest)) = f.split_first() else { return n.is_empty() };
        if c != b'%' {
            return n.first() == Some(&c) && go(rest, &n[1..], budget);
        }
        let mut k = 0;
        while k < rest.len() && rest[k].is_ascii_digit() {
            k += 1;
        }
        let Some(&spec) = rest.get(k) else { return false };
        let after = &rest[k + 1..];
        let class: fn(u8) -> bool = match spec.to_ascii_lowercase() {
            b'%' => return n.first() == Some(&b'%') && go(after, &n[1..], budget),
            b'd' => |b| b.is_ascii_digit(),
            b'h' | b'x' => |b| b.is_ascii_hexdigit(),
            b'o' => |b| (b'0'..=b'7').contains(&b),
            b'b' => |b| b == b'0' || b == b'1',
            b's' | b'p' => |_| true,
            _ => return false,
        };
        // One character at least, as many as the rest allows.
        let mut j = 0;
        while j < n.len() && class(n[j]) {
            j += 1;
            if go(after, &n[j..], budget) {
                return true;
            }
        }
        false
    }
    go(format.as_bytes(), name.as_bytes(), &mut 10_000)
}

/// Whether a child named `child` (made with `format`, when computed) is the one a log's path
/// segment `seg` names: the same name; a name the format makes; or, for a child that stands for
/// an array made in a loop (`agents[i]`, a symbolic index), `seg` with any index there.
fn segment_is(seg: &str, child: &VerifNode) -> u8 {
    if child.name == seg {
        return 3;
    }
    if child.name_format.as_deref().is_some_and(|f| format_makes(f, seg)) {
        return 2;
    }
    let (Some((stem, idx)), Some((cstem, cidx))) = (seg.split_once('['), child.name.split_once('[')) else { return 0 };
    let (want, have): (Vec<&str>, Vec<&str>) = (idx.trim_end_matches(']').split("][").collect(), cidx.trim_end_matches(']').split("][").collect());
    // The same number of indices; each a symbolic one of the child (`i`) or the same number.
    let concrete = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    let fits = want.len() == have.len() && want.iter().zip(&have).all(|(w, h)| if concrete(h) { w == h } else { !h.is_empty() });
    if stem == cstem && have.iter().any(|h| !concrete(h)) && fits {
        1
    } else {
        0
    }
}

/// The children of `node` along `segs` reaching deepest: each segment matched by the best kind
/// of match first (same name, format, array), trying the next sibling when a branch ends early
/// (two `agent%0d` siblings: `agent1.drv` may be under the second); the first deepest wins.
fn deepest<'n>(node: &'n VerifNode, segs: &[&str], budget: &mut usize) -> (usize, Vec<&'n VerifNode>) {
    let Some(seg) = segs.first() else { return (0, Vec::new()) };
    let mut cands: Vec<(u8, &VerifNode)> = node.children.iter().map(|c| (segment_is(seg, c), c)).filter(|(k, _)| *k > 0).collect();
    cands.sort_by_key(|(k, _)| std::cmp::Reverse(*k));
    let mut best: (usize, Vec<&VerifNode>) = (0, Vec::new());
    for (_, c) in cands {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let (n, rest) = deepest(c, &segs[1..], budget);
        if n + 1 > best.0 {
            let mut path = vec![c];
            path.extend(rest);
            best = (n + 1, path);
            if best.0 == segs.len() {
                break;
            }
        }
    }
    best
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
        // A test the build does not have: said, and another one searched.
        let unknown_test = test.filter(|t| !tests.iter().any(|x| x == t)).map(str::to_string);
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
            let mut budget = 100_000usize;
            let (_, path) = deepest(root, &segs[1..], &mut budget);
            let mut names = vec![keyed(&root.class, root.dup)];
            names.extend(path.iter().map(|c| keyed(&c.name, c.dup)));
            let node: &VerifNode = path.last().copied().unwrap_or(root);
            let matched = 1 + path.len();
            if best.as_ref().is_none_or(|b| matched > b.matched) {
                let incomplete = matched < segs.len() && node.truncated;
                best = Some(ComponentHit { test: t, names, matched, segments: segs.len(), class: node.class.clone(), uri: node.def_uri.clone(), line: node.def_line, unknown_test: unknown_test.clone(), incomplete });
            }
            // Found whole: no other test can do better (ties keep the first).
            if best.as_ref().is_some_and(|b| b.matched == segs.len()) {
                break;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::{format_makes, segment_is, written_parts};
    use crate::ide::views::VerifNode;

    #[test]
    fn array_indices_are_compared_one_by_one() {
        let node = |n: &str| VerifNode { name: n.to_string(), ..Default::default() };
        assert_eq!(segment_is("banks[0][4]", &node("banks[0][i]")), 1);
        assert_eq!(segment_is("banks[1][4]", &node("banks[0][i]")), 0, "a concrete index is that element");
        assert_eq!(segment_is("agents[3]", &node("agents[i]")), 1);
        assert_eq!(segment_is("agents[3]", &node("agents[0]")), 0);
        assert_eq!(segment_is("agents[3][1]", &node("agents[i]")), 0, "another number of indices");
        assert_eq!(segment_is("env", &node("env")), 3);
    }

    #[test]
    fn formats_make_names() {
        assert!(format_makes("agent%0d", "agent12"));
        assert!(format_makes("agent_%d_%s", "agent_3_rx"));
        assert!(format_makes("r[%0d]", "r[7]"));
        assert!(format_makes("m%h", "mff"));
        assert!(!format_makes("agent%0d", "agent"));
        assert!(!format_makes("agent%0d", "agentx"));
        assert!(!format_makes("a%d", "b1"));
        assert!(format_makes("100%%", "100%"));
    }

    #[test]
    fn written_parts_drop_dots_and_drives() {
        assert_eq!(written_parts("../tb/./sb.sv"), ["tb", "sb.sv"]);
        assert_eq!(written_parts("C:\\work\\rtl\\a.sv"), ["work", "rtl", "a.sv"]);
        assert_eq!(written_parts("/nfs/p//x.svh"), ["nfs", "p", "x.svh"]);
    }
}
