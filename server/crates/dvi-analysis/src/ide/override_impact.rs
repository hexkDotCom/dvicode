//! Factory override impact: from an override call (`set_type_override_by_type(...)` and the
//! other UVM forms), the creations it takes part in making, in the modeled build of a test,
//! and why other creations of the class it overrides are not changed.
//!
//! "Takes part" means: the call's registration is a step of the factory's resolution chain of
//! that creation (`A → B → C`: both steps), as the Verification Hierarchy's model resolves it.
//! Nothing is claimed beyond that model: what it does not follow is not counted, and a reason
//! "not changed" is given as certain only where it is (made with `new`; an instance path that
//! does not match the creation's known lookup path).

use super::factory::OverrideCall;
use super::views::{TreeQuery, VerifNode};
use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::DeclKind;
use dvi_syntax::{FileId, Span, TokKind};
use std::sync::Arc;

/// The hierarchies the impact is read from: the configured test's, or every test's (at most 8).
#[derive(Default)]
pub struct OverrideModel {
    pub tests: Vec<VerifNode>,
    /// The model stopped somewhere (node limit, a budget): results may miss creations.
    pub incomplete: bool,
    /// More tests than the ones modeled.
    pub more_tests: usize,
}

/// Tests modeled at most when no test is configured.
const MAX_TESTS: usize = 8;
/// Creations listed per kind of result.
const MAX_LISTED: usize = 10;

impl Analysis {
    /// The model, made once per analysis revision (a cancelled build is not kept).
    pub fn override_model(&self) -> Arc<OverrideModel> {
        if let Some(m) = self.override_model_cache.get() {
            return m.clone();
        }
        let mut q = TreeQuery::new(200_000);
        q.objects = true;
        let (h, meta) = self.verification_tree(None, &q);
        let (h, meta) = if h.test.is_none() && h.roots.len() > 1 { (h, meta) } else if h.roots.is_empty() { self.verification_tree(Some("*"), &q) } else { (h, meta) };
        let more_tests = h.roots.len().saturating_sub(MAX_TESTS);
        let m = Arc::new(OverrideModel { tests: h.roots.into_iter().take(MAX_TESTS).collect(), incomplete: meta.incomplete.is_some(), more_tests });
        if !self.cancelled() {
            let _ = self.override_model_cache.set(m.clone());
        }
        m
    }

    /// The override call whose method name is at `off`.
    fn override_call_at(&self, file: FileId, off: u32) -> Option<(OverrideCall, Span)> {
        let toks = self.file_tokens(file)?;
        let k = toks.partition_point(|t| t.span.end < off);
        let t = toks.get(k).filter(|t| t.span.start <= off && t.kind == TokKind::Ident && t.text.starts_with("set_") && t.text.contains("_override"))?;
        let at = t.span;
        let &uid = self.units_for(file).first()?;
        // The method around it.
        let mut d = Some(self.scope_at(uid, file, off));
        let u = self.unit(uid);
        let body = loop {
            let id = d?;
            let decl = u.parsed.decl(id);
            if matches!(decl.kind, DeclKind::Function | DeclKind::Task) {
                break DeclRef { unit: uid, decl: id };
            }
            d = decl.parent;
        };
        let span = self.decl(body).span;
        let lo = toks.partition_point(|t| t.span.start < span.start);
        let hi = toks.partition_point(|t| t.span.start < span.end);
        self.override_calls(body, &toks[lo..hi]).into_iter().map(|(_, c)| c).find(|c| c.span == at).map(|c| (c, at))
    }

    /// Hover on an override call: what it changes in the modeled build of each test.
    pub fn override_hover(&self, file: FileId, off: u32) -> Option<(String, Span)> {
        let (call, at) = self.override_call_at(file, off)?;
        let uri = crate::ide::url_of(&self.path(file)).to_string();
        let line = self.to_range(at)?.start.line;
        let from = self.decl(call.from).name.to_string();
        let to = self.decl(call.to).name.to_string();
        let from_key = self.decl_key(self.canonical(call.from));
        let what = if call.instance { format!("instance override `{from}` → `{to}` at `{}`", call.path) } else { format!("type override `{from}` → `{to}`") };
        let mut md = format!("**Factory {what}**");
        let model = self.override_model();
        // An override in a test class the model does not cover: that test is modeled too.
        let mut tests: Vec<&VerifNode> = model.tests.iter().collect();
        let extra;
        if let Some(c) = self.enclosing_class(self.units_for(file).first().copied()?, self.scope_at(self.units_for(file).first().copied()?, file, off)) {
            let cname = self.decl(c).name.to_string();
            let is_test = self.uvm_tests().iter().any(|t| self.decl_key(*t) == self.decl_key(c));
            if is_test && !tests.iter().any(|t| t.class == cname) {
                let mut q = TreeQuery::new(200_000);
                q.objects = true;
                extra = self.verification_tree(Some(&cname), &q).0.roots;
                tests.extend(extra.iter());
            }
        }
        if tests.is_empty() {
            md.push_str("\n\nNo UVM test is modeled in this build configuration.");
            return Some((md, at));
        }
        // The full path pattern: as written, or (relative to the caller) as a step made from it
        // says.
        let mut pattern: Option<String> = (call.instance && !call.relative).then(|| call.path.clone());
        if call.instance && call.relative {
            for root in &tests {
                walk(root, &mut |n| {
                    if pattern.is_none() {
                        pattern = n.overrides.iter().find(|s| s.uri.as_deref() == Some(uri.as_str()) && s.line == line).and_then(|s| s.path.clone());
                    }
                });
            }
        }
        for root in &tests {
            // Taken part in anywhere in this test? Else nothing here says why not.
            let mut met = false;
            walk(root, &mut |n| met |= n.overrides.iter().any(|s| s.uri.as_deref() == Some(uri.as_str()) && s.line == line));
            if !met {
                md.push_str(&format!("

**Test `{}`** (as modeled): no creation takes part in it (it may not be registered in this test).", root.class));
                continue;
            }
            let (mut changes, mut may, mut not) = (Vec::new(), Vec::new(), Vec::new());
            walk(root, &mut |n| {
                let mine: Vec<&super::views::OverrideStep> = n.overrides.iter().filter(|s| s.uri.as_deref() == Some(uri.as_str()) && s.line == line).collect();
                if let Some(s) = mine.iter().find(|s| s.applied) {
                    changes.push(format!("`{}` ({} → {})", n.path, s.from, s.to));
                } else if let Some(s) = mine.first() {
                    let when = if s.conditions.is_empty() { String::new() } else { format!(" when {}", s.conditions.iter().map(|c| c.join(" && ")).collect::<Vec<_>>().join(" or ")) };
                    may.push(format!("`{}`{when}", n.path));
                } else if n.requested_decl.is_some_and(|r| self.decl_key(self.canonical(r)) == from_key) {
                    let why = if n.made_by_factory == Some(false) {
                        "made with `new`: the factory is not asked".to_string()
                    } else if let Some(pat) = pattern.as_deref().filter(|p| !p.starts_with('/') && n.lookup.as_deref().is_some_and(|l| !super::factory::glob_match(p, l))) {
                        format!("its path `{}` does not match `{pat}`", n.lookup.as_deref().unwrap_or(""))
                    } else {
                        let winners: Vec<String> = n.overrides.iter().filter(|s| s.applied).map(|s| format!("{} → {} ({})", s.from, s.to, s.set_in)).collect();
                        if winners.is_empty() {
                            "registered after it was made, or not met before it".to_string()
                        } else {
                            format!("another override comes first: {}", winners.join(", "))
                        }
                    };
                    not.push(format!("`{}`: {why}", n.path));
                }
            });
            md.push_str(&format!("\n\n**Test `{}`** (as modeled)", root.class));
            let list = |title: &str, v: &[String], md: &mut String| {
                if v.is_empty() {
                    return;
                }
                md.push_str(&format!("\n\n{title} ({}):", v.len()));
                for x in v.iter().take(MAX_LISTED) {
                    md.push_str(&format!("\n- {x}"));
                }
                if v.len() > MAX_LISTED {
                    md.push_str(&format!("\n- and {} more", v.len() - MAX_LISTED));
                }
            };
            if changes.is_empty() && may.is_empty() && not.is_empty() {
                md.push_str(": not met in the modeled build.");
            }
            list("Changes", &changes, &mut md);
            list("May change", &may, &mut md);
            list("Not changed", &not, &mut md);
        }
        let mut notes = Vec::new();
        if model.incomplete {
            notes.push("the model stopped before its end (a limit)".to_string());
        }
        if model.more_tests > 0 {
            notes.push(format!("{} more tests not modeled", model.more_tests));
        }
        notes.push("creations the model does not follow (`create_component_by_name`, `create_object`, ...) are not counted".to_string());
        md.push_str(&format!("\n\n*{}.*", notes.join("; ")));
        Some((md, at))
    }
}

fn walk(n: &VerifNode, f: &mut dyn FnMut(&VerifNode)) {
    f(n);
    for c in &n.children {
        walk(c, f);
    }
}
