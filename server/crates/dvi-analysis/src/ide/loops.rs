//! How many times a loop runs, when its bounds are constants: the Verification Hierarchy shows
//! components created in a loop as one node with that count.

use crate::world::{Analysis, UnitId};
use dvi_syntax::ast::DeclKind;
use dvi_syntax::{FileId, TokKind, Token};
use std::sync::Arc;

/// Iterations counted at most (a bigger count is not believable as a component count).
const MAX_COUNT: i64 = 1 << 20;

/// Parentheses and unary operators nested at most in a constant expression.
const MAX_NEST: usize = 64;

/// The tokens of a file (cached by the caller).
pub(crate) type Toks<'a> = &'a dyn Fn(FileId) -> Option<Arc<Vec<Token>>>;

impl Analysis {
    /// The iterations of a loop: `kind` is its keyword, `header` the tokens inside its
    /// parentheses, resolved in `unit`. `None` when they are not constants.
    pub(crate) fn loop_count(&self, kind: &str, header: &[Token], unit: UnitId, toks: Toks) -> Option<u64> {
        let n = match kind {
            // A negative count repeats nothing.
            "repeat" => self.const_value(header, unit, 0, toks)?.max(0),
            "for" => self.for_count(header, unit, toks)?,
            "foreach" => self.foreach_count(header, unit, toks)?,
            _ => return None,
        };
        (0..=MAX_COUNT).contains(&n).then_some(n as u64)
    }

    /// `for (int i = A; i < B; i++)` and its variants: `<=`, `!=`, `B > i`; `i += 1`,
    /// `i = i + 1`, `++i`; counting down with `>`, `>=`, `--`, `-= 1`.
    fn for_count(&self, h: &[Token], unit: UnitId, toks: Toks) -> Option<i64> {
        let parts = split_top(h, ";");
        let [init, cond, step] = parts.as_slice() else { return None };
        let var = for_variable(h)?;
        let eq = init.iter().position(|x| x.is_punct("="))?;
        let a = self.const_value(&init[eq + 1..], unit, 0, toks)?;
        // Step: +1 or -1.
        let up = match step {
            [v, op] | [op, v] if v.text == var && op.is_punct("++") => true,
            [v, op] | [op, v] if v.text == var && op.is_punct("--") => false,
            [v, op, one] if v.text == var && is_one(one) && op.is_punct("+=") => true,
            [v, op, one] if v.text == var && is_one(one) && op.is_punct("-=") => false,
            [v, eq2, v2, op, one] if v.text == var && v2.text == var && eq2.is_punct("=") && is_one(one) && op.is_punct("+") => true,
            [v, eq2, v2, op, one] if v.text == var && v2.text == var && eq2.is_punct("=") && is_one(one) && op.is_punct("-") => false,
            _ => return None,
        };
        // Condition: `i OP B` or `B OP i`, turned into `i OP B`.
        let (op, b) = match cond {
            [v, op, rest @ ..] if v.text == var && v.is_ident() => (op.text.to_string(), self.const_value(rest, unit, 0, toks)?),
            [.., op, v] if v.text == var && v.is_ident() => {
                let flipped = match op.text.as_str() {
                    "<" => ">",
                    "<=" => ">=",
                    ">" => "<",
                    ">=" => "<=",
                    "!=" => "!=",
                    _ => return None,
                };
                (flipped.to_string(), self.const_value(&cond[..cond.len() - 2], unit, 0, toks)?)
            }
            _ => return None,
        };
        let n = match (up, op.as_str()) {
            (true, "<") => b.checked_sub(a)?,
            (true, "<=") => b.checked_sub(a)?.checked_add(1)?,
            (true, "!=") if a <= b => b.checked_sub(a)?,
            (false, ">") => a.checked_sub(b)?,
            (false, ">=") => a.checked_sub(b)?.checked_add(1)?,
            (false, "!=") if a >= b => a.checked_sub(b)?,
            _ => return None,
        };
        Some(n.max(0))
    }

    /// `foreach (arr[i])`, `foreach (arr[i, j])`, `foreach (arr[i][j])`, `foreach (arr[, j])`
    /// over a fixed-size array: the product of the dimensions iterated, read from the array's
    /// declaration.
    fn foreach_count(&self, h: &[Token], unit: UnitId, toks: Toks) -> Option<i64> {
        let open = h.iter().position(|x| x.is_punct("["))?;
        let name = h[..open].last().filter(|x| x.is_ident())?;
        // Which dimensions have an index variable: `[i, j]`, `[i][j]`, `[, j]`.
        let mut iterated: Vec<bool> = Vec::new();
        let mut k = open;
        while k < h.len() && h[k].is_punct("[") {
            let end = group_end(h, k)?;
            iterated.extend(split_top(&h[k + 1..end - 1], ",").iter().map(|p| !p.is_empty()));
            k = end;
        }
        if k != h.len() || !iterated.contains(&true) {
            return None;
        }
        let decl = self.resolve_at_span(unit, name.span)?;
        let d = self.decl(decl);
        if !matches!(d.kind, DeclKind::Variable | DeclKind::Field) {
            return None;
        }
        // The unpacked dimensions after the declared name.
        let file = toks(d.name_span.file)?;
        let mut k = file.partition_point(|t| t.span.start < d.name_span.end);
        let mut n = 1i64;
        for it in iterated {
            if !file.get(k)?.is_punct("[") {
                return None;
            }
            let end = group_end(&file, k)?;
            if it {
                let inner = &file[k + 1..end - 1];
                let size = match split_top(inner, ":").as_slice() {
                    [one] => self.const_value(one, decl.unit, 0, toks)?,
                    [lo, hi] => self.const_value(lo, decl.unit, 0, toks)?.checked_sub(self.const_value(hi, decl.unit, 0, toks)?)?.checked_abs()?.checked_add(1)?,
                    _ => return None,
                };
                n = n.checked_mul(size)?;
            }
            k = end;
        }
        Some(n)
    }

    /// The value of a constant expression: integer literals, parameters whose value is constant
    /// (not ones a class or module gets from `#(...)`), object-like macros, `+ - * / %` and
    /// parentheses.
    pub(crate) fn const_value(&self, t: &[Token], unit: UnitId, depth: usize, toks: Toks) -> Option<i64> {
        if depth > 4 || t.is_empty() {
            return None;
        }
        let mut p = Expr { a: self, t, k: 0, unit, depth, nest: 0, toks };
        let v = p.sum()?;
        (p.k == t.len()).then_some(v)
    }

    /// The value of a parameter, localparam or macro named by `x`.
    fn const_name(&self, x: &Token, unit: UnitId, depth: usize, toks: Toks) -> Option<i64> {
        if x.kind == TokKind::Directive {
            // Every definition of the macro must give the same value.
            let defs = self.global.macros.get(&x.text)?;
            let mut value = None;
            for d in defs {
                if d.params.is_some() {
                    return None;
                }
                let v = self.const_value(&d.body, unit, depth + 1, toks)?;
                if value.is_some_and(|w| w != v) {
                    return None;
                }
                value = Some(v);
            }
            return value;
        }
        let r = self.resolve_at_span(unit, x.span)?;
        let d = self.decl(r);
        let parent = d.parent.map(|p| self.unit(r.unit).parsed.decl(p).kind);
        match d.kind {
            DeclKind::LocalParam => {}
            // A parameter of a package or compilation unit cannot be given another value.
            DeclKind::Parameter if matches!(parent, Some(DeclKind::Package | DeclKind::Unit) | None) => {}
            _ => return None,
        }
        let file = toks(d.name_span.file)?;
        let lo = file.partition_point(|t| t.span.start < d.name_span.end);
        if !file.get(lo)?.is_punct("=") {
            return None;
        }
        let mut hi = lo + 1;
        let mut nest = 0i32;
        while let Some(x) = file.get(hi) {
            if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
                nest += 1;
            } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
                nest -= 1;
            } else if nest == 0 && (x.is_punct(",") || x.is_punct(";")) {
                break;
            }
            if nest < 0 {
                break;
            }
            hi += 1;
        }
        self.const_value(&file[lo + 1..hi], r.unit, depth + 1, toks)
    }
}

/// The loop variable of a `for` header (`int i = 0; ...`): the name before the first `=` of its
/// initialization, when there is one variable.
pub(crate) fn for_variable(h: &[Token]) -> Option<String> {
    let parts = split_top(h, ";");
    let init = parts.first()?;
    if split_top(init, ",").len() != 1 {
        return None;
    }
    let eq = init.iter().position(|x| x.is_punct("="))?;
    init.get(eq.checked_sub(1)?).filter(|x| x.is_ident()).map(|x| x.text.to_string())
}

/// A recursive-descent evaluator for [`Analysis::const_value`].
struct Expr<'a> {
    a: &'a Analysis,
    t: &'a [Token],
    k: usize,
    unit: UnitId,
    depth: usize,
    /// Parentheses and unary operators entered.
    nest: usize,
    toks: Toks<'a>,
}

impl Expr<'_> {
    fn sum(&mut self) -> Option<i64> {
        let mut v = self.product()?;
        while let Some(op) = self.t.get(self.k).filter(|x| x.is_punct("+") || x.is_punct("-")) {
            let plus = op.is_punct("+");
            self.k += 1;
            let r = self.product()?;
            v = if plus { v.checked_add(r)? } else { v.checked_sub(r)? };
        }
        Some(v)
    }

    fn product(&mut self) -> Option<i64> {
        let mut v = self.unary()?;
        while let Some(op) = self.t.get(self.k).filter(|x| x.is_punct("*") || x.is_punct("/") || x.is_punct("%")) {
            let op = op.text.clone();
            self.k += 1;
            let r = self.unary()?;
            v = match op.as_str() {
                "*" => v.checked_mul(r)?,
                "/" => v.checked_div(r)?,
                _ => v.checked_rem(r)?,
            };
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<i64> {
        let x = self.t.get(self.k)?;
        if x.is_punct("-") || x.is_punct("+") || x.is_punct("(") {
            self.nest += 1;
            if self.nest > MAX_NEST {
                return None;
            }
            self.k += 1;
            let v = if x.is_punct("-") {
                self.unary()?.checked_neg()
            } else if x.is_punct("+") {
                self.unary()
            } else {
                let v = self.sum()?;
                if !self.t.get(self.k)?.is_punct(")") {
                    return None;
                }
                self.k += 1;
                Some(v)
            };
            self.nest -= 1;
            return v;
        }
        self.k += 1;
        // A size and a based number may be two tokens: `8 'h1F`.
        if x.kind == TokKind::Number && !x.text.contains('\'') {
            if let Some(y) = self.t.get(self.k).filter(|y| y.kind == TokKind::Number && y.text.starts_with('\'')) {
                self.k += 1;
                return number(&format!("{}{}", x.text, y.text));
            }
        }
        match x.kind {
            TokKind::Number => number(&x.text),
            TokKind::Ident | TokKind::Directive => self.a.const_name(x, self.unit, self.depth, self.toks),
            _ => None,
        }
    }
}

/// An integer literal: `4`, `1_000`, `'d4`, `8'h1F`, `4'sb0101`, `'1`; sized values are cut to
/// their size, signed ones read as two's complement. `None` with x/z digits or a fraction.
fn number(s: &str) -> Option<i64> {
    let s: String = s.chars().filter(|c| *c != '_').collect();
    let Some(q) = s.find('\'') else { return s.parse().ok() };
    let size: Option<u32> = if q == 0 { None } else { Some(s[..q].trim().parse().ok()?) };
    let mut rest = &s[q + 1..];
    let signed = rest.starts_with(['s', 'S']);
    if signed {
        rest = &rest[1..];
    }
    // Unbased and unsized: `'0`, `'1`.
    if q == 0 && (rest == "0" || rest == "1") {
        return rest.parse().ok();
    }
    let mut chars = rest.chars();
    let radix = match chars.next()?.to_ascii_lowercase() {
        'd' => 10,
        'h' => 16,
        'o' => 8,
        'b' => 2,
        _ => return None,
    };
    let mut v = i64::from_str_radix(chars.as_str().trim(), radix).ok()?;
    if let Some(n) = size {
        if n == 0 || n > 62 {
            return (n > 62 && v >= 0 && !signed).then_some(v);
        }
        v &= (1i64 << n) - 1;
        if signed && v >> (n - 1) & 1 == 1 {
            v -= 1i64 << n;
        }
    }
    Some(v)
}

/// The value of an integer literal (see `number`).
pub(crate) fn literal(s: &str) -> Option<i64> {
    number(s)
}

fn is_one(x: &Token) -> bool {
    x.kind == TokKind::Number && number(&x.text) == Some(1)
}

/// Index after the bracket group opened at `t[open]`.
fn group_end(t: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (k, x) in t.iter().enumerate().skip(open) {
        if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
            depth += 1;
        } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
            depth -= 1;
            if depth == 0 {
                return Some(k + 1);
            }
        }
    }
    None
}

/// The tokens split at `sep` outside brackets.
fn split_top<'a>(t: &'a [Token], sep: &str) -> Vec<&'a [Token]> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (k, x) in t.iter().enumerate() {
        if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
            depth += 1;
        } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
            depth -= 1;
        } else if depth == 0 && x.is_punct(sep) {
            out.push(&t[start..k]);
            start = k + 1;
        }
    }
    out.push(&t[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::number;

    #[test]
    fn integer_literals() {
        assert_eq!(number("4"), Some(4));
        assert_eq!(number("1_000"), Some(1000));
        assert_eq!(number("'d4"), Some(4));
        assert_eq!(number("8'h1F"), Some(31));
        assert_eq!(number("4'sb0101"), Some(5));
        assert_eq!(number("4'bx01"), None);
        assert_eq!(number("1.5"), None);
        // Cut to their size; signed values are two's complement.
        assert_eq!(number("2'd4"), Some(0));
        assert_eq!(number("4'shF"), Some(-1));
        assert_eq!(number("'1"), Some(1));
    }
}
