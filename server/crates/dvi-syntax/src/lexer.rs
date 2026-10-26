//! SystemVerilog lexer.
//!
//! Comments and whitespace are dropped; each token records whether a newline or whitespace
//! preceded it, which the preprocessor needs to find the end of `` `define `` bodies and of
//! line-oriented directives. A backslash followed by a newline is a line continuation and does
//! not count as a newline.

use crate::span::{FileId, Span};
use crate::token::{flags, is_keyword, TokKind, Token};
use smol_str::SmolStr;

/// Operators, longest first so that the first match is the longest one.
const PUNCTS: &[&str] = &[
    "<<<=", ">>>=", "===", "!==", "==?", "!=?", "<<<", ">>>", "<<=", ">>=", "<->", "|->", "|=>",
    "->>", "#-#", "#=#", "&&&", "**", "==", "!=", "<=", ">=", "&&", "||", "<<", ">>", "++", "--",
    "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "~&", "~|", "~^", "^~", "->", "::", "##",
    "@@", ":=", ":/", "+:", "-:", ".*", "(*", "*)", "+", "-", "*", "/", "%", "=", "<", ">", "!",
    "~", "&", "|", "^", "?", ":", ";", ",", ".", "(", ")", "[", "]", "{", "}", "#", "@", "'",
];

pub fn lex(text: &str, file: FileId) -> Vec<Token> {
    // About one token per six bytes of source: room for them at once, rather than copying a
    // growing vector of a large file over and over (bounded: a huge comment has no tokens).
    let mut lx = Lexer::new(text, file, 0, (text.len() / 6).min(1 << 20));
    lx.run();
    shrink_if_wasteful(&mut lx.out);
    lx.out
}

/// The tokens of `new`, given those of `old` (the same file before an edit), as [`lex`] makes
/// them: only the edited lines are lexed again, and the tokens after them are those of `old`,
/// moved by the change in length.
///
/// The lexer carries one state from token to token, whether it is inside a macro body's
/// `` `"...`" `` string, and each real newline (not one in a comment or a continued line)
/// clears it. So lexing starts again at the first token after such a newline before the edit,
/// and the old tokens are taken back at the first token after the edit that has such a
/// newline before it, in both texts, and is the same token at the same place.
pub fn relex(old_text: &str, old: &[Token], new: &str, file: FileId) -> Vec<Token> {
    let (ob, nb) = (old_text.as_bytes(), new.as_bytes());
    let prefix = ob.iter().zip(nb).take_while(|(a, b)| a == b).count();
    if prefix == ob.len() && prefix == nb.len() {
        return old.to_vec();
    }
    let room = ob.len().min(nb.len()) - prefix;
    let suffix = ob.iter().rev().zip(nb.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    let delta = nb.len() as i64 - ob.len() as i64;
    // The first token to lex again: after a newline that clears the state, before the edit.
    let mut k = old.partition_point(|t| (t.span.end as usize) <= prefix);
    let floor = k.saturating_sub(4096);
    loop {
        if k == 0 {
            break;
        }
        let gap_end = old.get(k).map_or(prefix, |t| (t.span.start as usize).min(prefix));
        if clears_state(ob, old[k - 1].span.end as usize, gap_end) {
            break;
        }
        k -= 1;
        if k <= floor {
            return lex(new, file);
        }
    }
    let start = if k == 0 { 0 } else { old[k - 1].span.end as usize };
    // Room for all of them at once: the old ones and about one per six bytes inserted.
    let mut lx = Lexer::new(new, file, start, old.len() + (nb.len() - suffix).saturating_sub(prefix) / 6 + 16);
    lx.out.extend_from_slice(&old[..k]);
    let mut fl = if k == 0 { flags::NL_BEFORE } else { 0 };
    let edit_end = nb.len() - suffix;
    loop {
        let before = lx.out.len();
        if !lx.step(&mut fl) {
            break;
        }
        let t = &lx.out[before];
        if (t.span.start as usize) < edit_end || !lx.cleared_before {
            continue;
        }
        let at = (t.span.start as i64 - delta) as u32;
        let j = old.partition_point(|o| o.span.start < at);
        let Some(o) = old.get(j).filter(|o| o.span.start == at) else { continue };
        let same = o.kind == t.kind && o.text == t.text && o.flags == t.flags && o.span.end - o.span.start == t.span.end - t.span.start;
        if same && (j == 0 || clears_state(ob, old[j - 1].span.end as usize, o.span.start as usize)) {
            lx.out.truncate(before);
            lx.out.reserve(old.len() - j);
            lx.out.extend(old[j..].iter().map(|o| {
                let mut t = o.clone();
                t.span.start = (t.span.start as i64 + delta) as u32;
                t.span.end = (t.span.end as i64 + delta) as u32;
                t
            }));
            break;
        }
    }
    shrink_if_wasteful(&mut lx.out);
    lx.out
}

/// Whether the text between two tokens holds a newline that clears the lexer's state: one
/// outside comments and not continuing a line (see [`Lexer::step`]).
fn clears_state(src: &[u8], from: usize, to: usize) -> bool {
    let mut i = from;
    while i < to {
        match src[i] {
            b'\n' => return true,
            b'\\' if src.get(i + 1) == Some(&b'\n') => i += 2,
            b'\\' if src.get(i + 1) == Some(&b'\r') && src.get(i + 2) == Some(&b'\n') => i += 3,
            b'/' if src.get(i + 1) == Some(&b'/') => {
                while i < src.len() && src[i] != b'\n' {
                    i += 1;
                }
                // `// comment \` goes on to the next line.
                let mut e = i;
                while e > 0 && src[e - 1] == b'\r' {
                    e -= 1;
                }
                if e > 0 && src[e - 1] == b'\\' && i < src.len() {
                    i += 1;
                }
            }
            b'/' if src.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < src.len() && !(src[i] == b'*' && src.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    false
}

/// Gives back what a vector reserved and did not use, when that is worth a copy.
pub(crate) fn shrink_if_wasteful<T>(v: &mut Vec<T>) {
    let spare = v.capacity() - v.len();
    if spare > 4096 && spare > v.len() / 8 {
        v.shrink_to_fit();
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    file: FileId,
    out: Vec<Token>,
    /// Between `` `" `` and the closing `` `" `` of a macro body on one logical line.
    in_macro_str: bool,
    /// Identifiers too long to be stored inline, once each: the others share the string (as
    /// the long names of a register model come again and again).
    long: rustc_hash::FxHashMap<&'a str, SmolStr>,
    /// Large texts only: a short one (a request's line, a define) gains nothing from it.
    intern: bool,
    /// A newline that clears `in_macro_str` came since the last token (or the text begins).
    cleared: bool,
    /// `cleared` as it was before the last token.
    cleared_before: bool,
}

#[inline]
fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

#[inline]
fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str, file: FileId, pos: usize, capacity: usize) -> Lexer<'a> {
        Lexer {
            src: text.as_bytes(),
            text,
            pos,
            file,
            out: Vec::with_capacity(capacity),
            in_macro_str: false,
            intern: text.len() > 1 << 16,
            long: Default::default(),
            cleared: pos == 0,
            cleared_before: false,
        }
    }

    fn peek(&self, n: usize) -> u8 {
        *self.src.get(self.pos + n).unwrap_or(&0)
    }

    /// The text of an identifier: shared with the earlier ones of the same name when it is
    /// too long to be stored inline.
    fn ident(&mut self, s: &'a str) -> SmolStr {
        if s.len() <= 23 || !self.intern {
            return SmolStr::new(s);
        }
        self.long.entry(s).or_insert_with(|| SmolStr::new(s)).clone()
    }

    fn push(&mut self, kind: TokKind, start: usize, text: SmolStr, fl: u8) {
        self.out.push(Token {
            kind,
            text,
            span: Span::new(self.file, start as u32, self.pos as u32),
            flags: fl,
        });
    }

    fn run(&mut self) {
        let mut fl: u8 = flags::NL_BEFORE;
        while self.step(&mut fl) {}
    }

    /// Lexes up to the next token, with `fl` the flags gathered for it so far; false at the
    /// end of the text.
    fn step(&mut self, fl: &mut u8) -> bool {
        while self.pos < self.src.len() {
            let c = self.peek(0);
            match c {
                b'\n' => {
                    *fl |= flags::NL_BEFORE;
                    self.in_macro_str = false;
                    self.cleared = true;
                    self.pos += 1;
                    continue;
                }
                b' ' | b'\t' | b'\r' | 0x0b | 0x0c => {
                    *fl |= flags::WS_BEFORE;
                    self.pos += 1;
                    continue;
                }
                b'\\' if self.peek(1) == b'\n' || (self.peek(1) == b'\r' && self.peek(2) == b'\n') => {
                    // Line continuation.
                    *fl |= flags::WS_BEFORE | flags::CONT_BEFORE;
                    self.pos += if self.peek(1) == b'\r' { 3 } else { 2 };
                    continue;
                }
                // Inside `"...`" a `//` is part of the string (two `/` tokens), not a comment.
                b'/' if self.peek(1) == b'/' && !self.in_macro_str => {
                    while self.pos < self.src.len() && self.peek(0) != b'\n' {
                        self.pos += 1;
                    }
                    // `// comment \` continues a macro body on the next line.
                    let mut e = self.pos;
                    while e > 0 && self.src[e - 1] == b'\r' {
                        e -= 1;
                    }
                    if e > 0 && self.src[e - 1] == b'\\' && self.pos < self.src.len() {
                        self.pos += 1;
                        *fl |= flags::CONT_BEFORE;
                    }
                    *fl |= flags::WS_BEFORE;
                    continue;
                }
                b'/' if self.peek(1) == b'*' && !self.in_macro_str => {
                    self.pos += 2;
                    while self.pos < self.src.len() && !(self.peek(0) == b'*' && self.peek(1) == b'/') {
                        if self.peek(0) == b'\n' {
                            *fl |= flags::NL_BEFORE;
                        }
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(self.src.len());
                    *fl |= flags::WS_BEFORE;
                    continue;
                }
                _ => {}
            }
            let start = self.pos;
            self.cleared_before = self.cleared;
            self.cleared = false;
            self.lex_token(c, start, *fl);
            *fl = 0;
            return true;
        }
        false
    }

    fn slice(&self, start: usize) -> &'a str {
        &self.text[start..self.pos]
    }

    fn lex_token(&mut self, c: u8, start: usize, fl: u8) {
        if is_ident_start(c) {
            while is_ident_char(self.peek(0)) {
                self.pos += 1;
            }
            let s = self.slice(start);
            let kind = if is_keyword(s) { TokKind::Keyword } else { TokKind::Ident };
            let text = self.ident(s);
            self.push(kind, start, text, fl);
            return;
        }
        if c >= 0x80 {
            // Non-ASCII outside strings/comments: consume the whole character.
            let ch_len = self.text[start..].chars().next().map(|ch| ch.len_utf8()).unwrap_or(1);
            self.pos += ch_len;
            self.push(TokKind::Unknown, start, SmolStr::new(self.slice(start)), fl);
            return;
        }
        match c {
            b'\\' if self.in_macro_str && self.pos + 1 < self.src.len() => {
                // An escape such as `\"` inside `` `"...`" ``: kept as written, so the macro builds
                // the string `"\"%s\""` instead of an escaped identifier running to the next space.
                let ch_len = self.text[start + 1..].chars().next().map(|ch| ch.len_utf8()).unwrap_or(1);
                self.pos += 1 + ch_len;
                self.push(TokKind::Punct, start, SmolStr::new(self.slice(start)), fl);
            }
            b'\\' => {
                // Escaped identifier: up to the next whitespace.
                self.pos += 1;
                while self.pos < self.src.len() && !self.peek(0).is_ascii_whitespace() {
                    self.pos += 1;
                }
                let name = &self.text[start + 1..self.pos];
                self.push(TokKind::Ident, start, SmolStr::new(name), fl);
            }
            b'$' => {
                self.pos += 1;
                while is_ident_char(self.peek(0)) {
                    self.pos += 1;
                }
                let s = self.slice(start);
                let kind = if s.len() == 1 { TokKind::Punct } else { TokKind::SysIdent };
                self.push(kind, start, SmolStr::new(s), fl);
            }
            b'`' => {
                let n = self.peek(1);
                if n == b'`' {
                    self.pos += 2;
                    self.push(TokKind::MacroPaste, start, SmolStr::new_inline("``"), fl);
                } else if n == b'"' {
                    self.pos += 2;
                    self.in_macro_str = !self.in_macro_str;
                    self.push(TokKind::MacroQuote, start, SmolStr::new_inline("`\""), fl);
                } else if n == b'\\' && self.peek(2) == b'`' && self.peek(3) == b'"' {
                    self.pos += 4;
                    self.push(TokKind::MacroEscQuote, start, SmolStr::new_inline("`\\`\""), fl);
                } else if is_ident_start(n) {
                    self.pos += 1;
                    while is_ident_char(self.peek(0)) {
                        self.pos += 1;
                    }
                    let name = &self.text[start + 1..self.pos];
                    self.push(TokKind::Directive, start, SmolStr::new(name), fl);
                } else {
                    self.pos += 1;
                    self.push(TokKind::Unknown, start, SmolStr::new_inline("`"), fl);
                }
            }
            b'"' => self.lex_string(start, fl),
            b'0'..=b'9' => self.lex_number(start, fl),
            b'\'' => {
                let n = self.peek(1);
                let base_at = if n == b's' || n == b'S' { 2 } else { 1 };
                let b = self.peek(base_at);
                if matches!(b, b'b' | b'B' | b'o' | b'O' | b'd' | b'D' | b'h' | b'H')
                    && self.based_value_follows(base_at + 1)
                {
                    self.pos += base_at + 1;
                    self.lex_based_value();
                    self.push(TokKind::Number, start, SmolStr::new(self.slice(start)), fl);
                } else if matches!(b, b'b' | b'B' | b'o' | b'O' | b'd' | b'D' | b'h' | b'H') && self.peek(base_at + 1) == b'`' {
                    // `32'h`ADDR`: the digits come from a macro; the preprocessor joins them.
                    self.pos += base_at + 1;
                    self.push(TokKind::Number, start, SmolStr::new(self.slice(start)), fl);
                } else if matches!(n, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') && !is_ident_char(self.peek(2)) {
                    self.pos += 2;
                    self.push(TokKind::Number, start, SmolStr::new(self.slice(start)), fl);
                } else {
                    self.pos += 1;
                    self.push(TokKind::Punct, start, SmolStr::new_inline("'"), fl);
                }
            }
            _ => {
                for p in PUNCTS {
                    if self.src[self.pos..].starts_with(p.as_bytes()) {
                        // `(*` is only an attribute when not `(*)` as in `@(*)`.
                        if *p == "(*" && self.peek(2) == b')' {
                            for (i, q) in ["(", "*", ")"].into_iter().enumerate() {
                                let s = self.pos;
                                self.pos += 1;
                                self.push(TokKind::Punct, s, SmolStr::new_static(q), if i == 0 { fl } else { 0 });
                            }
                            return;
                        }
                        // `3'b0://comment`: a ':' followed by a comment, not the `:/` of `dist`.
                        if *p == ":/" && matches!(self.peek(2), b'/' | b'*') {
                            continue;
                        }
                        self.pos += p.len();
                        self.push(TokKind::Punct, start, SmolStr::new_static(p), fl);
                        return;
                    }
                }
                self.pos += 1;
                self.push(TokKind::Unknown, start, SmolStr::new(self.slice(start)), fl);
            }
        }
    }

    fn based_value_follows(&self, mut i: usize) -> bool {
        while matches!(self.peek(i), b' ' | b'\t') {
            i += 1;
        }
        let c = self.peek(i);
        c.is_ascii_hexdigit() || matches!(c, b'x' | b'X' | b'z' | b'Z' | b'?' | b'_')
    }

    fn lex_based_value(&mut self) {
        while matches!(self.peek(0), b' ' | b'\t') {
            self.pos += 1;
        }
        while {
            let c = self.peek(0);
            c.is_ascii_hexdigit() || matches!(c, b'x' | b'X' | b'z' | b'Z' | b'?' | b'_')
        } {
            self.pos += 1;
        }
    }

    fn lex_number(&mut self, start: usize, fl: u8) {
        while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
            self.pos += 1;
        }
        let mut real = false;
        if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
            real = true;
            self.pos += 1;
            while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
                self.pos += 1;
            }
        }
        if matches!(self.peek(0), b'e' | b'E')
            && (self.peek(1).is_ascii_digit()
                || (matches!(self.peek(1), b'+' | b'-') && self.peek(2).is_ascii_digit()))
        {
            real = true;
            self.pos += 2;
            while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
                self.pos += 1;
            }
        }
        // Time literal: 10ns, 1.5us, 1step.
        for unit in ["step", "fs", "ps", "ns", "us", "ms", "s"] {
            if self.src[self.pos..].starts_with(unit.as_bytes())
                && !is_ident_char(self.peek(unit.len()))
            {
                self.pos += unit.len();
                self.push(TokKind::Number, start, SmolStr::new(self.slice(start)), fl);
                return;
            }
        }
        if !real {
            // Size followed by a based value, possibly separated by spaces: 8'hFF, 4 'b1010.
            let mut i = 0;
            while matches!(self.peek(i), b' ' | b'\t') {
                i += 1;
            }
            if self.peek(i) == b'\'' {
                let base_at = if matches!(self.peek(i + 1), b's' | b'S') { i + 2 } else { i + 1 };
                if matches!(self.peek(base_at), b'b' | b'B' | b'o' | b'O' | b'd' | b'D' | b'h' | b'H')
                    && self.based_value_follows(base_at + 1)
                {
                    self.pos += base_at + 1;
                    self.lex_based_value();
                }
            }
        }
        self.push(TokKind::Number, start, SmolStr::new(self.slice(start)), fl);
    }

    fn lex_string(&mut self, start: usize, fl: u8) {
        // Triple-quoted strings (1800-2023) may span lines.
        if self.peek(1) == b'"' && self.peek(2) == b'"' {
            self.pos += 3;
            while self.pos < self.src.len() && !(self.peek(0) == b'"' && self.peek(1) == b'"' && self.peek(2) == b'"') {
                if self.peek(0) == b'\\' {
                    self.pos += 1;
                }
                self.pos += 1;
            }
            self.pos = (self.pos + 3).min(self.src.len());
        } else {
            self.pos += 1;
            while self.pos < self.src.len() {
                match self.peek(0) {
                    b'\\' => {
                        // Escapes, including an escaped newline (LF or CRLF) that continues the string.
                        self.pos += if self.peek(1) == b'\r' && self.peek(2) == b'\n' { 3 } else { 2 };
                    }
                    b'"' => {
                        self.pos += 1;
                        break;
                    }
                    b'\n' => break,
                    _ => self.pos += 1,
                }
            }
            self.pos = self.pos.min(self.src.len());
        }
        let s = &self.text[start..self.pos.min(self.text.len())];
        self.push(TokKind::String, start, SmolStr::new(s), fl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &str) -> Vec<(TokKind, String)> {
        lex(s, 0).into_iter().map(|t| (t.kind, t.text.to_string())).collect()
    }

    #[test]
    fn numbers_and_casts() {
        let k = kinds("8'hFF 4 'b10_1x 'd5 '0 int'(x) 8'(y) 1.5e3 10ns 3");
        let texts: Vec<_> = k.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            vec!["8'hFF", "4 'b10_1x", "'d5", "'0", "int", "'", "(", "x", ")", "8", "'", "(", "y", ")", "1.5e3", "10ns", "3"]
        );
    }

    #[test]
    fn directives_and_macro_tokens() {
        let k = kinds("`define S(x) `\"x`\" a``b \\esc$id rest");
        assert_eq!(k[0], (TokKind::Directive, "define".into()));
        assert_eq!(k[1], (TokKind::Ident, "S".into()));
        assert!(k.iter().any(|(kd, _)| *kd == TokKind::MacroQuote));
        assert!(k.iter().any(|(kd, _)| *kd == TokKind::MacroPaste));
        assert!(k.iter().any(|(kd, t)| *kd == TokKind::Ident && t == "esc$id"));
    }

    #[test]
    fn continuation_is_not_newline() {
        let toks = lex("a \\\n b\nc", 0);
        assert!(!toks[1].nl_before());
        assert!(toks[2].nl_before());
    }

    /// Random edits of text made of the pieces the lexer treats specially: each relexed text
    /// gives the tokens of a full lex.
    #[test]
    fn relexing_an_edit_gives_the_tokens_of_a_full_lex() {
        const PIECES: &[&str] = &[
            "module m;\n", "  logic [7:0] a;\n", "/* block\n comment */", "// line comment\n", "// continued \\\n",
            "`define S(x) `\"x is \\\"x\\\"`\" \\\n  + 1\n", "`\"", "\"str\\\"ing\"", "\"\"\"tri\nple\"\"\"", "8 'hFF", "4", " 'b10",
            "\\esc$id ", "a``b", "@(*)", "(* attr *)", "\n", "\r\n", " ", "\t", "x = y;", "end", "/*", "*/", "\"", "\\", "\\\n", "é", "1.5e3", "10ns",
        ];
        let mut seed: u64 = 0x5eed_1234_abcd;
        let mut rnd = |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % n.max(1)
        };
        for _ in 0..300 {
            let mut text = String::new();
            for _ in 0..rnd(80) {
                text.push_str(PIECES[rnd(PIECES.len())]);
            }
            let mut toks = lex(&text, 3);
            for _ in 0..20 {
                // Delete a range and insert pieces, on character boundaries.
                let bounds: Vec<usize> = (0..=text.len()).filter(|i| text.is_char_boundary(*i)).collect();
                let ia = rnd(bounds.len());
                let a = bounds[ia];
                let b = bounds[ia..].iter().copied().nth(rnd(8)).unwrap_or(text.len());
                let mut ins = String::new();
                for _ in 0..rnd(4) {
                    ins.push_str(PIECES[rnd(PIECES.len())]);
                }
                let new = format!("{}{}{}", &text[..a], ins, &text[b..]);
                let re = relex(&text, &toks, &new, 3);
                let full = lex(&new, 3);
                assert_eq!(re, full, "old {text:?}\nnew {new:?}");
                text = new;
                toks = re;
            }
        }
    }

    /// `DVICODE_RELEX_BENCH=file cargo test --release -p dvi-syntax relex_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn relex_bench() {
        let Ok(path) = std::env::var("DVICODE_RELEX_BENCH") else { return };
        let text = std::fs::read_to_string(path).unwrap();
        let t0 = std::time::Instant::now();
        let toks = lex(&text, 0);
        let full = t0.elapsed();
        let at = text.as_bytes()[..text.len() / 2].iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let new = format!("{}    x = 1;\n{}", &text[..at], &text[at..]);
        let t1 = std::time::Instant::now();
        let re = relex(&text, &toks, &new, 0);
        let part = t1.elapsed();
        assert_eq!(re, lex(&new, 0));
        println!("{} tokens: lex {full:?}, relex of a line {part:?}", toks.len());
    }

    #[test]
    fn attributes_vs_star_event() {
        let k = kinds("@(*) (* full_case *)");
        let texts: Vec<_> = k.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, vec!["@", "(", "*", ")", "(*", "full_case", "*)"]);
    }
}
