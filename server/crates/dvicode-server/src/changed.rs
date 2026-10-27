//! What changed since a Git revision, by file and line, for `lint --changed-since`.

use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The changes of one file.
#[derive(Debug, PartialEq)]
pub enum Changed {
    /// Added, renamed or untracked: every line counts as changed.
    Whole,
    /// These lines (1-based, inclusive ranges) of the current text.
    Lines(Vec<(u32, u32)>),
}

/// The files changed since `rev` (since the merge base of `rev` and `HEAD`, unless `exact`),
/// staged, unstaged and untracked changes included, by absolute path.
pub fn since(root: &Path, rev: &str, exact: bool) -> Result<FxHashMap<PathBuf, Changed>, String> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(top.trim());
    let base = if exact { rev.to_string() } else { git(root, &["merge-base", rev, "HEAD"])?.trim().to_string() };
    let diff = git(root, &["diff", "--unified=0", "--no-color", "--no-ext-diff", "-M", &base, "--"])?;
    let mut out = parse_diff(&diff, &top);
    // NUL-separated: names as they are, unquoted.
    for f in git(root, &["ls-files", "-z", "--others", "--exclude-standard", "--full-name"])?.split('\0').filter(|l| !l.is_empty()) {
        out.insert(top.join(f), Changed::Whole);
    }
    Ok(out)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("cannot run git: {e}"))?;
    if !o.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()));
    }
    String::from_utf8(o.stdout).map_err(|_| "git printed text that is not UTF-8".to_string())
}

/// Reads `git diff --unified=0` output: the changed lines of each file's new text.
pub fn parse_diff(diff: &str, top: &Path) -> FxHashMap<PathBuf, Changed> {
    let mut out: FxHashMap<PathBuf, Changed> = FxHashMap::default();
    let mut cur: Option<PathBuf> = None;
    let mut whole = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            cur = None;
            whole = false;
        } else if line.starts_with("new file mode") || line.starts_with("rename from") || line.starts_with("copy from") {
            whole = true;
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if p == "/dev/null" {
                cur = None;
                continue;
            }
            // A name with spaces ends with a tab; a quoted one is unquoted before its `b/` goes.
            let p = unquote(p.trim_end_matches('\t'));
            let p = p.strip_prefix("b/").unwrap_or(&p);
            let path = top.join(p);
            out.insert(path.clone(), if whole { Changed::Whole } else { Changed::Lines(Vec::new()) });
            cur = Some(path);
        } else if line.starts_with("rename to ") && whole {
            // A rename without content changes has no `+++` line.
            let p = unquote(line["rename to ".len()..].trim_end_matches('\t'));
            out.insert(top.join(p), Changed::Whole);
        } else if let (Some(h), Some(path)) = (line.strip_prefix("@@ "), cur.as_ref()) {
            // `@@ -a[,b] +c[,d] @@`: lines c..c+d-1 are new; d = 0 is a deletion after line c.
            let Some(plus) = h.split_whitespace().find(|w| w.starts_with('+')) else { continue };
            let mut it = plus[1..].split(',');
            let c: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let d: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
            // A deletion after line c changes the line that now follows it.
            let range = if d == 0 { (c + 1, c + 1) } else { (c, c + d - 1) };
            if let Some(Changed::Lines(v)) = out.get_mut(path) {
                v.push(range);
            }
        }
    }
    out
}

/// A path as Git prints it, quoted when it has unusual characters.
fn unquote(p: &str) -> String {
    let Some(inner) = p.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else { return p.to_string() };
    let mut out: Vec<u8> = Vec::new();
    let b = inner.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            let c = b[i + 1];
            if c.is_ascii_digit() && i + 3 < b.len() {
                if let Ok(v) = u8::from_str_radix(&inner[i + 1..i + 4], 8) {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
            out.push(match c {
                b'n' => b'\n',
                b't' => b'\t',
                other => other,
            });
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Whether lines `first..=last` (1-based) of `path` changed.
pub fn touches(changed: &FxHashMap<PathBuf, Changed>, path: &Path, first: u32, last: u32) -> bool {
    let key = changed.keys().find(|k| same(k, path));
    match key.and_then(|k| changed.get(k)) {
        None => false,
        Some(Changed::Whole) => true,
        Some(Changed::Lines(v)) => v.iter().any(|(a, b)| *a <= last && first <= *b),
    }
}

fn same(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().replace('\\', "/").to_lowercase() == b.to_string_lossy().replace('\\', "/").to_lowercase()
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_lines() {
        let d = "diff --git a/a.sv b/a.sv\nindex 1..2 100644\n--- a/a.sv\n+++ b/a.sv\n@@ -3 +3 @@\n-x\n+y\n@@ -10,2 +10,0 @@\n-p\n-q\n@@ -20,0 +19,3 @@\n+r\n\
diff --git a/n.sv b/n.sv\nnew file mode 100644\n--- /dev/null\n+++ b/n.sv\n@@ -0,0 +1,2 @@\n+a\n+b\n\
diff --git a/o.sv b/m.sv\nsimilarity index 100%\nrename from o.sv\nrename to m.sv\n";
        let top = Path::new("/r");
        let c = parse_diff(d, top);
        assert_eq!(c[&top.join("a.sv")], Changed::Lines(vec![(3, 3), (11, 11), (19, 21)]));
        assert_eq!(c[&top.join("n.sv")], Changed::Whole);
        assert_eq!(c[&top.join("m.sv")], Changed::Whole);
        assert!(touches(&c, &top.join("a.sv"), 11, 11));
        assert!(!touches(&c, &top.join("a.sv"), 10, 10));
        let q = parse_diff("diff --git a/x y.sv b/x y.sv\n--- a/x y.sv\t\n+++ b/x y.sv\t\n@@ -1 +1 @@\n-a\n+b\n\
diff --git \"a/q\\303\\251.sv\" \"b/q\\303\\251.sv\"\n--- \"a/q\\303\\251.sv\"\n+++ \"b/q\\303\\251.sv\"\n@@ -2 +2 @@\n-a\n+b\n", top);
        assert_eq!(q[&top.join("x y.sv")], Changed::Lines(vec![(1, 1)]));
        assert_eq!(q[&top.join("qé.sv")], Changed::Lines(vec![(2, 2)]));
        assert!(!touches(&c, &top.join("a.sv"), 5, 9));
        assert!(!touches(&c, &top.join("b.sv"), 1, 1));
        assert_eq!(unquote("\"sp ace\\303\\251.sv\""), "sp aceé.sv");
    }
}
