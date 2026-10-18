//! Error-tolerant recursive-descent parser over the preprocessed token stream.
//!
//! It recognizes declarations precisely and treats statements and expressions loosely: it
//! keeps local declarations, block structure and identifier reference chains, and skips the
//! rest. Recovery happens at `;`, at `end*` keywords and at item-starting keywords, and syntax
//! errors are only reported for structure it is sure about (missing `;`, unbalanced `end*`).

use crate::ast::*;
use crate::diag::Diag;
use crate::span::{FileId, Span};
use crate::token::{is_builtin_type_kw, is_end_kw, is_net_type_kw, TokKind, Token};
use smol_str::SmolStr;

pub fn parse(tokens: &[Token], root_file: FileId) -> ParsedUnit {
    let eof_span = tokens.last().map(|t| Span::new(t.span.file, t.span.end, t.span.end)).unwrap_or(Span::new(root_file, 0, 0));
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        eof: Token::eof(eof_span),
        u: ParsedUnit::default(),
        scopes: Vec::new(),
        end_stack: Vec::new(),
        genblk: Vec::new(),
        kind_override: None,
        stmt_depth: 0,
        too_deep: false,
    };
    p.u.decls.push(Decl {
        kind: DeclKind::Unit,
        name: SmolStr::new_static("$unit"),
        name_span: Span::new(root_file, 0, 0),
        span: Span::new(root_file, 0, eof_span.end),
        parent: None,
        children: Vec::new(),
        ty: None,
        extends: None,
        implements: Vec::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        flags: 0,
        dir: None,
        detail: SmolStr::default(),
        owner: None,
    });
    p.scopes.push(ParsedUnit::ROOT);
    p.genblk.push(0);
    p.items(None);
    let mut u = p.u;
    u.build_index();
    u
}

/// What stops an expression scan at nesting depth 0 (besides `;`, closing brackets and
/// structural keywords, which always stop it).
#[derive(Clone, Copy, Default)]
struct Stop {
    comma: bool,
    colon: bool,
    lbrace: bool,
    iff: bool,
    eq: bool,
}

const STOP_NONE: Stop = Stop { comma: false, colon: false, lbrace: false, iff: false, eq: false };
const STOP_COMMA: Stop = Stop { comma: true, colon: false, lbrace: false, iff: false, eq: false };

const ARRAY_ITER_METHODS: &[&str] = &[
    "find", "find_index", "find_first", "find_first_index", "find_last", "find_last_index", "min",
    "max", "unique", "unique_index", "sort", "rsort", "sum", "product", "and", "or", "xor", "map",
];

struct Parser<'t> {
    toks: &'t [Token],
    pos: usize,
    eof: Token,
    u: ParsedUnit,
    scopes: Vec<DeclId>,
    end_stack: Vec<&'static str>,
    /// Counter for unnamed generate blocks per scope.
    genblk: Vec<u32>,
    /// Reference kind forced inside `with` clauses.
    kind_override: Option<RefKind>,
    /// Statements being parsed inside each other.
    stmt_depth: u32,
    /// "Nested too deeply" was reported.
    too_deep: bool,
}

/// Statements nest at most this deep. Real code stays far below; much deeper (generated or
/// broken) code would overflow the stack, which ends the process.
const MAX_STMT_DEPTH: u32 = 2000;

struct TypeInfo {
    tref: TypeRef,
    /// A plain user type name (possibly with parameters): candidate for an instantiation.
    named: bool,
    /// Token range of a `#( ... )` parameter list on a named type, not yet scanned.
    params: Option<(usize, usize)>,
}

impl<'t> Parser<'t> {
    // ----------------------------------------------------------------- token helpers

    #[inline]
    fn tok(&self) -> &Token {
        self.toks.get(self.pos).unwrap_or(&self.eof)
    }

    #[inline]
    fn nth(&self, n: usize) -> &Token {
        self.toks.get(self.pos + n).unwrap_or(&self.eof)
    }

    #[inline]
    fn at_eof(&self) -> bool {
        self.pos >= self.toks.len()
    }

    #[inline]
    fn bump(&mut self) {
        if self.pos < self.toks.len() {
            self.pos += 1;
        }
    }

    #[inline]
    fn at_kw(&self, k: &str) -> bool {
        self.tok().is_kw(k)
    }

    #[inline]
    fn at_p(&self, p: &str) -> bool {
        self.tok().is_punct(p)
    }

    fn eat_kw(&mut self, k: &str) -> bool {
        if self.at_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_p(&mut self, p: &str) -> bool {
        if self.at_p(p) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            return self.tok().span;
        }
        self.toks[self.pos - 1].span
    }

    fn error(&mut self, span: Span, msg: impl Into<String>) {
        // One syntax error per location is enough.
        if self.u.diags.last().is_some_and(|d| d.span == span) {
            return;
        }
        self.u.diags.push(Diag::error(span, "SYNTAX_ERROR", msg));
    }

    fn expect_p(&mut self, p: &str) -> bool {
        if self.eat_p(p) {
            return true;
        }
        let sp = self.prev_span();
        let at = Span::new(sp.file, sp.end, sp.end);
        self.error(at, format!("Expected '{p}'"));
        false
    }

    fn describe(t: &Token) -> String {
        match t.kind {
            TokKind::Eof => "end of file".to_string(),
            _ => format!("'{}'", t.text),
        }
    }

    /// Index just past the bracket group starting at token index `i` (which must be an
    /// opening bracket).
    fn skip_group_from(&self, mut i: usize) -> usize {
        let mut depth = 0i32;
        while i < self.toks.len() {
            let t = &self.toks[i];
            if t.kind == TokKind::Punct {
                match t.text.as_str() {
                    "(" | "[" | "{" | "'{" => depth += 1,
                    ")" | "]" | "}" => {
                        depth -= 1;
                        if depth <= 0 {
                            return i + 1;
                        }
                    }
                    _ => {}
                }
            } else if t.kind == TokKind::Keyword && is_end_kw(&t.text) {
                return i;
            }
            i += 1;
        }
        i
    }

    fn at_ident(&self) -> bool {
        self.tok().kind == TokKind::Ident
    }

    // ----------------------------------------------------------------- tree helpers

    fn scope(&self) -> DeclId {
        *self.scopes.last().unwrap()
    }

    fn scope_kind(&self) -> DeclKind {
        self.u.decls[self.scope() as usize].kind
    }

    /// Kind of the nearest enclosing non-block scope.
    fn context(&self) -> DeclKind {
        for s in self.scopes.iter().rev() {
            let k = self.u.decls[*s as usize].kind;
            if !matches!(k, DeclKind::Block | DeclKind::Generate) {
                return k;
            }
        }
        DeclKind::Unit
    }

    fn in_design_context(&self) -> bool {
        matches!(
            self.context(),
            DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Checker
        )
    }

    fn new_decl(&mut self, kind: DeclKind, name_tok: Option<&Token>, start: Span) -> DeclId {
        let id = self.u.decls.len() as DeclId;
        let (name, name_span, mut flags) = match name_tok {
            Some(t) => (t.text.clone(), t.span, if t.is_real() { 0 } else { dflags::FROM_MACRO }),
            None => (SmolStr::default(), start, 0),
        };
        if name_tok.is_none() && start.file == u32::MAX {
            flags |= dflags::FROM_MACRO;
        }
        let parent = self.scope();
        let span = if start.file == name_span.file { start.cover(name_span) } else { name_span };
        self.u.decls.push(Decl {
            kind,
            name,
            name_span,
            span,
            parent: Some(parent),
            children: Vec::new(),
            ty: None,
            extends: None,
            implements: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            flags,
            dir: None,
            detail: SmolStr::default(),
            owner: None,
        });
        self.u.decls[parent as usize].children.push(id);
        id
    }

    fn finish(&mut self, id: DeclId) {
        let end = self.prev_span();
        let d = &mut self.u.decls[id as usize];
        if end.file == d.span.file && end.end >= d.span.start {
            d.span.end = d.span.end.max(end.end);
        }
    }

    fn push_scope(&mut self, id: DeclId) {
        self.scopes.push(id);
        self.genblk.push(0);
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
        self.genblk.pop();
    }

    fn add_ref(&mut self, segs: Vec<RefSeg>, kind: RefKind) -> RefId {
        let id = self.u.refs.len() as RefId;
        self.u.refs.push(RefChain { segs, scope: self.scope(), kind });
        id
    }

    fn seg(t: &Token, sep: Sep) -> RefSeg {
        RefSeg { name: t.text.clone(), span: t.span, sep, called: false, selects: 0, from_macro: !t.is_real() }
    }

    fn text_of(&self, from: usize, to: usize) -> SmolStr {
        let mut s = String::new();
        for (i, t) in self.toks[from..to.min(self.toks.len())].iter().enumerate() {
            if i > 0 && t.ws_before() && !matches!(t.text.as_str(), "," | ";" | ")" | "]") {
                s.push(' ');
            }
            if s.len() > 160 {
                s.push_str(" ...");
                break;
            }
            match t.kind {
                TokKind::Directive => {
                    s.push('`');
                    s.push_str(&t.text);
                }
                _ => s.push_str(&t.text),
            }
        }
        SmolStr::new(s)
    }

    // ----------------------------------------------------------------- items

    /// Parses items until `end_kw` (consumed) or until something that belongs to an outer
    /// construct. Returns true when `end_kw` was found.
    fn items(&mut self, end_kw: Option<&'static str>) -> bool {
        if let Some(e) = end_kw {
            self.end_stack.push(e);
        }
        let found = loop {
            let t = self.tok();
            if t.kind == TokKind::Eof {
                if let Some(e) = end_kw {
                    let sp = self.u.decls[self.scope() as usize].name_span;
                    self.error(sp, format!("Missing '{e}'"));
                }
                break false;
            }
            if let Some(e) = end_kw {
                if t.is_kw(e) {
                    self.bump();
                    self.end_label();
                    break true;
                }
            }
            if t.kind == TokKind::Keyword && is_end_kw(&t.text) {
                let text = t.text.clone();
                let span = t.span;
                if self.end_stack.iter().any(|e| *e == text) {
                    if let Some(e) = end_kw {
                        self.error(span, format!("Missing '{e}' before '{text}'"));
                    }
                    break false;
                }
                self.error(span, format!("Unexpected '{text}'"));
                self.bump();
                continue;
            }
            if let Some(e) = end_kw {
                if self.starts_foreign_unit() {
                    let sp = self.u.decls[self.scope() as usize].name_span;
                    self.error(sp, format!("Missing '{e}'"));
                    break false;
                }
            }
            let before = self.pos;
            self.item();
            if self.pos == before {
                let t = self.tok().clone();
                self.error(t.span, format!("Unexpected {}", Self::describe(&t)));
                self.bump();
            }
        };
        if end_kw.is_some() {
            self.end_stack.pop();
        }
        found
    }

    /// A design element that cannot appear in the current scope, which means the current
    /// scope is missing its end keyword.
    fn starts_foreign_unit(&self) -> bool {
        let t = self.tok();
        if t.kind != TokKind::Keyword {
            return false;
        }
        let ctx = self.context();
        let in_class_or_pkg = matches!(ctx, DeclKind::Class | DeclKind::Package | DeclKind::InterfaceClass);
        match t.text.as_str() {
            "package" => true,
            "module" | "macromodule" | "program" => in_class_or_pkg,
            "interface" => !self.nth(1).is_kw("class") && in_class_or_pkg,
            _ => false,
        }
    }

    fn end_label(&mut self) {
        if self.at_p(":") && matches!(self.nth(1).kind, TokKind::Ident | TokKind::Keyword) {
            self.bump();
            self.bump();
        }
    }

    fn skip_attributes(&mut self) {
        while self.at_p("(*") {
            while !self.at_eof() && !self.at_p("*)") {
                self.bump();
            }
            self.bump();
        }
    }

    fn item(&mut self) {
        self.skip_attributes();
        let t = self.tok().clone();
        match t.kind {
            TokKind::Punct => match t.text.as_str() {
                ";" => self.bump(),
                _ => self.expr_stmt(RefKind::Expr),
            },
            TokKind::Keyword => self.keyword_item(&t),
            TokKind::Ident => {
                if self.nth(1).is_punct(":") && !self.nth(2).is_punct(":") {
                    // Label (assertion, generate construct).
                    self.bump();
                    self.bump();
                    self.item();
                    return;
                }
                self.ident_item();
            }
            TokKind::Directive | TokKind::Unknown | TokKind::MacroPaste | TokKind::MacroQuote | TokKind::MacroEscQuote => {
                self.bump();
            }
            _ => self.expr_stmt(RefKind::Expr),
        }
    }

    fn keyword_item(&mut self, t: &Token) {
        let k = t.text.as_str();
        match k {
            "module" | "macromodule" => self.design_unit(DeclKind::Module, "endmodule"),
            "interface" => {
                if self.nth(1).is_kw("class") {
                    self.bump();
                    self.class_decl(0, true, t.span);
                } else {
                    self.design_unit(DeclKind::Interface, "endinterface");
                }
            }
            "program" => self.design_unit(DeclKind::Program, "endprogram"),
            "checker" => self.design_unit(DeclKind::Checker, "endchecker"),
            "package" => self.design_unit(DeclKind::Package, "endpackage"),
            "primitive" => self.skip_named(DeclKind::Primitive, "endprimitive"),
            "config" => self.skip_named(DeclKind::Config, "endconfig"),
            "class" => self.class_decl(0, false, t.span),
            "function" | "task" => self.subroutine(0, t.span),
            "import" => self.import_item(),
            "export" => self.export_item(),
            "typedef" => self.typedef(),
            "parameter" | "localparam" => self.param_decl(),
            "specparam" | "defparam" | "timeunit" | "timeprecision" | "alias" | "nettype" | "bind" => {
                self.bump();
                self.scan_until_semi(RefKind::Loose);
            }
            "constraint" => self.constraint(0, t.span),
            "covergroup" => self.covergroup(),
            "property" => self.assertion_decl(DeclKind::Property, "endproperty"),
            "sequence" => self.assertion_decl(DeclKind::Sequence, "endsequence"),
            "let" => {
                self.bump();
                if self.at_ident() {
                    let n = self.tok().clone();
                    let id = self.new_decl(DeclKind::Let, Some(&n), t.span);
                    self.bump();
                    self.push_scope(id);
                    if self.at_p("(") {
                        self.arg_list(DeclKind::Argument);
                    }
                    self.scan_until_semi(RefKind::Loose);
                    self.pop_scope();
                    self.finish(id);
                } else {
                    self.scan_until_semi(RefKind::Loose);
                }
            }
            "clocking" => self.clocking(t.span),
            "default" | "global" if self.nth(1).is_kw("clocking") => {
                self.bump();
                self.clocking(t.span);
            }
            "default" => {
                self.bump();
                self.scan_until_semi(RefKind::Loose);
            }
            "modport" => self.modport(),
            "genvar" => {
                self.bump();
                while self.at_ident() {
                    let n = self.tok().clone();
                    let id = self.new_decl(DeclKind::Genvar, Some(&n), n.span);
                    self.u.decls[id as usize].detail = SmolStr::new_static("genvar");
                    self.bump();
                    if !self.eat_p(",") {
                        break;
                    }
                }
                self.expect_p(";");
            }
            "generate" => {
                self.bump();
                self.items(Some("endgenerate"));
            }
            "for" => self.generate_for(),
            "if" => self.generate_if(),
            "case" => self.generate_case(),
            "begin" => self.generate_block(t.span),
            "always" | "always_comb" | "always_ff" | "always_latch" | "initial" | "final" => {
                self.bump();
                self.stmt();
            }
            "assign" => {
                self.bump();
                // Optional drive strength / delay.
                if self.at_p("(") && matches!(self.nth(1).text.as_str(), "strong0" | "strong1" | "weak0" | "weak1" | "pull0" | "pull1" | "supply0" | "supply1" | "highz0" | "highz1") {
                    let end = self.skip_group_from(self.pos);
                    self.pos = end;
                }
                self.delay_opt();
                self.scan_until_semi(RefKind::Loose);
            }
            "assert" | "assume" | "cover" | "restrict" | "expect" => self.stmt(),
            "specify" => self.skip_until_kw("endspecify"),
            "table" => self.skip_until_kw("endtable"),
            "input" | "output" | "inout" | "ref" => self.port_decl_nonansi(),
            "extern" | "pure" | "static" | "local" | "protected" | "rand" | "randc" | "const"
            | "automatic" | "virtual" | "var" => self.modified_item(t.span),
            _ if is_net_type_kw(k) || is_builtin_type_kw(k) => {
                self.data_decl(DeclKind::Variable, 0, t.span);
            }
            "this" | "super" | "new" | "null" => self.expr_stmt(RefKind::Expr),
            "else" => {
                self.error(t.span, "Unexpected 'else'");
                self.bump();
            }
            _ => {
                // Some other construct: skip it as a statement.
                self.bump();
                self.scan_until_semi(RefKind::Loose);
            }
        }
    }

    /// Items starting with qualifiers: class members, `extern` prototypes, `virtual` classes
    /// and interfaces, `const`/`var` declarations.
    fn modified_item(&mut self, start: Span) {
        let mut fl = 0u32;
        loop {
            let t = self.tok();
            if t.kind != TokKind::Keyword {
                break;
            }
            let f = match t.text.as_str() {
                "extern" => dflags::EXTERN,
                "pure" => dflags::PURE,
                "static" => dflags::STATIC,
                "local" => dflags::LOCAL,
                "protected" => dflags::PROTECTED,
                "rand" | "randc" => dflags::RAND,
                "const" => dflags::CONST,
                "automatic" => dflags::AUTOMATIC,
                "var" => 0,
                "virtual" => {
                    // `virtual interface x` / `virtual my_if x` is a type, not a qualifier.
                    let n = self.nth(1);
                    if n.is_kw("interface") || n.kind == TokKind::Ident {
                        break;
                    }
                    dflags::VIRTUAL
                }
                _ => break,
            };
            fl |= f;
            self.bump();
        }
        let t = self.tok().clone();
        match t.text.as_str() {
            "function" | "task" if t.kind == TokKind::Keyword => self.subroutine(fl, start),
            "constraint" if t.kind == TokKind::Keyword => self.constraint(fl, start),
            "class" if t.kind == TokKind::Keyword => self.class_decl(fl, false, start),
            "interface" if t.kind == TokKind::Keyword && self.nth(1).is_kw("class") => {
                self.bump();
                self.class_decl(fl, true, start);
            }
            "module" | "interface" | "program" | "primitive" if fl & dflags::EXTERN != 0 => {
                // extern module declaration.
                self.bump();
                self.scan_until_semi(RefKind::Loose);
            }
            "covergroup" => self.covergroup(),
            "typedef" if t.kind == TokKind::Keyword => self.typedef(),
            "parameter" | "localparam" if t.kind == TokKind::Keyword => self.param_decl(),
            _ => {
                self.data_decl(DeclKind::Variable, fl, start);
            }
        }
    }

    fn ident_item(&mut self) {
        let start = self.tok().span;
        match self.decl_lookahead() {
            Lookahead::Decl => {
                self.data_decl(DeclKind::Variable, 0, start);
            }
            Lookahead::Instance if self.in_design_context() || self.scope_kind() == DeclKind::Generate || self.context() == DeclKind::Unit => {
                self.data_decl(DeclKind::Variable, 0, start);
            }
            _ => self.expr_stmt(RefKind::Expr),
        }
    }

    fn skip_named(&mut self, kind: DeclKind, end: &'static str) {
        let start = self.tok().span;
        self.bump();
        if self.at_ident() {
            let n = self.tok().clone();
            let id = self.new_decl(kind, Some(&n), start);
            self.bump();
            self.skip_until_kw(end);
            self.finish(id);
        } else {
            self.skip_until_kw(end);
        }
    }

    fn skip_until_kw(&mut self, end: &str) {
        while !self.at_eof() && !self.at_kw(end) {
            self.bump();
        }
        self.bump();
        self.end_label();
    }

    fn skip_to_semi(&mut self) {
        while !self.at_eof() && !self.at_p(";") {
            if self.tok().kind == TokKind::Keyword && is_end_kw(&self.tok().text) {
                return;
            }
            self.bump();
        }
        self.bump();
    }

    fn design_unit(&mut self, kind: DeclKind, end: &'static str) {
        let start = self.tok().span;
        self.bump();
        while self.at_kw("static") || self.at_kw("automatic") {
            self.bump();
        }
        self.skip_attributes();
        let name = if self.at_ident() {
            let n = self.tok().clone();
            self.bump();
            Some(n)
        } else {
            let sp = self.tok().span;
            self.error(sp, format!("Expected {} name", kind.label()));
            None
        };
        let id = self.new_decl(kind, name.as_ref(), start);
        self.push_scope(id);
        loop {
            if self.at_kw("import") {
                self.import_item();
            } else if self.at_p("#") && self.nth(1).is_punct("(") {
                self.bump();
                self.param_port_list();
            } else if self.at_p("(") {
                self.port_list();
            } else {
                break;
            }
        }
        if !self.eat_p(";") {
            let sp = self.prev_span();
            self.error(Span::new(sp.file, sp.end, sp.end), "Expected ';'");
        }
        self.items(Some(end));
        self.pop_scope();
        self.finish(id);
    }

    fn class_decl(&mut self, fl: u32, interface: bool, start: Span) {
        self.bump(); // class
        while self.at_kw("static") || self.at_kw("automatic") {
            self.bump();
        }
        if !self.at_ident() {
            let sp = self.tok().span;
            self.error(sp, "Expected class name");
            self.skip_to_semi();
            return;
        }
        let n = self.tok().clone();
        self.bump();
        let kind = if interface { DeclKind::InterfaceClass } else { DeclKind::Class };
        let id = self.new_decl(kind, Some(&n), start);
        self.u.decls[id as usize].flags |= fl & dflags::VIRTUAL;
        self.push_scope(id);
        if self.at_p("#") && self.nth(1).is_punct("(") {
            self.bump();
            self.param_port_list();
        }
        if self.eat_kw("extends") {
            let mut first = true;
            loop {
                if let Some(ti) = self.parse_type(false, RefKind::Base) {
                    self.scan_params(ti.params);
                    if first {
                        self.u.decls[id as usize].extends = Some(ti.tref);
                    } else {
                        self.u.decls[id as usize].implements.push(ti.tref);
                    }
                }
                first = false;
                if self.at_p("(") {
                    // extends base(args)
                    self.paren_group(RefKind::Expr);
                }
                if !self.eat_p(",") {
                    break;
                }
            }
        }
        if self.eat_kw("implements") {
            loop {
                if let Some(ti) = self.parse_type(false, RefKind::Base) {
                    self.scan_params(ti.params);
                    self.u.decls[id as usize].implements.push(ti.tref);
                }
                if !self.eat_p(",") {
                    break;
                }
            }
        }
        self.expect_p(";");
        self.items(Some("endclass"));
        self.pop_scope();
        self.finish(id);
    }

    /// `#( parameter ... )` in a module, interface, program or class header.
    fn param_port_list(&mut self) {
        self.bump(); // (
        let mut kind = DeclKind::Parameter;
        let mut last_ty: Option<TypeRef> = None;
        loop {
            self.skip_attributes();
            if self.at_eof() {
                return;
            }
            if self.eat_p(")") {
                return;
            }
            let start = self.tok().span;
            let mut is_type = false;
            if self.eat_kw("parameter") {
                kind = DeclKind::Parameter;
                last_ty = None;
            } else if self.eat_kw("localparam") {
                kind = DeclKind::LocalParam;
                last_ty = None;
            }
            if self.eat_kw("type") {
                is_type = true;
            }
            let mut ty = None;
            if !is_type && !(self.at_ident() && matches!(self.nth(1).text.as_str(), "=" | "," | ")" | "[")) {
                ty = self.parse_type(false, RefKind::Type).map(|ti| {
                    self.scan_params(ti.params);
                    ti.tref
                });
            }
            if ty.is_some() {
                last_ty = ty.clone();
            }
            if self.at_ident() {
                let n = self.tok().clone();
                self.bump();
                let k = if is_type { DeclKind::TypeParam } else { kind };
                let id = self.new_decl(k, Some(&n), start);
                self.u.decls[id as usize].ty = if is_type { None } else { ty.or_else(|| last_ty.clone()) };
                self.dims(RefKind::Expr);
                if self.eat_p("=") {
                    let from = self.pos;
                    if is_type {
                        if let Some(ti) = self.parse_type(false, RefKind::Type) {
                            self.scan_params(ti.params);
                        }
                    }
                    self.scan_expr(STOP_COMMA, RefKind::Expr);
                    let v = self.text_of(from, self.pos);
                    self.u.decls[id as usize].detail = v;
                }
                self.finish(id);
            } else {
                // Positional values or junk: skip one element.
                self.scan_expr(STOP_COMMA, RefKind::Expr);
            }
            if !self.eat_p(",") {
                if !self.eat_p(")") {
                    let sp = self.tok().span;
                    self.error(sp, "Expected ',' or ')' in parameter list");
                    self.recover_list();
                }
                return;
            }
        }
    }

    fn recover_list(&mut self) {
        let mut depth = 0;
        while !self.at_eof() {
            let t = self.tok();
            if t.is_punct("(") {
                depth += 1;
            } else if t.is_punct(")") {
                if depth == 0 {
                    self.bump();
                    return;
                }
                depth -= 1;
            } else if t.is_punct(";") && depth == 0 {
                return;
            }
            self.bump();
        }
    }

    fn port_dir(&self) -> Option<PortDir> {
        let t = self.tok();
        if t.kind != TokKind::Keyword {
            return None;
        }
        match t.text.as_str() {
            "input" => Some(PortDir::Input),
            "output" => Some(PortDir::Output),
            "inout" => Some(PortDir::Inout),
            "ref" => Some(PortDir::Ref),
            "const" if self.nth(1).is_kw("ref") => Some(PortDir::ConstRef),
            _ => None,
        }
    }

    /// ANSI or non-ANSI port list of a module/interface/program header.
    fn port_list(&mut self) {
        self.bump(); // (
        let mut dir: Option<PortDir> = None;
        let mut last_ty: Option<TypeRef> = None;
        loop {
            self.skip_attributes();
            if self.at_eof() || self.eat_p(")") {
                return;
            }
            let start = self.tok().span;
            if let Some(d) = self.port_dir() {
                if d == PortDir::ConstRef {
                    self.bump();
                }
                self.bump();
                dir = Some(d);
                last_ty = None;
            }
            if self.at_p(".") && self.nth(1).kind == TokKind::Ident {
                // Explicit port: .name(expr)
                self.bump();
                let n = self.tok().clone();
                self.bump();
                let id = self.new_decl(DeclKind::Port, Some(&n), start);
                self.u.decls[id as usize].dir = dir;
                if self.at_p("(") {
                    self.paren_group(RefKind::Loose);
                }
                self.finish(id);
            } else {
                let mut ty = None;
                let mut net = false;
                if is_net_type_kw(&self.tok().text) && self.tok().kind == TokKind::Keyword {
                    net = true;
                    self.bump();
                }
                if self.at_kw("var") {
                    self.bump();
                }
                if self.at_kw("interface") {
                    self.bump();
                    let mut text = String::from("interface");
                    if self.at_p(".") && self.nth(1).kind == TokKind::Ident {
                        text.push('.');
                        text.push_str(&self.nth(1).text);
                        self.bump();
                        self.bump();
                    }
                    ty = Some(TypeRef { text: SmolStr::new(text), ref_id: None, inline: None });
                } else if !(self.at_ident() && matches!(self.nth(1).text.as_str(), "," | ")" | "[" | "=")) {
                    ty = self.parse_type(true, RefKind::Type).map(|ti| {
                        self.scan_params(ti.params);
                        ti.tref
                    });
                }
                if ty.is_some() {
                    last_ty = ty.clone();
                } else if net || dir.is_some() {
                    ty = last_ty.clone();
                }
                if self.at_ident() {
                    let n = self.tok().clone();
                    self.bump();
                    let id = self.new_decl(DeclKind::Port, Some(&n), start);
                    self.u.decls[id as usize].dir = dir;
                    self.u.decls[id as usize].ty = ty;
                    self.array_dims(id);
                    if self.eat_p("=") {
                        self.scan_expr(STOP_COMMA, RefKind::Expr);
                    }
                    self.finish(id);
                } else {
                    let sp = self.tok().span;
                    self.error(sp, "Expected port name");
                    self.scan_expr(STOP_COMMA, RefKind::Loose);
                }
            }
            if !self.eat_p(",") {
                if !self.eat_p(")") {
                    let sp = self.tok().span;
                    self.error(sp, "Expected ',' or ')' in port list");
                    self.recover_list();
                }
                return;
            }
        }
    }

    /// `input [7:0] a, b;` in a module body (non-ANSI) or a task/function body (old style).
    fn port_decl_nonansi(&mut self) {
        let start = self.tok().span;
        let dir = self.port_dir();
        if dir == Some(PortDir::ConstRef) {
            self.bump();
        }
        self.bump();
        if is_net_type_kw(&self.tok().text) || self.at_kw("var") {
            self.bump();
        }
        let ty = if self.at_ident() && matches!(self.nth(1).text.as_str(), "," | ";" | "[" | "=") {
            None
        } else {
            self.parse_type(true, RefKind::Type).map(|ti| {
                self.scan_params(ti.params);
                ti.tref
            })
        };
        let scope_kind = self.scope_kind();
        let kind = if matches!(scope_kind, DeclKind::Function | DeclKind::Task) { DeclKind::Argument } else { DeclKind::Port };
        loop {
            if !self.at_ident() {
                let sp = self.tok().span;
                self.error(sp, "Expected port name");
                self.scan_until_semi(RefKind::Loose);
                return;
            }
            let n = self.tok().clone();
            self.bump();
            // Complete a port already named in the header, if any.
            let scope = self.scope();
            let existing = self
                .u
                .decls[scope as usize]
                .children
                .iter()
                .copied()
                .find(|c| {
                    let d = &self.u.decls[*c as usize];
                    d.kind == kind && d.name == n.text
                });
            let id = match existing {
                Some(id) => {
                    // The name in the header becomes a reference to the port declared here, so
                    // rename and references cover both occurrences.
                    let header = self.u.decls[id as usize].name_span;
                    let from_macro = self.u.decls[id as usize].has(dflags::FROM_MACRO);
                    self.u.refs.push(RefChain {
                        segs: vec![RefSeg { name: n.text.clone(), span: header, sep: Sep::None, called: false, selects: 0, from_macro }],
                        scope,
                        kind: RefKind::Loose,
                    });
                    let d = &mut self.u.decls[id as usize];
                    // Navigation goes to the declaration with the direction.
                    d.name_span = n.span;
                    d.span = start.cover(n.span);
                    id
                }
                None => self.new_decl(kind, Some(&n), start),
            };
            self.u.decls[id as usize].dir = dir;
            if ty.is_some() {
                self.u.decls[id as usize].ty = ty.clone();
            }
            self.array_dims(id);
            if self.eat_p("=") {
                self.scan_expr(STOP_COMMA, RefKind::Expr);
            }
            self.finish(id);
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(";");
    }

    fn import_item(&mut self) {
        self.bump(); // import
        if self.tok().kind == TokKind::String {
            // import "DPI-C" [context|pure] [c_name =] function ...;
            self.bump();
            while self.at_kw("context") || self.at_kw("pure") {
                self.bump();
            }
            if self.at_ident() && self.nth(1).is_punct("=") {
                self.bump();
                self.bump();
            }
            if self.at_kw("function") || self.at_kw("task") {
                let sp = self.tok().span;
                self.subroutine(dflags::DPI | dflags::PROTOTYPE, sp);
            } else {
                self.scan_until_semi(RefKind::Loose);
            }
            return;
        }
        loop {
            if !self.at_ident() {
                let sp = self.tok().span;
                self.error(sp, "Expected package name");
                self.scan_until_semi(RefKind::Loose);
                return;
            }
            let pkg = self.tok().clone();
            self.bump();
            let mut item = None;
            if self.eat_p("::") {
                if self.eat_p("*") {
                } else if matches!(self.tok().kind, TokKind::Ident) {
                    item = Some(self.tok().clone());
                    self.bump();
                } else {
                    let sp = self.tok().span;
                    self.error(sp, "Expected '*' or an item name");
                }
            } else {
                self.expect_p("::");
            }
            self.add_ref(vec![Self::seg(&pkg, Sep::None)], RefKind::Package);
            if let Some(it) = &item {
                self.add_ref(vec![Self::seg(&pkg, Sep::None), Self::seg(it, Sep::Scope)], RefKind::ImportItem);
            }
            let scope = self.scope();
            self.u.decls[scope as usize].imports.push(Import {
                pkg: pkg.text.clone(),
                pkg_span: pkg.span,
                item: item.map(|t| t.text),
            });
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(";");
    }

    /// `export pkg::item, pkg::*, *::*;` (package re-exports) or `export "DPI-C" ...;`.
    fn export_item(&mut self) {
        self.bump(); // export
        if self.tok().kind == TokKind::String {
            // export "DPI-C" [c_name =] function name;
            self.skip_to_semi();
            return;
        }
        loop {
            let star_pkg = self.at_p("*");
            if !star_pkg && !self.at_ident() {
                self.scan_until_semi(RefKind::Loose);
                return;
            }
            let pkg = self.tok().clone();
            self.bump();
            if !self.eat_p("::") {
                self.scan_until_semi(RefKind::Loose);
                return;
            }
            let item = if self.eat_p("*") {
                None
            } else if self.at_ident() {
                let it = self.tok().clone();
                self.bump();
                Some(it)
            } else {
                self.scan_until_semi(RefKind::Loose);
                return;
            };
            if !star_pkg {
                self.add_ref(vec![Self::seg(&pkg, Sep::None)], RefKind::Package);
                if let Some(it) = &item {
                    self.add_ref(vec![Self::seg(&pkg, Sep::None), Self::seg(it, Sep::Scope)], RefKind::ImportItem);
                }
            }
            let scope = self.scope();
            self.u.decls[scope as usize].exports.push(Import {
                pkg: if star_pkg { SmolStr::new_static("*") } else { pkg.text.clone() },
                pkg_span: pkg.span,
                item: item.map(|t| t.text),
            });
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(";");
    }

    fn typedef(&mut self) {
        let start = self.tok().span;
        self.bump(); // typedef
        // Forward declarations.
        let fwd_kw = matches!(self.tok().text.as_str(), "class" | "enum" | "struct" | "union") && self.tok().kind == TokKind::Keyword;
        if (fwd_kw && self.nth(1).kind == TokKind::Ident && self.nth(2).is_punct(";"))
            || (self.at_kw("interface") && self.nth(1).is_kw("class"))
        {
            if self.at_kw("interface") {
                self.bump();
            }
            self.bump();
        }
        if self.at_ident() && self.nth(1).is_punct(";") {
            let n = self.tok().clone();
            self.bump();
            self.bump();
            let id = self.new_decl(DeclKind::Typedef, Some(&n), start);
            self.u.decls[id as usize].flags |= dflags::FORWARD;
            self.finish(id);
            return;
        }
        let ty = self.parse_type(true, RefKind::Type).map(|ti| {
            self.scan_params(ti.params);
            ti.tref
        });
        if self.at_ident() {
            let n = self.tok().clone();
            self.bump();
            let id = self.new_decl(DeclKind::Typedef, Some(&n), start);
            self.array_dims(id);
            let detail = ty.as_ref().map(|t| t.text.clone()).unwrap_or_default();
            // Re-parent an inline struct/enum body under the typedef for a tidy outline.
            if let Some(inline) = ty.as_ref().and_then(|t| t.inline) {
                let old_parent = self.u.decls[inline as usize].parent.unwrap();
                self.u.decls[old_parent as usize].children.retain(|c| *c != inline);
                self.u.decls[inline as usize].parent = Some(id);
                self.u.decls[id as usize].children.push(inline);
            }
            self.u.decls[id as usize].ty = ty;
            self.u.decls[id as usize].detail = detail;
            self.expect_p(";");
            self.finish(id);
        } else {
            let sp = self.tok().span;
            self.error(sp, "Expected type name");
            self.scan_until_semi(RefKind::Loose);
        }
    }

    fn param_decl(&mut self) {
        let start = self.tok().span;
        let kind = if self.at_kw("localparam") { DeclKind::LocalParam } else { DeclKind::Parameter };
        self.bump();
        if self.eat_kw("type") {
            loop {
                if !self.at_ident() {
                    break;
                }
                let n = self.tok().clone();
                self.bump();
                let id = self.new_decl(DeclKind::TypeParam, Some(&n), start);
                if self.eat_p("=") {
                    let from = self.pos;
                    if let Some(ti) = self.parse_type(false, RefKind::Type) {
                        self.scan_params(ti.params);
                    }
                    self.scan_expr(STOP_COMMA, RefKind::Expr);
                    self.u.decls[id as usize].detail = self.text_of(from, self.pos);
                }
                self.finish(id);
                if !self.eat_p(",") {
                    break;
                }
            }
            self.expect_p(";");
            return;
        }
        let ty = if self.at_ident() && matches!(self.nth(1).text.as_str(), "=" | "," | "[") {
            None
        } else {
            self.parse_type(false, RefKind::Type).map(|ti| {
                self.scan_params(ti.params);
                ti.tref
            })
        };
        loop {
            if !self.at_ident() {
                let sp = self.tok().span;
                self.error(sp, "Expected parameter name");
                self.scan_until_semi(RefKind::Loose);
                return;
            }
            let n = self.tok().clone();
            self.bump();
            let id = self.new_decl(kind, Some(&n), start);
            self.u.decls[id as usize].ty = ty.clone();
            self.dims(RefKind::Expr);
            if self.eat_p("=") {
                let from = self.pos;
                self.scan_expr(STOP_COMMA, RefKind::Expr);
                self.u.decls[id as usize].detail = self.text_of(from, self.pos);
            }
            self.finish(id);
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(";");
    }

    // ----------------------------------------------------------------- types

    /// Parses a data type if one starts here. `modport` allows `if_name.modport` (ports).
    fn parse_type(&mut self, modport: bool, kind: RefKind) -> Option<TypeInfo> {
        let from = self.pos;
        let t = self.tok().clone();
        let mut named = false;
        let mut params = None;
        let mut ref_id = None;
        let mut inline = None;
        let mut flags_virtual = false;
        match t.kind {
            TokKind::Keyword => match t.text.as_str() {
                "struct" | "union" => {
                    inline = Some(self.struct_type());
                }
                "enum" => {
                    inline = Some(self.enum_type());
                }
                "type" => {
                    self.bump();
                    if self.at_p("(") {
                        self.paren_group(RefKind::Expr);
                    }
                }
                "virtual" => {
                    self.bump();
                    self.eat_kw("interface");
                    flags_virtual = true;
                    if self.at_ident() {
                        let (rid, pr) = self.named_type(kind);
                        ref_id = Some(rid);
                        params = pr;
                        if self.at_p(".") && self.nth(1).kind == TokKind::Ident {
                            self.bump();
                            self.bump();
                        }
                    }
                }
                "signed" | "unsigned" => {
                    self.bump();
                }
                k if is_builtin_type_kw(k) || is_net_type_kw(k) => {
                    self.bump();
                    if is_net_type_kw(k) {
                        // Net type followed by an optional data type.
                        if let Some(inner) = self.parse_type(modport, kind) {
                            ref_id = inner.tref.ref_id;
                            inline = inner.tref.inline;
                            params = inner.params;
                        }
                    }
                    while self.at_kw("signed") || self.at_kw("unsigned") {
                        self.bump();
                    }
                }
                _ => return None,
            },
            TokKind::Ident => {
                let (rid, pr) = self.named_type(kind);
                ref_id = Some(rid);
                params = pr;
                named = true;
                if modport && self.at_p(".") && self.nth(1).kind == TokKind::Ident && self.nth(2).kind == TokKind::Ident {
                    self.bump();
                    self.bump();
                    named = false;
                }
            }
            TokKind::SysIdent if t.text == "$unit" && self.nth(1).is_punct("::") => {
                self.bump();
                self.bump();
                return self.parse_type(modport, kind);
            }
            TokKind::Punct if t.text == "[" => {}
            _ => return None,
        }
        // Packed dimensions.
        let had_dims = self.at_p("[");
        self.dims(RefKind::Expr);
        if had_dims {
            named = false;
        }
        let _ = flags_virtual;
        let text = self.text_of(from, self.pos);
        Some(TypeInfo { tref: TypeRef { text, ref_id, inline }, named, params })
    }

    /// `name [#(...)] {:: name [#(...)]}`; returns the reference and the parameter range of
    /// the last segment.
    fn named_type(&mut self, kind: RefKind) -> (RefId, Option<(usize, usize)>) {
        let mut segs = vec![Self::seg(self.tok(), Sep::None)];
        self.bump();
        let mut params = None;
        loop {
            if self.at_p("#") && self.nth(1).is_punct("(") {
                let open = self.pos + 1;
                let end = self.skip_group_from(open);
                params = Some((open + 1, end.saturating_sub(1)));
                self.pos = end;
                continue;
            }
            if self.at_p("::") && self.nth(1).kind == TokKind::Ident {
                if let Some((a, b)) = params.take() {
                    // Parameters of an inner segment: scan now.
                    self.scan_params(Some((a, b)));
                }
                self.bump();
                segs.push(Self::seg(self.tok(), Sep::Scope));
                self.bump();
                continue;
            }
            break;
        }
        (self.add_ref(segs, kind), params)
    }

    /// Scans a deferred `#( ... )` parameter value list for references.
    fn scan_params(&mut self, range: Option<(usize, usize)>) {
        let Some((a, b)) = range else { return };
        let save = self.pos;
        self.pos = a;
        while self.pos < b {
            let before = self.pos;
            if self.at_p(".") && self.nth(1).kind == TokKind::Ident {
                self.bump();
                self.bump();
                continue;
            }
            // Type values like `virtual my_if` or `my_pkg::T`.
            if self.at_kw("virtual") {
                self.bump();
                self.eat_kw("interface");
                continue;
            }
            self.scan_expr(STOP_COMMA, RefKind::Expr);
            if self.at_p(",") {
                self.bump();
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.pos = save;
    }

    /// Unpacked dimensions after a declared name; marks the declaration as an array.
    fn array_dims(&mut self, id: DeclId) {
        let mut n = 0u32;
        while self.at_p("[") {
            n += 1;
            self.bump();
            self.scan_expr(STOP_NONE, RefKind::Expr);
            if !self.eat_p("]") {
                break;
            }
        }
        if n > 0 {
            self.u.decls[id as usize].flags |= dflags::ARRAY | (n.min(15) << dflags::DIMS_SHIFT);
        }
    }

    fn dims(&mut self, kind: RefKind) {
        while self.at_p("[") {
            self.bump();
            self.scan_expr(STOP_NONE, kind);
            if !self.eat_p("]") {
                // Unbalanced: give up on this dimension.
                return;
            }
        }
    }

    fn struct_type(&mut self) -> DeclId {
        let start = self.tok().span;
        self.bump(); // struct/union
        while matches!(self.tok().text.as_str(), "packed" | "tagged" | "signed" | "unsigned" | "soft") && self.tok().kind == TokKind::Keyword {
            self.bump();
        }
        let id = self.new_decl(DeclKind::StructType, None, start);
        if !self.at_p("{") {
            let sp = self.tok().span;
            self.error(sp, "Expected '{'");
            return id;
        }
        self.bump();
        self.push_scope(id);
        while !self.at_eof() && !self.at_p("}") {
            let before = self.pos;
            self.skip_attributes();
            let mut fl = 0;
            while self.at_kw("rand") || self.at_kw("randc") {
                fl |= dflags::RAND;
                self.bump();
            }
            let s = self.tok().span;
            if self.tok().kind == TokKind::Keyword && is_end_kw(&self.tok().text) {
                break;
            }
            self.data_decl(DeclKind::Field, fl, s);
            if self.pos == before {
                self.bump();
            }
        }
        self.pop_scope();
        self.expect_p("}");
        self.finish(id);
        id
    }

    fn enum_type(&mut self) -> DeclId {
        let start = self.tok().span;
        self.bump(); // enum
        if !self.at_p("{") {
            if let Some(ti) = self.parse_type(false, RefKind::Type) {
                self.scan_params(ti.params);
            }
        }
        let id = self.new_decl(DeclKind::EnumType, None, start);
        if !self.eat_p("{") {
            let sp = self.tok().span;
            self.error(sp, "Expected '{'");
            return id;
        }
        self.push_scope(id);
        loop {
            if self.at_eof() || self.at_p("}") {
                break;
            }
            if self.at_ident() {
                let n = self.tok().clone();
                self.bump();
                let m = self.new_decl(DeclKind::EnumMember, Some(&n), n.span);
                self.dims(RefKind::Expr);
                if self.eat_p("=") {
                    let from = self.pos;
                    self.scan_expr(STOP_COMMA, RefKind::Expr);
                    self.u.decls[m as usize].detail = self.text_of(from, self.pos);
                }
                self.finish(m);
            } else {
                let sp = self.tok().span;
                self.error(sp, "Expected enum item name");
                self.scan_expr(STOP_COMMA, RefKind::Loose);
            }
            if !self.eat_p(",") {
                break;
            }
        }
        self.pop_scope();
        self.expect_p("}");
        self.finish(id);
        id
    }

    // ----------------------------------------------------------------- declarations

    /// Data declaration or (in design contexts) instantiation, starting at the type.
    fn data_decl(&mut self, kind: DeclKind, mut fl: u32, start: Span) {
        let mut kind = kind;
        while matches!(self.tok().text.as_str(), "const" | "var" | "static" | "automatic" | "rand" | "randc" | "local" | "protected")
            && self.tok().kind == TokKind::Keyword
        {
            match self.tok().text.as_str() {
                "const" => fl |= dflags::CONST,
                "static" => fl |= dflags::STATIC,
                "automatic" => fl |= dflags::AUTOMATIC,
                "rand" | "randc" => fl |= dflags::RAND,
                "local" => fl |= dflags::LOCAL,
                "protected" => fl |= dflags::PROTECTED,
                _ => {}
            }
            self.bump();
        }
        if self.at_kw("virtual") {
            fl |= dflags::VIRTUAL_IF;
        }
        if self.tok().kind == TokKind::Keyword && is_net_type_kw(&self.tok().text) {
            kind = DeclKind::Net;
            self.bump();
            // Strength, vectored/scalared, delay.
            if self.at_p("(") && matches!(self.nth(1).text.as_str(), "strong0" | "strong1" | "weak0" | "weak1" | "pull0" | "pull1" | "supply0" | "supply1" | "highz0" | "highz1" | "small" | "medium" | "large") {
                let end = self.skip_group_from(self.pos);
                self.pos = end;
            }
            while self.at_kw("vectored") || self.at_kw("scalared") {
                self.bump();
            }
        }
        let ty = if kind == DeclKind::Net && (self.at_ident() && matches!(self.nth(1).text.as_str(), ";" | "," | "=" | "[")) {
            None
        } else if kind == DeclKind::Net && self.at_p("#") {
            None
        } else {
            self.parse_type(false, RefKind::Type)
        };
        if kind == DeclKind::Net {
            self.delay_opt();
        }
        // Instantiation: `type_name [#(...)] inst (...)`.
        if let Some(ti) = &ty {
            if ti.named && self.at_ident() {
                let mut j = self.pos + 1;
                while self.toks.get(j).is_some_and(|t| t.is_punct("[")) {
                    j = self.skip_group_from(j);
                }
                if self.toks.get(j).is_some_and(|t| t.is_punct("(")) && kind != DeclKind::Field {
                    let ti = ty.unwrap();
                    if let Some(r) = ti.tref.ref_id {
                        self.u.refs[r as usize].kind = RefKind::Definition;
                    }
                    self.instances(ti, start);
                    return;
                }
            }
        }
        let tref = ty.map(|ti| {
            self.scan_params(ti.params);
            ti.tref
        });
        let mut first = true;
        loop {
            if !self.at_ident() {
                if first {
                    let sp = self.tok().span;
                    let d = Self::describe(self.tok());
                    self.error(sp, format!("Expected identifier, found {d}"));
                }
                self.scan_until_semi(RefKind::Loose);
                return;
            }
            first = false;
            let n = self.tok().clone();
            self.bump();
            let id = self.new_decl(kind, Some(&n), start);
            self.u.decls[id as usize].ty = tref.clone();
            self.u.decls[id as usize].flags |= fl;
            self.array_dims(id);
            if self.eat_p("=") {
                let from = self.pos;
                self.scan_expr(STOP_COMMA, RefKind::Expr);
                if matches!(self.u.decls[id as usize].kind, DeclKind::Variable | DeclKind::Net) && fl & dflags::CONST != 0 {
                    self.u.decls[id as usize].detail = self.text_of(from, self.pos);
                }
            }
            self.finish(id);
            if !self.eat_p(",") {
                break;
            }
        }
        if !self.eat_p(";") {
            let sp = self.prev_span();
            self.error(Span::new(sp.file, sp.end, sp.end), "Expected ';'");
            self.recover_semi();
        }
    }

    fn delay_opt(&mut self) {
        if self.at_p("#") {
            self.bump();
            if self.at_p("(") {
                self.paren_group(RefKind::Expr);
            } else {
                self.bump();
            }
        }
    }

    fn instances(&mut self, ti: TypeInfo, start: Span) {
        let module_ref = ti.tref.ref_id;
        loop {
            if !self.at_ident() {
                let sp = self.tok().span;
                self.error(sp, "Expected instance name");
                self.scan_until_semi(RefKind::Loose);
                return;
            }
            let n = self.tok().clone();
            self.bump();
            let id = self.new_decl(DeclKind::Instance, Some(&n), start);
            self.u.decls[id as usize].ty = Some(ti.tref.clone());
            // Parameter assignments belong to this instance.
            if let Some((a, b)) = ti.params {
                let save = self.pos;
                self.pos = a;
                self.connection_list(b, |inst| RefKind::Param { inst }, id);
                self.pos = save;
            }
            self.dims(RefKind::Expr);
            if self.at_p("(") {
                self.bump();
                let close = self.skip_group_from(self.pos - 1);
                self.connection_list(close.saturating_sub(1), |inst| RefKind::Port { inst }, id);
                self.pos = close;
            }
            self.finish(id);
            let _ = module_ref;
            if !self.eat_p(",") {
                break;
            }
        }
        if !self.eat_p(";") {
            let sp = self.prev_span();
            self.error(Span::new(sp.file, sp.end, sp.end), "Expected ';'");
            self.recover_semi();
        }
    }

    /// Named (`.a(x)`, `.a`, `.*`) or ordered connections up to token index `end`.
    fn connection_list(&mut self, end: usize, mk: fn(DeclId) -> RefKind, inst: DeclId) {
        while self.pos < end {
            let before = self.pos;
            if self.at_p(".*") {
                self.bump();
            } else if self.at_p(".") && matches!(self.nth(1).kind, TokKind::Ident | TokKind::Keyword) {
                self.bump();
                let n = self.tok().clone();
                self.bump();
                self.add_ref(vec![Self::seg(&n, Sep::None)], mk(inst));
                if self.at_p("(") {
                    self.paren_group(RefKind::Loose);
                } else {
                    // Implicit `.name` connection also refers to a local signal.
                    self.add_ref(vec![Self::seg(&n, Sep::None)], RefKind::Loose);
                }
            } else {
                self.scan_expr(STOP_COMMA, RefKind::Loose);
            }
            if self.at_p(",") {
                self.bump();
            }
            if self.pos == before {
                self.bump();
            }
        }
    }

    fn subroutine(&mut self, fl: u32, start: Span) {
        let is_task = self.at_kw("task");
        self.bump();
        let mut fl = fl;
        while self.at_kw("static") || self.at_kw("automatic") {
            if self.at_kw("static") {
                fl |= dflags::STATIC;
            }
            self.bump();
        }
        let kind = if is_task { DeclKind::Task } else { DeclKind::Function };
        if self.context() == DeclKind::InterfaceClass {
            fl |= dflags::PROTOTYPE;
        }
        if fl & (dflags::EXTERN | dflags::PURE) != 0 {
            fl |= dflags::PROTOTYPE;
        }
        // Return type (functions) and name, possibly class-qualified.
        let mut ret: Option<TypeRef> = None;
        let name_ahead = |p: &Self| -> bool {
            // name ( ; or Class :: name ...
            let t = p.tok();
            (t.kind == TokKind::Ident || t.is_kw("new")) && (p.nth(1).is_punct("(") || p.nth(1).is_punct(";"))
        };
        let qualified_ahead = |p: &Self| -> bool {
            p.tok().kind == TokKind::Ident
                && p.nth(1).is_punct("::")
                && (p.nth(2).kind == TokKind::Ident || p.nth(2).is_kw("new"))
                && (p.nth(3).is_punct("(") || p.nth(3).is_punct(";"))
        };
        if !is_task && !name_ahead(self) && !qualified_ahead(self) {
            ret = self.parse_type(false, RefKind::Type).map(|ti| {
                self.scan_params(ti.params);
                ti.tref
            });
        }
        let mut owner = None;
        if self.tok().kind == TokKind::Ident && self.nth(1).is_punct("::") {
            let c = self.tok().clone();
            owner = Some(self.add_ref(vec![Self::seg(&c, Sep::None)], RefKind::Type));
            self.bump();
            self.bump();
            fl |= dflags::OUT_OF_BLOCK;
        }
        let name = if self.tok().kind == TokKind::Ident || self.at_kw("new") {
            let n = self.tok().clone();
            self.bump();
            n
        } else {
            let sp = self.tok().span;
            self.error(sp, format!("Expected {} name", kind.label()));
            self.scan_until_semi(RefKind::Loose);
            return;
        };
        let id = self.new_decl(kind, Some(&name), start);
        {
            let d = &mut self.u.decls[id as usize];
            d.flags |= fl;
            d.ty = ret.clone();
            d.owner = owner;
        }
        self.push_scope(id);
        let args_from = self.pos;
        if self.at_p("(") {
            self.arg_list(DeclKind::Argument);
        }
        let args_text = self.text_of(args_from, self.pos);
        let ret_text = ret.as_ref().map(|t| t.text.to_string()).unwrap_or_else(|| if is_task || name.text == "new" { String::new() } else { "logic".into() });
        let detail = if ret_text.is_empty() {
            format!("{} {}{}", kind.label(), name.text, if args_text.is_empty() { "()".into() } else { args_text.to_string() })
        } else {
            format!("{} {} {}{}", kind.label(), ret_text, name.text, if args_text.is_empty() { "()".into() } else { args_text.to_string() })
        };
        self.u.decls[id as usize].detail = SmolStr::new(detail);
        self.expect_p(";");
        if fl & dflags::PROTOTYPE != 0 {
            self.pop_scope();
            self.finish(id);
            return;
        }
        let end = if is_task { "endtask" } else { "endfunction" };
        self.end_stack.push(end);
        self.stmt_list(&[end]);
        self.end_stack.pop();
        if self.at_kw(end) {
            self.bump();
            self.end_label();
        } else {
            let sp = name.span;
            self.error(sp, format!("Missing '{end}'"));
        }
        self.pop_scope();
        self.finish(id);
    }

    /// Argument list of a function, task, covergroup, property or sequence.
    fn arg_list(&mut self, kind: DeclKind) {
        self.bump(); // (
        let mut dir: Option<PortDir> = None;
        let mut last_ty: Option<TypeRef> = None;
        loop {
            self.skip_attributes();
            if self.at_eof() || self.eat_p(")") {
                return;
            }
            let start = self.tok().span;
            if let Some(d) = self.port_dir() {
                if d == PortDir::ConstRef {
                    self.bump();
                }
                self.bump();
                dir = Some(d);
                last_ty = None;
            }
            self.eat_kw("var");
            if self.at_kw("local") {
                self.bump();
            }
            let mut ty = None;
            if !(self.at_ident() && matches!(self.nth(1).text.as_str(), "," | ")" | "=" | "[")) {
                if self.at_kw("untyped") || self.at_kw("sequence") || self.at_kw("property") || self.at_kw("event") && kind != DeclKind::Argument {
                    self.bump();
                } else {
                    ty = self.parse_type(false, RefKind::Type).map(|ti| {
                        self.scan_params(ti.params);
                        ti.tref
                    });
                }
            }
            if ty.is_some() {
                last_ty = ty.clone();
            } else {
                ty = last_ty.clone();
            }
            if self.at_ident() {
                let n = self.tok().clone();
                self.bump();
                let id = self.new_decl(kind, Some(&n), start);
                self.u.decls[id as usize].dir = Some(dir.unwrap_or(PortDir::Input));
                self.u.decls[id as usize].ty = ty;
                self.array_dims(id);
                if self.eat_p("=") {
                    let from = self.pos;
                    self.scan_expr(STOP_COMMA, RefKind::Expr);
                    self.u.decls[id as usize].detail = self.text_of(from, self.pos);
                }
                self.finish(id);
            } else {
                let sp = self.tok().span;
                self.error(sp, "Expected argument name");
                self.scan_expr(STOP_COMMA, RefKind::Loose);
            }
            if !self.eat_p(",") {
                if !self.eat_p(")") {
                    let sp = self.tok().span;
                    self.error(sp, "Expected ',' or ')' in argument list");
                    self.recover_list();
                }
                return;
            }
        }
    }

    fn constraint(&mut self, fl: u32, start: Span) {
        self.bump(); // constraint
        let mut owner = None;
        if self.at_ident() && self.nth(1).is_punct("::") {
            let c = self.tok().clone();
            owner = Some(self.add_ref(vec![Self::seg(&c, Sep::None)], RefKind::Type));
            self.bump();
            self.bump();
        }
        if !self.at_ident() {
            let sp = self.tok().span;
            self.error(sp, "Expected constraint name");
            self.scan_until_semi(RefKind::Loose);
            return;
        }
        let n = self.tok().clone();
        self.bump();
        let id = self.new_decl(DeclKind::Constraint, Some(&n), start);
        self.u.decls[id as usize].flags |= fl | if owner.is_some() { dflags::OUT_OF_BLOCK } else { 0 };
        self.u.decls[id as usize].owner = owner;
        if self.eat_p(";") {
            self.u.decls[id as usize].flags |= dflags::PROTOTYPE;
            self.finish(id);
            return;
        }
        self.push_scope(id);
        if self.at_p("{") {
            self.constraint_block();
        } else {
            let sp = self.tok().span;
            self.error(sp, "Expected '{'");
        }
        self.pop_scope();
        self.finish(id);
    }

    fn constraint_block(&mut self) {
        self.bump(); // {
        while !self.at_eof() && !self.at_p("}") {
            if self.tok().kind == TokKind::Keyword && is_end_kw(&self.tok().text) {
                return;
            }
            let before = self.pos;
            self.constraint_item();
            if self.pos == before {
                self.bump();
            }
        }
        self.eat_p("}");
    }

    fn constraint_item(&mut self) {
        if self.at_p("{") {
            self.constraint_block();
            return;
        }
        if self.at_kw("if") {
            self.bump();
            if self.at_p("(") {
                self.paren_group(RefKind::Expr);
            }
            self.constraint_item();
            if self.eat_kw("else") {
                self.constraint_item();
            }
            return;
        }
        if self.at_kw("foreach") {
            let start = self.tok().span;
            self.bump();
            let blk = self.new_decl(DeclKind::Block, None, start);
            self.push_scope(blk);
            self.foreach_header();
            self.constraint_item();
            self.pop_scope();
            self.finish(blk);
            return;
        }
        if self.at_kw("soft") {
            self.bump();
        }
        if self.at_kw("solve") || (self.at_kw("disable") && self.nth(1).is_kw("soft")) {
            self.bump();
            self.scan_until_semi(RefKind::Expr);
            return;
        }
        if self.at_kw("unique") {
            self.bump();
        }
        self.scan_expr(STOP_NONE, RefKind::Expr);
        self.eat_p(";");
    }

    /// `( arr[i, j] )` of a foreach: declares the loop variables in the current scope.
    fn foreach_header(&mut self) {
        if !self.eat_p("(") {
            return;
        }
        // Array reference up to the first `[` of the loop variables.
        let mut segs = Vec::new();
        let mut sep = Sep::None;
        while matches!(self.tok().kind, TokKind::Ident) || self.at_kw("this") || self.at_kw("super") {
            segs.push(Self::seg(self.tok(), sep));
            self.bump();
            if self.at_p(".") || self.at_p("::") {
                sep = if self.at_p(".") { Sep::Dot } else { Sep::Scope };
                self.bump();
                continue;
            }
            if self.at_p("[") {
                // Either a select inside the array path or the loop variable list; the
                // loop variable list is the last bracket group before `)`.
                let close = self.skip_group_from(self.pos);
                if self.toks.get(close).is_some_and(|t| t.is_punct(".")) {
                    self.bump();
                    self.scan_expr(STOP_NONE, RefKind::Expr);
                    self.eat_p("]");
                    self.bump(); // .
                    sep = Sep::Dot;
                    continue;
                }
            }
            break;
        }
        if !segs.is_empty() {
            self.add_ref(segs, RefKind::Expr);
        }
        while self.at_p("[") {
            self.bump();
            loop {
                if self.at_ident() {
                    let n = self.tok().clone();
                    self.bump();
                    let id = self.new_decl(DeclKind::Variable, Some(&n), n.span);
                    self.u.decls[id as usize].detail = SmolStr::new_static("loop variable");
                    self.u.decls[id as usize].ty = Some(TypeRef { text: SmolStr::new_static("int"), ref_id: None, inline: None });
                }
                if self.eat_p(",") {
                    continue;
                }
                break;
            }
            if !self.eat_p("]") {
                self.scan_expr(STOP_NONE, RefKind::Loose);
                self.eat_p("]");
            }
        }
        self.expect_p(")");
    }

    fn covergroup(&mut self) {
        let start = self.tok().span;
        self.bump();
        if !self.at_ident() {
            let sp = self.tok().span;
            self.error(sp, "Expected covergroup name");
            self.skip_until_kw("endgroup");
            return;
        }
        let n = self.tok().clone();
        self.bump();
        let id = self.new_decl(DeclKind::Covergroup, Some(&n), start);
        self.push_scope(id);
        if self.at_p("(") {
            self.arg_list(DeclKind::Argument);
        }
        if self.at_p("@") || self.at_p("@@") {
            self.bump();
            if self.at_p("(") {
                self.paren_group(RefKind::Loose);
            } else {
                self.scan_expr(STOP_NONE, RefKind::Loose);
            }
        } else if self.at_kw("with") {
            self.bump();
            self.eat_kw("function");
            if self.tok().kind == TokKind::Ident {
                self.bump(); // sample
            }
            if self.at_p("(") {
                self.arg_list(DeclKind::Argument);
            }
        }
        self.expect_p(";");
        self.end_stack.push("endgroup");
        while !self.at_eof() && !self.at_kw("endgroup") {
            if self.tok().kind == TokKind::Keyword && is_end_kw(&self.tok().text) {
                break;
            }
            let before = self.pos;
            self.cover_item();
            if self.pos == before {
                self.bump();
            }
        }
        self.end_stack.pop();
        if self.eat_kw("endgroup") {
            self.end_label();
        } else {
            self.error(n.span, "Missing 'endgroup'");
        }
        self.pop_scope();
        self.finish(id);
    }

    fn cover_item(&mut self) {
        self.skip_attributes();
        let start = self.tok().span;
        let mut label = None;
        if self.at_ident() && self.nth(1).is_punct(":") {
            label = Some(self.tok().clone());
            self.bump();
            self.bump();
        }
        if self.at_kw("coverpoint") || self.at_kw("cross") {
            let is_cross = self.at_kw("cross");
            self.bump();
            let id = label.as_ref().map(|l| {
                let k = if is_cross { DeclKind::Cross } else { DeclKind::Coverpoint };
                self.new_decl(k, Some(l), start)
            });
            let stop = Stop { comma: false, colon: false, lbrace: true, iff: true, eq: false };
            loop {
                self.scan_expr(stop, RefKind::Loose);
                if is_cross && self.eat_p(",") {
                    continue;
                }
                break;
            }
            if self.eat_kw("iff") && self.at_p("(") {
                self.paren_group(RefKind::Expr);
            }
            if let Some(id) = id {
                self.push_scope(id);
            }
            if self.at_p("{") {
                self.bins_block();
            } else {
                self.eat_p(";");
            }
            if let Some(id) = id {
                self.pop_scope();
                self.finish(id);
            }
            return;
        }
        if self.at_kw("function") {
            let sp = self.tok().span;
            self.subroutine(0, sp);
            return;
        }
        self.scan_until_semi(RefKind::Loose);
    }

    fn bins_block(&mut self) {
        self.bump(); // {
        while !self.at_eof() && !self.at_p("}") {
            let before = self.pos;
            self.eat_kw("wildcard");
            if matches!(self.tok().text.as_str(), "bins" | "illegal_bins" | "ignore_bins") {
                self.bump();
                if self.at_ident() {
                    self.bump(); // bin name
                }
                if self.at_p("[") {
                    let end = self.skip_group_from(self.pos);
                    self.pos = end;
                }
                self.eat_p("=");
            } else if self.at_kw("function") {
                let sp = self.tok().span;
                self.subroutine(0, sp);
                continue;
            }
            self.scan_until_semi(RefKind::Loose);
            if self.pos == before {
                self.bump();
            }
        }
        self.eat_p("}");
    }

    fn assertion_decl(&mut self, kind: DeclKind, end: &'static str) {
        let start = self.tok().span;
        self.bump();
        if !self.at_ident() {
            self.skip_until_kw(end);
            return;
        }
        let n = self.tok().clone();
        self.bump();
        let id = self.new_decl(kind, Some(&n), start);
        self.push_scope(id);
        if self.at_p("(") {
            self.arg_list(DeclKind::Argument);
        }
        self.eat_p(";");
        // Local variable declarations come first in the body.
        while (self.tok().kind == TokKind::Keyword && is_builtin_type_kw(&self.tok().text)) || (self.at_ident() && self.decl_lookahead() == Lookahead::Decl) {
            let s = self.tok().span;
            self.data_decl(DeclKind::Variable, 0, s);
        }
        while !self.at_eof() && !self.at_kw(end) {
            let t = self.tok();
            if t.kind == TokKind::Keyword && is_end_kw(&t.text) && t.text != end {
                break;
            }
            self.scan_expr(STOP_NONE, RefKind::Loose);
            if !self.at_kw(end) {
                self.bump();
            }
        }
        if self.eat_kw(end) {
            self.end_label();
        }
        self.pop_scope();
        self.finish(id);
    }

    fn clocking(&mut self, start: Span) {
        self.bump(); // clocking
        let name = if self.at_ident() {
            let n = self.tok().clone();
            self.bump();
            Some(n)
        } else {
            None
        };
        if self.eat_p(";") {
            // `default clocking cb;` refers to an existing block.
            if let Some(n) = name {
                self.add_ref(vec![Self::seg(&n, Sep::None)], RefKind::Loose);
            }
            return;
        }
        let id = self.new_decl(DeclKind::ClockingBlock, name.as_ref(), start);
        self.push_scope(id);
        if self.eat_p("@") {
            if self.at_p("(") {
                self.paren_group(RefKind::Expr);
            } else {
                self.scan_expr(STOP_NONE, RefKind::Expr);
            }
        }
        self.eat_p(";");
        while !self.at_eof() && !self.at_kw("endclocking") {
            let t = self.tok();
            if t.kind == TokKind::Keyword && is_end_kw(&t.text) {
                break;
            }
            let before = self.pos;
            self.clocking_item();
            if self.pos == before {
                self.bump();
            }
        }
        if self.eat_kw("endclocking") {
            self.end_label();
        }
        self.pop_scope();
        self.finish(id);
    }

    fn clocking_item(&mut self) {
        if self.at_kw("default") {
            self.bump();
            self.scan_until_semi(RefKind::Loose);
            return;
        }
        let dir = self.port_dir();
        if dir.is_none() {
            if self.at_kw("property") || self.at_kw("sequence") {
                let k = if self.at_kw("property") { DeclKind::Property } else { DeclKind::Sequence };
                let e = if k == DeclKind::Property { "endproperty" } else { "endsequence" };
                self.assertion_decl(k, e);
                return;
            }
            self.scan_until_semi(RefKind::Loose);
            return;
        }
        self.bump();
        if self.at_kw("output") {
            // input #1 output #2 sig;
            self.bump();
        }
        // Skew: edge and/or delay.
        while matches!(self.tok().text.as_str(), "posedge" | "negedge" | "edge") {
            self.bump();
        }
        if self.at_p("#") {
            self.bump();
            if self.at_p("(") {
                self.paren_group(RefKind::Expr);
            } else {
                self.bump();
            }
        }
        if self.at_kw("output") {
            self.bump();
            while matches!(self.tok().text.as_str(), "posedge" | "negedge" | "edge") {
                self.bump();
            }
            if self.at_p("#") {
                self.bump();
                if self.at_p("(") {
                    self.paren_group(RefKind::Expr);
                } else {
                    self.bump();
                }
            }
        }
        loop {
            if !self.at_ident() {
                break;
            }
            let n = self.tok().clone();
            self.bump();
            let id = self.new_decl(DeclKind::Variable, Some(&n), n.span);
            self.u.decls[id as usize].dir = dir;
            self.u.decls[id as usize].detail = SmolStr::new_static("clocking signal");
            if self.eat_p("=") {
                self.scan_expr(STOP_COMMA, RefKind::Loose);
            } else {
                // The clockvar refers to the signal of the same name in the enclosing scope.
                self.add_ref(vec![Self::seg(&n, Sep::None)], RefKind::Loose);
            }
            self.finish(id);
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(";");
    }

    fn modport(&mut self) {
        self.bump();
        loop {
            if !self.at_ident() {
                break;
            }
            let n = self.tok().clone();
            self.bump();
            let id = self.new_decl(DeclKind::Modport, Some(&n), n.span);
            self.push_scope(id);
            if self.eat_p("(") {
                let mut dir = None;
                loop {
                    if self.at_eof() || self.eat_p(")") {
                        break;
                    }
                    if let Some(d) = self.port_dir() {
                        self.bump();
                        dir = Some(d);
                    } else if self.at_kw("import") || self.at_kw("export") {
                        self.bump();
                        dir = None;
                        if self.at_kw("function") || self.at_kw("task") {
                            // Prototype form.
                            let sp = self.tok().span;
                            self.subroutine(dflags::PROTOTYPE, sp);
                            continue;
                        }
                    } else if self.at_kw("clocking") {
                        self.bump();
                    }
                    if self.at_p(".") && self.nth(1).kind == TokKind::Ident {
                        self.bump();
                        let p = self.tok().clone();
                        self.bump();
                        let pid = self.new_decl(DeclKind::Port, Some(&p), p.span);
                        self.u.decls[pid as usize].dir = dir;
                        if self.at_p("(") {
                            self.paren_group(RefKind::Expr);
                        }
                    } else if self.at_ident() {
                        let p = self.tok().clone();
                        self.bump();
                        let pid = self.new_decl(DeclKind::Port, Some(&p), p.span);
                        self.u.decls[pid as usize].dir = dir;
                        // Refers to the interface item of the same name.
                        let parent = self.u.decls[id as usize].parent.unwrap();
                        self.u.refs.push(RefChain { segs: vec![Self::seg(&p, Sep::None)], scope: parent, kind: RefKind::Loose });
                    } else if !self.at_p(",") {
                        self.bump();
                    }
                    if !self.eat_p(",") {
                        self.expect_p(")");
                        break;
                    }
                }
            }
            self.pop_scope();
            self.finish(id);
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(";");
    }

    // ----------------------------------------------------------------- generate

    fn genblk_name(&mut self) -> SmolStr {
        let c = self.genblk.last_mut().unwrap();
        *c += 1;
        SmolStr::new(format!("genblk{c}"))
    }

    /// Body of a generate construct: a `begin ... end` block or a single item.
    fn generate_body(&mut self, holder: Option<DeclId>) {
        if self.at_kw("begin") {
            let start = self.tok().span;
            self.bump();
            let label = if self.at_p(":") && self.nth(1).kind == TokKind::Ident {
                self.bump();
                let l = self.tok().clone();
                self.bump();
                Some(l)
            } else {
                None
            };
            let id = match holder {
                Some(h) => {
                    if let Some(l) = &label {
                        let d = &mut self.u.decls[h as usize];
                        d.name = l.text.clone();
                        d.name_span = l.span;
                    }
                    h
                }
                None => {
                    let id = self.new_decl(DeclKind::Generate, label.as_ref(), start);
                    if label.is_none() {
                        let n = self.genblk_name();
                        self.u.decls[id as usize].name = n;
                        self.u.decls[id as usize].flags |= dflags::FROM_MACRO;
                    }
                    id
                }
            };
            self.push_scope(id);
            self.items(Some("end"));
            self.pop_scope();
            self.finish(id);
        } else {
            match holder {
                Some(h) => {
                    self.push_scope(h);
                    self.item();
                    self.pop_scope();
                    self.finish(h);
                }
                None => self.item(),
            }
        }
    }

    fn generate_for(&mut self) {
        let start = self.tok().span;
        self.bump();
        let id = self.new_decl(DeclKind::Generate, None, start);
        let n = self.genblk_name();
        self.u.decls[id as usize].name = n;
        self.u.decls[id as usize].flags |= dflags::FROM_MACRO;
        self.push_scope(id);
        if self.eat_p("(") {
            if self.eat_kw("genvar") && self.at_ident() {
                let g = self.tok().clone();
                let gid = self.new_decl(DeclKind::Genvar, Some(&g), g.span);
                self.u.decls[gid as usize].detail = SmolStr::new_static("genvar");
            }
            self.scan_expr(STOP_NONE, RefKind::Expr);
            self.eat_p(";");
            self.scan_expr(STOP_NONE, RefKind::Expr);
            self.eat_p(";");
            self.scan_expr(STOP_NONE, RefKind::Expr);
            self.expect_p(")");
        }
        self.pop_scope();
        // A label on the block names the generate scope.
        if self.at_kw("begin") && self.nth(1).is_punct(":") {
            self.u.decls[id as usize].flags &= !dflags::FROM_MACRO;
        }
        self.generate_body(Some(id));
    }

    fn generate_if(&mut self) {
        self.bump();
        if self.at_p("(") {
            self.paren_group(RefKind::Expr);
        }
        self.generate_body(None);
        if self.eat_kw("else") {
            if self.at_kw("if") {
                self.generate_if();
            } else {
                self.generate_body(None);
            }
        }
    }

    fn generate_case(&mut self) {
        self.bump();
        if self.at_p("(") {
            self.paren_group(RefKind::Expr);
        }
        while !self.at_eof() && !self.at_kw("endcase") {
            let t = self.tok();
            if t.kind == TokKind::Keyword && is_end_kw(&t.text) {
                break;
            }
            let before = self.pos;
            if self.eat_kw("default") {
                self.eat_p(":");
            } else {
                self.scan_expr(Stop { colon: true, comma: false, lbrace: false, iff: false, eq: false }, RefKind::Expr);
                while self.eat_p(",") {
                    self.scan_expr(Stop { colon: true, comma: true, lbrace: false, iff: false, eq: false }, RefKind::Expr);
                }
                self.expect_p(":");
            }
            self.generate_body(None);
            if self.pos == before {
                self.bump();
            }
        }
        self.eat_kw("endcase");
    }

    fn generate_block(&mut self, _start: Span) {
        self.generate_body(None);
    }

    // ----------------------------------------------------------------- statements

    /// Statements until one of `ends` (not consumed) or a foreign end keyword.
    fn stmt_list(&mut self, ends: &[&str]) {
        loop {
            let t = self.tok();
            if t.kind == TokKind::Eof {
                return;
            }
            if t.kind == TokKind::Keyword {
                if ends.contains(&t.text.as_str()) {
                    return;
                }
                if is_end_kw(&t.text) {
                    let text = t.text.clone();
                    if self.end_stack.iter().any(|e| *e == text) {
                        return;
                    }
                    let sp = t.span;
                    self.error(sp, format!("Unexpected '{text}'"));
                    self.bump();
                    continue;
                }
                // A new design element, class or subroutine means the body was not closed.
                if matches!(
                    t.text.as_str(),
                    "module" | "package" | "class" | "interface" | "program" | "function" | "task" | "constraint" | "covergroup"
                ) {
                    return;
                }
            }
            let before = self.pos;
            self.stmt();
            if self.pos == before {
                let t = self.tok().clone();
                self.error(t.span, format!("Unexpected {}", Self::describe(&t)));
                self.bump();
            }
        }
    }

    fn stmt(&mut self) {
        if self.stmt_depth >= MAX_STMT_DEPTH {
            // Skip one token per call; the levels above carry on with the rest.
            if !self.too_deep {
                self.too_deep = true;
                let span = self.tok().span;
                self.error(span, "Statements are nested too deeply; the rest of this block is skipped");
            }
            self.bump();
            return;
        }
        self.stmt_depth += 1;
        self.stmt_inner();
        self.stmt_depth -= 1;
    }

    fn stmt_inner(&mut self) {
        self.skip_attributes();
        let t = self.tok().clone();
        match t.kind {
            TokKind::Eof => {}
            TokKind::Punct => match t.text.as_str() {
                ";" => self.bump(),
                "@" | "@@" => {
                    self.bump();
                    self.event_expr();
                    self.stmt();
                }
                "#" | "##" => {
                    self.bump();
                    if self.at_p("(") {
                        self.paren_group(RefKind::Expr);
                    } else if self.at_p("[") {
                        let end = self.skip_group_from(self.pos);
                        self.pos = end;
                    } else {
                        // number, identifier or `1step`
                        if self.at_ident() {
                            self.scan_expr_operand();
                        } else {
                            self.bump();
                        }
                    }
                    self.stmt();
                }
                "->" | "->>" => {
                    self.bump();
                    self.scan_until_semi(RefKind::Expr);
                }
                _ => self.expr_stmt(RefKind::Expr),
            },
            TokKind::Keyword => self.keyword_stmt(&t),
            TokKind::Ident => {
                if self.nth(1).is_punct(":") && !self.nth(2).is_punct(":") && !self.nth(2).is_punct("=") {
                    // Statement label.
                    self.bump();
                    self.bump();
                    self.stmt();
                    return;
                }
                if self.decl_lookahead() == Lookahead::Decl {
                    let sp = t.span;
                    self.data_decl(DeclKind::Variable, 0, sp);
                } else {
                    self.expr_stmt(RefKind::Expr);
                }
            }
            _ => self.expr_stmt(RefKind::Expr),
        }
    }

    fn keyword_stmt(&mut self, t: &Token) {
        let k = t.text.as_str();
        match k {
            "begin" => self.block(&["end"]),
            "fork" => self.block(&["join", "join_any", "join_none"]),
            "if" => {
                self.bump();
                if self.at_p("(") {
                    self.paren_group(RefKind::Expr);
                } else {
                    let sp = self.tok().span;
                    self.error(sp, "Expected '(' after 'if'");
                }
                self.stmt();
                if self.eat_kw("else") {
                    self.stmt();
                }
            }
            "unique" | "unique0" | "priority" => {
                self.bump();
                self.stmt();
            }
            "case" | "casex" | "casez" => self.case_stmt(),
            "randcase" => {
                self.bump();
                while !self.at_eof() && !self.at_kw("endcase") {
                    let before = self.pos;
                    self.scan_expr(Stop { colon: true, ..STOP_NONE }, RefKind::Expr);
                    self.expect_p(":");
                    self.stmt();
                    if self.pos == before {
                        self.bump();
                    }
                }
                self.eat_kw("endcase");
            }
            "for" => self.for_stmt(),
            "foreach" => {
                let start = t.span;
                self.bump();
                let blk = self.new_decl(DeclKind::Block, None, start);
                self.push_scope(blk);
                self.foreach_header();
                self.stmt();
                self.pop_scope();
                self.finish(blk);
            }
            "while" | "repeat" => {
                self.bump();
                if self.at_p("(") {
                    self.paren_group(RefKind::Expr);
                }
                self.stmt();
            }
            "do" => {
                self.bump();
                self.stmt();
                if self.eat_kw("while") {
                    if self.at_p("(") {
                        self.paren_group(RefKind::Expr);
                    }
                    self.expect_p(";");
                }
            }
            "forever" => {
                self.bump();
                self.stmt();
            }
            "wait" => {
                self.bump();
                if self.eat_kw("fork") {
                    self.expect_p(";");
                } else {
                    if self.at_p("(") {
                        self.paren_group(RefKind::Expr);
                    }
                    self.stmt();
                }
            }
            "wait_order" => {
                self.bump();
                if self.at_p("(") {
                    self.paren_group(RefKind::Expr);
                }
                self.action_block();
            }
            "return" | "break" | "continue" => {
                self.bump();
                self.scan_until_semi(RefKind::Expr);
            }
            "disable" => {
                self.bump();
                if self.eat_kw("fork") {
                    self.expect_p(";");
                } else {
                    self.scan_until_semi(RefKind::Loose);
                }
            }
            "assert" | "assume" | "cover" | "restrict" | "expect" => {
                self.bump();
                while matches!(self.tok().text.as_str(), "property" | "sequence" | "final") && self.tok().kind == TokKind::Keyword {
                    self.bump();
                }
                if self.at_p("#") {
                    self.bump();
                    self.bump();
                }
                if self.at_p("(") {
                    self.paren_group(RefKind::Loose);
                }
                self.action_block();
            }
            "typedef" => self.typedef(),
            "parameter" | "localparam" => self.param_decl(),
            "import" => self.import_item(),
            "let" => self.keyword_item(t),
            "automatic" | "static" | "const" | "var" | "virtual" | "struct" | "union" | "enum" => {
                self.data_decl(DeclKind::Variable, 0, t.span);
            }
            "void" if self.nth(1).is_punct("'") => self.expr_stmt(RefKind::Expr),
            _ if is_builtin_type_kw(k) && !self.nth(1).is_punct("'") => {
                self.data_decl(DeclKind::Variable, 0, t.span);
            }
            "randsequence" => self.skip_until_kw("endsequence"),
            "force" | "release" | "assign" | "deassign" => {
                self.bump();
                self.scan_until_semi(RefKind::Expr);
            }
            "else" => {
                self.error(t.span, "Unexpected 'else'");
                self.bump();
            }
            "input" | "output" | "inout" | "ref" => self.port_decl_nonansi(),
            "covergroup" => self.covergroup(),
            _ if is_end_kw(k) => {}
            _ => self.expr_stmt(RefKind::Expr),
        }
    }

    fn action_block(&mut self) {
        if self.at_kw("else") {
            self.bump();
            self.stmt();
            return;
        }
        self.stmt();
        if self.eat_kw("else") {
            self.stmt();
        }
    }

    fn block(&mut self, ends: &[&'static str]) {
        let start = self.tok().span;
        self.bump();
        let label = if self.at_p(":") && matches!(self.nth(1).kind, TokKind::Ident) {
            self.bump();
            let l = self.tok().clone();
            self.bump();
            Some(l)
        } else {
            None
        };
        let id = self.new_decl(DeclKind::Block, label.as_ref(), start);
        self.push_scope(id);
        for e in ends {
            self.end_stack.push(e);
        }
        self.stmt_list(ends);
        for _ in ends {
            self.end_stack.pop();
        }
        if ends.iter().any(|e| self.at_kw(e)) {
            self.bump();
            self.end_label();
        } else {
            let sp = start;
            self.error(sp, format!("Missing '{}'", ends[0]));
        }
        self.pop_scope();
        self.finish(id);
    }

    fn case_stmt(&mut self) {
        self.bump();
        if self.at_p("(") {
            self.paren_group(RefKind::Expr);
        }
        if self.at_kw("inside") || self.at_kw("matches") {
            self.bump();
        }
        self.end_stack.push("endcase");
        while !self.at_eof() && !self.at_kw("endcase") {
            let t = self.tok();
            if t.kind == TokKind::Keyword && is_end_kw(&t.text) {
                break;
            }
            let before = self.pos;
            if self.eat_kw("default") {
                self.eat_p(":");
            } else {
                loop {
                    self.scan_expr(Stop { colon: true, comma: true, ..STOP_NONE }, RefKind::Expr);
                    if !self.eat_p(",") {
                        break;
                    }
                }
                if !self.eat_p(":") {
                    let sp = self.tok().span;
                    self.error(sp, "Expected ':' after case item");
                }
            }
            self.stmt();
            if self.pos == before {
                self.bump();
            }
        }
        self.end_stack.pop();
        if !self.eat_kw("endcase") {
            let sp = self.prev_span();
            self.error(sp, "Missing 'endcase'");
        }
    }

    fn for_stmt(&mut self) {
        let start = self.tok().span;
        self.bump();
        let blk = self.new_decl(DeclKind::Block, None, start);
        self.push_scope(blk);
        if self.eat_p("(") {
            // Initialization: declarations or assignments.
            loop {
                let is_decl = (self.tok().kind == TokKind::Keyword && (is_builtin_type_kw(&self.tok().text) || self.at_kw("var") || self.at_kw("automatic")))
                    || (self.at_ident() && self.nth(1).kind == TokKind::Ident);
                if is_decl {
                    self.eat_kw("var");
                    self.eat_kw("automatic");
                    let ty = self.parse_type(false, RefKind::Type).map(|ti| {
                        self.scan_params(ti.params);
                        ti.tref
                    });
                    if self.at_ident() {
                        let n = self.tok().clone();
                        self.bump();
                        let id = self.new_decl(DeclKind::Variable, Some(&n), n.span);
                        self.u.decls[id as usize].ty = ty;
                        self.finish(id);
                    }
                    if self.eat_p("=") {
                        self.scan_expr(STOP_COMMA, RefKind::Expr);
                    }
                } else {
                    self.scan_expr(STOP_COMMA, RefKind::Expr);
                }
                if !self.eat_p(",") {
                    break;
                }
            }
            self.eat_p(";");
            self.scan_expr(STOP_NONE, RefKind::Expr);
            self.eat_p(";");
            loop {
                self.scan_expr(STOP_COMMA, RefKind::Expr);
                if !self.eat_p(",") {
                    break;
                }
            }
            self.expect_p(")");
        }
        self.stmt();
        self.pop_scope();
        self.finish(blk);
    }

    fn event_expr(&mut self) {
        if self.at_p("(") {
            self.paren_group(RefKind::Expr);
        } else if self.at_p("*") || self.at_p("(*") {
            self.bump();
            self.eat_p(")");
        } else {
            self.scan_expr_operand();
        }
    }

    // ----------------------------------------------------------------- expressions

    fn expr_stmt(&mut self, kind: RefKind) {
        self.scan_expr(STOP_NONE, kind);
        if self.eat_p(";") {
            return;
        }
        let t = self.tok().clone();
        let sp = self.prev_span();
        if t.kind == TokKind::Eof || t.kind == TokKind::Keyword || t.is_punct("}") || t.is_punct(")") || t.is_punct("]") {
            self.error(Span::new(sp.file, sp.end, sp.end), "Expected ';'");
            if t.is_punct(")") || t.is_punct("]") || t.is_punct("}") {
                // Stray closing bracket.
                self.bump();
            }
        } else {
            self.error(t.span, format!("Unexpected {}", Self::describe(&t)));
            self.recover_semi();
        }
    }

    fn scan_until_semi(&mut self, kind: RefKind) {
        loop {
            self.scan_expr(STOP_NONE, kind);
            if self.eat_p(";") || self.at_eof() {
                return;
            }
            let t = self.tok();
            if t.kind == TokKind::Keyword && (is_end_kw(&t.text) || is_item_kw(&t.text)) {
                return;
            }
            // Unbalanced closing bracket or similar: skip it.
            self.bump();
        }
    }

    fn recover_semi(&mut self) {
        let mut depth = 0i32;
        while !self.at_eof() {
            let t = self.tok();
            if t.kind == TokKind::Punct {
                match t.text.as_str() {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth -= 1,
                    ";" if depth <= 0 => {
                        self.bump();
                        return;
                    }
                    _ => {}
                }
            } else if t.kind == TokKind::Keyword && (is_end_kw(&t.text) || is_item_kw(&t.text)) {
                return;
            }
            self.bump();
        }
    }

    /// `( ... )` with the references inside.
    fn paren_group(&mut self, kind: RefKind) {
        if !self.eat_p("(") {
            return;
        }
        loop {
            self.scan_expr(STOP_NONE, kind);
            if self.eat_p(")") {
                return;
            }
            let t = self.tok();
            if t.kind == TokKind::Eof || t.is_punct(";") || (t.kind == TokKind::Keyword && (is_end_kw(&t.text) || matches!(t.text.as_str(), "begin"))) {
                let sp = self.prev_span();
                self.error(Span::new(sp.file, sp.end, sp.end), "Expected ')'");
                return;
            }
            // `,` `:` `]` `}` at depth 0 of this group.
            self.bump();
        }
    }

    /// One operand (an identifier chain) used after `@` or `#`.
    fn scan_expr_operand(&mut self) {
        if self.at_ident() || self.at_kw("this") || self.at_kw("super") {
            self.chain(RefKind::Expr);
        } else {
            self.bump();
        }
    }

    /// Scans an expression, recording reference chains, up to a stop token at depth 0.
    fn scan_expr(&mut self, stop: Stop, kind: RefKind) {
        // Open brackets: b'(' b'[' b'{', or b'\'' for an assignment pattern `'{`.
        let mut stack: Vec<u8> = Vec::new();
        let mut last_chain: Option<RefId> = None;
        loop {
            let t = self.tok();
            if t.kind == TokKind::Eof {
                return;
            }
            let depth0 = stack.is_empty();
            match t.kind {
                TokKind::Punct => {
                    let p = t.text.as_str();
                    if depth0 {
                        match p {
                            ";" | ")" | "]" | "}" => return,
                            "," if stop.comma => return,
                            ":" if stop.colon => return,
                            "{" if stop.lbrace => return,
                            "=" if stop.eq => return,
                            _ => {}
                        }
                    } else if p == ";" {
                        // `;` inside `{}` of a constraint-like block (`x -> {a; b;}`) is fine,
                        // inside parentheses it means a missing `)`.
                        if stack.last() == Some(&b'{') {
                            self.bump();
                            continue;
                        }
                        return;
                    }
                    match p {
                        "(" => {
                            stack.push(b'(');
                            self.bump();
                        }
                        "[" => {
                            stack.push(b'[');
                            self.bump();
                        }
                        "{" => {
                            stack.push(b'{');
                            self.bump();
                        }
                        "'" if self.nth(1).is_punct("{") => {
                            self.bump();
                            self.bump();
                            stack.push(b'\'');
                        }
                        ")" | "]" | "}" => {
                            stack.pop();
                            self.bump();
                        }
                        "." if matches!(self.nth(1).kind, TokKind::Ident) => {
                            // Named argument/port binding `.name(...)`.
                            self.bump();
                            self.bump();
                        }
                        _ => self.bump(),
                    }
                }
                TokKind::Ident => {
                    // Member name in an assignment pattern: '{ a: 1 }
                    if stack.last() == Some(&b'\'') && self.nth(1).is_punct(":") {
                        self.bump();
                        continue;
                    }
                    last_chain = Some(self.chain(kind));
                }
                TokKind::Keyword => {
                    let k = t.text.as_str();
                    if depth0 && (is_end_kw(k) || is_item_kw(k) || matches!(k, "begin" | "else" | "fork")) {
                        return;
                    }
                    if !depth0 && matches!(k, "begin" | "end" | "endfunction" | "endtask" | "endclass" | "endmodule" | "endpackage") {
                        return;
                    }
                    if stop.iff && k == "iff" && depth0 {
                        return;
                    }
                    match k {
                        "this" | "super" => {
                            last_chain = Some(self.chain(kind));
                        }
                        "local" if self.nth(1).is_punct("::") => {
                            last_chain = Some(self.chain(kind));
                        }
                        "with" => {
                            self.bump();
                            self.with_clause(last_chain, kind);
                        }
                        "type" if self.nth(1).is_punct("(") => {
                            self.bump();
                        }
                        "default" if self.nth(1).is_punct(":") => {
                            self.bump();
                            self.bump();
                        }
                        _ => self.bump(),
                    }
                }
                TokKind::SysIdent => {
                    if t.text == "$unit" && self.nth(1).is_punct("::") {
                        self.bump();
                        self.bump();
                        continue;
                    }
                    self.bump();
                }
                _ => self.bump(),
            }
        }
    }

    /// `with (...)` / `with {...}` after `randomize()` or an array method.
    fn with_clause(&mut self, target: Option<RefId>, kind: RefKind) {
        let (open, close) = if self.at_p("(") { ("(", ")") } else if self.at_p("{") { ("{", "}") } else { return };
        // Randomization target: the chain before `.randomize`.
        let mut with_kind = RefKind::Loose;
        if let Some(r) = target {
            let segs = &self.u.refs[r as usize].segs;
            if segs.last().is_some_and(|s| s.name == "randomize") {
                let base: Vec<RefSeg> = segs[..segs.len() - 1].to_vec();
                if !base.is_empty() {
                    let t = self.add_ref_hidden(base);
                    with_kind = RefKind::With { target: t };
                } else {
                    with_kind = kind;
                }
            }
        }
        let _ = open;
        self.bump();
        if close == "}" {
            while !self.at_eof() && !self.at_p("}") {
                let before = self.pos;
                let saved = self.kind_override.replace(with_kind);
                self.constraint_item();
                self.kind_override = saved;
                if self.pos == before {
                    self.bump();
                }
            }
            self.eat_p("}");
        } else {
            loop {
                self.scan_expr(STOP_NONE, with_kind);
                if self.eat_p(")") || self.at_eof() || self.at_p(";") {
                    return;
                }
                self.bump();
            }
        }
    }

    /// Reference used only as a resolution aid (not reported, not navigated).
    fn add_ref_hidden(&mut self, segs: Vec<RefSeg>) -> RefId {
        let id = self.u.refs.len() as RefId;
        self.u.refs.push(RefChain { segs, scope: self.scope(), kind: RefKind::Loose });
        id
    }

    /// Identifier chain: `a.b[i].c(...)`, `pkg::x`, `C#(T)::m`, `this.x`, `super.new()`.
    fn chain(&mut self, kind: RefKind) -> RefId {
        let kind = self.kind_override.unwrap_or(kind);
        let mut segs: Vec<RefSeg> = vec![Self::seg(self.tok(), Sep::None)];
        self.bump();
        let mut cast = false;
        loop {
            if self.at_p("#") && self.nth(1).is_punct("(") {
                let open = self.pos + 1;
                let end = self.skip_group_from(open);
                // Only a parameterized type when followed by `::` (or used as a type).
                self.scan_params(Some((open + 1, end.saturating_sub(1))));
                self.pos = end;
                continue;
            }
            if self.at_p("::") && (matches!(self.nth(1).kind, TokKind::Ident) || self.nth(1).is_kw("new")) {
                self.bump();
                segs.push(Self::seg(self.tok(), Sep::Scope));
                self.bump();
                continue;
            }
            if self.at_p(".") && (matches!(self.nth(1).kind, TokKind::Ident) || self.nth(1).is_kw("new") || self.nth(1).is_kw("super")) {
                self.bump();
                segs.push(Self::seg(self.tok(), Sep::Dot));
                self.bump();
                continue;
            }
            if self.at_p("[") {
                if let Some(l) = segs.last_mut() {
                    l.selects = l.selects.saturating_add(1);
                }
                self.bump();
                self.scan_expr(STOP_NONE, kind);
                if !self.eat_p("]") {
                    // Unbalanced select; stop the chain.
                    break;
                }
                continue;
            }
            if self.at_p("(") {
                if let Some(l) = segs.last_mut() {
                    l.called = true;
                }
                let iter_method = segs.last().is_some_and(|s| ARRAY_ITER_METHODS.contains(&s.name.as_str())) && segs.len() > 1;
                self.bump();
                loop {
                    self.scan_expr(STOP_NONE, if iter_method { RefKind::Loose } else { kind });
                    if self.eat_p(")") {
                        break;
                    }
                    if self.at_p(",") || self.at_p(":") {
                        self.bump();
                        continue;
                    }
                    break;
                }
                continue;
            }
            if self.at_p("'") && self.nth(1).is_punct("(") {
                cast = true;
                break;
            }
            break;
        }
        let kind = if cast { RefKind::Type } else { kind };
        self.add_ref(segs, kind)
    }

    // ----------------------------------------------------------------- lookahead

    /// Decides whether an identifier at the cursor starts a declaration (`T x;`), an
    /// instantiation (`M #(..) u (...)`), or something else.
    fn decl_lookahead(&self) -> Lookahead {
        let mut i = self.pos;
        let toks = self.toks;
        let get = |i: usize| toks.get(i);
        if !get(i).is_some_and(|t| t.kind == TokKind::Ident) {
            return Lookahead::None;
        }
        i += 1;
        loop {
            match get(i) {
                Some(t) if t.is_punct("#") && get(i + 1).is_some_and(|t| t.is_punct("(")) => {
                    i = self.skip_group_from(i + 1);
                }
                Some(t) if t.is_punct("::") && get(i + 1).is_some_and(|t| t.kind == TokKind::Ident) => {
                    i += 2;
                }
                _ => break,
            }
        }
        // Interface port type `if_name.modport` is not handled here.
        while get(i).is_some_and(|t| t.is_punct("[")) {
            i = self.skip_group_from(i);
        }
        if !get(i).is_some_and(|t| t.kind == TokKind::Ident) {
            return Lookahead::None;
        }
        let mut j = i + 1;
        while get(j).is_some_and(|t| t.is_punct("[")) {
            j = self.skip_group_from(j);
        }
        match get(j) {
            Some(t) if t.is_punct(";") || t.is_punct(",") || t.is_punct("=") => Lookahead::Decl,
            Some(t) if t.is_punct("(") => Lookahead::Instance,
            Some(t) if t.nl_before() || t.kind == TokKind::Eof => Lookahead::Decl,
            None => Lookahead::Decl,
            _ => Lookahead::None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lookahead {
    None,
    Decl,
    Instance,
}

/// Keywords that start a new item; seeing one while scanning an expression means the
/// previous statement was not terminated.
fn is_item_kw(k: &str) -> bool {
    matches!(
        k,
        "module" | "macromodule" | "interface" | "program" | "package" | "class" | "function" | "task"
            | "typedef" | "import" | "export" | "always" | "always_comb" | "always_ff"
            | "always_latch" | "initial" | "final" | "assign" | "generate" | "modport"
            | "clocking" | "covergroup" | "property" | "sequence" | "constraint" | "localparam"
            | "parameter" | "checker" | "primitive" | "specify"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn deeply_nested_statements_do_not_overflow() {
        let n = 100_000;
        let text = format!("module m; initial {}x = 1; {}endmodule\n", "begin ".repeat(n), "end ".repeat(n));
        let u = super::parse(&crate::lexer::lex(&text, 0), 0);
        let deep: Vec<_> = u.diags.iter().filter(|d| d.message.contains("nested too deeply")).collect();
        assert_eq!(deep.len(), 1, "{:?}", u.diags.iter().take(5).collect::<Vec<_>>());
    }
}
