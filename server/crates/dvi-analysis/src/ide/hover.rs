//! Hover: declaration signature, location and documentation comment.

use super::{Hit, Target};
use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::{dflags, Decl, DeclKind};
use dvi_syntax::preproc::{expand_snippet, join_tokens, MacroDef};
use dvi_syntax::{FileId, SourceProvider, Span};

impl Analysis {
    /// Markdown hover text and the span it applies to.
    pub fn hover(&self, file: FileId, off: u32) -> Option<(String, Span)> {
        let hit = self.hit_at(file, off)?;
        let span = hit.span();
        let target = self.target_of(&hit)?;
        let md = match target {
            Target::Decl(r) => self.decl_hover(r),
            Target::Macro(m) => {
                let mut s = self.macro_hover(&m);
                if let Hit::MacroUse { unit, idx, .. } = hit {
                    if let Some(exp) = self.expand_macro_use(unit, idx, 1) {
                        let exp = if exp.len() > 2000 { format!("{}...", &exp[..exp.char_indices().nth(1990).map(|c| c.0).unwrap_or(exp.len())]) } else { exp };
                        s.push_str("\n\n**Expansion**\n```systemverilog\n");
                        s.push_str(&exp);
                        s.push_str("\n```");
                    }
                }
                s
            }
            Target::File(f) => format!("`{}`", self.vfs.path(f).display()),
        };
        Some((md, span))
    }

    pub fn macro_hover(&self, m: &MacroDef) -> String {
        let mut body = m.body_text.clone();
        let lines: Vec<&str> = body.lines().collect();
        if lines.len() > 30 {
            body = format!("{}\n...", lines[..30].join("\n"));
        }
        let mut s = format!("```systemverilog\n{} {}\n```", m.signature(), body.replace("\\\n", "\n"));
        if m.name_span.file != dvi_syntax::preproc::COMMAND_LINE_FILE {
            s.push_str(&format!("\n\n{}", self.location_line(m.name_span)));
            if let Some(doc) = self.doc_comment(m.name_span.file, line_start_of(self, m.name_span)) {
                s.push_str("\n\n");
                s.push_str(&doc);
            }
        } else {
            s.push_str("\n\nDefined on the command line (+define+)");
        }
        s
    }

    fn location_line(&self, span: Span) -> String {
        let path = self.vfs.path(span.file);
        let line = self.to_range(span).map(|r| r.start.line + 1).unwrap_or(0);
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        format!("*{name}:{line}*")
    }

    pub fn decl_signature(&self, r: DeclRef) -> String {
        let d = self.decl(r);
        let ty = |d: &Decl| d.ty.as_ref().map(|t| t.text.to_string());
        let mut quals = String::new();
        for (f, s) in [
            (dflags::LOCAL, "local "),
            (dflags::PROTECTED, "protected "),
            (dflags::STATIC, "static "),
            (dflags::PURE, "pure "),
            (dflags::VIRTUAL, "virtual "),
            (dflags::RAND, "rand "),
            (dflags::CONST, "const "),
        ] {
            if d.has(f) {
                quals.push_str(s);
            }
        }
        match d.kind {
            DeclKind::Class | DeclKind::InterfaceClass | DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Checker | DeclKind::Package | DeclKind::Covergroup => {
                self.header_text(d).unwrap_or_else(|| format!("{} {}", d.kind.label(), d.name))
            }
            DeclKind::Function | DeclKind::Task => {
                let owner = if d.has(dflags::OUT_OF_BLOCK) { self.owner_class(r) } else { d.parent.map(|p| DeclRef { unit: r.unit, decl: p }) };
                let owner = owner.filter(|o| matches!(self.decl(*o).kind, DeclKind::Class | DeclKind::InterfaceClass));
                let detail = d.detail.to_string();
                match owner {
                    Some(o) => {
                        let on = &self.decl(o).name;
                        format!("{quals}{}", detail.replacen(&format!(" {}(", d.name), &format!(" {on}::{}(", d.name), 1))
                    }
                    None => format!("{quals}{detail}"),
                }
            }
            DeclKind::Variable | DeclKind::Net | DeclKind::Field | DeclKind::Port | DeclKind::Argument | DeclKind::Instance => {
                let mut s = quals;
                if let Some(dir) = d.dir {
                    s.push_str(dir.as_str());
                    s.push(' ');
                }
                s.push_str(&ty(d).unwrap_or_else(|| if d.kind == DeclKind::Net { "wire".into() } else { "logic".into() }));
                s.push(' ');
                s.push_str(&d.name);
                if !d.detail.is_empty() && d.has(dflags::CONST) {
                    s.push_str(" = ");
                    s.push_str(&d.detail);
                }
                if !d.detail.is_empty() && d.kind == DeclKind::Argument {
                    s.push_str(" = ");
                    s.push_str(&d.detail);
                }
                s
            }
            DeclKind::Parameter | DeclKind::LocalParam => {
                let kw = if d.kind == DeclKind::Parameter { "parameter" } else { "localparam" };
                let t = ty(d).map(|t| format!("{t} ")).unwrap_or_default();
                if d.detail.is_empty() {
                    format!("{kw} {t}{}", d.name)
                } else {
                    format!("{kw} {t}{} = {}", d.name, d.detail)
                }
            }
            DeclKind::TypeParam => {
                if d.detail.is_empty() {
                    format!("parameter type {}", d.name)
                } else {
                    format!("parameter type {} = {}", d.name, d.detail)
                }
            }
            DeclKind::Typedef => {
                if d.has(dflags::FORWARD) {
                    format!("typedef class {}", d.name)
                } else {
                    format!("typedef {} {}", ty(d).unwrap_or_default(), d.name)
                }
            }
            DeclKind::EnumMember => {
                if d.detail.is_empty() {
                    d.name.to_string()
                } else {
                    format!("{} = {}", d.name, d.detail)
                }
            }
            DeclKind::Genvar => format!("genvar {}", d.name),
            _ => format!("{} {}", d.kind.label(), d.name),
        }
    }

    /// Source of a declaration header up to the first `;` (classes, modules...).
    fn header_text(&self, d: &Decl) -> Option<String> {
        if d.span.file != d.name_span.file || d.has(dflags::FROM_MACRO) {
            return None;
        }
        let text = self.file_text(d.span.file)?;
        let start = d.span.start as usize;
        let rest = text.get(start..)?;
        let mut depth = 0i32;
        let mut end = rest.len().min(1500);
        for (i, c) in rest.char_indices() {
            if i >= 1500 {
                break;
            }
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ';' if depth <= 0 => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        let h: Vec<&str> = rest[..end].lines().map(|l| l.trim_end()).collect();
        let h = if h.len() > 25 { format!("{}\n  ...", h[..25].join("\n")) } else { h.join("\n") };
        Some(strip_comments(&h))
    }

    pub fn decl_hover(&self, r: DeclRef) -> String {
        let d = self.decl(r);
        let mut s = format!("```systemverilog\n{}\n```", self.decl_signature(r));
        // Where it lives.
        let container = if d.has(dflags::OUT_OF_BLOCK) {
            self.owner_class(r)
        } else {
            let mut p = d.parent;
            let u = self.unit(r.unit);
            while let Some(x) = p {
                let pd = u.parsed.decl(x);
                if !pd.name.is_empty() && !matches!(pd.kind, DeclKind::Block | DeclKind::StructType | DeclKind::EnumType) {
                    break;
                }
                p = pd.parent;
            }
            p.map(|p| DeclRef { unit: r.unit, decl: p })
        };
        let mut info = Vec::new();
        if let Some(c) = container {
            let cd = self.decl(c);
            if cd.kind != DeclKind::Unit {
                info.push(format!("{} `{}`", cd.kind.label(), self.qualified_name(c)));
            }
        }
        if d.kind == DeclKind::Class {
            if let Some(b) = self.class_base(r) {
                info.push(format!("extends `{}`", self.qualified_name(b)));
            }
        }
        if !self.vfs.is_builtin(d.name_span.file) && !d.has(dflags::FROM_MACRO) {
            info.push(self.location_line(d.name_span));
        } else if self.vfs.is_builtin(d.name_span.file) {
            info.push("built-in".into());
        }
        if !info.is_empty() {
            s.push_str("\n\n");
            s.push_str(&info.join(" · "));
        }
        if !d.has(dflags::FROM_MACRO) {
            let start = if d.span.file == d.name_span.file { d.span.start } else { d.name_span.start };
            if let Some(doc) = self.doc_comment(d.name_span.file, start).or_else(|| self.trailing_comment(d.name_span.file, d.name_span.end)) {
                s.push_str("\n\n---\n\n");
                s.push_str(&doc);
            }
        }
        // Overridden method.
        if matches!(d.kind, DeclKind::Function | DeclKind::Task) {
            let class = if d.has(dflags::OUT_OF_BLOCK) { self.owner_class(r) } else { d.parent.map(|p| DeclRef { unit: r.unit, decl: p }) };
            if let Some(c) = class.filter(|c| self.decl(*c).kind == DeclKind::Class) {
                if let Some(base) = self.class_base(c) {
                    if let Some(over) = self.class_member(base, &d.name, 0) {
                        if over != r && d.name != "new" {
                            s.push_str(&format!("\n\nOverrides `{}`", self.qualified_name(over)));
                        }
                    }
                }
            }
        }
        s
    }

    /// Comment block right above `offset` (contiguous `//` lines or one `/* */` block).
    pub fn doc_comment(&self, file: FileId, offset: u32) -> Option<String> {
        let text = self.file_text(file)?;
        let before = text.get(..offset as usize)?;
        // Start of the declaration's line; text before it on the same line must be blank.
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        if !before[line_start..].trim().is_empty() {
            // Something precedes the declaration on its line (e.g. `rand int x;`): look above.
            let pre = before[line_start..].trim();
            if pre.contains(';') {
                return None;
            }
        }
        let mut lines: Vec<&str> = before[..line_start].lines().collect();
        let mut out: Vec<String> = Vec::new();
        // Skip attribute or blank? A blank line ends the comment block.
        if let Some(last) = lines.last() {
            let t = last.trim();
            if t.ends_with("*/") {
                // Block comment.
                let mut block = Vec::new();
                while let Some(l) = lines.pop() {
                    block.push(l);
                    if l.contains("/*") {
                        break;
                    }
                }
                block.reverse();
                for l in block {
                    let t = l.trim().trim_start_matches("/**").trim_start_matches("/*").trim_end_matches("*/").trim();
                    let t = t.strip_prefix('*').map(|x| x.trim_start()).unwrap_or(t);
                    out.push(t.to_string());
                }
            } else {
                while let Some(l) = lines.last() {
                    let t = l.trim();
                    if let Some(c) = t.strip_prefix("//") {
                        let c = c.trim_start_matches('/').trim_start_matches('!');
                        out.push(c.strip_prefix(' ').unwrap_or(c).to_string());
                        lines.pop();
                    } else {
                        break;
                    }
                }
                out.reverse();
            }
        }
        // Drop separator lines like "-----".
        let out: Vec<String> = out
            .into_iter()
            .filter(|l| !(l.len() > 3 && l.chars().all(|c| matches!(c, '-' | '=' | '*' | '/' | '#' | ' '))))
            .collect();
        let s = out.join("\n").trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// `// comment` after the declaration on the same line.
    pub fn trailing_comment(&self, file: FileId, offset: u32) -> Option<String> {
        let text = self.file_text(file)?;
        let rest = text.get(offset as usize..)?;
        let line = rest.lines().next()?;
        let i = line.find("//")?;
        let c = line[i + 2..].trim();
        (!c.is_empty()).then(|| c.to_string())
    }

    /// Text of a macro call expanded `levels` deep (1 = one level, usize::MAX = all).
    pub fn expand_macro_use(&self, unit: u32, idx: usize, levels: usize) -> Option<String> {
        let u = self.unit(unit);
        let mu = u.macro_uses.get(idx)?;
        let text = self.file_text(mu.call_span.file)?;
        let call = text.get(mu.call_span.start as usize..mu.call_span.end as usize)?;
        let toks = dvi_syntax::lexer::lex(call, mu.call_span.file);
        let cfg = self.setup.pp_configs.get(u.invocation).cloned().unwrap_or_default();
        let out = expand_snippet(&*self.vfs, &cfg, mu.call_span.file, toks, u.macros_after.clone(), levels);
        Some(format_expansion(&join_tokens(&out)))
    }
}

fn line_start_of(a: &Analysis, span: Span) -> u32 {
    let Some(text) = a.file_text(span.file) else { return span.start };
    text.get(..span.start as usize).and_then(|b| b.rfind('\n')).map(|i| i as u32 + 1).unwrap_or(0)
}

fn strip_comments(s: &str) -> String {
    s.lines()
        .map(|l| match l.find("//") {
            Some(i) => l[..i].trim_end(),
            None => l,
        })
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Breaks expanded macro text into lines for display.
pub fn format_expansion(s: &str) -> String {
    let mut out = String::new();
    let mut indent = 0usize;
    let toks: Vec<&str> = s.split(' ').collect();
    let mut line = String::new();
    let flush = |line: &mut String, out: &mut String, indent: usize| {
        let t = line.trim();
        if !t.is_empty() {
            out.push_str(&"  ".repeat(indent));
            out.push_str(t);
            out.push('\n');
        }
        line.clear();
    };
    for t in toks {
        match t {
            "begin" | "fork" => {
                line.push_str(t);
                flush(&mut line, &mut out, indent);
                indent += 1;
            }
            "end" | "join" | "join_none" | "join_any" | "endfunction" | "endtask" | "endclass" | "endcase" => {
                flush(&mut line, &mut out, indent);
                indent = indent.saturating_sub(1);
                line.push_str(t);
                line.push(' ');
            }
            _ => {
                line.push_str(t);
                if t.ends_with(';') {
                    flush(&mut line, &mut out, indent);
                } else {
                    line.push(' ');
                }
            }
        }
    }
    flush(&mut line, &mut out, indent);
    out.trim_end().to_string()
}
