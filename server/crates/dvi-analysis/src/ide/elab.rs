//! Parameter elaboration: the parameter values each instance gets, and what they decide (the
//! generate branches that exist, the copies of generate loops, the ranges of instance arrays).
//!
//! Nothing uncertain is given as certain: a value is evaluated, or says why not (a missing
//! binding, code the evaluator does not cover, an error, a budget); a generate branch is
//! elaborated, not elaborated (its condition is proven false) or undetermined.
//!
//! Values are two-state integers of at most 64 bits with their width and signedness, strings,
//! and type parameters as text. Expressions are typed first, then evaluated once with the type
//! propagated down to their context-determined operands, as SystemVerilog sizes them
//! (IEEE 1800 §11.6–11.8); a value is converted to its declared type as an assignment is.

use crate::world::{Analysis, DeclRef, UnitId};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, GenKind};
use dvi_syntax::{FileId, Span, TokKind, Token};
use rustc_hash::{FxHashMap, FxHashSet};
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
/// Expression nodes one request evaluates at most.
const MAX_NODES: usize = 2_000_000;
/// Macro expansions in one expression.
const MAX_EXPANSIONS: usize = 256;

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

    fn signed(&self) -> bool {
        matches!(self, Val::Int { signed: true, .. })
    }

    /// The value extended or cut to `width` bits, then typed `signed`: extended with its sign
    /// only when it is signed and `signed` (the type propagated to it is).
    fn to(&self, width: u32, signed: bool) -> R<Val> {
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

/// An integral type: its width, signedness, and whether it is four-state (its variables start
/// as X).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntType {
    pub width: u32,
    pub signed: bool,
    pub four: bool,
}

/// A declared type as far as values need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Int(IntType),
    Str,
    /// No type written: the value keeps its own.
    Implicit,
    /// Only `signed` or `unsigned` written: the value keeps its width.
    Sign(bool),
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
    /// A variable of a function being run, by its key.
    Key((u32, u32)),
    /// A macro that could not be expanded.
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
    /// A size, or a user type's name.
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

    fn deeper(&mut self) -> R<()> {
        self.nest += 1;
        if self.nest > MAX_NEST {
            return incomplete("an expression nested too deeply");
        }
        Ok(())
    }

    fn expr(&mut self) -> R<Ex> {
        self.deeper()?;
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
            // Every binary operator is left-associative (`**` too, IEEE 1800 §11.3.2).
            self.deeper()?;
            let r = self.binary(prec + 1)?;
            self.nest -= 1;
            l = Ex::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn unary(&mut self) -> R<Ex> {
        const UNARY: &[&str] = &["+", "-", "!", "~", "&", "~&", "|", "~|", "^", "~^", "^~"];
        if let Some(op) = self.peek().filter(|t| t.kind == TokKind::Punct).and_then(|t| UNARY.iter().find(|o| t.text == **o)) {
            self.k += 1;
            self.deeper()?;
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
                let args = if name == "$bits" {
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
                    return unsupported("a member select or hierarchical name");
                }
                if self.eat("(") {
                    let args = self.args()?;
                    return Ok(Ex::Call(last.text.clone(), last.span, args));
                }
                Ok(Ex::Name(last.text.clone(), last.span))
            }
            TokKind::Keyword => {
                if matches!(t.text.as_str(), "signed" | "unsigned") && self.eat("'") {
                    self.expect("(")?;
                    let v = self.expr()?;
                    self.expect(")")?;
                    return Ok(Ex::Cast(Box::new(CastTo::Signed(t.text == "signed")), Box::new(v)));
                }
                if let (Some(ty), true) = (keyword_type(&t.text), self.at("'")) {
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

/// The type of an integral type keyword.
fn keyword_type(k: &str) -> Option<IntType> {
    let (width, signed, four) = match k {
        "bit" => (1, false, false),
        "logic" | "reg" => (1, false, true),
        "byte" => (8, true, false),
        "shortint" => (16, true, false),
        "int" => (32, true, false),
        "integer" => (32, true, true),
        "longint" => (64, true, false),
        "time" => (64, false, true),
        _ => return None,
    };
    Some(IntType { width, signed, four })
}

/// An integer literal: `4`, `1_000`, `'d4`, `8'hFF`, `4'sb0101`, `'1`.
fn literal(s: &str) -> R<Ex> {
    let s: String = s.chars().filter(|c| *c != '_').collect();
    if s == "'0" || s == "'1" {
        return Ok(Ex::Fill(s == "'1"));
    }
    if s.contains('.') || (s.contains(['e', 'E']) && !s.contains('\'')) {
        return unsupported("a real number");
    }
    let bad = || Unknown { state: State::Unsupported, why: format!("the number {s}") };
    let Some(q) = s.find('\'') else {
        let v: u64 = s.parse().map_err(|_| bad())?;
        // An unsized decimal number is a signed 32-bit integer; a larger one keeps its width.
        let w = if v > i32::MAX as u64 { (64 - v.leading_zeros()).max(32) } else { 32 };
        return Ok(Ex::Num(Val::int(v as i128, w, true)));
    };
    let size: Option<u32> = if q == 0 { None } else { Some(s[..q].trim().parse().map_err(|_| bad())?) };
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
        _ => return Err(bad()),
    };
    let digits = digits.trim();
    if digits.chars().any(|c| matches!(c, 'x' | 'X' | 'z' | 'Z' | '?')) {
        return unsupported("an x or z value");
    }
    let width = size.unwrap_or(32);
    if width == 0 || width > MAX_WIDTH {
        return unsupported(format!("a {width}-bit value"));
    }
    // More digits than the size: cut (as a simulator does, with a warning).
    let v = u128::from_str_radix(digits, radix).map_err(|_| bad())?;
    let v = (v & mask(width) as u128) as u64;
    Ok(Ex::Num(Val::Int { bits: v, width, signed }))
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
    /// `x = e` (the value is the whole right side; compound operators are made into it).
    Assign((u32, u32), Ex),
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

/// A type: width and signedness, or a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct T {
    w: u32,
    s: bool,
    string: bool,
}

const STRING: T = T { w: 0, s: false, string: true };

/// The evaluator: one per request, with its memos.
pub struct Eval<'a> {
    pub a: &'a Analysis,
    tokens: FxHashMap<FileId, Option<Arc<Vec<Token>>>>,
    /// Package and compilation-unit constants by declaration.
    constants: FxHashMap<(u32, u32), R<Val>>,
    /// Constants being evaluated (a cycle is invalid).
    evaluating: Vec<(u32, u32)>,
    funcs: FxHashMap<(u32, u32), Option<Arc<Func>>>,
    /// Leaf values of the expression being evaluated (names, calls), by node address.
    leaves: FxHashMap<usize, R<Val>>,
    /// The types of the running functions' variables.
    var_types: Vec<FxHashMap<(u32, u32), Ty>>,
    steps: usize,
    nodes: usize,
    nest: usize,
}

impl<'a> Eval<'a> {
    pub fn new(a: &'a Analysis) -> Self {
        Eval { a, tokens: FxHashMap::default(), constants: FxHashMap::default(), evaluating: Vec::new(), funcs: FxHashMap::default(), leaves: FxHashMap::default(), var_types: Vec::new(), steps: 0, nodes: 0, nest: 0 }
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

    /// `t` with its object-like macros replaced by their bodies (as the preprocessor does),
    /// when every definition of each gives the same body.
    fn expand(&self, t: &[Token]) -> Vec<Token> {
        if !t.iter().any(|x| x.kind == TokKind::Directive) {
            return t.to_vec();
        }
        let mut out = Vec::with_capacity(t.len());
        let mut todo: Vec<Token> = t.iter().rev().cloned().collect();
        let mut n = 0;
        while let Some(x) = todo.pop() {
            if x.kind == TokKind::Directive && n < MAX_EXPANSIONS && !todo.last().is_some_and(|y| y.is_punct("(")) {
                if let Some(defs) = self.a.global.macros.get(&x.text) {
                    let first = defs.first();
                    let same = first.is_some_and(|f| f.params.is_none() && defs.iter().all(|d| d.params.is_none() && d.body.len() == f.body.len() && d.body.iter().zip(&f.body).all(|(p, q)| p.text == q.text && p.kind == q.kind)));
                    if let (true, Some(f)) = (same, first) {
                        n += 1;
                        todo.extend(f.body.iter().rev().cloned());
                        continue;
                    }
                }
            }
            out.push(x);
        }
        out
    }

    /// The self-determined value of the expression `t` (written in `unit`) in `frame`.
    pub fn eval_tokens(&mut self, t: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<Val> {
        let t = self.expand(t);
        let x = parse(&t)?;
        self.value(&x, unit, frame)
    }

    /// `t`'s value assigned to a variable of type `ty`.
    pub fn eval_as(&mut self, t: &[Token], unit: UnitId, frame: &Arc<Frame>, ty: &Ty) -> R<Val> {
        let t = self.expand(t);
        let x = parse(&t)?;
        self.assign(&x, unit, frame, ty)
    }

    fn enter(&mut self) -> R<()> {
        self.nest += 1;
        self.nodes += 1;
        if self.nest > MAX_NEST || self.nodes > MAX_NODES {
            self.nest -= 1;
            return incomplete(if self.nest >= MAX_NEST { "expressions or calls nested too deeply" } else { "too much to evaluate" });
        }
        if self.nodes % 1024 == 0 && self.a.cancelled() {
            self.nest -= 1;
            return incomplete("cancelled");
        }
        Ok(())
    }

    /// A self-determined value.
    fn value(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>) -> R<Val> {
        let saved = std::mem::take(&mut self.leaves);
        let r = (|| {
            let t = self.ty(x, unit, frame)?;
            self.eval(x, unit, frame, t)
        })();
        self.leaves = saved;
        r
    }

    /// `x` assigned to a variable of type `ty` (IEEE 1800 §10.7): evaluated in the width of the
    /// wider of both, with its own signedness, then cut to the variable's type.
    fn assign(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>, ty: &Ty) -> R<Val> {
        let saved = std::mem::take(&mut self.leaves);
        let r = (|| {
            let t = self.ty(x, unit, frame)?;
            match ty {
                Ty::Implicit => self.eval(x, unit, frame, t),
                Ty::Str => {
                    if !t.string {
                        return unsupported("a number given to a string");
                    }
                    self.eval(x, unit, frame, t)
                }
                Ty::Sign(s) => {
                    if t.string {
                        return unsupported("a string given to a number");
                    }
                    let v = self.eval(x, unit, frame, t)?;
                    Ok(retype(v, *s))
                }
                Ty::Int(i) => {
                    if t.string {
                        return unsupported("a string given to a number");
                    }
                    if t.w > MAX_WIDTH {
                        return unsupported(format!("a {}-bit value", t.w));
                    }
                    let v = self.eval(x, unit, frame, T { w: t.w.max(i.width), s: t.s, string: false })?;
                    Ok(retype(v.to(i.width, t.s)?, i.signed))
                }
            }
        })();
        self.leaves = saved;
        r
    }

    /// A leaf's value (a name, a call, a macro, a system function), once per expression.
    fn leaf(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>) -> R<Val> {
        let key = x as *const Ex as usize;
        if let Some(v) = self.leaves.get(&key) {
            return v.clone();
        }
        let v = match x {
            Ex::Name(n, sp) => self.name(n, *sp, unit, frame),
            Ex::Key(k) => frame.get(*k).cloned().unwrap_or_else(|| unresolved("a variable without a value")),
            Ex::Macro(m) => {
                if self.a.global.macros.contains_key(m) {
                    unsupported(format!("the macro `{m}` (several definitions or arguments)"))
                } else {
                    unresolved(format!("the macro `{m}` is not defined"))
                }
            }
            Ex::Call(n, sp, args) => {
                let saved = std::mem::take(&mut self.leaves);
                let r = self.call(n, *sp, args, unit, frame);
                self.leaves = saved;
                r
            }
            Ex::Sys(n, args, raw) => {
                let saved = std::mem::take(&mut self.leaves);
                let r = self.system(n, args, raw, unit, frame);
                self.leaves = saved;
                r
            }
            _ => unsupported("an expression"),
        };
        self.leaves.insert(key, v.clone());
        v
    }

    /// The self-determined type of `x` (IEEE 1800 §11.6.1, §11.8.1).
    fn ty(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>) -> R<T> {
        self.enter()?;
        let r = self.ty_inner(x, unit, frame);
        self.nest -= 1;
        r
    }

    fn ty_inner(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>) -> R<T> {
        let of = |v: &Val| match v {
            Val::Int { width, signed, .. } => Ok(T { w: *width, s: *signed, string: false }),
            Val::Str(_) => Ok(STRING),
            Val::Type(_) => unsupported("a type in an expression"),
        };
        Ok(match x {
            Ex::Num(v) => of(v)?,
            Ex::Fill(_) => T { w: 1, s: false, string: false },
            Ex::Str(_) => STRING,
            Ex::Name(..) | Ex::Key(_) | Ex::Macro(_) | Ex::Call(..) | Ex::Sys(..) => {
                let v = self.leaf(x, unit, frame)?;
                of(&v)?
            }
            Ex::Unary(op, v) => match *op {
                "+" | "-" | "~" => self.ty(v, unit, frame)?,
                _ => T { w: 1, s: false, string: false },
            },
            Ex::Bin(op, l, r) => match *op {
                "==" | "!=" | "===" | "!==" | "<" | "<=" | ">" | ">=" | "&&" | "||" => T { w: 1, s: false, string: false },
                "<<" | ">>" | "<<<" | ">>>" | "**" => self.ty(l, unit, frame)?,
                _ => {
                    let (a, b) = (self.ty(l, unit, frame)?, self.ty(r, unit, frame)?);
                    if a.string || b.string {
                        return unsupported("a string in an arithmetic expression");
                    }
                    T { w: a.w.max(b.w), s: a.s && b.s, string: false }
                }
            },
            Ex::Cond(_, a, b) => {
                let (a, b) = (self.ty(a, unit, frame)?, self.ty(b, unit, frame)?);
                if a.string && b.string {
                    STRING
                } else if a.string || b.string {
                    return unsupported("a string and a number in one condition");
                } else {
                    T { w: a.w.max(b.w), s: a.s && b.s, string: false }
                }
            }
            Ex::Concat(items) => {
                let mut w = 0u32;
                let mut strings = 0;
                for it in items {
                    let t = self.ty(it, unit, frame)?;
                    if t.string {
                        strings += 1;
                    }
                    w = w.saturating_add(t.w);
                }
                if strings == items.len() && !items.is_empty() {
                    STRING
                } else if strings > 0 {
                    return unsupported("a concatenation of strings and numbers");
                } else {
                    T { w, s: false, string: false }
                }
            }
            Ex::Repl(n, items) => {
                let n = self.value(n, unit, frame)?.num().unwrap_or(-1);
                if !(1..=MAX_WIDTH as i128).contains(&n) {
                    return unsupported(format!("a replication {n} times"));
                }
                let one = self.ty(&Ex::Concat(items.clone()), unit, frame)?;
                if one.string {
                    return unsupported("a replication of strings");
                }
                T { w: one.w.saturating_mul(n as u32), s: false, string: false }
            }
            Ex::Select(_, kind, a, b) => {
                let w = match (*kind, b) {
                    ("", _) => 1,
                    (":", Some(b)) => {
                        let i = self.value(a, unit, frame)?.num().unwrap_or(0);
                        let j = self.value(b, unit, frame)?.num().unwrap_or(0);
                        ((i - j).abs() + 1).min(u32::MAX as i128) as u32
                    }
                    (_, Some(b)) => self.value(b, unit, frame)?.num().unwrap_or(0).clamp(0, u32::MAX as i128) as u32,
                    _ => return unsupported("a select"),
                };
                T { w, s: false, string: false }
            }
            Ex::Cast(to, v) => match &**to {
                CastTo::Signed(s) => T { w: self.ty(v, unit, frame)?.w, s: *s, string: false },
                CastTo::Type(t) => T { w: t.width, s: t.signed, string: false },
                CastTo::Size(n) => match self.user_cast(n, unit, frame)? {
                    Some(t) => T { w: t.width, s: t.signed, string: false },
                    None => {
                        let n = self.value(n, unit, frame)?.num().unwrap_or(0);
                        if n <= 0 || n > MAX_WIDTH as i128 {
                            return unsupported(format!("a cast to {n} bits"));
                        }
                        T { w: n as u32, s: self.ty(v, unit, frame)?.s, string: false }
                    }
                },
            },
        })
    }

    /// `T'(...)` where `T` names a user type: that type.
    fn user_cast(&mut self, n: &Ex, unit: UnitId, frame: &Arc<Frame>) -> R<Option<IntType>> {
        if let Ex::Name(name, sp) = n {
            if self.a.resolve_at_span(unit, *sp).is_some_and(|r| self.a.decl(r).kind == DeclKind::Typedef) {
                return self.user_type(name, *sp, unit, frame).map(Some);
            }
        }
        Ok(None)
    }

    /// The value of `x` in a context of type `c` (its operands' type propagated down).
    fn eval(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>, c: T) -> R<Val> {
        self.enter()?;
        let r = self.eval_inner(x, unit, frame, c);
        self.nest -= 1;
        r
    }

    fn eval_inner(&mut self, x: &Ex, unit: UnitId, frame: &Arc<Frame>, c: T) -> R<Val> {
        if c.w > MAX_WIDTH {
            return unsupported(format!("a {}-bit value", c.w));
        }
        // A self-determined result widened to the context.
        let fit = |v: Val| -> R<Val> {
            match v {
                Val::Str(_) if c.string => Ok(v),
                Val::Int { .. } if !c.string => v.to(c.w, c.s),
                Val::Str(_) => unsupported("a string in an arithmetic expression"),
                _ => unsupported("a number used as a string"),
            }
        };
        match x {
            Ex::Num(v) => fit(v.clone()),
            Ex::Fill(one) => Ok(Val::Int { bits: if *one { mask(c.w.max(1)) } else { 0 }, width: c.w.max(1), signed: c.s }),
            Ex::Str(s) => fit(Val::Str(s.clone())),
            Ex::Name(..) | Ex::Key(_) | Ex::Macro(_) | Ex::Call(..) | Ex::Sys(..) => {
                let v = self.leaf(x, unit, frame)?;
                fit(v)
            }
            Ex::Unary(op, v) => match *op {
                "+" | "-" | "~" => {
                    let a = self.eval(v, unit, frame, c)?.num().unwrap_or(0);
                    Ok(match *op {
                        "-" => Val::int(a.wrapping_neg(), c.w, c.s),
                        "~" => Val::int(!a, c.w, c.s),
                        _ => Val::int(a, c.w, c.s),
                    })
                }
                op => {
                    let t = self.ty(v, unit, frame)?;
                    let a = self.eval(v, unit, frame, t)?;
                    let r = match op {
                        "!" => !a.truth()?,
                        _ => {
                            let Val::Int { bits, width, .. } = a else { return unsupported("a reduction of a string") };
                            let ones = (bits & mask(width)).count_ones();
                            let all = bits & mask(width) == mask(width);
                            match op {
                                "&" => all,
                                "~&" => !all,
                                "|" => bits != 0,
                                "~|" => bits == 0,
                                "^" => ones % 2 == 1,
                                _ => ones % 2 == 0,
                            }
                        }
                    };
                    fit(Val::int(r as i128, 1, false))
                }
            },
            Ex::Bin(op, l, r) => self.binary(op, l, r, unit, frame, c),
            Ex::Cond(cond, a, b) => {
                let t = self.ty(cond, unit, frame)?;
                if self.eval(cond, unit, frame, t)?.truth()? {
                    self.eval(a, unit, frame, c)
                } else {
                    self.eval(b, unit, frame, c)
                }
            }
            Ex::Concat(items) => {
                let mut bits: u128 = 0;
                let mut width = 0u32;
                let mut strings = String::new();
                for it in items {
                    let t = self.ty(it, unit, frame)?;
                    match self.eval(it, unit, frame, t)? {
                        Val::Str(s) => strings.push_str(&s),
                        Val::Int { bits: b, width: w, .. } => {
                            width += w;
                            if width > MAX_WIDTH {
                                return unsupported(format!("a {width}-bit value"));
                            }
                            bits = (bits << w) | b as u128;
                        }
                        Val::Type(_) => return unsupported("a type in a concatenation"),
                    }
                }
                if c.string {
                    return Ok(Val::Str(strings));
                }
                fit(Val::Int { bits: bits as u64, width, signed: false })
            }
            Ex::Repl(n, items) => {
                let n = self.value(n, unit, frame)?.num().unwrap_or(-1);
                let t = self.ty(&Ex::Concat(items.clone()), unit, frame)?;
                let one = self.eval(&Ex::Concat(items.clone()), unit, frame, t)?;
                let Val::Int { bits, width, .. } = one else { return unsupported("a replication of strings") };
                let total = width.saturating_mul(n.clamp(0, MAX_WIDTH as i128) as u32);
                if total > MAX_WIDTH || total == 0 || n <= 0 {
                    return unsupported(format!("a {total}-bit value"));
                }
                let mut out: u128 = 0;
                for _ in 0..n {
                    out = (out << width) | bits as u128;
                }
                fit(Val::Int { bits: out as u64, width: total, signed: false })
            }
            Ex::Select(base, kind, a, b) => {
                // Only a value whose bits are numbered from 0 with one dimension: other layouts
                // would be read wrong.
                if !self.plain_bits(base, unit) {
                    return unsupported("a select of a name declared with another range or several dimensions");
                }
                let tb = self.ty(base, unit, frame)?;
                let v = self.eval(base, unit, frame, tb)?;
                let Val::Int { bits, width, .. } = v else { return unsupported("a select of a string") };
                let i = self.value(a, unit, frame)?.num().unwrap_or(-1);
                let (lo, w) = match (*kind, b) {
                    ("", _) => (i, 1),
                    (":", Some(b)) => {
                        let j = self.value(b, unit, frame)?.num().unwrap_or(-1);
                        (i.min(j), (i - j).abs() + 1)
                    }
                    ("+:", Some(b)) => (i, self.value(b, unit, frame)?.num().unwrap_or(0)),
                    ("-:", Some(b)) => {
                        let w = self.value(b, unit, frame)?.num().unwrap_or(0);
                        (i - w + 1, w)
                    }
                    _ => return unsupported("a select"),
                };
                if lo < 0 || w <= 0 || lo + w > width as i128 {
                    return unsupported("a select outside the value");
                }
                fit(Val::int(((bits >> lo) & mask(w as u32)) as i128, w as u32, false))
            }
            Ex::Cast(to, v) => {
                let r = match &**to {
                    CastTo::Signed(s) => {
                        let t = self.ty(v, unit, frame)?;
                        retype(self.eval(v, unit, frame, t)?, *s)
                    }
                    CastTo::Type(t) => self.assign(v, unit, frame, &Ty::Int(*t))?,
                    CastTo::Size(n) => match self.user_cast(n, unit, frame)? {
                        Some(t) => self.assign(v, unit, frame, &Ty::Int(t))?,
                        None => {
                            let n = self.value(n, unit, frame)?.num().unwrap_or(0) as u32;
                            let t = self.ty(v, unit, frame)?;
                            let a = self.eval(v, unit, frame, T { w: t.w.max(n), s: t.s, string: false })?;
                            a.to(n, t.s)?
                        }
                    },
                };
                fit(r)
            }
        }
    }

    /// Whether a select of `x` can be read from its value's bits: a genvar, or a parameter
    /// with no type, an integer keyword, or one range ending at 0.
    fn plain_bits(&mut self, x: &Ex, unit: UnitId) -> bool {
        let Ex::Name(_, sp) = x else { return matches!(x, Ex::Num(_) | Ex::Concat(_) | Ex::Repl(..)) };
        let Some(r) = self.a.resolve_at_span(unit, *sp) else { return false };
        match self.a.decl(r).kind {
            DeclKind::Genvar => true,
            DeclKind::Parameter | DeclKind::LocalParam => {
                let Some(t) = self.type_tokens(r) else { return false };
                let dims = t.iter().filter(|x| x.is_punct("[")).count();
                let user = t.iter().any(|x| x.kind == TokKind::Ident);
                !user && (dims == 0 || (dims == 1 && t.iter().rev().nth(1).is_some_and(|x| x.text == "0") && t.iter().rev().nth(2).is_some_and(|x| x.is_punct(":"))))
            }
            _ => false,
        }
    }

    fn binary(&mut self, op: &'static str, l: &Ex, r: &Ex, unit: UnitId, frame: &Arc<Frame>, c: T) -> R<Val> {
        let fit = |v: Val| v.to(c.w, c.s);
        match op {
            "&&" | "||" => {
                let tl = self.ty(l, unit, frame)?;
                let a = self.eval(l, unit, frame, tl)?.truth()?;
                // Short-circuit: the other side may be unknown.
                if (op == "&&" && !a) || (op == "||" && a) {
                    return fit(Val::int(a as i128, 1, false));
                }
                let tr = self.ty(r, unit, frame)?;
                let b = self.eval(r, unit, frame, tr)?.truth()?;
                fit(Val::int(b as i128, 1, false))
            }
            "==" | "!=" | "===" | "!==" | "<" | "<=" | ">" | ">=" => {
                let (tl, tr) = (self.ty(l, unit, frame)?, self.ty(r, unit, frame)?);
                let t = if tl.string && tr.string {
                    STRING
                } else if tl.string || tr.string {
                    return unsupported("a comparison of a string and a number");
                } else {
                    T { w: tl.w.max(tr.w), s: tl.s && tr.s, string: false }
                };
                let (va, vb) = (self.eval(l, unit, frame, t)?, self.eval(r, unit, frame, t)?);
                let o = match (&va, &vb) {
                    (Val::Str(a), Val::Str(b)) => a.cmp(b),
                    _ => va.num().unwrap_or(0).cmp(&vb.num().unwrap_or(0)),
                };
                let truth = match op {
                    "==" | "===" => o.is_eq(),
                    "!=" | "!==" => o.is_ne(),
                    "<" => o.is_lt(),
                    "<=" => o.is_le(),
                    ">" => o.is_gt(),
                    _ => o.is_ge(),
                };
                fit(Val::int(truth as i128, 1, false))
            }
            "<<" | ">>" | "<<<" | ">>>" => {
                let a = self.eval(l, unit, frame, c)?;
                let tr = self.ty(r, unit, frame)?;
                // The shift count is always unsigned.
                let Val::Int { bits: n, .. } = self.eval(r, unit, frame, tr)? else { return unsupported("a shift by a string") };
                let Val::Int { bits, .. } = a else { return unsupported("a shift of a string") };
                let w = c.w;
                let v = if n >= w as u64 {
                    if op == ">>>" && c.s && a.num().is_some_and(|x| x < 0) {
                        -1
                    } else {
                        0
                    }
                } else {
                    match op {
                        "<<" | "<<<" => (bits << n) as i128,
                        ">>>" if c.s => a.num().unwrap_or(0) >> n,
                        _ => (bits >> n) as i128,
                    }
                };
                Ok(Val::int(v, w, c.s))
            }
            "**" => {
                let a = self.eval(l, unit, frame, c)?;
                let tr = self.ty(r, unit, frame)?;
                let b = self.eval(r, unit, frame, tr)?;
                let e = b.num().unwrap_or(0);
                if e < 0 {
                    return unsupported("a negative power");
                }
                // By squaring, modulo 2^w.
                let (mut base, mut e, mut v) = (a.num().unwrap_or(0) as u128, e as u128, 1u128);
                let m = mask(c.w) as u128;
                while e > 0 {
                    if e & 1 == 1 {
                        v = v.wrapping_mul(base) & m;
                    }
                    base = base.wrapping_mul(base) & m;
                    e >>= 1;
                }
                Ok(Val::int(v as i128, c.w, c.s))
            }
            _ => {
                let a = self.eval(l, unit, frame, c)?.num().unwrap_or(0);
                let b = self.eval(r, unit, frame, c)?.num().unwrap_or(0);
                let v = match op {
                    "+" => a.wrapping_add(b),
                    "-" => a.wrapping_sub(b),
                    "*" => a.wrapping_mul(b),
                    "/" | "%" if b == 0 => return unsupported("a division by zero (an x value)"),
                    "/" => a.wrapping_div(b),
                    "%" => a.wrapping_rem(b),
                    "&" => a & b,
                    "|" => a | b,
                    "^" => a ^ b,
                    _ => !(a ^ b),
                };
                Ok(Val::int(v, c.w, c.s))
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
        let saved = std::mem::take(&mut self.leaves);
        let v = self.declared_value(r, &root);
        self.leaves = saved;
        self.evaluating.pop();
        if v.as_ref().err().is_none_or(|e| e.state != State::Incomplete) {
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

    /// The tokens of declaration `r`'s initial value (after its `=`).
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

    /// The tokens of the type a declaration's name takes (a parameter, an argument, a port,
    /// a variable), its own or, after a comma, the one before it (`logic [3:0] a, b`): without
    /// the declaration keywords. `None` when it cannot be found.
    pub fn type_tokens(&mut self, r: DeclRef) -> Option<Vec<Token>> {
        let d = self.a.decl(r);
        let toks = self.file_tokens(d.name_span.file)?;
        let name = toks.partition_point(|t| t.span.start < d.name_span.start);
        (toks.get(name)?.span == d.name_span).then_some(())?;
        type_tokens_at(&toks, name, 0)
    }

    /// Parameter `r`'s declared type.
    pub fn declared_type(&mut self, r: DeclRef, frame: &Arc<Frame>) -> R<Ty> {
        match self.type_tokens(r) {
            Some(t) => self.type_of_tokens(&t, r.unit, frame),
            None => Ok(Ty::Implicit),
        }
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
            } else if x.kind == TokKind::Ident && base.is_none() {
                // A user type: a typedef of an integral type or an enum.
                let mut last = x;
                k += 1;
                while t.get(k).is_some_and(|y| y.is_punct("::")) && t.get(k + 1).is_some_and(|y| y.kind == TokKind::Ident) {
                    last = &t[k + 1];
                    k += 2;
                }
                base = Some(self.user_type(&last.text, last.span, unit, frame)?);
                if t.get(k).is_some_and(|y| y.is_punct("[")) {
                    return unsupported(format!("the type `{}` with dimensions", text_of(t)));
                }
            } else {
                break;
            }
        }
        // Packed dimensions: of a 1-bit base only (`logic [7:0]`, `bit [3:0][7:0]`).
        let mut width: u32 = base.map_or(1, |b| b.width);
        let mut dims = 0;
        while t.get(k).is_some_and(|x| x.is_punct("[")) {
            if base.is_some_and(|b| b.width > 1) {
                return unsupported(format!("the type `{}`", text_of(t)));
            }
            let close = matching(t, k).ok_or_else(|| Unknown { state: State::Unsupported, why: "a range".into() })?;
            let n = self.range_width(&t[k + 1..close], unit, frame)?;
            width = if dims == 0 { n } else { width.saturating_mul(n) };
            dims += 1;
            k = close + 1;
        }
        if k != t.len() {
            return unsupported(format!("the type `{}`", text_of(t)));
        }
        if base.is_none() && dims == 0 {
            // Only `signed` or `unsigned`: the value keeps its width.
            return Ok(signed.map_or(Ty::Implicit, Ty::Sign));
        }
        if width > MAX_WIDTH {
            return unsupported(format!("a {width}-bit type"));
        }
        let four = base.is_none_or(|b| b.four);
        let signed = signed.unwrap_or(base.is_some_and(|b| b.signed));
        Ok(Ty::Int(IntType { width, signed, four }))
    }

    /// The width of a range `a:b` (or a size `n`).
    pub fn range_width(&mut self, inner: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<u32> {
        let (a, b) = self.range_bounds(inner, unit, frame)?;
        let n = (a - b).abs() + 1;
        if n > u32::MAX as i128 {
            return unsupported(format!("a range of {n}"));
        }
        Ok(n as u32)
    }

    /// The bounds of a range `a:b` (or `[n]`: `0:n-1`).
    pub fn range_bounds(&mut self, inner: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<(i128, i128)> {
        let parts = split_colon(inner);
        let num = |ev: &mut Self, t: &[Token]| -> R<i128> { ev.eval_tokens(t, unit, frame)?.num().ok_or_else(|| Unknown { state: State::Unsupported, why: "a string as a bound".into() }) };
        match parts.as_slice() {
            [one] => {
                let n = num(self, one)?;
                if n <= 0 {
                    return unsupported(format!("a size of {n}"));
                }
                Ok((0, n - 1))
            }
            [a, b] => Ok((num(self, a)?, num(self, b)?)),
            _ => unsupported("a range"),
        }
    }

    /// A user type named `text` at `span`: a typedef of an integral type, or an enum (its
    /// base type).
    fn user_type(&mut self, text: &str, span: Span, unit: UnitId, frame: &Arc<Frame>) -> R<IntType> {
        let a = self.a;
        let Some(r) = a.resolve_at_span(unit, span) else { return unresolved(format!("the type `{text}` is not resolved")) };
        let d = a.decl(r);
        if d.kind != DeclKind::Typedef {
            return unsupported(format!("the type `{text}`"));
        }
        let Some(toks) = self.file_tokens(d.name_span.file) else { return unsupported(format!("the type `{text}`")) };
        let k = toks.partition_point(|x| x.span.start < d.span.start);
        let name = toks.partition_point(|x| x.span.start < d.name_span.start);
        if !toks.get(k).is_some_and(|x| x.is_kw("typedef")) || name <= k + 1 {
            return unsupported(format!("the type `{text}`"));
        }
        let body: Vec<Token> = toks[k + 1..name].to_vec();
        self.enter()?;
        let r = if body.first().is_some_and(|x| x.is_kw("enum")) {
            let brace = body.iter().position(|x| x.is_punct("{")).unwrap_or(body.len());
            self.enum_base(&body[1..brace], r.unit, frame)
        } else if body.first().is_some_and(|x| x.is_kw("struct") || x.is_kw("union")) {
            unsupported(format!("the struct or union type `{text}`"))
        } else {
            match self.type_of_tokens(&body, r.unit, frame) {
                Ok(Ty::Int(i)) => Ok(i),
                Ok(_) => unsupported(format!("the type `{text}`")),
                Err(e) => Err(e),
            }
        };
        self.nest -= 1;
        r
    }

    /// An enum's base type (`int` when none is written).
    fn enum_base(&mut self, base: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<IntType> {
        if base.is_empty() {
            return Ok(IntType { width: 32, signed: true, four: false });
        }
        match self.type_of_tokens(base, unit, frame)? {
            Ty::Int(i) => Ok(i),
            _ => unsupported("an enum's base type"),
        }
    }

    /// An enum member's value: its own, or one more than the previous member's, in the
    /// enum's base type.
    fn enum_member(&mut self, r: DeclRef) -> R<Val> {
        let a = self.a;
        let d = a.decl(r);
        let Some(e) = d.parent else { return unsupported("an enum member") };
        let p = &a.unit(r.unit).parsed;
        let ed = p.decl(e);
        let root = Arc::new(Frame::default());
        // The base: the tokens between `enum` and `{`.
        let toks = self.file_tokens(ed.span.file).ok_or_else(|| Unknown { state: State::Unsupported, why: "an enum member".into() })?;
        let k = toks.partition_point(|x| x.span.start < ed.span.start);
        let base = if toks.get(k).is_some_and(|x| x.is_kw("enum")) {
            let brace = toks[k..].iter().position(|x| x.is_punct("{")).map_or(k + 1, |j| k + j);
            let b: Vec<Token> = toks[k + 1..brace].to_vec();
            self.enum_base(&b, r.unit, &root)?
        } else {
            return unsupported("an enum member");
        };
        let ty = Ty::Int(base);
        let mut prev: Option<Val> = None;
        for &c in &ed.children {
            let m = DeclRef { unit: r.unit, decl: c };
            if a.decl(m).kind != DeclKind::EnumMember {
                continue;
            }
            let v = match self.default_tokens(m) {
                Some(t) => self.eval_as(&t, r.unit, &root, &ty)?,
                None => match &prev {
                    None => Val::int(0, base.width, base.signed),
                    Some(p) => Val::int(p.num().unwrap_or(0) + 1, base.width, base.signed),
                },
            };
            if c == r.decl {
                return Ok(v);
            }
            prev = Some(v);
        }
        unsupported("an enum member")
    }

    fn system(&mut self, n: &str, args: &[Ex], raw: &[Token], unit: UnitId, frame: &Arc<Frame>) -> R<Val> {
        match n {
            "$clog2" => {
                let [x] = args else { return unsupported("`$clog2` with other than one argument") };
                let t = self.ty(x, unit, frame)?;
                // Its argument is unsigned.
                let Val::Int { bits, .. } = self.eval(x, unit, frame, T { s: false, ..t })? else { return unsupported("`$clog2` of a string") };
                let mut r = 0u32;
                while r < 64 && (1u128 << r) < bits as u128 {
                    r += 1;
                }
                Ok(Val::int(r as i128, 32, true))
            }
            "$signed" | "$unsigned" => {
                let [x] = args else { return unsupported(format!("`{n}` with other than one argument")) };
                let t = self.ty(x, unit, frame)?;
                let v = self.eval(x, unit, frame, t)?;
                Ok(retype(v, n == "$signed"))
            }
            "$bits" => {
                // A type, or an expression (its width).
                let w = match self.type_of_tokens(raw, unit, frame) {
                    Ok(Ty::Int(i)) => i.width,
                    _ => {
                        let t = self.expand(raw);
                        let x = parse(&t)?;
                        if let Ex::Name(_, sp) = &x {
                            // A signal: its declared type.
                            if let Some(r) = self.a.resolve_at_span(unit, *sp) {
                                if matches!(self.a.decl(r).kind, DeclKind::Variable | DeclKind::Net | DeclKind::Port) {
                                    return self.signal_width(r, frame).map(|w| Val::int(w as i128, 32, true));
                                }
                            }
                        }
                        let saved = std::mem::take(&mut self.leaves);
                        let t = self.ty(&x, unit, frame);
                        self.leaves = saved;
                        let t = t?;
                        if t.string {
                            return unsupported("`$bits` of a string");
                        }
                        t.w
                    }
                };
                Ok(Val::int(w as i128, 32, true))
            }
            _ => unsupported(format!("`{n}`")),
        }
    }

    /// The width of a signal's declared type (packed dimensions only).
    pub fn signal_width(&mut self, r: DeclRef, frame: &Arc<Frame>) -> R<u32> {
        let Some(t) = self.type_tokens(r) else { return unsupported("a signal's type") };
        match self.type_of_tokens(&t, r.unit, frame)? {
            Ty::Int(i) => Ok(i.width),
            Ty::Implicit | Ty::Sign(_) => Ok(1),
            Ty::Str => unsupported("a string signal"),
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
        let mut types = FxHashMap::default();
        for ((k, ty), x) in f.args.iter().zip(args) {
            vals.push((*k, Ok(self.assign(x, unit, frame, ty)?)));
            types.insert(*k, ty.clone());
        }
        // The result variable (the function's name): a four-state type starts as X.
        vals.push((key, initial(&f.ret)));
        types.insert(key, f.ret.clone());
        // The caller's frame stays visible: a module's function may read its parameters.
        let mut fr = Frame::with(frame, vals);
        self.enter()?;
        self.var_types.push(types);
        let flow = self.run_block(&f.body, f.unit, &mut fr, key);
        self.var_types.pop();
        self.nest -= 1;
        match flow? {
            Flow::Return(Some(v)) => Ok(v),
            _ => fr.get(key).cloned().unwrap_or_else(|| unresolved("no result")),
        }
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
    fn set(fr: &mut Arc<Frame>, key: (u32, u32), v: R<Val>) {
        let mut vals: Vec<((u32, u32), R<Val>)> = fr.vals.clone();
        if let Some(x) = vals.iter_mut().find(|(k, _)| *k == key) {
            x.1 = v;
        } else {
            vals.push((key, v));
        }
        *fr = Arc::new(Frame { vals, parent: fr.parent.clone() });
    }

    fn var_type(&self, key: (u32, u32)) -> Ty {
        self.var_types.last().and_then(|m| m.get(&key)).cloned().unwrap_or(Ty::Implicit)
    }

    fn truth_of(&mut self, x: &Ex, unit: UnitId, fr: &Arc<Frame>) -> R<bool> {
        self.value(x, unit, fr)?.truth()
    }

    fn run(&mut self, s: &Stmt, unit: UnitId, fr: &mut Arc<Frame>, fkey: (u32, u32)) -> R<Flow> {
        self.step()?;
        match s {
            Stmt::Nop => Ok(Flow::Next),
            Stmt::Local(k, ty, init) => {
                if let Some(m) = self.var_types.last_mut() {
                    m.insert(*k, ty.clone());
                }
                let v = match init {
                    Some(x) => {
                        let f = fr.clone();
                        self.assign(x, unit, &f, ty)
                    }
                    None => initial(ty),
                };
                Self::set(fr, *k, v);
                Ok(Flow::Next)
            }
            Stmt::Assign(k, x) => {
                let ty = self.var_type(*k);
                let f = fr.clone();
                let v = self.assign(x, unit, &f, &ty);
                // An unknown value stays unknown in the variable (it may be assigned again).
                Self::set(fr, *k, v);
                Ok(Flow::Next)
            }
            Stmt::Return(x) => {
                let v = match x {
                    Some(x) => {
                        let ty = self.var_type(fkey);
                        let f = fr.clone();
                        Some(self.assign(x, unit, &f, &ty)?)
                    }
                    None => None,
                };
                Ok(Flow::Return(v))
            }
            Stmt::If(c, a, b) => {
                let f = fr.clone();
                if self.truth_of(c, unit, &f)? {
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
                if !self.truth_of(c, unit, &f)? {
                    return Ok(Flow::Next);
                }
                if let Flow::Return(v) = self.run(body, unit, fr, fkey)? {
                    return Ok(Flow::Return(v));
                }
            },
            Stmt::Repeat(n, body) => {
                let f = fr.clone();
                let n = self.value(n, unit, &f)?.num().unwrap_or(0);
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
                        if !self.truth_of(c, unit, &f)? {
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
        let ret = if name > k {
            match self.type_of_tokens(&t[k..name], r.unit, &root).ok()? {
                Ty::Implicit => return None,
                x => x,
            }
        } else {
            Ty::Int(IntType { width: 1, signed: false, four: true })
        };
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
            let ty = match self.type_tokens(ar) {
                Some(tt) => self.type_of_tokens(&tt, ar.unit, &root).ok()?,
                None => return None,
            };
            // An argument without a type is a 1-bit logic.
            let ty = if ty == Ty::Implicit { Ty::Int(IntType { width: 1, signed: false, four: true }) } else { ty };
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
        let body_toks = self.expand(&t[j + 1..end]);
        let mut sp = StmtParser { e: self, t: &body_toks, k: 0, unit: r.unit, func: r };
        let mut body = Vec::new();
        while sp.k < body_toks.len() {
            body.push(sp.stmt()?);
        }
        Some(Func { ret, args, body, unit: r.unit })
    }
}

/// A variable's value before any assignment: 0, or unknown (X) for a four-state type.
fn initial(ty: &Ty) -> R<Val> {
    match ty {
        Ty::Int(i) if !i.four => Ok(Val::int(0, i.width, i.signed)),
        Ty::Str => Ok(Val::Str(String::new())),
        _ => unsupported("an x value (a four-state variable not yet assigned)"),
    }
}

/// `v` with signedness `signed` (its bits unchanged).
fn retype(v: Val, signed: bool) -> Val {
    match v {
        Val::Int { bits, width, .. } => Val::Int { bits, width, signed },
        x => x,
    }
}

/// The type tokens of the declarator whose name is `toks[name]` (see `Eval::type_tokens`).
fn type_tokens_at(toks: &[Token], name: usize, depth: usize) -> Option<Vec<Token>> {
    if depth > 256 {
        return None;
    }
    const DECL_KW: &[&str] = &[
        "parameter", "localparam", "input", "output", "inout", "ref", "const", "var", "automatic", "static", "wire", "tri", "wand", "wor", "triand", "trior", "trireg", "tri0", "tri1", "uwire", "supply0", "supply1", "rand", "randc", "local", "protected", "interconnect",
    ];
    // Back from the name to `(`, `,`, `;` or a declaration keyword, over bracket groups.
    let mut k = name;
    let mut stop_comma = false;
    while k > 0 {
        let t = &toks[k - 1];
        if t.is_punct("]") || t.is_punct(")") {
            let close = k - 1;
            let mut d = 0i32;
            let mut j = close;
            loop {
                if toks[j].is_punct("]") || toks[j].is_punct(")") {
                    d += 1;
                } else if toks[j].is_punct("[") || toks[j].is_punct("(") {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                if j == 0 {
                    return None;
                }
                j -= 1;
            }
            // `#(...)` of a parameterized type belongs to it; a `(` of a list does not.
            if toks[j].is_punct("(") && !(j > 0 && toks[j - 1].is_punct("#")) {
                break;
            }
            k = j;
            continue;
        }
        if t.is_punct("(") || t.is_punct(";") || t.is_punct("#") {
            break;
        }
        if t.is_punct(",") {
            stop_comma = true;
            break;
        }
        if t.kind == TokKind::Keyword && (DECL_KW.contains(&t.text.as_str()) || matches!(t.text.as_str(), "function" | "task" | "module" | "interface" | "program" | "begin" | "end" | "typedef")) {
            break;
        }
        k -= 1;
    }
    let own: Vec<Token> = toks[k..name].iter().filter(|t| !(t.kind == TokKind::Keyword && DECL_KW.contains(&t.text.as_str()))).cloned().collect();
    let had_any = k < name;
    if !own.is_empty() || !stop_comma || had_any {
        return Some(own);
    }
    // After a comma with nothing of its own: the previous declarator's type.
    let comma = k - 1;
    let mut j = comma;
    let mut d = 0i32;
    let mut eq = None;
    while j > 0 {
        j -= 1;
        let t = &toks[j];
        if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
            d += 1;
        } else if t.is_punct("(") || t.is_punct("[") || t.is_punct("{") {
            if d == 0 {
                break;
            }
            d -= 1;
        } else if d == 0 && (t.is_punct(",") || t.is_punct(";")) {
            break;
        } else if d == 0 && t.is_punct("=") {
            eq = Some(j);
        }
    }
    // The previous name: before its `=`, else just before the comma (over unpacked ranges).
    let mut p = eq.unwrap_or(comma);
    while p > 0 && toks[p - 1].is_punct("]") {
        let mut d = 0i32;
        let mut q = p - 1;
        loop {
            if toks[q].is_punct("]") {
                d += 1;
            } else if toks[q].is_punct("[") {
                d -= 1;
                if d == 0 {
                    break;
                }
            }
            if q == 0 {
                return None;
            }
            q -= 1;
        }
        p = q;
    }
    let prev = p.checked_sub(1)?;
    (toks[prev].kind == TokKind::Ident).then_some(())?;
    type_tokens_at(toks, prev, depth + 1)
}

/// A parser of a constant function's statements.
struct StmtParser<'e, 'a, 't> {
    e: &'e mut Eval<'a>,
    t: &'t [Token],
    k: usize,
    unit: UnitId,
    /// The function parsed.
    func: DeclRef,
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
            if self.at(",") && stop == [";"] {
                // `int a, b;`: one local per statement only.
                return None;
            }
            return Some(Stmt::Local(key, ty, init));
        }
        // `x++`, `++x`, `x = e`, `x += e`.
        let prefix = t.is_punct("++") || t.is_punct("--");
        let (name, op) = if prefix {
            let n = self.t.get(self.k + 1)?.clone();
            self.k += 2;
            (n, if t.is_punct("++") { "++" } else { "--" })
        } else if t.kind == TokKind::Ident {
            self.k += 1;
            let o = self.t.get(self.k)?.clone();
            const OPS: &[&str] = &["=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", "<<<=", ">>>=", "++", "--"];
            let op = OPS.iter().find(|x| o.is_punct(x))?;
            self.k += 1;
            (t.clone(), *op)
        } else {
            return None;
        };
        if name.kind != TokKind::Ident {
            return None;
        }
        let key = self.key_of(&name)?;
        let one = || Ex::Num(Val::int(1, 32, true));
        let x = match op {
            "++" => Ex::Bin("+", Box::new(Ex::Key(key)), Box::new(one())),
            "--" => Ex::Bin("-", Box::new(Ex::Key(key)), Box::new(one())),
            "=" => self.expr_until(stop)?,
            o => {
                let rhs = self.expr_until(stop)?;
                let bin: &'static str = match o {
                    "+=" => "+",
                    "-=" => "-",
                    "*=" => "*",
                    "/=" => "/",
                    "%=" => "%",
                    "&=" => "&",
                    "|=" => "|",
                    "^=" => "^",
                    "<<=" => "<<",
                    ">>=" => ">>",
                    "<<<=" => "<<<",
                    ">>>=" => ">>>",
                    _ => return None,
                };
                Ex::Bin(bin, Box::new(Ex::Key(key)), Box::new(rhs))
            }
        };
        Some(Stmt::Assign(key, x))
    }
}

/// `v` converted to type `ty` (an assignment of an already evaluated value).
pub fn convert(v: Val, ty: &Ty) -> R<Val> {
    match (ty, v) {
        (Ty::Implicit, v) => Ok(v),
        (Ty::Sign(s), v @ Val::Int { .. }) => Ok(retype(v, *s)),
        (Ty::Str, v @ Val::Str(_)) => Ok(v),
        (Ty::Str, _) => unsupported("a number given to a string"),
        (Ty::Int(i), v @ Val::Int { .. }) => {
            let s = v.signed();
            Ok(retype(v.to(i.width, s)?, i.signed))
        }
        (_, Val::Str(_)) => unsupported("a string given to a number"),
        (_, Val::Type(_)) => unsupported("a type given to a number"),
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
    pub decl: DeclId,
    pub presence: Presence,
    /// A generate loop's block: the genvar and its values (or why they are not known).
    pub iterations: Option<R<((u32, u32), Vec<i128>)>>,
    /// The name it is shown with (`genblk2` for an unnamed generate block).
    pub name: SmolStr,
    /// In the body of a conditional generate written without `begin`: the implicit generate
    /// block it is in (`genblk1`).
    pub implicit: Option<SmolStr>,
}

/// A plain `defparam` (names only), with what it is written in.
struct Plain {
    /// The definition the statement is in.
    module: (u32, u32),
    names: Vec<SmolStr>,
    value: Option<(Span, Span)>,
    unit: UnitId,
}

/// A `defparam`'s value: its tokens and where they are evaluated.
type Setting = (Vec<Token>, UnitId, Arc<Frame>);

/// Elaboration of one design: the evaluator and what holds for every instance.
pub struct Elab<'a> {
    pub ev: Eval<'a>,
    plain: Vec<Plain>,
    /// Parameter names some `defparam` this version does not follow may set.
    others: FxHashSet<SmolStr>,
    /// Generate constructs by (unit, scope), in source order.
    constructs: FxHashMap<(UnitId, DeclId), Vec<usize>>,
    /// Definitions that may be instantiated under the top being elaborated.
    instantiated: FxHashSet<(u32, u32)>,
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
        let mut constructs: FxHashMap<(UnitId, DeclId), Vec<usize>> = FxHashMap::default();
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
        Elab { ev: Eval::new(a), plain, others, constructs, instantiated: FxHashSet::default() }
    }

    /// Elaborates under top `t` from now on: the definitions that may be instantiated under
    /// it (in any generate branch) are those an absolute `defparam` may come from.
    pub fn set_top(&mut self, t: DeclRef) {
        let a = self.a();
        if self.plain.is_empty() {
            return;
        }
        let mut seen: FxHashSet<(u32, u32)> = FxHashSet::default();
        let mut todo = vec![t];
        seen.insert(a.decl_key(a.canonical(t)));
        while let Some(def) = todo.pop() {
            if a.cancelled() || seen.len() > 1_000_000 {
                break;
            }
            let u = a.unit(def.unit);
            let mut scopes = vec![def.decl];
            while let Some(sc) = scopes.pop() {
                for &c in &u.parsed.decl(sc).children {
                    let d = u.parsed.decl(c);
                    match d.kind {
                        DeclKind::Instance if !d.has(dflags::BOUND) => {
                            if let Some(m) = a.type_of(DeclRef { unit: def.unit, decl: c }).filter(|m| a.decl(*m).kind.is_definition()) {
                                if seen.insert(a.decl_key(a.canonical(m))) {
                                    todo.push(m);
                                }
                            }
                        }
                        DeclKind::Generate | DeclKind::Block => scopes.push(c),
                        _ => {}
                    }
                }
            }
        }
        self.instantiated = seen;
    }

    fn a(&self) -> &'a Analysis {
        self.ev.a
    }

    /// Whether `def` has a parameter port list (`#(...)`, `#()` too): its body's parameters
    /// are then local (IEEE 1800 §6.20.1).
    fn has_param_list(&mut self, def: DeclRef) -> bool {
        let d = self.a().decl(def);
        let Some(toks) = self.ev.file_tokens(d.name_span.file) else { return false };
        let mut j = toks.partition_point(|t| t.span.start < d.name_span.start) + 1;
        // `import pkg::*;` clauses first.
        while toks.get(j).is_some_and(|t| t.is_kw("import")) {
            while toks.get(j).is_some_and(|t| !t.is_punct(";")) {
                j += 1;
            }
            j += 1;
        }
        toks.get(j).is_some_and(|t| t.is_punct("#"))
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
        // The defparams that set a parameter of this instance, with where to evaluate them.
        let n = path.len();
        let mut found: Vec<(SmolStr, Option<(Span, Span)>, UnitId, Arc<Frame>)> = Vec::new();
        // Parameters an absolute defparam written elsewhere under the top may set.
        let mut maybe: FxHashSet<SmolStr> = FxHashSet::default();
        for p in &self.plain {
            let Some((pname, prefix)) = p.names.split_last() else { continue };
            let mut frame: Option<Arc<Frame>> = None;
            // Relative to an instance of the module it is written in, above this one.
            for x in anc {
                if x.module == p.module && x.depth + prefix.len() == n && path[x.depth..] == prefix[..] {
                    frame = Some(x.frame.clone());
                }
            }
            let absolute = prefix.len() == n && path == prefix;
            if let Some(frame) = frame {
                found.push((pname.clone(), p.value, p.unit, frame));
            } else if let (true, Some(x)) = (absolute, anc.iter().rev().find(|x| x.module == p.module)) {
                // An absolute path written in an instance above this one.
                found.push((pname.clone(), p.value, p.unit, x.frame.clone()));
            } else if absolute && self.instantiated.contains(&p.module) {
                // Written in a module elsewhere under the top: it applies when that module is
                // elaborated, which this walk has not established.
                maybe.insert(pname.clone());
            }
        }
        let mut set_by: FxHashMap<SmolStr, Vec<Option<Setting>>> = FxHashMap::default();
        for (pname, value, unit, frame) in found {
            let t = value.and_then(|v| self.ev.tokens_of(v));
            set_by.entry(pname).or_default().push(t.map(|t| (t, unit, frame)));
        }
        let header_list = self.has_param_list(def);
        // Each parameter in order: later ones may use earlier ones.
        let mut vals: Vec<((u32, u32), R<Val>)> = Vec::new();
        let mut shown = Vec::new();
        let u = a.unit(def.unit);
        let params: Vec<DeclRef> = u.parsed.decl(def.decl).children.iter().map(|&c| DeclRef { unit: def.unit, decl: c }).filter(|c| matches!(a.decl(*c).kind, DeclKind::Parameter | DeclKind::LocalParam | DeclKind::TypeParam)).collect();
        for p in params {
            let d = a.decl(p);
            let key = a.decl_key(a.canonical(p));
            let frame = Arc::new(Frame { vals: vals.clone(), parent: None });
            // A body parameter is local when the definition has a parameter port list.
            let local = d.kind == DeclKind::LocalParam || d.has(dflags::LOCAL_PARAM) || (header_list && !d.has(dflags::PARAM_PORT));
            let typed = d.kind == DeclKind::TypeParam;
            let name = d.name.clone();
            let (v, origin) = if local {
                if overrides.contains_key(name.as_str()) || set_by.contains_key(&name) {
                    (invalid(format!("`{name}` is local: it cannot be overridden")), Origin::Override)
                } else {
                    (self.ev.declared_value(p, &frame), Origin::Local)
                }
            } else if let Some(vs) = set_by.get(&name).cloned() {
                let v = if vs.len() > 1 {
                    invalid("more than one defparam sets it")
                } else if typed {
                    invalid("a defparam cannot set a type parameter")
                } else {
                    match &vs[0] {
                        Some((t, unit, f)) => self.ev.declared_type(p, &frame).and_then(|ty| self.ev.eval_as(t, *unit, f, &ty)),
                        None => unsupported("the defparam's value"),
                    }
                };
                (v, Origin::Defparam)
            } else if self.others.contains(&name) || maybe.contains(&name) {
                (unresolved("a defparam with a path this version does not follow may set it"), Origin::Defparam)
            } else if let Some(o) = overrides.get(name.as_str()) {
                match o {
                    Some(t) if typed => (Ok(Val::Type(text_of(t))), Origin::Override),
                    // Evaluated where the instance is, as an assignment to the parameter's type.
                    Some(t) => (self.ev.declared_type(p, &frame).and_then(|ty| self.ev.eval_as(t, inst.map_or(def.unit, |i| i.unit), parent, &ty)), Origin::Override),
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
    /// the names of unnamed generate blocks, and the implicit blocks of bodies without `begin`.
    pub fn children(&mut self, scope: DeclRef, frame: &Arc<Frame>) -> Vec<Child> {
        let a = self.a();
        let u = a.unit(scope.unit);
        let ids = self.constructs.get(&(scope.unit, scope.decl)).cloned().unwrap_or_default();
        let gs: Vec<dvi_syntax::ast::GenConstruct> = ids.iter().map(|&i| u.parsed.generates[i].clone()).collect();
        // A branch with `begin`: its first declaration is its block, a child of the scope.
        let braced = |b: &dvi_syntax::ast::GenBranch| b.decls.0 < b.decls.1 && u.parsed.decl(b.decls.0).kind == DeclKind::Generate && u.parsed.decl(b.decls.0).parent == Some(scope.decl);
        let within = |g: &dvi_syntax::ast::GenConstruct, b: &dvi_syntax::ast::GenBranch| !g.branches.is_empty() && g.branches.iter().all(|x| b.decls.0 <= x.decls.0 && x.decls.1 <= b.decls.1);
        // A conditional directly in another's branch without `begin` takes no number of its
        // own (IEEE 1800 §27.5): the outermost such construct's.
        let mut owner: Vec<Option<usize>> = vec![None; gs.len()];
        for x in 0..gs.len() {
            if gs[x].kind == GenKind::For {
                continue;
            }
            for y in 0..gs.len() {
                if y != x && gs[y].kind != GenKind::For && gs[y].start.start < gs[x].start.start && gs[y].branches.iter().any(|b| !braced(b) && within(&gs[x], b)) && owner[x].is_none_or(|o| gs[o].start.start > gs[y].start.start) {
                    owner[x] = Some(y);
                }
            }
        }
        let root = |mut x: usize| {
            let mut n = 0;
            while let Some(o) = owner[x] {
                x = o;
                n += 1;
                if n > gs.len() {
                    break;
                }
            }
            x
        };
        let mut ordinal = vec![0usize; gs.len()];
        let mut count = 0;
        for x in 0..gs.len() {
            if owner[x].is_none() {
                count += 1;
                ordinal[x] = count;
            }
        }
        for x in 0..gs.len() {
            ordinal[x] = ordinal[root(x)];
        }
        let mut branches: Vec<((u32, u32), Presence)> = Vec::new();
        let mut loops: FxHashMap<DeclId, usize> = FxHashMap::default();
        let mut names: FxHashMap<DeclId, SmolStr> = FxHashMap::default();
        // Bodies without `begin` of outermost constructs: their items' implicit block.
        let mut implicit: Vec<((u32, u32), SmolStr)> = Vec::new();
        let mut blocks: FxHashSet<DeclId> = FxHashSet::default();
        for x in 0..gs.len() {
            let g = &gs[x];
            let genblk = SmolStr::new(format!("genblk{}", ordinal[x]));
            match g.kind {
                GenKind::For => {
                    if let Some(b) = g.block {
                        loops.insert(b, ids[x]);
                        if u.parsed.decl(b).has(dflags::FROM_MACRO) {
                            names.insert(b, genblk);
                        }
                    }
                }
                GenKind::If | GenKind::Case => {
                    let p = self.decide(g, scope.unit, frame);
                    for (b, pr) in g.branches.iter().zip(p) {
                        if braced(b) {
                            blocks.insert(b.decls.0);
                            if u.parsed.decl(b.decls.0).has(dflags::FROM_MACRO) {
                                names.insert(b.decls.0, genblk.clone());
                            }
                        } else if owner[x].is_none() && b.decls.0 < b.decls.1 {
                            // Not when its only item is a conditional directly in it.
                            let nested = (0..gs.len()).any(|y| owner[y] == Some(x) && within(&gs[y], b));
                            if !nested {
                                implicit.push((b.decls, genblk.clone()));
                            }
                        } else if owner[x].is_some() && b.decls.0 < b.decls.1 {
                            implicit.push((b.decls, genblk.clone()));
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
            // The innermost body without `begin` it is in.
            let implicit = (!blocks.contains(&c)).then(|| implicit.iter().filter(|((lo, hi), _)| *lo <= c && c < *hi).min_by_key(|((lo, hi), _)| hi - lo).map(|(_, n)| n.clone())).flatten();
            out.push(Child { decl: c, presence, iterations, name, implicit });
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
                                    Some(t) => self.ev.eval_tokens(&t, unit, frame).and_then(|v| v.truth()),
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
            GenKind::Case => match self.case_choice(g, unit, frame) {
                Ok(chosen) => {
                    let default = g.branches.iter().position(|b| b.exprs.is_empty());
                    for i in 0..g.branches.len() {
                        let elaborated = match chosen {
                            Some(c) => c == i,
                            None => Some(i) == default,
                        };
                        out.push(if elaborated { Presence::Elaborated } else { Presence::NotElaborated });
                    }
                }
                Err(w) => return g.branches.iter().map(|_| Presence::Undetermined(w.clone())).collect(),
            },
            GenKind::For => {}
        }
        out
    }

    /// The item a generate `case` takes (`None`: no label matches: the default), its
    /// expression and labels evaluated in their common type (IEEE 1800 §12.5).
    fn case_choice(&mut self, g: &dvi_syntax::ast::GenConstruct, unit: UnitId, frame: &Arc<Frame>) -> Result<Option<usize>, String> {
        let mut texts: Vec<(Vec<Token>, usize)> = Vec::new();
        let sel = g.exprs.first().copied().flatten().and_then(|e| self.ev.tokens_of(e)).ok_or_else(|| "case expression written through a macro".to_string())?;
        texts.push((sel, usize::MAX));
        for (i, b) in g.branches.iter().enumerate() {
            for &e in &b.exprs {
                let t = self.ev.tokens_of(e).ok_or_else(|| "a case item written through a macro".to_string())?;
                texts.push((t, i));
            }
        }
        let mut exprs = Vec::new();
        for (t, _) in &texts {
            let t = self.ev.expand(t);
            exprs.push(parse(&t).map_err(|e| format!("case item not evaluated: {}", e.why))?);
        }
        let saved = std::mem::take(&mut self.ev.leaves);
        let r = (|| -> Result<Option<usize>, String> {
            let mut t = T { w: 0, s: true, string: false };
            let mut strings = 0;
            for x in &exprs {
                let ty = self.ev.ty(x, unit, frame).map_err(|e| format!("case item not evaluated: {}", e.why))?;
                if ty.string {
                    strings += 1;
                }
                t.w = t.w.max(ty.w);
                t.s &= ty.s;
            }
            if strings > 0 && strings != exprs.len() {
                return Err("case items of strings and numbers".into());
            }
            if strings > 0 {
                t = STRING;
            }
            let v = self.ev.eval(&exprs[0], unit, frame, t).map_err(|e| format!("case expression not evaluated: {}", e.why))?;
            for (x, (_, i)) in exprs.iter().zip(&texts).skip(1) {
                let l = self.ev.eval(x, unit, frame, t).map_err(|e| format!("case item not evaluated: {}", e.why))?;
                if l == v {
                    return Ok(Some(*i));
                }
            }
            Ok(None)
        })();
        self.ev.leaves = saved;
        r
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
        // A genvar is an integer.
        let int = Ty::Int(IntType { width: 32, signed: true, four: false });
        let mut v = self.ev.eval_as(&init[eq + 1..], unit, frame, &int)?.num().unwrap_or(0);
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        loop {
            let f = Frame::with(frame, vec![(key, Ok(Val::int(v, 32, true)))]);
            if !self.ev.eval_tokens(&cond, unit, &f)?.truth()? {
                break;
            }
            if !seen.insert(v) {
                return invalid(format!("the loop repeats {v}"));
            }
            out.push(v);
            if out.len() > MAX_ITERATIONS {
                return incomplete(format!("more than {MAX_ITERATIONS} iterations"));
            }
            v = self.step(&step, var, unit, &f, &int)?;
        }
        Ok((key, out))
    }

    /// The genvar's next value after `step` (`i++`, `i = i + 2`, `i <<= 1`...), as an integer.
    fn step(&mut self, step: &[Token], var: &Token, unit: UnitId, f: &Arc<Frame>, int: &Ty) -> R<i128> {
        let names_var = |t: &Token| t.kind == TokKind::Ident && t.text == var.text;
        // The step as the expression assigned: `var op (rest)`.
        let tok = |kind: TokKind, text: &str| {
            let mut t = var.clone();
            t.kind = kind;
            t.text = text.into();
            t
        };
        let expr = |op: &str, rest: &[Token]| -> Vec<Token> {
            let mut t = vec![var.clone(), tok(TokKind::Punct, op), tok(TokKind::Punct, "(")];
            t.extend(rest.iter().cloned());
            t.push(tok(TokKind::Punct, ")"));
            t
        };
        let e: Vec<Token> = match step {
            [x, op] | [op, x] if names_var(x) && (op.is_punct("++") || op.is_punct("--")) => expr(if op.is_punct("++") { "+" } else { "-" }, &[tok(TokKind::Number, "1")]),
            [x, op, rest @ ..] if names_var(x) && !rest.is_empty() => match op.text.as_str() {
                "=" => rest.to_vec(),
                o => match o.strip_suffix('=').filter(|b| matches!(*b, "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "<<" | ">>" | "<<<" | ">>>")) {
                    Some(b) => expr(b, rest),
                    None => return unsupported("the loop's step"),
                },
            },
            _ => return unsupported("the loop's step"),
        };
        Ok(self.ev.eval_as(&e, unit, f, int)?.num().unwrap_or(0))
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

/// Instances and generated scopes indexed at most.
const MAX_INDEXED: usize = 200_000;
/// Instance paths a hover lists at most, and values.
const MAX_LISTED: usize = 20;
const MAX_VALUES: usize = 10;

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
    /// Every instance was reached (no limit, no recursion cut, nothing undetermined above a
    /// loop).
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
    /// Counts one scope or instance; false when the budget is spent.
    fn charge(&mut self) -> bool {
        self.count += 1;
        if self.count > MAX_INDEXED || (self.count % 1024 == 0 && self.el.a().cancelled()) {
            self.index.complete = false;
            return false;
        }
        true
    }

    fn walk(&mut self, scope: DeclRef, frame: &Arc<Frame>, path: &str, certain: bool) {
        let a = self.el.a();
        if a.cancelled() {
            self.index.complete = false;
            return;
        }
        let u = a.unit(scope.unit);
        for child in self.el.children(scope, frame) {
            if child.presence == Presence::NotElaborated {
                continue;
            }
            if !self.charge() {
                return;
            }
            let sure = certain && child.presence == Presence::Elaborated;
            let d = u.parsed.decl(child.decl);
            // The implicit block of a body without `begin`.
            let path = match &child.implicit {
                Some(n) => {
                    self.names.push(n.clone());
                    format!("{path}.{n}")
                }
                None => path.to_string(),
            };
            match d.kind {
                DeclKind::Instance if !d.has(dflags::BOUND) => {
                    let cr = DeclRef { unit: scope.unit, decl: child.decl };
                    if let Some(m) = a.type_of(cr).filter(|m| a.decl(*m).kind.is_definition()) {
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
                        } else {
                            // A recursion cut: what is below is not counted.
                            self.index.complete = false;
                        }
                        self.names.pop();
                    }
                }
                DeclKind::Generate | DeclKind::Block => {
                    let gr = DeclRef { unit: scope.unit, decl: child.decl };
                    let named = d.kind == DeclKind::Generate;
                    match &child.iterations {
                        Some(Ok((key, values))) => {
                            for v in values.clone() {
                                if !self.charge() {
                                    break;
                                }
                                let f = self.el.block_frame(gr, frame, Some((*key, v)));
                                let copy = SmolStr::new(format!("{}[{v}]", child.name));
                                let p = if named { format!("{path}.{copy}") } else { path.clone() };
                                if named {
                                    self.names.push(copy);
                                }
                                self.walk(gr, &f, &p, sure);
                                if named {
                                    self.names.pop();
                                }
                            }
                        }
                        Some(Err(_)) => {
                            self.index.complete = false;
                            let f = self.el.block_frame(gr, frame, None);
                            let p = if named { format!("{path}.{}", child.name) } else { path.clone() };
                            if named {
                                self.names.push(child.name.clone());
                            }
                            self.walk(gr, &f, &p, false);
                            if named {
                                self.names.pop();
                            }
                        }
                        None => {
                            let f = self.el.block_frame(gr, frame, None);
                            let p = if named { format!("{path}.{}", child.name) } else { path.clone() };
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
            if child.implicit.is_some() {
                self.names.pop();
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
            w.el.set_top(t);
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
            // Only when the width depends on something: a name in its type's range.
            let t = el.ev.type_tokens(r)?;
            let named = t.iter().skip_while(|t| !t.is_punct("[")).any(|t| t.kind == TokKind::Ident || t.kind == TokKind::Directive);
            if !named {
                return None;
            }
        }
        let index = self.elab_index()?;
        let def_key = self.decl_key(self.canonical(DeclRef { unit: r.unit, decl: parent }));
        let insts = index.by_def.get(&def_key)?;
        let key = self.decl_key(self.canonical(r));
        // Value (or why not) → the instances with it, in order of first appearance.
        let mut order: Vec<String> = Vec::new();
        let mut groups: FxHashMap<String, Vec<(&str, bool)>> = FxHashMap::default();
        for (n, i) in insts.iter().enumerate() {
            if n % 256 == 0 && self.cancelled() {
                return None;
            }
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
            let g = groups.entry(v.clone()).or_default();
            if g.is_empty() {
                order.push(v);
            }
            g.push((&i.path, i.certain));
        }
        let title = if param { "Values in the design hierarchy" } else { "Width in the design hierarchy" };
        let mut md = format!("**{title}** ({} instance{} from {})", insts.len(), if insts.len() == 1 { "" } else { "s" }, index.tops.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", "));
        let mut listed = 0;
        for v in order.iter().take(MAX_VALUES) {
            let paths = &groups[v];
            let room = MAX_LISTED.saturating_sub(listed).max(1);
            let shown: Vec<String> = paths.iter().take(room).map(|(p, sure)| if *sure { format!("`{p}`") } else { format!("`{p}` (if it exists)") }).collect();
            listed += shown.len();
            let more = if paths.len() > shown.len() { format!(", and {} more", paths.len() - shown.len()) } else { String::new() };
            md.push_str(&format!("\n- `{v}` in {}{more}", shown.join(", ")));
        }
        if order.len() > MAX_VALUES {
            let rest: usize = order[MAX_VALUES..].iter().map(|v| groups[v].len()).sum();
            md.push_str(&format!("\n- and {} other values in {rest} instances", order.len() - MAX_VALUES));
        }
        if !index.complete {
            md.push_str("\n\n*The design was not elaborated to its end (a limit, a recursion cut, or a loop not evaluated): other instances may exist.*");
        }
        Some(md)
    }
}
