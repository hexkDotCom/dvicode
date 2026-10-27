//! `uvm-registration-mismatch`: a UVM registration macro that does not fit the class it is in.

use crate::lint::facts::UvmRole;
use crate::lint::{Cx, Fix, Level, Rule, RuleMeta, Ruleset};
use crate::world::{DeclRef, Unit};
use dvi_syntax::preproc::MacroUse;
use dvi_syntax::ast::DeclKind;
use dvi_syntax::{Span, TokKind};

pub struct RegistrationMismatch;

static META: RuleMeta = RuleMeta {
    id: "uvm-registration-mismatch",
    guide: &["FAC-2"],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &["other-class", "family", "param", "abstract"],
    summary: "A UVM registration macro does not fit its class.",
    why: "The factory creates what the macro registers: another class's name, the object macro on a component, or a non-parameterized macro on a parameterized class registers the wrong thing or fails to compile only where the factory is used.",
    params: &[],
    text_only: false,
};

/// What a registration macro's name says.
#[derive(Clone, Copy)]
pub(super) struct Form {
    pub component: bool,
    pub param: bool,
    pub abstract_: bool,
    pub begin: bool,
}

/// A UVM registration macro written in a class of the file being linted.
pub(super) struct Registration<'u> {
    pub class: DeclRef,
    pub call: &'u MacroUse,
    pub form: Form,
}

/// The registration macros of the UVM library written in the file being linted, each with the
/// innermost class around it (a call outside any class is left out).
pub(super) fn registrations<'u>(cx: &mut Cx, u: &'u Unit) -> Vec<Registration<'u>> {
    let a = cx.a;
    let uvm = a.lint_facts.uvm_files(a);
    // A call belongs to the innermost class around it.
    let facts = cx.facts.clone();
    let mut out = Vec::new();
    for m in &u.macro_uses {
        if m.name_span.file != cx.file || !cx.step(1) {
            continue;
        }
        let Some(f) = form(&m.name) else { continue };
        // UVM's own macro, not a project's of the same name.
        if !m.def.as_ref().is_some_and(|d| uvm.contains(&d.name_span.file)) {
            continue;
        }
        let Some(cid) = facts.classes.innermost(m.name_span) else { continue };
        out.push(Registration { class: DeclRef { unit: cx.unit, decl: cid }, call: m, form: f });
    }
    out
}

fn form(name: &str) -> Option<Form> {
    let (component, rest) = if let Some(r) = name.strip_prefix("uvm_component_") {
        (true, r)
    } else {
        (false, name.strip_prefix("uvm_object_")?)
    };
    let (begin, rest) = match rest.strip_suffix("_begin") {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let (abstract_, rest) = match rest.strip_prefix("abstract_") {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let (param, rest) = match rest.strip_prefix("param_") {
        Some(r) => (true, r),
        None => (false, rest),
    };
    (rest == "utils").then_some(Form { component, param, abstract_, begin })
}

fn macro_name(f: &Form) -> String {
    format!(
        "uvm_{}_{}{}utils{}",
        if f.component { "component" } else { "object" },
        if f.abstract_ { "abstract_" } else { "" },
        if f.param { "param_" } else { "" },
        if f.begin { "_begin" } else { "" }
    )
}

impl Rule for RegistrationMismatch {
    fn meta(&self) -> &'static RuleMeta {
        &META
    }

    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let facts = cx.facts.clone();
        let u = a.unit(cx.unit).clone();
        let uvm = a.lint_facts.uvm_files(a);
        for Registration { class, call: m, form: f } in registrations(cx, &u) {
            if facts.repeated {
                cx.unknown("repeated inclusion");
                continue;
            }
            let cf = a.lint_facts.class(a, class);
            let cname = a.decl(class).name.clone();
            let related = vec![(a.decl(class).name_span, format!("class {cname}"))];
            // other-class: the argument names another class.
            match self.argument_class(cx, m.name_span) {
                Arg::Class(r) if a.decl_key(a.canonical(r)) != a.decl_key(a.canonical(class)) => {
                    let named = a.decl(r).name.clone();
                    cx.report(Some("other-class"), m.call_span, format!("`{} registers class {named}, but it is written in class {cname}: the factory will create {named} where {cname} is asked for", m.name), related.clone());
                    continue;
                }
                Arg::Class(_) => {}
                Arg::Unknown => cx.unknown_at("unresolved macro argument", m.call_span),
            }
            let role = match cf.role {
                Ok(r) => r,
                Err(_) => {
                    cx.unknown_at("class ancestry", m.call_span);
                    continue;
                }
            };
            match (role, f.component) {
                (UvmRole::Component, false) => {
                    let fix = macro_name(&Form { component: true, ..f });
                    let fixes = vec![rename(m.name_span, &fix, true)];
                    cx.report_fix(Some("family"), m.call_span, format!("{cname} is a component but is registered with `{}: use `{fix}", m.name), related.clone(), fixes);
                    continue;
                }
                (UvmRole::Object, true) => {
                    let fix = macro_name(&Form { component: false, ..f });
                    let fixes = vec![rename(m.name_span, &fix, true)];
                    cx.report_fix(Some("family"), m.call_span, format!("{cname} is an object, not a component, but is registered with `{}: use `{fix}", m.name), related.clone(), fixes);
                    continue;
                }
                (UvmRole::Component | UvmRole::Object, _) => {}
                // Not a factory class: the compiler says what is wrong.
                (UvmRole::None | UvmRole::Port, _) => continue,
            }
            if cf.parameterized && !f.param {
                let fix = macro_name(&Form { param: true, ..f });
                let fixes = vec![rename(m.name_span, &fix, false)];
                cx.report_fix(Some("param"), m.call_span, format!("{cname} is parameterized but `{} registers it by name: use `{fix} (parameterized classes are registered by type)", m.name), related.clone(), fixes);
                continue;
            }
            if cf.is_virtual && !f.abstract_ {
                let abs = macro_name(&Form { abstract_: true, ..f });
                // Only where the library has abstract registration (IEEE 1800.2).
                if u.macros_after.get(abs.as_str()).is_some_and(|d| uvm.contains(&d.name_span.file)) {
                    cx.report(Some("abstract"), m.call_span, format!("{cname} is a virtual class: register it with `{abs} (the factory cannot construct it)"), related);
                }
            }
        }
    }
}

/// Replaces the macro called at `name` (its `` `name `` token) with `` `to ``.
fn rename(name: Span, to: &str, safe: bool) -> Fix {
    Fix { title: format!("Use `{to}"), edits: vec![(name, format!("`{to}"))], safe }
}

enum Arg {
    Class(DeclRef),
    Unknown,
}

impl RegistrationMismatch {
    /// The class the first argument of the macro called at `name` names.
    fn argument_class(&self, cx: &Cx, name: Span) -> Arg {
        let t = &cx.facts.tokens;
        let k = t.partition_point(|x| x.span.start < name.start);
        if !t.get(k + 1).is_some_and(|x| x.is_punct("(")) {
            return Arg::Unknown;
        }
        // The last name before `#(`, `,` or `)`: `C`, `pkg::C`, `C#(T)`.
        let mut j = k + 2;
        let mut last: Option<Span> = None;
        while let Some(x) = t.get(j) {
            match x.kind {
                TokKind::Ident => last = Some(x.span),
                TokKind::Punct if x.text == "::" => {}
                _ => break,
            }
            j += 1;
        }
        let Some(at) = last else { return Arg::Unknown };
        match cx.a.resolve_at_span(cx.unit, at).and_then(|r| if cx.a.decl(r).kind == DeclKind::Typedef { cx.a.follow_typedef(r) } else { Some(r) }) {
            Some(r) if cx.a.decl(r).kind == DeclKind::Class => Arg::Class(r),
            _ => Arg::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_forms() {
        let f = form("uvm_component_abstract_param_utils_begin").unwrap();
        assert!(f.component && f.abstract_ && f.param && f.begin);
        assert_eq!(macro_name(&f), "uvm_component_abstract_param_utils_begin");
        let f = form("uvm_object_utils").unwrap();
        assert!(!f.component && !f.param && !f.abstract_ && !f.begin);
        assert!(form("uvm_object_utils_end").is_none());
        assert!(form("uvm_field_int").is_none());
        assert!(form("uvm_component_registry").is_none());
    }
}
