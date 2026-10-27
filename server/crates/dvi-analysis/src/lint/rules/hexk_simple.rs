//! `hexk` rules that need one fact each: a declaration's kind or flag, a macro's identity, a
//! system call's name, a statement's shape.

use crate::ide::stmt::Simple;
use crate::lint::facts::{member_class, UvmRole};
use crate::lint::waivers::WaiverKind;
use crate::lint::{Cx, Fix, Level, Rule, RuleMeta, Ruleset};
use crate::world::DeclRef;
use dvi_syntax::ast::{dflags, DeclId, DeclKind};
use dvi_syntax::{Span, TokKind};

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

meta!(PROGRAM, "program-block", ["IFC-2"], Warning, "A `program` block.", "Program blocks change scheduling in ways that hide races instead of fixing them; the style guide does not use them (IFC-2).");
meta!(TIMESCALE, "timescale", ["FIL-7"], Warning, "A `` `timescale `` directive.", "`` `timescale `` leaks into every file compiled after it, so a file's time unit depends on compile order; declare `timeunit` and `timeprecision` in the package, module or interface instead (FIL-7).");
meta!(EXTERN, "extern-method", ["CLS-2"], Warning, "A class method declared `extern`.", "Two copies of every signature cost more than they give; define methods in the class body (CLS-2).");
meta!(ILLEGAL_BINS, "illegal-bins", ["COV-6"], Warning, "`illegal_bins` in a covergroup.", "Illegal values are errors that assertions or checkers must report; coverage only samples. Exclude values with `ignore_bins` (COV-6).");
meta!(UVM_TRANSACTION, "extends-uvm-transaction", ["TRN-1"], Warning, "A class extends `uvm_transaction`.", "Sequence items extend `uvm_sequence_item`, which sequences and sequencers expect (TRN-1).");
meta!(FIELD_MACROS, "field-macros-in-component", ["FAC-4"], Warning, "A `` `uvm_field_* `` macro in a component.", "Field macros in a component trigger implicit config-db lookups (apply_config_settings); configuration is explicit in the style guide (FAC-4).");
meta!(UVM_DO, "uvm-do-macros", ["FAC-7"], Warning, "A `` `uvm_do ``, `` `uvm_create ``, `` `uvm_send `` or `` `uvm_rand_send `` macro.", "These macros hide create, start_item, randomize and finish_item, and their randomization results; write the steps explicitly (FAC-7).");
meta!(P_SEQUENCER, "p-sequencer", ["FAC-8"], Warning, "`` `uvm_declare_p_sequencer ``.", "It locks a sequence to one sequencer type (FAC-8); see SEQ-8 for alternatives.");
meta!(CAST_AS_TASK, "cast-as-task", ["LNG-20"], Warning, "`$cast` called as a task.", "A failed task-form `$cast` is only a run-time error the simulator may continue past with the destination unchanged; call it as a function and report failure (LNG-20).");
meta!(VOID_RANDOMIZE, "randomize-void-cast", ["LNG-10"], Warning, "The result of randomize() is discarded with void'(...).", "The style guide asks for every randomize() result to be checked (LNG-10); `void'` only says the result is ignored on purpose.");
meta!(DISPLAY, "display-in-uvm-class", ["MSG-1"], Warning, "`$display` (or `$write`, `$monitor`, `$strobe`) in a UVM class.", "UVM classes report with the UVM report macros, which the test can filter, count and redirect; `$display` bypasses all of that (MSG-1).");
meta!(SUPPRESSION, "suppression-not-waiver", [], Info, "A `dvicode-lint:` suppression.", "Suppressions turn rules off for a region; the style guide's waiver record is a `// <ID> waived: <reason>` comment on the code it covers (§0).");

pub struct ProgramBlock;
pub struct Timescale;
pub struct ExternMethod;
pub struct IllegalBins;
pub struct ExtendsUvmTransaction;
pub struct FieldMacrosInComponent;
pub struct UvmDoMacros;
pub struct PSequencer;
pub struct CastAsTask;
pub struct RandomizeVoidCast;
pub struct DisplayInUvmClass;
pub struct SuppressionNotWaiver;

/// The declarations of the unit written in the file being linted.
fn decls_here<'u>(cx: &Cx, u: &'u crate::world::Unit) -> impl Iterator<Item = (DeclId, &'u dvi_syntax::ast::Decl)> + 'u {
    let file = cx.file;
    u.parsed.decls.iter().enumerate().filter(move |(_, d)| d.name_span.file == file && !d.has(dflags::FROM_MACRO)).map(|(i, d)| (i as DeclId, d))
}

impl Rule for ProgramBlock {
    fn meta(&self) -> &'static RuleMeta {
        &PROGRAM
    }
    fn check(&self, cx: &mut Cx) {
        let u = cx.a.unit(cx.unit).clone();
        let hits: Vec<Span> = decls_here(cx, &u).filter(|(_, d)| d.kind == DeclKind::Program).map(|(_, d)| d.name_span).collect();
        for s in hits {
            cx.report(None, s, "A `program` block: use a module (IFC-2)", Vec::new());
        }
    }
}

impl Rule for Timescale {
    fn meta(&self) -> &'static RuleMeta {
        &TIMESCALE
    }
    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        for x in facts.tokens.iter().filter(|x| x.kind == TokKind::Directive && x.text == "timescale") {
            cx.report(None, x.span, "`timescale: declare timeunit and timeprecision in the package, module or interface instead", Vec::new());
        }
    }
}

impl Rule for ExternMethod {
    fn meta(&self) -> &'static RuleMeta {
        &EXTERN
    }
    fn check(&self, cx: &mut Cx) {
        let u = cx.a.unit(cx.unit).clone();
        let hits: Vec<(Span, String)> = decls_here(cx, &u)
            .filter(|(_, d)| matches!(d.kind, DeclKind::Function | DeclKind::Task) && d.has(dflags::EXTERN))
            .filter(|(_, d)| d.parent.is_some_and(|p| u.parsed.decl(p).kind == DeclKind::Class))
            .map(|(_, d)| (d.name_span, d.name.to_string()))
            .collect();
        for (s, n) in hits {
            cx.report(None, s, format!("{n} is declared extern: define it in the class body"), Vec::new());
        }
    }
}

impl Rule for IllegalBins {
    fn meta(&self) -> &'static RuleMeta {
        &ILLEGAL_BINS
    }
    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        for (k, x) in facts.tokens.iter().enumerate() {
            if x.is_kw("illegal_bins") && !facts.in_define(k) {
                cx.report(None, x.span, "illegal_bins: exclude the values with ignore_bins and check them with an assertion or a checker", Vec::new());
            }
        }
    }
}

impl Rule for ExtendsUvmTransaction {
    fn meta(&self) -> &'static RuleMeta {
        &UVM_TRANSACTION
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        let classes: Vec<DeclId> = decls_here(cx, &u).filter(|(_, d)| d.kind == DeclKind::Class && d.extends.is_some()).map(|(i, _)| i).collect();
        for c in classes {
            let class = DeclRef { unit: cx.unit, decl: c };
            let Some(base) = a.class_base(class) else { continue };
            if a.decl(base).name == "uvm_transaction" && a.in_uvm_library(base) {
                cx.report(None, a.decl(class).name_span, format!("{} extends uvm_transaction: extend uvm_sequence_item", a.decl(class).name), Vec::new());
            }
        }
    }
}

/// UVM library macro calls written in the file, with the innermost class around each.
fn library_calls<'u>(cx: &Cx, u: &'u crate::world::Unit, want: &dyn Fn(&str) -> bool) -> Vec<(&'u dvi_syntax::preproc::MacroUse, Option<DeclRef>)> {
    let a = cx.a;
    let uvm = a.lint_facts.uvm_files(a);
    let mut classes: Vec<DeclId> = u.parsed.decls.iter().enumerate().filter(|(_, d)| d.kind == DeclKind::Class && d.span.file == cx.file).map(|(i, _)| i as DeclId).collect();
    classes.sort_by_key(|c| std::cmp::Reverse(u.parsed.decl(*c).span.end - u.parsed.decl(*c).span.start));
    u.macro_uses
        .iter()
        .filter(|m| m.name_span.file == cx.file && want(&m.name) && m.def.as_ref().is_some_and(|d| uvm.contains(&d.name_span.file)))
        .map(|m| {
            let c = classes.iter().rev().find(|c| {
                let d = u.parsed.decl(**c);
                d.span.start <= m.name_span.start && m.name_span.end <= d.span.end
            });
            (m, c.map(|c| DeclRef { unit: cx.unit, decl: *c }))
        })
        .collect()
}

impl Rule for FieldMacrosInComponent {
    fn meta(&self) -> &'static RuleMeta {
        &FIELD_MACROS
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        for (m, class) in library_calls(cx, &u, &|n| n.starts_with("uvm_field_")) {
            let Some(class) = class else { continue };
            match a.lint_facts.class(a, class).role {
                Ok(UvmRole::Component) => cx.report(None, m.call_span, format!("`{} in component {}: field macros make components read the config db implicitly", m.name, a.decl(class).name), Vec::new()),
                Ok(_) => {}
                Err(_) => cx.unknown_at("class ancestry", m.call_span),
            }
        }
    }
}

impl Rule for UvmDoMacros {
    fn meta(&self) -> &'static RuleMeta {
        &UVM_DO
    }
    fn check(&self, cx: &mut Cx) {
        let u = cx.a.unit(cx.unit).clone();
        let fam = |n: &str| ["uvm_do", "uvm_create", "uvm_send", "uvm_rand_send"].iter().any(|p| n == *p || n.strip_prefix(p).is_some_and(|r| r.starts_with('_')));
        for (m, _) in library_calls(cx, &u, &fam) {
            cx.report(None, m.call_span, format!("`{}: write create, start_item, randomize (checked) and finish_item explicitly", m.name), Vec::new());
        }
    }
}

impl Rule for PSequencer {
    fn meta(&self) -> &'static RuleMeta {
        &P_SEQUENCER
    }
    fn check(&self, cx: &mut Cx) {
        let u = cx.a.unit(cx.unit).clone();
        for (m, _) in library_calls(cx, &u, &|n| n == "uvm_declare_p_sequencer") {
            cx.report(None, m.call_span, "`uvm_declare_p_sequencer ties the sequence to one sequencer type", Vec::new());
        }
    }
}

impl Rule for CastAsTask {
    fn meta(&self) -> &'static RuleMeta {
        &CAST_AS_TASK
    }
    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        for b in &facts.bodies {
            let Some(stmts) = &b.stmts else { continue };
            for s in stmts {
                let Simple::Expr(s, e) = *s else { continue };
                // Exactly `$cast(...)`.
                if !(t[s].kind == TokKind::SysIdent && t[s].text == "$cast" && t.get(s + 1).is_some_and(|x| x.is_punct("(")) && t[e - 1].is_punct(")")) {
                    continue;
                }
                let mut depth = 0i32;
                let closes_at_end = t[s + 1..e].iter().enumerate().all(|(i, x)| {
                    if x.is_punct("(") {
                        depth += 1;
                    } else if x.is_punct(")") {
                        depth -= 1;
                    }
                    depth > 0 || i == e - s - 2
                });
                if !closes_at_end {
                    continue;
                }
                let span = Span::new(cx.file, t[s].span.start, t[e].span.end);
                let fix = cast_fix(cx, span);
                cx.report_fix(None, span, "$cast called as a task: call it as a function and report a failure (if (!$cast(...)) `uvm_fatal(...))", Vec::new(), fix.into_iter().collect());
            }
        }
    }
}

fn cast_fix(cx: &Cx, stmt: Span) -> Option<Fix> {
    let text = cx.a.file_text(cx.file)?;
    let src = text.get(stmt.start as usize..stmt.end as usize)?;
    if src.contains('\n') || !src.ends_with(';') {
        return None;
    }
    let call = src[..src.len() - 1].trim_end();
    let uvm = cx.a.unit(cx.unit).macros_after.contains_key("uvm_fatal");
    let new = if uvm { format!("if (!{call}) `uvm_fatal(\"CAST\", \"$cast failed\")") } else { format!("if (!{call}) $fatal(1, \"$cast failed\");") };
    Some(Fix { title: "Check the result of $cast".into(), edits: vec![(stmt, super::randomize::keep_else(cx, stmt, new))], safe: false })
}

impl Rule for RandomizeVoidCast {
    fn meta(&self) -> &'static RuleMeta {
        &VOID_RANDOMIZE
    }
    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        for b in &facts.bodies {
            let Some(stmts) = &b.stmts else { continue };
            for s in stmts {
                let Simple::Expr(s, e) = *s else { continue };
                // `void'( ... randomize ... )`.
                if !(t[s].is_kw("void") && t.get(s + 1).is_some_and(|x| x.text == "'") && t.get(s + 2).is_some_and(|x| x.is_punct("(")) && t[e - 1].is_punct(")")) {
                    continue;
                }
                if t[s + 3..e - 1].iter().any(|x| x.kind == TokKind::Ident && x.text == "randomize") {
                    cx.report(None, Span::new(cx.file, t[s].span.start, t[e].span.end), "void'(randomize()): check the result instead (LNG-10)", Vec::new());
                }
            }
        }
    }
}

impl Rule for DisplayInUvmClass {
    fn meta(&self) -> &'static RuleMeta {
        &DISPLAY
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        let u = a.unit(cx.unit).clone();
        for b in &facts.bodies {
            let Some(d) = b.decl else { continue };
            let Some(class) = member_class(a, DeclRef { unit: cx.unit, decl: d }) else { continue };
            let hits: Vec<Span> = t[b.start..b.end]
                .iter()
                .filter(|x| x.kind == TokKind::SysIdent && is_display(&x.text))
                .map(|x| x.span)
                .collect();
            if hits.is_empty() {
                continue;
            }
            let _ = &u;
            match a.lint_facts.class(a, class).role {
                Ok(UvmRole::None) => {}
                Ok(_) => {
                    for s in hits {
                        cx.report(None, s, "Report with `uvm_info (or `uvm_warning, `uvm_error, `uvm_fatal) in UVM classes, not with $display", Vec::new());
                    }
                }
                Err(_) => cx.unknown_at("class ancestry", hits[0]),
            }
        }
    }
}

fn is_display(s: &str) -> bool {
    let base = ["$display", "$write", "$monitor", "$strobe"];
    base.iter().any(|b| s == *b || s.strip_prefix(b).is_some_and(|r| matches!(r, "b" | "h" | "o")))
}

impl Rule for SuppressionNotWaiver {
    fn meta(&self) -> &'static RuleMeta {
        &SUPPRESSION
    }
    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        for w in &facts.waivers.items {
            if matches!(w.kind, WaiverKind::Disable | WaiverKind::DisableFile) {
                cx.report(None, w.span, "A dvicode-lint suppression: the style guide records exceptions as `// <ID> waived: <reason>` on the code they cover", Vec::new());
            }
        }
    }
}
