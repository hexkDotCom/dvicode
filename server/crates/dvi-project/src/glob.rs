//! Ant-style wildcards: `?`, `*` (within one path component) and `**` (any depth).

use std::path::{Path, PathBuf};

pub fn has_wildcard(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

/// Matches one path component against a pattern with `*` and `?`.
pub fn match_component(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
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
    while i < comps.len() && !has_wildcard(&comps[i]) {
        base.push(&comps[i]);
        i += 1;
    }
    let mut out = Vec::new();
    walk(&base, &comps[i..], &mut out, 0);
    out
}

fn walk(dir: &Path, rest: &[String], out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 64 {
        return;
    }
    let Some(first) = rest.first() else {
        out.push(dir.to_path_buf());
        return;
    };
    if first == "**" {
        // Zero directories...
        walk(dir, &rest[1..], out, depth + 1);
        // ...or one more.
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && !e.file_name().to_string_lossy().starts_with('.') {
                    walk(&p, rest, out, depth + 1);
                }
            }
        }
        return;
    }
    if !has_wildcard(first) {
        let p = dir.join(first);
        if p.exists() {
            walk(&p, &rest[1..], out, depth + 1);
        }
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if match_component(first, &e.file_name().to_string_lossy()) {
                walk(&e.path(), &rest[1..], out, depth + 1);
            }
        }
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

    #[test]
    fn components() {
        assert!(match_component("*.sv", "a.sv"));
        assert!(match_component("a?c*", "abcdef"));
        assert!(!match_component("*.sv", "a.svh"));
    }

    #[test]
    fn paths() {
        assert!(matches_path("*/obsolete/*", Path::new("/p/x/obsolete/a.sv")));
        assert!(matches_path("**/*.svh", Path::new("/p/x/y/a.svh")));
        assert!(!matches_path("/q/**", Path::new("/p/a.sv")));
    }
}
