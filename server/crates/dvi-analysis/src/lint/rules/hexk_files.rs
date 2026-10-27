//! `hexk` rules about files and their structure (style guide FIL-2, FIL-3, FIL-4, FIL-6, FIL-7).

use crate::ide::stmt::Simple;
use crate::lint::facts::guarded;
use crate::lint::{Cx, Level, Rule, RuleMeta, Ruleset};
use dvi_syntax::ast::{dflags, DeclId, DeclKind, ParsedUnit};
use dvi_syntax::{Span, TokKind, Token};

macro_rules! meta {
    ($name:ident, $id:literal, [$($g:literal),*], $level:ident, $summary:literal, $why:literal) => {
        static $name: RuleMeta = RuleMeta {
            id: $id,
            guide: &[$($g),*],
            rulesets: &[Ruleset::Hexk],
            default_level: Level::$level,
            subchecks: &[],
            summary: $summary,
            why: $why,
            params: &[],
            text_only: false,
        };
    };
}

meta!(CU_IMPORT, "compilation-unit-import", ["FIL-4"], Warning, "An `import` at compilation-unit scope.", "An import outside a module, interface or package leaks into every file compiled after it, so a file's meaning depends on the compile order (FIL-4).");
meta!(CLASS_OUTSIDE, "class-outside-package", ["FIL-3"], Warning, "A class declared outside a package.", "Classes in packages have one definition the whole testbench shares; a class in a module or at compilation-unit scope is a separate type in each place (FIL-3).");
meta!(INCLUDE_GUARD, "include-guard", ["FIL-6"], Warning, "An included file has no include guard named after it.", "A guard makes a second `include harmless; naming it after the file (BUS_DRIVER_SV) avoids two files sharing one guard (FIL-6).");
meta!(DELAY_UNIT, "delay-without-unit", ["FIL-7"], Warning, "A delay literal without a time unit.", "A bare #10 means whatever the scope's time unit is; write #10ns (FIL-7).");
meta!(TIMEUNIT_FIRST, "timeunit-first", ["FIL-7"], Warning, "A scope with delays or time values does not declare timeunit and timeprecision first.", "Without them the scope's time unit comes from compile order or tool defaults (FIL-7).");
meta!(FILE_EXTENSION, "file-extension", ["FIL-2"], Warning, "A compiled file or an included file has the wrong extension.", "Compiled units and files included into packages use .sv; .svh is for macro and header files only (FIL-2).");

pub struct CompilationUnitImport;
pub struct ClassOutsidePackage;
pub struct IncludeGuard;
pub struct DelayWithoutUnit;
pub struct TimeunitFirst;
pub struct FileExtension;

impl Rule for CompilationUnitImport {
    fn meta(&self) -> &'static RuleMeta {
        &CU_IMPORT
    }
    fn check(&self, cx: &mut Cx) {
        let u = cx.a.unit(cx.unit).clone();
        let root = u.parsed.decl(ParsedUnit::ROOT);
        for i in &root.imports {
            if i.pkg_span.file == cx.file {
                cx.report(None, i.pkg_span, format!("import {}::{} at compilation-unit scope: import inside the package, module or interface that uses it", i.pkg, i.item.as_deref().unwrap_or("*")), Vec::new());
            }
        }
    }
}

impl Rule for ClassOutsidePackage {
    fn meta(&self) -> &'static RuleMeta {
        &CLASS_OUTSIDE
    }
    fn check(&self, cx: &mut Cx) {
        let u = cx.a.unit(cx.unit).clone();
        for d in u.parsed.decls.iter() {
            if !matches!(d.kind, DeclKind::Class | DeclKind::InterfaceClass) || d.name_span.file != cx.file || d.has(dflags::FROM_MACRO) {
                continue;
            }
            // The nearest enclosing package or design element.
            let mut p = d.parent;
            let mut outer = None;
            while let Some(x) = p {
                let k = u.parsed.decl(x).kind;
                if matches!(k, DeclKind::Package | DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Unit) {
                    outer = Some(k);
                    break;
                }
                p = u.parsed.decl(x).parent;
            }
            match outer {
                Some(DeclKind::Package) => {}
                // FIL-3's exception: a self-contained example or unit test whose testbench is one
                // module (the files the configuration marks as examples).
                Some(DeclKind::Module) if cx.example => {}
                Some(DeclKind::Unit) | None => cx.report(None, d.name_span, format!("Class {} is at compilation-unit scope: put it in a package", d.name), Vec::new()),
                Some(_) => cx.report(None, d.name_span, format!("Class {} is declared in a module or interface: put it in a package", d.name), Vec::new()),
            }
        }
    }
}

/// The guard name FIL-6 expects for a file: its name upper-cased, each run of other characters
/// as `_` (`bus_driver.sv` → `BUS_DRIVER_SV`).
pub fn guard_name(file_name: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in file_name.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c.to_ascii_uppercase());
        } else {
            gap = true;
        }
    }
    out
}

/// A guard name without leading and trailing underscores, runs of them as one.
fn squeeze(g: &str) -> String {
    g.split('_').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("_")
}

/// The guard of a file: its name when `ifndef G` / `define G` open the file and the `endif`
/// that closes that `ifndef` is the file's last token.
fn guard_of(t: &[Token]) -> Option<String> {
    let t: Vec<&Token> = t.iter().filter(|x| x.kind != TokKind::Eof).collect();
    if !guarded(&t.iter().map(|x| (*x).clone()).collect::<Vec<_>>()) {
        return None;
    }
    // The `endif` matching the first `ifndef`.
    let mut depth = 0;
    for (k, x) in t.iter().enumerate() {
        if x.kind != TokKind::Directive {
            continue;
        }
        match x.text.as_str() {
            "ifdef" | "ifndef" => depth += 1,
            // An `else` of the guard itself: a second include gets that branch.
            "else" | "elsif" if depth == 1 => return None,
            "endif" => {
                depth -= 1;
                if depth == 0 {
                    return (k == t.len() - 1).then(|| t[1].text.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

impl Rule for IncludeGuard {
    fn meta(&self) -> &'static RuleMeta {
        &INCLUDE_GUARD
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        // Files the unit includes (not its root).
        if u.root == cx.file || !u.includes.iter().any(|i| i.entered && i.resolved == Some(cx.file)) {
            return;
        }
        let Some(raw) = a.file_tokens(cx.file) else { return };
        let path = a.path(cx.file);
        let want = guard_name(&path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        let first = raw.iter().find(|x| x.kind != TokKind::Eof).map(|x| x.span).unwrap_or(Span::new(cx.file, 0, 0));
        match guard_of(&raw) {
            None => cx.report(None, first, format!("This included file has no include guard around all of it: `ifndef {want} / `define {want} ... `endif"), Vec::new()),
            // Named after the file, decorations aside (`__BUS_DRIVER_SV__`).
            Some(g) if squeeze(&g) != want => cx.report(None, raw.iter().filter(|x| x.kind != TokKind::Eof).nth(1).map_or(first, |x| x.span), format!("The include guard {g} is not named after the file: {want}"), Vec::new()),
            Some(_) => {}
        }
    }
}

/// Time units of a time literal.
fn unit(x: &Token) -> bool {
    x.kind == TokKind::Ident && matches!(x.text.as_str(), "s" | "ms" | "us" | "ns" | "ps" | "fs" | "step")
}

/// Whether `t[j]` is a number with a time unit, written in the number (`10ns`) or right after it.
fn timed(t: &[Token], j: usize) -> bool {
    let x = &t[j];
    if x.kind != TokKind::Number {
        return false;
    }
    let s = x.text.as_str();
    let inline = ["step", "ms", "us", "ns", "ps", "fs", "s"].iter().any(|u| s.strip_suffix(u).is_some_and(|r| r.ends_with(|c: char| c.is_ascii_digit() || c == '.')));
    inline || t.get(j + 1).is_some_and(|n| unit(n) && !n.ws_before())
}

/// Whether a number token's value is zero (`0`, `0.0`, `8'h0`, `'b00`); `None` when it cannot
/// be told (`'hx`).
pub fn zero(text: &str) -> Option<bool> {
    let digits = match text.find('\'') {
        // Sized or based: the digits after the base letter.
        Some(q) => {
            let rest = text[q + 1..].trim_start_matches(['s', 'S']);
            rest.get(1..)?
        }
        None => text,
    };
    let d: Vec<char> = digits.chars().filter(|c| *c != '_').collect();
    if d.is_empty() || d.iter().any(|c| matches!(c, 'x' | 'X' | 'z' | 'Z' | '?')) {
        return None;
    }
    Some(d.iter().all(|c| *c == '0' || *c == '.'))
}

/// Whether the delay value starting at `t[k]` (after `#`) is one of the literal forms without a
/// unit, other than 0: `10`, `(10)`, `(1:2:3)`. Any other expression (names, operators, a time
/// literal anywhere in it) is not judged.
fn unitless(t: &[Token], k: usize) -> bool {
    let num_unitless = |j: usize| t[j].kind == TokKind::Number && !timed(t, j) && zero(&t[j].text) == Some(false);
    match t.get(k) {
        Some(x) if x.kind == TokKind::Number => num_unitless(k),
        Some(x) if x.is_punct("(") => {
            // `( n )` or `( n : n : n )`, nothing else.
            let mut j = k + 1;
            let mut any = false;
            loop {
                let Some(y) = t.get(j) else { return false };
                if y.kind != TokKind::Number || timed(t, j) {
                    return false;
                }
                any |= num_unitless(j);
                match t.get(j + 1) {
                    Some(z) if z.is_punct(")") => return any,
                    Some(z) if z.is_punct(":") => j += 2,
                    _ => return false,
                }
            }
        }
        _ => false,
    }
}

impl Rule for DelayWithoutUnit {
    fn meta(&self) -> &'static RuleMeta {
        &DELAY_UNIT
    }
    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        let mut hits: Vec<Span> = Vec::new();
        for b in &facts.bodies {
            let Some(stmts) = &b.stmts else { continue };
            for s in stmts {
                match *s {
                    // A delay control before a statement.
                    Simple::Delay(k, _) => {
                        if unitless(t, k + 1) {
                            hits.push(t[k].span);
                        }
                    }
                    // An intra-assignment delay: `a = #5 b;`, `a <= #5 b;`.
                    Simple::Expr(s, e) => {
                        for j in s..e.saturating_sub(1) {
                            if (t[j].is_punct("=") || t[j].is_punct("<=")) && t[j + 1].is_punct("#") && unitless(t, j + 2) {
                                hits.push(t[j + 1].span);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // `assign #5 a = b;`.
        for k in 0..t.len().saturating_sub(2) {
            if t[k].is_kw("assign") && t[k + 1].is_punct("#") && unitless(t, k + 2) && !facts.in_define(k) {
                hits.push(t[k + 1].span);
            }
        }
        for s in hits {
            cx.report(None, s, "A delay without a time unit: write it with one (#10ns)", Vec::new());
        }
    }
}

/// Whether `t[k]` is a delay's `#` by its context (not a parameter list `name #(`).
fn delay_hash(t: &[Token], k: usize) -> bool {
    t[k].is_punct("#") && !k.checked_sub(1).is_some_and(|p| t[p].kind == TokKind::Ident || t[p].is_punct("::") || t[p].kind == TokKind::Keyword && matches!(t[p].text.as_str(), "class" | "module" | "interface" | "program" | "extends"))
}

/// Whether tokens use time: a delay (`#10`, `#(d)`, `#PERIOD`) or a literal with a unit.
fn uses_time(t: &[Token], k: usize) -> bool {
    delay_hash(t, k) && t.get(k + 1).is_some_and(|n| n.kind == TokKind::Number || n.is_punct("(") || n.kind == TokKind::Ident) || timed(t, k)
}

impl Rule for TimeunitFirst {
    fn meta(&self) -> &'static RuleMeta {
        &TIMEUNIT_FIRST
    }
    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        let u = cx.a.unit(cx.unit).clone();
        let scopes: Vec<(DeclId, Span, String)> = u
            .parsed
            .decls
            .iter()
            .enumerate()
            .filter(|(_, d)| matches!(d.kind, DeclKind::Module | DeclKind::Interface | DeclKind::Package) && d.name_span.file == cx.file && !d.has(dflags::FROM_MACRO))
            .map(|(i, d)| (i as DeclId, d.span, d.name.to_string()))
            .collect();
        for (i, span, name) in &scopes {
            let lo = t.partition_point(|x| x.span.start < span.start);
            let hi = t.partition_point(|x| x.span.start < span.end);
            // Nested design elements have their own declarations.
            let nested: Vec<Span> = scopes.iter().filter(|(j, s, _)| j != i && span.start < s.start && s.end <= span.end).map(|(_, s, _)| *s).collect();
            let own = |k: usize| !nested.iter().any(|s| s.start <= t[k].span.start && t[k].span.start < s.end) && !facts.in_define(k);
            let mut uses = (lo..hi).any(|k| own(k) && uses_time(t, k));
            // Files included inside it (class files of a package) are its code too.
            if !uses {
                let inside: Vec<usize> = u.includes.iter().enumerate().filter(|(_, i)| i.span.file == cx.file && span.start <= i.span.start && i.span.end <= span.end).map(|(n, _)| n).collect();
                let mut files: Vec<dvi_syntax::FileId> = Vec::new();
                for (n, inc) in u.includes.iter().enumerate() {
                    // Included here, or by a file included here.
                    let mut cur = Some(n);
                    let mut steps = 0;
                    while let Some(c) = cur {
                        if inside.contains(&c) {
                            if let Some(f) = inc.resolved.filter(|_| inc.entered) {
                                if !files.contains(&f) {
                                    files.push(f);
                                }
                            }
                            break;
                        }
                        steps += 1;
                        cur = if steps > 64 { None } else { u.includes.get(c).and_then(|x| x.from).map(|x| x as usize) };
                    }
                }
                uses = files.iter().any(|f| cx.a.file_tokens(*f).is_some_and(|ft| (0..ft.len()).any(|k| uses_time(&ft, k))));
            }
            if !uses {
                continue;
            }
            // The header ends at its first `;` outside brackets, after the imports a header may
            // start with (`module m import p::*; #(...) (...);`).
            let mut depth = 0i32;
            let mut j = lo;
            while j < hi && !t[j].is_kw("import") && !t[j].is_punct(";") && !t[j].is_punct("#") && !t[j].is_punct("(") {
                j += 1;
            }
            while j < hi && t[j].is_kw("import") {
                while j < hi && !t[j].is_punct(";") {
                    j += 1;
                }
                j += 1;
            }
            while j < hi {
                let x = &t[j];
                if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
                    depth += 1;
                } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
                    depth -= 1;
                } else if x.is_punct(";") && depth == 0 {
                    break;
                }
                j += 1;
            }
            // `timeunit`/`timeprecision` (either order) as the first body items.
            let mut k = j + 1;
            let (mut unit_seen, mut prec_seen) = (false, false);
            while k < hi && (t[k].is_kw("timeunit") || t[k].is_kw("timeprecision")) {
                let tu = t[k].is_kw("timeunit");
                let mut e = k;
                while e < hi && !t[e].is_punct(";") {
                    if tu && t[e].is_punct("/") {
                        prec_seen = true;
                    }
                    e += 1;
                }
                if tu {
                    unit_seen = true;
                } else {
                    prec_seen = true;
                }
                k = e + 1;
            }
            if !(unit_seen && prec_seen) {
                let decl = u.parsed.decl(*i);
                cx.report(None, decl.name_span, format!("{name} uses delays or time values: declare timeunit and timeprecision as its first items"), Vec::new());
            }
        }
    }
}

impl Rule for FileExtension {
    fn meta(&self) -> &'static RuleMeta {
        &FILE_EXTENSION
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let path = a.path(cx.file);
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let u = a.unit(cx.unit).clone();
        let at = cx.facts.tokens.first().map_or(Span::new(cx.file, 0, 0), |x| x.span);
        // Verilog files (`.v`, `.vh`) are not SystemVerilog sources: FIL-2 is about those.
        if matches!(ext.as_str(), "v" | "vh") {
            return;
        }
        if u.root == cx.file {
            if ext != "sv" {
                cx.report(None, at, format!("A compiled file named .{ext}: compiled units use .sv"), Vec::new());
            }
            return;
        }
        match ext.as_str() {
            "sv" => {}
            "svh" => {
                // A header holds macros, types and parameters, not code: no design elements,
                // classes, subroutines, variables or nets (outside type declarations), processes or
                // continuous assignments.
                let in_type = |d: &dvi_syntax::ast::Decl| d.parent.is_some_and(|p| matches!(u.parsed.decl(p).kind, DeclKind::StructType | DeclKind::EnumType | DeclKind::Typedef));
                let code = u.parsed.decls.iter().any(|d| {
                    d.name_span.file == cx.file
                        && !d.has(dflags::FROM_MACRO)
                        && (matches!(d.kind, DeclKind::Class | DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Package | DeclKind::Function | DeclKind::Task | DeclKind::Covergroup)
                            || matches!(d.kind, DeclKind::Variable | DeclKind::Net) && !in_type(d))
                }) || u.parsed.processes.iter().any(|(_, f, _)| f.file == cx.file)
                    || u.parsed.accesses.iter().any(|x| x.kind == dvi_syntax::ast::AccessKind::Continuous && x.start.file == cx.file);
                if code {
                    cx.report(None, at, "This .svh file holds code (classes, functions...): name it .sv (.svh is for macro and header files)", Vec::new());
                }
            }
            _ => cx.report(None, at, format!("An included file named .{ext}: use .sv (or .svh for macro and header files)"), Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_names() {
        assert_eq!(guard_name("bus_driver.sv"), "BUS_DRIVER_SV");
        assert_eq!(guard_name("a--b.c.svh"), "A_B_C_SVH");
    }

    fn lex(s: &str) -> Vec<Token> {
        dvi_syntax::lexer::lex(s, 0).into_iter().filter(|t| t.kind != TokKind::Eof).collect()
    }

    #[test]
    fn guards_must_enclose_everything() {
        assert_eq!(guard_of(&lex("`ifndef A_SV\n`define A_SV\nclass a; endclass\n`ifdef X\n`endif\n`endif\n")), Some("A_SV".into()));
        assert_eq!(guard_of(&lex("`ifndef A_SV\n`define A_SV\n`endif\nclass a; endclass\n`ifdef X\n`endif\n")), None);
        assert_eq!(guard_of(&lex("class a; endclass\n")), None);
        assert_eq!(guard_of(&lex("`ifndef H\n`define H\ntypedef int a;\n`else\ntypedef int b;\n`endif\n")), None);
    }

    #[test]
    fn unitless_delays() {
        let t = lex("#10 #10ns #(5) #(1:2:3) #(d) #0 #(2.5ns) #1step #(2 * 1ns) #('hA) #(8'h0) #(2 * 5)");
        let hashes: Vec<usize> = t.iter().enumerate().filter(|(_, x)| x.is_punct("#")).map(|(k, _)| k).collect();
        let got: Vec<bool> = hashes.iter().map(|k| unitless(&t, k + 1)).collect();
        assert_eq!(got, [true, false, true, true, false, false, false, false, false, true, false, false]);
        assert_eq!((zero("0"), zero("8'h0"), zero("'hA"), zero("'hx"), zero("0.0")), (Some(true), Some(true), Some(false), None, Some(true)));
    }
}
