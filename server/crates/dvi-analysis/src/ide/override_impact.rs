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
use super::views::{OverrideStep, TreeQuery, VerifNode};
use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::DeclKind;
use dvi_syntax::{FileId, Span, TokKind};
use std::sync::Arc;

/// One test's hierarchy as the model builds it (objects included).
pub struct TestModel {
    pub root: VerifNode,
    /// The model stopped before its end (a node limit, a budget).
    pub incomplete: bool,
    /// Its nodes (what keeping it costs).
    pub nodes: usize,
}

/// Tests modeled at most when no test is configured, and besides it.
const MAX_TESTS: usize = 8;
/// Creations listed per kind of result.
const MAX_LISTED: usize = 10;
/// Nodes of the models kept at most (one analysis revision): past it, the ones kept go.
const MAX_KEPT_NODES: usize = 400_000;

impl Analysis {
    /// The model of test `test`, made once per analysis revision (a cancelled build is not kept).
    pub fn test_model(&self, test: DeclRef) -> Option<Arc<TestModel>> {
        let key = self.decl_key(self.canonical(test));
        if let Some(m) = self.override_models.lock().unwrap().get(&key) {
            return Some(m.clone());
        }
        let mut q = TreeQuery::new(200_000);
        q.objects = true;
        let name = self.decl(test).name.to_string();
        let (h, meta) = self.verification_tree(Some(&name), &q);
        let root = h.roots.into_iter().next()?;
        let mut nodes = 0;
        let mut todo = vec![&root];
        while let Some(n) = todo.pop() {
            nodes += 1;
            todo.extend(&n.children);
        }
        let m = Arc::new(TestModel { root, incomplete: meta.incomplete.is_some(), nodes });
        if !self.cancelled() {
            let mut kept = self.override_models.lock().unwrap();
            if kept.values().map(|m| m.nodes).sum::<usize>() + nodes > MAX_KEPT_NODES {
                kept.clear();
            }
            kept.insert(key, m.clone());
        }
        Some(m)
    }

    /// The tests a hover on an override in class `class` reads: the configured one (or, when
    /// none is configured, the first 8), and the tests derived from `class` (an override in a
    /// test class or its base), at most 8 more. Notes say what was left out.
    fn hover_tests(&self, class: Option<DeclRef>) -> (Vec<Arc<TestModel>>, Vec<String>) {
        let tests = self.uvm_tests();
        let mut picked: Vec<DeclRef> = Vec::new();
        let mut notes = Vec::new();
        match self.setup.config.uvm_testname() {
            Some(name) => match tests.iter().find(|t| self.decl(**t).name == name.as_str()) {
                Some(t) => picked.push(*t),
                None => notes.push(format!("the configured test `{name}` is not a test of this build")),
            },
            None => {
                picked.extend(tests.iter().take(MAX_TESTS));
                if tests.len() > MAX_TESTS {
                    notes.push(format!("{} more tests not modeled", tests.len() - MAX_TESTS));
                }
            }
        }
        if let Some(c) = class {
            let ck = self.decl_key(self.canonical(c));
            let derived: Vec<DeclRef> = tests
                .iter()
                .copied()
                .filter(|t| !picked.iter().any(|p| self.decl_key(*p) == self.decl_key(*t)))
                .filter(|t| {
                    let mut cur = Some(*t);
                    let mut steps = 0;
                    while let Some(x) = cur {
                        if self.decl_key(self.canonical(x)) == ck {
                            return true;
                        }
                        steps += 1;
                        if steps > 32 {
                            return false;
                        }
                        cur = self.class_base(x);
                    }
                    false
                })
                .collect();
            if derived.len() > MAX_TESTS {
                notes.push(format!("{} more tests derived from `{}` not modeled", derived.len() - MAX_TESTS, self.decl(c).name));
            }
            picked.extend(derived.into_iter().take(MAX_TESTS));
        }
        let models = picked.into_iter().filter_map(|t| self.test_model(t)).collect();
        (models, notes)
    }

    /// The override call whose method name is at `off`.
    fn override_call_at(&self, file: FileId, off: u32) -> Option<(OverrideCall, Span)> {
        let toks = self.file_tokens(file)?;
        let k = toks.partition_point(|t| t.span.end < off);
        let t = toks.get(k).filter(|t| t.span.start <= off && t.kind == TokKind::Ident && t.text.starts_with("set_") && t.text.contains("_override"))?;
        let at = t.span;
        let uid = self.context_unit(file)?;
        // Code the chosen context removes is not read (with no choice, as before).
        if self.chosen_unit(file).is_some() && self.unit(uid).inactive.iter().any(|s| s.file == file && s.start <= off && off < s.end) {
            return None;
        }
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
        let from = self.decl(call.from).name.to_string();
        let to = self.decl(call.to).name.to_string();
        let from_key = self.decl_key(self.canonical(call.from));
        let what = if call.instance { format!("instance override `{from}` → `{to}` at `{}`", call.path) } else { format!("type override `{from}` → `{to}`") };
        let mut md = format!("**Factory {what}**");
        let uid = self.context_unit(file)?;
        let class = self.enclosing_class(uid, self.scope_at(uid, file, off));
        let (tests, mut notes) = self.hover_tests(class);
        if tests.is_empty() {
            md.push_str("\n\nNo UVM test is modeled in this build configuration.");
            if !notes.is_empty() {
                md.push_str(&format!("\n\n*{}.*", notes.join("; ")));
            }
            return Some((md, at));
        }
        let mine = |s: &OverrideStep| s.at == Some(at);
        // A pattern known for certain: written as a full path (a relative one depends on where
        // each registration is made).
        let pattern: Option<String> = (call.instance && !call.relative).then(|| call.path.clone());
        for model in &tests {
            let root = &model.root;
            if model.incomplete {
                notes.push(format!("the model of `{}` stopped before its end (a limit)", root.class));
            }
            // Taken part in anywhere in this test? Else nothing here says why not.
            let mut met = false;
            walk(root, &mut |n| met |= n.overrides.iter().any(mine));
            if !met {
                md.push_str(&format!("

**Test `{}`** (as modeled): no creation takes part in it (it may not be registered in this test).", root.class));
                continue;
            }
            let (mut changes, mut may, mut not) = (Vec::new(), Vec::new(), Vec::new());
            walk(root, &mut |n| {
                let steps: Vec<&OverrideStep> = n.overrides.iter().filter(|s| mine(s)).collect();
                if let Some(s) = steps.iter().find(|s| s.applied) {
                    changes.push(format!("`{}` ({} → {})", n.path, s.from, s.to));
                } else if let Some(s) = steps.first() {
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
                            "registered after it was made, a path that does not match, or not met before it".to_string()
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
