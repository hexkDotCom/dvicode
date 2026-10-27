//! `uvm-override-signature`: a UVM phase method overridden with a different signature.

use crate::lint::catalog;
use crate::lint::facts::UvmRole;
use crate::lint::{Cx, Level, Rule, RuleMeta, Ruleset};
use crate::world::DeclRef;
use dvi_syntax::ast::{dflags, DeclId, DeclKind};
use dvi_syntax::{TokKind, Token};

pub struct OverrideSignature;

static META: RuleMeta = RuleMeta {
    id: "uvm-override-signature",
    guide: &[],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &["kind", "arguments", "return"],
    summary: "A UVM phase method is overridden with another signature.",
    why: "A phase method only runs when it overrides UVM's own declaration exactly: a task where UVM declares a function, or other arguments, is either refused by the compiler late or never called.",
    params: &[],
    text_only: false,
};

impl Rule for OverrideSignature {
    fn meta(&self) -> &'static RuleMeta {
        &META
    }

    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let facts = cx.facts.clone();
        let u = a.unit(cx.unit).clone();
        if facts.repeated {
            if u.parsed.decls.iter().any(|d| d.name_span.file == cx.file && catalog::phase(&d.name).is_some()) {
                cx.unknown("repeated inclusion");
            }
            return;
        }
        for (i, d) in u.parsed.decls.iter().enumerate() {
            if !matches!(d.kind, DeclKind::Function | DeclKind::Task) || d.name_span.file != cx.file || d.has(dflags::OUT_OF_BLOCK) || !cx.step(1) {
                continue;
            }
            if catalog::phase(&d.name).is_none() {
                continue;
            }
            let Some(p) = d.parent.filter(|p| u.parsed.decl(*p).kind == DeclKind::Class) else { continue };
            let class = DeclRef { unit: cx.unit, decl: p };
            match a.lint_facts.class(a, class).role {
                Ok(UvmRole::Component) => {}
                Ok(_) => continue,
                Err(_) => {
                    cx.unknown_at("class ancestry", d.name_span);
                    continue;
                }
            }
            if d.has(dflags::FROM_MACRO) || crate::lint::facts::in_macro_call(a, cx.unit, d.name_span) {
                cx.unknown_at("macro", d.name_span);
                continue;
            }
            // The UVM declaration it overrides, through the project's own overrides between.
            let uvm_origin = || {
                let mut cur = a.class_base(class)?;
                for _ in 0..64 {
                    let m = a.class_member(cur, &d.name, 0)?;
                    let owner = crate::lint::facts::member_class(a, m)?;
                    if a.in_uvm_library(owner) {
                        return Some(m);
                    }
                    cur = a.class_base(owner)?;
                }
                None
            };
            let Some(theirs) = uvm_origin() else { continue };
            let mine = DeclRef { unit: cx.unit, decl: i as DeclId };
            let td = a.decl(theirs);
            let related = vec![(td.name_span, format!("UVM declares {} {}", if td.kind == DeclKind::Task { "task" } else { "function" }, td.name))];
            if td.kind != d.kind {
                let (m, t) = if d.kind == DeclKind::Task { ("a task", "a function") } else { ("a function", "a task") };
                cx.report(Some("kind"), d.name_span, format!("{} is {m} here but {t} in UVM: it does not override UVM's {}, so UVM never calls it", d.name, d.name), related);
                continue;
            }
            let (Some(h1), Some(h2)) = (header(a, &facts.tokens, cx.file, mine), header(a, &[], cx.file, theirs)) else {
                cx.unknown("method header");
                continue;
            };
            let same_type = |x: &Formal, y: &Formal, xu: crate::world::UnitId, yu: crate::world::UnitId| -> Option<bool> {
                // By declaration where the types are names; by text where they are built in.
                let resolve = |u, s: Option<dvi_syntax::Span>| s.and_then(|s| a.resolve_at_span(u, s)).map(|r| if a.decl(r).kind == DeclKind::Typedef { a.follow_typedef(r).unwrap_or(r) } else { r });
                match (x.name_at, y.name_at) {
                    (None, None) => Some(x.ty == y.ty && x.dims == y.dims),
                    _ => match (resolve(xu, x.name_at), resolve(yu, y.name_at)) {
                        (Some(p), Some(q)) => Some(a.decl_key(a.canonical(p)) == a.decl_key(a.canonical(q)) && x.dims == y.dims),
                        // A built-in type (`int`) is never a class.
                        (Some(p), None) if y.name_at.is_none() && a.decl(p).kind == DeclKind::Class => Some(false),
                        (None, Some(q)) if x.name_at.is_none() && a.decl(q).kind == DeclKind::Class => Some(false),
                        _ if x.ty == y.ty && x.dims == y.dims => Some(true),
                        _ => None,
                    },
                }
            };
            let mut differ = h1.formals.len() != h2.formals.len();
            let mut unsure = false;
            for (x, y) in h1.formals.iter().zip(&h2.formals) {
                if x.dir != y.dir {
                    differ = true;
                }
                match same_type(x, y, cx.unit, theirs.unit) {
                    Some(false) => differ = true,
                    None => unsure = true,
                    Some(true) => {}
                }
            }
            if !differ && unsure {
                cx.unknown_at("argument type", d.name_span);
                continue;
            }
            if differ {
                let show = |h: &Header| h.formals.iter().map(|f| format!("{} {}{}", f.dir, f.ty, if f.dims.is_empty() { String::new() } else { format!(" {}", f.dims) })).collect::<Vec<_>>().join(", ");
                cx.report(Some("arguments"), d.name_span, format!("{} takes ({}) here but ({}) in UVM: an override must take the same arguments", d.name, show(&h1), show(&h2)), related);
                continue;
            }
            if d.kind == DeclKind::Function && h1.ret != h2.ret {
                cx.report(Some("return"), d.name_span, format!("{} returns {} here but {} in UVM", d.name, if h1.ret.is_empty() { "logic" } else { &h1.ret }, h2.ret), related);
            }
        }
    }
}

/// A method header: its formals and return type.
struct Header {
    formals: Vec<Formal>,
    ret: String,
}

/// A formal argument: its direction, its type as written (normalized, unpacked dimensions
/// after it), where the type's name is (`None` for a built-in type), and its unpacked
/// dimensions.
#[derive(Clone, Debug, PartialEq)]
struct Formal {
    dir: String,
    ty: String,
    name_at: Option<dvi_syntax::Span>,
    dims: String,
}

/// Reads the header of `m` from `own` (the tokens of `file`) or from its file's tokens.
fn header(a: &crate::world::Analysis, own: &[Token], file: dvi_syntax::FileId, m: DeclRef) -> Option<Header> {
    let d = a.decl(m);
    let toks;
    let t: &[Token] = if d.name_span.file == file && !own.is_empty() {
        own
    } else {
        toks = a.file_tokens(d.name_span.file)?;
        &toks
    };
    let k = t.partition_point(|x| x.span.start < d.name_span.start);
    if t.get(k)?.span.start != d.name_span.start {
        return None;
    }
    // The return type: between the keyword and the name, qualifiers left out.
    let kw = (k.saturating_sub(16)..k).rev().find(|&j| t[j].is_kw("function") || t[j].is_kw("task"))?;
    let ret: Vec<&str> = t[kw + 1..k].iter().filter(|x| !matches!(x.text.as_str(), "virtual" | "static" | "automatic" | "protected" | "local" | "extern" | "pure")).map(|x| x.text.as_str()).collect();
    let ret = normalize(&ret.join(" "));
    let mut formals = Vec::new();
    if t.get(k + 1).is_some_and(|x| x.is_punct("(")) {
        let mut depth = 0i32;
        let mut cur: Vec<&Token> = Vec::new();
        let mut prev = Formal { dir: "input".into(), ty: "logic".into(), name_at: None, dims: String::new() };
        for x in &t[k + 1..] {
            if x.kind == TokKind::Directive {
                return None;
            }
            let mut end = false;
            if x.kind == TokKind::Punct {
                match x.text.as_str() {
                    "(" | "[" | "{" => {
                        depth += 1;
                        if depth == 1 {
                            continue;
                        }
                    }
                    ")" | "]" | "}" => {
                        depth -= 1;
                        if depth == 0 {
                            end = true;
                        }
                    }
                    "," if depth == 1 => {
                        prev = formal(&cur, &prev)?;
                        formals.push(prev.clone());
                        cur.clear();
                        continue;
                    }
                    _ => {}
                }
            }
            if end {
                if !cur.is_empty() {
                    formals.push(formal(&cur, &prev)?);
                }
                return Some(Header { formals, ret });
            }
            cur.push(x);
        }
        return None;
    }
    Some(Header { formals, ret })
}

/// One formal (its direction and type inherited from `prev` when not written).
fn formal(t: &[&Token], prev: &Formal) -> Option<Formal> {
    // Without the default value; the unpacked dimensions after the name kept apart.
    let t: Vec<&Token> = t.iter().take_while(|x| !x.is_punct("=")).copied().collect();
    let mut end = t.len();
    while end > 0 && t[end - 1].is_punct("]") {
        let mut depth = 0;
        let mut j = end;
        while j > 0 {
            j -= 1;
            if t[j].is_punct("]") {
                depth += 1;
            } else if t[j].is_punct("[") {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
        end = j;
    }
    let name = end.checked_sub(1)?;
    if t[name].kind != TokKind::Ident {
        return None;
    }
    let dims = t[end..].iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join(" ");
    let mut f = Formal { dims, ..prev.clone() };
    let mut s = 0;
    if t.first().is_some_and(|x| matches!(x.text.as_str(), "input" | "output" | "inout" | "ref" | "const")) {
        let mut d = Vec::new();
        while s < name && matches!(t[s].text.as_str(), "input" | "output" | "inout" | "ref" | "const") {
            d.push(t[s].text.as_str());
            s += 1;
        }
        f.dir = d.join(" ");
    }
    if s < name {
        let words: Vec<&str> = t[s..name].iter().map(|x| x.text.as_str()).collect();
        f.ty = normalize(&words.join(" "));
        // The type's own name: the last name before the formal's (not a parameter's).
        f.name_at = t[s..name].iter().rev().find(|x| x.kind == TokKind::Ident).map(|x| x.span).filter(|_| !t[s..name].iter().any(|x| x.is_punct("#")));
    }
    Some(f)
}

/// A type as written, comparable: `uvm_pkg::` left out, one space between words.
fn normalize(s: &str) -> String {
    s.replace("uvm_pkg :: ", "").replace("uvm_pkg::", "").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formals_of(s: &str) -> Vec<String> {
        let t: Vec<Token> = dvi_syntax::lexer::lex(s, 0).into_iter().filter(|t| t.kind != TokKind::Eof).collect();
        let refs: Vec<&Token> = t.iter().collect();
        let mut out = Vec::new();
        let mut prev = Formal { dir: "input".into(), ty: "logic".into(), name_at: None, dims: String::new() };
        for part in refs.split(|x| x.is_punct(",")) {
            prev = formal(part, &prev).unwrap();
            out.push(format!("{} {}{}", prev.dir, prev.ty, if prev.dims.is_empty() { String::new() } else { format!(" {}", prev.dims) }));
        }
        out
    }

    #[test]
    fn formals_inherit_direction_and_type() {
        assert_eq!(formals_of("uvm_phase phase"), ["input uvm_phase"]);
        assert_eq!(formals_of("input int a, b, output bit [3:0] c[2], d = 1"), ["input int", "input int", "output bit [ 3 : 0 ] [ 2 ]", "output bit [ 3 : 0 ]"]);
        assert_eq!(formals_of("uvm_pkg::uvm_phase phase"), ["input uvm_phase"]);
        assert_eq!(formals_of("uvm_phase phase[]"), ["input uvm_phase [ ]"]);
    }
}
