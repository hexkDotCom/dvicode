//! Semantic highlighting and folding.

use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::{dflags, Decl, DeclKind};
use dvi_syntax::{FileId, Span};
use lsp_types::{FoldingRange, FoldingRangeKind, SemanticToken, SemanticTokenModifier, SemanticTokenType};

pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::NAMESPACE,      // 0 package
    SemanticTokenType::TYPE,           // 1 typedef, type parameter
    SemanticTokenType::CLASS,          // 2 class, covergroup
    SemanticTokenType::ENUM,           // 3
    SemanticTokenType::INTERFACE,      // 4 interface, modport
    SemanticTokenType::STRUCT,         // 5
    SemanticTokenType::TYPE_PARAMETER, // 6
    SemanticTokenType::PARAMETER,      // 7 ports and arguments
    SemanticTokenType::VARIABLE,       // 8
    SemanticTokenType::PROPERTY,       // 9 class members
    SemanticTokenType::ENUM_MEMBER,    // 10
    SemanticTokenType::FUNCTION,       // 11
    SemanticTokenType::METHOD,         // 12
    SemanticTokenType::MACRO,          // 13
    SemanticTokenType::COMMENT,        // 14 inactive code
    SemanticTokenType::new("module"),  // 15 module, program, checker, instance types
    SemanticTokenType::EVENT,          // 16 property, sequence, constraint
];

pub const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION, // 1
    SemanticTokenModifier::READONLY,    // 2 parameters, constants
    SemanticTokenModifier::STATIC,      // 4
    SemanticTokenModifier::new("port"), // 8
];

const MOD_DECL: u32 = 1;
const MOD_READONLY: u32 = 2;
const MOD_STATIC: u32 = 4;
const MOD_PORT: u32 = 8;

fn classify(a: &Analysis, r: DeclRef) -> Option<(u32, u32)> {
    let d: &Decl = a.decl(r);
    let in_class = d.parent.is_some_and(|p| matches!(a.decl(DeclRef { unit: r.unit, decl: p }).kind, DeclKind::Class | DeclKind::InterfaceClass));
    let st = if d.has(dflags::STATIC) { MOD_STATIC } else { 0 };
    Some(match d.kind {
        DeclKind::Package => (0, 0),
        DeclKind::Typedef => {
            let t = d.ty.as_ref().map(|t| t.text.as_str()).unwrap_or("");
            if t.starts_with("enum") {
                (3, 0)
            } else if t.starts_with("struct") || t.starts_with("union") {
                (5, 0)
            } else if d.has(dflags::FORWARD) {
                (2, 0)
            } else {
                (1, 0)
            }
        }
        DeclKind::TypeParam => (6, 0),
        DeclKind::Class | DeclKind::Covergroup => (2, 0),
        DeclKind::InterfaceClass | DeclKind::Interface | DeclKind::Modport => (4, 0),
        DeclKind::StructType => (5, 0),
        DeclKind::EnumType => (3, 0),
        DeclKind::Port => (7, MOD_PORT),
        DeclKind::Argument => (7, 0),
        DeclKind::Parameter | DeclKind::LocalParam => (8, MOD_READONLY),
        DeclKind::Variable if in_class => (9, st | if d.has(dflags::CONST) { MOD_READONLY } else { 0 }),
        DeclKind::Field => (9, 0),
        DeclKind::Variable | DeclKind::Net | DeclKind::Genvar | DeclKind::Instance | DeclKind::Let => (8, if d.has(dflags::CONST) { MOD_READONLY } else { 0 }),
        DeclKind::EnumMember => (10, MOD_READONLY),
        DeclKind::Function | DeclKind::Task => {
            let method = in_class || d.has(dflags::OUT_OF_BLOCK);
            (if method { 12 } else { 11 }, st)
        }
        DeclKind::Module | DeclKind::Program | DeclKind::Checker | DeclKind::Primitive => (15, 0),
        DeclKind::Property | DeclKind::Sequence | DeclKind::Constraint => (16, 0),
        DeclKind::Coverpoint | DeclKind::Cross | DeclKind::ClockingBlock => (9, 0),
        _ => return None,
    })
}

impl Analysis {
    /// Semantic tokens of a file, already delta-encoded.
    pub fn semantic_tokens(&self, file: FileId) -> Vec<SemanticToken> {
        let Some(text) = self.file_text(file) else { return Vec::new() };
        let Some(li) = self.file_lines(file) else { return Vec::new() };
        let mut raw: Vec<(u32, u32, u32, u32, u32)> = Vec::new(); // line, col, len, type, mods
        let push_span = |span: Span, ty: u32, m: u32, raw: &mut Vec<(u32, u32, u32, u32, u32)>| {
            let (l1, c1) = li.position(&text, span.start);
            let (l2, c2) = li.position(&text, span.end);
            if l1 == l2 {
                if c2 > c1 {
                    raw.push((l1, c1, c2 - c1, ty, m));
                }
            } else {
                // Multi-line: one token per line.
                for l in l1..=l2 {
                    let ls = li.line_start(l);
                    let le = if l + 1 < li.line_count() { li.line_start(l + 1).saturating_sub(1) } else { text.len() as u32 };
                    let s = if l == l1 { span.start } else { ls };
                    let e = if l == l2 { span.end } else { le };
                    if e > s {
                        let (_, cs) = li.position(&text, s);
                        let (_, ce) = li.position(&text, e);
                        if ce > cs {
                            raw.push((l, cs, ce - cs, ty, m));
                        }
                    }
                }
            }
        };
        let Some(&uid) = self.units_for(file).first() else { return Vec::new() };
        let u = self.unit(uid);
        if let Some(pos) = u.positions.get(&file) {
            for did in &pos.decls {
                let r = DeclRef { unit: uid, decl: *did };
                if let Some((t, m)) = classify(self, r) {
                    push_span(u.parsed.decl(*did).name_span, t, m | MOD_DECL, &mut raw);
                }
            }
            for (span, rid, seg) in &pos.refs {
                if let Some(r) = self.resolve_ref(uid, *rid, *seg as usize) {
                    if let Some((t, m)) = classify(self, r) {
                        push_span(*span, t, m, &mut raw);
                    }
                }
            }
        }
        for m in &u.macro_uses {
            if m.name_span.file == file {
                push_span(m.name_span, 13, 0, &mut raw);
            }
        }
        for m in &u.macro_defs {
            if m.name_span.file == file {
                push_span(m.name_span, 13, MOD_DECL, &mut raw);
            }
        }
        for s in &u.inactive {
            if s.file == file {
                push_span(*s, 14, 0, &mut raw);
            }
        }
        raw.sort();
        raw.dedup_by(|b, a| a.0 == b.0 && a.1 == b.1);
        // Drop overlaps (inactive regions win over nothing else since they contain no refs).
        let mut out = Vec::with_capacity(raw.len());
        let (mut pl, mut pc, mut pend) = (0u32, 0u32, 0u32);
        let mut first = true;
        for (l, c, len, t, m) in raw {
            if !first && l == pl && c < pend {
                continue;
            }
            let dl = if first { l } else { l - pl };
            let dc = if first || dl > 0 { c } else { c - pc };
            out.push(SemanticToken { delta_line: dl, delta_start: dc, length: len, token_type: t, token_modifiers_bitset: m });
            pl = l;
            pc = c;
            pend = c + len;
            first = false;
        }
        out
    }

    pub fn folding_ranges(&self, file: FileId) -> Vec<FoldingRange> {
        let Some(text) = self.file_text(file) else { return Vec::new() };
        let Some(li) = self.file_lines(file) else { return Vec::new() };
        let mut out = Vec::new();
        let add = |s: u32, e: u32, kind: Option<FoldingRangeKind>, out: &mut Vec<FoldingRange>| {
            let (l1, _) = li.position(&text, s);
            let (l2, _) = li.position(&text, e);
            if l2 > l1 {
                out.push(FoldingRange { start_line: l1, start_character: None, end_line: l2, end_character: None, kind, collapsed_text: None });
            }
        };
        for uid in self.units_for(file).into_iter().take(1) {
            let u = self.unit(uid);
            for d in &u.parsed.decls {
                if d.span.file != file || d.has(dflags::FROM_MACRO) && d.kind != DeclKind::Block {
                    continue;
                }
                if d.kind.is_scope() && d.kind != DeclKind::Unit || matches!(d.kind, DeclKind::Typedef) {
                    add(d.span.start, d.span.end, None, &mut out);
                }
            }
            for s in &u.inactive {
                if s.file == file {
                    add(s.start, s.end, None, &mut out);
                }
            }
        }
        // Comment blocks and `// #region` markers.
        let mut block_start: Option<u32> = None;
        let mut prev_line = 0u32;
        let mut regions: Vec<u32> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            let i = i as u32;
            if t.starts_with("//") {
                let c = t.trim_start_matches('/').trim();
                if c.starts_with("#region") || c.starts_with("region") {
                    regions.push(i);
                } else if c.starts_with("#endregion") || c.starts_with("endregion") {
                    if let Some(s) = regions.pop() {
                        out.push(FoldingRange { start_line: s, start_character: None, end_line: i, end_character: None, kind: Some(FoldingRangeKind::Region), collapsed_text: None });
                    }
                }
                if block_start.is_none() {
                    block_start = Some(i);
                }
                prev_line = i;
            } else {
                if let Some(s) = block_start.take() {
                    if prev_line > s {
                        out.push(FoldingRange { start_line: s, start_character: None, end_line: prev_line, end_character: None, kind: Some(FoldingRangeKind::Comment), collapsed_text: None });
                    }
                }
            }
        }
        out.sort_by_key(|f| (f.start_line, std::cmp::Reverse(f.end_line)));
        out.dedup_by(|b, a| a.start_line == b.start_line);
        out
    }
}
