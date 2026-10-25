//! A SystemVerilog formatter that changes whitespace only: each line's indentation, its
//! trailing blanks and (when asked) runs of blank lines. It never joins, splits or reorders
//! anything, and it leaves alone what whitespace could change: `` `define `` definitions,
//! lines inside block comments and strings, continued lines and protected envelopes.
//!
//! Its own scanner keeps every byte (the lexer drops comments and normalizes some tokens);
//! the indentation comes from a contextual scan of the constructs, not from the parser, so
//! files with errors and files outside the build are formatted too. Every result is checked:
//! its lexemes (comments included) and the lexer's tokens must equal the original's.



/// What the formatter does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Spaces per level (also the tab width for alignment).
    pub indent_size: usize,
    pub use_tabs: bool,
    /// `case` items one level inside their `case`.
    pub indent_case_items: bool,
    /// Lines inside parentheses line up after the `(` when something follows it on its line.
    pub align_lists: bool,
    /// Levels added to the continuation lines of a statement.
    pub continuation: usize,
    pub directives: Directives,
    pub trim_trailing: bool,
    /// Blank lines kept in a row at most (0: all kept).
    pub max_blank_lines: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { indent_size: 2, use_tabs: false, indent_case_items: true, align_lists: false, continuation: 1, directives: Directives::Keep, trim_trailing: true, max_blank_lines: 0 }
    }
}

/// Where compiler directive lines (`` `ifdef ``, `` `include ``...) go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Directives {
    /// Their indentation is kept.
    Keep,
    Column0,
    /// At the level of the code around them.
    Indent,
}

/// A change to one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineEdit {
    /// The line's leading whitespace (bytes `0..old`) becomes `new`.
    Indent { old: usize, new: String },
    /// The line's trailing whitespace, from byte `from` to its end, goes.
    Trim { from: usize },
    /// Both.
    Both { old: usize, new: String, from: usize },
    /// The (blank) line goes, with its line break.
    Delete,
}

/// A formatted file: the edits by line (0-based), in order.
#[derive(Clone, Debug, Default)]
pub struct Formatted {
    pub edits: Vec<(usize, LineEdit)>,
}

// ------------------------------------------------------------------ scanner

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lk {
    Word,
    /// `\name` (its terminating whitespace is not part of it).
    Escaped,
    /// `` `name ``.
    Directive,
    Number,
    Str,
    LineComment,
    BlockComment,
    Punct,
    /// `\` at the end of a line.
    Continuation,
    /// A protected envelope, kept as it is (from the line of its begin to its end).
    Opaque,
}

#[derive(Clone, Copy, Debug)]
struct Lex {
    kind: Lk,
    start: usize,
    end: usize,
}

const MULTI: &[&str] = &["(*", "*)", "::", "'{", "->>", "|->", "|=>", "<=", ">=", "==", "!=", "&&", "||", "##", "+:", "-:", "++", "--", "<<", ">>", "->"];

/// The lexemes of `t` (no whitespace).
fn scan(t: &str) -> Vec<Lex> {
    let b = t.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < n {
        let c = b[i];
        // A protected envelope: from its begin line to its end line, taken whole.
        if line_start {
            let mut j = i;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            if let Some(end) = protected_end(t, j) {
                out.push(Lex { kind: Lk::Opaque, start: j, end });
                i = end;
                line_start = false;
                continue;
            }
        }
        if c == b'\n' {
            i += 1;
            line_start = true;
            continue;
        }
        line_start = false;
        if c == b' ' || c == b'\t' || c == b'\r' || c == 0x0b || c == 0x0c {
            i += 1;
            continue;
        }
        let start = i;
        let kind = if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            // Its line break's `\r` is not part of it.
            if i > start && b[i - 1] == b'\r' {
                i -= 1;
            }
            Lk::LineComment
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < n && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(n);
            Lk::BlockComment
        } else if c == b'"' {
            if t[i..].starts_with("\"\"\"") {
                i += 3;
                while i < n && !t[i..].starts_with("\"\"\"") {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 3).min(n);
            } else {
                i += 1;
                while i < n && b[i] != b'"' && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(n);
            }
            Lk::Str
        } else if c == b'\\' {
            // A continuation (only blanks up to the line break: `\` then `\r\n` too), else an
            // escaped identifier up to whitespace.
            let mut j = i + 1;
            while j < n && b[j] == b'\r' {
                j += 1;
            }
            if j >= n || b[j] == b'\n' {
                i += 1;
                Lk::Continuation
            } else {
                i += 1;
                while i < n && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                Lk::Escaped
            }
        } else if c == b'`' {
            i += 1;
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                i += 1;
            }
            if i == start + 1 {
                // `` `" ``, ``` `` ```, `` `\`" ``: macro text.
                i += 1;
                Lk::Punct
            } else {
                Lk::Directive
            }
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                i += 1;
            }
            Lk::Word
        } else if c.is_ascii_digit() {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                i += 1;
            }
            Lk::Number
        } else if c >= 0x80 {
            // Other characters (in malformed code): one whole character.
            i += t[i..].chars().next().map_or(1, |ch| ch.len_utf8());
            Lk::Punct
        } else {
            match MULTI.iter().find(|m| t[i..].starts_with(**m)) {
                Some(m) => i += m.len(),
                None => i += 1,
            }
            Lk::Punct
        };
        out.push(Lex { kind, start, end: i });
    }
    out
}

/// When a protected envelope begins at byte `i` (a line's first non-blank): the byte after
/// the line break of its end (the end of the text when it has none).
fn protected_end(t: &str, i: usize) -> Option<usize> {
    let rest = &t[i..];
    let words = |s: &str| s.split_whitespace().map(|w| w.to_string()).take(3).collect::<Vec<_>>();
    let first_line = rest.split('\n').next().unwrap_or("");
    let w = words(first_line);
    let (begin, end_word): (bool, &str) = if w.first().is_some_and(|x| x == "`pragma") && w.get(1).is_some_and(|x| x == "protect") && w.get(2).is_some_and(|x| x.starts_with("begin_protected")) {
        (true, "end_protected")
    } else if w.first().is_some_and(|x| x == "`protected") {
        (true, "`endprotected")
    } else {
        (false, "")
    };
    if !begin {
        return None;
    }
    let mut at = i + first_line.len();
    while at < t.len() {
        at += 1; // the line break
        let line = t[at..].split('\n').next().unwrap_or("");
        let lw = words(line);
        let ends = if end_word == "end_protected" {
            lw.first().is_some_and(|x| x == "`pragma") && lw.get(1).is_some_and(|x| x == "protect") && lw.get(2).is_some_and(|x| x.starts_with("end_protected"))
        } else {
            lw.first().is_some_and(|x| x == "`endprotected")
        };
        at += line.len();
        if ends {
            return Some(at.min(t.len()));
        }
    }
    Some(t.len())
}

// ------------------------------------------------------------------ lines

struct Line {
    /// Byte range of its text (without the line break).
    start: usize,
    end: usize,
    /// Its leading whitespace ends here (absolute).
    lead: usize,
    /// Its trailing whitespace starts here (absolute; `end` when none).
    trail: usize,
    /// Nothing on it can be changed.
    frozen: bool,
    /// Its trailing whitespace cannot be changed.
    keep_trail: bool,
    /// Its first lexeme (index), when one starts on it.
    first: Option<usize>,
    /// Part of a `` `define `` (its lexemes are no code here).
    define: bool,
}

fn lines_of(t: &str) -> Vec<Line> {
    let b = t.as_bytes();
    let mut out = Vec::new();
    let mut s = 0;
    loop {
        let e = t[s..].find('\n').map_or(t.len(), |k| s + k);
        let text_end = if e > s && b[e - 1] == b'\r' { e - 1 } else { e };
        let mut lead = s;
        while lead < text_end && (b[lead] == b' ' || b[lead] == b'\t') {
            lead += 1;
        }
        let mut trail = text_end;
        while trail > lead && (b[trail - 1] == b' ' || b[trail - 1] == b'\t') {
            trail -= 1;
        }
        // A `\` before trailing blanks: without them it would continue the line.
        let keep_trail = trail > lead && b[trail - 1] == b'\\';
        out.push(Line { start: s, end: text_end, lead, trail, frozen: false, keep_trail, first: None, define: false });
        if e >= t.len() {
            break;
        }
        s = e + 1;
    }
    out
}

// ------------------------------------------------------------------ indentation

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fk {
    Design,
    Class,
    Sub,
    Begin,
    Fork,
    Case,
    Generate,
    Group,
    Prop,
    Seq,
    Clocking,
    Specify,
    Table,
    Config,
    Macro,
    /// A bracket left open at the end of a line: closes when the depth falls back.
    Bracket,
    /// The one statement of `if`/`else`/a loop/a process/a case item, on a later line.
    Body,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    kind: Fk,
    /// The level of the line that opened it (where its closer goes).
    base: usize,
    /// The level of the lines inside.
    inner: usize,
    /// Bracket: the depth it closes at; others: the depth when opened.
    depth: usize,
    /// Bracket in align mode: the column its lines line up at.
    align: Option<usize>,
    /// Bracket: the columns of its opening bracket and of what follows it on its line (in
    /// the new layout).
    open_col: Option<(usize, Option<usize>)>,
    /// `if`s without their `else` yet: their levels.
    ifs: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
struct State {
    frames: Vec<Frame>,
    depth: usize,
    /// A body is expected after a control header: the level it takes on a later line.
    expect: Option<usize>,
    /// A control header's parenthesis being read: the body is expected when the depth falls
    /// back to this.
    header: Option<usize>,
    /// An event control or delay before a process's body (`@(...)`, `#5`).
    event: bool,
    /// A statement has begun and has not ended (its later lines are continuations).
    open: bool,
    /// The depth when the open statement began.
    open_depth: usize,
    /// The statement in progress began with a directive (a macro call).
    macro_stmt: bool,
    /// Prototype keywords seen in the statement (`extern`, `pure`, `import`, `export`, `typedef`).
    proto: bool,
    /// In a case item's label (before its `:`).
    in_label: bool,
    /// `?` seen in a case label (its `:` is a ternary's).
    ternary: usize,
    /// The previous significant word.
    prev: String,
    /// The lowest bracket depth reached on the current line.
    min_depth: usize,
    /// A `case`'s expression being read: its statement ends when the depth falls back.
    case_header: Option<usize>,
    /// The statement's first word (after a `label :`).
    first_word: String,
    /// Words read in the statement.
    words: usize,
    /// The next word is a block's label (`begin : name`, `end : name`).
    label: bool,
    /// The level of the line a control header (`if`, a loop, a process) began on: its body
    /// and a `begin` after it go by it, not by the line its condition ends on.
    hdr: usize,
    /// A header just ended on this line: the level a `begin` right after it takes.
    after_header: Option<usize>,
    /// How far the line that began the current statement moved (new width minus old):
    /// its continuation lines move as much, keeping their own alignment.
    shift: isize,
    /// The level of the line the current statement began on.
    stmt_level: usize,
    /// The statement is a design element's, class's or subroutine's header.
    in_header: bool,
    /// An `import ...;` clause in a header: its `;` does not end the header.
    header_import: bool,
}

impl State {
    fn level(&self) -> usize {
        self.frames.last().map_or(0, |f| f.inner)
    }

    fn top(&self) -> Option<&Frame> {
        self.frames.last()
    }

    fn push(&mut self, kind: Fk, base: usize, inner: usize) {
        let depth = self.depth;
        self.frames.push(Frame { kind, base, inner, depth, align: None, open_col: None, ifs: Vec::new() });
    }

    /// The frame `ifs` of the innermost block-like frame (not a bracket or body).
    fn ifs_mut(&mut self) -> Option<&mut Vec<usize>> {
        self.frames.iter_mut().rev().find(|f| !matches!(f.kind, Fk::Bracket | Fk::Body)).map(|f| &mut f.ifs)
    }

    /// The statement ended: one-statement bodies around it end too.
    fn end_statement(&mut self) {
        self.open = false;
        self.in_header = false;
        self.macro_stmt = false;
        self.proto = false;
        self.words = 0;
        self.first_word.clear();
        while self.top().is_some_and(|f| f.kind == Fk::Body && f.depth == self.depth) {
            self.frames.pop();
        }
    }
}

fn closer_of(w: &str) -> Option<&'static [Fk]> {
    Some(match w {
        "end" => &[Fk::Begin],
        "join" | "join_any" | "join_none" => &[Fk::Fork],
        "endcase" => &[Fk::Case],
        "endmodule" | "endinterface" | "endprogram" | "endpackage" | "endchecker" | "endprimitive" => &[Fk::Design],
        "endconfig" => &[Fk::Config],
        "endclass" => &[Fk::Class],
        "endfunction" | "endtask" => &[Fk::Sub],
        "endgenerate" => &[Fk::Generate],
        "endgroup" => &[Fk::Group],
        "endproperty" => &[Fk::Prop],
        "endsequence" => &[Fk::Seq],
        "endclocking" => &[Fk::Clocking],
        "endspecify" => &[Fk::Specify],
        "endtable" => &[Fk::Table],
        _ => return None,
    })
}

const DIRECTIVES: &[&str] = &[
    "define", "undef", "undefineall", "ifdef", "ifndef", "elsif", "else", "endif", "include", "timescale", "default_nettype", "resetall", "celldefine", "endcelldefine", "unconnected_drive", "nounconnected_drive", "line", "pragma", "begin_keywords", "end_keywords", "protect", "endprotect", "default_decay_time", "default_trireg_strength", "delay_mode_distributed", "delay_mode_path", "delay_mode_unit", "delay_mode_zero",
];

const HEADERS: &[&str] = &["if", "for", "foreach", "while", "repeat", "wait"];
const PROCESSES: &[&str] = &["always", "always_ff", "always_comb", "always_latch", "initial", "final", "forever", "do"];

/// Formats `t`; `None` when the result would change more than whitespace (a bug: logged by
/// the caller) or the text is too large.
pub fn format(t: &str, o: &Options) -> Option<Formatted> {
    if t.len() > 64 * 1024 * 1024 {
        return None;
    }
    let f = candidate(t, o);
    let out = apply(t, &f, None);
    difference(t, &out, o.max_blank_lines > 0).is_none().then_some(f)
}

/// The edits, not checked.
pub fn candidate(t: &str, o: &Options) -> Formatted {
    let lex = scan(t);
    let mut lines = lines_of(t);
    freeze(t, &lex, &mut lines);
    let levels = indent(t, &lex, &lines, o);
    let mut edits = Vec::new();
    let mut blank_run = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if l.frozen {
            blank_run = 0;
            continue;
        }
        let blank = l.lead == l.end;
        if blank {
            blank_run += 1;
            if o.max_blank_lines > 0 && blank_run > o.max_blank_lines && i + 1 < lines.len() {
                edits.push((i, LineEdit::Delete));
                continue;
            }
            if o.trim_trailing && l.end > l.start {
                edits.push((i, LineEdit::Trim { from: 0 }));
            }
            continue;
        }
        blank_run = 0;
        let lead_old = l.lead - l.start;
        let new_lead = levels[i].as_ref().map(|lv| lv.text(o));
        let indent = new_lead.filter(|n| *n != t[l.start..l.lead]);
        let trim = (o.trim_trailing && !l.keep_trail && l.trail < l.end).then_some(l.trail - l.start);
        match (indent, trim) {
            (Some(new), Some(from)) => edits.push((i, LineEdit::Both { old: lead_old, new, from })),
            (Some(new), None) => edits.push((i, LineEdit::Indent { old: lead_old, new })),
            (None, Some(from)) => edits.push((i, LineEdit::Trim { from })),
            (None, None) => {}
        }
    }
    Formatted { edits }
}

/// A line's new indentation: levels, and spaces after them (alignment).
#[derive(Clone, Copy, Debug)]
struct Indent {
    levels: usize,
    /// A column to reach instead (alignment), when given.
    column: Option<usize>,
}

impl Indent {
    fn text(&self, o: &Options) -> String {
        let width = self.column.unwrap_or(self.levels * o.indent_size);
        if o.use_tabs {
            let tabs = if self.column.is_some() { width / o.indent_size.max(1) } else { self.levels };
            let spaces = width - tabs * o.indent_size;
            format!("{}{}", "\t".repeat(tabs), " ".repeat(spaces))
        } else {
            " ".repeat(width)
        }
    }
}

/// Marks the lines nothing may change on: `` `define ``s, lines starting inside a block
/// comment or a string, continued lines, protected envelopes.
fn freeze(t: &str, lex: &[Lex], lines: &mut [Line]) {
    let line_of = |lines: &[Line], off: usize| lines.partition_point(|l| l.start <= off).saturating_sub(1);
    for (k, x) in lex.iter().enumerate() {
        let a = line_of(lines, x.start);
        if lines[a].first.is_none() && x.start >= lines[a].start {
            lines[a].first = Some(k);
        }
        let b = line_of(lines, x.end.saturating_sub(1).max(x.start));
        match x.kind {
            Lk::BlockComment | Lk::Str | Lk::Opaque if b > a => {
                // Lines after its first one start inside it; its first line keeps its end.
                lines[a].keep_trail = true;
                for l in &mut lines[a + 1..=b] {
                    l.frozen = true;
                }
                if x.kind == Lk::Opaque {
                    lines[a].frozen = true;
                }
            }
            Lk::Opaque => lines[a].frozen = true,
            Lk::Continuation => {
                lines[a].keep_trail = true;
                if a + 1 < lines.len() {
                    lines[a + 1].frozen = true;
                }
            }
            Lk::Directive if &t[x.start..x.end] == "`define" => {
                // The whole definition: its continued lines, and a parameter list without
                // continuations up to its `)`.
                let mut end_line = a;
                let mut j = k + 1;
                let mut depth = 0i32;
                let mut params = lex.get(k + 2).is_some_and(|p| &t[p.start..p.end] == "(" && lex.get(k + 1).is_some_and(|n| n.end == p.start));
                while let Some(y) = lex.get(j) {
                    let yl = line_of(lines, y.start);
                    if yl > end_line {
                        let continued = lex[..j].iter().rev().find(|p| p.end <= lines[end_line].end + 1).is_some_and(|p| line_of(lines, p.start) == end_line && (p.kind == Lk::Continuation || (p.kind == Lk::LineComment && t[p.start..p.end].trim_end().ends_with('\\'))));
                        if !continued && !(params && depth > 0) {
                            break;
                        }
                        end_line = yl;
                    }
                    if params {
                        match &t[y.start..y.end] {
                            "(" => depth += 1,
                            ")" => {
                                depth -= 1;
                                if depth == 0 {
                                    params = false;
                                }
                            }
                            _ => {}
                        }
                    }
                    let last_line = line_of(lines, y.end.saturating_sub(1).max(y.start));
                    end_line = end_line.max(last_line);
                    j += 1;
                }
                // A definition whose last line ends in a continuation goes on to the next line.
                for l in &mut lines[a..=end_line] {
                    l.frozen = true;
                    l.define = true;
                }
                if end_line + 1 < lines.len() && lex[..j].last().is_some_and(|p| p.kind == Lk::Continuation) {
                    lines[end_line + 1].frozen = true;
                    lines[end_line + 1].define = true;
                }
            }
            _ => {}
        }
    }
}

/// The indentation of each line (`None`: kept as it is). Conditional regions whose branches
/// end differently keep their indentation; the walk is made again knowing them, so that the
/// lines after them align with where those lines really are.
fn indent(t: &str, lex: &[Lex], lines: &[Line], o: &Options) -> Vec<Option<Indent>> {
    let mut kept = vec![false; lines.len()];
    for _ in 0..4 {
        let (out, keep) = indent_pass(t, lex, lines, o, &kept);
        let mut next = vec![false; lines.len()];
        for (a, b) in keep {
            for x in &mut next[a..=b] {
                *x = true;
            }
        }
        if next == kept {
            return out;
        }
        kept = next;
    }
    indent_pass(t, lex, lines, o, &kept).0
}

fn indent_pass(t: &str, lex: &[Lex], lines: &[Line], o: &Options, kept: &[bool]) -> (Vec<Option<Indent>>, Vec<(usize, usize)>) {
    let mut out: Vec<Option<Indent>> = vec![None; lines.len()];
    let mut s = State::default();
    // Conditional compilation: the state at each `ifdef, the ends of its branches, its line.
    let mut conds: Vec<(State, Vec<State>, usize)> = Vec::new();
    let mut keep: Vec<(usize, usize)> = Vec::new();
    let text = |x: &Lex| &t[x.start..x.end];
    let line_of = |off: usize| lines.partition_point(|l| l.start <= off).saturating_sub(1);
    let mut k = 0;
    let mut li = 0;
    // The previous line's trailing comment: its column before and after.
    let mut trailing: Option<(usize, usize)> = None;
    // The columns its lexemes start at (as it will be).
    let mut prev_cols: Vec<usize> = Vec::new();
    while li < lines.len() {
        let l = &lines[li];
        // The lexemes starting on this line.
        let from = k;
        while k < lex.len() && lex[k].start < l.end.max(l.start) + if l.end == l.start { 0 } else { 0 } && line_of(lex[k].start) == li {
            k += 1;
        }
        let toks = &lex[from..k];
        if toks.is_empty() || l.define {
            li += 1;
            continue;
        }
        let first = toks[0];
        let w0 = text(&first);
        // A directive line.
        if first.kind == Lk::Directive && DIRECTIVES.contains(&&w0[1..]) {
            let lv = s.level();
            out[li] = match o.directives {
                Directives::Keep => None,
                Directives::Column0 => Some(Indent { levels: 0, column: None }),
                Directives::Indent => Some(Indent { levels: lv, column: None }),
            };
            match &w0[1..] {
                "ifdef" | "ifndef" => conds.push((s.clone(), Vec::new(), li)),
                "elsif" | "else" => {
                    if let Some((at, ends, _)) = conds.last_mut() {
                        ends.push(s.clone());
                        s = at.clone();
                    }
                }
                "endif" => {
                    if let Some((_, mut ends, start)) = conds.pop() {
                        ends.push(s.clone());
                        // Compared by structure, not by how far lines moved.
                        let shape = |e: &State| State { shift: 0, ..e.clone() };
                        if ends.iter().all(|e| shape(e) == shape(&ends[0])) {
                            s = ends.swap_remove(0);
                        } else {
                            // The branches end differently: their lines keep their indentation.
                            keep.push((start, li));
                            s = ends.swap_remove(0);
                        }
                    }
                }
                _ => {}
            }
            // `define lines are frozen; the others carry no structure.
            li += 1;
            continue;
        }
        // This line's level, from the state before its lexemes.
        let mut lv = Indent { levels: s.level(), column: s.top().and_then(|f| f.align) };
        let old_width = display_column(t, l.start, l.lead, o.indent_size);
        let in_brackets = s.top().is_some_and(|f| f.kind == Fk::Bracket);
        let mut continuation = false;
        let starts_closer = |w: &str, kind: Lk| kind == Lk::Word && closer_of(w).is_some();
        if starts_closer(w0, first.kind) {
            let kinds = closer_of(w0).unwrap();
            if let Some(f) = s.frames.iter().rev().find(|f| kinds.contains(&f.kind)) {
                lv = Indent { levels: f.base, column: None };
            }
        } else if first.kind == Lk::Punct && matches!(w0, ")" | "]" | "}") {
            if let Some(f) = s.frames.iter().rev().find(|f| f.kind == Fk::Bracket) {
                lv = Indent { levels: f.base, column: None };
                // Placed by hand under its opening bracket: kept there.
                let moved = (old_width as isize + s.shift).max(0) as usize;
                if !o.align_lists && f.open_col.is_some_and(|(c, _)| c == moved) && moved > f.base * o.indent_size {
                    lv = Indent { levels: f.base, column: Some(moved) };
                }
            }
        } else if first.kind == Lk::Directive && (w0.ends_with("_end") || w0.ends_with("_END")) && s.top().is_some_and(|f| f.kind == Fk::Macro) {
            lv = Indent { levels: s.top().unwrap().base, column: None };
        } else if first.kind == Lk::Word && w0 == "else" {
            if let Some(&at) = s.frames.iter().rev().find(|f| !matches!(f.kind, Fk::Bracket | Fk::Body)).and_then(|f| f.ifs.last()) {
                lv = Indent { levels: at, column: None };
            }
        } else if s.in_header && s.open && s.depth == s.open_depth && first.kind == Lk::Punct && matches!(w0, "#" | "(") {
            // `#(` or `(` of a header on a line of its own: at the header's level.
            lv = Indent { levels: s.stmt_level, column: None };
        } else if s.open && s.depth == s.open_depth && s.expect.is_none() && s.header.is_none() && !s.in_label {
            // A continuation line of a statement (not of a case item's labels): one level in
            // from the line the statement began on.
            lv = Indent { levels: s.stmt_level + o.continuation, column: None };
            continuation = true;
        } else if s.expect.is_some() && !(first.kind == Lk::Word && matches!(w0, "begin" | "fork")) {
            lv = Indent { levels: s.expect.unwrap(), column: None };
        } else if let Some(e) = s.expect {
            // `begin` on the line after its header: at the header's level, or one deeper when
            // it is written so (both styles are common).
            let moved = (old_width as isize + s.shift).max(0) as usize;
            lv = Indent { levels: if moved == e * o.indent_size { e } else { e.saturating_sub(1) }, column: None };
        } else if in_brackets {
            continuation = true;
        }
        let comment_line = matches!(first.kind, Lk::LineComment | Lk::BlockComment);
        if continuation && lv.column.is_none() {
            // Aligned by hand (under a token of the line above, or after its opening bracket):
            // kept, moved as the line it continues moved.
            let moved = (old_width as isize + s.shift).max(0) as usize;
            let floor = if in_brackets { s.top().map_or(0, |f| f.base) } else { s.stmt_level } * o.indent_size;
            let after_open = in_brackets && s.top().and_then(|f| f.open_col).and_then(|(_, a)| a) == Some(moved);
            if moved > floor && (prev_cols.contains(&moved) || after_open) {
                lv = Indent { levels: lv.levels, column: Some(moved) };
            }
        }
        // A comment line under the previous line's trailing comment (in its column) stays
        // under it.
        let mut comment_col = None;
        if comment_line {
            if let Some((old, new)) = trailing.filter(|(old, _)| *old == old_width) {
                lv = Indent { levels: lv.levels, column: Some(new) };
                comment_col = Some((old, new));
            }
        }
        if !l.frozen && !kept[li] {
            out[li] = Some(lv);
        }
        let new_width = if l.frozen || kept[li] { old_width } else { lv.column.unwrap_or(lv.levels * o.indent_size) };
        if !continuation && !comment_line {
            s.shift = new_width as isize - old_width as isize;
        }
        // A statement beginning on this line: its level.
        if !s.open && !comment_line {
            s.stmt_level = lv.levels;
        }
        // Where this line's lexemes start, as it will be.
        prev_cols = toks.iter().map(|x| new_width + display_column(t, l.lead, x.start, o.indent_size)).collect();
        // The column of this line's trailing comment, before and after.
        trailing = comment_col.or_else(|| {
            let c = toks.last().filter(|c| toks.len() > 1 && matches!(c.kind, Lk::LineComment | Lk::BlockComment))?;
            let col = display_column(t, l.lead, c.start, o.indent_size);
            Some((old_width + col, new_width + col))
        });
        let line_level = lv.levels;
        // The body expected after a header: on this line it opens a Body frame.
        if let Some(body) = s.expect.take() {
            let w = text(&first);
            if !(first.kind == Lk::Word && matches!(w, "begin" | "fork")) {
                s.push(Fk::Body, body.saturating_sub(1), body);
            }
        }
        // The lexemes of the line, in order.
        s.min_depth = s.depth;
        for (j, x) in toks.iter().enumerate() {
            step(&mut s, t, lex, from + j, x, line_level, o, lines, li);
        }
        // A macro call on its own line (`` `uvm_object_new ``, no `;`) ends with it.
        if s.macro_stmt && s.open && s.depth == s.open_depth {
            s.end_statement();
        }
        let depth_at_start = s.min_depth;
        // Brackets opened on the line and left open: one frame for the line.
        if s.depth > depth_at_start {
            // The line's last unclosed opening bracket, and the first lexeme after it.
            let lead = if l.frozen || out[li].is_none() { old_width } else { lv.column.unwrap_or(lv.levels * o.indent_size) };
            let mut open_col = None;
            let mut d = 0i32;
            for (j, x) in toks.iter().enumerate().rev() {
                match text(x) {
                    ")" | "]" | "}" | "*)" => d += 1,
                    "(" | "[" | "{" | "'{" | "(*" => {
                        if d == 0 {
                            let after = toks.get(j + 1).filter(|n| !matches!(n.kind, Lk::LineComment | Lk::BlockComment)).map(|nx| lead + display_column(t, l.lead, nx.start, o.indent_size));
                            open_col = Some((lead + display_column(t, l.lead, x.start, o.indent_size), after));
                            break;
                        }
                        d -= 1;
                    }
                    _ => {}
                }
            }
            let align = if o.align_lists { open_col.and_then(|(_, a)| a) } else { None };
            let depth = depth_at_start;
            s.frames.push(Frame { kind: Fk::Bracket, base: line_level, inner: line_level + 1, depth, align, open_col, ifs: Vec::new() });
        }
        // A header complete at the end of its line: its body is on a later line.
        li += 1;
    }
    for (a, b) in &keep {
        for x in &mut out[*a..=*b] {
            *x = None;
        }
    }
    (out, keep)
}

/// The display column of byte `at` in the line starting at `start` (tabs to `tab` stops).
fn display_column(t: &str, start: usize, at: usize, tab: usize) -> usize {
    let mut col = 0;
    for ch in t[start..at].chars() {
        col = if ch == '\t' { (col / tab.max(1) + 1) * tab.max(1) } else { col + 1 };
    }
    col
}

/// One lexeme's effect on the state.
#[allow(clippy::too_many_arguments)]
fn step(s: &mut State, t: &str, lex: &[Lex], k: usize, x: &Lex, line_level: usize, o: &Options, _lines: &[Line], _li: usize) {
    let w = &t[x.start..x.end];
    if matches!(x.kind, Lk::LineComment | Lk::BlockComment | Lk::Continuation | Lk::Opaque) {
        return;
    }
    let next_word = |k: usize| lex[k + 1..].iter().find(|y| !matches!(y.kind, Lk::LineComment | Lk::BlockComment)).map(|y| &t[y.start..y.end]);
    // Brackets.
    match (x.kind, w) {
        (Lk::Punct, "(" | "[" | "{" | "'{" | "(*") => {
            s.depth += 1;
            if !s.open {
                s.open = true;
                s.open_depth = s.depth - 1;
            }
            return;
        }
        (Lk::Punct, ")" | "]" | "}" | "*)") => {
            s.depth = s.depth.saturating_sub(1);
            s.min_depth = s.min_depth.min(s.depth);
            while s.top().is_some_and(|f| f.kind == Fk::Bracket && f.depth >= s.depth) {
                s.frames.pop();
            }
            // A `case`'s expression read: its items follow.
            if s.case_header == Some(s.depth) {
                s.case_header = None;
                s.open = false;
                s.words = 0;
                s.first_word.clear();
            }
            // A macro call's arguments closed: the call is a statement of its own.
            if s.macro_stmt && s.open && s.depth == s.open_depth {
                s.end_statement();
            }
            // A block `{...}` of a constraint or a coverage item ends its statement.
            if w == "}" && s.open && s.depth == s.open_depth && matches!(s.first_word.as_str(), "constraint" | "coverpoint" | "cross" | "bins" | "ignore_bins" | "illegal_bins" | "wildcard" | "covergroup" | "dist") {
                let next = lex[k + 1..].iter().find(|y| !matches!(y.kind, Lk::LineComment | Lk::BlockComment)).map(|y| &t[y.start..y.end]);
                if next != Some(";") {
                    s.end_statement();
                }
            }
            // A control header's parenthesis closed: its body is next.
            if s.header == Some(s.depth) {
                s.header = None;
                s.expect = Some(s.hdr + 1);
                s.open = false;
            }
            // A process's event control `@(...)` closed.
            if s.event && s.depth == s.open_depth {
                s.event = false;
                s.expect = Some(s.hdr + 1);
                s.open = false;
            }
            return;
        }
        _ => {}
    }
    if s.depth > s.open_depth && s.open {
        // Inside brackets nothing opens a block (but `begin`... in a macro argument is rare).
        if x.kind == Lk::Word && w == "begin" {
            // fall through: a block inside parentheses (e.g. a fork in a task call) is kept simple.
        } else {
            s.prev = w.to_string();
            return;
        }
    }
    // A body was expected but something else came on the same line: no Body frame (a
    // `begin` there takes the header's level).
    s.after_header = s.expect.take().map(|e| e.saturating_sub(1));
    // Case item labels.
    let in_case = s.top().is_some_and(|f| f.kind == Fk::Case) && s.depth == s.top().unwrap().depth;
    if in_case && !s.in_label && !s.open {
        s.in_label = true;
        s.ternary = 0;
    }
    match (x.kind, w) {
        (Lk::Punct, ";") if s.in_header && s.header_import => {
            s.header_import = false;
            s.proto = false;
        }
        (Lk::Punct, ";") => {
            s.in_label = false;
            s.header = None;
            s.event = false;
            s.end_statement();
        }
        (Lk::Punct, "?") if s.in_label => s.ternary += 1,
        (Lk::Punct, ":") => {
            // `end : name`, `begin : name`: a label.
            if closer_of(&s.prev).is_some() || matches!(s.prev.as_str(), "begin" | "fork") {
                s.prev = ":".into();
                s.label = true;
                return;
            }
            if s.in_label && in_case {
                if s.ternary > 0 {
                    s.ternary -= 1;
                } else {
                    s.in_label = false;
                    s.open = false;
                    s.expect = Some(line_level + 1);
                }
            }
        }
        (Lk::Word, "default") if in_case && s.in_label => {
            if next_word(k) != Some(":") {
                s.in_label = false;
                s.open = false;
                s.expect = Some(line_level + 1);
            }
        }
        (Lk::Directive, _) => {
            if !s.open {
                s.open = true;
                s.open_depth = s.depth;
                s.macro_stmt = true;
            }
            // UVM's paired macros.
            let name = &w[1..];
            if name.starts_with("uvm_") && name.ends_with("_begin") {
                s.push(Fk::Macro, line_level, line_level + 1);
                s.open = true;
                s.open_depth = s.depth;
                s.macro_stmt = true;
            } else if name.starts_with("uvm_") && name.ends_with("_end") {
                if let Some(i) = s.frames.iter().rposition(|f| f.kind == Fk::Macro) {
                    s.frames.truncate(i);
                }
                s.open = false;
                s.macro_stmt = false;
            }
        }
        (Lk::Word, _) if s.label => s.label = false,
        (Lk::Word, _) => {
            if s.words == 0 || (s.words == 1 && s.prev == ":") {
                s.first_word = w.to_string();
            }
            s.words += 1;
            word(s, t, lex, k, w, line_level, o)
        }
        _ => {
            if !s.open {
                s.open = true;
                s.open_depth = s.depth;
            }
        }
    }
    s.prev = w.to_string();
}

/// A word's effect: keywords that open, close or head something.
fn word(s: &mut State, t: &str, lex: &[Lex], k: usize, w: &str, line_level: usize, o: &Options) {
    let next = lex[k + 1..].iter().find(|y| !matches!(y.kind, Lk::LineComment | Lk::BlockComment)).map(|y| &t[y.start..y.end]);
    let prev = s.prev.clone();
    if let Some(kinds) = closer_of(w) {
        if let Some(i) = s.frames.iter().rposition(|f| kinds.contains(&f.kind)) {
            s.frames.truncate(i);
        }
        s.open = false;
        s.macro_stmt = false;
        s.proto = false;
        s.in_label = false;
        // The closed construct was the statement of a body.
        while s.top().is_some_and(|f| f.kind == Fk::Body && f.depth == s.depth) {
            s.frames.pop();
        }
        return;
    }
    let opener = |s: &mut State, kind: Fk| {
        s.open = false;
        s.macro_stmt = false;
        s.push(kind, line_level, line_level + 1);
    };
    // A block right after a header: at the header's level.
    let after = s.after_header.take();
    let block = |s: &mut State, kind: Fk| {
        let base = after.unwrap_or(line_level);
        s.open = false;
        s.macro_stmt = false;
        s.push(kind, base, base + 1);
    };
    match w {
        "begin" => block(s, Fk::Begin),
        "fork" if !matches!(prev.as_str(), "disable" | "wait") => block(s, Fk::Fork),
        "case" | "casex" | "casez" | "randcase" => {
            let inner = if o.indent_case_items { line_level + 1 } else { line_level };
            s.open = false;
            s.push(Fk::Case, line_level, inner);
            // The case expression's parentheses: the items start after them.
            if next == Some("(") {
                s.case_header = Some(s.depth);
            }
        }
        "module" | "macromodule" | "program" | "package" | "checker" | "primitive" if !s.proto => {
            opener(s, Fk::Design);
            s.open = true;
            s.open_depth = s.depth;
            s.in_header = true;
        }
        "interface" if prev != "virtual" && !s.proto => {
            if next == Some("class") {
                // `interface class`: its `class` opens it.
            } else {
                opener(s, Fk::Design);
            }
        }
        "config" => opener(s, Fk::Config),
        "class" if prev != "typedef" && !s.proto => opener(s, Fk::Class),
        "function" | "task" => {
            if !s.proto && !matches!(prev.as_str(), "import" | "export" | "extern" | "pure") {
                opener(s, Fk::Sub);
            }
        }
        "import" if s.in_header => s.header_import = true,
        "extern" | "pure" | "import" | "export" | "typedef" => {
            s.proto = true;
            s.open = true;
            s.open_depth = s.depth;
        }
        "generate" => opener(s, Fk::Generate),
        "covergroup" => opener(s, Fk::Group),
        "property" if !matches!(prev.as_str(), "assert" | "assume" | "cover" | "expect" | "restrict") => opener(s, Fk::Prop),
        "sequence" if !matches!(prev.as_str(), "cover" | "expect") => opener(s, Fk::Seq),
        "randsequence" => opener(s, Fk::Seq),
        "clocking" => {
            // Only with a body: an `@` before the statement's `;`.
            let body = lex[k + 1..].iter().map(|y| &t[y.start..y.end]).take_while(|y| *y != ";").any(|y| y == "@");
            if body {
                opener(s, Fk::Clocking);
            } else if !s.open {
                s.open = true;
                s.open_depth = s.depth;
            }
        }
        "specify" => opener(s, Fk::Specify),
        "table" => opener(s, Fk::Table),
        "else" => {
            if let Some(ifs) = s.ifs_mut() {
                ifs.pop();
            }
            s.open = false;
            s.hdr = line_level;
            s.expect = Some(line_level + 1);
        }
        "if" => {
            let level = line_level;
            s.hdr = line_level;
            if let Some(ifs) = s.ifs_mut() {
                // An `else if` takes the place of its `if`.
                if prev == "else" {
                    ifs.push(level);
                } else {
                    ifs.push(level);
                }
            }
            s.open = true;
            s.open_depth = s.depth;
            if next == Some("(") {
                s.header = Some(s.depth);
            }
        }
        _ if HEADERS.contains(&w) && !(w == "wait" && next == Some("fork")) => {
            s.hdr = line_level;
            s.open = true;
            s.open_depth = s.depth;
            if next == Some("(") {
                s.header = Some(s.depth);
            }
        }
        _ if PROCESSES.contains(&w) => {
            s.hdr = line_level;
            s.open = true;
            s.open_depth = s.depth;
            match next {
                Some("@") | Some("#") => s.event = true,
                _ => {
                    s.open = false;
                    s.expect = Some(line_level + 1);
                }
            }
        }
        _ => {
            if s.event && prev == "@" && next != Some("(") {
                // `@*`, `@clk`: the event control ends with this word.
                s.event = false;
                s.open = false;
                s.expect = Some(s.hdr + 1);
                return;
            }
            if !s.open {
                s.open = true;
                s.open_depth = s.depth;
            }
        }
    }
}

// ------------------------------------------------------------------ results

/// `t` with the edits of the lines in `range` (all when `None`) applied.
pub fn apply(t: &str, f: &Formatted, range: Option<(usize, usize)>) -> String {
    let lines = lines_of(t);
    let mut out = String::with_capacity(t.len());
    let mut e = f.edits.iter().peekable();
    for (i, l) in lines.iter().enumerate() {
        let eol_end = if i + 1 < lines.len() { lines[i + 1].start } else { t.len() };
        let mut edit = None;
        while let Some((li, x)) = e.peek() {
            if *li < i {
                e.next();
            } else {
                if *li == i {
                    edit = Some(x.clone());
                }
                break;
            }
        }
        let in_range = range.is_none_or(|(a, b)| a <= i && i < b);
        let line = &t[l.start..l.end];
        let eol = &t[l.end..eol_end];
        match edit.filter(|_| in_range) {
            None => {
                out.push_str(line);
                out.push_str(eol);
            }
            Some(LineEdit::Delete) => {}
            Some(LineEdit::Indent { old, new }) => {
                out.push_str(&new);
                out.push_str(&line[old..]);
                out.push_str(eol);
            }
            Some(LineEdit::Trim { from }) => {
                out.push_str(&line[..from]);
                out.push_str(eol);
            }
            Some(LineEdit::Both { old, new, from }) => {
                out.push_str(&new);
                out.push_str(&line[old..from]);
                out.push_str(eol);
            }
        }
    }
    out
}

/// Whether `new` differs from `old` in whitespace only: the same lexemes in order (comments
/// included; a line comment's trailing blanks apart) and the same lexer tokens, and the same
/// number of lines unless blank lines may go.
pub fn verify(old: &str, new: &str, blank_lines_may_go: bool) -> bool {
    difference(old, new, blank_lines_may_go).is_none()
}

/// The first difference of `new` from `old` beyond whitespace, when there is one.
pub fn difference(old: &str, new: &str, blank_lines_may_go: bool) -> Option<String> {
    let a = scan(old);
    let b = scan(new);
    for (k, (x, y)) in a.iter().zip(&b).enumerate() {
        let (tx, ty) = (&old[x.start..x.end], &new[y.start..y.end]);
        let same = x.kind == y.kind && if x.kind == Lk::LineComment { tx.trim_end() == ty.trim_end() } else { tx == ty };
        if !same {
            return Some(format!("lexeme {k}: {:?} {tx:?} became {:?} {ty:?}", x.kind, y.kind));
        }
    }
    if a.len() != b.len() {
        return Some(format!("{} lexemes became {}", a.len(), b.len()));
    }
    if !blank_lines_may_go && old.matches('\n').count() != new.matches('\n').count() {
        return Some("the number of lines changed".into());
    }
    let la = crate::lexer::lex(old, 0);
    let lb = crate::lexer::lex(new, 0);
    for (k, (x, y)) in la.iter().zip(&lb).enumerate() {
        let flags = |t: &crate::token::Token| if t.flags & crate::token::flags::NL_BEFORE != 0 { t.flags & !crate::token::flags::WS_BEFORE } else { t.flags };
        if !(x.kind == y.kind && x.text == y.text && flags(x) == flags(y)) {
            return Some(format!("token {k}: {:?} {:?} {} became {:?} {:?} {}", x.kind, x.text, x.flags, y.kind, y.text, y.flags));
        }
    }
    (la.len() != lb.len()).then(|| format!("{} tokens became {}", la.len(), lb.len()))
}

/// The lines (`[a, b)`) a range from line `start` to `end` (position `end_col` on it) touches:
/// an end at column 0 of a later line does not take that line.
pub fn range_lines(start: usize, end: usize, end_col: usize) -> (usize, usize) {
    let last = if end > start && end_col == 0 { end - 1 } else { end };
    (start, last.max(start) + 1)
}
