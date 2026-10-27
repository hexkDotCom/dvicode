//! `hexk` rules about UVM classes and the UVM API (style guide FAC, CMP, CLS, LNG, MSG, OBJ,
//! SEQ, NAM-7/8).

use super::registration::registrations;
use super::util::{argument, call_args, in_uvm, member_class_name, string_value};
use crate::ide::stmt::{self, Simple};
use crate::lint::catalog;
use crate::lint::facts::{in_macro_call, member_class, UvmRole};
use crate::lint::{Cx, Fix, Level, Rule, RuleMeta, Ruleset};
use crate::world::DeclRef;
use dvi_syntax::ast::{dflags, DeclId, DeclKind, RefId};
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

meta!(NOT_REGISTERED, "uvm-class-not-registered", ["FAC-2"], Warning, "A concrete UVM class is not registered with the factory.", "Only registered classes can be created and overridden through the factory (FAC-1, FAC-2); an unregistered class cannot be replaced from a test.");
meta!(MISSING_SUPER, "missing-super-phase-call", ["CMP-4"], Warning, "A function phase does not call `super.<phase>(phase)` first.", "The parent's phase work (UVM's own, or a base class's) is skipped or runs late; CMP-4 asks for the call first in every function phase.");
meta!(CREATED_WITH_NEW, "component-created-with-new", ["FAC-1"], Warning, "A component is created with `new` instead of the factory.", "A component made with `new` cannot be overridden by a test (FAC-1); only TLM ports, exports, imps and analysis FIFOs are made with `new`.");
meta!(INTERNAL_MEMBER, "uvm-internal-member", ["FAC-11"], Warning, "A UVM internal member (`m_...`) is used.", "Members whose names start with `m_` are UVM's internals: they change between UVM versions without notice (FAC-11). `m_sequencer` is documented and allowed.");
meta!(DIRECT_REPORT, "direct-report-call", ["FAC-9"], Warning, "`uvm_report_info/warning/error/fatal` is called directly.", "The report macros skip building the message when it is filtered; direct calls always build it (FAC-9).");
meta!(MESSAGE_ID, "message-id-get-name", ["MSG-2"], Warning, "A report message ID is `get_name()`.", "Instance names depend on where the class is instantiated and are already in the message header; IDs should be static and functional (MSG-2).");
meta!(OBJECTION, "objection-without-description", ["OBJ-4"], Warning, "`raise_objection`/`drop_objection` without a description.", "The description is what `+UVM_OBJECTION_TRACE` shows (OBJ-4).");
meta!(DEFAULT_SEQUENCE, "default-sequence", ["SEQ-11"], Warning, "`default_sequence` is set.", "Default sequences start stimulus implicitly from configuration; start sequences explicitly from the test (SEQ-11).");
meta!(PRE_DO, "pre-do-override", ["SEQ-14"], Info, "`pre_do`, `mid_do` or `post_do` is overridden.", "These hooks run inside UVM's item flow and are easy to misuse; SEQ-14 allows them only to change a sequence that cannot be edited, together with a factory override.");
meta!(VIRTUAL_OVERRIDE, "virtual-on-override", ["CLS-4"], Warning, "`virtual` is repeated on an override of a UVM virtual method.", "An override of a virtual method is virtual anyway; the style guide keeps `virtual` for new methods that extensions may override (CLS-4).");
meta!(CONSTRAINT_NAME, "constraint-name", ["LNG-13"], Warning, "A constraint is not named `<what>_cstr`.", "One suffix makes constraints easy to find and to switch off by name (LNG-13).");
meta!(CREATE_CHARS, "create-name-chars", ["NAM-7"], Warning, "A factory `create` name has characters outside `[a-zA-Z0-9_]` or `__`.", "Instance names appear in hierarchical paths, config-db lookups and logs; other characters break glob matching and tools (NAM-7).");
meta!(CREATE_MATCHES, "create-name-matches", ["NAM-8"], Warning, "A factory `create` name differs from the variable it is assigned to.", "Log paths then map directly to code (NAM-8).");

pub struct ClassNotRegistered;
pub struct MissingSuperPhaseCall;
pub struct CreatedWithNew;
pub struct InternalMember;
pub struct DirectReportCall;
pub struct MessageIdGetName;
pub struct ObjectionWithoutDescription;
pub struct DefaultSequence;
pub struct PreDoOverride;
pub struct VirtualOnOverride;
pub struct ConstraintName;
pub struct CreateNameChars;
pub struct CreateNameMatches;

/// The function phases CMP-4 asks `super` of.
fn function_phase(name: &str) -> bool {
    catalog::phase(name) == Some(false)
}

impl Rule for ClassNotRegistered {
    fn meta(&self) -> &'static RuleMeta {
        &NOT_REGISTERED
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        if cx.facts.repeated {
            cx.unknown("repeated inclusion");
            return;
        }
        let registered: Vec<DeclId> = registrations(cx, &u).iter().map(|r| r.class.decl).collect();
        let uvm = a.lint_facts.uvm_files(a);
        for (i, d) in u.parsed.decls.iter().enumerate() {
            if d.kind != DeclKind::Class || d.name_span.file != cx.file || d.has(dflags::FROM_MACRO) || d.has(dflags::VIRTUAL) || !cx.step(1) {
                continue;
            }
            let class = DeclRef { unit: cx.unit, decl: i as DeclId };
            match a.lint_facts.class(a, class).role {
                Ok(UvmRole::Object | UvmRole::Component) => {}
                Ok(_) => continue,
                Err(_) => {
                    cx.unknown_at("class ancestry", d.name_span);
                    continue;
                }
            }
            if registered.contains(&(i as DeclId)) {
                continue;
            }
            // Its own `type_id` (a registry typedef written by hand).
            if u.parsed.lookup_member(i as DeclId, "type_id").iter().any(|m| u.parsed.decl(*m).kind == DeclKind::Typedef) {
                continue;
            }
            // A project macro in the body may register it: not known.
            let opaque = u.macro_uses.iter().any(|m| m.name_span.file == d.span.file && d.span.start <= m.name_span.start && m.name_span.end <= d.span.end && !m.def.as_ref().is_some_and(|x| uvm.contains(&x.name_span.file)));
            if opaque {
                cx.unknown_at("macro", d.name_span);
                continue;
            }
            let comp = matches!(a.lint_facts.class(a, class).role, Ok(UvmRole::Component));
            let param = a.lint_facts.class(a, class).parameterized;
            let m = format!("uvm_{}_{}utils", if comp { "component" } else { "object" }, if param { "param_" } else { "" });
            cx.report(None, d.name_span, format!("{} is not registered with the factory: add `{m}({})", d.name, d.name), Vec::new());
        }
    }
}

/// What a top-level statement of a method body is, for CMP-4.
#[derive(PartialEq)]
enum Item {
    /// A declaration without an initializer (or with one that calls nothing).
    Decl,
    /// A declaration whose initializer calls something: it runs before the next statement.
    DeclWithCall,
    Other,
}

/// Classifies `t` (one top-level statement with its `;`).
fn item(t: &[Token]) -> Item {
    let Some(first) = t.first() else { return Item::Decl };
    let typed = first.kind == TokKind::Keyword && matches!(first.text.as_str(), "int" | "bit" | "logic" | "reg" | "byte" | "shortint" | "longint" | "integer" | "real" | "time" | "string" | "event" | "chandle" | "automatic" | "static" | "const" | "var")
        || first.kind == TokKind::Ident && t.get(1).is_some_and(|x| x.kind == TokKind::Ident || x.is_punct("::") || x.is_punct("#"));
    if !typed {
        return Item::Other;
    }
    // A call in the initializer (a parameterized type's `#(...)` is not one).
    match t.iter().position(|x| x.is_punct("=")) {
        Some(eq) if t[eq..].iter().any(|x| x.is_punct("(")) => Item::DeclWithCall,
        _ => Item::Decl,
    }
}

/// The top-level statements of the function body `t[start..end]` (from its header's `;` to its
/// `endfunction`): token ranges; `None` when the walker cannot follow them.
fn top_statements(t: &[Token], start: usize, end: usize) -> Option<Vec<(usize, usize)>> {
    let body = &t[start..end];
    let mut k = 0;
    let mut depth = 0i32;
    // After the header: its first `;` outside brackets.
    while k < body.len() {
        let x = &body[k];
        if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
            depth += 1;
        } else if x.is_punct(")") || x.is_punct("]") || x.is_punct("}") {
            depth -= 1;
        } else if x.is_punct(";") && depth == 0 {
            break;
        }
        k += 1;
    }
    k += 1;
    let mut out = Vec::new();
    while k < body.len() && !body[k].is_kw("endfunction") {
        let e = stmt::statement_end(body, k)?;
        if e <= k {
            return None;
        }
        out.push((start + k, start + e));
        k = e;
    }
    Some(out)
}

impl Rule for MissingSuperPhaseCall {
    fn meta(&self) -> &'static RuleMeta {
        &MISSING_SUPER
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        for b in &facts.bodies {
            let Some(di) = b.decl else { continue };
            let me = DeclRef { unit: cx.unit, decl: di };
            let d = a.decl(me);
            if d.kind != DeclKind::Function || !function_phase(&d.name) {
                continue;
            }
            if in_macro_call(a, cx.unit, d.name_span) {
                cx.unknown_at("macro", d.name_span);
                continue;
            }
            let Some(class) = member_class(a, me) else { continue };
            match a.lint_facts.class(a, class).role {
                Ok(UvmRole::Component) => {}
                Ok(_) => continue,
                Err(_) => {
                    cx.unknown_at("class ancestry", d.name_span);
                    continue;
                }
            }
            // What it overrides: none, or a pure virtual method, asks for no `super` call.
            let Some(base) = a.class_base(class) else { continue };
            let Some(parent) = a.class_member(base, &d.name, 0) else { continue };
            if a.decl(parent).has(dflags::PURE) {
                continue;
            }
            let Some(stmts) = top_statements(t, b.start, b.end) else {
                cx.unknown_at("statement structure", d.name_span);
                continue;
            };
            // The first top-level statement that is not a plain declaration.
            let first = stmts.iter().position(|(s, e)| item(&t[*s..*e]) != Item::Decl);
            let is_super = |(s, e): (usize, usize)| {
                t[s].text == "super" && t.get(s + 1).is_some_and(|x| x.is_punct(".")) && t.get(s + 2).is_some_and(|x| x.text == d.name) && t.get(s + 3).is_some_and(|x| x.is_punct("(")) && t[e - 1].is_punct(";")
            };
            if first.is_some_and(|i| is_super(stmts[i])) {
                continue;
            }
            // The fix: only where the body calls no `super.<phase>` at all (else it would run twice),
            // and the first statement is not a declaration with an initializer (declarations come
            // first in a block).
            let anywhere = t[b.start..b.end].windows(3).any(|w| w[0].text == "super" && w[1].is_punct(".") && w[2].text == d.name);
            let before = first.map(|i| stmts[i]);
            let fixable = !anywhere && before.is_none_or(|(s, e)| item(&t[s..e]) == Item::Other);
            let fixes = if fixable { super_fix(cx, me, before.map(|(s, _)| s), b.end).into_iter().collect() } else { Vec::new() };
            let msg = if anywhere { format!("{} must call super.{}(phase) first, unconditionally", d.name, d.name) } else { format!("{} does not call super.{}(phase): call it first", d.name, d.name) };
            cx.report_fix(None, d.name_span, msg, vec![(a.decl(parent).name_span, "the method it overrides".to_string())], fixes);
        }
    }
}

/// Inserts `super.<phase>(<arg>);` before the top-level statement at token `before` (else before
/// the `endfunction` at `end - 1`).
fn super_fix(cx: &Cx, m: DeclRef, before: Option<usize>, end: usize) -> Option<Fix> {
    let a = cx.a;
    let d = a.decl(m);
    let arg = d.children.iter().map(|c| a.unit(m.unit).parsed.decl(*c)).find(|c| c.kind == DeclKind::Argument)?.name.clone();
    let t = &cx.facts.tokens;
    let text = a.file_text(cx.file)?;
    let li = a.file_lines(cx.file)?;
    let (at, indent) = match before {
        Some(k) => {
            let ls = li.line_start(li.line_of(t[k].span.start)) as usize;
            if !text[ls..t[k].span.start as usize].trim().is_empty() {
                return None;
            }
            (ls, text[ls..t[k].span.start as usize].to_string())
        }
        None => {
            let close = &t[end.checked_sub(1)?];
            let ls = li.line_start(li.line_of(close.span.start)) as usize;
            if !text[ls..close.span.start as usize].trim().is_empty() {
                return None;
            }
            (ls, format!("{}  ", &text[ls..close.span.start as usize]))
        }
    };
    let at = at as u32;
    Some(Fix { title: format!("Call super.{}({arg}) first", d.name), edits: vec![(Span::new(cx.file, at, at), format!("{indent}super.{}({arg});\n", d.name))], safe: false })
}

/// Whether a class derives from UVM's `uvm_tlm_analysis_fifo` (FAC-1's exemption).
fn analysis_fifo(cx: &Cx, class: DeclRef) -> bool {
    match &cx.a.lint_facts.class(cx.a, class).ancestry {
        Ok(chain) => chain.iter().any(|c| cx.a.decl(*c).name == "uvm_tlm_analysis_fifo" && in_uvm(cx, *c)),
        Err(_) => false,
    }
}

impl Rule for CreatedWithNew {
    fn meta(&self) -> &'static RuleMeta {
        &CREATED_WITH_NEW
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        let u = a.unit(cx.unit).clone();
        // The file's declarations by the place of their name.
        let by_name: rustc_hash::FxHashMap<(u32, u32), DeclId> =
            u.parsed.decls.iter().enumerate().filter(|(_, d)| d.name_span.file == cx.file).map(|(i, d)| ((d.name_span.start, d.name_span.end), i as DeclId)).collect();
        // Every `dest = new` / `dest = new(...)` written in the file: in methods and processes,
        // and class properties or variables with an initializer (`new[n]` sizes a dynamic array).
        for eq in 0..t.len().saturating_sub(1) {
            if !(t[eq].is_punct("=") && t[eq + 1].is_kw("new")) || t.get(eq + 2).is_some_and(|x| x.is_punct("[")) || facts.in_define(eq) {
                continue;
            }
            if !cx.step(1) {
                return;
            }
            // The destination's name: the last name before `=` (selects skipped).
            let mut k = eq;
            while k > 0 && t[k - 1].is_punct("]") {
                let mut depth = 0;
                while k > 0 {
                    k -= 1;
                    if t[k].is_punct("]") {
                        depth += 1;
                    } else if t[k].is_punct("[") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            let Some(name) = k.checked_sub(1).filter(|n| t[*n].kind == TokKind::Ident) else { continue };
            let at = t[name].span;
            // A declaration's own name, else a reference.
            let target = by_name.get(&(at.start, at.end)).map(|i| DeclRef { unit: cx.unit, decl: *i }).or_else(|| super::util::reference_at(cx, at).and_then(|r| r.3));
            let Some(target) = target else {
                cx.unknown_at("unresolved destination", at);
                continue;
            };
            let Some(ty) = a.type_of(target) else { continue };
            let ty = if a.decl(ty).kind == DeclKind::Typedef { a.follow_typedef(ty).unwrap_or(ty) } else { ty };
            if a.decl(ty).kind != DeclKind::Class {
                continue;
            }
            match a.lint_facts.class(a, ty).role {
                Ok(UvmRole::Component) if !analysis_fifo(cx, ty) => {
                    cx.report(None, t[eq + 1].span, format!("{} is a component: create it with {}::type_id::create(\"{}\", this)", a.decl(ty).name, a.decl(ty).name, t[name].text), Vec::new());
                }
                Ok(_) => {}
                Err(_) => cx.unknown_at("class ancestry", t[eq + 1].span),
            }
        }
    }
}

/// Calls written in the file whose callee resolves to a UVM library member: (the name's token
/// index, the reference, the resolved member, its class's name).
fn library_calls(cx: &Cx, names: &[&str]) -> Vec<(usize, RefId, DeclRef, Option<smol_str::SmolStr>)> {
    let a = cx.a;
    let u = a.unit(cx.unit);
    let Some(pos) = u.positions.get(&cx.file) else { return Vec::new() };
    let t = &cx.facts.tokens;
    let mut out = Vec::new();
    for &(span, rid, seg) in &pos.refs {
        let chain = u.parsed.reference(rid);
        let Some(sg) = chain.segs.get(seg as usize) else { continue };
        if !sg.called || !names.contains(&sg.name.as_str()) {
            continue;
        }
        let Some(target) = a.resolve_ref(cx.unit, rid, seg as usize) else { continue };
        if !in_uvm(cx, target) {
            continue;
        }
        let k = t.partition_point(|x| x.span.start < span.start);
        if t.get(k).is_some_and(|x| x.span == span) {
            out.push((k, rid, target, member_class_name(cx, target)));
        }
    }
    out
}

impl Rule for InternalMember {
    fn meta(&self) -> &'static RuleMeta {
        &INTERNAL_MEMBER
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        let Some(pos) = u.positions.get(&cx.file) else { return };
        for &(span, rid, seg) in &pos.refs {
            let chain = u.parsed.reference(rid);
            let Some(sg) = chain.segs.get(seg as usize) else { continue };
            if !sg.name.starts_with("m_") || sg.name == "m_sequencer" || !cx.step(1) {
                continue;
            }
            let Some(target) = a.resolve_ref(cx.unit, rid, seg as usize) else { continue };
            if in_uvm(cx, target) && member_class(a, target).is_some() {
                cx.report(None, span, format!("{} is a UVM internal: use UVM's documented API", sg.name), Vec::new());
            }
        }
    }
}

impl Rule for DirectReportCall {
    fn meta(&self) -> &'static RuleMeta {
        &DIRECT_REPORT
    }
    fn check(&self, cx: &mut Cx) {
        for (k, _, _, _) in library_calls(cx, &["uvm_report_info", "uvm_report_warning", "uvm_report_error", "uvm_report_fatal"]) {
            let x = &cx.facts.tokens[k];
            let (span, name) = (x.span, x.text.clone());
            cx.report(None, span, format!("{name}() called directly: use the report macro (`uvm_info and the others), which skips filtered messages"), Vec::new());
        }
    }
}

impl Rule for MessageIdGetName {
    fn meta(&self) -> &'static RuleMeta {
        &MESSAGE_ID
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        let uvm = a.lint_facts.uvm_files(a);
        let t = cx.facts.tokens.clone();
        for m in &u.macro_uses {
            let report = matches!(m.name.as_str(), "uvm_info" | "uvm_warning" | "uvm_error" | "uvm_fatal" | "uvm_info_context" | "uvm_warning_context" | "uvm_error_context" | "uvm_fatal_context");
            if m.name_span.file != cx.file || !report || !m.def.as_ref().is_some_and(|d| uvm.contains(&d.name_span.file)) {
                continue;
            }
            let k = t.partition_point(|x| x.span.start < m.name_span.start);
            let Some(args) = call_args(&t, k + 1) else { continue };
            let Some(&(mut s, mut e)) = args.first() else { continue };
            // Without redundant parentheses.
            while e > s + 1 && t[s].is_punct("(") && t[e - 1].is_punct(")") {
                s += 1;
                e -= 1;
            }
            // `[recv.]get_name()`.
            let ends = e >= s + 3 && t[e - 3].text == "get_name" && t[e - 2].is_punct("(") && t[e - 1].is_punct(")");
            let recv_ok = ends && t[s..e - 3].iter().all(|x| x.kind == TokKind::Ident || x.is_punct(".") || x.text == "this" || x.text == "super");
            if ends && recv_ok {
                cx.report(None, Span::new(cx.file, t[s].span.start, t[e - 1].span.end), "The message ID is get_name(): use a static, functional ID (or get_type_name())", Vec::new());
            }
        }
    }
}

impl Rule for ObjectionWithoutDescription {
    fn meta(&self) -> &'static RuleMeta {
        &OBJECTION
    }
    fn check(&self, cx: &mut Cx) {
        let t = cx.facts.tokens.clone();
        for (k, _, _, class) in library_calls(cx, &["raise_objection", "drop_objection"]) {
            if !class.as_deref().is_some_and(|c| c == "uvm_phase" || c == "uvm_objection") {
                continue;
            }
            let Some(args) = call_args(&t, k + 1) else {
                // `phase.raise_objection;`: no arguments at all.
                if !t.get(k + 1).is_some_and(|x| x.is_punct("(")) {
                    cx.report(None, t[k].span, format!("{} without a description: pass one (shown by +UVM_OBJECTION_TRACE)", t[k].text), Vec::new());
                }
                continue;
            };
            let Some(desc) = argument(&t, &args, 1, "description") else { continue };
            let missing = match desc {
                None => true,
                Some((s, e)) => e == s || e == s + 1 && string_value(&t[s]).is_some_and(|v| v.is_empty()),
            };
            if missing {
                cx.report(None, t[k].span, format!("{} without a description: pass one (shown by +UVM_OBJECTION_TRACE)", t[k].text), Vec::new());
            }
        }
    }
}

impl Rule for DefaultSequence {
    fn meta(&self) -> &'static RuleMeta {
        &DEFAULT_SEQUENCE
    }
    fn check(&self, cx: &mut Cx) {
        let t = cx.facts.tokens.clone();
        for (k, _, _, class) in library_calls(cx, &["set"]) {
            let (pos, formal) = match class.as_deref() {
                Some("uvm_config_db") => (2, "field_name"),
                Some("uvm_resource_db") => (1, "name"),
                _ => continue,
            };
            let Some(args) = call_args(&t, k + 1) else { continue };
            let Some(Some((s, e))) = argument(&t, &args, pos, formal) else { continue };
            if e == s + 1 && string_value(&t[s]) == Some("default_sequence") {
                cx.report(None, t[s].span, "default_sequence: start the sequence explicitly from the test", Vec::new());
            }
        }
    }
}

/// The UVM library method a method overrides, through any number of project overrides.
fn library_origin(cx: &Cx, class: DeclRef, name: &str) -> Option<DeclRef> {
    let a = cx.a;
    let mut cur = a.class_base(class)?;
    for _ in 0..64 {
        let m = a.class_member(cur, name, 0)?;
        if in_uvm(cx, m) {
            return Some(m);
        }
        let owner = member_class(a, m)?;
        cur = a.class_base(owner)?;
    }
    None
}

impl Rule for PreDoOverride {
    fn meta(&self) -> &'static RuleMeta {
        &PRE_DO
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        for (i, d) in u.parsed.decls.iter().enumerate() {
            if !matches!(d.kind, DeclKind::Function | DeclKind::Task) || d.name_span.file != cx.file || !matches!(d.name.as_str(), "pre_do" | "mid_do" | "post_do") || d.has(dflags::OUT_OF_BLOCK) {
                continue;
            }
            let Some(class) = member_class(a, DeclRef { unit: cx.unit, decl: i as DeclId }) else { continue };
            if library_origin(cx, class, &d.name).is_some_and(|o| member_class_name(cx, o).as_deref() == Some("uvm_sequence_base")) {
                cx.report(None, d.name_span, format!("{} overrides UVM's {}: SEQ-14 allows this only to change a sequence that cannot be edited, with a factory override", d.name, d.name), Vec::new());
            }
        }
    }
}

impl Rule for VirtualOnOverride {
    fn meta(&self) -> &'static RuleMeta {
        &VIRTUAL_OVERRIDE
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        let t = cx.facts.tokens.clone();
        for (i, d) in u.parsed.decls.iter().enumerate() {
            if !matches!(d.kind, DeclKind::Function | DeclKind::Task) || d.name_span.file != cx.file || !d.has(dflags::VIRTUAL) || d.has(dflags::PURE) || d.has(dflags::FROM_MACRO) || d.name == "new" {
                continue;
            }
            if in_macro_call(a, cx.unit, d.name_span) {
                continue;
            }
            let Some(class) = member_class(a, DeclRef { unit: cx.unit, decl: i as DeclId }) else { continue };
            let Some(origin) = library_origin(cx, class, &d.name) else { continue };
            if !a.decl(origin).has(dflags::VIRTUAL) {
                continue;
            }
            // The method's own `virtual`: among the qualifiers right before its `function`/`task`
            // keyword (a `virtual` a macro writes, or the class's, is not this header's).
            let k = t.partition_point(|x| x.span.start < d.name_span.start);
            let Some(kw) = (k.saturating_sub(16)..k).rev().find(|&j| t[j].is_kw("function") || t[j].is_kw("task")) else { continue };
            let mut q = kw;
            let mut v = None;
            while q > 0 && matches!(t[q - 1].text.as_str(), "virtual" | "protected" | "local" | "static" | "automatic") && t[q - 1].kind == TokKind::Keyword {
                q -= 1;
                if t[q].is_kw("virtual") {
                    v = Some(q);
                }
            }
            let Some(v) = v else { continue };
            let end = t.get(v + 1).map_or(t[v].span.end, |n| n.span.start);
            let fix = Fix { title: "Remove `virtual`".into(), edits: vec![(Span::new(cx.file, t[v].span.start, end), String::new())], safe: true };
            cx.report_fix(None, t[v].span, format!("{} overrides UVM's virtual {}: leave out `virtual`", d.name, d.name), Vec::new(), vec![fix]);
        }
    }
}

impl Rule for ConstraintName {
    fn meta(&self) -> &'static RuleMeta {
        &CONSTRAINT_NAME
    }
    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        for (i, d) in u.parsed.decls.iter().enumerate() {
            if d.kind != DeclKind::Constraint || d.name_span.file != cx.file || d.has(dflags::FROM_MACRO) || d.has(dflags::OUT_OF_BLOCK) || d.name.ends_with("_cstr") {
                continue;
            }
            let Some(class) = member_class(a, DeclRef { unit: cx.unit, decl: i as DeclId }) else { continue };
            // An override keeps the name of the constraint it replaces.
            match a.lint_facts.class(a, class).ancestry.as_ref() {
                Ok(chain) => {
                    if chain.iter().skip(1).any(|c| a.unit(c.unit).parsed.lookup_member(c.decl, &d.name).iter().any(|m| a.unit(c.unit).parsed.decl(*m).kind == DeclKind::Constraint)) {
                        continue;
                    }
                }
                Err(_) => {
                    cx.unknown_at("class ancestry", d.name_span);
                    continue;
                }
            }
            cx.report(None, d.name_span, format!("Constraint {} is not named <what>_cstr", d.name), Vec::new());
        }
    }
}

/// Calls of the UVM factory's `create` (a registry's): the token of `create` and its arguments.
fn factory_creates(cx: &Cx) -> Vec<(usize, Vec<(usize, usize)>)> {
    let t = &cx.facts.tokens;
    library_calls(cx, &["create"])
        .into_iter()
        .filter(|(_, _, _, c)| c.as_deref().is_some_and(|c| c.ends_with("_registry")))
        .filter_map(|(k, _, _, _)| Some((k, call_args(t, k + 1)?)))
        .collect()
}

impl Rule for CreateNameChars {
    fn meta(&self) -> &'static RuleMeta {
        &CREATE_CHARS
    }
    fn check(&self, cx: &mut Cx) {
        let t = cx.facts.tokens.clone();
        for (_, args) in factory_creates(cx) {
            let Some(Some((s, e))) = argument(&t, &args, 0, "name") else { continue };
            if e != s + 1 {
                continue;
            }
            let Some(v) = string_value(&t[s]) else { continue };
            if v.is_empty() || v.contains("__") || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                cx.report(None, t[s].span, format!("The instance name \"{v}\" must use [a-zA-Z0-9_] only, without __"), Vec::new());
            }
        }
    }
}

impl Rule for CreateNameMatches {
    fn meta(&self) -> &'static RuleMeta {
        &CREATE_MATCHES
    }
    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        let t = &facts.tokens;
        // Simple statements by start, for the statement around each call.
        let mut stmts: Vec<(usize, usize)> = facts.bodies.iter().filter_map(|b| b.stmts.as_ref()).flatten().filter_map(|x| match *x {
            Simple::Expr(a, b) => Some((a, b)),
            _ => None,
        }).collect();
        stmts.sort();
        for (k, args) in factory_creates(cx) {
            let Some(Some((s, e))) = argument(t, &args, 0, "name") else { continue };
            if e != s + 1 {
                continue;
            }
            let Some(v) = string_value(&t[s]) else { continue };
            let i = stmts.partition_point(|(a, _)| *a <= k);
            let Some(&(st, end)) = i.checked_sub(1).map(|i| &stmts[i]).filter(|(_, b)| k < *b) else { continue };
            let Some(eq) = (st..k).find(|&j| t[j].is_punct("=")) else { continue };
            // The call must be the value assigned: `dest = T::type_id::create(...);`.
            let close = args.last().map_or(k + 2, |(_, e)| *e);
            let direct = t[eq + 1..k].iter().all(|x| x.kind == TokKind::Ident || x.is_punct("::") || x.is_punct(".") || x.is_punct("#") || x.is_punct("(") || x.is_punct(")") || x.kind == TokKind::Keyword && x.text == "type") && close + 1 == end;
            if !direct {
                continue;
            }
            // An element of an array, or a destination that is not a name: not checked.
            let dest = &t[st..eq];
            if dest.last().is_some_and(|x| x.is_punct("]")) || dest.is_empty() {
                continue;
            }
            let Some(name) = dest.last().filter(|x| x.kind == TokKind::Ident) else { continue };
            if name.text != v {
                cx.report(None, t[s].span, format!("create(\"{v}\") is assigned to {}: name it \"{}\"", name.text, name.text), Vec::new());
            }
        }
    }
}
