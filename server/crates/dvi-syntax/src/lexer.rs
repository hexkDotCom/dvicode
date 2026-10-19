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
    let mut lx = Lexer { src: text.as_bytes(), text, pos: 0, file, out: Vec::new(), in_macro_str: false };
    lx.run();
    lx.out
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    file: FileId,
    out: Vec<Token>,
    /// Between `` `" `` and the closing `` `" `` of a macro body on one logical line.
    in_macro_str: bool,
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
    fn peek(&self, n: usize) -> u8 {
        *self.src.get(self.pos + n).unwrap_or(&0)
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
        while self.pos < self.src.len() {
            let c = self.peek(0);
            match c {
                b'\n' => {
                    fl |= flags::NL_BEFORE;
                    self.in_macro_str = false;
                    self.pos += 1;
                    continue;
                }
                b' ' | b'\t' | b'\r' | 0x0b | 0x0c => {
                    fl |= flags::WS_BEFORE;
                    self.pos += 1;
                    continue;
                }
                b'\\' if self.peek(1) == b'\n' || (self.peek(1) == b'\r' && self.peek(2) == b'\n') => {
                    // Line continuation.
                    fl |= flags::WS_BEFORE;
                    self.pos += if self.peek(1) == b'\r' { 3 } else { 2 };
                    continue;
                }
                b'/' if self.peek(1) == b'/' => {
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
                    }
                    fl |= flags::WS_BEFORE;
                    continue;
                }
                b'/' if self.peek(1) == b'*' => {
                    self.pos += 2;
                    while self.pos < self.src.len() && !(self.peek(0) == b'*' && self.peek(1) == b'/') {
                        if self.peek(0) == b'\n' {
                            fl |= flags::NL_BEFORE;
                        }
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(self.src.len());
                    fl |= flags::WS_BEFORE;
                    continue;
                }
                _ => {}
            }
            let start = self.pos;
            self.lex_token(c, start, fl);
            fl = 0;
        }
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
            self.push(kind, start, SmolStr::new(s), fl);
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
                        // Escapes, including an escaped newline that continues the string.
                        self.pos += 2;
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

    #[test]
    fn attributes_vs_star_event() {
        let k = kinds("@(*) (* full_case *)");
        let texts: Vec<_> = k.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, vec!["@", "(", "*", ")", "(*", "full_case", "*)"]);
    }
}
