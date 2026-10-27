//! `randomize-result-discarded`: a `randomize()` call used as a statement, so a failed
//! randomization (contradicting constraints) goes unnoticed and the object keeps stale values.

use crate::ide::stmt::Simple;
use crate::lint::{Cx, Fix, Level, Rule, RuleMeta, Ruleset};
use dvi_syntax::ast::{DeclKind, RefId, Sep};
use dvi_syntax::{Span, TokKind, Token};

pub struct RandomizeResultDiscarded;
pub struct RandomizeInAssert;

static ASSERT_META: RuleMeta = RuleMeta {
    id: "randomize-in-assert",
    guide: &["LNG-10"],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "randomize() is called inside an assertion.",
    why: "When assertions are turned off ($assertoff, a simulator option), the whole assertion is skipped, the randomize() call with it: the test silently runs on unrandomized values.",
    params: &[],
    text_only: false,
};

impl Rule for RandomizeInAssert {
    fn meta(&self) -> &'static RuleMeta {
        &ASSERT_META
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        let has = |a: usize, b: usize| t[a..b].iter().any(|x| x.is_kw("assert") || x.is_kw("assume")) && t[a..b].iter().any(|x| x.kind == TokKind::Ident && x.text == "randomize");
        if facts.repeated {
            if has(0, t.len()) {
                cx.unknown("repeated inclusion");
            }
            return;
        }
        for b in &facts.bodies {
            if !cx.step((b.end - b.start) as u64 + 1) {
                return;
            }
            let Some(stmts) = &b.stmts else {
                if has(b.start, b.end) {
                    cx.unknown("statement structure");
                }
                continue;
            };
            for s in stmts {
                let Simple::Assert(c0, c1) = *s else { continue };
                // A macro in the condition: what it expands to is not looked at.
                if t[c0..c1].iter().any(|x| x.kind == TokKind::Directive) {
                    if t[c0..c1].iter().any(|x| x.kind == TokKind::Ident && x.text == "randomize") {
                        cx.unknown("macro");
                    }
                    continue;
                }
                // `assert (`, `assert #0 (`, `assert final (`: the keyword is a few tokens back.
                let kw = t[c0.saturating_sub(4)..c0].iter().rev().find(|x| x.kind == TokKind::Keyword && matches!(x.text.as_str(), "assert" | "assume" | "cover" | "restrict"));
                if !kw.is_some_and(|k| k.text == "assert" || k.text == "assume") {
                    continue;
                }
                for k in c0..c1 {
                    let x = &t[k];
                    // A call: `randomize(`, `randomize with`, or `randomize` alone as an operand.
                    let call = t.get(k + 1).is_some_and(|y| y.is_punct("(") || y.is_kw("with") || matches!(y.text.as_str(), ")" | "&&" | "||" | "," | ";" | "?"));
                    if x.kind != TokKind::Ident || x.text != "randomize" || !call {
                        continue;
                    }
                    let prefix = if k == c0 || !(t[k - 1].is_punct(".") || t[k - 1].is_punct("::")) {
                        Prefix::None
                    } else if k >= 2 && t[k - 2].text == "std" && t[k - 1].is_punct("::") {
                        Prefix::Std
                    } else if k >= 2 && (t[k - 2].text == "this" || t[k - 2].text == "super") && t[k - 1].is_punct(".") {
                        Prefix::ThisOrSuper
                    } else {
                        Prefix::Handle
                    };
                    match callee_is_builtin(cx, x.span, prefix) {
                        Some(true) => {
                            let kw = kw.unwrap();
                            cx.report(None, Span::new(cx.file, kw.span.start, t[c1].span.end), format!("randomize() is called inside an {}: when assertions are turned off the randomization is skipped too. Call it before and check its result with if", kw.text), Vec::new());
                            break;
                        }
                        Some(false) => {}
                        None => cx.unknown_at("unresolved receiver", x.span),
                    }
                }
            }
        }
    }
}

static META: RuleMeta = RuleMeta {
    id: "randomize-result-discarded",
    guide: &["LNG-10"],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "The result of randomize() is discarded.",
    why: "randomize() returns 0 when the constraints cannot be met and then leaves the variables unchanged; a discarded result lets the test run on with stale values.",
    params: &[],
    text_only: false,
};

const MESSAGE: &str = "The result of randomize() is discarded: a failed randomization goes unnoticed. Check it (if (!x.randomize()) `uvm_error(...)) or discard it explicitly with void'(...)";

impl Rule for RandomizeResultDiscarded {
    fn meta(&self) -> &'static RuleMeta {
        &META
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        if facts.repeated {
            if t.iter().any(|x| x.text == "randomize") {
                cx.unknown("repeated inclusion");
            }
            return;
        }
        for b in &facts.bodies {
            if !cx.step((b.end - b.start) as u64 + 1) {
                return;
            }
            let Some(stmts) = &b.stmts else {
                // A body the statement walker cannot follow: its calls cannot be placed.
                if let Some(x) = t[b.start..b.end].iter().find(|x| x.text == "randomize" && x.kind == TokKind::Ident) {
                    cx.unknown_at("statement structure", x.span);
                }
                continue;
            };
            for s in stmts {
                let (s, e) = match *s {
                    Simple::Expr(s, e) => (s, e),
                    // A macro call: what it expands to is not looked at.
                    Simple::Macro(s, e) => {
                        if t[s..e].iter().any(|x| x.kind == TokKind::Ident && x.text == "randomize") {
                            cx.unknown("macro");
                        }
                        continue;
                    }
                    Simple::Assert(..) | Simple::Delay(..) => continue,
                };
                let Some(call) = shape(&t[s..e]) else { continue };
                let rnd = &t[s + call.name];
                if call.macro_in_callee {
                    cx.unknown("macro");
                    continue;
                }
                match callee_is_builtin(cx, rnd.span, call.prefix) {
                    Some(true) => {
                        let span = Span::new(cx.file, t[s].span.start, t[e].span.end);
                        let fixes = check_fix(cx, span).into_iter().collect();
                        cx.report_fix(None, span, MESSAGE, Vec::new(), fixes)
                    }
                    Some(false) => {}
                    None => cx.unknown_at("unresolved receiver", rnd.span),
                }
            }
        }
    }
}

/// `X;` as `if (!X) `uvm_fatal("RAND", "...")` (or `$fatal` without UVM's macros), for a
/// statement on one line.
fn check_fix(cx: &Cx, stmt: Span) -> Option<Fix> {
    let text = cx.a.file_text(cx.file)?;
    let src = text.get(stmt.start as usize..stmt.end as usize)?;
    if src.contains('\n') || !src.ends_with(';') {
        return None;
    }
    let call = src[..src.len() - 1].trim_end();
    let what = format!("{} failed", call.split(" with").next().unwrap_or(call).trim()).replace('\\', "\\\\").replace('"', "\\\"");
    let uvm = cx.a.unit(cx.unit).macros_after.contains_key("uvm_fatal");
    let new = if uvm { format!("if (!{call}) `uvm_fatal(\"RAND\", \"{what}\")") } else { format!("if (!{call}) $fatal(1, \"{what}\");") };
    Some(Fix { title: "Check the result of randomize()".into(), edits: vec![(stmt, keep_else(cx, stmt, new))], safe: false })
}

/// `new` (an `if` without `else`) in place of the statement at `stmt`: in `begin ... end` unless
/// the statement is one of a sequence (after `;`, `begin`, `end`, ...), where no `else` can
/// follow it: as the body of an `if`, `else` or loop, a later `else` (perhaps behind an
/// `` `ifdef ``) would bind to the new `if`.
pub(super) fn keep_else(cx: &Cx, stmt: Span, new: String) -> String {
    let t = &cx.facts.tokens;
    let k = t.partition_point(|x| x.span.start < stmt.start);
    let in_sequence = k.checked_sub(1).and_then(|p| t.get(p)).is_some_and(|x| {
        x.is_punct(";") || x.kind == TokKind::Keyword && matches!(x.text.as_str(), "begin" | "end" | "fork" | "join" | "join_any" | "join_none") || x.is_punct(":") && k >= 2 && t[k - 2].kind == TokKind::Ident && k >= 3 && (t[k - 3].is_kw("begin") || t[k - 3].is_kw("fork") || t[k - 3].is_punct(";"))
    });
    if in_sequence {
        new
    } else {
        format!("begin {new} end")
    }
}

/// Whether `t` (inside a `void'(...)`) is exactly a call of the built-in `randomize`.
pub(super) fn builtin_randomize_call(cx: &Cx, t: &[Token]) -> Option<bool> {
    let call = shape(t)?;
    if call.macro_in_callee {
        return None;
    }
    callee_is_builtin(cx, t[call.name].span, call.prefix)
}

/// A statement that is only a `randomize` call: where the name is and what is before it.
struct Call {
    /// Index of `randomize`.
    name: usize,
    prefix: Prefix,
    macro_in_callee: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prefix {
    /// `randomize(...)`.
    None,
    /// `std::randomize(...)`.
    Std,
    /// `this.randomize()`, `super.randomize()`.
    ThisOrSuper,
    /// `x.randomize()`, `a[i].b.randomize()`, `f().randomize()`.
    Handle,
}

/// Recognizes `CALLEE [( args )] [with [( ids )] { ... }]` filling all of `t` (a statement
/// without its `;`), where CALLEE ends in `randomize`.
fn shape(t: &[Token]) -> Option<Call> {
    // The outer call: a `randomize` outside brackets (one in an index or an argument is not
    // the statement's), the last that fits.
    let mut depth = 0i32;
    let mut names = Vec::new();
    for (k, x) in t.iter().enumerate() {
        if x.kind == TokKind::Punct {
            match x.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                _ => {}
            }
        } else if depth == 0 && x.kind == TokKind::Ident && x.text == "randomize" {
            names.push(k);
        }
    }
    names.into_iter().rev().find_map(|name| shape_at(t, name))
}

fn shape_at(t: &[Token], name: usize) -> Option<Call> {
    // The callee: names, `.`/`::`, selects and call parentheses only.
    let mut k = 0;
    let mut macro_in_callee = false;
    while k < name {
        let x = &t[k];
        match x.kind {
            TokKind::Ident | TokKind::Keyword if x.kind == TokKind::Ident || x.text == "this" || x.text == "super" || x.text == "std" => k += 1,
            TokKind::Directive => {
                macro_in_callee = true;
                k += 1;
            }
            TokKind::Punct if x.text == "." || x.text == "::" => k += 1,
            TokKind::Punct if x.text == "[" || x.text == "(" => k = group_end(t, k)?,
            _ => return None,
        }
    }
    if name > 0 && !(t[name - 1].is_punct(".") || t[name - 1].is_punct("::")) {
        return None;
    }
    let mut k = name + 1;
    if t.get(k).is_some_and(|x| x.is_punct("(")) {
        k = group_end(t, k)?;
    }
    if t.get(k).is_some_and(|x| x.is_kw("with")) {
        k += 1;
        if t.get(k).is_some_and(|x| x.is_punct("(")) {
            k = group_end(t, k)?;
        }
        if !t.get(k)?.is_punct("{") {
            return None;
        }
        k = group_end(t, k)?;
    }
    if k != t.len() {
        return None;
    }
    let prefix = if name == 0 {
        Prefix::None
    } else if name == 2 && t[0].text == "std" && t[1].is_punct("::") {
        Prefix::Std
    } else if name == 2 && (t[0].is_kw("this") || t[0].is_kw("super") || t[0].text == "this" || t[0].text == "super") && t[1].is_punct(".") {
        Prefix::ThisOrSuper
    } else {
        Prefix::Handle
    };
    Some(Call { name, prefix, macro_in_callee })
}

/// Index after the bracket group opened at `t[open]`.
fn group_end(t: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (k, x) in t.iter().enumerate().skip(open) {
        if x.kind != TokKind::Punct {
            continue;
        }
        match x.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth -= 1;
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether the `randomize` written at `at` is the built-in one: `Some(false)` when it is a
/// user's function, `None` when that cannot be told.
fn callee_is_builtin(cx: &Cx, at: Span, prefix: Prefix) -> Option<bool> {
    let a = cx.a;
    let u = a.unit(cx.unit);
    // A class cannot declare its own `randomize` (the built-in one cannot be overridden), but a
    // project's code may still try: such a method is not the built-in.
    let own = |class: crate::world::DeclRef| a.class_member(class, "randomize", 0).is_some_and(|d| !a.vfs.is_builtin(a.decl(d).name_span.file));
    match prefix {
        Prefix::Std => return Some(true),
        Prefix::ThisOrSuper => {
            let scope = a.scope_at(cx.unit, cx.file, at.start);
            let Some(c) = a.enclosing_class(cx.unit, scope) else { return None };
            return Some(!own(c));
        }
        Prefix::None | Prefix::Handle => {}
    }
    if prefix == Prefix::None {
        // A user function named `randomize` in scope would be called instead.
        let scope = a.scope_at(cx.unit, cx.file, at.start);
        return Some(match a.lookup(cx.unit, scope, "randomize") {
            // The built-in declaration (the engine's `std` package), or no function at all.
            Some(d) => a.decl(d).kind != DeclKind::Function || a.vfs.is_builtin(a.decl(d).name_span.file),
            None => true,
        });
    }
    let pos = u.positions.get(&cx.file)?;
    let i = pos.refs.partition_point(|(s, _, _)| (s.start, s.end) < (at.start, at.end));
    let &(s, rid, seg) = pos.refs.get(i)?;
    if s != at {
        return None;
    }
    let chain = u.parsed.reference(rid as RefId);
    if seg == 0 {
        return None;
    }
    if chain.segs.get(seg as usize)?.sep != Sep::Dot {
        return None;
    }
    let receiver = a.resolve_segments(cx.unit, rid as RefId).get(seg as usize - 1).copied().flatten()?;
    let ty = a.type_of(receiver)?;
    let ty = if a.decl(ty).kind == DeclKind::Typedef { a.follow_typedef(ty)? } else { ty };
    if a.decl(ty).kind != DeclKind::Class {
        return Some(false);
    }
    Some(!own(ty))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> Vec<Token> {
        dvi_syntax::lexer::lex(s, 0).into_iter().filter(|t| t.kind != TokKind::Eof).collect()
    }

    fn prefix(s: &str) -> Option<&'static str> {
        let t = lex(s);
        shape(&t[..t.len() - 1]).map(|c| match c.prefix {
            Prefix::None => "none",
            Prefix::Std => "std",
            Prefix::ThisOrSuper => "this",
            Prefix::Handle => "handle",
        })
    }

    #[test]
    fn statement_shapes() {
        assert_eq!(prefix("randomize();"), Some("none"));
        assert_eq!(prefix("std::randomize(a, b) with { a < b; };"), Some("std"));
        assert_eq!(prefix("this.randomize();"), Some("this"));
        assert_eq!(prefix("super.randomize() with { x == 1; };"), Some("this"));
        assert_eq!(prefix("item.randomize() with (a) { a > 0; };"), Some("handle"));
        assert_eq!(prefix("cfg.items[i].randomize();"), Some("handle"));
        assert_eq!(prefix("x.randomize;"), Some("handle"));
        assert_eq!(prefix("ok = x.randomize();"), None);
        assert_eq!(prefix("void'(x.randomize());"), None);
        assert_eq!(prefix("bit ok = x.randomize();"), None);
        assert_eq!(prefix("return x.randomize();"), None);
        assert_eq!(prefix("x.randomize() + 1;"), None);
        assert_eq!(prefix("x.y.randomize_all();"), None);
        assert_eq!(prefix("items[std::randomize(i)].randomize();"), Some("handle"));
        assert_eq!(prefix("get(randomize()).randomize();"), Some("handle"));
    }
}
