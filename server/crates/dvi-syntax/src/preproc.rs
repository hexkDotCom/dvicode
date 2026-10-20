//! SystemVerilog preprocessor.
//!
//! Produces one flat token stream for a compilation root, following `` `include `` and
//! expanding macros. Every output token keeps an origin span: real tokens (including macro
//! arguments) keep their own location, tokens created by a macro body point at the outermost
//! macro call site and carry [`flags::FROM_MACRO`].

use crate::diag::Diag;
use crate::span::{FileId, Span};
use crate::token::{flags, TokKind, Token};
use crate::SourceProvider;
use rustc_hash::FxHashMap;
use smol_str::SmolStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct MacroParam {
    pub name: SmolStr,
    pub default: Option<Vec<Token>>,
}

#[derive(Clone, Debug)]
pub struct MacroDef {
    pub name: SmolStr,
    /// `None` for object-like macros, `Some` (possibly empty) for function-like ones.
    pub params: Option<Vec<MacroParam>>,
    pub body: Vec<Token>,
    /// Location of the name in the `` `define ``; for command-line defines the file is
    /// [`COMMAND_LINE_FILE`].
    pub name_span: Span,
    /// Source text of the body, for hover.
    pub body_text: String,
}

impl MacroDef {
    pub fn signature(&self) -> String {
        match &self.params {
            None => format!("`define {}", self.name),
            Some(ps) => {
                let p: Vec<String> = ps
                    .iter()
                    .map(|p| match &p.default {
                        Some(d) => format!("{}={}", p.name, join_tokens(d)),
                        None => p.name.to_string(),
                    })
                    .collect();
                format!("`define {}({})", self.name, p.join(", "))
            }
        }
    }
}

/// File id used for macros defined on the command line (`+define+`).
pub const COMMAND_LINE_FILE: FileId = u32::MAX;

/// Macro definitions by name.
///
/// Every compilation root keeps a snapshot of the table it started with, so cloning must be
/// cheap: clones share one map, and the first change after a clone copies it (copy-on-write),
/// which happens at most once per root.
#[derive(Clone, Debug, Default)]
pub struct MacroTable(Arc<FxHashMap<SmolStr, Arc<MacroDef>>>);

impl MacroTable {
    pub fn new() -> MacroTable {
        MacroTable::default()
    }

    pub fn get(&self, name: &str) -> Option<&Arc<MacroDef>> {
        self.0.get(name)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    pub fn insert(&mut self, name: SmolStr, def: Arc<MacroDef>) {
        Arc::make_mut(&mut self.0).insert(name, def);
    }

    pub fn remove(&mut self, name: &str) {
        if self.0.contains_key(name) {
            Arc::make_mut(&mut self.0).remove(name);
        }
    }

    pub fn clear(&mut self) {
        self.0 = Arc::default();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SmolStr, &Arc<MacroDef>)> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Identity of the shared map: tables with the same identity are equal.
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as *const () as usize
    }

    /// A copy in which the definitions found in `moved` (keyed by the address of the old
    /// definition, see [`def_identity`]) are replaced; `None` when the table has none of them.
    pub fn replaced(&self, moved: &FxHashMap<usize, Arc<MacroDef>>) -> Option<MacroTable> {
        if !self.0.values().any(|d| moved.contains_key(&def_identity(d))) {
            return None;
        }
        let map = self.0.iter().map(|(k, d)| (k.clone(), moved.get(&def_identity(d)).unwrap_or(d).clone())).collect();
        Some(MacroTable(Arc::new(map)))
    }

    /// Same macro names with the same parameters and bodies (locations may differ): files
    /// compiled after a table expand identically with either.
    pub fn same_definitions(&self, other: &MacroTable) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        self.0.len() == other.0.len()
            && self.0.iter().all(|(k, a)| {
                other.0.get(k).is_some_and(|b| Arc::ptr_eq(a, b) || (a.body_text == b.body_text && a.signature() == b.signature()))
            })
    }
}

/// Identity of a definition (its address), for maps from old to new definitions.
pub fn def_identity(d: &Arc<MacroDef>) -> usize {
    Arc::as_ptr(d) as usize
}

/// A `` `define `` given as `NAME` or `NAME=value`.
pub fn command_line_define(name: &str, value: Option<&str>) -> MacroDef {
    let body = value.map(|v| crate::lexer::lex(v, COMMAND_LINE_FILE)).unwrap_or_default();
    MacroDef {
        name: SmolStr::new(name),
        params: None,
        body,
        name_span: Span::new(COMMAND_LINE_FILE, 0, 0),
        body_text: value.unwrap_or("").to_string(),
    }
}

#[derive(Clone, Debug)]
pub struct MacroUse {
    pub name: SmolStr,
    /// The `` `name `` token.
    pub name_span: Span,
    /// Name token through the closing parenthesis of the arguments.
    pub call_span: Span,
    pub def: Option<Arc<MacroDef>>,
}

/// A macro name written after `` `ifdef ``, `` `ifndef ``, `` `elsif `` or `` `undef `` (no
/// backtick): what it names when it is defined there.
#[derive(Clone, Debug)]
pub struct MacroName {
    pub name: SmolStr,
    pub span: Span,
    pub def: Option<Arc<MacroDef>>,
}

#[derive(Clone, Debug)]
pub struct IncludeInfo {
    /// The file name token (string literal) of the directive.
    pub span: Span,
    pub path_text: String,
    pub resolved: Option<FileId>,
}

#[derive(Clone, Debug, Default)]
pub struct PpConfig {
    pub incdirs: Vec<PathBuf>,
    /// Last resort for relative includes, usually the project directory.
    pub root_dir: PathBuf,
}

#[derive(Debug, Default)]
pub struct PpResult {
    pub tokens: Vec<Token>,
    pub diags: Vec<Diag>,
    pub includes: Vec<IncludeInfo>,
    pub macro_uses: Vec<MacroUse>,
    pub macro_defs: Vec<Arc<MacroDef>>,
    pub macro_names: Vec<MacroName>,
    /// Macros called through a name built by token pasting.
    pub pasted_macros: Vec<SmolStr>,
    /// The root and every file it includes, in first-include order.
    pub files: Vec<FileId>,
    /// Regions removed by `` `ifdef `` and friends.
    pub inactive: Vec<Span>,
    /// Encrypted regions (`` `pragma protect begin_protected ``, `` `protected ``).
    pub protected: Vec<Span>,
    pub macros_after: MacroTable,
    /// The text of every file read, exactly as it was lexed.
    pub sources: Vec<(FileId, Arc<str>)>,
}

pub fn preprocess(
    src: &dyn SourceProvider,
    cfg: &PpConfig,
    root: FileId,
    macros: MacroTable,
) -> PpResult {
    let mut pp = Pp {
        src,
        cfg,
        macros,
        frames: Vec::new(),
        conds: Vec::new(),
        res: PpResult::default(),
        inactive_start: None,
        file_set: FxHashMap::default(),
        depth_limit: MAX_MACRO_DEPTH,
    };
    pp.enter_file(root);
    pp.run();
    let mut res = pp.res;
    res.macros_after = pp.macros;
    res
}

/// Expands macros in a snippet of text with the given table, for "expand macro" features.
/// `levels` limits nested expansion (`usize::MAX` for all levels).
pub fn expand_snippet(
    src: &dyn SourceProvider,
    cfg: &PpConfig,
    file: FileId,
    tokens: Vec<Token>,
    macros: MacroTable,
    levels: usize,
) -> Vec<Token> {
    let mut pp = Pp {
        src,
        cfg,
        macros,
        frames: Vec::new(),
        conds: Vec::new(),
        res: PpResult::default(),
        inactive_start: None,
        file_set: FxHashMap::default(),
        depth_limit: MAX_MACRO_DEPTH,
    };
    pp.frames.push(Frame { toks: Arc::new(tokens), pos: 0, kind: FrameKind::File(file), cond_depth: 0 });
    pp.max_macro_depth_override(levels);
    pp.run();
    pp.res.tokens
}

/// Joins tokens back to text, using the whitespace flags.
pub fn join_tokens(toks: &[Token]) -> String {
    let mut s = String::new();
    for (i, t) in toks.iter().enumerate() {
        if i > 0 && t.ws_before() {
            s.push(' ');
        }
        match t.kind {
            TokKind::Directive => {
                s.push('`');
                s.push_str(&t.text);
            }
            _ => s.push_str(&t.text),
        }
    }
    s
}

struct Frame {
    toks: Arc<Vec<Token>>,
    pos: usize,
    kind: FrameKind,
    cond_depth: usize,
}

#[derive(Clone)]
enum FrameKind {
    File(FileId),
    Macro { site: Span, name: SmolStr },
}

struct Cond {
    parent_active: bool,
    active: bool,
    taken: bool,
    span: Span,
}

const MAX_INCLUDE_DEPTH: usize = 64;
const MAX_MACRO_DEPTH: usize = 128;

struct Pp<'a> {
    src: &'a dyn SourceProvider,
    cfg: &'a PpConfig,
    macros: MacroTable,
    frames: Vec<Frame>,
    conds: Vec<Cond>,
    res: PpResult,
    inactive_start: Option<(FileId, u32)>,
    file_set: FxHashMap<FileId, ()>,
    depth_limit: usize,
}

impl<'a> Pp<'a> {
    fn max_macro_depth_override(&mut self, levels: usize) {
        self.depth_limit = levels.min(MAX_MACRO_DEPTH);
    }

    fn active(&self) -> bool {
        self.conds.last().map_or(true, |c| c.active)
    }

    fn enter_file(&mut self, id: FileId) {
        let first = self.file_set.insert(id, ()).is_none();
        if first {
            self.res.files.push(id);
        }
        let (text, toks) = match self.src.source(id) {
            Some((t, k)) => (t, k),
            None => (Arc::from(""), Arc::default()),
        };
        if first {
            self.res.sources.push((id, text));
        }
        let depth = self.conds.len();
        self.frames.push(Frame { toks, pos: 0, kind: FrameKind::File(id), cond_depth: depth });
    }

    /// Text a file was lexed from in this run (falls back to the provider).
    fn read_text(&self, file: FileId) -> Option<Arc<str>> {
        self.res.sources.iter().find(|(f, _)| *f == file).map(|(_, t)| t.clone()).or_else(|| self.src.text(file))
    }

    fn macro_depth(&self) -> usize {
        self.frames.iter().filter(|f| matches!(f.kind, FrameKind::Macro { .. })).count()
    }

    fn in_macro(&self) -> Option<Span> {
        // The outermost macro frame above the nearest file frame defines the call site.
        let mut site = None;
        for f in self.frames.iter().rev() {
            match &f.kind {
                FrameKind::Macro { site: s, .. } => site = Some(*s),
                FrameKind::File(_) => break,
            }
        }
        site
    }

    fn current_file(&self) -> Option<FileId> {
        self.frames.iter().rev().find_map(|f| match f.kind {
            FrameKind::File(id) => Some(id),
            _ => None,
        })
    }

    /// Next token from the stack, popping exhausted frames.
    fn next(&mut self) -> Option<Token> {
        loop {
            let f = self.frames.last_mut()?;
            if f.pos < f.toks.len() {
                let t = f.toks[f.pos].clone();
                f.pos += 1;
                return Some(t);
            }
            let f = self.frames.pop().unwrap();
            if let FrameKind::File(_) = f.kind {
                while self.conds.len() > f.cond_depth {
                    let c = self.conds.pop().unwrap();
                    self.res.diags.push(Diag::error(c.span, "MISSING_ENDIF", "Missing `endif for this conditional"));
                }
            }
        }
    }

    /// Next token without consuming, looking through exhausted frames.
    fn peek(&self) -> Option<&Token> {
        self.frames.iter().rev().find_map(|f| f.toks.get(f.pos))
    }

    /// Next token of the current frame only, if it is on the same line. In a macro expansion
    /// a line continuation of the body ends the line too, so that a `` `define `` or
    /// `` `undef `` in a macro body ends where its line does.
    fn take_same_line(&mut self) -> Option<Token> {
        let f = self.frames.last_mut()?;
        let t = f.toks.get(f.pos)?;
        if t.nl_before() || (t.flags & flags::CONT_BEFORE != 0 && matches!(f.kind, FrameKind::Macro { .. })) {
            return None;
        }
        let t = t.clone();
        f.pos += 1;
        Some(t)
    }

    /// In a `` `define `` parameter list, the next token if it starts the next line and can
    /// continue the list (a parameter name or the closing parenthesis).
    fn take_param_on_next_line(&mut self) -> Option<Token> {
        let f = self.frames.last_mut()?;
        let t = f.toks.get(f.pos)?;
        if !(matches!(t.kind, TokKind::Ident | TokKind::Keyword) || t.is_punct(")")) {
            return None;
        }
        let t = t.clone();
        f.pos += 1;
        Some(t)
    }

    fn rest_of_line(&mut self) -> Vec<Token> {
        let mut v = Vec::new();
        while let Some(t) = self.take_same_line() {
            v.push(t);
        }
        v
    }

    fn run(&mut self) {
        while let Some(t) = self.next() {
            if t.kind == TokKind::Directive {
                self.directive(t);
            } else if self.active() {
                match t.kind {
                    TokKind::MacroPaste | TokKind::MacroQuote | TokKind::MacroEscQuote => {}
                    _ => {
                        // `32'h`ADDR` with ADDR = 1A11_0000: the expansion is the number's value.
                        if let Some(last) = self.res.tokens.last_mut() {
                            if continues_based_number(last, &t) {
                                last.text = SmolStr::new(format!("{}{}", last.text, t.text));
                                continue;
                            }
                        }
                        self.res.tokens.push(t)
                    }
                }
            }
        }
        while let Some(c) = self.conds.pop() {
            self.res.diags.push(Diag::error(c.span, "MISSING_ENDIF", "Missing `endif for this conditional"));
        }
    }

    fn real_span(&self, t: &Token) -> Span {
        self.in_macro().unwrap_or(t.span)
    }

    fn note_transition(&mut self, was: bool, tok: &Token) {
        let now = self.active();
        if self.in_macro().is_some() {
            return;
        }
        if was && !now {
            self.inactive_start = Some((tok.span.file, tok.span.end));
        } else if !was && now {
            if let Some((file, start)) = self.inactive_start.take() {
                if file == tok.span.file && start <= tok.span.start {
                    self.res.inactive.push(Span::new(file, start, tok.span.start));
                }
            }
        }
    }

    fn directive(&mut self, tok: Token) {
        match tok.text.as_str() {
            "ifdef" | "ifndef" => {
                let was = self.active();
                let val = self.cond_value(&tok);
                let val = if tok.text == "ifdef" { val } else { !val };
                self.conds.push(Cond { parent_active: was, active: was && val, taken: val, span: self.real_span(&tok) });
                self.note_transition(was, &tok);
            }
            "elsif" => {
                let was = self.active();
                let val = self.cond_value(&tok);
                match self.conds.last_mut() {
                    Some(c) => {
                        c.active = c.parent_active && !c.taken && val;
                        c.taken |= val;
                    }
                    None => self.res.diags.push(Diag::error(tok.span, "UNMATCHED_DIRECTIVE", "`elsif without `ifdef")),
                }
                self.note_transition(was, &tok);
            }
            "else" => {
                let was = self.active();
                match self.conds.last_mut() {
                    Some(c) => {
                        c.active = c.parent_active && !c.taken;
                        c.taken = true;
                    }
                    None => self.res.diags.push(Diag::error(tok.span, "UNMATCHED_DIRECTIVE", "`else without `ifdef")),
                }
                self.note_transition(was, &tok);
            }
            "endif" => {
                let was = self.active();
                if self.conds.pop().is_none() {
                    self.res.diags.push(Diag::error(tok.span, "UNMATCHED_DIRECTIVE", "`endif without `ifdef"));
                }
                self.note_transition(was, &tok);
            }
            _ if !self.active() => {}
            "define" => self.define(tok),
            "undef" => {
                if let Some(n) = self.take_same_line() {
                    self.note_macro_name(&n);
                    self.macros.remove(&n.text);
                }
            }
            "undefineall" => self.macros.clear(),
            "include" => self.include(tok),
            "protected" => {
                // Verilog-XL style encrypted region: the text up to `endprotected is ciphertext.
                let mut end = tok.span;
                while let Some(t) = self.next() {
                    end = t.span;
                    if t.kind == TokKind::Directive && t.text == "endprotected" {
                        break;
                    }
                }
                self.note_protected(&tok, end);
            }
            "resetall" | "celldefine" | "endcelldefine" | "nounconnected_drive" | "end_keywords"
            | "endprotect" | "protect" | "endprotected" | "delay_mode_distributed"
            | "delay_mode_path" | "delay_mode_unit" | "delay_mode_zero" => {}
            "timescale" | "default_nettype" | "line" | "begin_keywords" | "unconnected_drive"
            | "default_decay_time" | "default_trireg_strength" => {
                self.rest_of_line();
            }
            "pragma" => {
                let rest = self.rest_of_line();
                let is_begin_protected = rest.iter().any(|t| t.text == "protect")
                    && rest.iter().any(|t| t.text == "begin_protected");
                if is_begin_protected {
                    let end = self.skip_protected();
                    self.note_protected(&tok, end.unwrap_or(tok.span));
                }
            }
            "__FILE__" => {
                let span = self.real_span(&tok);
                let path = self.src.path(span.file);
                let text = format!("\"{}\"", path.display());
                self.emit_generated(&tok, TokKind::String, SmolStr::new(text));
            }
            "__LINE__" => {
                let span = self.real_span(&tok);
                let line = self
                    .read_text(span.file)
                    .map(|t| t[..(span.start as usize).min(t.len())].bytes().filter(|b| *b == b'\n').count() + 1)
                    .unwrap_or(0);
                self.emit_generated(&tok, TokKind::Number, SmolStr::new(line.to_string()));
            }
            _ => self.macro_call(tok),
        }
    }

    fn emit_generated(&mut self, tok: &Token, kind: TokKind, text: SmolStr) {
        let mut t = tok.clone();
        t.kind = kind;
        t.text = text;
        if let Some(site) = self.in_macro() {
            t.span = site;
            t.flags |= flags::FROM_MACRO;
        }
        self.res.tokens.push(t);
    }

    /// Skips to the `` `pragma protect end_protected `` line; returns the span of its last token.
    fn skip_protected(&mut self) -> Option<Span> {
        while let Some(t) = self.next() {
            if t.kind == TokKind::Directive && t.text == "pragma" {
                let rest = self.rest_of_line();
                if rest.iter().any(|t| t.text == "end_protected") {
                    return Some(rest.last().map_or(t.span, |l| l.span));
                }
            }
        }
        None
    }

    /// Records an encrypted region from its first directive to `end`.
    fn note_protected(&mut self, begin: &Token, end: Span) {
        let span = match self.in_macro() {
            Some(site) => site,
            None if end.file == begin.span.file && end.end >= begin.span.start => begin.span.cover(end),
            None => begin.span,
        };
        self.res.protected.push(span);
    }

    fn defined(&self, name: &str) -> bool {
        self.macros.contains_key(name)
    }

    /// Value of the condition after `` `ifdef ``/`` `elsif ``: a name, or a parenthesized
    /// expression of names with `!`, `&&`, `||`, `->`, `<->` (IEEE 1800-2023).
    fn cond_value(&mut self, tok: &Token) -> bool {
        let Some(t) = self.take_same_line() else {
            self.res.diags.push(Diag::error(tok.span, "MISSING_MACRO_NAME", format!("`{} needs a macro name", tok.text)));
            return false;
        };
        if t.is_punct("(") {
            let mut toks = Vec::new();
            let mut depth = 1;
            while let Some(t) = self.take_same_line() {
                if t.is_punct("(") {
                    depth += 1;
                } else if t.is_punct(")") {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                toks.push(t);
            }
            for t in &toks {
                self.note_macro_name(t);
            }
            let mut pos = 0;
            return self.cond_expr(&toks, &mut pos);
        }
        self.note_macro_name(&t);
        self.defined(&t.text)
    }

    /// Records a macro name written in a conditional or `` `undef `` directive.
    fn note_macro_name(&mut self, t: &Token) {
        if t.kind == TokKind::Ident && t.is_real() {
            self.res.macro_names.push(MacroName { name: t.text.clone(), span: t.span, def: self.macros.get(&t.text).cloned() });
        }
    }

    fn cond_expr(&self, toks: &[Token], pos: &mut usize) -> bool {
        let mut v = self.cond_and(toks, pos);
        while *pos < toks.len() {
            let op = toks[*pos].text.clone();
            match op.as_str() {
                "||" => {
                    *pos += 1;
                    let r = self.cond_and(toks, pos);
                    v = v || r;
                }
                "->" => {
                    *pos += 1;
                    let r = self.cond_and(toks, pos);
                    v = !v || r;
                }
                "<->" => {
                    *pos += 1;
                    let r = self.cond_and(toks, pos);
                    v = v == r;
                }
                _ => break,
            }
        }
        v
    }

    fn cond_and(&self, toks: &[Token], pos: &mut usize) -> bool {
        let mut v = self.cond_unary(toks, pos);
        while *pos < toks.len() && toks[*pos].is_punct("&&") {
            *pos += 1;
            let r = self.cond_unary(toks, pos);
            v = v && r;
        }
        v
    }

    fn cond_unary(&self, toks: &[Token], pos: &mut usize) -> bool {
        let Some(t) = toks.get(*pos) else { return false };
        *pos += 1;
        if t.is_punct("!") {
            return !self.cond_unary(toks, pos);
        }
        if t.is_punct("(") {
            let v = self.cond_expr(toks, pos);
            if toks.get(*pos).is_some_and(|t| t.is_punct(")")) {
                *pos += 1;
            }
            return v;
        }
        self.defined(&t.text)
    }

    fn define(&mut self, tok: Token) {
        let Some(name) = self.take_same_line() else {
            self.res.diags.push(Diag::error(tok.span, "MISSING_MACRO_NAME", "`define needs a macro name"));
            return;
        };
        if !matches!(name.kind, TokKind::Ident | TokKind::Keyword) {
            self.res.diags.push(Diag::error(name.span, "MISSING_MACRO_NAME", "Invalid macro name"));
            self.rest_of_line();
            return;
        }
        let mut params = None;
        let next_is_paren = self
            .frames
            .last()
            .and_then(|f| f.toks.get(f.pos))
            .is_some_and(|t| t.is_punct("(") && !t.ws_before() && !t.nl_before());
        if next_is_paren {
            self.take_same_line();
            let mut ps: Vec<MacroParam> = Vec::new();
            loop {
                // Simulators accept a parameter list that continues on the next lines without
                // a backslash: `define m(a,\n b)`.
                let Some(t) = self.take_same_line().or_else(|| self.take_param_on_next_line()) else {
                    self.res.diags.push(Diag::error(name.span, "MACRO_SYNTAX", "Unterminated macro parameter list"));
                    break;
                };
                if t.is_punct(")") {
                    break;
                }
                if t.is_punct(",") {
                    continue;
                }
                let mut p = MacroParam { name: t.text.clone(), default: None };
                // Optional default: `= tokens` up to `,` or `)` at depth 0.
                let has_default = self.frames.last().and_then(|f| f.toks.get(f.pos)).is_some_and(|t| t.is_punct("=") && !t.nl_before());
                if has_default {
                    self.take_same_line();
                    let mut d = Vec::new();
                    let mut depth = 0i32;
                    loop {
                        let stop = self.frames.last().and_then(|f| f.toks.get(f.pos)).map(|t| {
                            t.nl_before() || (depth == 0 && (t.is_punct(",") || t.is_punct(")")))
                        });
                        if stop != Some(false) {
                            break;
                        }
                        let t = self.take_same_line().unwrap();
                        if matches!(t.text.as_str(), "(" | "[" | "{") {
                            depth += 1;
                        } else if matches!(t.text.as_str(), ")" | "]" | "}") {
                            depth -= 1;
                        }
                        d.push(t);
                    }
                    p.default = Some(d);
                }
                ps.push(p);
            }
            params = Some(ps);
        }
        let body = self.rest_of_line();
        let body_text = match (body.first(), body.last()) {
            (Some(a), Some(b)) if a.span.file == b.span.file && a.is_real() => self
                .read_text(a.span.file)
                .and_then(|t| t.get(a.span.start as usize..b.span.end as usize).map(|s| s.to_string()))
                .unwrap_or_else(|| join_tokens(&body)),
            _ => join_tokens(&body),
        };
        let def = Arc::new(MacroDef {
            name: name.text.clone(),
            params,
            body,
            name_span: if self.in_macro().is_some() { self.real_span(&name) } else { name.span },
            body_text,
        });
        if self.in_macro().is_none() {
            self.res.macro_defs.push(def.clone());
        }
        self.macros.insert(name.text.clone(), def);
    }

    fn include(&mut self, tok: Token) {
        let Some(t) = self.take_same_line() else {
            self.res.diags.push(Diag::error(tok.span, "INCLUDE_SYNTAX", "`include needs a file name"));
            return;
        };
        let (path_text, span) = match t.kind {
            TokKind::String => (t.text.trim_matches('"').to_string(), t.span),
            TokKind::Directive => {
                // `include `FILE or `include `FILE(args): expand the operand like any other
                // macro use (nested macros, arguments), then take the file name it produces.
                let mut call = vec![t.clone()];
                if self.frames.last().and_then(|f| f.toks.get(f.pos)).is_some_and(|n| n.is_punct("(")) {
                    let mut depth = 0i32;
                    while let Some(x) = self.take_same_line() {
                        if x.is_punct("(") {
                            depth += 1;
                        } else if x.is_punct(")") {
                            depth -= 1;
                        }
                        call.push(x);
                        if depth == 0 {
                            break;
                        }
                    }
                }
                let file = self.current_file().unwrap_or(t.span.file);
                let out = expand_snippet(self.src, self.cfg, file, call, self.macros.clone(), usize::MAX);
                let name = match out.iter().position(|x| x.kind == TokKind::String || x.is_punct("<")) {
                    Some(i) if out[i].kind == TokKind::String => Some(out[i].text.trim_matches('"').to_string()),
                    Some(i) => {
                        let s: String = out[i + 1..].iter().take_while(|x| !x.is_punct(">")).map(|x| x.text.as_str()).collect();
                        (!s.is_empty()).then_some(s)
                    }
                    None => None,
                };
                match name {
                    Some(s) => (s, t.span),
                    None => {
                        self.res.diags.push(Diag::error(t.span, "INCLUDE_SYNTAX", format!("`{} does not expand to a file name", t.text)));
                        return;
                    }
                }
            }
            _ if t.is_punct("<") => {
                let mut s = String::new();
                let mut end = t.span;
                while let Some(x) = self.take_same_line() {
                    end = x.span;
                    if x.is_punct(">") {
                        break;
                    }
                    s.push_str(&x.text);
                }
                (s, t.span.cover(end))
            }
            _ => {
                self.res.diags.push(Diag::error(t.span, "INCLUDE_SYNTAX", "`include needs a quoted file name"));
                return;
            }
        };
        let span = if self.in_macro().is_some() { self.real_span(&tok) } else { span };
        let resolved = self.resolve_include(&path_text);
        self.res.includes.push(IncludeInfo { span, path_text: path_text.clone(), resolved });
        let Some(id) = resolved else {
            self.res.diags.push(Diag::error(span, "INCLUDE_NOT_FOUND", format!("Cannot find include file \"{path_text}\"")));
            return;
        };
        let depth = self.frames.iter().filter(|f| matches!(f.kind, FrameKind::File(_))).count();
        if depth >= MAX_INCLUDE_DEPTH || self.frames.iter().any(|f| matches!(f.kind, FrameKind::File(x) if x == id)) {
            self.res.diags.push(Diag::error(span, "RECURSIVE_INCLUDE", format!("Recursive include of \"{path_text}\"")));
            return;
        }
        self.enter_file(id);
    }

    fn resolve_include(&self, p: &str) -> Option<FileId> {
        let path = Path::new(p);
        if path.is_absolute() {
            return self.src.is_file(path).then(|| self.src.file_id(path));
        }
        let mut candidates: Vec<PathBuf> = Vec::with_capacity(self.cfg.incdirs.len() + 2);
        if let Some(cur) = self.current_file() {
            if let Some(dir) = self.src.path(cur).parent() {
                candidates.push(dir.join(path));
            }
        }
        for d in &self.cfg.incdirs {
            candidates.push(d.join(path));
        }
        candidates.push(self.cfg.root_dir.join(path));
        candidates.into_iter().find(|c| self.src.is_file(c)).map(|c| self.src.file_id(&normalize(&c)))
    }

    fn macro_call(&mut self, tok: Token) {
        let def = self.macros.get(&tok.text).cloned();
        let outer_site = self.in_macro();
        // Written by the user: outside any expansion, or in the arguments of a macro call
        // (`` `uvm_info(`gfn, ...) ``). Such uses are recorded and their problems shown there.
        let written = outer_site.is_none() || tok.is_real();
        if tok.flags & flags::PASTED != 0 && !self.res.pasted_macros.contains(&tok.text) {
            self.res.pasted_macros.push(tok.text.clone());
        }
        let Some(def) = def else {
            if written {
                self.res.diags.push(Diag::error(tok.span, "UNDEFINED_MACRO", format!("Macro `{} is not defined", tok.text)));
                self.res.macro_uses.push(MacroUse { name: tok.text.clone(), name_span: tok.span, call_span: tok.span, def: None });
            } else {
                self.res.diags.push(Diag::error(outer_site.unwrap(), "UNDEFINED_MACRO", format!("Macro `{} is not defined", tok.text)));
            }
            // Skip an argument list written right after the name.
            if self.peek().is_some_and(|t| t.is_punct("(") && !t.nl_before()) {
                self.next();
                let _ = self.collect_args(&tok);
            }
            return;
        };
        let mut call_end = tok.span;
        let mut args: Vec<Vec<Token>> = Vec::new();
        if let Some(params) = &def.params {
            if self.peek().is_some_and(|t| t.is_punct("(")) {
                self.next();
                match self.collect_args(&tok) {
                    Some((a, close)) => {
                        args = a;
                        call_end = close;
                    }
                    None => return,
                }
                if args.len() > params.len() && !(params.is_empty() && args.len() == 1 && args[0].is_empty()) {
                    let sp = if written { tok.span } else { outer_site.unwrap_or(tok.span) };
                    self.res.diags.push(Diag::error(
                        sp,
                        "MACRO_ARGUMENTS",
                        format!("Macro `{} expects {} argument(s), got {}", def.name, params.len(), args.len()),
                    ));
                }
            } else {
                let sp = if written { tok.span } else { outer_site.unwrap_or(tok.span) };
                self.res.diags.push(Diag::error(sp, "MACRO_ARGUMENTS", format!("Macro `{} needs arguments", def.name)));
            }
            // Missing actuals without defaults.
            for p in params.iter().skip(args.len().max(1)) {
                if p.default.is_none() {
                    let sp = if written { tok.span } else { outer_site.unwrap_or(tok.span) };
                    self.res.diags.push(Diag::error(
                        sp,
                        "MACRO_ARGUMENTS",
                        format!("Missing argument '{}' for macro `{}", p.name, def.name),
                    ));
                    break;
                }
            }
        }
        let site = outer_site.unwrap_or_else(|| tok.span.cover(call_end));
        if written {
            let call_span = if call_end.file == tok.span.file && call_end.start >= tok.span.start { tok.span.cover(call_end) } else { tok.span };
            self.res.macro_uses.push(MacroUse { name: tok.text.clone(), name_span: tok.span, call_span, def: Some(def.clone()) });
        }
        let limit = self.depth_limit;
        if self.macro_depth() >= limit {
            if limit < MAX_MACRO_DEPTH {
                // Partial expansion: keep the call as written.
                self.res.tokens.push(tok);
                for (i, a) in args.iter().enumerate() {
                    if i == 0 {
                        self.res.tokens.push(punct("(", site));
                    } else {
                        self.res.tokens.push(punct(",", site));
                    }
                    self.res.tokens.extend(a.iter().cloned());
                }
                if def.params.is_some() {
                    self.res.tokens.push(punct(")", site));
                }
                return;
            }
            self.res.diags.push(Diag::error(site, "MACRO_RECURSION", format!("Macro `{} expands too deeply", def.name)));
            return;
        }
        if self.frames.iter().any(|f| matches!(&f.kind, FrameKind::Macro { name, .. } if *name == def.name)) {
            self.res.diags.push(Diag::error(site, "MACRO_RECURSION", format!("Macro `{} is recursive", def.name)));
            return;
        }
        let first_flags = tok.flags & (flags::NL_BEFORE | flags::WS_BEFORE);
        let toks = substitute(&def, &args, site, first_flags);
        self.frames.push(Frame {
            toks: Arc::new(toks),
            pos: 0,
            kind: FrameKind::Macro { site, name: def.name.clone() },
            cond_depth: self.conds.len(),
        });
    }

    /// Collects macro actual arguments after the opening parenthesis. Returns the arguments
    /// and the span of the closing parenthesis.
    fn collect_args(&mut self, tok: &Token) -> Option<(Vec<Vec<Token>>, Span)> {
        let mut args = vec![Vec::new()];
        let mut depth = 1i32;
        loop {
            let Some(t) = self.next() else {
                self.res.diags.push(Diag::error(tok.span, "MACRO_ARGUMENTS", format!("Unterminated arguments of macro `{}", tok.text)));
                return None;
            };
            if t.kind == TokKind::Punct {
                match t.text.as_str() {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => {
                        depth -= 1;
                        if depth == 0 {
                            let close = t.span;
                            if args.len() == 1 && args[0].is_empty() {
                                args.clear();
                                args.push(Vec::new());
                            }
                            return Some((args, close));
                        }
                    }
                    "," if depth == 1 => {
                        args.push(Vec::new());
                        continue;
                    }
                    _ => {}
                }
            }
            args.last_mut().unwrap().push(t);
        }
    }
}

fn punct(p: &'static str, span: Span) -> Token {
    Token { kind: TokKind::Punct, text: SmolStr::new_static(p), span, flags: flags::FROM_MACRO }
}

pub fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn substitute(def: &MacroDef, args: &[Vec<Token>], site: Span, first_flags: u8) -> Vec<Token> {
    let params = def.params.as_deref().unwrap_or(&[]);
    let param_index = |name: &str| params.iter().position(|p| p.name == name);
    let body_tok = |t: &Token| -> Token {
        let mut t = t.clone();
        t.span = site;
        t.flags = flags::FROM_MACRO | (t.flags & (flags::WS_BEFORE | flags::CONT_BEFORE));
        t
    };
    let actual = |i: usize| -> Vec<Token> {
        match args.get(i) {
            Some(a) if !a.is_empty() => a
                .iter()
                .map(|t| {
                    let mut t = t.clone();
                    if t.flags & flags::FROM_MACRO == 0 {
                        t.flags |= flags::FROM_MACRO | flags::MACRO_ARG;
                    }
                    t.flags &= !(flags::NL_BEFORE | flags::CONT_BEFORE);
                    t
                })
                .collect(),
            _ => params[i].default.as_ref().map(|d| d.iter().map(body_tok).collect()).unwrap_or_default(),
        }
    };
    let mut out: Vec<Token> = Vec::with_capacity(def.body.len());
    let mut i = 0;
    while i < def.body.len() {
        let t = &def.body[i];
        match t.kind {
            TokKind::MacroQuote => {
                // Stringify up to the closing `".
                let mut s = String::from("\"");
                let mut first = true;
                i += 1;
                while i < def.body.len() && def.body[i].kind != TokKind::MacroQuote {
                    let b = &def.body[i];
                    if !first && b.ws_before() {
                        s.push(' ');
                    }
                    first = false;
                    match b.kind {
                        TokKind::MacroEscQuote => s.push_str("\\\""),
                        TokKind::Ident if param_index(&b.text).is_some() => {
                            s.push_str(&join_tokens(&actual(param_index(&b.text).unwrap())));
                        }
                        TokKind::Directive => {
                            s.push('`');
                            s.push_str(&b.text);
                        }
                        TokKind::String => s.push_str(b.text.trim_matches('"')),
                        _ => s.push_str(&b.text),
                    }
                    i += 1;
                }
                s.push('"');
                let mut st = body_tok(t);
                st.kind = TokKind::String;
                st.text = SmolStr::new(s);
                out.push(st);
            }
            TokKind::Ident | TokKind::Keyword if param_index(&t.text).is_some() => {
                let mut a = actual(param_index(&t.text).unwrap());
                if let Some(f) = a.first_mut() {
                    let keep = flags::WS_BEFORE | flags::CONT_BEFORE;
                    f.flags = (f.flags & !keep) | (t.flags & keep);
                }
                out.extend(a);
            }
            TokKind::Directive if param_index(&t.text).is_some() => {
                // `` `MACRO(x) `` with a formal MACRO: the macro its actual names, as
                // simulators do.
                let a = actual(param_index(&t.text).unwrap());
                let mut d = body_tok(t);
                d.flags |= flags::PASTED;
                match a.first() {
                    Some(f) if a.len() == 1 && matches!(f.kind, TokKind::Ident | TokKind::Keyword | TokKind::Directive) => {
                        d.text = f.text.clone();
                        out.push(d);
                    }
                    // Anything else (empty, several tokens): the formal's own name.
                    _ => out.push(d),
                }
            }
            _ => out.push(body_tok(t)),
        }
        i += 1;
    }
    // Token pasting.
    if out.iter().any(|t| t.kind == TokKind::MacroPaste) {
        let mut merged: Vec<Token> = Vec::with_capacity(out.len());
        let mut it = out.into_iter().peekable();
        while let Some(t) = it.next() {
            if t.kind == TokKind::MacroPaste {
                // `a````b` (two pastes, or a paste around an empty argument) is one paste.
                while it.peek().is_some_and(|n| n.kind == TokKind::MacroPaste) {
                    it.next();
                }
                // `/``* text *``/` pastes a block comment together (common_cells' registers.svh).
                if merged.last().is_some_and(|p| p.is_punct("/")) && it.peek().is_some_and(|n| n.is_punct("*")) {
                    merged.pop();
                    it.next();
                    let mut star = false;
                    for x in it.by_ref() {
                        if x.is_punct("/") && star {
                            break;
                        }
                        if x.kind != TokKind::MacroPaste {
                            star = x.is_punct("*");
                        }
                    }
                    continue;
                }
                if t.ws_before() {
                    // `covergroup ``NAME``_cg`: nothing to join on the left of the space.
                    if let Some(mut n) = it.next() {
                        n.flags |= flags::WS_BEFORE;
                        merged.push(n);
                    }
                    continue;
                }
                let next = it.next();
                let word = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$');
                match (merged.last_mut(), next) {
                    // `name``()`: only words join; anything else just follows without a space.
                    (Some(prev), Some(mut n)) if !(word(&prev.text) && word(&n.text)) => {
                        n.flags &= !(flags::WS_BEFORE | flags::NL_BEFORE);
                        merged.push(n);
                    }
                    (Some(prev), Some(n)) => {
                        let text = format!("{}{}", prev.text, n.text);
                        if prev.kind != TokKind::Directive {
                            prev.kind = classify(&text, prev.kind);
                        }
                        prev.text = SmolStr::new(text);
                        prev.span = site;
                        let pasted = if prev.kind == TokKind::Directive { flags::PASTED } else { 0 };
                        prev.flags = (prev.flags & (flags::WS_BEFORE | flags::CONT_BEFORE)) | flags::FROM_MACRO | pasted;
                    }
                    (None, Some(n)) => merged.push(n),
                    _ => {}
                }
            } else {
                merged.push(t);
            }
        }
        out = merged;
    }
    if let Some(f) = out.first_mut() {
        f.flags = (f.flags & !(flags::NL_BEFORE | flags::WS_BEFORE)) | first_flags;
    }
    out
}

/// Whether `t` continues the based number `last` (`'h` + digits from a macro expansion, which
/// the lexer split into a number and an identifier: `1` `A11_0000`).
fn continues_based_number(last: &Token, t: &Token) -> bool {
    if last.kind != TokKind::Number || !matches!(t.kind, TokKind::Number | TokKind::Ident) || t.text.is_empty() {
        return false;
    }
    let Some(q) = last.text.rfind('\'') else { return false };
    let base = last.text[q + 1..].trim_start_matches(['s', 'S']);
    let Some(b) = base.bytes().next() else { return false };
    if !matches!(b, b'b' | b'B' | b'o' | b'O' | b'd' | b'D' | b'h' | b'H') {
        return false;
    }
    // Right after the base, or (without a space) after digits that came from a macro.
    let after_base = base.len() == 1;
    (after_base || (!t.ws_before() && t.flags & flags::FROM_MACRO != 0))
        && t.text.bytes().all(|c| c.is_ascii_hexdigit() || matches!(c, b'x' | b'X' | b'z' | b'Z' | b'?' | b'_'))
}

fn classify(text: &str, fallback: TokKind) -> TokKind {
    let b = text.as_bytes();
    if !b.is_empty() && (b[0].is_ascii_alphabetic() || b[0] == b'_') && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$') {
        if crate::token::is_keyword(text) {
            TokKind::Keyword
        } else {
            TokKind::Ident
        }
    } else if !b.is_empty() && b[0].is_ascii_digit() {
        TokKind::Number
    } else {
        fallback
    }
}
