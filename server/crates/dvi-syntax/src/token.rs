//! Tokens and the SystemVerilog keyword table.

use crate::span::Span;
use rustc_hash::FxHashSet;
use smol_str::SmolStr;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokKind {
    Ident,
    Keyword,
    /// `$display`, `$root`, ...
    SysIdent,
    /// `` `name ``; the text holds the name without the backtick.
    Directive,
    Number,
    String,
    Punct,
    /// `` `` `` inside macro text.
    MacroPaste,
    /// `` `" `` inside macro text.
    MacroQuote,
    /// `` `\`" `` inside macro text.
    MacroEscQuote,
    Unknown,
    Eof,
}

pub mod flags {
    /// An unescaped newline separates this token from the previous one.
    pub const NL_BEFORE: u8 = 1;
    /// Whitespace or a comment separates this token from the previous one.
    pub const WS_BEFORE: u8 = 2;
    /// Produced by a macro expansion; the span is the macro call site.
    pub const FROM_MACRO: u8 = 4;
    /// Came from a macro argument; the span is the real location in the argument text.
    pub const MACRO_ARG: u8 = 8;
    /// A `` `name `` built by token pasting (`` `dv_``SEV_ ``).
    pub const PASTED: u8 = 16;
    /// A line continuation (backslash, newline) comes before this token: a line break of the
    /// macro body it belongs to.
    pub const CONT_BEFORE: u8 = 32;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokKind,
    pub text: SmolStr,
    pub span: Span,
    pub flags: u8,
}

impl Token {
    pub fn eof(span: Span) -> Token {
        Token { kind: TokKind::Eof, text: SmolStr::default(), span, flags: flags::NL_BEFORE }
    }

    #[inline]
    pub fn is_kw(&self, kw: &str) -> bool {
        self.kind == TokKind::Keyword && self.text == kw
    }

    #[inline]
    pub fn is_punct(&self, p: &str) -> bool {
        self.kind == TokKind::Punct && self.text == p
    }

    #[inline]
    pub fn is_ident(&self) -> bool {
        self.kind == TokKind::Ident
    }

    #[inline]
    pub fn nl_before(&self) -> bool {
        self.flags & flags::NL_BEFORE != 0
    }

    #[inline]
    pub fn ws_before(&self) -> bool {
        self.flags & (flags::WS_BEFORE | flags::NL_BEFORE) != 0
    }

    /// True for tokens written by the user at their span, including macro arguments.
    #[inline]
    pub fn is_real(&self) -> bool {
        self.flags & flags::FROM_MACRO == 0 || self.flags & flags::MACRO_ARG != 0
    }
}

/// IEEE 1800-2017 reserved words (Annex B).
pub const KEYWORDS: &[&str] = &[
    "accept_on", "alias", "always", "always_comb", "always_ff", "always_latch", "and", "assert",
    "assign", "assume", "automatic", "before", "begin", "bind", "bins", "binsof", "bit", "break",
    "buf", "bufif0", "bufif1", "byte", "case", "casex", "casez", "cell", "chandle", "checker",
    "class", "clocking", "cmos", "config", "const", "constraint", "context", "continue", "cover",
    "covergroup", "coverpoint", "cross", "deassign", "default", "defparam", "design", "disable",
    "dist", "do", "edge", "else", "end", "endcase", "endchecker", "endclass", "endclocking",
    "endconfig", "endfunction", "endgenerate", "endgroup", "endinterface", "endmodule",
    "endpackage", "endprimitive", "endprogram", "endproperty", "endspecify", "endsequence",
    "endtable", "endtask", "enum", "event", "eventually", "expect", "export", "extends", "extern",
    "final", "first_match", "for", "force", "foreach", "forever", "fork", "forkjoin", "function",
    "generate", "genvar", "global", "highz0", "highz1", "if", "iff", "ifnone", "ignore_bins",
    "illegal_bins", "implements", "implies", "import", "incdir", "include", "initial", "inout",
    "input", "inside", "instance", "int", "integer", "interconnect", "interface", "intersect",
    "join", "join_any", "join_none", "large", "let", "liblist", "library", "local", "localparam",
    "logic", "longint", "macromodule", "matches", "medium", "modport", "module", "nand",
    "negedge", "nettype", "new", "nexttime", "nmos", "nor", "noshowcancelled", "not", "notif0",
    "notif1", "null", "or", "output", "package", "packed", "parameter", "pmos", "posedge",
    "primitive", "priority", "program", "property", "protected", "pull0", "pull1", "pulldown",
    "pullup", "pulsestyle_ondetect", "pulsestyle_onevent", "pure", "rand", "randc", "randcase",
    "randsequence", "rcmos", "real", "realtime", "ref", "reg", "reject_on", "release", "repeat",
    "restrict", "return", "rnmos", "rpmos", "rtran", "rtranif0", "rtranif1", "s_always",
    "s_eventually", "s_nexttime", "s_until", "s_until_with", "scalared", "sequence",
    "shortint", "shortreal", "showcancelled", "signed", "small", "soft", "solve", "specify",
    "specparam", "static", "string", "strong", "strong0", "strong1", "struct", "super",
    "supply0", "supply1", "sync_accept_on", "sync_reject_on", "table", "tagged", "task", "this",
    "throughout", "time", "timeprecision", "timeunit", "tran", "tranif0", "tranif1", "tri",
    "tri0", "tri1", "triand", "trior", "trireg", "type", "typedef", "union", "unique", "unique0",
    "unsigned", "until", "until_with", "untyped", "use", "uwire", "var", "vectored", "virtual",
    "void", "wait", "wait_order", "wand", "weak", "weak0", "weak1", "while", "wildcard", "wire",
    "with", "within", "wor", "xnor", "xor",
];

pub fn is_keyword(s: &str) -> bool {
    static SET: OnceLock<FxHashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| KEYWORDS.iter().copied().collect()).contains(s)
}

/// Built-in data type keywords that can start a declaration.
pub fn is_builtin_type_kw(s: &str) -> bool {
    matches!(
        s,
        "bit" | "logic" | "reg" | "byte" | "shortint" | "int" | "longint" | "integer" | "time"
            | "real" | "realtime" | "shortreal" | "string" | "chandle" | "event" | "void"
            | "signed" | "unsigned" | "struct" | "union" | "enum" | "type"
    )
}

pub fn is_net_type_kw(s: &str) -> bool {
    matches!(
        s,
        "wire" | "tri" | "tri0" | "tri1" | "triand" | "trior" | "trireg" | "wand" | "wor"
            | "supply0" | "supply1" | "uwire" | "interconnect"
    )
}

/// Keywords that end a construct; seeing one while scanning an expression means a missing `;`.
pub fn is_end_kw(s: &str) -> bool {
    matches!(
        s,
        "end" | "endcase" | "endchecker" | "endclass" | "endclocking" | "endconfig"
            | "endfunction" | "endgenerate" | "endgroup" | "endinterface" | "endmodule"
            | "endpackage" | "endprimitive" | "endprogram" | "endproperty" | "endspecify"
            | "endsequence" | "endtable" | "endtask" | "join" | "join_any" | "join_none"
    )
}
