//! Baselines: the findings a project accepted when it adopted lint, so that only new ones are
//! reported. A finding is recognized by a fingerprint that survives edits elsewhere in the file
//! (line numbers are not part of it): its rule, its file, the declaration it is in, its own text
//! and the text of the lines around it.

use super::Finding;
use crate::world::Analysis;
use dvi_syntax::TokKind;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FORMAT_VERSION: u32 = 1;
pub const FINGERPRINT_VERSION: u32 = 1;

/// What identifies a finding across edits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct Fingerprint {
    pub rule: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subcheck: String,
    /// Relative to the project root, with `/`.
    pub path: String,
    /// The innermost declaration around it (`pkg::class::method`), empty at file level.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub symbol: String,
    /// Its tokens, one space apart.
    pub text: String,
    /// A hash of the lines before and after it, whitespace normalized.
    pub around: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub fp: Fingerprint,
    /// Findings with this fingerprint accepted.
    pub count: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BaselineFile {
    pub version: u32,
    pub fingerprint: u32,
    #[serde(default)]
    pub tool: String,
    pub findings: Vec<Entry>,
}

/// A baseline being matched, by file: what is left of each entry's count.
#[derive(Clone, Debug, Default)]
pub struct Baseline {
    files: FxHashMap<String, FileBaseline>,
}

#[derive(Clone, Debug, Default)]
struct FileBaseline {
    exact: FxHashMap<Fingerprint, u32>,
    /// Without `around`: a finding whose neighbourhood changed still matches.
    loose: FxHashMap<Fingerprint, u32>,
}

fn loose(fp: &Fingerprint) -> Fingerprint {
    Fingerprint { around: String::new(), ..fp.clone() }
}

impl Baseline {
    pub fn from_file(f: &BaselineFile) -> Baseline {
        let mut b = Baseline::default();
        for e in &f.findings {
            let x = b.files.entry(e.fp.path.clone()).or_default();
            // Saturating: a file may list the same finding twice, with any counts.
            let c = x.exact.entry(e.fp.clone()).or_default();
            *c = c.saturating_add(e.count);
            let c = x.loose.entry(loose(&e.fp)).or_default();
            *c = c.saturating_add(e.count);
        }
        b
    }

    /// Like [`Baseline::take`] for the findings of one file, leaving the baseline as it is (the
    /// editor matches each file on its own).
    pub fn matched(&self, fps: &[Fingerprint]) -> Vec<bool> {
        let mut out = vec![false; fps.len()];
        // What these findings have used of the baseline's counts (it stays as it is).
        let looses: Vec<Fingerprint> = fps.iter().map(loose).collect();
        let mut used: FxHashMap<&Fingerprint, u32> = FxHashMap::default();
        let mut used_loose: FxHashMap<&Fingerprint, u32> = FxHashMap::default();
        for (i, fp) in fps.iter().enumerate() {
            let Some(x) = self.files.get(&fp.path) else { continue };
            let u = used.entry(fp).or_default();
            if x.exact.get(fp).is_some_and(|n| *n > *u) {
                *u += 1;
                *used_loose.entry(&looses[i]).or_default() += 1;
                out[i] = true;
            }
        }
        for (i, fp) in fps.iter().enumerate() {
            if out[i] {
                continue;
            }
            let Some(x) = self.files.get(&fp.path) else { continue };
            let u = used_loose.entry(&looses[i]).or_default();
            if x.loose.get(&looses[i]).is_some_and(|n| *n > *u) {
                *u += 1;
                out[i] = true;
            }
        }
        out
    }

    /// Reads a baseline file; `Err` says what is wrong with it.
    pub fn read(text: &str) -> Result<Baseline, String> {
        let f: BaselineFile = serde_json::from_str(text).map_err(|e| format!("not a lint baseline: {e}"))?;
        if f.version != FORMAT_VERSION || f.fingerprint != FINGERPRINT_VERSION {
            return Err(format!("written by another version of DVICode (format {}, fingerprint {}): write it again with --write-baseline", f.version, f.fingerprint));
        }
        Ok(Baseline::from_file(&f))
    }

    /// Matches `fps` (the findings of one file, in order) and takes them out of the baseline:
    /// for each, whether the baseline accepts it. Exact matches first, then the rest by the
    /// fingerprint without its neighbourhood; a finding is never matched by its position.
    pub fn take(&mut self, fps: &[Fingerprint]) -> Vec<bool> {
        let mut out = vec![false; fps.len()];
        for (i, fp) in fps.iter().enumerate() {
            let Some(x) = self.files.get_mut(&fp.path) else { continue };
            if let Some(n) = x.exact.get_mut(fp).filter(|n| **n > 0) {
                *n -= 1;
                if let Some(l) = x.loose.get_mut(&loose(fp)) {
                    *l = l.saturating_sub(1);
                }
                out[i] = true;
            }
        }
        for (i, fp) in fps.iter().enumerate() {
            if out[i] {
                continue;
            }
            let Some(x) = self.files.get_mut(&fp.path) else { continue };
            if let Some(n) = x.loose.get_mut(&loose(fp)).filter(|n| **n > 0) {
                *n -= 1;
                out[i] = true;
            }
        }
        out
    }
}

/// Builds a baseline file from fingerprints (sorted, equal ones counted).
pub fn file_of(fps: Vec<Fingerprint>, tool: &str) -> BaselineFile {
    let mut counts: FxHashMap<Fingerprint, u32> = FxHashMap::default();
    for fp in fps {
        *counts.entry(fp).or_default() += 1;
    }
    let mut findings: Vec<Entry> = counts.into_iter().map(|(fp, count)| Entry { fp, count }).collect();
    findings.sort_by(|a, b| a.fp.cmp(&b.fp));
    BaselineFile { version: FORMAT_VERSION, fingerprint: FINGERPRINT_VERSION, tool: tool.to_string(), findings }
}

/// `path` relative to `root`, with `..` for a file outside it (a folder `include` names), so
/// that the baseline does not depend on where the checkout is; absolute only on another drive.
fn rel_path(root: &Path, path: &Path) -> String {
    if let Ok(r) = path.strip_prefix(root) {
        return r.to_string_lossy().into_owned();
    }
    let (r, p): (Vec<_>, Vec<_>) = (root.components().collect(), path.components().collect());
    let common = r.iter().zip(&p).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return path.to_string_lossy().into_owned();
    }
    let mut out: Vec<String> = std::iter::repeat("..".to_string()).take(r.len() - common).collect();
    out.extend(p[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    out.join("/")
}

impl Analysis {
    /// The fingerprint of a finding of `unit`'s view of its file; `root`: the project root.
    pub fn fingerprint(&self, unit: crate::world::UnitId, x: &Finding, root: &Path) -> Fingerprint {
        let f = x.span.file;
        let path = self.path(f);
        let path = rel_path(root, &path).replace('\\', "/");
        let scope = self.scope_at(unit, f, x.span.start);
        let u = self.unit(unit);
        let symbol = if scope == dvi_syntax::ast::ParsedUnit::ROOT {
            String::new()
        } else {
            // Named scopes only, outermost first; an out-of-block method under its class.
            let mut names = Vec::new();
            let mut cur = Some(scope);
            while let Some(s) = cur {
                let d = u.parsed.decl(s);
                if !d.name.is_empty() && s != dvi_syntax::ast::ParsedUnit::ROOT {
                    names.push(d.name.to_string());
                    if d.has(dvi_syntax::ast::dflags::OUT_OF_BLOCK) {
                        if let Some(c) = self.owner_class(crate::world::DeclRef { unit, decl: s }) {
                            names.push(self.decl(c).name.to_string());
                        }
                    }
                }
                cur = d.parent;
            }
            names.reverse();
            names.join("::")
        };
        let toks = self.file_tokens(f).unwrap_or_default();
        let lo = toks.partition_point(|t| t.span.start < x.span.start);
        let hi = toks.partition_point(|t| t.span.start < x.span.end);
        let mut text: String = toks[lo..hi].iter().filter(|t| t.kind != TokKind::Eof).map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        let (src, li) = (self.file_text(f), self.file_lines(f));
        let around = |at: u32, dir: i32| match (&src, &li) {
            (Some(t), Some(li)) => line_around(t, li, at, dir),
            _ => String::new(),
        };
        if text.is_empty() {
            // A finding about whitespace: the line's text.
            text = around(x.span.start, 0);
        }
        let around = format!("{:016x}", fnv(format!("{}\n{}", around(x.span.start, -1), around(x.span.end, 1)).as_bytes()));
        Fingerprint { rule: x.rule.to_string(), subcheck: x.subcheck.unwrap_or("").to_string(), path, symbol, text, around }
    }
}

/// The nearest non-blank line before (`dir` -1), at (0) or after (1) the line of `at`,
/// whitespace normalized (at most 64 lines away).
fn line_around(text: &str, li: &dvi_syntax::LineIndex, at: u32, dir: i32) -> String {
    let n = li.line_count() as i64;
    let line = |i: i64| -> &str {
        let s = li.line_start(i as u32) as usize;
        let e = if i + 1 < n { li.line_start(i as u32 + 1) as usize } else { text.len() };
        text.get(s..e).unwrap_or("")
    };
    let mut i = li.line_of(at.min(text.len() as u32)) as i64;
    if dir == 0 {
        return norm(line(i));
    }
    for _ in 0..64 {
        i += dir as i64;
        if i < 0 || i >= n {
            return String::new();
        }
        let l = norm(line(i));
        if !l.is_empty() {
            return l;
        }
    }
    String::new()
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fnv(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(text: &str, around: &str) -> Fingerprint {
        Fingerprint { rule: "r".into(), subcheck: String::new(), path: "a.sv".into(), symbol: "p::c::f".into(), text: text.into(), around: around.into() }
    }

    #[test]
    fn matched_is_take_without_taking() {
        // Exact first, then loose; equal findings by count.
        let f = file_of(vec![fp("x ;", "1"), fp("x ;", "2"), fp("y ;", "3")], "t");
        let b = Baseline::from_file(&f);
        let fps = [fp("x ;", "2"), fp("x ;", "9"), fp("x ;", "9"), fp("y ;", "3"), fp("z ;", "3")];
        let want = b.clone().take(&fps);
        assert_eq!(b.matched(&fps), want);
        assert_eq!(want, [true, true, false, true, false]);
        // Nothing was taken.
        assert_eq!(b.matched(&fps), want);
    }

    #[test]
    fn counts_saturate() {
        let mut f = file_of(vec![fp("x ;", "1")], "t");
        f.findings[0].count = u32::MAX;
        f.findings.push(Entry { fp: fp("x ;", "1"), count: 1 });
        let b = Baseline::from_file(&f);
        assert_eq!(b.matched(&[fp("x ;", "1")]), [true]);
    }

    #[test]
    fn paths_outside_the_root_are_relative() {
        let root = if cfg!(windows) { Path::new("C:\\w\\proj") } else { Path::new("/w/proj") };
        let p = root.join("a").join("b.sv");
        assert_eq!(rel_path(root, &p).replace('\\', "/"), "a/b.sv");
        let p = root.parent().unwrap().join("shared").join("c.sv");
        assert_eq!(rel_path(root, &p), "../shared/c.sv");
    }

    #[test]
    fn matching_counts_and_never_by_order() {
        let f = file_of(vec![fp("x ;", "1"), fp("x ;", "1"), fp("y ;", "2")], "t");
        assert_eq!(f.findings.len(), 2);
        let mut b = Baseline::from_file(&f);
        // Three equal findings where two were accepted: one is new.
        let got = b.take(&[fp("x ;", "1"), fp("x ;", "1"), fp("x ;", "1")]);
        assert_eq!(got.iter().filter(|x| **x).count(), 2);
        // A neighbourhood that changed still matches.
        let mut b = Baseline::from_file(&f);
        assert_eq!(b.take(&[fp("y ;", "9")]), [true]);
        // Exact matches are taken before loose ones.
        let mut b = Baseline::from_file(&file_of(vec![fp("x ;", "1")], "t"));
        assert_eq!(b.take(&[fp("x ;", "2"), fp("x ;", "1")]), [false, true]);
    }

    #[test]
    fn versions_are_checked() {
        let f = file_of(vec![fp("x ;", "1")], "t");
        let text = serde_json::to_string(&f).unwrap();
        assert!(Baseline::read(&text).is_ok());
        assert!(Baseline::read(&text.replace("\"fingerprint\":1", "\"fingerprint\":2")).unwrap_err().contains("--write-baseline"));
    }
}
