//! `factory-incompatible-constructor`: a class registered with the UVM factory whose
//! constructor the factory cannot call.

use super::registration::{registrations, Registration};
use crate::lint::facts::UvmRole;
use crate::lint::{Cx, Level, Rule, RuleMeta, Ruleset};
use dvi_syntax::ast::{dflags, DeclKind};
use dvi_syntax::{TokKind, Token};

pub struct IncompatibleConstructor;

static META: RuleMeta = RuleMeta {
    id: "factory-incompatible-constructor",
    guide: &["CLS-8"],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &["component", "object"],
    summary: "The factory cannot call the constructor of a registered class.",
    why: "The UVM factory constructs a component with new(name, parent) and an object with new() and new(name); a constructor that needs other arguments fails when the factory first creates the class, often far from the class itself.",
    params: &[],
    text_only: false,
};

impl Rule for IncompatibleConstructor {
    fn meta(&self) -> &'static RuleMeta {
        &META
    }

    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let facts = cx.facts.clone();
        let u = a.unit(cx.unit).clone();
        for Registration { class, call, form } in registrations(cx, &u) {
            // An abstract class is never constructed by the factory.
            if form.abstract_ {
                continue;
            }
            if facts.repeated {
                cx.unknown("repeated inclusion");
                continue;
            }
            // The registry the macro makes decides the call: the component registry calls
            // new(name, parent), the object registry new() and new(name), whatever the class is
            // (a mismatch is uvm-registration-mismatch's).
            match a.lint_facts.class(a, class).role {
                Ok(UvmRole::Component | UvmRole::Object) => {}
                Ok(_) => continue,
                Err(_) => {
                    cx.unknown_at("class ancestry", a.decl(class).name_span);
                    continue;
                }
            }
            let role = form.component;
            // UVM's own constructor macros write a constructor the factory can call.
            let d = a.decl(class);
            let uvm = a.lint_facts.uvm_files(a);
            if u.macro_uses.iter().any(|m| {
                matches!(m.name.as_str(), "uvm_object_new" | "uvm_component_new")
                    && m.name_span.file == d.span.file
                    && d.span.start <= m.name_span.start
                    && m.name_span.end <= d.span.end
                    && m.def.as_ref().is_some_and(|x| uvm.contains(&x.name_span.file))
            }) {
                continue;
            }
            let cname = a.decl(class).name.clone();
            // The class's own constructor (in the class, or its extern prototype); none: the
            // implicit `new()`.
            let ctor = u.parsed.lookup_member(class.decl, "new").iter().copied().find(|d| u.parsed.decl(*d).kind == DeclKind::Function);
            let (formals, at) = match ctor {
                None => (Vec::new(), a.decl(class).name_span),
                Some(d) => {
                    let decl = u.parsed.decl(d);
                    if decl.has(dflags::FROM_MACRO) || crate::lint::facts::in_macro_call(a, cx.unit, decl.name_span) {
                        cx.unknown_at("macro", decl.name_span);
                        continue;
                    }
                    let toks = if decl.name_span.file == cx.file { None } else { a.file_tokens(decl.name_span.file) };
                    let t: &[Token] = match &toks {
                        Some(t) => t,
                        None => &facts.tokens,
                    };
                    match formals(t, decl.name_span.start) {
                        Some(f) => (f, decl.name_span),
                        None => {
                            cx.unknown("constructor header");
                            continue;
                        }
                    }
                }
            };
            let related = vec![(call.call_span, format!("registered with `{}", call.name))];
            let own = ctor.is_some();
            let what = if own { format!("the constructor of {cname}") } else { format!("{cname} has no constructor; its implicit new()") };
            if role {
                let bad = formals.len() < 2 || formals.iter().skip(2).any(|d| !d);
                if bad {
                    cx.report(Some("component"), at, format!("{what} cannot be called as new(name, parent), which is how the factory creates a component: declare `function new(string name, uvm_component parent)`"), related);
                }
            } else if u.macros_after.contains_key("UVM_OBJECT_DO_NOT_NEED_CONSTRUCTOR") {
                // UVM 1.2's compatibility mode: the factory calls new() and then set_name().
                if formals.iter().any(|d| !d) {
                    cx.report(Some("object"), at, format!("{what} cannot be called as new(), which is how the factory creates an object with UVM_OBJECT_DO_NOT_NEED_CONSTRUCTOR: give every argument a default value"), related);
                }
            } else {
                let bad = formals.is_empty() || formals.iter().any(|d| !d);
                if bad {
                    let fix = if formals.is_empty() { format!("declare `function new(string name = \"{cname}\")`") } else { "give every argument a default value".to_string() };
                    cx.report(Some("object"), at, format!("{what} cannot be called as both new() and new(name), which is how the factory creates an object: {fix}"), related);
                }
            }
        }
    }
}

/// The formals of the method whose name is at byte `name` in `t`: for each, whether it has a
/// default value. `None` when the header cannot be read.
fn formals(t: &[Token], name: u32) -> Option<Vec<bool>> {
    let k = t.partition_point(|x| x.span.start < name);
    if t.get(k)?.span.start != name {
        return None;
    }
    let Some(open) = t.get(k + 1) else { return Some(Vec::new()) };
    if !open.is_punct("(") {
        // `function new;`: no formals.
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let (mut depth, mut cur_has, mut cur_default) = (0i32, false, false);
    for x in &t[k + 1..] {
        if x.kind == TokKind::Directive {
            return None;
        }
        if x.kind == TokKind::Punct {
            match x.text.as_str() {
                "(" | "[" | "{" => {
                    depth += 1;
                    if depth == 1 {
                        continue;
                    }
                }
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 {
                        if cur_has {
                            out.push(cur_default);
                        }
                        return Some(out);
                    }
                }
                "," if depth == 1 => {
                    out.push(cur_default);
                    (cur_has, cur_default) = (false, false);
                    continue;
                }
                "=" if depth == 1 => cur_default = true,
                _ => {}
            }
        }
        cur_has = true;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> Option<Vec<bool>> {
        let t: Vec<Token> = dvi_syntax::lexer::lex(s, 0).into_iter().filter(|t| t.kind != TokKind::Eof).collect();
        let at = s.find("new").unwrap() as u32;
        formals(&t, at)
    }

    #[test]
    fn constructor_formals() {
        assert_eq!(f("function new(string name, uvm_component parent);"), Some(vec![false, false]));
        assert_eq!(f("function new(string name = \"x\", int a[2] = '{1, 2});"), Some(vec![true, true]));
        assert_eq!(f("function new();"), Some(vec![]));
        assert_eq!(f("function new;"), Some(vec![]));
        assert_eq!(f("extern function new(string name = get_type_name(), uvm_component parent = null);"), Some(vec![true, true]));
        assert_eq!(f("function new(`ARGS);"), None);
    }
}
