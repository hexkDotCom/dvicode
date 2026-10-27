//! `formatter-drift` (style guide FMT-13): lines the project's formatter would change, when the
//! project names DVICode's formatter.

use crate::lint::{Cx, Level, Rule, RuleMeta, Ruleset};
use dvi_syntax::format::{self, Directives, Options};
use dvi_syntax::Span;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct FormatterDrift;

static META: RuleMeta = RuleMeta {
    id: "formatter-drift",
    guide: &["FMT-13"],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "Lines differ from what the project's formatter makes.",
    why: "A project that names its formatter keeps its code as the formatter makes it, so that layout is never argued about or changed by hand (FMT-13). Checked only when .dvicode/lint.json names DVICode's formatter (\"formatter\": \"dvicode\").",
    params: &[],
    text_only: true,
};

/// The nearest `.dvicode/format.json` above `file`.
fn format_json(file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent();
    while let Some(d) = dir {
        let p = d.join(".dvicode").join("format.json");
        if dvi_project::fsx::is_file(&p) {
            return Some(p);
        }
        dir = d.parent();
    }
    None
}

/// The formatter options of `file`: the defaults, then its project's `format.json` (as the
/// editor reads it, without the editor's own settings). `None`: formatting is turned off.
pub fn options_for(file: &Path) -> Option<Options> {
    let mut o = Options { indent_size: 2, use_tabs: false, ..Options::default() };
    let Some(p) = format_json(file) else { return Some(o) };
    // An unreadable or invalid file is ignored, as the editor's formatter ignores it.
    let Some(v) = crate::lint::read_control_file(&p).and_then(|t| serde_json::from_slice::<Value>(&t).ok()) else { return Some(o) };
    let Some(m) = v.as_object() else { return Some(o) };
    for (k, x) in m {
        match k.as_str() {
            "enable" if x.as_bool() == Some(false) => return None,
            "indentSize" => {
                if let Some(n) = x.as_u64().filter(|n| (1..=16).contains(n)) {
                    o.indent_size = n as usize;
                }
            }
            "useTabs" => o.use_tabs = x.as_bool().unwrap_or(o.use_tabs),
            "indentCaseItems" => o.indent_case_items = x.as_bool().unwrap_or(o.indent_case_items),
            "listIndent" => o.align_lists = x.as_str() == Some("align"),
            "continuationIndent" => {
                if let Some(n) = x.as_u64().filter(|n| *n <= 4) {
                    o.continuation = n as usize;
                }
            }
            "preprocessorIndent" => {
                o.directives = match x.as_str() {
                    Some("column0") => Directives::Column0,
                    Some("indent") => Directives::Indent,
                    _ => Directives::Keep,
                }
            }
            "trimTrailingWhitespace" => o.trim_trailing = x.as_bool().unwrap_or(o.trim_trailing),
            "maxBlankLines" => {
                if let Some(n) = x.as_u64().filter(|n| *n <= 100) {
                    o.max_blank_lines = n as usize;
                }
            }
            _ => {}
        }
    }
    Some(o)
}

impl Rule for FormatterDrift {
    fn meta(&self) -> &'static RuleMeta {
        &META
    }
    fn check(&self, cx: &mut Cx) {
        if !cx.formatter_named {
            return;
        }
        let Some(text) = cx.a.file_text(cx.file) else { return };
        let Some(li) = cx.a.file_lines(cx.file) else { return };
        let Some(opts) = options_for(&cx.a.path(cx.file)) else { return };
        let Some(f) = format::format(&text, &opts) else {
            cx.unknown("the formatter cannot format this file");
            return;
        };
        // Runs of changed lines, one finding each.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for (line, _) in &f.edits {
            match runs.last_mut() {
                Some((_, e)) if *e + 1 == *line => *e = *line,
                _ => runs.push((*line, *line)),
            }
        }
        for (s, e) in runs {
            if !cx.step(1) {
                return;
            }
            let start = li.line_start(s as u32);
            let end = if (e as u32 + 1) < li.line_count() { li.line_start(e as u32 + 1).saturating_sub(1) } else { text.len() as u32 };
            let what = if s == e { format!("Line {} is", s + 1) } else { format!("Lines {}-{} are", s + 1, e + 1) };
            cx.report(None, Span::new(cx.file, start, end.max(start)), format!("{what} not as the formatter makes them: format the file (Format Document)"), Vec::new());
        }
    }
}
