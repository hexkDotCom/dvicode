//! Naming rules (style guide NAM-1, NAM-2, NAM-3).

use crate::lint::facts::{in_macro_call, member_class};
use crate::lint::{Cx, Level, Rule, RuleMeta, Ruleset};
use crate::world::DeclRef;
use dvi_syntax::ast::{dflags, DeclId, DeclKind};
use dvi_syntax::TokKind;

pub struct NamingCase;
pub struct NamingPrefix;

static CASE: RuleMeta = RuleMeta {
    id: "naming-case",
    guide: &["NAM-1", "NAM-2"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &["lower", "upper"],
    summary: "A name is not in the case the style guide asks for.",
    why: "lower_snake_case for classes, variables, functions, tasks, instances, signals, packages, interfaces and modules (as the UVM library); UPPER_SNAKE_CASE for parameters, localparams, enum literals and constant macros (NAM-1, NAM-2).",
    params: &[],
    text_only: false,
};

static PREFIX: RuleMeta = RuleMeta {
    id: "naming-prefix",
    guide: &["NAM-3"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "A name starts with `uvm_` or `m_`.",
    why: "`uvm_` belongs to the library and `m_` marks UVM internals; project names with them hide that distinction (NAM-3).",
    params: &[],
    text_only: false,
};

pub fn lower_snake(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b[0].is_ascii_lowercase() && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_') && !s.contains("__") && !s.ends_with('_')
}

/// Why a name in the right case is still not snake case: its underscores.
fn underscores(s: &str, upper: bool) -> &'static str {
    let case_ok = s.bytes().all(|c| c == b'_' || c.is_ascii_digit() || if upper { c.is_ascii_uppercase() } else { c.is_ascii_lowercase() });
    if !case_ok {
        ""
    } else if s.contains("__") {
        " (no double underscores)"
    } else if s.starts_with('_') || s.ends_with('_') {
        " (no underscore at the start or the end)"
    } else {
        ""
    }
}

pub fn upper_snake(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b[0].is_ascii_uppercase() && b.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_') && !s.contains("__") && !s.ends_with('_')
}

/// What case a declaration kind takes: `Some(true)` UPPER, `Some(false)` lower, `None` not
/// checked.
fn case_of(k: DeclKind) -> Option<bool> {
    Some(match k {
        DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam | DeclKind::EnumMember => true,
        DeclKind::Class
        | DeclKind::InterfaceClass
        | DeclKind::Variable
        | DeclKind::Net
        | DeclKind::Port
        | DeclKind::Argument
        | DeclKind::Function
        | DeclKind::Task
        | DeclKind::Instance
        | DeclKind::Module
        | DeclKind::Interface
        | DeclKind::Program
        | DeclKind::Package
        | DeclKind::Typedef
        | DeclKind::Covergroup
        | DeclKind::Coverpoint
        | DeclKind::Cross
        | DeclKind::Constraint
        | DeclKind::Modport
        | DeclKind::ClockingBlock
        | DeclKind::Property
        | DeclKind::Sequence => false,
        _ => return None,
    })
}

/// Declarations whose spelling is someone else's: methods that override a library method or
/// implement an interface class's, and their arguments.
fn spelled_elsewhere(cx: &Cx, i: DeclId) -> bool {
    let a = cx.a;
    let u = a.unit(cx.unit);
    let d = u.parsed.decl(i);
    let method = match d.kind {
        DeclKind::Function | DeclKind::Task => i,
        DeclKind::Argument => match d.parent {
            Some(p) if matches!(u.parsed.decl(p).kind, DeclKind::Function | DeclKind::Task) => p,
            _ => return false,
        },
        _ => return false,
    };
    let m = DeclRef { unit: cx.unit, decl: method };
    let name = &u.parsed.decl(method).name;
    let Some(class) = member_class(a, m) else { return false };
    let library = |r: DeclRef| a.lint_facts.uvm_files(a).contains(&a.decl(r).name_span.file) || !a.is_editable(a.decl(r).name_span.file);
    // The contract comes from a class the project does not own (through any number of project
    // overrides), or from an interface class any class of the chain implements.
    let chain = match &a.lint_facts.class(a, class).ancestry {
        Ok(c) => c.clone(),
        Err(_) => vec![class],
    };
    for c in chain.iter().skip(1) {
        let cu = a.unit(c.unit);
        if cu.parsed.lookup_member(c.decl, name).iter().any(|x| library(DeclRef { unit: c.unit, decl: *x })) {
            return true;
        }
    }
    chain.iter().any(|c| a.class_interfaces(*c).into_iter().any(|ic| a.class_member(ic, name, 0).is_some()))
}

impl Rule for NamingCase {
    fn meta(&self) -> &'static RuleMeta {
        &CASE
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        for (i, d) in u.parsed.decls.iter().enumerate() {
            if d.name_span.file != cx.file || d.name.is_empty() || d.name.starts_with('\\') || d.has(dflags::FROM_MACRO) || d.has(dflags::OUT_OF_BLOCK) || !cx.step(1) {
                continue;
            }
            let Some(upper) = case_of(d.kind) else { continue };
            // Implicit names (`genblk1`), and names a macro call writes.
            if in_macro_call(a, cx.unit, d.name_span) || d.name == "new" {
                continue;
            }
            let ok = if upper { upper_snake(&d.name) } else { lower_snake(&d.name) };
            if ok || spelled_elsewhere(cx, i as DeclId) {
                continue;
            }
            let (sub, want) = if upper { ("upper", "UPPER_SNAKE_CASE") } else { ("lower", "lower_snake_case") };
            cx.report(Some(sub), d.name_span, format!("{} should be {want}{}", d.name, underscores(&d.name, upper)), Vec::new());
        }
        // Constant macros: object-like macros whose body is one literal.
        for m in u.macro_defs.iter() {
            if m.name_span.file != cx.file || m.params.is_some() {
                continue;
            }
            let body: Vec<_> = m.body.iter().filter(|t| t.kind != TokKind::Eof).collect();
            if body.len() == 1 && matches!(body[0].kind, TokKind::Number | TokKind::String) && !upper_snake(&m.name) {
                cx.report(Some("upper"), m.name_span, format!("The constant macro {} should be UPPER_SNAKE_CASE", m.name), Vec::new());
            }
        }
    }
}

impl Rule for NamingPrefix {
    fn meta(&self) -> &'static RuleMeta {
        &PREFIX
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        for (i, d) in u.parsed.decls.iter().enumerate() {
            if d.name_span.file != cx.file || d.has(dflags::FROM_MACRO) || d.has(dflags::OUT_OF_BLOCK) || !(d.name.starts_with("uvm_") || d.name.starts_with("m_")) {
                continue;
            }
            if in_macro_call(a, cx.unit, d.name_span) || spelled_elsewhere(cx, i as DeclId) {
                continue;
            }
            cx.report(None, d.name_span, format!("{} starts with {}: uvm_ belongs to the UVM library and m_ marks its internals", d.name, if d.name.starts_with("m_") { "m_" } else { "uvm_" }), Vec::new());
        }
        for m in u.macro_defs.iter() {
            if m.name_span.file == cx.file && (m.name.starts_with("uvm_") || m.name.starts_with("m_")) {
                cx.report(None, m.name_span, format!("The macro {} starts with a prefix that belongs to UVM", m.name), Vec::new());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases() {
        assert!(lower_snake("bus_driver") && lower_snake("a2b") && !lower_snake("busDriver") && !lower_snake("a__b") && !lower_snake("_a") && !lower_snake("a_"));
        assert!(upper_snake("MAX_LEN") && upper_snake("T") && !upper_snake("MaxLen") && !upper_snake("A__B"));
    }
}
