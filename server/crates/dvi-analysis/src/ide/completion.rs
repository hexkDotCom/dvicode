//! Content assist and signature help.

use crate::world::{Analysis, DeclRef, UnitId, UnitKind};
use dvi_syntax::ast::{dflags, Decl, DeclId, DeclKind, ParsedUnit, RefChain, RefKind, RefSeg, Sep};
use dvi_syntax::lexer::lex;
use dvi_syntax::token::{TokKind, KEYWORDS};
use dvi_syntax::{FileId, Span};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, Documentation, InsertTextFormat, MarkupContent, MarkupKind,
    ParameterInformation, ParameterLabel, SignatureHelp, SignatureInformation,
};
use rustc_hash::FxHashSet;
use smol_str::SmolStr;

const MAX_ITEMS: usize = 3000;

pub fn completion_kind(d: &Decl, in_class: bool) -> CompletionItemKind {
    match d.kind {
        DeclKind::Module | DeclKind::Program | DeclKind::Checker | DeclKind::Primitive => CompletionItemKind::MODULE,
        DeclKind::Interface | DeclKind::InterfaceClass | DeclKind::Modport => CompletionItemKind::INTERFACE,
        DeclKind::Package => CompletionItemKind::MODULE,
        DeclKind::Class | DeclKind::Covergroup => CompletionItemKind::CLASS,
        DeclKind::Function | DeclKind::Task => {
            if d.name == "new" {
                CompletionItemKind::CONSTRUCTOR
            } else if in_class {
                CompletionItemKind::METHOD
            } else {
                CompletionItemKind::FUNCTION
            }
        }
        DeclKind::Variable if in_class => CompletionItemKind::FIELD,
        DeclKind::Field => CompletionItemKind::FIELD,
        DeclKind::Variable | DeclKind::Net | DeclKind::Port | DeclKind::Argument | DeclKind::Genvar | DeclKind::Instance => CompletionItemKind::VARIABLE,
        DeclKind::Parameter | DeclKind::LocalParam => CompletionItemKind::CONSTANT,
        DeclKind::TypeParam | DeclKind::Typedef | DeclKind::StructType => CompletionItemKind::STRUCT,
        DeclKind::EnumType => CompletionItemKind::ENUM,
        DeclKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        DeclKind::Constraint => CompletionItemKind::PROPERTY,
        DeclKind::Property | DeclKind::Sequence => CompletionItemKind::EVENT,
        _ => CompletionItemKind::TEXT,
    }
}

struct Ctx {
    unit: UnitId,
    scope: DeclId,
    /// Text of the line before the cursor.
    line: String,
}

impl Analysis {
    fn completion_ctx(&self, file: FileId, off: u32) -> Option<Ctx> {
        let text = self.file_text(file)?;
        let before = text.get(..off as usize)?;
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line = before[line_start..].to_string();
        let unit = *self.units_for(file).first()?;
        let scope = self.scope_at(unit, file, off);
        Some(Ctx { unit, scope, line })
    }

    pub fn completions(&self, file: FileId, off: u32) -> Vec<CompletionItem> {
        let Some(ctx) = self.completion_ctx(file, off) else { return Vec::new() };
        self.completions_in(&ctx, file)
    }

    /// [`Analysis::completions`] at `off` in this snapshot's text, with `line` as the text of the
    /// line before the cursor: the editor's, newer than this snapshot, while an update of edits
    /// on that line only is under way (`Analysis::line_local_scope`).
    pub fn completions_with_line(&self, file: FileId, off: u32, line: &str) -> Vec<CompletionItem> {
        let Some(&unit) = self.units_for(file).first() else { return Vec::new() };
        let ctx = Ctx { unit, scope: self.scope_at(unit, file, off), line: line.to_string() };
        self.completions_in(&ctx, file)
    }

    fn completions_in(&self, ctx: &Ctx, file: FileId) -> Vec<CompletionItem> {
        let toks = lex(&ctx.line, file);
        // Partial identifier being typed.
        let mut end = toks.len();
        let mut prefix = String::new();
        if let Some(t) = toks.last() {
            if matches!(t.kind, TokKind::Ident | TokKind::Keyword) && t.span.end as usize == ctx.line.len() {
                prefix = t.text.to_string();
                end -= 1;
            }
            if t.kind == TokKind::Directive && t.span.end as usize == ctx.line.len() {
                return self.macro_completions();
            }
        }
        if ctx.line.ends_with('`') {
            return self.macro_completions();
        }
        let toks = &toks[..end];
        if let Some(last) = toks.last() {
            if (last.is_punct(".") || last.is_punct("::")) && last.span.end as usize == ctx.line.trim_end_matches(prefix.as_str()).len() {
                return self.member_completions(ctx, toks, &prefix);
            }
        }
        // A new item in a class body: the inherited methods to override come first.
        let mut out = if toks.is_empty() { self.override_completions(ctx, &prefix) } else { Vec::new() };
        out.extend(self.scope_completions(ctx, &prefix, file));
        out
    }

    /// The whole definition of each inherited method the class around `ctx` may override (see
    /// [`Analysis::overridable_methods`]), for a name being typed alone on its line.
    fn override_completions(&self, ctx: &Ctx, prefix: &str) -> Vec<CompletionItem> {
        let class = DeclRef { unit: ctx.unit, decl: ctx.scope };
        if self.decl(class).kind != DeclKind::Class {
            return Vec::new();
        }
        let mut out = Vec::new();
        for o in self.overridable_methods(class) {
            let d = self.decl(o.method);
            if !starts_like(&d.name, prefix) {
                continue;
            }
            let Some(text) = self.override_text(o.method, "	", true) else { continue };
            let what = if o.pure { "implement" } else { "override" };
            out.push(CompletionItem {
                label: d.name.to_string(),
                label_details: Some(CompletionItemLabelDetails { detail: None, description: Some(format!("{what} {}", self.decl(o.owner).name)) }),
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(format!("{what} {}::{}", self.decl(o.owner).name, d.name)),
                insert_text: Some(text),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                filter_text: Some(d.name.to_string()),
                sort_text: Some(format!("0{}", d.name)),
                ..Default::default()
            });
        }
        out
    }

    fn macro_completions(&self) -> Vec<CompletionItem> {
        let mut out = Vec::new();
        let mut names: Vec<_> = self.global.macros.iter().collect();
        names.sort_by(|a, b| a.0.cmp(b.0));
        for (n, defs) in names {
            let Some(m) = defs.first() else { continue };
            let (insert, fmt) = match &m.params {
                Some(ps) if !ps.is_empty() => {
                    let args: Vec<String> = ps.iter().enumerate().map(|(i, p)| format!("${{{}:{}}}", i + 1, p.name)).collect();
                    (format!("{n}({})", args.join(", ")), InsertTextFormat::SNIPPET)
                }
                Some(_) => (format!("{n}()"), InsertTextFormat::PLAIN_TEXT),
                None => (n.to_string(), InsertTextFormat::PLAIN_TEXT),
            };
            out.push(CompletionItem {
                label: n.to_string(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some(m.signature()),
                documentation: Some(Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value: format!("```systemverilog\n{}\n```", truncate(&m.body_text, 600)) })),
                insert_text: Some(insert),
                insert_text_format: Some(fmt),
                ..Default::default()
            });
            if out.len() >= MAX_ITEMS {
                break;
            }
        }
        out
    }

    /// Builds a reference chain from the tokens before a trailing `.`/`::` and resolves it.
    fn chain_before(&self, ctx: &Ctx, toks: &[dvi_syntax::Token]) -> Option<(DeclRef, Sep)> {
        let last = toks.last()?;
        let sep = if last.is_punct("::") { Sep::Scope } else { Sep::Dot };
        let mut i = toks.len() - 1; // index of the separator
        let mut rev: Vec<(SmolStr, bool, u8)> = Vec::new();
        let mut rev_seps: Vec<Sep> = Vec::new();
        loop {
            if i == 0 {
                return None;
            }
            i -= 1;
            let mut called = false;
            let mut selects = 0u8;
            while toks[i].is_punct("]") || toks[i].is_punct(")") {
                called |= toks[i].is_punct(")");
                if toks[i].is_punct("]") {
                    selects += 1;
                }
                let mut depth = 0i32;
                loop {
                    let t = &toks[i];
                    if t.is_punct("]") || t.is_punct(")") {
                        depth += 1;
                    } else if t.is_punct("[") || t.is_punct("(") {
                        depth -= 1;
                    }
                    if depth == 0 {
                        break;
                    }
                    if i == 0 {
                        return None;
                    }
                    i -= 1;
                }
                if i == 0 {
                    return None;
                }
                i -= 1;
                if toks[i].is_punct("#") {
                    called = false;
                    if i == 0 {
                        return None;
                    }
                    i -= 1;
                }
            }
            let t = &toks[i];
            if !(t.kind == TokKind::Ident || t.is_kw("this") || t.is_kw("super") || t.is_kw("new")) {
                return None;
            }
            rev.push((t.text.clone(), called, selects));
            if i > 0 && (toks[i - 1].is_punct(".") || toks[i - 1].is_punct("::")) {
                rev_seps.push(if toks[i - 1].is_punct(".") { Sep::Dot } else { Sep::Scope });
                i -= 1;
                continue;
            }
            break;
        }
        let n = rev.len();
        let segs: Vec<RefSeg> = rev
            .into_iter()
            .rev()
            .enumerate()
            .map(|(k, (name, called, selects))| RefSeg {
                name,
                span: Span::default(),
                sep: if k == 0 { Sep::None } else { rev_seps[n - 1 - k] },
                called,
                selects,
                from_macro: false,
            })
            .collect();
        let chain = RefChain { segs, scope: ctx.scope, kind: RefKind::Expr };
        let r = self.resolve_chain(ctx.unit, &chain, chain.segs.len() - 1)?;
        Some((r, sep))
    }

    fn member_completions(&self, ctx: &Ctx, toks: &[dvi_syntax::Token], prefix: &str) -> Vec<CompletionItem> {
        let Some((r, sep)) = self.chain_before(ctx, toks) else { return Vec::new() };
        let d = self.decl(r);
        let container = match (sep, d.kind) {
            (Sep::Scope, DeclKind::Package | DeclKind::Class | DeclKind::InterfaceClass) => Some(r),
            (Sep::Scope, DeclKind::Typedef) => self.follow_typedef(r),
            _ => self.type_of(r),
        };
        let Some(container) = container else { return Vec::new() };
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        for m in self.members_list(container) {
            let md = self.decl(m);
            if md.name.is_empty() || md.has(dflags::OUT_OF_BLOCK) || !starts_like(&md.name, prefix) {
                continue;
            }
            // Through `::` only static items and types make sense; keep all, VS Code filters.
            if seen.insert(md.name.clone()) {
                out.push(self.decl_completion(m));
            }
        }
        let cd = self.decl(container);
        if matches!(cd.kind, DeclKind::Class | DeclKind::InterfaceClass) && sep == Sep::Dot {
            for b in ["randomize", "rand_mode", "constraint_mode", "srandom"] {
                if seen.insert(SmolStr::new(b)) {
                    out.push(CompletionItem { label: b.into(), kind: Some(CompletionItemKind::METHOD), detail: Some("built-in".into()), ..Default::default() });
                }
            }
        }
        if cd.kind == DeclKind::EnumType {
            for b in ["first", "last", "next", "prev", "num", "name"] {
                out.push(CompletionItem { label: b.into(), kind: Some(CompletionItemKind::METHOD), detail: Some("enum method".into()), ..Default::default() });
            }
        }
        out
    }

    /// Members of a container; for classes including inherited ones (nearest first).
    pub fn members_list(&self, container: DeclRef) -> Vec<DeclRef> {
        let mut out = Vec::new();
        let mut cur = Some(container);
        let mut depth = 0;
        while let Some(c) = cur {
            out.extend(self.scope_members(c));
            depth += 1;
            if depth > 32 {
                break;
            }
            cur = match self.decl(c).kind {
                DeclKind::Class | DeclKind::InterfaceClass => self.class_base(c),
                _ => None,
            };
        }
        out
    }

    /// Named children of a scope plus hoisted enum items.
    pub fn scope_members(&self, scope: DeclRef) -> Vec<DeclRef> {
        let u = self.unit(scope.unit);
        let d = u.parsed.decl(scope.decl);
        let mut out = Vec::new();
        for &c in &d.children {
            let cd = u.parsed.decl(c);
            if !cd.name.is_empty() {
                out.push(DeclRef { unit: scope.unit, decl: c });
            }
            // Enum items of typedefs and variables declared here.
            let mut stack: Vec<DeclId> = cd.children.clone();
            if let Some(i) = cd.ty.as_ref().and_then(|t| t.inline) {
                stack.push(i);
            }
            while let Some(x) = stack.pop() {
                let xd = u.parsed.decl(x);
                match xd.kind {
                    DeclKind::EnumType => stack.extend(xd.children.iter().copied()),
                    DeclKind::EnumMember => out.push(DeclRef { unit: scope.unit, decl: x }),
                    _ => {}
                }
            }
        }
        out
    }

    fn decl_completion(&self, r: DeclRef) -> CompletionItem {
        let d = self.decl(r);
        let u = self.unit(r.unit);
        let in_class = d.parent.is_some_and(|p| matches!(u.parsed.decl(p).kind, DeclKind::Class | DeclKind::InterfaceClass));
        let mut item = CompletionItem {
            label: d.name.to_string(),
            kind: Some(completion_kind(d, in_class)),
            detail: Some(self.decl_signature(r)),
            ..Default::default()
        };
        if matches!(d.kind, DeclKind::Function | DeclKind::Task) {
            let args: Vec<&Decl> = d.children.iter().map(|c| u.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Argument).collect();
            let snippet = if args.is_empty() {
                format!("{}()", d.name)
            } else {
                let a: Vec<String> = args
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| a.detail.is_empty())
                    .map(|(i, a)| if a.name.is_empty() { format!("${{{}:arg{}}}", i + 1, i + 1) } else { format!("${{{}:{}}}", i + 1, a.name) })
                    .collect();
                format!("{}({})", d.name, a.join(", "))
            };
            item.insert_text = Some(snippet);
            item.insert_text_format = Some(InsertTextFormat::SNIPPET);
            item.label_details = Some(CompletionItemLabelDetails {
                detail: Some(format!("({})", args.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "))),
                description: d.ty.as_ref().map(|t| t.text.to_string()),
            });
        } else if let Some(t) = &d.ty {
            item.label_details = Some(CompletionItemLabelDetails { detail: None, description: Some(t.text.to_string()) });
        }
        item
    }

    fn scope_completions(&self, ctx: &Ctx, prefix: &str, file: FileId) -> Vec<CompletionItem> {
        let mut out = Vec::new();
        let mut seen: FxHashSet<SmolStr> = FxHashSet::default();
        let u = self.unit(ctx.unit);
        let mut cur = Some(ctx.scope);
        let mut design_ctx = false;
        let push = |this: &Analysis, r: DeclRef, out: &mut Vec<CompletionItem>, seen: &mut FxHashSet<SmolStr>| {
            let d = this.decl(r);
            if d.name.is_empty() || d.has(dflags::OUT_OF_BLOCK) || d.has(dflags::FROM_MACRO) && d.kind == DeclKind::Generate {
                return;
            }
            if !starts_like(&d.name, prefix) || !seen.insert(d.name.clone()) {
                return;
            }
            out.push(this.decl_completion(r));
        };
        while let Some(s) = cur {
            let d = u.parsed.decl(s);
            if matches!(d.kind, DeclKind::Module | DeclKind::Interface | DeclKind::Program) {
                design_ctx = true;
            }
            let sr = DeclRef { unit: ctx.unit, decl: s };
            let members = match d.kind {
                DeclKind::Class | DeclKind::InterfaceClass => self.members_list(sr),
                _ if d.has(dflags::OUT_OF_BLOCK) => {
                    let mut v = self.scope_members(sr);
                    if let Some(c) = self.owner_class(sr) {
                        v.extend(self.members_list(c));
                    }
                    v
                }
                _ => self.scope_members(sr),
            };
            for m in members {
                push(self, m, &mut out, &mut seen);
            }
            for imp in &d.imports {
                if let Some(p) = self.package(sr.unit, &imp.pkg) {
                    for m in self.scope_members(p) {
                        if imp.item.as_ref().map_or(true, |i| *i == self.decl(m).name) {
                            push(self, m, &mut out, &mut seen);
                        }
                    }
                }
            }
            if out.len() > MAX_ITEMS {
                return out;
            }
            cur = d.parent;
        }
        let root = u.parsed.decl(ParsedUnit::ROOT);
        let _ = root;
        // Compilation-unit items, packages, definitions: the scopes' items all stay (the exact
        // name is among them), these only while there is room (they carry signatures and
        // instance snippets).
        for v in self.global.unit_items.values() {
            if out.len() >= MAX_ITEMS {
                break;
            }
            if let Some(r) = v.first() {
                push(self, *r, &mut out, &mut seen);
            }
        }
        for v in self.global.packages.values() {
            if out.len() >= MAX_ITEMS {
                break;
            }
            if let Some(r) = v.first() {
                if self.unit(r.unit).kind != UnitKind::Builtin {
                    push(self, *r, &mut out, &mut seen);
                }
            }
        }
        for (name, v) in &self.global.definitions {
            if out.len() >= MAX_ITEMS {
                break;
            }
            let Some(r) = v.first() else { continue };
            if !starts_like(name, prefix) || !seen.insert(name.clone()) {
                continue;
            }
            let mut item = self.decl_completion(*r);
            if design_ctx && matches!(self.decl(*r).kind, DeclKind::Module | DeclKind::Interface | DeclKind::Program) {
                item.insert_text = Some(self.instance_snippet(*r));
                item.insert_text_format = Some(InsertTextFormat::SNIPPET);
                item.label_details = Some(CompletionItemLabelDetails { detail: Some(" instance".into()), description: Some(self.decl(*r).kind.label().into()) });
            }
            out.push(item);
        }
        if let Some(std) = self.global.std_pkg {
            for m in self.scope_members(std) {
                if out.len() >= MAX_ITEMS {
                    break;
                }
                push(self, m, &mut out, &mut seen);
            }
        }
        for k in KEYWORDS {
            if out.len() < MAX_ITEMS && starts_like(k, prefix) && seen.insert(SmolStr::new(k)) {
                out.push(CompletionItem { label: k.to_string(), kind: Some(CompletionItemKind::KEYWORD), ..Default::default() });
            }
        }
        let _ = file;
        out.truncate(MAX_ITEMS);
        out
    }

    /// Module instantiation template: parameters and ports connected by name.
    pub fn instance_snippet(&self, module: DeclRef) -> String {
        let u = self.unit(module.unit);
        let d = u.parsed.decl(module.decl);
        let params: Vec<&Decl> = d.children.iter().map(|c| u.parsed.decl(*c)).filter(|c| matches!(c.kind, DeclKind::Parameter | DeclKind::TypeParam)).collect();
        let ports: Vec<&Decl> = d.children.iter().map(|c| u.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Port).collect();
        let w = ports.iter().map(|p| p.name.len()).max().unwrap_or(0);
        let wp = params.iter().map(|p| p.name.len()).max().unwrap_or(0);
        let mut s = d.name.to_string();
        let mut n = 1;
        if !params.is_empty() {
            s.push_str(" #(\n");
            let v: Vec<String> = params
                .iter()
                .map(|p| {
                    let def = if p.detail.is_empty() { p.name.to_string() } else { p.detail.replace('$', "\\$").replace('}', "\\}") };
                    let line = format!("\t.{:<wp$} (${{{}:{}}})", p.name, n, def);
                    n += 1;
                    line
                })
                .collect();
            s.push_str(&v.join(",\n"));
            s.push_str("\n)");
        }
        s.push_str(&format!(" ${{{}:u_{}}}", n, d.name));
        n += 1;
        if ports.is_empty() {
            s.push_str(" ();");
        } else {
            s.push_str(" (\n");
            let v: Vec<String> = ports
                .iter()
                .map(|p| {
                    let line = format!("\t.{:<w$} (${{{}:{}}})", p.name, n, p.name);
                    n += 1;
                    line
                })
                .collect();
            s.push_str(&v.join(",\n"));
            s.push_str("\n);");
        }
        s
    }

    // ------------------------------------------------------------ signature help

    pub fn signature_help(&self, file: FileId, off: u32) -> Option<SignatureHelp> {
        let text = self.file_text(file)?;
        let start = (off as usize).saturating_sub(4000);
        let start = (start..off as usize).find(|i| text.is_char_boundary(*i)).unwrap_or(off as usize);
        let window = text.get(start..off as usize)?;
        self.signature_help_in(file, window, off)
    }

    /// [`Analysis::signature_help`] with `line` as the text of the line before the cursor (see
    /// [`Analysis::completions_with_line`]); `line_start` is where that line starts in this
    /// snapshot's text.
    pub fn signature_help_with_line(&self, file: FileId, line_start: u32, line: &str) -> Option<SignatureHelp> {
        let text = self.file_text(file)?;
        // The 4000 bytes before the cursor, as `signature_help` reads.
        let start = (line_start as usize).saturating_sub(4000);
        let start = (start..line_start as usize).find(|i| text.is_char_boundary(*i)).unwrap_or(line_start as usize);
        let window = format!("{}{line}", text.get(start..line_start as usize)?);
        let cut = window.len().saturating_sub(4000);
        let cut = (cut..window.len()).find(|i| window.is_char_boundary(*i)).unwrap_or(window.len());
        self.signature_help_in(file, &window[cut..], line_start)
    }

    /// Signature help for the text `window` that ends at the cursor, in the scope at `off`.
    fn signature_help_in(&self, file: FileId, window: &str, off: u32) -> Option<SignatureHelp> {
        let toks = lex(window, file);
        // Find the unmatched '(' and count commas at its depth.
        let mut depth = 0i32;
        let mut commas = 0u32;
        let mut open = None;
        for (i, t) in toks.iter().enumerate().rev() {
            if t.kind != TokKind::Punct {
                continue;
            }
            match t.text.as_str() {
                ")" | "]" | "}" => depth += 1,
                "(" | "[" | "{" => {
                    if depth == 0 {
                        if t.text == "(" {
                            open = Some(i);
                        }
                        break;
                    }
                    depth -= 1;
                }
                "," if depth == 0 => commas += 1,
                ";" if depth == 0 => return None,
                _ => {}
            }
        }
        let open = open?;
        if open == 0 {
            return None;
        }
        let ctx = Ctx { unit: *self.units_for(file).first()?, scope: 0, line: String::new() };
        let ctx = Ctx { scope: self.scope_at(ctx.unit, file, off), ..ctx };
        let before = &toks[..open];
        // Macro call.
        if let Some(t) = before.last() {
            if t.kind == TokKind::Directive {
                let m = self.global.macros.get(&t.text)?.first()?.clone();
                let params: Vec<String> = m.params.as_ref()?.iter().map(|p| p.name.to_string()).collect();
                return Some(make_sig(format!("`{}", m.name), params, commas, None));
            }
        }
        // Function call: resolve the chain ending right before '('.
        let mut chain_toks: Vec<dvi_syntax::Token> = before.to_vec();
        chain_toks.push(dvi_syntax::Token { kind: TokKind::Punct, text: SmolStr::new_static("."), span: Span::default(), flags: 0 });
        let (r, _) = self.chain_before(&ctx, &chain_toks)?;
        let d = self.decl(r);
        let r = if matches!(d.kind, DeclKind::Class) { self.class_member(r, "new", 0)? } else { r };
        let d = self.decl(r);
        if !matches!(d.kind, DeclKind::Function | DeclKind::Task | DeclKind::Covergroup | DeclKind::Property | DeclKind::Sequence | DeclKind::Let) {
            return None;
        }
        let u = self.unit(r.unit);
        let params: Vec<String> = d
            .children
            .iter()
            .map(|c| u.parsed.decl(*c))
            .filter(|c| c.kind == DeclKind::Argument)
            .map(|a| {
                let mut s = String::new();
                if let Some(dir) = a.dir {
                    if dir != dvi_syntax::ast::PortDir::Input {
                        s.push_str(dir.as_str());
                        s.push(' ');
                    }
                }
                if let Some(t) = &a.ty {
                    s.push_str(&t.text);
                    s.push(' ');
                }
                // Unnamed (DPI import): the type alone.
                s.push_str(&a.name);
                s.truncate(s.trim_end().len());
                if !a.detail.is_empty() {
                    s.push_str(" = ");
                    s.push_str(&a.detail);
                }
                s
            })
            .collect();
        let doc = self.doc_comment(d.name_span.file, if d.span.file == d.name_span.file { d.span.start } else { d.name_span.start });
        Some(make_sig(d.name.to_string(), params, commas, doc))
    }
}

/// Where the editor's text `new` differs from a snapshot's text `old` only on line `line` (both
/// have it), and the two versions of that line can change nothing but themselves: no keyword
/// that opens or closes a scope, no directive, no block comment, string or line continuation.
/// Returns where the line starts in `old` (the same in `new`), and the line in `old` and in
/// `new`, without its end.
///
/// What the line being typed does to the lines after it (a statement not finished yet, a type
/// whose name is on the next line) shows when the update is in, a keystroke later, as do
/// names it declares itself.
pub fn line_local_edit<'a>(old: &'a str, new: &'a str, line: u32) -> Option<(u32, &'a str, &'a str)> {
    let start = |t: &str| -> Option<usize> {
        if line == 0 {
            return Some(0);
        }
        t.match_indices('\n').nth(line as usize - 1).map(|(i, _)| i + 1)
    };
    let (os, ns) = (start(old)?, start(new)?);
    if os != ns || old.as_bytes()[..os] != new.as_bytes()[..ns] {
        return None;
    }
    let end = |t: &str, s: usize| t[s..].find('\n').map_or(t.len(), |i| s + i);
    let (oe, ne) = (end(old, os), end(new, ns));
    if old.as_bytes()[oe..] != new.as_bytes()[ne..] {
        return None;
    }
    let (lo, ln) = (&old[os..oe], &new[ns..ne]);
    // The line before must not continue into this one (`ifdef \, a `define body).
    if old[..os].trim_end_matches(['\n', '\r']).ends_with('\\') {
        return None;
    }
    let structural = |l: &str| {
        let l = l.trim_end_matches('\r');
        l.contains("/*") || l.contains("*/") || l.contains('"') || l.ends_with('\\') || l.contains('`') || {
            lex(l, 0).iter().any(|t| {
                t.kind == TokKind::Keyword
                    && matches!(
                        t.text.as_str(),
                        "begin" | "end" | "fork" | "join" | "join_any" | "join_none" | "function" | "endfunction" | "task" | "endtask"
                            | "class" | "endclass" | "module" | "endmodule" | "package" | "endpackage" | "interface" | "endinterface"
                            | "program" | "endprogram" | "generate" | "endgenerate" | "case" | "casex" | "casez" | "endcase"
                            | "randcase" | "covergroup" | "endgroup" | "property" | "endproperty" | "sequence" | "endsequence"
                            | "constraint" | "clocking" | "endclocking" | "checker" | "endchecker" | "config" | "endconfig"
                            | "specify" | "endspecify" | "primitive" | "endprimitive" | "table" | "endtable" | "foreach" | "for"
                            | "extern" | "import" | "export" | "typedef" | "let" | "randsequence"
                    )
            })
        }
    };
    if structural(lo) || structural(ln) {
        return None;
    }
    // The same keywords (an `else` gone may end a loop's statement on this line) and as many
    // statement ends: names and expressions may change, not the statement's shape.
    let shape = |l: &str| {
        let toks = lex(l, 0);
        let kws: Vec<SmolStr> = toks.iter().filter(|t| t.kind == TokKind::Keyword).map(|t| t.text.clone()).collect();
        (kws, toks.iter().filter(|t| t.is_punct(";")).count())
    };
    if shape(lo) != shape(ln) {
        return None;
    }
    // Nor may the line be inside the arguments of a macro call begun on an earlier line of its
    // statement (written through an alias too): they may open or close scopes.
    let mut from = os.saturating_sub(4096);
    while !old.is_char_boundary(from) {
        from += 1;
    }
    let back = &old.as_bytes()[from..os];
    let Some(stmt) = back.iter().rposition(|c| *c == b';') else { return None };
    let mut i = stmt;
    while let Some(k) = back[i..].iter().position(|c| *c == b'`') {
        let mut j = i + k + 1;
        while j < back.len() && (back[j].is_ascii_alphanumeric() || back[j] == b'_') {
            j += 1;
        }
        while j < back.len() && back[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < back.len() && back[j] == b'(' {
            let mut depth = 0usize;
            let close = back[j..].iter().position(|c| {
                match c {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                depth == 0
            });
            match close {
                Some(c) => j += c + 1,
                None => return None,
            }
        }
        i = j;
    }
    Some((os as u32, lo, ln))
}

impl Analysis {
    /// The scope a line of `file` (from `start` to `end`) is in, when it is a subroutine or a
    /// block that begins before the line and goes on after it, of a project unit: there an
    /// edit of that line only (see [`line_local_edit`]) changes nothing a completion sees but
    /// the line. The offset to resolve at (`start`).
    pub fn line_scope(&self, file: FileId, start: u32, end: u32) -> Option<u32> {
        let &unit = self.units_for(file).first()?;
        let u = self.unit(unit);
        if u.kind != crate::world::UnitKind::Project {
            return None;
        }
        // Inside the arguments of a macro call begun on an earlier line: what the line says is
        // read through the macro, which may open or close scopes.
        if u.macro_uses.iter().any(|m| m.call_span.file == file && m.call_span.start < start && m.call_span.end > start) {
            return None;
        }
        let scope = self.scope_at(unit, file, start);
        let d = u.parsed.decl(scope);
        let inside = scope != dvi_syntax::ast::ParsedUnit::ROOT
            && matches!(d.kind, DeclKind::Function | DeclKind::Task | DeclKind::Block)
            && d.span.file == file
            && d.span.start < start
            && d.span.end > end
            && self.scope_at(unit, file, end) == scope;
        inside.then_some(start)
    }
}

fn make_sig(name: String, params: Vec<String>, active: u32, doc: Option<String>) -> SignatureHelp {
    let mut label = format!("{name}(");
    let mut infos = Vec::new();
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let s = label.encode_utf16().count() as u32;
        label.push_str(p);
        let e = label.encode_utf16().count() as u32;
        infos.push(ParameterInformation { label: ParameterLabel::LabelOffsets([s, e]), documentation: None });
    }
    label.push(')');
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: doc.map(|d| Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value: d })),
            parameters: Some(infos),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    }
}

/// Case-insensitive prefix or camel-case/underscore abbreviation match.
pub fn starts_like(name: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let n = name.to_ascii_lowercase();
    let p = prefix.to_ascii_lowercase();
    if n.starts_with(&p) {
        return true;
    }
    // Abbreviation: initials of '_'-separated words.
    let initials: String = name.split('_').filter_map(|w| w.chars().next()).collect::<String>().to_ascii_lowercase();
    initials.starts_with(&p) || crate::ide::symbols::fuzzy(&n, &p) && p.len() >= 3
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let cut = s.char_indices().nth(n).map(|c| c.0).unwrap_or(s.len());
        format!("{}...", &s[..cut])
    }
}
