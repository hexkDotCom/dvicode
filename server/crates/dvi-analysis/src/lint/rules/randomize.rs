//! `randomize-result-discarded`: a `randomize()` call used as a statement, so a failed
//! randomization (contradicting constraints) goes unnoticed and the object keeps stale values.

use crate::ide::stmt::Simple;
use crate::lint::{Cx, Level, Rule, RuleMeta, Ruleset};
use dvi_syntax::ast::{DeclKind, RefId, Sep};
use dvi_syntax::{Span, TokKind, Token};

pub struct RandomizeResultDiscarded;

static META: RuleMeta = RuleMeta {
    id: "randomize-result-discarded",
    guide: &["LNG-10"],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "The result of randomize() is discarded.",
    why: "randomize() returns 0 when the constraints cannot be met and then leaves the variables unchanged; a discarded result lets the test run on with stale values.",
    params: &[],
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
                if t[b.start..b.end].iter().any(|x| x.text == "randomize" && x.kind == TokKind::Ident) {
                    cx.unknown("statement structure");
                }
                continue;
            };
            for s in stmts {
                let Simple::Expr(s, e) = *s else { continue };
                let Some(call) = shape(&t[s..e]) else { continue };
                let rnd = &t[s + call.name];
                if call.macro_in_callee {
                    cx.unknown("macro");
                    continue;
                }
                match self.callee_is_builtin(cx, rnd.span, call.prefix) {
                    Some(true) => cx.report(None, Span::new(cx.file, t[s].span.start, t[e].span.end), MESSAGE, Vec::new()),
                    Some(false) => {}
                    None => cx.unknown("unresolved receiver"),
                }
            }
        }
    }
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
    let name = t.iter().position(|x| x.kind == TokKind::Ident && x.text == "randomize")?;
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

impl RandomizeResultDiscarded {
    /// Whether the `randomize` written at `at` is the built-in one: `Some(false)` when it is a
    /// user's function, `None` when that cannot be told.
    fn callee_is_builtin(&self, cx: &Cx, at: Span, prefix: Prefix) -> Option<bool> {
        match prefix {
            Prefix::Std | Prefix::ThisOrSuper => return Some(true),
            Prefix::None | Prefix::Handle => {}
        }
        let a = cx.a;
        let u = a.unit(cx.unit);
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
        Some(a.decl(ty).kind == DeclKind::Class)
    }
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
    }
}
