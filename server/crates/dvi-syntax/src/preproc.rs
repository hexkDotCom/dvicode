//! SystemVerilog preprocessor.
//!
//! Produces one flat token stream for a compilation root, following `` `include `` and
//! expanding macros. Every output token keeps an origin span: real tokens (including macro
//! arguments) keep their own location, tokens created by a macro body point at the outermost
//! macro call site and carry [`flags::FROM_MACRO`].

use crate::diag::Diag;
use crate::span::{FileId, LineIndex, Span};
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
    /// Whether `other` defines the macro the same way: the same body text, parameter names and
    /// defaults, compared without building strings (macro tables of thousands of definitions
    /// are compared after edits). As [`MacroDef::signature`] shows them, but for the rare
    /// escaped parameter name that prints like a default (`\a= ` and `a=`): those differ.
    pub fn same_as(&self, other: &MacroDef) -> bool {
        let same_tokens = |a: &[Token], b: &[Token]| {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.kind == y.kind && x.text == y.text && x.ws_before() == y.ws_before())
                || join_tokens(a) == join_tokens(b)
        };
        self.body_text == other.body_text
            && self.name == other.name
            && match (&self.params, &other.params) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.len() == b.len()
                        && a.iter().zip(b).all(|(x, y)| {
                            x.name == y.name
                                && match (&x.default, &y.default) {
                                    (None, None) => true,
                                    (Some(dx), Some(dy)) => same_tokens(dx, dy),
                                    _ => false,
                                }
                        })
                }
                _ => false,
            }
    }

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

pub use crate::macro_table::{MacroTable, SameMemo};
#[cfg(feature = "work-count")]
pub use crate::macro_table::visited as macro_table_work;

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
    /// The include (an index in the same list) that entered the file holding this one; `None`
    /// in the root file.
    pub from: Option<u32>,
    /// The file was read here (not when it is missing or the include is recursive).
    pub entered: bool,
    /// What found the file.
    pub step: Option<IncludeStep>,
}

/// Where an include's file was found, in the order they are tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncludeStep {
    /// The name is an absolute path.
    Absolute,
    /// In the folder of the file holding the `` `include ``.
    IncluderFolder,
    /// In the `+incdir` of this index (in the invocation's order).
    Incdir(u32),
    /// In the project's root folder (the last resort).
    Root,
}

#[derive(Clone, Debug, Default)]
pub struct PpConfig {
    pub incdirs: Vec<PathBuf>,
    /// Last resort for relative includes, usually the project directory.
    pub root_dir: PathBuf,
}

/// The places an include of `name` is looked for, in order, with the step each one is: the
/// name itself when it is absolute; else the including file's folder (`folder`), each incdir,
/// the project root.
pub fn include_candidates<'c>(name: &'c str, folder: Option<&'c Path>, cfg: &'c PpConfig) -> impl Iterator<Item = (PathBuf, IncludeStep)> + 'c {
    let path = Path::new(name);
    let absolute = path.is_absolute();
    let abs = absolute.then(|| (path.to_path_buf(), IncludeStep::Absolute));
    let rest = (!absolute).then(move || {
        let here = folder.map(|d| (d.join(path), IncludeStep::IncluderFolder));
        let dirs = cfg.incdirs.iter().enumerate().map(move |(i, d)| (d.join(path), IncludeStep::Incdir(i as u32)));
        here.into_iter().chain(dirs).chain(std::iter::once((cfg.root_dir.join(path), IncludeStep::Root)))
    });
    abs.into_iter().chain(rest.into_iter().flatten())
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
    /// Indexes in `tokens` of the tokens right after an encrypted region (flagged
    /// [`flags::AFTER_PROTECTED`] at the end).
    after_protected: Vec<usize>,
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
        outer_depth: 0,
        outer_names: Vec::new(),
        nested_runs: Default::default(),
        line_index: Default::default(),
        work: Default::default(),
        adopted: Default::default(),
        adopted_diags: Default::default(),
    };
    pp.enter_file(root, None);
    // The root file's tokens are most of the output: room for them at once.
    if let Some(f) = pp.frames.last() {
        let n = f.toks.len().min(1 << 20);
        pp.res.tokens.reserve(n);
    }
    pp.run();
    let mut res = pp.res;
    for i in std::mem::take(&mut res.after_protected) {
        if let Some(t) = res.tokens.get_mut(i) {
            t.flags |= flags::AFTER_PROTECTED;
        }
    }
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
        outer_depth: 0,
        outer_names: Vec::new(),
        nested_runs: Default::default(),
        line_index: Default::default(),
        work: Default::default(),
        adopted: Default::default(),
        adopted_diags: Default::default(),
    };
    pp.frames.push(Frame { toks: Arc::new(tokens), pos: 0, include: None, repeat: false, kind: FrameKind::File(file), cond_depth: 0 });
    pp.max_macro_depth_override(levels);
    pp.run();
    pp.res.tokens
}

/// Joins tokens back to text, using the whitespace flags.
pub fn join_tokens(toks: &[Token]) -> String {
    let mut s = String::new();
    // An escaped name ends at white space: one is put after it when the next token has none.
    let mut escaped = false;
    for (i, t) in toks.iter().enumerate() {
        if i > 0 && (t.ws_before() || escaped) {
            s.push(' ');
        }
        escaped = false;
        match t.kind {
            TokKind::Directive => {
                s.push('`');
                s.push_str(&t.text);
            }
            TokKind::Ident if !plain_name(&t.text) => {
                s.push('\\');
                s.push_str(&t.text);
                escaped = true;
            }
            _ => s.push_str(&t.text),
        }
    }
    if escaped {
        s.push(' ');
    }
    s
}

/// Whether `n` can be written as it is (not an escaped identifier's text).
fn plain_name(n: &str) -> bool {
    let mut c = n.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_' || x == '$') && !crate::token::is_keyword(n)
}

struct Frame {
    toks: Arc<Vec<Token>>,
    pos: usize,
    /// For a file: the include (index in `includes`) that entered it; `None` for the root.
    include: Option<u32>,
    /// A file included again: its active tokens count against the work budget.
    repeat: bool,
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
/// Tokens of macro expansions and repeated includes one file's preprocessing reads (see
/// `Pp::work`): far more than real code expands to, few enough to keep in memory.
const MAX_WORK: u64 = 5_000_000;
/// Parentheses and `!` nested in an `` `ifdef `` expression.
const MAX_COND_DEPTH: usize = 256;
/// Nested preprocessors one file may run (see `Pp::nested_runs`).
const MAX_NESTED_RUNS: u32 = 1_000_000;

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
    /// Macro levels already open around this preprocessor (when it expands an actual argument
    /// for [`Pp::called_name`]); they count against the depth limit.
    outer_depth: usize,
    /// The macros being expanded around this preprocessor: a use of one of them is recursive.
    outer_names: Vec<SmolStr>,
    /// Nested preprocessors run for one file so far (shared with them): text like
    /// `` `"`A`A`" `` doubles the work at each level, so the total is bounded.
    nested_runs: std::rc::Rc<std::cell::Cell<u32>>,
    /// Line starts of the files `` `__LINE__ `` was used in (counting from the start of the
    /// file at each use made a file of UVM report macros quadratic).
    /// Shared with the nested preprocessors (a `__LINE__` in each of many stringified actuals).
    line_index: std::rc::Rc<std::cell::RefCell<FxHashMap<FileId, LineIndex>>>,
    /// Tokens read from macro expansions and repeated includes so far for one file, shared
    /// with the nested preprocessors: macros that each call the previous one twice, or headers
    /// that include the previous one twice, double the work at every level within the depth
    /// limits.
    work: std::rc::Rc<std::cell::Cell<u64>>,
    /// Macro uses written in an actual that was expanded before it was substituted (pasted or
    /// stringified): recorded with the call's, so rename and navigation see them.
    adopted: std::cell::RefCell<Vec<MacroUse>>,
    /// Problems of such an expansion that failed inside a string (nothing reads it again).
    adopted_diags: std::cell::RefCell<Vec<Diag>>,
}

impl<'a> Pp<'a> {
    fn max_macro_depth_override(&mut self, levels: usize) {
        self.depth_limit = levels.min(MAX_MACRO_DEPTH);
    }

    fn active(&self) -> bool {
        self.conds.last().map_or(true, |c| c.active)
    }

    fn enter_file(&mut self, id: FileId, include: Option<u32>) {
        let first = self.file_set.insert(id, ()).is_none();
        if first {
            self.res.files.push(id);
        }
        let (text, toks) = match self.src.source(id) {
            Some((t, k)) => (t, k),
            None => {
                // Not an empty file: one that could not be read (permissions, a device).
                // (A file that is not there is the build's "File not found".)
                if first && self.src.is_file(&self.src.path(id)) {
                    self.res.diags.push(Diag::error(Span::new(id, 0, 0), "UNREADABLE_FILE", format!("Cannot read {}", self.src.path(id).display())));
                }
                (Arc::from(""), Arc::default())
            }
        };
        if first {
            self.res.sources.push((id, text));
        }
        let depth = self.conds.len();
        self.frames.push(Frame { toks, pos: 0, include, repeat: !first, kind: FrameKind::File(id), cond_depth: depth });
    }

    /// Text a file was lexed from in this run (falls back to the provider).
    fn read_text(&self, file: FileId) -> Option<Arc<str>> {
        self.res.sources.iter().find(|(f, _)| *f == file).map(|(_, t)| t.clone()).or_else(|| self.src.text(file))
    }

    fn macro_depth(&self) -> usize {
        self.outer_depth + self.frames.iter().filter(|f| matches!(f.kind, FrameKind::Macro { .. })).count()
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
                // Macro text, and what a file included again adds (not what its guard skips).
                let charged = matches!(f.kind, FrameKind::Macro { .. }) || f.repeat;
                if (charged && (matches!(f.kind, FrameKind::Macro { .. }) || self.active()) && !self.charge(1, self.real_span(&t))) || self.work.get() > MAX_WORK {
                    self.over_budget(self.real_span(&t));
                    return None;
                }
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

    /// Counts `n` tokens of expansion work; past the budget, reports it once and ends the
    /// preprocessing (false).
    fn charge(&mut self, n: u64, at: Span) -> bool {
        let work = self.work.get() + n;
        self.work.set(work);
        if work <= MAX_WORK {
            return true;
        }
        self.over_budget(at);
        false
    }

    /// Ends the preprocessing past the work budget (crossed here or in a nested preprocessor,
    /// whose problems are not kept), reporting it once.
    fn over_budget(&mut self, at: Span) {
        if !self.res.diags.iter().any(|d| d.code == "EXPANSION_LIMIT") {
            let msg = format!("Preprocessing stops here: macro expansions and repeated includes produced over {MAX_WORK} tokens");
            self.res.diags.push(Diag::error(at, "EXPANSION_LIMIT", msg));
        }
        self.frames.clear();
    }

    /// Next token without consuming, looking through exhausted frames.
    fn peek(&self) -> Option<&Token> {
        self.frames.iter().rev().find_map(|f| f.toks.get(f.pos))
    }

    /// Next token of the current frame only, if it is on the same line. In a macro expansion
    /// a line continuation of the body ends the line too, so that a `` `define `` or
    /// `` `undef `` in a macro body ends where its line does.
    fn take_same_line(&mut self) -> Option<Token> {
        let t = self.peek_same_line()?.clone();
        self.frames.last_mut()?.pos += 1;
        Some(t)
    }

    /// The token [`take_same_line`](Self::take_same_line) would take.
    fn peek_same_line(&self) -> Option<&Token> {
        let f = self.frames.last()?;
        let t = f.toks.get(f.pos)?;
        if t.nl_before() || (t.flags & flags::CONT_BEFORE != 0 && matches!(f.kind, FrameKind::Macro { .. })) {
            return None;
        }
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
                let line = match self.read_text(span.file) {
                    Some(t) => self.line_index.borrow_mut().entry(span.file).or_insert_with(|| LineIndex::new(&t)).line_of(span.start) as usize + 1,
                    None => 0,
                };
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
        self.res.after_protected.push(self.res.tokens.len());
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
            return self.cond_expr(&toks, &mut pos, 0);
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

    fn cond_expr(&self, toks: &[Token], pos: &mut usize, depth: usize) -> bool {
        let mut v = self.cond_and(toks, pos, depth);
        while *pos < toks.len() {
            let op = toks[*pos].text.clone();
            match op.as_str() {
                "||" => {
                    *pos += 1;
                    let r = self.cond_and(toks, pos, depth);
                    v = v || r;
                }
                "->" => {
                    *pos += 1;
                    let r = self.cond_and(toks, pos, depth);
                    v = !v || r;
                }
                "<->" => {
                    *pos += 1;
                    let r = self.cond_and(toks, pos, depth);
                    v = v == r;
                }
                _ => break,
            }
        }
        v
    }

    fn cond_and(&self, toks: &[Token], pos: &mut usize, depth: usize) -> bool {
        let mut v = self.cond_unary(toks, pos, depth);
        while *pos < toks.len() && toks[*pos].is_punct("&&") {
            *pos += 1;
            let r = self.cond_unary(toks, pos, depth);
            v = v && r;
        }
        v
    }

    fn cond_unary(&self, toks: &[Token], pos: &mut usize, depth: usize) -> bool {
        // Parentheses and `!` nest by recursion: a generated condition nested deeper than any
        // real one is false rather than the end of the stack.
        if depth > MAX_COND_DEPTH {
            *pos = toks.len();
            return false;
        }
        let Some(t) = toks.get(*pos) else { return false };
        *pos += 1;
        if t.is_punct("!") {
            return !self.cond_unary(toks, pos, depth + 1);
        }
        if t.is_punct("(") {
            let v = self.cond_expr(toks, pos, depth + 1);
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
                let has_default = self.peek_same_line().is_some_and(|t| t.is_punct("="));
                if has_default {
                    self.take_same_line();
                    let mut d = Vec::new();
                    let mut depth = 0i32;
                    loop {
                        if self.peek_same_line().map_or(true, |t| depth == 0 && (t.is_punct(",") || t.is_punct(")"))) {
                            break;
                        }
                        let Some(t) = self.take_same_line() else { break };
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
        self.macros.insert(def);
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
                // Within the expansion around it: same macros being expanded, depth and budget.
                let (out, uses) = match self.nested(None) {
                    Some(mut pp) => {
                        pp.frames.push(Frame { toks: Arc::new(call), pos: 0, include: None, repeat: false, kind: FrameKind::File(file), cond_depth: 0 });
                        pp.run();
                        (pp.res.tokens, pp.res.macro_uses)
                    }
                    None => (Vec::new(), Vec::new()),
                };
                // The macros named in the operand are used here too (references, rename).
                self.res.macro_uses.extend(uses);
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
        let (resolved, step) = match self.resolve_include(&path_text) {
            Some((id, step)) => (Some(id), Some(step)),
            None => (None, None),
        };
        // The include that entered the file this directive is in.
        let from = self.frames.iter().rev().find(|f| matches!(f.kind, FrameKind::File(_))).and_then(|f| f.include);
        let index = self.res.includes.len() as u32;
        self.res.includes.push(IncludeInfo { span, path_text: path_text.clone(), resolved, from, entered: false, step });
        let Some(id) = resolved else {
            self.res.diags.push(Diag::error(span, "INCLUDE_NOT_FOUND", format!("Cannot find include file \"{path_text}\"")));
            return;
        };
        // A file already being read may be included again (IEEE 1800 22.4): a cycle of guarded
        // headers ends when the guards are defined, whatever its length. Only the depth bounds
        // it: past it, a cycle when the file is being read already, else a chain too long.
        let depth = self.frames.iter().filter(|f| matches!(f.kind, FrameKind::File(_))).count();
        if depth >= MAX_INCLUDE_DEPTH {
            let open = self.frames.iter().any(|f| matches!(f.kind, FrameKind::File(x) if x == id));
            self.res.diags.push(if open {
                Diag::error(span, "RECURSIVE_INCLUDE", format!("Recursive include of \"{path_text}\" ({MAX_INCLUDE_DEPTH} levels deep and not stopped by a guard)"))
            } else {
                Diag::error(span, "INCLUDE_TOO_DEEP", format!("Includes nested more than {MAX_INCLUDE_DEPTH} deep at \"{path_text}\""))
            });
            return;
        }
        self.res.includes[index as usize].entered = true;
        self.enter_file(id, Some(index));
    }

    fn resolve_include(&self, p: &str) -> Option<(FileId, IncludeStep)> {
        let folder = self.current_file().and_then(|cur| self.src.path(cur).parent().map(Path::to_path_buf));
        let (path, step) = include_candidates(p, folder.as_deref(), &self.cfg).find(|(c, _)| self.src.is_file(c))?;
        let id = if step == IncludeStep::Absolute { self.src.file_id(&path) } else { self.src.file_id(&normalize(&path)) };
        Some((id, step))
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
        if self.outer_names.contains(&def.name) || self.frames.iter().any(|f| matches!(&f.kind, FrameKind::Macro { name, .. } if *name == def.name)) {
            self.res.diags.push(Diag::error(site, "MACRO_RECURSION", format!("Macro `{} is recursive", def.name)));
            return;
        }
        if self.outer_depth > 0 {
            // Inside a nested preprocessor every expansion counts: `A30 = `A29 `A29 ... would
            // otherwise double the work at each level.
            if self.nested_runs.get() >= MAX_NESTED_RUNS {
                self.res.diags.push(Diag::error(site, "MACRO_RECURSION", format!("Macro `{} expands too much", def.name)));
                return;
            }
            self.nested_runs.set(self.nested_runs.get() + 1);
        }
        let first_flags = tok.flags & (flags::NL_BEFORE | flags::WS_BEFORE);
        // Built only up to what the budget can still read: a body repeating a long argument
        // would otherwise be built whole first.
        let limit = MAX_WORK.saturating_sub(self.work.get()) as usize;
        let toks = substitute(&def, &args, site, first_flags, limit, &|a| self.called_name(a, site, &def.name), &|a, report| self.expand_tokens(a, site, &def.name, report));
        let adopted = self.adopted.take();
        self.res.macro_uses.extend(adopted);
        let failed = self.adopted_diags.take();
        self.res.diags.extend(failed);
        self.frames.push(Frame {
            toks: Arc::new(toks),
            pos: 0,
            include: None,
            repeat: false,
            kind: FrameKind::Macro { site, name: def.name.clone() },
            cond_depth: self.conds.len(),
        });
    }

    /// The macro an actual argument names in `` `FORMAL(...) ``: an identifier names itself; a
    /// `` `NAME `` is expanded first (when it is a plain name, as in `` `define SELECT DECL ``).
    /// Expanded like any other text (`` `define SELECT `ID(DECL) `` names DECL), within the depth
    /// limit of the expansion it is part of. `None` when it is not one name.
    fn called_name(&self, actual: &[Token], site: Span, current: &SmolStr) -> Option<SmolStr> {
        let name = |t: &Token| matches!(t.kind, TokKind::Ident | TokKind::Keyword).then(|| t.text.clone());
        match actual {
            [t] if t.kind != TokKind::Directive => name(t),
            [t, ..] if t.kind == TokKind::Directive => match self.expand_tokens(actual, site, current, false)?.as_slice() {
                [t] => name(t),
                _ => None,
            },
            _ => None,
        }
    }

    /// `actual`, starting with a macro call, expanded with the macros defined so far (within
    /// the depth limit); `None` when that fails (an undefined macro, a recursive use, too much
    /// work). `current`: the macro whose body `actual` is in.
    /// `report`: a failure's problems (an undefined macro) are kept for the call, where nothing
    /// else would report them (inside a string).
    fn expand_tokens(&self, actual: &[Token], site: Span, current: &SmolStr, report: bool) -> Option<Vec<Token>> {
        let mut pp = self.nested(Some(current))?;
        pp.frames.push(Frame { toks: Arc::new(actual.to_vec()), pos: 0, include: None, repeat: false, kind: FrameKind::Macro { site, name: SmolStr::default() }, cond_depth: 0 });
        pp.run();
        if pp.res.diags.iter().any(|d| d.code == "UNDEFINED_MACRO" || d.code == "MACRO_RECURSION") {
            if report {
                // Only a macro written in the actual (`S(`MISSING)`): one inside another macro's
                // body (`DV_STRINGIFY(`DUT_HIER)`, defined on a simulator's command line) is the
                // body's, as a string written in a body is.
                let written: rustc_hash::FxHashSet<Span> = pp.res.macro_uses.iter().filter(|u| u.def.is_none()).map(|u| u.name_span).collect();
                self.adopted_diags.borrow_mut().extend(pp.res.diags.into_iter().filter(|d| d.code == "UNDEFINED_MACRO" && written.contains(&d.span)));
                self.adopted.borrow_mut().append(&mut pp.res.macro_uses);
            }
            return None;
        }
        self.adopted.borrow_mut().append(&mut pp.res.macro_uses);
        Some(std::mem::take(&mut pp.res.tokens))
    }

    /// A preprocessor for text inside the current expansion (an actual argument, a macro in a
    /// `" string, an `include operand). It shares the macros being expanded (a use of one is
    /// recursive), the depth and the work budget; `None` when they are used up.
    fn nested(&self, current: Option<&SmolStr>) -> Option<Pp<'a>> {
        let depth = self.macro_depth() + 1;
        if depth >= self.depth_limit || self.nested_runs.get() >= MAX_NESTED_RUNS {
            return None;
        }
        self.nested_runs.set(self.nested_runs.get() + 1);
        let mut outer_names = self.outer_names.clone();
        outer_names.extend(self.frames.iter().filter_map(|f| match &f.kind {
            FrameKind::Macro { name, .. } if !name.is_empty() => Some(name.clone()),
            _ => None,
        }));
        if let Some(c) = current.filter(|c| !c.is_empty()) {
            outer_names.push(c.clone());
        }
        Some(Pp {
            src: self.src,
            cfg: self.cfg,
            macros: self.macros.clone(),
            frames: Vec::new(),
            conds: Vec::new(),
            res: PpResult::default(),
            inactive_start: None,
            file_set: FxHashMap::default(),
            depth_limit: self.depth_limit,
            outer_depth: depth,
            outer_names,
            nested_runs: self.nested_runs.clone(),
            line_index: self.line_index.clone(),
            work: self.work.clone(),
            adopted: Default::default(),
            adopted_diags: Default::default(),
        })
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

/// Compiler directives: never macro formals, even when a formal has the same name.
const COMPILER_DIRECTIVES: &[&str] = &[
    "define", "undef", "undefineall", "ifdef", "ifndef", "elsif", "else", "endif", "include", "timescale",
    "resetall", "celldefine", "endcelldefine", "default_nettype", "pragma", "line", "__FILE__", "__LINE__",
    "begin_keywords", "end_keywords", "unconnected_drive", "nounconnected_drive", "protect", "endprotect",
    "protected", "endprotected", "default_decay_time", "default_trireg_strength", "delay_mode_distributed",
    "delay_mode_path", "delay_mode_unit", "delay_mode_zero",
];

/// `called` gives the macro an actual names after a backtick (see [`Pp::called_name`]).
/// Bytes of a name built by token pasting at most: past it, the parts stay apart.
const MAX_PASTED: usize = 4096;
/// Bytes of a `` `"...`" `` string built at most.
const MAX_STRINGIFIED: usize = 1 << 20;

/// An actual with `` `"...`" `` strings in it: each made into the string a body's would be
/// (macros inside expanded); the other tokens as they are.
fn quote_actual(a: Vec<Token>, site: Span, limit: usize, called: &dyn Fn(&[Token]) -> Option<SmolStr>, expand: &dyn Fn(&[Token], bool) -> Option<Vec<Token>>) -> Vec<Token> {
    let mut out = Vec::with_capacity(a.len());
    let mut k = 0;
    while k < a.len() {
        if a[k].kind == TokKind::MacroQuote {
            if let Some(end) = (k + 1..a.len()).find(|&j| a[j].kind == TokKind::MacroQuote) {
                let quoted = MacroDef { name: SmolStr::default(), params: None, body: a[k..=end].to_vec(), name_span: site, body_text: String::new() };
                out.extend(substitute(&quoted, &[], site, a[k].flags & flags::WS_BEFORE, limit, called, expand));
                k = end + 1;
                continue;
            }
        }
        out.push(a[k].clone());
        k += 1;
    }
    out
}

fn substitute(
    def: &MacroDef,
    args: &[Vec<Token>],
    site: Span,
    first_flags: u8,
    limit: usize,
    called: &dyn Fn(&[Token]) -> Option<SmolStr>,
    expand: &dyn Fn(&[Token], bool) -> Option<Vec<Token>>,
) -> Vec<Token> {
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
    while i < def.body.len() && out.len() <= limit {
        let t = &def.body[i];
        match t.kind {
            TokKind::MacroQuote => {
                // Stringify up to the closing `".
                let mut s = String::from("\"");
                let mut first = true;
                // After a `` the next token is glued on (`"x``_vif`" is "a_vif").
                let mut glue = false;
                i += 1;
                while i < def.body.len() && def.body[i].kind != TokKind::MacroQuote {
                    // A formal repeated in the string, given a long actual, would make a string
                    // of gigabytes: past this size the rest is left out.
                    if s.len() > MAX_STRINGIFIED {
                        i += 1;
                        continue;
                    }
                    let b = &def.body[i];
                    if b.kind == TokKind::MacroPaste {
                        glue = true;
                        i += 1;
                        continue;
                    }
                    if !first && !glue && b.ws_before() {
                        s.push(' ');
                    }
                    first = false;
                    glue = false;
                    match b.kind {
                        TokKind::MacroEscQuote => s.push_str("\\\""),
                        TokKind::Ident | TokKind::Keyword if param_index(&b.text).is_some() => {
                            let a = actual(param_index(&b.text).unwrap());
                            // Macros in the actual are expanded here too, as they are in the body
                            // (`VIF(`BUS) with `"x``_vif`" is "bus_vif").
                            let expands = |x: &Token| x.kind == TokKind::Directive && (!COMPILER_DIRECTIVES.contains(&x.text.as_str()) || matches!(x.text.as_str(), "__LINE__" | "__FILE__"));
                            match a.iter().any(expands).then(|| expand(&a, true)).flatten() {
                                Some(toks) => {
                                    for (k, e) in toks.iter().enumerate() {
                                        if s.len() > MAX_STRINGIFIED {
                                            break;
                                        }
                                        if k > 0 && e.ws_before() {
                                            s.push(' ');
                                        }
                                        s.push_str(if e.kind == TokKind::String { e.text.trim_matches('"') } else { &e.text });
                                    }
                                }
                                None => s.push_str(&join_tokens(&a)),
                            }
                        }
                        // Macros inside `" are expanded (IEEE 1800 22.5.1): `"dir/`VENDOR/x`",
                        // with their arguments (`"dir/`ID(vcs)/x`"), formals replaced.
                        TokKind::Directive => match (!COMPILER_DIRECTIVES.contains(&b.text.as_str()) || matches!(b.text.as_str(), "__LINE__" | "__FILE__")).then(|| {
                            let mut call = vec![b.clone()];
                            // `"`M(x)`" with a formal M: the macro its actual names.
                            if let Some(n) = param_index(&b.text).and_then(|k| called(&actual(k))) {
                                call[0].text = n;
                            }
                            if def.body.get(i + 1).is_some_and(|x| x.is_punct("(")) {
                                let mut depth = 0i32;
                                let mut j = i + 1;
                                while j < def.body.len() && def.body[j].kind != TokKind::MacroQuote {
                                    let x = &def.body[j];
                                    if x.is_punct("(") {
                                        depth += 1;
                                    } else if x.is_punct(")") {
                                        depth -= 1;
                                    }
                                    match param_index(&x.text).filter(|_| matches!(x.kind, TokKind::Ident | TokKind::Keyword)) {
                                        Some(k) => call.extend(actual(k)),
                                        None => call.push(x.clone()),
                                    }
                                    if depth == 0 {
                                        break;
                                    }
                                    j += 1;
                                }
                                if depth != 0 {
                                    call.truncate(1);
                                } else {
                                    let toks = expand(&call, false)?;
                                    i = j;
                                    return Some(toks);
                                }
                            }
                            expand(&call, false)
                        }).flatten() {
                            Some(toks) => {
                                for (k, e) in toks.iter().enumerate() {
                                    if s.len() > MAX_STRINGIFIED {
                                        break;
                                    }
                                    if k > 0 && e.ws_before() {
                                        s.push(' ');
                                    }
                                    s.push_str(if e.kind == TokKind::String { e.text.trim_matches('"') } else { &e.text });
                                }
                            }
                            None => {
                                s.push('`');
                                s.push_str(&b.text);
                            }
                        },
                        TokKind::String => s.push_str(b.text.trim_matches('"')),
                        _ => s.push_str(&b.text),
                    }
                    i += 1;
                }
                // One long actual may have gone past the limit: cut there.
                if s.len() > MAX_STRINGIFIED {
                    let mut cut = MAX_STRINGIFIED;
                    while !s.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    s.truncate(cut);
                }
                s.push('"');
                let mut st = body_tok(t);
                st.kind = TokKind::String;
                st.text = SmolStr::new(s);
                out.push(st);
            }
            TokKind::Ident | TokKind::Keyword if param_index(&t.text).is_some() => {
                let mut a = actual(param_index(&t.text).unwrap());
                // `` `"...`" `` written in the actual, not the body: a string once substituted, as
                // simulators make it (`` `LOG(`"`TC run`") ``).
                if a.iter().any(|x| x.kind == TokKind::MacroQuote) {
                    a = quote_actual(a, site, limit, called, expand);
                }
                // Pasted to its neighbour: a macro in the actual is expanded first, as simulators
                // do (`` `CFG(`TC) `` with the body `` tc``_cfg `` is `demo_cfg`, not `` `TC_cfg ``).
                let pasted = def.body.get(i + 1).is_some_and(|x| x.kind == TokKind::MacroPaste) || i.checked_sub(1).is_some_and(|p| def.body[p].kind == TokKind::MacroPaste);
                if pasted && a.iter().any(|x| x.kind == TokKind::Directive && (!COMPILER_DIRECTIVES.contains(&x.text.as_str()) || matches!(x.text.as_str(), "__LINE__" | "__FILE__"))) {
                    // A failure is not reported here: the actual is substituted as written, and
                    // its macros are reported where they are read again.
                    if let Some(e) = expand(&a, false) {
                        a = e
                            .into_iter()
                            .map(|mut x| {
                                x.flags |= flags::FROM_MACRO;
                                x.flags &= !(flags::NL_BEFORE | flags::CONT_BEFORE);
                                x
                            })
                            .collect();
                    }
                }
                if let Some(f) = a.first_mut() {
                    let keep = flags::WS_BEFORE | flags::CONT_BEFORE;
                    f.flags = (f.flags & !keep) | (t.flags & keep);
                }
                out.extend(a);
            }
            TokKind::Directive if param_index(&t.text).is_some() && !COMPILER_DIRECTIVES.contains(&t.text.as_str()) => {
                // `` `MACRO(x) `` with a formal MACRO: the macro its actual names, as
                // simulators do.
                let a = actual(param_index(&t.text).unwrap());
                let mut d = body_tok(t);
                d.flags |= flags::PASTED;
                // Anything else (empty, several names): the formal's own name.
                if let Some(n) = called(&a) {
                    d.text = n;
                }
                out.push(d);
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
                    (Some(prev), Some(mut n)) if !(word(&prev.text) && word(&n.text)) || prev.text.len() + n.text.len() > MAX_PASTED => {
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
