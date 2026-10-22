//! UVM factory overrides for the Verification Hierarchy: the override calls a method body
//! makes, and which class the factory builds for a request once they are registered.

use crate::world::{Analysis, DeclRef, UnitKind};
use dvi_syntax::ast::{dflags, DeclId, DeclKind};
use dvi_syntax::{Span, TokKind, Token};

/// An override call with literal arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverrideCall {
    /// An instance override (else a type override).
    pub instance: bool,
    pub from: DeclRef,
    pub to: DeclRef,
    /// Instance overrides: the path pattern as written.
    pub path: String,
    /// The path is relative to the component that makes the call (else it is a full path).
    pub relative: bool,
    /// Type overrides: replace an earlier override of the same type (`replace` argument).
    pub replace: bool,
    /// The `replace` argument is not a literal: the override may or may not replace one.
    pub replace_unknown: bool,
    /// A class is written with parameter values (`A#(8)::get_type()`): which specialization it
    /// is cannot be told, so the override is never applied for certain.
    pub params: bool,
    /// Given by name (`set_type_override("A", "B")`).
    pub by_name: bool,
    /// The called method's name, for the tooltip.
    pub span: Span,
}

/// Who receives an override call.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Receiver {
    /// A component's own method (`set_type_override_by_type(...)`, `this.set_...`).
    Component,
    /// The factory (`factory.`, `uvm_factory::get().`, `cs.get_factory().`).
    Factory,
    /// A class's registry (`X::type_id::set_type_override(...)`), with the token of `X`.
    Registry(usize),
}

/// The formal arguments of each override method, by receiver.
fn formals(r: Receiver, name: &str) -> Option<&'static [&'static str]> {
    Some(match (r, name) {
        (Receiver::Component | Receiver::Factory, "set_type_override_by_type") => &["original_type", "override_type", "replace"],
        (Receiver::Component, "set_inst_override_by_type") => &["relative_inst_path", "original_type", "override_type"],
        (Receiver::Component, "set_type_override") => &["original_type_name", "override_type_name", "replace"],
        (Receiver::Component, "set_inst_override") => &["relative_inst_path", "original_type_name", "override_type_name"],
        (Receiver::Factory, "set_inst_override_by_type") => &["original_type", "override_type", "full_inst_path"],
        (Receiver::Factory, "set_type_override_by_name") => &["original_type_name", "override_type_name", "replace"],
        (Receiver::Factory, "set_inst_override_by_name") => &["original_type_name", "override_type_name", "full_inst_path"],
        (Receiver::Registry(_), "set_type_override") => &["override_type", "replace"],
        (Receiver::Registry(_), "set_inst_override") => &["override_type", "inst_path", "parent"],
        _ => return None,
    })
}

impl Analysis {
    /// The override calls in a method body's tokens (resolved in `body`'s unit), with the index
    /// of each. Calls with an argument that is not literal are left out.
    pub(crate) fn override_calls(&self, body: DeclRef, t: &[Token]) -> Vec<(usize, OverrideCall)> {
        let mut out = Vec::new();
        for i in 0..t.len().saturating_sub(1) {
            let x = &t[i];
            if !x.is_ident() || !x.text.starts_with("set_") || !x.text.contains("_override") || !t[i + 1].is_punct("(") {
                continue;
            }
            let Some(recv) = self.receiver(body, t, i) else { continue };
            let Some(names) = formals(recv, &x.text) else { continue };
            let args = super::views::call_args(t, i + 1);
            // Arguments by formal name: given by name (`.replace(0)`) or by position.
            let named = args.first().is_some_and(|a| a.first().is_some_and(|y| y.is_punct(".")));
            let arg = |formal: &str| -> Option<&[Token]> {
                if named {
                    args.iter().find_map(|a| match a {
                        [dot, n, open, inner @ .., close] if dot.is_punct(".") && open.is_punct("(") && close.is_punct(")") && n.text == formal => Some(inner),
                        _ => None,
                    })
                } else {
                    names.iter().position(|f| *f == formal).and_then(|p| args.get(p).copied())
                }
            };
            let params = std::cell::Cell::new(false);
            let class_arg = |formal: &str| -> Option<DeclRef> {
                if formal.ends_with("_name") {
                    let a = arg(formal)?;
                    let [s] = a else { return None };
                    (s.kind == TokKind::String).then(|| self.class_by_factory_name(s.text.trim_matches('"')))?
                } else {
                    let (c, p) = self.type_handle(body, arg(formal)?)?;
                    params.set(params.get() || p);
                    Some(c)
                }
            };
            let instance = x.text.starts_with("set_inst_");
            let (from, to) = match recv {
                Receiver::Registry(k) => {
                    let Some(from) = self.resolve_at_span(body.unit, t[k].span).and_then(|r| self.follow_typedef(r)).filter(|r| self.decl(*r).kind == DeclKind::Class) else { continue };
                    (Some(from), class_arg("override_type"))
                }
                _ if names.contains(&"original_type") => (class_arg("original_type"), class_arg("override_type")),
                _ => (class_arg("original_type_name"), class_arg("override_type_name")),
            };
            let (Some(from), Some(to)) = (from, to) else { continue };
            let by_name = !names.contains(&"original_type") && !matches!(recv, Receiver::Registry(_));
            let (replace, replace_unknown) = match arg("replace") {
                None => (true, false),
                Some([n]) if n.kind == TokKind::Number => match super::loops::literal(&n.text) {
                    Some(v) => (v != 0, false),
                    None => (true, true),
                },
                Some(_) => (true, true),
            };
            let (mut path, mut relative) = (String::new(), false);
            if instance {
                let (formal, rel) = match recv {
                    Receiver::Component => ("relative_inst_path", true),
                    Receiver::Factory => ("full_inst_path", false),
                    Receiver::Registry(_) => {
                        // Relative to `parent` when it is `this`; a full path without one.
                        match arg("parent") {
                            None => ("inst_path", false),
                            Some([p]) if p.is_kw("null") => ("inst_path", false),
                            Some([p]) if p.is_kw("this") => ("inst_path", true),
                            Some(_) => continue,
                        }
                    }
                };
                let Some([s]) = arg(formal) else { continue };
                if s.kind != TokKind::String {
                    continue;
                }
                path = s.text.trim_matches('"').to_string();
                relative = rel;
            }
            out.push((i, OverrideCall { instance, from, to, path, relative, replace, replace_unknown, params: params.get(), by_name, span: x.span }));
        }
        out
    }

    /// Who receives the call whose method name is `t[i]`.
    fn receiver(&self, body: DeclRef, t: &[Token], i: usize) -> Option<Receiver> {
        let prev = i.checked_sub(1).map(|k| &t[k]);
        match prev {
            Some(p) if p.is_punct("::") => {
                // `X::type_id::set_...`
                (i >= 4 && t[i - 2].text == "type_id" && t[i - 3].is_punct("::") && t[i - 4].is_ident()).then_some(Receiver::Registry(i - 4))
            }
            Some(p) if p.is_punct(".") => {
                let r = &t[i.checked_sub(2)?];
                if r.is_kw("this") {
                    return Some(Receiver::Component);
                }
                if r.is_punct(")") {
                    // `uvm_factory::get().` or `...get_factory().`
                    let mut depth = 0i32;
                    let mut k = i - 2;
                    loop {
                        if t[k].is_punct(")") {
                            depth += 1;
                        } else if t[k].is_punct("(") {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        k = k.checked_sub(1)?;
                    }
                    let f = &t[k.checked_sub(1)?];
                    let factory_get = f.text == "get" && k >= 3 && t[k - 2].is_punct("::") && t[k - 3].text == "uvm_factory";
                    return (f.text == "get_factory" || factory_get).then_some(Receiver::Factory);
                }
                // A variable of a factory class (`factory`, `f`).
                let v = self.resolve_at_span(body.unit, r.span)?;
                let ty = self.type_of(v)?;
                self.derives_from(ty, "uvm_factory").then_some(Receiver::Factory)
            }
            _ => Some(Receiver::Component),
        }
    }

    /// The class of a type handle argument, the whole argument being `X::get_type()`,
    /// `X::type_id::get()`, `pkg::X::get_type()` or `X#(8)::get_type()`; and whether it is written
    /// with parameter values.
    fn type_handle(&self, body: DeclRef, a: &[Token]) -> Option<(DeclRef, bool)> {
        let n = a.len();
        let end = match a {
            [.., c, g, open, close] if c.is_punct("::") && g.text == "get_type" && open.is_punct("(") && close.is_punct(")") => n - 4,
            [.., c1, ti, c2, g, open, close] if c1.is_punct("::") && ti.text == "type_id" && c2.is_punct("::") && g.text == "get" && open.is_punct("(") && close.is_punct(")") => n - 6,
            _ => return None,
        };
        // The class name, after `pkg::` scopes and before a `#(...)`.
        let mut k = 0;
        let mut name = None;
        while k < end {
            if !a[k].is_ident() {
                return None;
            }
            name = Some(k);
            k += 1;
            if k < end && a[k].is_punct("::") {
                k += 1;
                continue;
            }
            break;
        }
        let mut params = false;
        if k < end {
            if !(a[k].is_punct("#") && a.get(k + 1).is_some_and(|x| x.is_punct("("))) {
                return None;
            }
            k = super::views::skip_group(a, k + 1);
            params = true;
        }
        if k != end {
            return None;
        }
        let x = &a[name?];
        let c = self.resolve_at_span(body.unit, x.span).and_then(|r| self.follow_typedef(r)).filter(|r| self.decl(*r).kind == DeclKind::Class)?;
        Some((c, params))
    }

    /// The class registered with the factory under `name`: the one class that registers that
    /// name (`` `uvm_component_utils(X) ``, `` `uvm_object_utils(X) `` and their `_begin` forms
    /// register "X"; the `_param_utils` macros register no name). `None` when there is none, or
    /// several.
    pub(crate) fn class_by_factory_name(&self, name: &str) -> Option<DeclRef> {
        let mut found: Option<DeclRef> = None;
        for u in self.units.iter().flatten() {
            if u.kind == UnitKind::Builtin {
                continue;
            }
            for (i, d) in u.parsed.decls.iter().enumerate() {
                if d.kind != DeclKind::Class || d.name != name || d.has(dflags::FROM_MACRO) {
                    continue;
                }
                let r = DeclRef { unit: u.id, decl: i as DeclId };
                if !self.registers_name(r, name) {
                    continue;
                }
                match found {
                    Some(f) if self.decl_key(f) != self.decl_key(r) => return None,
                    _ => found = Some(r),
                }
            }
        }
        found
    }

    /// Whether a class registers `name` with the factory: one of the utils macros with `name`
    /// in its body.
    fn registers_name(&self, class: DeclRef, name: &str) -> bool {
        let d = self.decl(class);
        let Some(toks) = self.file_tokens(d.span.file) else { return false };
        let lo = toks.partition_point(|t| t.span.start < d.span.start);
        let hi = toks.partition_point(|t| t.span.start < d.span.end);
        let t = &toks[lo..hi];
        t.windows(4).any(|w| {
            w[0].kind == TokKind::Directive
                && matches!(w[0].text.as_str(), "uvm_component_utils" | "uvm_object_utils" | "uvm_component_utils_begin" | "uvm_object_utils_begin")
                && w[1].is_punct("(")
                && w[2].text == name
                && w[3].is_punct(")")
        })
    }
}

/// UVM's glob match of an instance path (`uvm_is_match`): `*` any characters (dots too), `?`
/// one character, `+` one or more. A pattern written as a regular expression (`/.../`) never
/// matches here.
pub(crate) fn glob_match(pattern: &str, s: &str) -> bool {
    if pattern.len() > 1 && pattern.starts_with('/') && pattern.ends_with('/') {
        return false;
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = s.chars().collect();
    // ok[j]: the pattern so far matches s[..j].
    let mut ok = vec![false; s.len() + 1];
    ok[0] = true;
    for &c in &p {
        let mut next = vec![false; s.len() + 1];
        match c {
            '*' => {
                let mut any = false;
                for j in 0..=s.len() {
                    any |= ok[j];
                    next[j] = any;
                }
            }
            '+' => {
                let mut any = false;
                for j in 1..=s.len() {
                    any |= ok[j - 1];
                    next[j] = any;
                }
            }
            _ => {
                for j in 1..=s.len() {
                    next[j] = ok[j - 1] && (c == '?' || c == s[j - 1]);
                }
            }
        }
        ok = next;
    }
    ok[s.len()]
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn uvm_globs() {
        assert!(glob_match("uvm_test_top.env.agt.drv", "uvm_test_top.env.agt.drv"));
        assert!(glob_match("*.drv", "uvm_test_top.env.agt.drv"));
        assert!(glob_match("uvm_test_top.env.*", "uvm_test_top.env.agt.drv"));
        assert!(glob_match("uvm_test_top.env.agt?.drv", "uvm_test_top.env.agt1.drv"));
        assert!(!glob_match("uvm_test_top.env.agt?.drv", "uvm_test_top.env.agt.drv"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("+", ""));
        assert!(!glob_match("/env.*/", "env.a"));
        assert!(!glob_match("env.*", "uvm_test_top.env.a"));
    }
}
