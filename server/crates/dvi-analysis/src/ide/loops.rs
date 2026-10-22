//! How many times a loop runs, when its bounds are constants: the Verification Hierarchy shows
//! components created in a loop as one node with that count.

use crate::world::{Analysis, UnitId};
use dvi_syntax::ast::DeclKind;
use dvi_syntax::{TokKind, Token};

/// Iterations counted at most (a bigger count is not believable as a component count).
const MAX_COUNT: i64 = 1 << 20;

impl Analysis {
    /// The iterations of a loop: `kind` is its keyword, `header` the tokens inside its
    /// parentheses, resolved in `unit`. `None` when they are not constants.
    pub(crate) fn loop_count(&self, kind: &str, header: &[Token], unit: UnitId) -> Option<u64> {
        let n = match kind {
            "repeat" => self.const_value(header, unit, 0)?,
            "for" => self.for_count(header, unit)?,
            "foreach" => self.foreach_count(header, unit)?,
            _ => return None,
        };
        (0..=MAX_COUNT).contains(&n).then_some(n as u64)
    }

    /// `for (int i = A; i < B; i++)` and its variants: `<=`, `!=`, `B > i`; `i += 1`,
    /// `i = i + 1`, `++i`; counting down with `>`, `>=`, `--`, `-= 1`.
    fn for_count(&self, h: &[Token], unit: UnitId) -> Option<i64> {
        let parts = split_top(h, ";");
        let [init, cond, step] = parts.as_slice() else { return None };
        // Init: `[type] i = A`, one variable.
        if split_top(init, ",").len() != 1 {
            return None;
        }
        let eq = init.iter().position(|x| x.is_punct("="))?;
        let var = init.get(eq.checked_sub(1)?).filter(|x| x.is_ident())?;
        let a = self.const_value(&init[eq + 1..], unit, 0)?;
        // Step: +1 or -1.
        let up = match step {
            [v, op] | [op, v] if v.text == var.text && op.is_punct("++") => true,
            [v, op] | [op, v] if v.text == var.text && op.is_punct("--") => false,
            [v, op, one] if v.text == var.text && is_one(one) && op.is_punct("+=") => true,
            [v, op, one] if v.text == var.text && is_one(one) && op.is_punct("-=") => false,
            [v, eq2, v2, op, one] if v.text == var.text && v2.text == var.text && eq2.is_punct("=") && is_one(one) && op.is_punct("+") => true,
            [v, eq2, v2, op, one] if v.text == var.text && v2.text == var.text && eq2.is_punct("=") && is_one(one) && op.is_punct("-") => false,
            _ => return None,
        };
        // Condition: `i OP B` or `B OP i`, turned into `i OP B`.
        let (op, b) = match cond {
            [v, op, rest @ ..] if v.text == var.text && v.is_ident() => (op.text.to_string(), self.const_value(rest, unit, 0)?),
            [.., op, v] if v.text == var.text && v.is_ident() => {
                let flipped = match op.text.as_str() {
                    "<" => ">",
                    "<=" => ">=",
                    ">" => "<",
                    ">=" => "<=",
                    "!=" => "!=",
                    _ => return None,
                };
                (flipped.to_string(), self.const_value(&cond[..cond.len() - 2], unit, 0)?)
            }
            _ => return None,
        };
        let n = match (up, op.as_str()) {
            (true, "<") => b - a,
            (true, "<=") => b - a + 1,
            (true, "!=") if a <= b => b - a,
            (false, ">") => a - b,
            (false, ">=") => a - b + 1,
            (false, "!=") if a >= b => a - b,
            _ => return None,
        };
        Some(n.max(0))
    }

    /// `foreach (arr[i])`, `foreach (arr[i, j])`, `foreach (arr[i][j])` over a fixed-size array:
    /// the product of the dimensions iterated, read from the array's declaration.
    fn foreach_count(&self, h: &[Token], unit: UnitId) -> Option<i64> {
        let open = h.iter().position(|x| x.is_punct("["))?;
        let name = h[..open].last().filter(|x| x.is_ident())?;
        // Index variables: `[i, j]` or `[i][j]`.
        let mut indexes = 0;
        let mut k = open;
        while k < h.len() && h[k].is_punct("[") {
            let end = group_end(h, k)?;
            indexes += split_top(&h[k + 1..end - 1], ",").len();
            k = end;
        }
        if k != h.len() || indexes == 0 {
            return None;
        }
        let decl = self.resolve_at_span(unit, name.span)?;
        let d = self.decl(decl);
        if !matches!(d.kind, DeclKind::Variable | DeclKind::Field) {
            return None;
        }
        // The unpacked dimensions after the declared name.
        let toks = self.file_tokens(d.name_span.file)?;
        let mut k = toks.partition_point(|t| t.span.start < d.name_span.end);
        let mut n = 1i64;
        for _ in 0..indexes {
            if !toks.get(k)?.is_punct("[") {
                return None;
            }
            let end = group_end(&toks, k)?;
            let inner = &toks[k + 1..end - 1];
            let size = match split_top(inner, ":").as_slice() {
                [one] => self.const_value(one, decl.unit, 0)?,
                [lo, hi] => (self.const_value(lo, decl.unit, 0)? - self.const_value(hi, decl.unit, 0)?).abs() + 1,
                _ => return None,
            };
            n = n.checked_mul(size)?;
            k = end;
        }
        Some(n)
    }

    /// The value of a constant expression: integer literals, parameters whose value is constant
    /// (not ones a class or module gets from `#(...)`), object-like macros, `+ - * / %` and
    /// parentheses.
    pub(crate) fn const_value(&self, t: &[Token], unit: UnitId, depth: usize) -> Option<i64> {
        if depth > 4 || t.is_empty() {
            return None;
        }
        let mut p = Expr { a: self, t, k: 0, unit, depth };
        let v = p.sum()?;
        (p.k == t.len()).then_some(v)
    }

    /// The value of a parameter, localparam or macro named by `x`.
    fn const_name(&self, x: &Token, unit: UnitId, depth: usize) -> Option<i64> {
        if x.kind == TokKind::Directive {
            // Every definition of the macro must give the same value.
            let defs = self.global.macros.get(&x.text)?;
            let mut value = None;
            for d in defs {
                if d.params.is_some() {
                    return None;
                }
                let v = self.const_value(&d.body, unit, depth + 1)?;
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
        let toks = self.file_tokens(d.name_span.file)?;
        let lo = toks.partition_point(|t| t.span.start < d.name_span.end);
        if !toks.get(lo)?.is_punct("=") {
            return None;
        }
        let mut hi = lo + 1;
        let mut nest = 0i32;
        while let Some(x) = toks.get(hi) {
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
        self.const_value(&toks[lo + 1..hi], r.unit, depth + 1)
    }
}

/// A recursive-descent evaluator for [`Analysis::const_value`].
struct Expr<'a> {
    a: &'a Analysis,
    t: &'a [Token],
    k: usize,
    unit: UnitId,
    depth: usize,
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
        if x.is_punct("-") {
            self.k += 1;
            return self.unary()?.checked_neg();
        }
        if x.is_punct("+") {
            self.k += 1;
            return self.unary();
        }
        if x.is_punct("(") {
            self.k += 1;
            let v = self.sum()?;
            if !self.t.get(self.k)?.is_punct(")") {
                return None;
            }
            self.k += 1;
            return Some(v);
        }
        self.k += 1;
        // A size and a based number may be two tokens: `8 'h1F`.
        if x.kind == TokKind::Number && !x.text.contains('\'') {
            if let Some(y) = self.t.get(self.k).filter(|y| y.kind == TokKind::Number && y.text.starts_with('\'')) {
                self.k += 1;
                return number(&y.text);
            }
        }
        match x.kind {
            TokKind::Number => number(&x.text),
            TokKind::Ident | TokKind::Directive => self.a.const_name(x, self.unit, self.depth),
            _ => None,
        }
    }
}

/// An integer literal: `4`, `1_000`, `'d4`, `8'h1F`, `4'sb0101`; `None` with x/z digits or a
/// fraction.
fn number(s: &str) -> Option<i64> {
    let s: String = s.chars().filter(|c| *c != '_').collect();
    let Some(q) = s.find('\'') else { return s.parse().ok() };
    let rest = s[q + 1..].trim_start_matches(['s', 'S']);
    let mut chars = rest.chars();
    let radix = match chars.next()?.to_ascii_lowercase() {
        'd' => 10,
        'h' => 16,
        'o' => 8,
        'b' => 2,
        _ => return None,
    };
    i64::from_str_radix(chars.as_str().trim(), radix).ok()
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
    }
}
