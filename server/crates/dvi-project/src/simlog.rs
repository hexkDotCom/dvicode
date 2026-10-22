//! What a simulator compiled, read from its log (`+dvicode_init_from_simlog`):
//!
//! - VCS and vlogan: the messages `Parsing design file '...'`, `Parsing library file '...'` and
//!   `Parsing included file '...'`, and the defines and include directories of the command line
//!   the log shows (`Command: vcs ...`, `Command  = vlogan ...`, or a line starting with the tool).
//! - Questa: the `vlog ...` command lines (`# vlog ...` in a GUI transcript), whose words are
//!   read like an argument file (Questa does not list the files it parses).

/// The compile of one log, as written there (paths not resolved).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SimLog {
    /// Files compiled, in order.
    pub files: Vec<String>,
    /// Library (`-y`/`-v`) files the compile used.
    pub lib_files: Vec<String>,
    /// Files included by `` `include ``.
    pub included: Vec<String>,
    /// `NAME` or `NAME=VALUE`, from the command line.
    pub defines: Vec<String>,
    /// `+incdir+` directories of the command line.
    pub incdirs: Vec<String>,
    /// A command line was found.
    pub command: bool,
    /// The compiler of the command line (`vcs`, `vlogan`, `vlog`...), without a path or `.exe`.
    pub tool: String,
    /// The words of the command line after the tool.
    pub words: Vec<String>,
}

impl SimLog {
    /// A Questa compile: its files are the words of its command line, not `Parsing` messages.
    pub fn from_command_line(&self) -> bool {
        matches!(self.tool.as_str(), "vlog" | "qverilog")
    }
}

/// The compiler a word names: `/tools/vcs/bin/vcs`, `C:\\questa\\win64\\vlog.exe`.
fn tool_name(w: &str) -> Option<&'static str> {
    let name = w.rsplit(['/', '\\']).next().unwrap_or(w);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    ["vcs", "vlogan", "vhdlan", "vlog", "qverilog"].into_iter().find(|t| *t == name)
}

/// The quoted path of a `Parsing <kind> file '<path>'` message.
fn parsed_file<'a>(line: &'a str, kind: &str) -> Option<&'a str> {
    let rest = line.strip_prefix("Parsing ")?.strip_prefix(kind)?.strip_prefix(" file ")?;
    let rest = rest.strip_prefix('\'').or_else(|| rest.strip_prefix('"'))?;
    let end = rest.find(['\'', '"'])?;
    Some(&rest[..end]).filter(|p| !p.is_empty())
}

/// The words of a command line; quotes keep spaces in a word (`+incdir+"a b"`,
/// `+define+MSG="hello world"`) and stay in it; a backslash keeps the next character. An
/// apostrophe in a number (`8'hff`) is not a quote.
fn shell_words(line: &str) -> Vec<String> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.char_indices();
    while let Some((i, c)) = chars.next() {
        match (quote, c) {
            (_, '\\') => {
                cur.push(c);
                if let Some((_, n)) = chars.next() {
                    cur.push(n);
                }
            }
            (None, '"') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, '\'') if crate::opens_quote(b, i) => {
                quote = Some(c);
                cur.push(c);
            }
            (Some(q), _) if c == q => {
                quote = None;
                cur.push(c);
            }
            (None, _) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The parts of `a+"b+c"+d` (`a`, `b+c`, `d`): split at `+` outside quotes, quotes removed.
fn split_plus(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '+') => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.retain(|p| !p.is_empty());
    out
}

/// The command line of a log line, as words: `Command: vcs ...`, `Command  = vwi vlogan ...`,
/// `Command arguments: ...`, or a line whose first word is the tool.
fn command_words(line: &str) -> Option<Vec<String>> {
    let tool = |w: &str| tool_name(w).is_some();
    let words = shell_words(line);
    let start = if line.trim_start().starts_with("Command") {
        // Up to the separator, then the words of the command (possibly a wrapper first).
        let sep = words.iter().position(|w| w.ends_with(':') || w == "=" || w.ends_with('='))?;
        sep + 1
    } else if words.first().is_some_and(|w| tool(w)) {
        0
    } else {
        return None;
    };
    let words = &words[start..];
    // Only a compile command: the tool among the first words, or options of one.
    let has_tool = words.iter().take(4).any(|w| tool(w));
    let has_opts = words.iter().any(|w| w.starts_with("+define+") || w.starts_with("+incdir+"));
    (has_tool || has_opts).then(|| words.to_vec())
}

/// Values in order, each once.
#[derive(Default)]
struct Ordered {
    v: Vec<String>,
    seen: rustc_hash::FxHashSet<String>,
}

impl Ordered {
    fn push(&mut self, x: &str) {
        if self.seen.insert(x.to_string()) {
            self.v.push(x.to_string());
        }
    }
}

/// The first compile of a log (see [`parse_runs`]).
#[cfg(test)]
pub fn parse(text: &str) -> SimLog {
    parse_runs(text).into_iter().next().unwrap_or_default()
}

/// The compiles of a log, one per compile command (`vlogan` for each library, say), each with
/// the files parsed after its command line and the defines and include directories of that
/// command. Files parsed before any command line belong to the first.
pub fn parse_runs(text: &str) -> Vec<SimLog> {
    let mut runs = Vec::new();
    let mut run = Run::default();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        // A Questa GUI transcript starts its lines with `# `.
        let line = line.strip_prefix("# ").unwrap_or(line);
        // The same command twice in a row (a Questa transcript echoes `vlog ...` as typed, then
        // vlog prints it again with its options quoted) is one compile.
        if run.command && run.files.v.is_empty() {
            if let Some(words) = command_words(line) {
                let plain = |ws: &[String]| ws.iter().map(|w| w.trim_matches('"').to_string()).collect::<Vec<_>>();
                let at = words.iter().position(|w| tool_name(w).is_some()).map_or(0, |i| i + 1);
                if plain(&words[at..]) == plain(&run.words) {
                    continue;
                }
            }
        }
        if command_words(line).is_some() && run.command {
            runs.push(std::mem::take(&mut run).finish());
        }
        run.line(line, &mut lines);
    }
    runs.push(run.finish());
    runs
}

/// One compile of a log, being read.
#[derive(Default)]
struct Run {
    files: Ordered,
    lib_files: Ordered,
    included: Ordered,
    defines: Ordered,
    incdirs: Ordered,
    command: bool,
    tool: String,
    words: Vec<String>,
}

impl Run {
    fn finish(self) -> SimLog {
        SimLog { files: self.files.v, lib_files: self.lib_files.v, included: self.included.v, defines: self.defines.v, incdirs: self.incdirs.v, command: self.command, tool: self.tool, words: self.words }
    }

    fn line<'a>(&mut self, line: &str, lines: &mut impl Iterator<Item = &'a str>) {
        let Run { files, lib_files, included, defines, incdirs, command, tool, words: command_line } = self;
        if let Some(p) = parsed_file(line, "design") {
            files.push(p);
        } else if let Some(p) = parsed_file(line, "library") {
            lib_files.push(p);
        } else if let Some(p) = parsed_file(line, "included") {
            included.push(p);
        } else if command_words(line).is_some() {
            // The whole command: a line ending in `\` goes on with the next one.
            let mut logical = line.to_string();
            while logical.ends_with('\\') {
                logical.pop();
                let Some(next) = lines.next() else { break };
                logical.push_str(next.trim_end());
            }
            let words = command_words(&logical).unwrap_or_default();
            *command = true;
            if let Some(at) = words.iter().position(|w| tool_name(w).is_some()) {
                *tool = tool_name(&words[at]).unwrap_or_default().to_string();
                *command_line = words[at + 1..].to_vec();
            }
            let mut i = 0;
            while i < words.len() {
                let w = words[i].as_str();
                if let Some(rest) = w.strip_prefix("+define+") {
                    for d in crate::split_defines(rest) {
                        defines.push(&d);
                    }
                } else if let Some(rest) = w.strip_prefix("+incdir+") {
                    for d in split_plus(rest) {
                        incdirs.push(&d);
                    }
                } else if (w == "-define" || w == "-D") && i + 1 < words.len() {
                    i += 1;
                    defines.push(&words[i]);
                } else if w == "-incdir" && i + 1 < words.len() {
                    i += 1;
                    incdirs.push(&split_plus(&words[i]).concat());
                }
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_vcs_compile_log() {
        let log = "\
Command  = vwi vcs -full64 -sverilog +define+FCOV_disable +define+A=1+B -define C +incdir+inc+../other -f rtl.vfiles
                         Chronologic VCS (TM)
Warning-[UNK_COMP_ARG] Unknown compile time plus argument used
Parsing design file '/tools/uvm/uvm_pkg.sv'
Parsing included file '/tools/uvm/uvm_macros.svh'.
Parsing design file 'rtl/top.sv'
Parsing design file '/tools/uvm/uvm_pkg.sv'
Parsing library file '/lib/cells/and2.v'
Top Level Modules:
";
        let s = parse(log);
        assert_eq!(s.files, vec!["/tools/uvm/uvm_pkg.sv", "rtl/top.sv"]);
        assert_eq!(s.lib_files, vec!["/lib/cells/and2.v"]);
        assert_eq!(s.included, vec!["/tools/uvm/uvm_macros.svh"]);
        assert_eq!(s.defines, vec!["FCOV_disable", "A=1", "B", "C"]);
        assert_eq!(s.incdirs, vec!["inc", "../other"]);
        assert!(s.command);
    }

    #[test]
    fn reads_a_vlogan_command_line_and_ignores_other_text() {
        let s = parse("vlogan -sverilog +define+X +incdir+a\nThe command vcs was not used\nParsing design file \"b.sv\"\n");
        assert_eq!((s.defines, s.incdirs, s.files), (vec!["X".to_string()], vec!["a".to_string()], vec!["b.sv".to_string()]));
        // Prose that mentions the tool is not a command line.
        assert!(parse("Please rerun vcs with -debug\n").defines.is_empty());
    }

    #[test]
    fn one_compile_per_command() {
        let log = "Parsing design file 'early.sv'\nCommand: vlogan +define+A lib_a.sv\nParsing design file 'lib_a.sv'\nCommand: vlogan +define+B lib_b.sv\nParsing design file 'lib_a.sv'\nParsing design file 'lib_b.sv'\nCommand: vcs top -o simv\n";
        let runs = parse_runs(log);
        let got: Vec<(Vec<String>, Vec<String>)> = runs.iter().map(|r| (r.files.clone(), r.defines.clone())).collect();
        assert_eq!(
            got,
            vec![
                (vec!["early.sv".to_string(), "lib_a.sv".to_string()], vec!["A".to_string()]),
                (vec!["lib_a.sv".to_string(), "lib_b.sv".to_string()], vec!["B".to_string()]),
                (vec![], vec![]),
            ]
        );
    }

    #[test]
    fn quoted_values_and_continued_command_lines() {
        let s = parse("Command: vcs +incdir+\"include dir\" +define+MSG=\"hello world\" \\\n   -incdir 'other dir' +define+B\nParsing design file 'a.sv'\n");
        assert_eq!(s.incdirs, vec!["include dir", "other dir"]);
        assert_eq!(s.defines, vec!["MSG=\"hello world\"", "B"]);
        assert_eq!(s.files, vec!["a.sv"]);
        // A word continued on the next line, a number with an apostrophe, a quoted + in a folder.
        let s = parse("Command: vcs +incdir+inc\\\n/headers +define+WIDTH=8'hff +incdir+\"a+b\" +define+B\n");
        assert_eq!(s.incdirs, vec!["inc/headers", "a+b"]);
        assert_eq!(s.defines, vec!["WIDTH=8'hff", "B"]);
    }
}
