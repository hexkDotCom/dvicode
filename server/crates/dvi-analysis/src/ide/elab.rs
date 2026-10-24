//! Parameter elaboration: the parameter values each instance gets, and what they decide (the
//! generate branches that exist, the copies of generate loops, the ranges of instance arrays).
//!
//! Nothing uncertain is given as certain: a value is evaluated, or says why not (a missing
//! binding, code the evaluator does not cover, an error, a budget); a generate branch is
//! elaborated, not elaborated (its condition is proven false) or undetermined.
//!
//! Values are two-state integers of at most 64 bits with their width and signedness, strings,
//! and type parameters as text. Expressions are sized as SystemVerilog sizes them: context-
//! determined operators take the widest of their operands and the context, comparisons and
//! reductions are 1 bit wide, and a parameter's value is converted to its declared type.

use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, DeclKind, GenKind};
use rustc_hash::FxHashSet;
use dvi_syntax::{FileId, Span, TokKind, Token};
use rustc_hash::FxHashMap;
use serde::Serialize;
use smol_str::SmolStr;
use std::sync::Arc;

/// Widest value evaluated.
pub const MAX_WIDTH: u32 = 64;
/// Statements a constant function call may run.
const MAX_STEPS: usize = 10_000;
/// Iterations of a generate loop elaborated.
pub const MAX_ITERATIONS: usize = 1_024;
/// Tokens of one expression evaluated.
const MAX_EXPR_TOKENS: usize = 4_000;
/// Nesting of expressions and calls.
const MAX_NEST: usize = 64;

/// An evaluated value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    /// Two-state bits (the low `width` bits of `bits`).
    Int { bits: u64, width: u32, signed: bool },
    Str(String),
    /// A type parameter's type, as written.
    Type(String),
}

/// Why a value is not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Evaluated,
    /// A name with no value here.
    Unresolved,
    /// Valid code the evaluator does not cover.
    Unsupported,
    /// An error (a cycle, a division by zero, a loop that repeats an index).
    Invalid,
    /// A budget was reached.
    Incomplete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unknown {
    pub state: State,
    pub why: String,
}

pub type R<T> = Result<T, Unknown>;

fn unsupported<T>(why: impl Into<String>) -> R<T> {
    Err(Unknown { state: State::Unsupported, why: why.into() })
}

fn unresolved<T>(why: impl Into<String>) -> R<T> {
    Err(Unknown { state: State::Unresolved, why: why.into() })
}

fn invalid<T>(why: impl Into<String>) -> R<T> {
    Err(Unknown { state: State::Invalid, why: why.into() })
}

fn incomplete<T>(why: impl Into<String>) -> R<T> {
    Err(Unknown { state: State::Incomplete, why: why.into() })
}

fn mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

impl Val {
    pub fn int(v: i128, width: u32, signed: bool) -> Val {
        Val::Int { bits: (v as u64) & mask(width), width, signed }
    }

    /// The value as a number (sign-extended when signed).
    pub fn num(&self) -> Option<i128> {
        match *self {
            Val::Int { bits, width, signed } => Some(if signed && width > 0 && bits >> (width - 1) & 1 == 1 { bits as i128 - (1i128 << width) } else { bits as i128 }),
            _ => None,
        }
    }

    fn width(&self) -> u32 {
        match self {
            Val::Int { width, .. } => *width,
            _ => 0,
        }
    }

    fn signed(&self) -> bool {
        matches!(self, Val::Int { signed: true, .. })
    }

    /// Extended or cut to `width` bits, sign-extended when the value is signed and `signed`.
    fn resize(&self, width: u32, signed: bool) -> R<Val> {
        match self {
            Val::Int { bits, width: w, signed: s } => {
                let v = if *s && signed && *w > 0 && bits >> (w - 1) & 1 == 1 { *bits as i128 - (1i128 << w) } else { *bits as i128 };
                Ok(Val::int(v, width, signed))
            }
            Val::Str(_) => unsupported("a string in an arithmetic expression"),
            Val::Type(_) => unsupported("a type in an expression"),
        }
    }

    pub fn truth(&self) -> R<bool> {
        match self {
            Val::Int { bits, .. } => Ok(*bits != 0),
            Val::Str(s) => Ok(!s.is_empty()),
            Val::Type(_) => unsupported("a type as a condition"),
        }
    }

    /// The value as shown: a number in decimal, a string in quotes, a type as written.
    pub fn text(&self) -> String {
        match self {
            Val::Int { .. } => self.num().map(|n| n.to_string()).unwrap_or_default(),
            Val::Str(s) => format!("\"{s}\""),
            Val::Type(t) => t.clone(),
        }
    }
}

/// An integral type: its width and signedness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntType {
    pub width: u32,
    pub signed: bool,
}

/// A declared type as far as values need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Int(IntType),
    Str,
    /// No type written: the value keeps its own.
    Implicit,
}

// ------------------------------------------------------------------ expressions

#[derive(Clone, Debug)]
enum Ex {
    Num(Val),
    /// `'0`, `'1`: every bit of the context.
    Fill(bool),
    Str(String),
    /// A name (the span of its last identifier resolves it).
    Name(SmolStr, Span),
    Macro(SmolStr),
    Call(SmolStr, Span, Vec<Ex>),
    Sys(SmolStr, Vec<Ex>, Vec<Token>),
    Unary(&'static str, Box<Ex>),
    Bin(&'static str, Box<Ex>, Box<Ex>),
    Cond(Box<Ex>, Box<Ex>, Box<Ex>),
    Concat(Vec<Ex>),
    Repl(Box<Ex>, Vec<Ex>),
    /// `x[i]`, `x[a:b]`, `x[a+:w]`, `x[a-:w]`.
    Select(Box<Ex>, &'static str, Box<Ex>, Option<Box<Ex>>),
    /// `T'(x)`: a size, `signed`/`unsigned` or a type.
    Cast(Box<CastTo>, Box<Ex>),
}

#[derive(Clone, Debug)]
enum CastTo {
    Size(Ex),
    Signed(bool),
    Type(IntType),
}

const BINARY: &[(&str, u8)] = &[
    ("||", 2),
    ("&&", 3),
    ("|", 4),
    ("^", 5),
    ("~^", 5),
    ("^~", 5),
    ("&", 6),
    ("==", 7),
    ("!=", 7),
    ("===", 7),
    ("!==", 7),
    ("<", 8),
    ("<=", 8),
    (">", 8),
    (">=", 8),
    ("<<", 9),
    (">>", 9),
    ("<<<", 9),
    (">>>", 9),
    ("+", 10),
    ("-", 10),
    ("*", 11),
    ("/", 11),
    ("%", 11),
    ("**", 12),
];

/// A parser of one expression's tokens.
struct P<'a> {
    t: &'a [Token],
    k: usize,
    nest: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<&Token> {
        self.t.get(self.k)
    }

    fn at(&self, p: &str) -> bool {
        self.peek().is_some_and(|t| t.is_punct(p))
    }

    fn eat(&mut self, p: &str) -> bool {
        if self.at(p) {
            self.k += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            unsupported(format!("`{p}` expected"))
        }
    }

    fn expr(&mut self) -> R<Ex> {
        self.nest += 1;
        if self.nest > MAX_NEST {
            return incomplete("an expression nested too deeply");
        }
        let c = self.binary(0)?;
        let r = if self.eat("?") {
            let a = self.expr()?;
            self.expect(":")?;
            let b = self.expr()?;
            Ex::Cond(Box::new(c), Box::new(a), Box::new(b))
        } else {
            c
        };
        self.nest -= 1;
        Ok(r)
    }

    fn binary(&mut self, min: u8) -> R<Ex> {
        let mut l = self.unary()?;
        loop {
            let Some(t) = self.peek().filter(|t| t.kind == TokKind::Punct) else { break };
            let Some(&(op, prec)) = BINARY.iter().find(|(o, _)| t.text == *o) else { break };
            if prec < min {
                break;
            }
            self.k += 1;
            // `**` is right-associative, the others left.
            let r = self.binary(if op == "**" { prec } else { prec + 1 })?;
            l = Ex::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn unary(&mut self) -> R<Ex> {
        const UNARY: &[&str] = &["+", "-", "!", "~", "&", "~&", "|", "~|", "^", "~^", "^~"];
        if let Some(op) = self.peek().filter(|t| t.kind == TokKind::Punct).and_then(|t| UNARY.iter().find(|o| t.text == **o)) {
            self.k += 1;
            self.nest += 1;
            if self.nest > MAX_NEST {
                return incomplete("an expression nested too deeply");
            }
            let x = self.unary()?;
            self.nest -= 1;
            return Ok(Ex::Unary(op, Box::new(x)));
        }
        let mut x = self.primary()?;
        // Selects and casts after the primary.
        loop {
            if self.eat("[") {
                let a = self.expr()?;
                let (kind, b) = if self.eat(":") {
                    (":", Some(self.expr()?))
                } else if self.eat("+:") {
                    ("+:", Some(self.expr()?))
                } else if self.eat("-:") {
                    ("-:", Some(self.expr()?))
                } else {
                    ("", None)
                };
                self.expect("]")?;
                x = Ex::Select(Box::new(x), kind, Box::new(a), b.map(Box::new));
            } else if self.at("'") && self.t.get(self.k + 1).is_some_and(|t| t.is_punct("(")) {
                self.k += 2;
                let v = self.expr()?;
                self.expect(")")?;
                x = Ex::Cast(Box::new(CastTo::Size(x)), Box::new(v));
            } else {
                return Ok(x);
            }
        }
    }

    fn args(&mut self) -> R<Vec<Ex>> {
        let mut v = Vec::new();
        if self.eat(")") {
            return Ok(v);
        }
        loop {
            v.push(self.expr()?);
            if self.eat(")") {
                return Ok(v);
            }
            self.expect(",")?;
        }
    }

    fn primary(&mut self) -> R<Ex> {
        let Some(t) = self.peek().cloned() else { return unsupported("an incomplete expression") };
        self.k += 1;
        match t.kind {
            TokKind::Number => {
                // A size and a based number may be two tokens: `8 'h1F`.
                if !t.text.contains('\'') {
                    if let Some(y) = self.peek().filter(|y| y.kind == TokKind::Number && y.text.starts_with('\'')).cloned() {
                        self.k += 1;
                        return literal(&format!("{}{}", t.text, y.text));
                    }
                }
                literal(&t.text)
            }
            TokKind::String => Ok(Ex::Str(t.text.trim_matches('"').to_string())),
            TokKind::Directive => {
                if self.at("(") {
                    return unsupported(format!("the macro call `{}`", t.text));
                }
                Ok(Ex::Macro(t.text.clone()))
            }
            TokKind::SysIdent => {
                let name = t.text.clone();
                if !self.eat("(") {
                    return unsupported(format!("`{name}`"));
                }
                // The raw argument tokens too: `$bits` may take a type.
                let from = self.k;
                let mut depth = 1;
                let mut j = self.k;
                while j < self.t.len() {
                    if self.t[j].is_punct("(") {
                        depth += 1;
                    } else if self.t[j].is_punct(")") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    j += 1;
                }
                let raw = self.t[from..j.min(self.t.len())].to_vec();
                let args = if matches!(name.as_str(), "$bits") {
                    self.k = (j + 1).min(self.t.len());
                    Vec::new()
                } else {
                    self.args()?
                };
                Ok(Ex::Sys(name, args, raw))
            }
            TokKind::Ident => {
                // `pkg::name`: the last name resolves.
                let mut last = t.clone();
                while self.at("::") && self.t.get(self.k + 1).is_some_and(|x| x.kind == TokKind::Ident) {
                    last = self.t[self.k + 1].clone();
                    self.k += 2;
                }
                if self.at(".") {
                    return unsupported(format!("the hierarchical or member name `{}.`", t.text));
                }
                if self.eat("(") {
                    let args = self.args()?;
                    return Ok(Ex::Call(last.text.clone(), last.span, args));
                }
                Ok(Ex::Name(last.text.clone(), last.span))
            }
            TokKind::Keyword => {
                let ty = match t.text.as_str() {
                    "signed" | "unsigned" => {
                        if self.eat("'") {
                            self.expect("(")?;
                            let v = self.expr()?;
                            self.expect(")")?;
                            return Ok(Ex::Cast(Box::new(CastTo::Signed(t.text == "signed")), Box::new(v)));
                        }
                        return unsupported(format!("`{}`", t.text));
                    }
                    k => keyword_type(k),
                };
                if let (Some(ty), true) = (ty, self.at("'")) {
                    self.k += 1;
                    self.expect("(")?;
                    let v = self.expr()?;
                    self.expect(")")?;
                    return Ok(Ex::Cast(Box::new(CastTo::Type(ty)), Box::new(v)));
                }
                unsupported(format!("`{}`", t.text))
            }
            TokKind::Punct => match t.text.as_str() {
                "(" => {
                    let x = self.expr()?;
                    self.expect(")")?;
                    Ok(x)
                }
                "{" => {
                    let first = self.expr()?;
                    if self.eat("{") {
                        // Replication: `{n{a, b}}`.
                        let mut items = vec![self.expr()?];
                        while self.eat(",") {
                            items.push(self.expr()?);
                        }
                        self.expect("}")?;
                        self.expect("}")?;
                        return Ok(Ex::Repl(Box::new(first), items));
                    }
                    let mut items = vec![first];
                    while self.eat(",") {
                        items.push(self.expr()?);
                    }
                    self.expect("}")?;
                    Ok(Ex::Concat(items))
                }
                "'" => match self.peek().map(|x| x.text.as_str()) {
                    Some("0") | Some("1") => {
                        let one = self.peek().unwrap().text == "1";
                        self.k += 1;
                        Ok(Ex::Fill(one))
                    }
                    _ => unsupported("an assignment pattern"),
                },
                p => unsupported(format!("`{p}`")),
            },
            _ => unsupported(format!("`{}`", t.text)),
        }
    }
}

/// The width and signedness of an integral type keyword.
fn keyword_type(k: &str) -> Option<IntType> {
    Some(match k {
        "bit" | "logic" | "reg" => IntType { width: 1, signed: false },
        "byte" => IntType { width: 8, signed: true },
        "shortint" => IntType { width: 16, signed: true },
        "int" | "integer" => IntType { width: 32, signed: true },
        "longint" => IntType { width: 64, signed: true },
        "time" => IntType { width: 64, signed: false },
        _ => return None,
    })
}

/// An integer literal: `4`, `1_000`, `'d4`, `8'hFF`, `4'sb0101`, `'1`.
fn literal(s: &str) -> R<Ex> {
    let s: String = s.chars().filter(|c| *c != '_').collect();
    if s == "'0" || s == "'1" {
        return Ok(Ex::Fill(s == "'1"));
    }
    if s.contains('.') || s.contains(['e', 'E']) && !s.contains('\'') {
        return unsupported("a real number");
    }
    let Some(q) = s.find('\'') else {
        let v: u64 = s.parse().map_err(|_| Unknown { state: State::Unsupported, why: format!("the number {s}") })?;
        if v > i32::MAX as u64 {
            // An unsized number wider than 32 bits keeps its width.
            let w = 64 - v.leading_zeros();
            return Ok(Ex::Num(Val::int(v as i128, w.max(32), true)));
        }
        return Ok(Ex::Num(Val::int(v as i128, 32, true)));
    };
    let size: Option<u32> = if q == 0 { None } else { Some(s[..q].trim().parse().map_err(|_| Unknown { state: State::Unsupported, why: format!("the number {s}") })?) };
    let mut rest = &s[q + 1..];
    let signed = rest.starts_with(['s', 'S']);
    if signed {
        rest = &rest[1..];
    }
    let (radix, digits) = match rest.chars().next() {
        Some('h' | 'H') => (16, &rest[1..]),
        Some('d' | 'D') => (10, &rest[1..]),
        Some('o' | 'O') => (8, &rest[1..]),
        Some('b' | 'B') => (2, &rest[1..]),
        _ => return unsupported(format!("the number {s}")),
    };
    let digits = digits.trim();
    if digits.chars().any(|c| matches!(c, 'x' | 'X' | 'z' | 'Z' | '?')) {
        return unsupported("an x or z value");
    }
    let width = size.unwrap_or(32);
    if width == 0 || width > MAX_WIDTH {
        return unsupported(format!("a {width}-bit value"));
    }
    let v = u128::from_str_radix(digits, radix).map_err(|_| Unknown { state: State::Unsupported, why: format!("the number {s}") })?;
    if v > u64::MAX as u128 {
        return unsupported(format!("the number {s}"));
    }
    Ok(Ex::Num(Val::int(v as i128, width, signed)))
}

/// Parses all of `t` as one expression.
fn parse(t: &[Token]) -> R<Ex> {
    if t.is_empty() {
        return unsupported("an empty expression");
    }
    if t.len() > MAX_EXPR_TOKENS {
        return incomplete("an expression too long");
    }
    let mut p = P { t, k: 0, nest: 0 };
    let x = p.expr()?;
    if p.k != t.len() {
        if t[p.k].is_punct(".") {
            return unsupported("a member select or hierarchical name");
        }
        return unsupported(format!("`{}`", t[p.k].text));
    }
    Ok(x)
}

// ------------------------------------------------------------------ frames

/// Values of declarations in a scope: an instance's parameters, a generate loop's genvar, a
/// function's locals; looked up through the chain of parents.
#[derive(Debug, Default)]
pub struct Frame {
    pub vals: Vec<((u32, u32), R<Val>)>,
    pub parent: Option<Arc<Frame>>,
}

impl Frame {
    pub fn get(&self, key: (u32, u32)) -> Option<&R<Val>> {
        let mut f = Some(self);
        while let Some(x) = f {
            if let Some((_, v)) = x.vals.iter().rev().find(|(k, _)| *k == key) {
                return Some(v);
            }
            f = x.parent.as_deref();
        }
        None
    }

    pub fn with(parent: &Arc<Frame>, vals: Vec<((u32, u32), R<Val>)>) -> Arc<Frame> {
        Arc::new(Frame { vals, parent: Some(parent.clone()) })
    }
}

/// A parsed constant function, made once.
struct Func {
    ret: Ty,
    /// Arguments: key and type.
    args: Vec<((u32, u32), Ty)>,
    body: Vec<Stmt>,
    unit: UnitId,
}

#[derive(Debug)]
enum Stmt {
    /// A local's declaration (with its type and initializer).
    Local((u32, u32), Ty, Option<Ex>),
    /// `x = e`, `x += e`, `x++`: the target's key, the operator, the value.
    Assign((u32, u32), &'static str, Option<Ex>),
    Return(Option<Ex>),
    If(Ex, Box<Stmt>, Option<Box<Stmt>>),
    While(Ex, Box<Stmt>),
    For(Vec<Stmt>, Option<Ex>, Vec<Stmt>, Box<Stmt>),
    Repeat(Ex, Box<Stmt>),
    Block(Vec<Stmt>),
    Nop,
}

/// What a function body's run ended with.
enum Flow {
    Next,
    Return(Option<Val>),
}

/// The evaluator: one per request, with its memos.
pub struct Eval<'a> {
    pub a: &'a Analysis,
    tokens: FxHashMap<FileId, Option<Arc<Vec<Token>>>>,
    /// Package and compilation-unit constants by declaration.
    constants: FxHashMap<(u32, u32), R<Val>>,
    /// Constants being evaluated (a cycle is invalid).
    evaluating: Vec<(u32, u32)>,
    funcs: FxHashMap<(u32, u32), Option<Arc<Func>>>,
    steps: usize,
    nest: usize,
}

impl<'a> Eval<'a> {
    pub fn new(a: &'a Analysis) -> Self {
        Eval { a, tokens: FxHashMap::default(), constants: FxHashMap::default(), evaluating: Vec::new(), funcs: FxHashMap::default(), steps: 0, nest: 0 }
    }

    pub fn file_tokens(&mut self, f: FileId) -> Option<Arc<Vec<Token>>> {
        let a = self.a;
        self.tokens.entry(f).or_insert_with(|| a.file_tokens(f)).clone()
    }

    /// The tokens from `first` to `last` (both included), when both are in one file.
    pub fn tokens_of(&mut self, e: (Span, Span)) -> Option<Vec<Token>> {
        let (first, last) = e;
        if first.file != last.file || last.end < first.start {
            return None;
        }
        let toks = self.file_tokens(first.file)?;
        let lo = toks.partition_point(|t| t.span.start < first.start);
        let hi = toks.partition_point(|t| t.span.start < last.end);
        (lo < hi && toks[lo].span == first).then(|| toks[lo..hi].to_vec())
    }

    /// The value of the expression `t` (written in `unit`) in `frame`, in a context `ctx` bits
    /// wide (0: self-determined).
    pub fn eval_tokens(&mut self, t: &[Token], unit: UnitId, frame: &Arc<Frame>, ctx: u32) -> R<Val> {
        let x = parse(t)?;
        self.eval(&x, unit, frame, ctx)
    }

    /// `t`'s value converted to type `ty`.
    pub fn eval_as(&mut self, t: &[Token], unit: UnitId, frame: &Arc<Frame>, ty: &Ty) -> R<Val> {
        let ctx = match ty {
            Ty::Int(i) => i.width,
            _ => 0,
        };
        let v = self.eval_tokens(t, unit, frame, ctx)?;
        convert(v, ty)
    }

    fn enter(&mut self) -> R<()> {
        self.nest += 1;
        if self.nest > MAX_NEST {
            self.nest -= 1;
            return incomplete("expressions or calls nested too deeply");
        }
        if self.a.cancelled() {
            self.nest -= 1;
            return incomplete("cancelled");
        }
        Ok(())
    }

    fn eval(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>, ctx: u32) -> R<Val> {
        self.enter()?;
        let r = self.eval_inner(x, unit, frame, ctx);
        self.nest -= 1;
        r
    }

    /// The self-determined width of `x` (0 for strings and types).
    fn width_of(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>) -> R<u32> {
        Ok(match x {
            Ex::Fill(_) => 1,
            Ex::Bin(op, l, r) => match *op {
                "==" | "!=" | "===" | "!==" | "<" | "<=" | ">" | ">=" | "&&" | "||" => 1,
                "<<" | ">>" | "<<<" | ">>>" | "**" => self.width_of(l, unit, frame)?,
                _ => self.width_of(l, unit, frame)?.max(self.width_of(r, unit, frame)?),
            },
            Ex::Unary(op, v) => match *op {
                "+" | "-" | "~" => self.width_of(v, unit, frame)?,
                _ => 1,
            },
            Ex::Cond(_, a, b) => self.width_of(a, unit, frame)?.max(self.width_of(b, unit, frame)?),
            _ => self.eval(x, unit, frame, 0)?.width(),
        })
    }

    fn signed_of(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>) -> R<bool> {
        Ok(match x {
            Ex::Fill(_) => false,
            Ex::Bin(op, l, r) => match *op {
                "==" | "!=" | "===" | "!==" | "<" | "<=" | ">" | ">=" | "&&" | "||" => false,
                "<<" | ">>" | "<<<" | ">>>" | "**" => self.signed_of(l, unit, frame)?,
                _ => self.signed_of(l, unit, frame)? && self.signed_of(r, unit, frame)?,
            },
            Ex::Unary(op, v) => match *op {
                "+" | "-" | "~" => self.signed_of(v, unit, frame)?,
                _ => false,
            },
            Ex::Cond(_, a, b) => self.signed_of(a, unit, frame)? && self.signed_of(b, unit, frame)?,
            _ => self.eval(x, unit, frame, 0)?.signed(),
        })
    }

    fn eval_inner(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>, ctx: u32) -> R<Val> {
        let wide = |w: u32| if w > MAX_WIDTH { unsupported(format!("a {w}-bit value")) } else { Ok(w) };
        match x {
            Ex::Num(v) => Ok(v.clone()),
            Ex::Fill(one) => {
                let w = ctx.max(1);
                Ok(Val::Int { bits: if *one { mask(w) } else { 0 }, width: w, signed: false })
            }
            Ex::Str(s) => Ok(Val::Str(s.clone())),
            Ex::Name(n, sp) => self.name(n, *sp, unit, frame),
            Ex::Macro(m) => self.macro_value(m, unit, frame),
            Ex::Call(n, sp, args) => self.call(n, *sp, args, unit, frame),
            Ex::Sys(n, args, raw) => self.system(n, args, raw, unit, frame),
            Ex::Unary(op, v) => match *op {
                "+" | "-" | "~" => {
                    let w = wide(self.width_of(v, unit, frame)?.max(ctx))?;
                    let s = self.signed_of(v, unit, frame)?;
                    let a = self.eval(v, unit, frame, w)?.resize(w, s)?;
                    let n = a.num().unwrap_or(0);
                    Ok(match *op {
                        "-" => Val::int(-n, w, s),
                        "~" => Val::int(!n, w, s),
                        _ => a,
                    })
                }
                "!" => Ok(Val::int((!self.eval(v, unit, frame, 0)?.truth()?) as i128, 1, false)),
                op => {
                    let a = self.eval(v, unit, frame, 0)?;
                    let Val::Int { bits, width, .. } = a else { return unsupported("a reduction of a string") };
                    let ones = (bits & mask(width)).count_ones();
                    let all = bits & mask(width) == mask(width);
                    let r = match op {
                        "&" => all,
                        "~&" => !all,
                        "|" => bits != 0,
                        "~|" => bits == 0,
                        "^" => ones % 2 == 1,
                        _ => ones % 2 == 0,
                    };
                    Ok(Val::int(r as i128, 1, false))
                }
            },
            Ex::Bin(op, l, r) => self.binary(op, l, r, unit, frame, ctx),
            Ex::Cond(c, a, b) => {
                let c = self.eval(c, unit, frame, 0)?.truth()?;
                let la = self.eval(a, unit, frame, 0)?;
                let lb = self.eval(b, unit, frame, 0)?;
                if let (Val::Str(_), Val::Str(_)) = (&la, &lb) {
                    return Ok(if c { la } else { lb });
                }
                let w = wide(la.width().max(lb.width()).max(ctx))?;
                let s = la.signed() && lb.signed();
                let v = if c { self.eval(a, unit, frame, w)? } else { self.eval(b, unit, frame, w)? };
                v.resize(w, s)
            }
            Ex::Concat(items) => {
                let mut bits: u128 = 0;
                let mut width = 0u32;
                let mut strings = String::new();
                let mut all_str = true;
                for it in items {
                    let v = self.eval(it, unit, frame, 0)?;
                    match v {
                        Val::Str(s) => strings.push_str(&s),
                        Val::Int { bits: b, width: w, .. } => {
                            all_str = false;
                            width += w;
                            if width > MAX_WIDTH {
                                return unsupported(format!("a {width}-bit value"));
                            }
                            bits = (bits << w) | b as u128;
                        }
                        Val::Type(_) => return unsupported("a type in a concatenation"),
                    }
                }
                if all_str && !items.is_empty() {
                    return Ok(Val::Str(strings));
                }
                if !strings.is_empty() {
                    return unsupported("a concatenation of strings and numbers");
                }
                Ok(Val::Int { bits: bits as u64, width, signed: false })
            }
            Ex::Repl(n, items) => {
                let n = self.eval(n, unit, frame, 0)?.num().unwrap_or(-1);
                if !(0..=MAX_WIDTH as i128).contains(&n) {
                    return unsupported(format!("a replication {n} times"));
                }
                let one = self.eval(&Ex::Concat(items.clone()), unit, frame, 0)?;
                let Val::Int { bits, width, .. } = one else { return unsupported("a replication of strings") };
                let total = width * n as u32;
                if total > MAX_WIDTH || total == 0 {
                    return unsupported(format!("a {total}-bit value"));
                }
                let mut out: u128 = 0;
                for _ in 0..n {
                    out = (out << width) | bits as u128;
                }
                Ok(Val::Int { bits: out as u64, width: total, signed: false })
            }
            Ex::Select(base, kind, a, b) => {
                let v = self.eval(base, unit, frame, 0)?;
                let Val::Int { bits, width, .. } = v else { return unsupported("a select of a string") };
                // Only the value's own bits, numbered from 0: a name declared with another
                // range would be read wrong.
                if !matches!(**base, Ex::Num(_) | Ex::Concat(_) | Ex::Repl(..)) && !self.lsb_zero(base, unit) {
                    return unsupported("a select of a name whose range does not end at 0");
                }
                let i = self.eval(a, unit, frame, 0)?.num().unwrap_or(-1);
                let (lo, w) = match (*kind, b) {
                    ("", _) => (i, 1),
                    (":", Some(b)) => {
                        let j = self.eval(b, unit, frame, 0)?.num().unwrap_or(-1);
                        (i.min(j), (i - j).abs() + 1)
                    }
                    ("+:", Some(b)) => (i, self.eval(b, unit, frame, 0)?.num().unwrap_or(0)),
                    ("-:", Some(b)) => {
                        let w = self.eval(b, unit, frame, 0)?.num().unwrap_or(0);
                        (i - w + 1, w)
                    }
                    _ => return unsupported("a select"),
                };
                if lo < 0 || w <= 0 || lo + w > width as i128 {
                    return unsupported("a select outside the value");
                }
                Ok(Val::int(((bits >> lo) & mask(w as u32)) as i128, w as u32, false))
            }
            Ex::Cast(to, v) => match &**to {
                CastTo::Signed(s) => {
                    let a = self.eval(v, unit, frame, 0)?;
                    let w = a.width();
                    Ok(Val::Int { bits: if let Val::Int { bits, .. } = a { bits } else { return unsupported("a cast of a string") }, width: w, signed: *s })
                }
                CastTo::Type(t) => {
                    let a = self.eval(v, unit, frame, t.width)?;
                    a.resize(t.width, t.signed).map(|x| if let Val::Int { bits, width, .. } = x { Val::Int { bits, width, signed: t.signed } } else { x })
                }
                CastTo::Size(n) => {
                    // `my_t'(x)`: a cast to a user type.
                    if let Ex::Name(name, sp) = n {
                        if self.a.resolve_at_span(unit, *sp).is_some_and(|r| self.a.decl(r).kind == DeclKind::Typedef) {
                            let t = self.user_type(name, *sp, unit, frame)?;
                            let a = self.eval(v, unit, frame, t.width)?;
                            return a.resize(t.width, a.signed()).map(|x| if let Val::Int { bits, width, .. } = x { Val::Int { bits, width, signed: t.signed } } else { x });
                        }
                    }
                    let n = self.eval(n, unit, frame, 0)?.num().unwrap_or(0);
                    if n <= 0 || n > MAX_WIDTH as i128 {
                        return unsupported(format!("a cast to {n} bits"));
                    }
                    let s = self.signed_of(v, unit, frame)?;
                    let a = self.eval(v, unit, frame, n as u32)?;
                    a.resize(n as u32, s)
                }
            },
        }
    }

    /// Whether name `x` (a parameter) has no declared range, or one ending at bit 0.
    fn lsb_zero(&mut self, x: &Ex, unit: UnitId) -> bool {
        let Ex::Name(_, sp) = x else { return false };
        let Some(r) = self.a.resolve_at_span(unit, *sp) else { return false };
        let d = self.a.decl(r);
        let text = d.ty.as_ref().map(|t| t.text.to_string()).unwrap_or_default();
        !text.contains('[') || text.contains(":0]")
    }

    fn binary(&mut self, op: &'static str, l: &Ex, r: &Ex, unit: UnitId, frame: &Arc<Frame>, ctx: u32) -> R<Val> {
        let wide = |w: u32| if w > MAX_WIDTH { unsupported(format!("a {w}-bit value")) } else { Ok(w) };
        match op {
            "&&" | "||" => {
                let a = self.eval(l, unit, frame, 0)?.truth()?;
                // Short-circuit: the other side may be unknown.
                if (op == "&&" && !a) || (op == "||" && a) {
                    return Ok(Val::int(a as i128, 1, false));
                }
                let b = self.eval(r, unit, frame, 0)?.truth()?;
                Ok(Val::int(b as i128, 1, false))
            }
            "==" | "!=" | "===" | "!==" | "<" | "<=" | ">" | ">=" => {
                let la = self.eval(l, unit, frame, 0)?;
                let lb = self.eval(r, unit, frame, 0)?;
                if let (Val::Str(a), Val::Str(b)) = (&la, &lb) {
                    let o = a.cmp(b);
                    let t = match op {
                        "==" | "===" => o.is_eq(),
                        "!=" | "!==" => o.is_ne(),
                        "<" => o.is_lt(),
                        "<=" => o.is_le(),
                        ">" => o.is_gt(),
                        _ => o.is_ge(),
                    };
                    return Ok(Val::int(t as i128, 1, false));
                }
                let w = wide(self.width_of(l, unit, frame)?.max(self.width_of(r, unit, frame)?))?;
                let s = la.signed() && lb.signed();
                let a = self.eval(l, unit, frame, w)?.resize(w, s)?.num().unwrap_or(0);
                let b = self.eval(r, unit, frame, w)?.resize(w, s)?.num().unwrap_or(0);
                let t = match op {
                    "==" | "===" => a == b,
                    "!=" | "!==" => a != b,
                    "<" => a < b,
                    "<=" => a <= b,
                    ">" => a > b,
                    _ => a >= b,
                };
                Ok(Val::int(t as i128, 1, false))
            }
            "<<" | ">>" | "<<<" | ">>>" => {
                let w = wide(self.width_of(l, unit, frame)?.max(ctx))?;
                let s = self.signed_of(l, unit, frame)?;
                let a = self.eval(l, unit, frame, w)?.resize(w, s)?;
                let n = self.eval(r, unit, frame, 0)?.num().unwrap_or(-1);
                if n < 0 {
                    return unsupported("a negative shift");
                }
                let n = n.min(127) as u32;
                let v = a.num().unwrap_or(0);
                Ok(match op {
                    "<<" | "<<<" => Val::int(if n >= w { 0 } else { v << n }, w, s),
                    ">>>" if s => Val::int(v >> n.min(127), w, s),
                    _ => {
                        let bits = if let Val::Int { bits, .. } = a { bits } else { 0 };
                        Val::int(if n >= 64 { 0 } else { (bits >> n) as i128 }, w, s)
                    }
                })
            }
            "**" => {
                let w = wide(self.width_of(l, unit, frame)?.max(ctx))?;
                let s = self.signed_of(l, unit, frame)?;
                let a = self.eval(l, unit, frame, w)?.resize(w, s)?.num().unwrap_or(0);
                let b = self.eval(r, unit, frame, 0)?.num().unwrap_or(-1);
                if b < 0 {
                    return unsupported("a negative power");
                }
                let mut v: i128 = 1;
                for _ in 0..b.min(128) {
                    v = v.wrapping_mul(a) & ((1i128 << 64) - 1).max(0);
                    if v == 0 {
                        break;
                    }
                }
                Ok(Val::int(v, w, s))
            }
            _ => {
                let w = wide(self.width_of(l, unit, frame)?.max(self.width_of(r, unit, frame)?).max(ctx))?;
                let s = self.signed_of(l, unit, frame)? && self.signed_of(r, unit, frame)?;
                let a = self.eval(l, unit, frame, w)?.resize(w, s)?.num().unwrap_or(0);
                let b = self.eval(r, unit, frame, w)?.resize(w, s)?.num().unwrap_or(0);
                let v = match op {
                    "+" => a.wrapping_add(b),
                    "-" => a.wrapping_sub(b),
                    "*" => a.wrapping_mul(b),
                    "/" | "%" if b == 0 => return invalid("a division by zero"),
                    "/" => a / b,
                    "%" => a % b,
                    "&" => a & b,
                    "|" => a | b,
                    "^" => a ^ b,
                    _ => !(a ^ b),
                };
                Ok(Val::int(v, w, s))
            }
        }
    }

    /// The value of a name: in the frame, else a package or compilation-unit constant, else
    /// an enum member.
    fn name(&mut self, n: &str, sp: Span, unit: UnitId, frame: &Arc<Frame>) -> R<Val> {
        let a = self.a;
        let Some(r) = a.resolve_at_span(unit, sp) else { return unresolved(format!("`{n}` is not resolved")) };
        let key = a.decl_key(a.canonical(r));
        if let Some(v) = frame.get(key) {
            return v.clone();
        }
        let d = a.decl(r);
        let parent = d.parent.map(|p| a.unit(r.unit).parsed.decl(p).kind);
        match d.kind {
            DeclKind::Parameter | DeclKind::LocalParam if matches!(parent, Some(DeclKind::Package | DeclKind::Unit) | None) => self.constant(r),
            DeclKind::EnumMember => self.enum_member(r),
            DeclKind::Parameter | DeclKind::LocalParam | DeclKind::Genvar => unresolved(format!("`{n}` has no value here")),
            DeclKind::TypeParam => unsupported(format!("the type parameter `{n}` in an expression")),
            _ => unsupported(format!("`{n}` is not a constant")),
        }
    }

    /// A package or compilation-unit parameter's value (made once).
    pub fn constant(&mut self, r: DeclRef) -> R<Val> {
        let key = self.a.decl_key(self.a.canonical(r));
        if let Some(v) = self.constants.get(&key) {
            return v.clone();
        }
        if self.evaluating.contains(&key) {
            return invalid(format!("`{}` depends on itself", self.a.decl(r).name));
        }
        self.evaluating.push(key);
        let root = Arc::new(Frame::default());
        let v = self.declared_value(r, &root);
        self.evaluating.pop();
        if v.as_ref().err().is_none_or(|e| e.why != "cancelled") {
            self.constants.insert(key, v.clone());
        }
        v
    }

    /// The value parameter `r`'s declaration gives it (its default) in `frame`, converted to
    /// its declared type.
    pub fn declared_value(&mut self, r: DeclRef, frame: &Arc<Frame>) -> R<Val> {
        let d = self.a.decl(r);
        if d.kind == DeclKind::TypeParam {
            return if d.detail.is_empty() { unresolved(format!("`{}` has no default", d.name)) } else { Ok(Val::Type(d.detail.to_string())) };
        }
        let Some(value) = self.default_tokens(r) else { return unresolved(format!("`{}` has no default", d.name)) };
        let ty = self.declared_type(r, frame)?;
        self.eval_as(&value, r.unit, frame, &ty)
    }

    /// The tokens of parameter `r`'s default (after its `=`).
    pub fn default_tokens(&mut self, r: DeclRef) -> Option<Vec<Token>> {
        let d = self.a.decl(r);
        let toks = self.file_tokens(d.name_span.file)?;
        let mut lo = toks.partition_point(|t| t.span.start < d.name_span.end);
        // Unpacked dimensions of a parameter array: not a value this evaluator covers.
        if toks.get(lo).is_some_and(|t| t.is_punct("[")) {
            return None;
        }
        if !toks.get(lo)?.is_punct("=") {
            return None;
        }
        lo += 1;
        let mut hi = lo;
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
        Some(toks[lo..hi].to_vec())
    }

    /// Parameter `r`'s declared type: the tokens between `parameter`/`localparam` and the
    /// first name of its declaration.
    pub fn declared_type(&mut self, r: DeclRef, frame: &Arc<Frame>) -> R<Ty> {
        let d = self.a.decl(r);
        let Some(toks) = self.file_tokens(d.name_span.file) else { return Ok(Ty::Implicit) };
        let k = toks.partition_point(|t| t.span.start < d.span.start);
        let name = toks.partition_point(|t| t.span.start < d.name_span.start);
        if !toks.get(k).is_some_and(|t| t.is_kw("parameter") || t.is_kw("localparam")) || name <= k {
            return Ok(Ty::Implicit);
        }
        // The first name of the declaration: the first identifier not inside brackets that is
        // followed by `=`, `,`, `;`, `)` or `[`.
        let mut end = name;
        let mut depth = 0i32;
        for (j, t) in toks.iter().enumerate().take(name + 1).skip(k + 1) {
            if t.is_punct("[") || t.is_punct("(") {
                depth += 1;
            } else if t.is_punct("]") || t.is_punct(")") {
                depth -= 1;
            } else if depth == 0 && t.kind == TokKind::Ident && toks.get(j + 1).is_some_and(|n| n.is_punct("=") || n.is_punct(",") || n.is_punct(";") || n.is_punct(")") || n.is_punct("[")) {
                end = j;
                break;
            }
        }
        let ty: Vec<Token> = toks[k + 1..end].to_vec();
        self.type_of_tokens(&ty, r.unit, frame)
    }

    /// The type written by `t` (empty: implicit).
    pub fn type_of_tokens(&mut self, t: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<Ty> {
        if t.is_empty() {
            return Ok(Ty::Implicit);
        }
        if t.len() == 1 && t[0].is_kw("string") {
            return Ok(Ty::Str);
        }
        let mut k = 0;
        let mut base: Option<IntType> = None;
        let mut signed: Option<bool> = None;
        let mut user = false;
        while let Some(x) = t.get(k) {
            if x.is_kw("signed") || x.is_kw("unsigned") {
                signed = Some(x.is_kw("signed"));
                k += 1;
            } else if let Some(b) = (x.kind == TokKind::Keyword).then(|| keyword_type(&x.text)).flatten() {
                if base.is_some() {
                    return unsupported(format!("the type `{}`", text_of(t)));
                }
                base = Some(b);
                k += 1;
            } else if x.kind == TokKind::Ident && base.is_none() && !user {
                // A user type: a typedef of an integral type or an enum.
                let mut last = x;
                k += 1;
                while t.get(k).is_some_and(|y| y.is_punct("::")) && t.get(k + 1).is_some_and(|y| y.kind == TokKind::Ident) {
                    last = &t[k + 1];
                    k += 2;
                }
                let it = self.user_type(&last.text, last.span, unit, frame)?;
                base = Some(it);
                user = true;
            } else {
                break;
            }
        }
        // Packed dimensions.
        let mut width: u32 = base.map_or(1, |b| b.width);
        let mut dims = 0;
        while t.get(k).is_some_and(|x| x.is_punct("[")) {
            let close = matching(t, k).ok_or_else(|| Unknown { state: State::Unsupported, why: "a range".into() })?;
            let inner = &t[k + 1..close];
            let n = self.range_width(inner, unit, frame)?;
            width = if dims == 0 && base.is_some_and(|b| b.width > 1) && !user { return unsupported(format!("the type `{}`", text_of(t))) } else if dims == 0 && base.is_none_or(|b| b.width == 1) { n } else { width.checked_mul(n).unwrap_or(u32::MAX) };
            dims += 1;
            k = close + 1;
        }
        if k != t.len() {
            return unsupported(format!("the type `{}`", text_of(t)));
        }
        if width > MAX_WIDTH {
            return unsupported(format!("a {width}-bit type"));
        }
        let signed = signed.unwrap_or(base.is_some_and(|b| b.signed) && dims == 0);
        Ok(Ty::Int(IntType { width, signed }))
    }

    /// The width of a range `a:b` (or a size `n`).
    pub fn range_width(&mut self, inner: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<u32> {
        let parts = split_colon(inner);
        let n = match parts.as_slice() {
            [one] => self.eval_tokens(one, unit, frame, 0)?.num().unwrap_or(-1),
            [a, b] => {
                let a = self.eval_tokens(a, unit, frame, 0)?.num().unwrap_or(0);
                let b = self.eval_tokens(b, unit, frame, 0)?.num().unwrap_or(0);
                (a - b).abs() + 1
            }
            _ => return unsupported("a range"),
        };
        if n <= 0 || n > u32::MAX as i128 {
            return unsupported(format!("a range of {n}"));
        }
        Ok(n as u32)
    }

    /// The bounds of a range `a:b` (or `[n]`: `0:n-1`).
    pub fn range_bounds(&mut self, inner: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<(i128, i128)> {
        let parts = split_colon(inner);
        match parts.as_slice() {
            [one] => {
                let n = self.eval_tokens(one, unit, frame, 0)?.num().unwrap_or(0);
                if n <= 0 {
                    return unsupported(format!("a size of {n}"));
                }
                Ok((0, n - 1))
            }
            [a, b] => Ok((self.eval_tokens(a, unit, frame, 0)?.num().unwrap_or(0), self.eval_tokens(b, unit, frame, 0)?.num().unwrap_or(0))),
            _ => unsupported("a range"),
        }
    }

    /// A user type named by `t`: a typedef of an integral type, or an enum (its base type).
    fn user_type(&mut self, text: &str, span: Span, unit: UnitId, frame: &Arc<Frame>) -> R<IntType> {
        let a = self.a;
        let t = UserName { text };
        let Some(r) = a.resolve_at_span(unit, span) else { return unresolved(format!("the type `{}` is not resolved", t.text)) };
        let d = a.decl(r);
        if d.kind != DeclKind::Typedef {
            return unsupported(format!("the type `{}`", t.text));
        }
        let Some(toks) = self.file_tokens(d.name_span.file) else { return unsupported(format!("the type `{}`", t.text)) };
        let k = toks.partition_point(|x| x.span.start < d.span.start);
        let name = toks.partition_point(|x| x.span.start < d.name_span.start);
        if !toks.get(k).is_some_and(|x| x.is_kw("typedef")) || name <= k + 1 {
            return unsupported(format!("the type `{}`", t.text));
        }
        let body: Vec<Token> = toks[k + 1..name].to_vec();
        self.enter()?;
        let r = if body.first().is_some_and(|x| x.is_kw("enum")) {
            // `enum [base] { ... }`: its base type, `int` by default.
            let brace = body.iter().position(|x| x.is_punct("{")).unwrap_or(body.len());
            let base = &body[1..brace];
            if base.is_empty() {
                Ok(IntType { width: 32, signed: true })
            } else {
                match self.type_of_tokens(base, r.unit, frame) {
                    Ok(Ty::Int(i)) => Ok(i),
                    Ok(_) => unsupported(format!("the type `{}`", t.text)),
                    Err(e) => Err(e),
                }
            }
        } else if body.first().is_some_and(|x| x.is_kw("struct") || x.is_kw("union")) {
            unsupported(format!("the struct or union type `{}`", t.text))
        } else {
            match self.type_of_tokens(&body, r.unit, frame) {
                Ok(Ty::Int(i)) => Ok(i),
                Ok(_) => unsupported(format!("the type `{}`", t.text)),
                Err(e) => Err(e),
            }
        };
        self.nest -= 1;
        r
    }

    /// An enum member's value: its own, or one more than the previous member's.
    fn enum_member(&mut self, r: DeclRef) -> R<Val> {
        let a = self.a;
        let d = a.decl(r);
        let Some(e) = d.parent else { return unsupported("an enum member") };
        let p = &a.unit(r.unit).parsed;
        let enum_decl = DeclRef { unit: r.unit, decl: e };
        let base = {
            let ed = a.decl(enum_decl);
            let text = ed.ty.as_ref().map(|t| t.text.to_string()).unwrap_or_default();
            let _ = text;
            IntType { width: 32, signed: true }
        };
        let root = Arc::new(Frame::default());
        let mut prev: Option<i128> = None;
        for &c in &p.decl(e).children {
            let m = DeclRef { unit: r.unit, decl: c };
            if a.decl(m).kind != DeclKind::EnumMember {
                continue;
            }
            let v = match self.default_tokens(m) {
                Some(t) => self.eval_tokens(&t, r.unit, &root, base.width)?.num().unwrap_or(0),
                None => prev.map_or(0, |x| x + 1),
            };
            if c == r.decl {
                return Ok(Val::int(v, base.width, base.signed));
            }
            prev = Some(v);
        }
        unsupported("an enum member")
    }

    fn macro_value(&mut self, m: &str, unit: UnitId, frame: &Arc<Frame>) -> R<Val> {
        let Some(defs) = self.a.global.macros.get(m) else { return unresolved(format!("the macro `{m}` is not defined")) };
        let defs = defs.clone();
        let mut value: Option<Val> = None;
        for d in defs.iter() {
            if d.params.is_some() {
                return unsupported(format!("the macro `{m}` with arguments"));
            }
            self.enter()?;
            let v = self.eval_tokens(&d.body, unit, frame, 0);
            self.nest -= 1;
            let v = v?;
            if value.as_ref().is_some_and(|w| *w != v) {
                return unsupported(format!("the macro `{m}` defined with different values"));
            }
            value = Some(v);
        }
        value.ok_or_else(|| Unknown { state: State::Unresolved, why: format!("the macro `{m}` is not defined") })
    }

    fn system(&mut self, n: &str, args: &[Ex], raw: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<Val> {
        match n {
            "$clog2" => {
                let [x] = args else { return unsupported("`$clog2` with other than one argument") };
                let v = self.eval(x, unit, frame, 0)?.num().unwrap_or(-1);
                if v < 0 {
                    return unsupported("`$clog2` of a negative value");
                }
                let mut r = 0i128;
                while (1i128 << r) < v {
                    r += 1;
                }
                Ok(Val::int(r, 32, true))
            }
            "$signed" | "$unsigned" => {
                let [x] = args else { return unsupported(format!("`{n}` with other than one argument")) };
                let v = self.eval(x, unit, frame, 0)?;
                let Val::Int { bits, width, .. } = v else { return unsupported(format!("`{n}` of a string")) };
                Ok(Val::Int { bits, width, signed: n == "$signed" })
            }
            "$bits" => {
                // A type, or an expression (its width).
                let w = match self.type_of_tokens(raw, unit, frame) {
                    Ok(Ty::Int(i)) => i.width,
                    _ => {
                        let x = parse(raw)?;
                        if let Ex::Name(_, sp) = &x {
                            // A name of a signal: its declared type.
                            if let Some(r) = self.a.resolve_at_span(unit, *sp) {
                                if matches!(self.a.decl(r).kind, DeclKind::Variable | DeclKind::Net | DeclKind::Port) {
                                    return self.signal_width(r, frame).map(|w| Val::int(w as i128, 32, true));
                                }
                            }
                        }
                        self.width_of(&x, unit, frame)?
                    }
                };
                Ok(Val::int(w as i128, 32, true))
            }
            _ => unsupported(format!("`{n}`")),
        }
    }

    /// The width of a signal's declared type (packed dimensions only).
    pub fn signal_width(&mut self, r: DeclRef, frame: &Arc<Frame>) -> R<u32> {
        let d = self.a.decl(r);
        let Some(toks) = self.file_tokens(d.name_span.file) else { return unsupported("a signal") };
        let name = toks.partition_point(|t| t.span.start < d.name_span.start);
        // The type: back from the name to the declaration's start, without the direction and
        // net or variable keywords.
        let start = toks.partition_point(|t| t.span.start < d.span.start).min(name);
        let mut ty: Vec<Token> = toks[start..name].iter().filter(|t| !(t.kind == TokKind::Keyword && matches!(t.text.as_str(), "input" | "output" | "inout" | "ref" | "var" | "wire" | "tri" | "wand" | "wor" | "supply0" | "supply1" | "uwire" | "const" | "static" | "automatic" | "rand" | "randc" | "local" | "protected"))).cloned().collect();
        // A list `logic [7:0] a, b`: the type ends before the first name.
        if let Some(i) = ty.iter().position(|t| t.is_punct(",")) {
            ty.truncate(i.saturating_sub(1));
        }
        if ty.is_empty() {
            return Ok(1);
        }
        match self.type_of_tokens(&ty, r.unit, frame)? {
            Ty::Int(i) => Ok(i.width),
            _ => unsupported("a signal that is not integral"),
        }
    }

    // ------------------------------------------------------------------ functions

    fn call(&mut self, n: &str, sp: Span, args: &[Ex], unit: UnitId, frame: &Arc<Frame>) -> R<Val> {
        let a = self.a;
        let Some(r) = a.resolve_at_span(unit, sp) else { return unresolved(format!("the function `{n}` is not resolved")) };
        if a.decl(r).kind != DeclKind::Function {
            return unsupported(format!("the call of `{n}`"));
        }
        let key = a.decl_key(a.canonical(r));
        let f = match self.funcs.get(&key) {
            Some(f) => f.clone(),
            None => {
                let f = self.parse_function(r).map(Arc::new);
                self.funcs.insert(key, f.clone());
                f
            }
        };
        let Some(f) = f else { return unsupported(format!("the function `{n}` (statements the evaluator does not cover)")) };
        if f.args.len() != args.len() {
            return unsupported(format!("the call of `{n}` with {} arguments", args.len()));
        }
        let mut vals = Vec::new();
        for ((k, ty), x) in f.args.iter().zip(args) {
            let ctx = if let Ty::Int(i) = ty { i.width } else { 0 };
            let v = self.eval(x, unit, frame, ctx)?;
            vals.push((*k, Ok(convert(v, ty)?)));
        }
        // The result variable: the function's name.
        vals.push((key, Ok(Val::int(0, if let Ty::Int(i) = &f.ret { i.width } else { 32 }, if let Ty::Int(i) = &f.ret { i.signed } else { true }))));
        // The caller's frame stays visible: a module's function may read its parameters.
        let local = Frame::with(frame, vals);
        self.enter()?;
        let mut fr = local;
        let flow = self.run_block(&f.body, f.unit, &mut fr, key);
        self.nest -= 1;
        let out = match flow? {
            Flow::Return(Some(v)) => v,
            _ => fr.get(key).cloned().unwrap_or_else(|| unresolved("no result"))?,
        };
        convert(out, &f.ret)
    }

    fn run_block(&mut self, body: &[Stmt], unit: UnitId, fr: &mut Arc<Frame>, fkey: (u32, u32)) -> R<Flow> {
        for s in body {
            if let Flow::Return(v) = self.run(s, unit, fr, fkey)? {
                return Ok(Flow::Return(v));
            }
        }
        Ok(Flow::Next)
    }

    fn step(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return incomplete("a function ran too many steps");
        }
        if self.steps % 256 == 0 && self.a.cancelled() {
            return incomplete("cancelled");
        }
        Ok(())
    }

    /// Sets `key` in the innermost frame (the function's).
    fn set(fr: &mut Arc<Frame>, key: (u32, u32), v: Val) {
        let mut vals: Vec<((u32, u32), R<Val>)> = fr.vals.clone();
        if let Some(x) = vals.iter_mut().find(|(k, _)| *k == key) {
            x.1 = Ok(v);
        } else {
            vals.push((key, Ok(v)));
        }
        *fr = Arc::new(Frame { vals, parent: fr.parent.clone() });
    }

    fn type_in(fr: &Arc<Frame>, key: (u32, u32)) -> Option<(u32, bool)> {
        match fr.vals.iter().find(|(k, _)| *k == key) {
            Some((_, Ok(Val::Int { width, signed, .. }))) => Some((*width, *signed)),
            _ => None,
        }
    }

    fn run(&mut self, s: &Stmt, unit: UnitId, fr: &mut Arc<Frame>, fkey: (u32, u32)) -> R<Flow> {
        self.step()?;
        match s {
            Stmt::Nop => Ok(Flow::Next),
            Stmt::Local(k, ty, init) => {
                let v = match init {
                    Some(x) => {
                        let f = fr.clone();
                        let ctx = if let Ty::Int(i) = ty { i.width } else { 0 };
                        convert(self.eval(x, unit, &f, ctx)?, ty)?
                    }
                    None => match ty {
                        Ty::Int(i) => Val::int(0, i.width, i.signed),
                        Ty::Str => Val::Str(String::new()),
                        Ty::Implicit => Val::int(0, 1, false),
                    },
                };
                Self::set(fr, *k, v);
                Ok(Flow::Next)
            }
            Stmt::Assign(k, op, x) => {
                let (w, signed) = Self::type_in(fr, *k).unwrap_or((32, true));
                let f = fr.clone();
                let cur = f.get(*k).cloned().unwrap_or_else(|| unresolved("an unknown variable"))?;
                let v = match (*op, x) {
                    ("=", Some(x)) => self.eval(x, unit, &f, w)?,
                    ("++", _) => Val::int(cur.num().unwrap_or(0) + 1, w, signed),
                    ("--", _) => Val::int(cur.num().unwrap_or(0) - 1, w, signed),
                    (op, Some(x)) => {
                        let b = self.eval(x, unit, &f, w)?.resize(w, signed)?.num().unwrap_or(0);
                        let a = cur.num().unwrap_or(0);
                        Val::int(
                            match op {
                                "+=" => a.wrapping_add(b),
                                "-=" => a.wrapping_sub(b),
                                "*=" => a.wrapping_mul(b),
                                "/=" | "%=" if b == 0 => return invalid("a division by zero"),
                                "/=" => a / b,
                                "%=" => a % b,
                                "&=" => a & b,
                                "|=" => a | b,
                                "^=" => a ^ b,
                                "<<=" => a << b.clamp(0, 127),
                                ">>=" => a >> b.clamp(0, 127),
                                _ => return unsupported(format!("`{op}`")),
                            },
                            w,
                            signed,
                        )
                    }
                    _ => return unsupported("an assignment"),
                };
                let v = if matches!(v, Val::Str(_)) { v } else { v.resize(w, signed)? };
                Self::set(fr, *k, v);
                Ok(Flow::Next)
            }
            Stmt::Return(x) => {
                let v = match x {
                    Some(x) => {
                        let f = fr.clone();
                        let (w, _) = Self::type_in(fr, fkey).unwrap_or((0, true));
                        Some(self.eval(x, unit, &f, w)?)
                    }
                    None => None,
                };
                Ok(Flow::Return(v))
            }
            Stmt::If(c, a, b) => {
                let f = fr.clone();
                if self.eval(c, unit, &f, 0)?.truth()? {
                    self.run(a, unit, fr, fkey)
                } else if let Some(b) = b {
                    self.run(b, unit, fr, fkey)
                } else {
                    Ok(Flow::Next)
                }
            }
            Stmt::While(c, body) => loop {
                self.step()?;
                let f = fr.clone();
                if !self.eval(c, unit, &f, 0)?.truth()? {
                    return Ok(Flow::Next);
                }
                if let Flow::Return(v) = self.run(body, unit, fr, fkey)? {
                    return Ok(Flow::Return(v));
                }
            },
            Stmt::Repeat(n, body) => {
                let f = fr.clone();
                let n = self.eval(n, unit, &f, 0)?.num().unwrap_or(0);
                for _ in 0..n.max(0) {
                    self.step()?;
                    if let Flow::Return(v) = self.run(body, unit, fr, fkey)? {
                        return Ok(Flow::Return(v));
                    }
                }
                Ok(Flow::Next)
            }
            Stmt::For(init, c, stepping, body) => {
                if let Flow::Return(v) = self.run_block(init, unit, fr, fkey)? {
                    return Ok(Flow::Return(v));
                }
                loop {
                    self.step()?;
                    if let Some(c) = c {
                        let f = fr.clone();
                        if !self.eval(c, unit, &f, 0)?.truth()? {
                            return Ok(Flow::Next);
                        }
                    }
                    if let Flow::Return(v) = self.run(body, unit, fr, fkey)? {
                        return Ok(Flow::Return(v));
                    }
                    self.run_block(stepping, unit, fr, fkey)?;
                }
            }
            Stmt::Block(b) => self.run_block(b, unit, fr, fkey),
        }
    }

    /// Function `r` as statements, or `None` when it uses what the interpreter does not cover.
    fn parse_function(&mut self, r: DeclRef) -> Option<Func> {
        let a = self.a;
        let d = a.decl(r);
        let toks = self.file_tokens(d.name_span.file)?;
        let lo = toks.partition_point(|t| t.span.start < d.span.start);
        let hi = toks.partition_point(|t| t.span.start < d.span.end);
        let t = &toks[lo..hi];
        let name = t.iter().position(|x| x.span == d.name_span)?;
        // The return type: between `function [automatic|static]` and the name.
        let mut k = 1;
        while t.get(k).is_some_and(|x| x.is_kw("automatic") || x.is_kw("static")) {
            k += 1;
        }
        let root = Arc::new(Frame::default());
        let ret = if name > k { self.type_of_tokens(&t[k..name], r.unit, &root).ok()? } else { Ty::Int(IntType { width: 1, signed: false }) };
        let ret = if let (Ty::Implicit, true) = (&ret, name > k) { return None } else { ret };
        // The arguments: the function's Argument declarations, in order.
        let mut args = Vec::new();
        for &c in &a.unit(r.unit).parsed.decl(r.decl).children {
            let ar = DeclRef { unit: r.unit, decl: c };
            let ad = a.decl(ar);
            if ad.kind != DeclKind::Argument {
                continue;
            }
            if !matches!(ad.dir, None | Some(dvi_syntax::ast::PortDir::Input)) {
                return None;
            }
            let ty = self.arg_type(ar).ok()?;
            args.push((a.decl_key(a.canonical(ar)), ty));
        }
        // The body: after the header's `;`.
        let mut j = name + 1;
        if t.get(j).is_some_and(|x| x.is_punct("(")) {
            j = matching(t, j)? + 1;
        }
        if !t.get(j)?.is_punct(";") {
            return None;
        }
        let end = t.iter().rposition(|x| x.is_kw("endfunction"))?;
        let body_toks = &t[j + 1..end];
        let mut sp = StmtParser { e: self, t: body_toks, k: 0, unit: r.unit, func: r, ok: true };
        let mut body = Vec::new();
        while sp.k < body_toks.len() {
            // Old-style argument declarations (`input int v;`) are not covered.
            body.push(sp.stmt()?);
        }
        sp.ok.then_some(Func { ret, args, body, unit: r.unit })
    }

    fn arg_type(&mut self, ar: DeclRef) -> R<Ty> {
        let d = self.a.decl(ar);
        let Some(toks) = self.file_tokens(d.name_span.file) else { return Ok(Ty::Implicit) };
        let name = toks.partition_point(|t| t.span.start < d.name_span.start);
        // Back from the name to the `(`, `,` or direction before it.
        let mut k = name;
        while k > 0 {
            let t = &toks[k - 1];
            if t.is_punct("(") || t.is_punct(",") || t.is_punct(";") || t.is_kw("input") {
                break;
            }
            if t.is_punct("]") {
                let mut depth = 0;
                while k > 0 {
                    k -= 1;
                    if toks[k].is_punct("]") {
                        depth += 1;
                    } else if toks[k].is_punct("[") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                continue;
            }
            k -= 1;
        }
        let ty: Vec<Token> = toks[k..name].to_vec();
        let root = Arc::new(Frame::default());
        let ty = self.type_of_tokens(&ty, ar.unit, &root)?;
        // An argument without a type is a 1-bit logic.
        Ok(if ty == Ty::Implicit { Ty::Int(IntType { width: 1, signed: false }) } else { ty })
    }
}

/// A parser of a constant function's statements.
struct StmtParser<'e, 'a, 't> {
    e: &'e mut Eval<'a>,
    t: &'t [Token],
    k: usize,
    unit: UnitId,
    /// The function parsed.
    func: DeclRef,
    ok: bool,
}

impl StmtParser<'_, '_, '_> {
    fn at(&self, p: &str) -> bool {
        self.t.get(self.k).is_some_and(|t| t.is_punct(p))
    }

    fn at_kw(&self, k: &str) -> bool {
        self.t.get(self.k).is_some_and(|t| t.is_kw(k))
    }

    /// The tokens up to `stop` at depth 0 (not consumed).
    fn until(&mut self, stop: &[&str]) -> Option<&[Token]> {
        let from = self.k;
        let mut depth = 0i32;
        while let Some(t) = self.t.get(self.k) {
            if depth == 0 && t.kind == TokKind::Punct && stop.contains(&t.text.as_str()) {
                return Some(&self.t[from..self.k]);
            }
            if t.is_punct("(") || t.is_punct("[") || t.is_punct("{") {
                depth += 1;
            } else if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            self.k += 1;
        }
        None
    }

    fn expr_until(&mut self, stop: &[&str]) -> Option<Ex> {
        let t = self.until(stop)?;
        parse(t).ok()
    }

    fn paren_expr(&mut self) -> Option<Ex> {
        if !self.at("(") {
            return None;
        }
        let close = matching(self.t, self.k)?;
        let x = parse(&self.t[self.k + 1..close]).ok()?;
        self.k = close + 1;
        Some(x)
    }

    fn key_of(&mut self, t: &Token) -> Option<(u32, u32)> {
        let a = self.e.a;
        if let Some(r) = a.resolve_at_span(self.unit, t.span) {
            return Some(a.decl_key(a.canonical(r)));
        }
        // A local's declaration: its name is no reference.
        let p = &a.unit(self.func.unit).parsed;
        let mut todo = vec![self.func.decl];
        while let Some(d) = todo.pop() {
            for &c in &p.decl(d).children {
                if p.decl(c).name_span == t.span {
                    return Some(a.decl_key(a.canonical(DeclRef { unit: self.func.unit, decl: c })));
                }
                todo.push(c);
            }
        }
        None
    }

    fn stmt(&mut self) -> Option<Stmt> {
        let t = self.t.get(self.k)?.clone();
        if t.is_punct(";") {
            self.k += 1;
            return Some(Stmt::Nop);
        }
        if t.is_kw("begin") {
            self.k += 1;
            if self.at(":") {
                self.k += 2;
            }
            let mut v = Vec::new();
            while !self.at_kw("end") {
                if self.k >= self.t.len() {
                    return None;
                }
                v.push(self.stmt()?);
            }
            self.k += 1;
            if self.at(":") {
                self.k += 2;
            }
            return Some(Stmt::Block(v));
        }
        if t.is_kw("return") {
            self.k += 1;
            let x = if self.at(";") { None } else { Some(self.expr_until(&[";"])?) };
            self.k += 1;
            return Some(Stmt::Return(x));
        }
        if t.is_kw("if") {
            self.k += 1;
            let c = self.paren_expr()?;
            let a = self.stmt()?;
            let b = if self.at_kw("else") {
                self.k += 1;
                Some(Box::new(self.stmt()?))
            } else {
                None
            };
            return Some(Stmt::If(c, Box::new(a), b));
        }
        if t.is_kw("while") {
            self.k += 1;
            let c = self.paren_expr()?;
            let b = self.stmt()?;
            return Some(Stmt::While(c, Box::new(b)));
        }
        if t.is_kw("repeat") {
            self.k += 1;
            let c = self.paren_expr()?;
            let b = self.stmt()?;
            return Some(Stmt::Repeat(c, Box::new(b)));
        }
        if t.is_kw("for") {
            self.k += 1;
            if !self.at("(") {
                return None;
            }
            let close = matching(self.t, self.k)?;
            self.k += 1;
            let mut init = Vec::new();
            while !self.at(";") {
                init.push(self.simple(&[",", ";"])?);
                if self.at(",") {
                    self.k += 1;
                }
            }
            self.k += 1;
            let c = if self.at(";") { None } else { Some(self.expr_until(&[";"])?) };
            self.k += 1;
            let mut stepping = Vec::new();
            while self.k < close {
                stepping.push(self.simple(&[",", ")"])?);
                if self.at(",") {
                    self.k += 1;
                }
            }
            self.k = close + 1;
            let b = self.stmt()?;
            return Some(Stmt::For(init, c, stepping, Box::new(b)));
        }
        let s = self.simple(&[";"])?;
        if !self.at(";") {
            return None;
        }
        self.k += 1;
        Some(s)
    }

    /// A declaration, an assignment or an increment, up to one of `stop` (not consumed).
    fn simple(&mut self, stop: &[&str]) -> Option<Stmt> {
        let t = self.t.get(self.k)?.clone();
        // A local: `int i = 0`, `logic [7:0] v`, `automatic int i`.
        let decl = t.kind == TokKind::Keyword && (keyword_type(&t.text).is_some() || matches!(t.text.as_str(), "automatic" | "static" | "signed" | "unsigned" | "string" | "var"));
        if decl {
            let from = self.k;
            while self.t.get(self.k).is_some_and(|x| x.kind == TokKind::Keyword && matches!(x.text.as_str(), "automatic" | "static" | "var")) {
                self.k += 1;
            }
            let ty_from = self.k;
            let name_at = (self.k..self.t.len()).find(|&j| self.t[j].kind == TokKind::Ident && self.t.get(j + 1).is_some_and(|n| n.is_punct("=") || n.is_punct(";") || n.is_punct(",")))?;
            let ty_toks: Vec<Token> = self.t[ty_from..name_at].to_vec();
            let root = Arc::new(Frame::default());
            let ty = self.e.type_of_tokens(&ty_toks, self.unit, &root).ok()?;
            let name = self.t[name_at].clone();
            let key = self.key_of(&name)?;
            self.k = name_at + 1;
            let init = if self.at("=") {
                self.k += 1;
                Some(self.expr_until(stop)?)
            } else {
                None
            };
            let _ = from;
            if self.at(",") && stop == [";"] {
                // `int a, b;`: one local per statement only.
                return None;
            }
            return Some(Stmt::Local(key, ty, init));
        }
        // `x++`, `++x`, `x = e`, `x += e`.
        let (name, op) = if t.is_punct("++") || t.is_punct("--") {
            let n = self.t.get(self.k + 1)?.clone();
            self.k += 2;
            (n, if t.is_punct("++") { "++" } else { "--" })
        } else if t.kind == TokKind::Ident {
            self.k += 1;
            let o = self.t.get(self.k)?.clone();
            const OPS: &[&str] = &["=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", "++", "--"];
            let Some(op) = OPS.iter().find(|x| o.is_punct(x)) else { return None };
            self.k += 1;
            (t.clone(), *op)
        } else {
            return None;
        };
        if name.kind != TokKind::Ident {
            return None;
        }
        let key = self.key_of(&name)?;
        let x = if op == "++" || op == "--" { None } else { Some(self.expr_until(stop)?) };
        Some(Stmt::Assign(key, op, x))
    }
}

/// `v` converted to type `ty`.
pub fn convert(v: Val, ty: &Ty) -> R<Val> {
    match (ty, v) {
        (Ty::Implicit, v) => Ok(v),
        (Ty::Str, v @ Val::Str(_)) => Ok(v),
        (Ty::Str, _) => unsupported("a number given to a string"),
        (Ty::Int(i), v @ Val::Int { .. }) => {
            let r = v.resize(i.width, v.signed())?;
            Ok(if let Val::Int { bits, width, .. } = r { Val::Int { bits, width, signed: i.signed } } else { r })
        }
        (Ty::Int(_), Val::Str(_)) => unsupported("a string given to a number"),
        (Ty::Int(_), Val::Type(_)) => unsupported("a type given to a number"),
    }
}

/// The index of the token closing the group opened at `t[open]`.
pub fn matching(t: &[Token], open: usize) -> Option<usize> {
    let (o, c) = match t.get(open)?.text.as_str() {
        "(" => ("(", ")"),
        "[" => ("[", "]"),
        "{" => ("{", "}"),
        _ => return None,
    };
    let mut depth = 0;
    for (j, x) in t.iter().enumerate().skip(open) {
        if x.is_punct(o) {
            depth += 1;
        } else if x.is_punct(c) {
            depth -= 1;
            if depth == 0 {
                return Some(j);
            }
        }
    }
    None
}

/// `t` split at its top-level `:`.
fn split_colon(t: &[Token]) -> Vec<&[Token]> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut from = 0;
    for (j, x) in t.iter().enumerate() {
        if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
            depth += 1;
        } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
            depth -= 1;
        } else if depth == 0 && x.is_punct(":") {
            out.push(&t[from..j]);
            from = j + 1;
        }
    }
    out.push(&t[from..]);
    out
}

fn text_of(t: &[Token]) -> String {
    t.iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------------ elaboration

/// Whether a generate branch, block or instance exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    Elaborated,
    /// Its condition is proven false.
    NotElaborated,
    /// Whether it exists is not known: why.
    Undetermined(String),
}

/// Where a parameter's value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Default,
    Override,
    Defparam,
    Local,
}

/// A parameter of an instance as shown.
#[derive(Clone, Debug, Serialize)]
pub struct ParamShow {
    pub name: String,
    /// Its value (when evaluated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    pub origin: Origin,
    /// `parameter`, `localparam` or `type`.
    pub kind: &'static str,
}

/// An instance's parameters: their values (the frame its body is evaluated in) and as shown.
pub struct InstParams {
    pub frame: Arc<Frame>,
    pub shown: Vec<ParamShow>,
}

/// A child declaration of a scope, as elaborated in a frame.
pub struct Child {
    pub decl: dvi_syntax::ast::DeclId,
    pub presence: Presence,
    /// A generate loop's block: the genvar and its values (or why they are not known).
    pub iterations: Option<R<((u32, u32), Vec<i128>)>>,
    /// The name it is shown with (`genblk2` for an unnamed generate block).
    pub name: SmolStr,
}

/// A plain `defparam` (names only), with what it is written in.
struct Plain {
    /// The definition the statement is in.
    module: (u32, u32),
    names: Vec<SmolStr>,
    value: Option<(Span, Span)>,
    unit: UnitId,
}

/// Elaboration of one design: the evaluator and what holds for every instance.
pub struct Elab<'a> {
    pub ev: Eval<'a>,
    plain: Vec<Plain>,
    /// Parameter names some `defparam` this version does not follow may set.
    others: FxHashSet<SmolStr>,
    /// Generate constructs by (unit, scope), in source order.
    constructs: FxHashMap<(UnitId, dvi_syntax::ast::DeclId), Vec<usize>>,
}

/// An instance on the way down: its definition, frame and path length.
#[derive(Clone)]
pub struct Ancestor {
    pub module: (u32, u32),
    pub frame: Arc<Frame>,
    pub depth: usize,
}

impl<'a> Elab<'a> {
    pub fn new(a: &'a Analysis) -> Self {
        let mut plain = Vec::new();
        let mut others = FxHashSet::default();
        let mut constructs: FxHashMap<(UnitId, dvi_syntax::ast::DeclId), Vec<usize>> = FxHashMap::default();
        for u in a.units.iter().flatten() {
            for (i, g) in u.parsed.generates.iter().enumerate() {
                constructs.entry((u.id, g.scope)).or_default().push(i);
            }
            for d in &u.parsed.defparams {
                let module = super::checks::definition_of_pub(&u.parsed, d.scope);
                // A defparam inside a generate block depends on the branch: not followed.
                let in_definition = module == Some(d.scope);
                match (d.plain && in_definition, module) {
                    (true, Some(m)) => plain.push(Plain { module: a.decl_key(a.canonical(DeclRef { unit: u.id, decl: m })), names: d.path.iter().map(|(n, _)| n.clone()).collect(), value: d.value, unit: u.id }),
                    _ => {
                        if let Some((n, _)) = d.path.last() {
                            others.insert(n.clone());
                        }
                    }
                }
            }
        }
        Elab { ev: Eval::new(a), plain, others, constructs }
    }

    fn a(&self) -> &'a Analysis {
        self.ev.a
    }

    /// The parameters of instance `inst` of `def` (none: a top) at `path` (its names from the
    /// top), its parent's frame `parent` and the instances above it `anc` (the top first).
    pub fn instance(&mut self, inst: Option<DeclRef>, def: DeclRef, parent: &Arc<Frame>, path: &[SmolStr], anc: &[Ancestor]) -> InstParams {
        let a = self.a();
        // The overrides: name → value tokens (None: `.P()`, the default stays).
        let mut overrides: FxHashMap<String, Option<Vec<Token>>> = FxHashMap::default();
        let mut list_unknown: Option<String> = None;
        if let Some(inst) = inst {
            if let Some((toks, _, _, Some(l))) = a.instance_lists(inst) {
                if l.macros {
                    list_unknown = Some("a macro in the parameter list".into());
                } else {
                    let header = a.param_header(def);
                    for (k, it) in l.items.iter().enumerate() {
                        match &it.kind {
                            super::instances::ItemKind::Named { name, open, .. } => {
                                let v = open.and_then(|o| (o + 1 < it.end.saturating_sub(1)).then(|| toks[o + 1..it.end - 1].to_vec()));
                                overrides.insert(name.clone(), v);
                            }
                            super::instances::ItemKind::Ordered if header.known => match header.slots.get(k).and_then(|s| s.name.clone()) {
                                Some(n) => {
                                    overrides.insert(n, Some(toks[it.body..it.end].to_vec()));
                                }
                                None => list_unknown = Some("more values than parameters".into()),
                            },
                            super::instances::ItemKind::Ordered => list_unknown = Some("the parameter list of the definition is not known".into()),
                            _ => {}
                        }
                    }
                }
            }
        }
        // The defparams that set a parameter of this instance: value evaluated where written.
        let mut set_by: FxHashMap<SmolStr, Vec<R<Val>>> = FxHashMap::default();
        let n = path.len();
        let plain: Vec<(Vec<SmolStr>, Option<(Span, Span)>, UnitId, (u32, u32))> = self.plain.iter().map(|p| (p.names.clone(), p.value, p.unit, p.module)).collect();
        for (names, value, unit, module) in plain {
            let Some((pname, prefix)) = names.split_last() else { continue };
            let mut frame: Option<Arc<Frame>> = None;
            // Relative to an instance of the module it is written in, above this one.
            for x in anc {
                if x.module == module && x.depth + prefix.len() == n && path[x.depth..] == prefix[..] {
                    frame = Some(x.frame.clone());
                }
            }
            // Or a path from a top.
            if frame.is_none() && prefix.len() == n && path == prefix {
                frame = Some(Arc::new(Frame::default()));
            }
            let Some(frame) = frame else { continue };
            let v = match value.and_then(|v| self.ev.tokens_of(v)) {
                Some(t) => self.ev.eval_tokens(&t, unit, &frame, 0),
                None => unsupported("the defparam's value"),
            };
            set_by.entry(pname.clone()).or_default().push(v);
        }
        // Each parameter in order: later ones may use earlier ones.
        let mut vals: Vec<((u32, u32), R<Val>)> = Vec::new();
        let mut shown = Vec::new();
        let u = a.unit(def.unit);
        let params: Vec<DeclRef> = u.parsed.decl(def.decl).children.iter().map(|&c| DeclRef { unit: def.unit, decl: c }).filter(|c| matches!(a.decl(*c).kind, DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam)).collect();
        for p in params {
            let d = a.decl(p);
            let key = a.decl_key(a.canonical(p));
            let frame = Arc::new(Frame { vals: vals.clone(), parent: None });
            let local = d.kind == DeclKind::LocalParam || d.has(dflags::LOCAL_PARAM);
            let typed = d.kind == DeclKind::TypeParam;
            let name = d.name.clone();
            let (v, origin) = if local {
                (self.ev.declared_value(p, &frame), Origin::Local)
            } else if let Some(vs) = set_by.get(&name) {
                let v = if vs.len() > 1 {
                    invalid("more than one defparam sets it")
                } else if typed {
                    invalid("a defparam cannot set a type parameter")
                } else {
                    vs[0].clone().and_then(|v| self.ev.declared_type(p, &frame).and_then(|ty| convert(v, &ty)))
                };
                (v, Origin::Defparam)
            } else if self.others.contains(&name) {
                (unresolved("a defparam with a path this version does not follow may set it"), Origin::Defparam)
            } else if let Some(o) = overrides.get(name.as_str()) {
                match o {
                    Some(t) if typed => (Ok(Val::Type(text_of(t))), Origin::Override),
                    Some(t) => {
                        let raw = self.ev.eval_tokens(t, inst.map_or(def.unit, |i| i.unit), parent, 0);
                        (raw.and_then(|v| self.ev.declared_type(p, &frame).and_then(|ty| convert(v, &ty))), Origin::Override)
                    }
                    None => (self.ev.declared_value(p, &frame), Origin::Default),
                }
            } else if let Some(why) = &list_unknown {
                (unresolved(why.clone()), Origin::Override)
            } else {
                (self.ev.declared_value(p, &frame), Origin::Default)
            };
            shown.push(ParamShow {
                name: name.to_string(),
                value: v.as_ref().ok().map(|v| v.text()),
                state: v.as_ref().err().map_or(State::Evaluated, |e| e.state),
                why: v.as_ref().err().map(|e| e.why.clone()),
                origin,
                kind: if typed {
                    "type"
                } else if local {
                    "localparam"
                } else {
                    "parameter"
                },
            });
            vals.push((key, v));
        }
        InstParams { frame: Arc::new(Frame { vals, parent: None }), shown }
    }

    /// The frame of generate block `block` (a loop's copy when `genvar` is given): the genvar
    /// and the block's own parameters.
    pub fn block_frame(&mut self, block: DeclRef, frame: &Arc<Frame>, genvar: Option<((u32, u32), i128)>) -> Arc<Frame> {
        let a = self.a();
        let mut f = match genvar {
            Some((k, v)) => Frame::with(frame, vec![(k, Ok(Val::int(v, 32, true)))]),
            None => frame.clone(),
        };
        let locals: Vec<DeclRef> = a.unit(block.unit).parsed.decl(block.decl).children.iter().map(|&c| DeclRef { unit: block.unit, decl: c }).filter(|c| matches!(a.decl(*c).kind, DeclKind::Parameter | DeclKind::LocalParam)).collect();
        for p in locals {
            let v = self.ev.declared_value(p, &f);
            f = Frame::with(&f, vec![(a.decl_key(a.canonical(p)), v)]);
        }
        f
    }

    /// The children of `scope` in `frame`: whether each exists, a loop block's iterations,
    /// and the names of unnamed generate blocks.
    pub fn children(&mut self, scope: DeclRef, frame: &Arc<Frame>) -> Vec<Child> {
        let a = self.a();
        let u = a.unit(scope.unit);
        let ids = self.constructs.get(&(scope.unit, scope.decl)).cloned().unwrap_or_default();
        // Each construct's branches' presence.
        let mut branches: Vec<((u32, u32), Presence)> = Vec::new();
        let mut loops: FxHashMap<dvi_syntax::ast::DeclId, usize> = FxHashMap::default();
        let mut names: FxHashMap<dvi_syntax::ast::DeclId, SmolStr> = FxHashMap::default();
        for (ordinal, &i) in ids.iter().enumerate() {
            let g = u.parsed.generates[i].clone();
            let genblk = SmolStr::new(format!("genblk{}", ordinal + 1));
            match g.kind {
                GenKind::For => {
                    if let Some(b) = g.block {
                        loops.insert(b, i);
                        if u.parsed.decl(b).has(dflags::FROM_MACRO) {
                            names.insert(b, genblk);
                        }
                    }
                }
                GenKind::If | GenKind::Case => {
                    let p = self.decide(&g, scope.unit, frame);
                    for (b, pr) in g.branches.iter().zip(p) {
                        // An unnamed `begin ... end` body: the branch's first declaration.
                        if b.decls.0 < b.decls.1 {
                            let first = b.decls.0;
                            let d = u.parsed.decl(first);
                            if d.kind == DeclKind::Generate && d.has(dflags::FROM_MACRO) && d.parent == Some(scope.decl) {
                                names.insert(first, genblk.clone());
                            }
                        }
                        branches.push(((b.decls.0, b.decls.1), pr));
                    }
                }
            }
        }
        let mut out = Vec::new();
        for &c in &u.parsed.decl(scope.decl).children {
            let mut presence = Presence::Elaborated;
            for ((lo, hi), p) in &branches {
                if *lo <= c && c < *hi {
                    match p {
                        Presence::NotElaborated => presence = Presence::NotElaborated,
                        Presence::Undetermined(w) if presence == Presence::Elaborated => presence = Presence::Undetermined(w.clone()),
                        _ => {}
                    }
                }
            }
            let iterations = loops.get(&c).map(|&i| {
                let g = u.parsed.generates[i].clone();
                self.iterations(&g, scope.unit, frame)
            });
            let name = names.remove(&c).unwrap_or_else(|| u.parsed.decl(c).name.clone());
            out.push(Child { decl: c, presence, iterations, name });
        }
        out
    }

    /// Which branch of an `if` chain or `case` is elaborated.
    fn decide(&mut self, g: &dvi_syntax::ast::GenConstruct, unit: UnitId, frame: &Arc<Frame>) -> Vec<Presence> {
        let mut out = Vec::new();
        match g.kind {
            GenKind::If => {
                // `None`: a branch before was taken; `Some(Err)`: not known.
                let mut open: Option<Result<(), String>> = Some(Ok(()));
                for b in &g.branches {
                    let p = match open.clone() {
                        None => Presence::NotElaborated,
                        Some(Err(w)) => Presence::Undetermined(w),
                        Some(Ok(())) => match b.exprs.first() {
                            None => {
                                open = None;
                                Presence::Elaborated
                            }
                            Some(&e) => {
                                let v = match self.ev.tokens_of(e) {
                                    Some(t) => self.ev.eval_tokens(&t, unit, frame, 0).and_then(|v| v.truth()),
                                    None => unsupported("a condition written through a macro"),
                                };
                                match v {
                                    Ok(true) => {
                                        open = None;
                                        Presence::Elaborated
                                    }
                                    Ok(false) => Presence::NotElaborated,
                                    Err(e) => {
                                        let w = format!("condition not evaluated: {}", e.why);
                                        open = Some(Err(w.clone()));
                                        Presence::Undetermined(w)
                                    }
                                }
                            }
                        },
                    };
                    out.push(p);
                }
            }
            GenKind::Case => {
                let value = match g.exprs.first().copied().flatten().and_then(|e| self.ev.tokens_of(e)) {
                    Some(t) => self.ev.eval_tokens(&t, unit, frame, 0),
                    None => unsupported("a case expression written through a macro"),
                };
                let value = match value {
                    Ok(v) => v,
                    Err(e) => {
                        let w = format!("case expression not evaluated: {}", e.why);
                        return g.branches.iter().map(|_| Presence::Undetermined(w.clone())).collect();
                    }
                };
                // The first item with a matching label; else the default.
                let mut chosen: Option<Result<usize, String>> = None;
                for (i, b) in g.branches.iter().enumerate() {
                    for &e in &b.exprs {
                        let m = match self.ev.tokens_of(e) {
                            Some(t) => self.ev.eval_tokens(&t, unit, frame, 0).and_then(|l| equal(&value, &l)),
                            None => unsupported("a case item written through a macro"),
                        };
                        match m {
                            Ok(true) => chosen = Some(Ok(i)),
                            Ok(false) => {}
                            Err(e) => chosen = Some(Err(format!("case item not evaluated: {}", e.why))),
                        }
                        if chosen.is_some() {
                            break;
                        }
                    }
                    if chosen.is_some() {
                        break;
                    }
                }
                let default = g.branches.iter().position(|b| b.exprs.is_empty());
                for (i, _) in g.branches.iter().enumerate() {
                    out.push(match &chosen {
                        Some(Ok(c)) if *c == i => Presence::Elaborated,
                        Some(Ok(_)) => Presence::NotElaborated,
                        Some(Err(w)) => Presence::Undetermined(w.clone()),
                        None if Some(i) == default => Presence::Elaborated,
                        None => Presence::NotElaborated,
                    });
                }
            }
            GenKind::For => {}
        }
        out
    }

    /// A generate loop's genvar and its values.
    fn iterations(&mut self, g: &dvi_syntax::ast::GenConstruct, unit: UnitId, frame: &Arc<Frame>) -> R<((u32, u32), Vec<i128>)> {
        let a = self.a();
        let mut part = |i: usize| g.exprs.get(i).copied().flatten().and_then(|e| self.ev.tokens_of(e));
        let (Some(init), Some(cond), Some(step)) = (part(0), part(1), part(2)) else { return unsupported("a loop header written through a macro or empty") };
        // `i = expr`.
        let Some(eq) = init.iter().position(|t| t.is_punct("=")) else { return unsupported("the loop's initialization") };
        let [var] = &init[..eq] else { return unsupported("the loop's initialization") };
        let Some(r) = a.resolve_at_span(unit, var.span) else { return unresolved(format!("`{}` is not resolved", var.text)) };
        let key = a.decl_key(a.canonical(r));
        let mut v = self.ev.eval_tokens(&init[eq + 1..], unit, frame, 32)?.resize(32, true)?.num().unwrap_or(0);
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        loop {
            let f = Frame::with(frame, vec![(key, Ok(Val::int(v, 32, true)))]);
            if !self.ev.eval_tokens(&cond, unit, &f, 0)?.truth()? {
                break;
            }
            if !seen.insert(v) {
                return invalid(format!("the loop repeats {v}"));
            }
            out.push(v);
            if out.len() > MAX_ITERATIONS {
                return incomplete(format!("more than {MAX_ITERATIONS} iterations"));
            }
            v = self.step(&step, var, v, unit, &f)?;
        }
        Ok((key, out))
    }

    /// The genvar's next value after `step` (`i++`, `i = i + 2`, `i -= 1`...).
    fn step(&mut self, step: &[Token], var: &Token, cur: i128, unit: UnitId, f: &Arc<Frame>) -> R<i128> {
        let names_var = |t: &Token| t.kind == TokKind::Ident && t.text == var.text;
        match step {
            [x, op] | [op, x] if names_var(x) && (op.is_punct("++") || op.is_punct("--")) => Ok(if op.is_punct("++") { cur + 1 } else { cur - 1 }),
            [x, op, rest @ ..] if names_var(x) && !rest.is_empty() => {
                let r = self.ev.eval_tokens(rest, unit, f, 32)?.resize(32, true)?.num().unwrap_or(0);
                let v = match op.text.as_str() {
                    "=" => r,
                    "+=" => cur + r,
                    "-=" => cur - r,
                    "*=" => cur * r,
                    "/=" if r != 0 => cur / r,
                    "<<=" => cur << r.clamp(0, 31),
                    ">>=" => cur >> r.clamp(0, 31),
                    _ => return unsupported("the loop's step"),
                };
                Ok(Val::int(v, 32, true).num().unwrap_or(0))
            }
            _ => unsupported("the loop's step"),
        }
    }

    /// An instance array's range (`u[0:3]`), when the instance has one.
    pub fn array_range(&mut self, inst: DeclRef, frame: &Arc<Frame>) -> Option<R<(i128, i128)>> {
        let a = self.a();
        let d = a.decl(inst);
        let toks = self.ev.file_tokens(d.name_span.file)?;
        let k = toks.partition_point(|t| t.span.start < d.name_span.start);
        if !toks.get(k + 1).is_some_and(|t| t.is_punct("[")) {
            return None;
        }
        let close = matching(&toks, k + 1)?;
        if toks.get(close + 1).is_some_and(|t| t.is_punct("[")) {
            return Some(unsupported("an array of instances with more than one dimension"));
        }
        let inner: Vec<Token> = toks[k + 2..close].to_vec();
        Some(self.ev.range_bounds(&inner, inst.unit, frame))
    }
}

/// Case equality of two evaluated values.
fn equal(a: &Val, b: &Val) -> R<bool> {
    match (a, b) {
        (Val::Str(x), Val::Str(y)) => Ok(x == y),
        (Val::Int { .. }, Val::Int { .. }) => {
            let w = a.width().max(b.width());
            let s = a.signed() && b.signed();
            Ok(a.resize(w, s)?.num() == b.resize(w, s)?.num())
        }
        _ => unsupported("a comparison of a string and a number"),
    }
}

/// A parameter value as a hover says it: "16", or "unresolved (why)".
pub fn shown_value(p: &ParamShow) -> String {
    match (&p.value, &p.why) {
        (Some(v), _) => v.clone(),
        (None, Some(w)) => format!("{} ({w})", state_name(p.state)),
        _ => state_name(p.state).to_string(),
    }
}

pub fn state_name(s: State) -> &'static str {
    match s {
        State::Evaluated => "evaluated",
        State::Unresolved => "unresolved",
        State::Unsupported => "not evaluated",
        State::Invalid => "invalid",
        State::Incomplete => "incomplete",
    }
}

// ------------------------------------------------------------------ index for hover

/// Instances indexed at most.
const MAX_INDEXED: usize = 200_000;
/// Instance paths a hover lists at most.
const MAX_LISTED: usize = 20;

/// An instance of a definition in the elaborated design.
pub struct IndexedInst {
    pub path: String,
    pub frame: Arc<Frame>,
    /// Every generate branch above it is proven elaborated.
    pub certain: bool,
}

/// The elaborated instances of every definition, from the design's tops.
pub struct ElabIndex {
    pub by_def: FxHashMap<(u32, u32), Vec<IndexedInst>>,
    /// Every instance was reached (no limit, nothing left undetermined above a loop).
    pub complete: bool,
    pub tops: Vec<String>,
}

struct IndexWalk<'e, 'a> {
    el: &'e mut Elab<'a>,
    index: ElabIndex,
    count: usize,
    stack: Vec<((u32, u32), u64)>,
    anc: Vec<Ancestor>,
    names: Vec<SmolStr>,
}

impl IndexWalk<'_, '_> {
    fn walk(&mut self, scope: DeclRef, frame: &Arc<Frame>, path: &str, certain: bool) {
        let a = self.el.a();
        if a.cancelled() || self.count > MAX_INDEXED {
            self.index.complete = false;
            return;
        }
        let u = a.unit(scope.unit);
        for child in self.el.children(scope, frame) {
            let d = u.parsed.decl(child.decl);
            if child.presence == Presence::NotElaborated {
                continue;
            }
            let sure = certain && child.presence == Presence::Elaborated;
            match d.kind {
                DeclKind::Instance if !d.has(dflags::BOUND) => {
                    let cr = DeclRef { unit: scope.unit, decl: child.decl };
                    let Some(m) = a.type_of(cr).filter(|m| a.decl(*m).kind.is_definition()) else { continue };
                    self.count += 1;
                    let child_path = format!("{path}.{}", d.name);
                    self.names.push(d.name.clone());
                    let ip = self.el.instance(Some(cr), m, frame, &self.names, &self.anc);
                    let key = a.decl_key(a.canonical(m));
                    let entry = (key, hash_shown(&ip.shown));
                    self.index.by_def.entry(key).or_default().push(IndexedInst { path: child_path.clone(), frame: ip.frame.clone(), certain: sure });
                    if !self.stack.contains(&entry) && self.stack.len() < 64 {
                        self.stack.push(entry);
                        self.anc.push(Ancestor { module: key, frame: ip.frame.clone(), depth: self.names.len() });
                        self.walk(m, &ip.frame, &child_path, sure);
                        self.anc.pop();
                        self.stack.pop();
                    }
                    self.names.pop();
                }
                DeclKind::Generate | DeclKind::Block => {
                    let gr = DeclRef { unit: scope.unit, decl: child.decl };
                    let named = d.kind == DeclKind::Generate;
                    match &child.iterations {
                        Some(Ok((key, values))) => {
                            for v in values.clone() {
                                let f = self.el.block_frame(gr, frame, Some((*key, v)));
                                let p = if named { format!("{path}.{}[{v}]", child.name) } else { path.to_string() };
                                self.walk(gr, &f, &p, sure);
                            }
                        }
                        Some(Err(_)) => {
                            self.index.complete = false;
                            let f = self.el.block_frame(gr, frame, None);
                            let p = if named { format!("{path}.{}", child.name) } else { path.to_string() };
                            self.walk(gr, &f, &p, false);
                        }
                        None => {
                            let f = self.el.block_frame(gr, frame, None);
                            let p = if named { format!("{path}.{}", child.name) } else { path.to_string() };
                            if named {
                                self.names.push(child.name.clone());
                            }
                            self.walk(gr, &f, &p, sure);
                            if named {
                                self.names.pop();
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn hash_shown(p: &[ParamShow]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    for x in p {
        x.name.hash(&mut h);
        x.value.hash(&mut h);
        x.why.hash(&mut h);
    }
    h.finish()
}

impl Analysis {
    /// The elaborated instances of the design (made once per analysis revision; a cancelled
    /// build is not kept).
    pub fn elab_index(&self) -> Option<Arc<ElabIndex>> {
        if let Some(i) = self.elab_index.lock().unwrap().as_ref() {
            return Some(i.clone());
        }
        let mut el = Elab::new(self);
        let tops = self.design_tops();
        let mut w = IndexWalk { el: &mut el, index: ElabIndex { by_def: FxHashMap::default(), complete: true, tops: Vec::new() }, count: 0, stack: Vec::new(), anc: Vec::new(), names: Vec::new() };
        for t in tops {
            let name = self.decl(t).name.clone();
            w.index.tops.push(name.to_string());
            w.names = vec![name.clone()];
            let ip = w.el.instance(None, t, &Arc::new(Frame::default()), &w.names, &[]);
            let key = self.decl_key(self.canonical(t));
            w.index.by_def.entry(key).or_default().push(IndexedInst { path: name.to_string(), frame: ip.frame.clone(), certain: true });
            w.stack = vec![(key, hash_shown(&ip.shown))];
            w.anc = vec![Ancestor { module: key, frame: ip.frame.clone(), depth: 1 }];
            w.walk(t, &ip.frame, &name, true);
        }
        if self.cancelled() {
            return None;
        }
        let index = Arc::new(w.index);
        *self.elab_index.lock().unwrap() = Some(index.clone());
        Some(index)
    }

    /// Hover lines on a parameter or a signal of a design element: its values (a signal: its
    /// width) in the elaborated instances, grouped.
    pub fn elab_hover(&self, r: DeclRef) -> Option<String> {
        let d = self.decl(r);
        let parent = d.parent?;
        let u = self.unit(r.unit);
        if !u.parsed.decl(parent).kind.is_definition() {
            return None;
        }
        let param = matches!(d.kind, DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam);
        let signal = matches!(d.kind, DeclKind::Variable | DeclKind::Net | DeclKind::Port);
        if !param && !signal {
            return None;
        }
        let mut el = Elab::new(self);
        if signal {
            // Only when the width depends on something: a name in its range.
            let toks = el.ev.file_tokens(d.name_span.file)?;
            let name = toks.partition_point(|t| t.span.start < d.name_span.start);
            let start = toks.partition_point(|t| t.span.start < d.span.start).min(name);
            let ranged = toks[start..name].iter().any(|t| t.is_punct("["));
            let named = toks[start..name].iter().skip_while(|t| !t.is_punct("[")).any(|t| t.kind == TokKind::Ident || t.kind == TokKind::Directive);
            if !(ranged && named) {
                return None;
            }
        }
        let index = self.elab_index()?;
        let def_key = self.decl_key(self.canonical(DeclRef { unit: r.unit, decl: parent }));
        let insts = index.by_def.get(&def_key)?;
        let key = self.decl_key(self.canonical(r));
        // Value (or why not) → the instances with it.
        let mut groups: Vec<(String, Vec<(&str, bool)>)> = Vec::new();
        for i in insts {
            let v = if param {
                match i.frame.get(key) {
                    Some(Ok(v)) => v.text(),
                    Some(Err(e)) => format!("{} ({})", state_name(e.state), e.why),
                    None => "unresolved".to_string(),
                }
            } else {
                match el.ev.signal_width(r, &i.frame) {
                    Ok(w) => format!("{w} bits"),
                    Err(e) => format!("{} ({})", state_name(e.state), e.why),
                }
            };
            match groups.iter_mut().find(|g| g.0 == v) {
                Some(g) => g.1.push((&i.path, i.certain)),
                None => groups.push((v, vec![(&i.path, i.certain)])),
            }
        }
        let title = if param { "Values in the design hierarchy" } else { "Width in the design hierarchy" };
        let mut md = format!("**{title}** ({} instance{} from {})", insts.len(), if insts.len() == 1 { "" } else { "s" }, index.tops.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", "));
        let mut listed = 0;
        for (v, paths) in &groups {
            let shown: Vec<String> = paths
                .iter()
                .take(MAX_LISTED.saturating_sub(listed).max(1))
                .map(|(p, sure)| if *sure { format!("`{p}`") } else { format!("`{p}` (if it exists)") })
                .collect();
            listed += shown.len();
            let more = if paths.len() > shown.len() { format!(", and {} more", paths.len() - shown.len()) } else { String::new() };
            md.push_str(&format!("\n- `{v}` in {}{more}", shown.join(", ")));
        }
        if !index.complete {
            md.push_str("\n\n*The design was not elaborated to its end (a limit, or a loop not evaluated): other instances may exist.*");
        }
        Some(md)
    }
}

/// A user type's name, for messages.
struct UserName<'t> {
    text: &'t str,
}
