//! Ant-style wildcards: `?`, `*` (within one path component), `**` (any depth) and character
//! classes `[abc]`, `[a-z]`, `[!abc]` (as in `.gitignore`).

use rustc_hash::FxHashSet;
use std::path::{Path, PathBuf};

pub fn has_wildcard(s: &str) -> bool {
    // A Windows extended-length prefix (`\\?\C:\...`) is not a wildcard.
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    s.contains('*') || s.contains('?')
}

/// A character class (`[ab]`) in some path component: a `[` closed later in the component.
pub fn has_class(s: &str) -> bool {
    s.split(['/', '\\']).any(|c| {
        let p: Vec<char> = c.chars().collect();
        (0..p.len()).any(|i| p[i] == '[' && class_at(&p, i, ' ').is_some())
    })
}

/// Whether a path written in a build file is a pattern to expand: a wildcard, or a character
/// class unless a file or folder has that very name (`a[1].sv`).
pub fn is_pattern(written: &str, abs: &Path) -> bool {
    has_wildcard(written) || (has_class(written) && !abs.exists())
}

/// The character class starting at `p[i]` (`[`): whether it matches `c`, and the index after
/// its `]`. `None` when there is no closing `]` (the `[` is then an ordinary character). A `]`
/// right after `[` or `[!` belongs to the class; `!` or `^` first negates it.
fn class_at(p: &[char], i: usize, c: char) -> Option<(bool, usize)> {
    let mut j = i + 1;
    let negate = matches!(p.get(j), Some('!') | Some('^'));
    if negate {
        j += 1;
    }
    let first = j;
    let mut found = false;
    while j < p.len() && (p[j] != ']' || j == first) {
        if j + 2 < p.len() && p[j + 1] == '-' && p[j + 2] != ']' {
            found |= p[j] <= c && c <= p[j + 2];
            j += 3;
        } else {
            found |= p[j] == c;
            j += 1;
        }
    }
    (j < p.len()).then_some((found != negate, j + 1))
}

/// Matches one path component against a pattern with `*`, `?` and `[...]`.
pub fn match_component(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        let class = (pi < p.len() && p[pi] == '[').then(|| class_at(&p, pi, t[ti])).flatten();
        if let Some((true, next)) = class {
            pi = next;
            ti += 1;
        } else if class.is_none() && pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Expands an absolute pattern to existing paths.
pub fn expand(pattern: &Path) -> Vec<PathBuf> {
    let comps: Vec<String> = pattern.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    // Fixed prefix.
    let mut base = PathBuf::new();
    let mut i = 0;
    while i < comps.len() && !has_wildcard(&comps[i]) && !has_class(&comps[i]) {
        base.push(&comps[i]);
        i += 1;
    }
    let mut out = Vec::new();
    walk(&base, &comps[i..], &mut out, 0, &mut FxHashSet::default());
    out
}

/// `links`: the directories that `**` reached through symbolic links, with the part of the
/// pattern left there, each walked once (links between sibling folders would otherwise branch
/// again at every level).
fn walk(dir: &Path, rest: &[String], out: &mut Vec<PathBuf>, depth: usize, links: &mut FxHashSet<(PathBuf, usize)>) {
    if depth > 64 {
        return;
    }
    let Some(first) = rest.first() else {
        out.push(dir.to_path_buf());
        return;
    };
    if first == "**" {
        // Zero directories...
        walk(dir, &rest[1..], out, depth + 1, links);
        // ...or one more.
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && !e.file_name().to_string_lossy().starts_with('.') && !links_to_ancestor(dir, &e) && first_visit(&e, rest.len(), links) {
                    walk(&p, rest, out, depth + 1, links);
                }
            }
        }
        return;
    }
    // A class names itself when a file or folder has that name.
    if !has_wildcard(first) && (!has_class(first) || dir.join(first).exists()) {
        let p = dir.join(first);
        if p.exists() {
            walk(&p, &rest[1..], out, depth + 1, links);
        }
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if match_component(first, &e.file_name().to_string_lossy()) {
                walk(&e.path(), &rest[1..], out, depth + 1, links);
            }
        }
    }
}

/// Not a symbolic link, or the first one to its target with this much of the pattern left.
fn first_visit(e: &std::fs::DirEntry, left: usize, links: &mut FxHashSet<(PathBuf, usize)>) -> bool {
    if !e.file_type().is_ok_and(|t| t.is_symlink()) {
        return true;
    }
    std::fs::canonicalize(e.path()).is_ok_and(|t| links.insert((t, left)))
}

/// A symbolic link in `dir` to `dir` itself or a directory above it (`a -> .`): following it
/// with `**` would walk the same tree over and over.
fn links_to_ancestor(dir: &Path, e: &std::fs::DirEntry) -> bool {
    if !e.file_type().is_ok_and(|t| t.is_symlink()) {
        return false;
    }
    match (std::fs::canonicalize(dir), std::fs::canonicalize(e.path())) {
        (Ok(d), Ok(t)) => d.starts_with(t),
        _ => true,
    }
}

/// Matches a whole path against a pattern; a relative pattern may match any suffix.
pub fn matches_path(pattern: &str, path: &Path) -> bool {
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let comps: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).filter(|c| c != "/").collect();
    let anchored = pattern.starts_with('/');
    if anchored {
        return match_seq(&pat, &comps);
    }
    (0..=comps.len()).any(|s| match_seq(&pat, &comps[s..]))
}

fn match_seq(pat: &[&str], comps: &[String]) -> bool {
    match pat.first() {
        None => comps.is_empty(),
        Some(&"**") => (0..=comps.len()).any(|k| match_seq(&pat[1..], &comps[k..])),
        Some(p) => !comps.is_empty() && match_component(p, &comps[0]) && match_seq(&pat[1..], &comps[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn links_between_sibling_folders_end() {
        // a/x, a/y -> b and b/x, b/y -> a: every level would branch again.
        let dir = std::env::temp_dir().join(format!("dvicode_glob_links_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["a", "b"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
            std::fs::write(dir.join(d).join(format!("{d}.sv")), "").unwrap();
        }
        for (from, to) in [("a/x", "../b"), ("a/y", "../b"), ("b/x", "../a"), ("b/y", "../a")] {
            std::os::unix::fs::symlink(to, dir.join(from)).unwrap();
        }
        let t = std::time::Instant::now();
        let found = expand(&dir.join("**/*.sv"));
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
        assert!(found.len() <= 8 && found.iter().any(|p| p.ends_with("a/a.sv")) && found.iter().any(|p| p.ends_with("b/b.sv")), "{found:?}");
        // One target reached twice, with different parts of the pattern left: p/a/link while
        // `rtl` is still to come, p/z/rtl/link after it.
        let p = dir.join("p");
        std::fs::create_dir_all(p.join("a")).unwrap();
        std::fs::create_dir_all(p.join("z/rtl")).unwrap();
        std::fs::create_dir_all(dir.join("ip")).unwrap();
        std::fs::write(dir.join("ip/x.sv"), "").unwrap();
        std::os::unix::fs::symlink(dir.join("ip"), p.join("a/link")).unwrap();
        std::os::unix::fs::symlink(dir.join("ip"), p.join("z/rtl/link")).unwrap();
        let found = expand(&p.join("**/rtl/**/*.sv"));
        assert_eq!(found, vec![p.join("z/rtl/link/x.sv")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn character_classes_expand_unless_a_file_has_that_name() {
        let dir = std::env::temp_dir().join(format!("dvicode_glob_classes_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rtl")).unwrap();
        for f in ["rtl/a.sv", "rtl/b.sv", "rtl/c.sv", "rtl/x[1].sv", "rtl/x1.sv"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let p = dir.join("rtl/[ab].sv");
        assert!(is_pattern("rtl/[ab].sv", &p));
        assert_eq!(expand(&p), vec![dir.join("rtl/a.sv"), dir.join("rtl/b.sv")]);
        // A file named with brackets is that file; `[]` is no class.
        assert!(!is_pattern("rtl/x[1].sv", &dir.join("rtl/x[1].sv")));
        assert!(!has_class("gen[]/x.sv") && has_class("gen[ab]/x.sv"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn components() {
        assert!(match_component("*.sv", "a.sv"));
        assert!(match_component("a?c*", "abcdef"));
        assert!(!match_component("*.sv", "a.svh"));
    }

    #[test]
    fn character_classes() {
        assert!(match_component("[abc].sv", "b.sv"));
        assert!(!match_component("[abc].sv", "d.sv"));
        assert!(match_component("v[0-9]*", "v7_old"));
        assert!(!match_component("v[0-9]*", "vx"));
        assert!(match_component("[!a-c]x", "dx"));
        assert!(!match_component("[!a-c]x", "bx"));
        assert!(match_component("[^a]x", "bx"));
        // `]` first is a member; `-` last is a member; no `]` means a plain `[`.
        assert!(match_component("[]a]", "]"));
        assert!(match_component("[a-]", "-"));
        assert!(match_component("a[b", "a[b"));
        assert!(match_component("*[0-9].log", "run12.log"));
        assert!(!match_component("*[0-9].log", "run.log"));
    }

    #[test]
    fn windows_extended_prefix_is_not_a_wildcard() {
        assert!(!has_wildcard(r"\\?\D:\proj\a.sv"));
        assert!(has_wildcard(r"\\?\D:\proj\*.sv"));
    }

    #[test]
    fn paths() {
        assert!(matches_path("*/obsolete/*", Path::new("/p/x/obsolete/a.sv")));
        assert!(matches_path("**/*.svh", Path::new("/p/x/y/a.svh")));
        assert!(!matches_path("/q/**", Path::new("/p/a.sv")));
    }
}
