//! What a simulator compiled, read from its log (`+dvicode_init_from_simlog`): the VCS and
//! vlogan messages `Parsing design file '...'`, `Parsing library file '...'` and
//! `Parsing included file '...'`, and the defines and include directories of the command line
//! the log shows (`Command: vcs ...`, `Command  = vlogan ...`, or a line starting with the tool).

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
}

/// The quoted path of a `Parsing <kind> file '<path>'` message.
fn parsed_file<'a>(line: &'a str, kind: &str) -> Option<&'a str> {
    let rest = line.strip_prefix("Parsing ")?.strip_prefix(kind)?.strip_prefix(" file ")?;
    let rest = rest.strip_prefix('\'').or_else(|| rest.strip_prefix('"'))?;
    let end = rest.find(['\'', '"'])?;
    Some(&rest[..end]).filter(|p| !p.is_empty())
}

/// The command line of a log line, as words: `Command: vcs ...`, `Command  = vwi vlogan ...`,
/// `Command arguments: ...`, or a line whose first word is the tool.
fn command_words(line: &str) -> Option<Vec<&str>> {
    let tool = |w: &str| {
        let name = w.rsplit('/').next().unwrap_or(w);
        matches!(name, "vcs" | "vlogan" | "vhdlan")
    };
    let words: Vec<&str> = line.split_whitespace().collect();
    let start = if line.trim_start().starts_with("Command") {
        // Up to the separator, then the words of the command (possibly a wrapper first).
        let sep = words.iter().position(|w| w.ends_with(':') || *w == "=" || w.ends_with('='))?;
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

pub fn parse(text: &str) -> SimLog {
    let mut s = SimLog::default();
    let push = |v: &mut Vec<String>, x: &str| {
        if !v.iter().any(|y| y == x) {
            v.push(x.to_string());
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(p) = parsed_file(line, "design") {
            push(&mut s.files, p);
        } else if let Some(p) = parsed_file(line, "library") {
            push(&mut s.lib_files, p);
        } else if let Some(p) = parsed_file(line, "included") {
            push(&mut s.included, p);
        } else if let Some(words) = command_words(line) {
            s.command = true;
            let mut i = 0;
            while i < words.len() {
                let w = words[i];
                if let Some(rest) = w.strip_prefix("+define+") {
                    for d in crate::split_defines(rest) {
                        push(&mut s.defines, &d);
                    }
                } else if let Some(rest) = w.strip_prefix("+incdir+") {
                    for d in rest.split('+').filter(|d| !d.is_empty()) {
                        push(&mut s.incdirs, d);
                    }
                } else if (w == "-define" || w == "-D") && i + 1 < words.len() {
                    i += 1;
                    push(&mut s.defines, words[i]);
                } else if w == "-incdir" && i + 1 < words.len() {
                    i += 1;
                    push(&mut s.incdirs, words[i]);
                }
                i += 1;
            }
        }
    }
    s
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
}
