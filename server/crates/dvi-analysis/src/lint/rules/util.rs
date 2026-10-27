//! Helpers the rules share: call arguments, resolved references, library identity.

use crate::lint::Cx;
use crate::world::DeclRef;
use dvi_syntax::ast::{RefId, RefSeg};
use dvi_syntax::{Span, TokKind, Token};

/// The arguments of the call whose `(` is `t[open]`: token ranges, empty ranges for empty
/// arguments (`f(a, , b)`); `None` when the parentheses do not close.
pub fn call_args(t: &[Token], open: usize) -> Option<Vec<(usize, usize)>> {
    if !t.get(open)?.is_punct("(") {
        return None;
    }
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    for (k, x) in t.iter().enumerate().skip(open) {
        if x.kind != TokKind::Punct {
            continue;
        }
        match x.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth -= 1;
                if depth == 0 {
                    if k > start || !out.is_empty() {
                        out.push((start, k));
                    }
                    return Some(out);
                }
            }
            "," if depth == 1 => {
                out.push((start, k));
                start = k + 1;
            }
            _ => {}
        }
    }
    None
}

/// The argument for formal `name` at position `pos`: named (`.name(x)`, the inside of its
/// parentheses) or positional. `Some(None)`: not given; `None`: the call cannot be read.
pub fn argument(t: &[Token], args: &[(usize, usize)], pos: usize, name: &str) -> Option<Option<(usize, usize)>> {
    let named = args.iter().any(|(s, e)| e > s && t[*s].is_punct("."));
    if named {
        for &(s, e) in args {
            if e > s + 2 && t[s].is_punct(".") && t[s + 1].text == name && t[s + 2].is_punct("(") {
                let inner = call_args(t, s + 2)?;
                return Some(inner.first().copied().or(Some((s + 3, s + 3))));
            }
        }
        // Positional ones before the first named one.
        let first_named = args.iter().position(|(s, e)| e > s && t[*s].is_punct(".")).unwrap_or(args.len());
        return Some(args[..first_named].get(pos).copied().filter(|(s, e)| e > s));
    }
    Some(args.get(pos).copied().filter(|(s, e)| e > s))
}

/// The reference segment written at `at` (exactly), with what it resolves to.
pub fn reference_at<'u>(cx: &'u Cx, at: Span) -> Option<(RefId, &'u RefSeg, usize, Option<DeclRef>)> {
    let u = cx.a.unit(cx.unit);
    let pos = u.positions.get(&cx.file)?;
    let i = pos.refs.partition_point(|(s, _, _)| (s.start, s.end) < (at.start, at.end));
    let &(s, rid, seg) = pos.refs.get(i)?;
    if s != at {
        return None;
    }
    let chain = u.parsed.reference(rid);
    let r = cx.a.resolve_ref(cx.unit, rid, seg as usize);
    Some((rid, chain.segs.get(seg as usize)?, seg as usize, r))
}

/// Whether a declaration is the UVM library's.
pub fn in_uvm(cx: &Cx, d: DeclRef) -> bool {
    cx.a.lint_facts.uvm_files(cx.a).contains(&cx.a.decl(d).name_span.file)
}

/// The class a library member belongs to, by name.
pub fn member_class_name(cx: &Cx, d: DeclRef) -> Option<smol_str::SmolStr> {
    crate::lint::facts::member_class(cx.a, d).map(|c| cx.a.decl(c).name.clone())
}

/// The text of a simple string literal token (`"abc"`), without escapes decoded.
pub fn string_value(x: &Token) -> Option<&str> {
    (x.kind == TokKind::String).then(|| x.text.strip_prefix('"').and_then(|s| s.strip_suffix('"'))).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> Vec<Token> {
        dvi_syntax::lexer::lex(s, 0).into_iter().filter(|t| t.kind != TokKind::Eof).collect()
    }

    #[test]
    fn arguments_by_position_and_name() {
        let t = lex("f(this, \"d\", 2)");
        let a = call_args(&t, 1).unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(argument(&t, &a, 1, "description").unwrap().map(|(s, _)| t[s].text.as_str()), Some("\"d\""));
        let t = lex("f(this, , 2)");
        let a = call_args(&t, 1).unwrap();
        assert_eq!(argument(&t, &a, 1, "description").unwrap(), None);
        let t = lex("f(.obj(this), .description(\"x\"))");
        let a = call_args(&t, 1).unwrap();
        assert_eq!(argument(&t, &a, 1, "description").unwrap().map(|(s, _)| t[s].text.as_str()), Some("\"x\""));
        let t = lex("f(.obj(this), .count(2))");
        let a = call_args(&t, 1).unwrap();
        assert_eq!(argument(&t, &a, 1, "description").unwrap(), None);
        let t = lex("f()");
        assert_eq!(call_args(&t, 1).unwrap().len(), 0);
    }
}
