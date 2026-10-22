//! The statement structure of a method body, from its tokens: for each token, the conditions
//! and loops around it (`if`, `case`, `for`, ...). Used by the Verification Hierarchy to tell
//! which creations are conditional or repeated.

use dvi_syntax::{TokKind, Token};

/// A condition or loop around statements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub parent: Option<u32>,
    pub kind: FrameKind,
}

/// Token ranges are `start..end` indexes into the walked tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// The branch of an `if` (or an assertion's action) taken when `cond` is true, or false
    /// (`negated`: the `else` branch).
    If { cond: (usize, usize), negated: bool },
    /// A `case` item: the case expression and the item's labels (`None`: `default`).
    Case { expr: (usize, usize), labels: Option<(usize, usize)> },
    /// A loop: its keyword, its header (inside the parentheses; empty for `forever`) and its body.
    Loop { kind: &'static str, header: (usize, usize), body: (usize, usize) },
}

/// The frames of a body and, for each token, the innermost frame around it.
#[derive(Clone, Debug, Default)]
pub struct StmtMap {
    pub frames: Vec<Frame>,
    /// `NONE` when the token is in no frame.
    pub at: Vec<u32>,
}

pub const NONE: u32 = u32::MAX;

impl StmtMap {
    /// The frames around token `k`, outermost first.
    pub fn frames_at(&self, k: usize) -> Vec<&Frame> {
        self.frame_ids_at(k).into_iter().map(|i| &self.frames[i as usize]).collect()
    }

    /// The indexes of the frames around token `k`, outermost first.
    pub fn frame_ids_at(&self, k: usize) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = self.at.get(k).copied().unwrap_or(NONE);
        while cur != NONE {
            out.push(cur);
            cur = self.frames[cur as usize].parent.unwrap_or(NONE);
        }
        out.reverse();
        out
    }
}

/// Walks the tokens of a function or task (header to `endfunction`/`endtask`). `None` when the
/// body uses something the walker cannot follow: then nothing is known about its conditions.
pub fn walk_method(t: &[Token]) -> Option<StmtMap> {
    let mut w = Walker { t, frames: Vec::new(), at: vec![NONE; t.len()], steps: 0, depth: 0 };
    // The header ends at the first `;` outside brackets after `function` or `task`.
    let kw = t.iter().position(|x| x.is_kw("function") || x.is_kw("task"))?;
    let mut k = kw;
    while k < t.len() && !t[k].is_punct(";") {
        k = if t[k].is_punct("(") || t[k].is_punct("[") || t[k].is_punct("{") { w.group(k)? } else { k + 1 };
    }
    k += 1;
    loop {
        let x = t.get(k)?;
        if x.is_kw("endfunction") || x.is_kw("endtask") {
            return Some(StmtMap { frames: w.frames, at: w.at });
        }
        k = w.stmt(k, NONE)?;
    }
}

/// The index after the statement that starts at `t[k]` (`begin ... end`, `if ... else ...`, an
/// assignment...); `None` when it cannot be followed.
pub fn statement_end(t: &[Token], k: usize) -> Option<usize> {
    let mut w = Walker { t, frames: Vec::new(), at: vec![NONE; t.len()], steps: 0, depth: 0 };
    w.stmt(k, NONE)
}

struct Walker<'a> {
    t: &'a [Token],
    frames: Vec<Frame>,
    at: Vec<u32>,
    /// Statements walked, and statements nested; bounds against pathological input.
    steps: usize,
    depth: usize,
}

/// Statements nested at most (the parser's limit is similar).
const MAX_DEPTH: usize = 200;

/// Keywords that end a statement list; met where a statement should start, the body is not
/// what the walker expects.
const ENDS: &[&str] = &["end", "endcase", "join", "join_any", "join_none", "endfunction", "endtask", "else", "endmodule", "endclass", "endpackage"];

impl Walker<'_> {
    fn frame(&mut self, parent: u32, kind: FrameKind) -> u32 {
        self.frames.push(Frame { parent: (parent != NONE).then_some(parent), kind });
        (self.frames.len() - 1) as u32
    }

    fn mark(&mut self, from: usize, to: usize, f: u32) {
        for k in from..to.min(self.at.len()) {
            self.at[k] = f;
        }
    }

    /// Index after the bracket group opened at `open`; `None` when it is not closed.
    fn group(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;
        for (k, x) in self.t.iter().enumerate().skip(open) {
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
                    if depth < 0 {
                        return None;
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// A parenthesized group at `k`: the range inside it and the index after it.
    fn parens(&self, k: usize) -> Option<((usize, usize), usize)> {
        if !self.t.get(k)?.is_punct("(") {
            return None;
        }
        let end = self.group(k)?;
        Some(((k + 1, end - 1), end))
    }

    /// One statement starting at `k`, in frame `f`; the index after it.
    fn stmt(&mut self, k: usize, f: u32) -> Option<usize> {
        self.steps += 1;
        if self.steps > 200_000 || self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let r = self.stmt_inner(k, f);
        self.depth -= 1;
        r
    }

    fn stmt_inner(&mut self, k: usize, f: u32) -> Option<usize> {
        let t = self.t;
        let mut k = k;
        // Attributes: `(* keep = 1 *) if (...)`.
        while t.get(k).is_some_and(|x| x.is_punct("(*")) {
            let end = (k + 1..t.len()).find(|&j| t[j].is_punct("*)"))?;
            self.mark(k, end + 1, f);
            k = end + 1;
        }
        let x = t.get(k)?;
        if x.kind == TokKind::Keyword && ENDS.contains(&x.text.as_str()) {
            return None;
        }
        // Statement label: `name: stmt`.
        if x.is_ident() && t.get(k + 1).is_some_and(|y| y.is_punct(":")) {
            self.mark(k, k + 2, f);
            return self.stmt(k + 2, f);
        }
        if x.kind == TokKind::Directive {
            return self.directive(k, f);
        }
        if x.kind == TokKind::Keyword {
            match x.text.as_str() {
                "begin" | "fork" => return self.block(k, f),
                "unique" | "unique0" | "priority" => {
                    self.mark(k, k + 1, f);
                    return self.stmt(k + 1, f);
                }
                "if" => return self.if_stmt(k, f),
                "case" | "casez" | "casex" => return self.case_stmt(k, f),
                "randcase" => return self.randcase(k, f),
                "for" | "foreach" | "repeat" | "while" => {
                    let (header, after) = self.parens(k + 1)?;
                    self.mark(k, after, f);
                    let fl = self.frame(f, FrameKind::Loop { kind: kw_name(&x.text), header, body: (after, after) });
                    let end = self.stmt(after, fl)?;
                    self.set_body(fl, (after, end));
                    return Some(end);
                }
                "forever" => {
                    self.mark(k, k + 1, f);
                    let fl = self.frame(f, FrameKind::Loop { kind: "forever", header: (k + 1, k + 1), body: (k + 1, k + 1) });
                    let end = self.stmt(k + 1, fl)?;
                    self.set_body(fl, (k + 1, end));
                    return Some(end);
                }
                "do" => {
                    self.mark(k, k + 1, f);
                    let fl = self.frame(f, FrameKind::Loop { kind: "do", header: (k, k), body: (k + 1, k + 1) });
                    let end = self.stmt(k + 1, fl)?;
                    if !t.get(end)?.is_kw("while") {
                        return None;
                    }
                    let (header, after) = self.parens(end + 1)?;
                    self.frames[fl as usize].kind = FrameKind::Loop { kind: "do", header, body: (k + 1, end) };
                    if !t.get(after)?.is_punct(";") {
                        return None;
                    }
                    self.mark(end, after + 1, f);
                    return Some(after + 1);
                }
                "wait" => {
                    if t.get(k + 1).is_some_and(|y| y.is_kw("fork")) {
                        return self.expr_stmt(k, f);
                    }
                    let (_, after) = self.parens(k + 1)?;
                    self.mark(k, after, f);
                    return self.stmt(after, f);
                }
                "assert" | "assume" | "cover" | "restrict" | "expect" => return self.assertion(k, f),
                "randsequence" => return None,
                _ => {}
            }
        }
        // Timing controls before a statement: `@(e)`, `@e`, `@*`, `#5`, `#(d)`, `##1`.
        if x.is_punct("@") || x.is_punct("@*") {
            let mut j = k + 1;
            if x.is_punct("@") {
                let y = t.get(j)?;
                if y.is_punct("(") {
                    j = self.group(j)?;
                } else if y.is_punct("*") {
                    j += 1;
                } else {
                    while t.get(j).is_some_and(|z| z.is_ident()) {
                        j += 1;
                        if t.get(j).is_some_and(|z| z.is_punct(".")) {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                }
            }
            self.mark(k, j, f);
            return self.stmt(j, f);
        }
        if x.is_punct("#") || x.is_punct("##") {
            let y = t.get(k + 1)?;
            let j = if y.is_punct("(") { self.group(k + 1)? } else { k + 2 };
            // `#1step`, `#10ns`: the unit is part of the number or a following identifier.
            let j = if t.get(j).is_some_and(|z| z.is_ident() && !z.ws_before() && matches!(z.text.as_str(), "s" | "ms" | "us" | "ns" | "ps" | "fs" | "step")) { j + 1 } else { j };
            self.mark(k, j, f);
            return self.stmt(j, f);
        }
        self.expr_stmt(k, f)
    }

    fn set_body(&mut self, fl: u32, body: (usize, usize)) {
        if let FrameKind::Loop { body: b, .. } = &mut self.frames[fl as usize].kind {
            *b = body;
        }
    }

    /// Up to the `;` outside brackets.
    fn expr_stmt(&mut self, k: usize, f: u32) -> Option<usize> {
        let t = self.t;
        let mut j = k;
        loop {
            let x = t.get(j)?;
            if x.is_punct(";") {
                self.mark(k, j + 1, f);
                return Some(j + 1);
            }
            if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
                j = self.group(j)?;
                continue;
            }
            if x.kind == TokKind::Keyword && (ENDS.contains(&x.text.as_str()) || matches!(x.text.as_str(), "begin" | "fork")) {
                return None;
            }
            j += 1;
        }
    }

    /// `begin [: l] ... end [: l]`, `fork ... join*`.
    fn block(&mut self, k: usize, f: u32) -> Option<usize> {
        let t = self.t;
        let fork = t[k].is_kw("fork");
        let mut j = self.label(k + 1);
        self.mark(k, j, f);
        loop {
            let x = t.get(j)?;
            let closes = if fork { x.is_kw("join") || x.is_kw("join_any") || x.is_kw("join_none") } else { x.is_kw("end") };
            if closes {
                let after = self.label(j + 1);
                self.mark(j, after, f);
                return Some(after);
            }
            j = self.stmt(j, f)?;
        }
    }

    /// After an optional `: label` at `k`.
    fn label(&self, k: usize) -> usize {
        if self.t.get(k).is_some_and(|x| x.is_punct(":")) && self.t.get(k + 1).is_some_and(|x| x.is_ident()) {
            k + 2
        } else {
            k
        }
    }

    fn if_stmt(&mut self, k: usize, f: u32) -> Option<usize> {
        let (cond, after) = self.parens(k + 1)?;
        self.mark(k, after, f);
        let ft = self.frame(f, FrameKind::If { cond, negated: false });
        let end = self.stmt(after, ft)?;
        if self.t.get(end).is_some_and(|x| x.is_kw("else")) {
            self.mark(end, end + 1, f);
            let fe = self.frame(f, FrameKind::If { cond, negated: true });
            return self.stmt(end + 1, fe);
        }
        Some(end)
    }

    /// `assert (c) pass [else fail];` (also `assert property`, `assert final`, `assert #0`).
    fn assertion(&mut self, k: usize, f: u32) -> Option<usize> {
        let t = self.t;
        let mut j = k + 1;
        if t.get(j).is_some_and(|x| x.is_kw("property") || x.is_kw("final") || x.is_kw("sequence")) {
            j += 1;
        } else if t.get(j).is_some_and(|x| x.is_punct("#")) {
            j += 2;
        }
        let (cond, after) = self.parens(j)?;
        self.mark(k, after, f);
        if t.get(after)?.is_punct(";") {
            self.mark(after, after + 1, f);
            return Some(after + 1);
        }
        let mut end = after;
        if !t.get(after)?.is_kw("else") {
            let ft = self.frame(f, FrameKind::If { cond, negated: false });
            end = self.stmt(after, ft)?;
        }
        if t.get(end).is_some_and(|x| x.is_kw("else")) {
            self.mark(end, end + 1, f);
            let fe = self.frame(f, FrameKind::If { cond, negated: true });
            return self.stmt(end + 1, fe);
        }
        Some(end)
    }

    /// `case (e) [inside|matches] labels: stmt ... default[:] stmt endcase`.
    fn case_stmt(&mut self, k: usize, f: u32) -> Option<usize> {
        let t = self.t;
        let (expr, mut j) = self.parens(k + 1)?;
        if t.get(j).is_some_and(|x| x.is_kw("inside") || x.is_kw("matches")) {
            j += 1;
        }
        self.mark(k, j, f);
        loop {
            let x = t.get(j)?;
            if x.is_kw("endcase") {
                self.mark(j, j + 1, f);
                return Some(j + 1);
            }
            let (labels, body) = if x.is_kw("default") {
                let b = if t.get(j + 1).is_some_and(|y| y.is_punct(":")) { j + 2 } else { j + 1 };
                (None, b)
            } else {
                let colon = self.item_colon(j)?;
                (Some((j, colon)), colon + 1)
            };
            self.mark(j, body, f);
            let fi = self.frame(f, FrameKind::Case { expr, labels });
            j = self.stmt(body, fi)?;
        }
    }

    /// `randcase weight: stmt ... endcase`: each item is taken with some probability.
    fn randcase(&mut self, k: usize, f: u32) -> Option<usize> {
        let t = self.t;
        let mut j = k + 1;
        self.mark(k, j, f);
        loop {
            let x = t.get(j)?;
            if x.is_kw("endcase") {
                self.mark(j, j + 1, f);
                return Some(j + 1);
            }
            let colon = self.item_colon(j)?;
            self.mark(j, colon + 1, f);
            let fi = self.frame(f, FrameKind::Case { expr: (k, k + 1), labels: Some((j, colon)) });
            j = self.stmt(colon + 1, fi)?;
        }
    }

    /// The `:` ending a case item's labels starting at `k` (outside brackets, and not the `:`
    /// of a `? :`).
    fn item_colon(&self, k: usize) -> Option<usize> {
        let mut j = k;
        let mut ternaries = 0;
        loop {
            let x = self.t.get(j)?;
            if x.is_punct("?") {
                ternaries += 1;
            } else if x.is_punct(":") {
                if ternaries == 0 {
                    return Some(j);
                }
                ternaries -= 1;
            }
            if x.is_punct(";") || x.kind == TokKind::Keyword && ENDS.contains(&x.text.as_str()) {
                return None;
            }
            j = if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") { self.group(j)? } else { j + 1 };
        }
    }

    /// A directive where a statement starts: conditional compilation (the code it leaves out is
    /// already gone), or a macro call used as a statement, with or without a `;`.
    fn directive(&mut self, k: usize, f: u32) -> Option<usize> {
        let t = self.t;
        let x = &t[k];
        let j = match x.text.as_str() {
            "ifdef" | "ifndef" | "elsif" | "undef" => k + 2,
            "else" | "endif" | "resetall" | "celldefine" | "endcelldefine" | "nounconnected_drive" => k + 1,
            "include" | "line" | "timescale" | "default_nettype" | "unconnected_drive" | "pragma" | "begin_keywords" | "end_keywords" | "define" => {
                // To the end of the line.
                let mut j = k + 1;
                while t.get(j).is_some_and(|y| !y.nl_before()) {
                    j += 1;
                }
                j
            }
            _ => {
                let mut j = k + 1;
                if t.get(j).is_some_and(|y| y.is_punct("(") && !y.ws_before()) {
                    j = self.group(j)?;
                }
                match t.get(j) {
                    Some(y) if y.is_punct(";") => j + 1,
                    // The next statement starts on a new line.
                    Some(y) if y.nl_before() => j,
                    None => j,
                    // Part of an expression (`` `CFG.x = 1; ``).
                    Some(_) => return self.expr_stmt(k, f),
                }
            }
        };
        self.mark(k, j, f);
        Some(j)
    }
}

fn kw_name(s: &str) -> &'static str {
    match s {
        "for" => "for",
        "foreach" => "foreach",
        "repeat" => "repeat",
        _ => "while",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(s: &str) -> Vec<Token> {
        dvi_syntax::lexer::lex(s, 0).into_iter().filter(|t| t.kind != TokKind::Eof).collect()
    }

    fn text(t: &[Token], r: (usize, usize)) -> String {
        t[r.0..r.1].iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// The frames around the token `needle`, as text.
    fn around(src: &str, needle: &str) -> Vec<String> {
        let t = lex(src);
        let m = walk_method(&t).expect("walked");
        let k = t.iter().position(|x| x.text == needle).expect("needle");
        m.frames_at(k)
            .iter()
            .map(|f| match &f.kind {
                FrameKind::If { cond, negated } => format!("{}{}", if *negated { "!" } else { "" }, text(&t, *cond)),
                FrameKind::Case { expr, labels } => format!("case {}: {}", text(&t, *expr), labels.map_or("default".into(), |l| text(&t, l))),
                FrameKind::Loop { kind, header, .. } => format!("{kind} {}", text(&t, *header)),
            })
            .collect()
    }

    fn body(s: &str) -> String {
        format!("function void build_phase(uvm_phase phase);\n{s}\nendfunction")
    }

    #[test]
    fn if_else_chains() {
        let src = body("if (a) x = 1; else if (b) y = 2; else begin z = 3; end w = 4;");
        assert_eq!(around(&src, "x"), ["a"]);
        assert_eq!(around(&src, "y"), ["!a", "b"]);
        assert_eq!(around(&src, "z"), ["!a", "!b"]);
        assert!(around(&src, "w").is_empty());
    }

    #[test]
    fn dangling_else_binds_to_the_nearest_if() {
        let src = body("if (a) if (b) x = 1; else y = 2;");
        assert_eq!(around(&src, "y"), ["a", "!b"]);
    }

    #[test]
    fn case_items_and_default() {
        let src = body("unique case (mode) A, B: x = 1; [3:4]: begin y = 2; end default z = 3; endcase");
        assert_eq!(around(&src, "x"), ["case mode: A , B"]);
        assert_eq!(around(&src, "y"), ["case mode: [ 3 : 4 ]"]);
        assert_eq!(around(&src, "z"), ["case mode: default"]);
    }

    #[test]
    fn loops_labels_and_timing() {
        let src = body("lbl: for (int i = 0; i < 4; i++) begin : blk foreach (a[j]) @(posedge clk) #5 x = 1; end : blk\nrepeat (3) y = 1; forever begin z = 1; end do q = 1; while (c);");
        assert_eq!(around(&src, "x"), ["for int i = 0 ; i < 4 ; i ++", "foreach a [ j ]"]);
        assert_eq!(around(&src, "y"), ["repeat 3"]);
        assert_eq!(around(&src, "z"), ["forever "]);
        assert_eq!(around(&src, "q"), ["do c"]);
    }

    #[test]
    fn macros_with_and_without_semicolons() {
        let src = body("`uvm_info(\"a\", \"b\", UVM_LOW)\nif (c) `uvm_create(s)\nx = 1;\n`ifdef FOO\ny = 2;\n`endif\nfork z = 1; join_none");
        assert!(around(&src, "x").is_empty());
        assert_eq!(around(&src, "s"), ["c"]);
        assert!(around(&src, "y").is_empty());
        assert!(around(&src, "z").is_empty());
    }

    #[test]
    fn assertion_actions() {
        let src = body("assert (ok) else x = 1;\nassert (ok2) y = 1; else z = 1;");
        assert_eq!(around(&src, "x"), ["!ok"]);
        assert_eq!(around(&src, "y"), ["ok2"]);
        assert_eq!(around(&src, "z"), ["!ok2"]);
    }

    #[test]
    fn attributes_and_ternary_labels() {
        let src = body("(* keep = 1 *) if (mode) x = 1;
case (sel) p ? A : B: y = 1; p ? A : C: z = 1; endcase");
        assert_eq!(around(&src, "x"), ["mode"]);
        assert_eq!(around(&src, "y"), ["case sel: p ? A : B"]);
        assert_eq!(around(&src, "z"), ["case sel: p ? A : C"]);
    }

    #[test]
    fn deep_nesting_is_not_walked() {
        let deep = format!("{}x = 1;{}", "begin ".repeat(5000), " end".repeat(5000));
        assert!(walk_method(&lex(&body(&deep))).is_none());
    }

    #[test]
    fn unbalanced_bodies_are_not_walked() {
        assert!(walk_method(&lex(&body("if (a) begin x = 1;"))).is_none());
        assert!(walk_method(&lex(&body("x = (1;"))).is_none());
        assert!(walk_method(&lex(&body("randsequence (main) main: a; endsequence"))).is_none());
    }
}
