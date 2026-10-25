//! Document and range formatting: the options (the `dvicode.format` settings, a project's
//! `.dvicode/format.json` over them, the editor's tab size and spaces/tabs when not set) and
//! the edits as LSP text edits.

use dvi_syntax::format::{self, Directives, LineEdit, Options};
use lsp_types::{FormattingOptions, Position, Range, TextEdit};
use serde_json::Value;
use std::path::Path;

/// The options for a file at `path`: `settings` (the `dvicode.format` section), the nearest
/// `.dvicode/format.json` above the file over them, and the editor's `editor` options for
/// what neither sets. Invalid values are ignored one by one (logged). `None`: formatting is
/// turned off.
pub fn options(settings: &Value, path: Option<&Path>, editor: &FormattingOptions) -> Option<Options> {
    let mut o = Options { indent_size: (editor.tab_size as usize).clamp(1, 16), use_tabs: !editor.insert_spaces, ..Options::default() };
    let mut enable = true;
    let mut apply = |v: &Value, from: &str| {
        let Some(m) = v.as_object() else { return };
        for (k, x) in m {
            let ok = match k.as_str() {
                "enable" => x.as_bool().map(|b| enable = b).is_some(),
                "indentSize" => x.as_u64().filter(|n| (1..=16).contains(n)).map(|n| o.indent_size = n as usize).is_some() || x.is_null(),
                "useTabs" => x.as_bool().map(|b| o.use_tabs = b).is_some() || x.is_null(),
                "indentCaseItems" => x.as_bool().map(|b| o.indent_case_items = b).is_some(),
                "listIndent" => match x.as_str() {
                    Some("indent") => {
                        o.align_lists = false;
                        true
                    }
                    Some("align") => {
                        o.align_lists = true;
                        true
                    }
                    _ => false,
                },
                "continuationIndent" => x.as_u64().filter(|n| *n <= 4).map(|n| o.continuation = n as usize).is_some(),
                "preprocessorIndent" => match x.as_str() {
                    Some("keep") => {
                        o.directives = Directives::Keep;
                        true
                    }
                    Some("column0") => {
                        o.directives = Directives::Column0;
                        true
                    }
                    Some("indent") => {
                        o.directives = Directives::Indent;
                        true
                    }
                    _ => false,
                },
                "trimTrailingWhitespace" => x.as_bool().map(|b| o.trim_trailing = b).is_some(),
                "maxBlankLines" => x.as_u64().filter(|n| *n <= 100).map(|n| o.max_blank_lines = n as usize).is_some(),
                _ => false,
            };
            if !ok {
                log::warn!("format: ignored {k} = {x} ({from})");
            }
        }
    };
    apply(settings, "settings");
    if let Some(p) = path.and_then(project_file) {
        match std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            Some(v) => apply(&v, &p.display().to_string()),
            None => log::warn!("format: {} is not valid JSON (ignored)", p.display()),
        }
    }
    enable.then_some(o)
}

/// The nearest `.dvicode/format.json` in the folders above `file`.
fn project_file(file: &Path) -> Option<std::path::PathBuf> {
    let mut dir = file.parent();
    let mut n = 0;
    while let Some(d) = dir {
        let p = d.join(".dvicode").join("format.json");
        if p.is_file() {
            return Some(p);
        }
        n += 1;
        if n > 64 {
            break;
        }
        dir = d.parent();
    }
    None
}

/// The edits formatting `text` gives (only those of the lines `lines`, `[a, b)`, when given),
/// checked once applied; `None` when the result would change more than whitespace (logged).
pub fn edits(text: &str, o: &Options, lines: Option<(usize, usize)>) -> Option<Vec<TextEdit>> {
    let f = format::format(text, o)?;
    if let Some(range) = lines {
        let out = format::apply(text, &f, Some(range));
        if let Some(d) = format::difference(text, &out, o.max_blank_lines > 0) {
            log::error!("format: range result refused: {d}");
            return None;
        }
    }
    // Line starts, to cut text by line.
    let mut starts = vec![0usize];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let line_text = |i: usize| {
        let s = starts[i];
        let e = starts.get(i + 1).map_or(text.len(), |n| n - 1);
        let e = if e > s && text.as_bytes()[e - 1] == b'\r' { e - 1 } else { e };
        &text[s..e]
    };
    let col = |line: &str, byte: usize| line[..byte].encode_utf16().count() as u32;
    let mut out = Vec::new();
    for (i, e) in &f.edits {
        if lines.is_some_and(|(a, b)| !(a <= *i && *i < b)) {
            continue;
        }
        let ln = *i as u32;
        let l = line_text(*i);
        match e {
            LineEdit::Indent { old, new } => out.push(TextEdit { range: Range::new(Position::new(ln, 0), Position::new(ln, col(l, *old))), new_text: new.clone() }),
            LineEdit::Trim { from } => out.push(TextEdit { range: Range::new(Position::new(ln, col(l, *from)), Position::new(ln, col(l, l.len()))), new_text: String::new() }),
            LineEdit::Both { old, new, from } => {
                out.push(TextEdit { range: Range::new(Position::new(ln, 0), Position::new(ln, col(l, *old))), new_text: new.clone() });
                out.push(TextEdit { range: Range::new(Position::new(ln, col(l, *from)), Position::new(ln, col(l, l.len()))), new_text: String::new() });
            }
            LineEdit::Delete => out.push(TextEdit { range: Range::new(Position::new(ln, 0), Position::new(ln + 1, 0)), new_text: String::new() }),
        }
    }
    Some(out)
}
