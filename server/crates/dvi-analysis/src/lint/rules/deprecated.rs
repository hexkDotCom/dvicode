//! `uvm-deprecated-api`: uses of UVM API that UVM 1.2 deprecated and IEEE 1800.2 removed.

use crate::lint::{catalog, Cx, Level, Rule, RuleMeta, Ruleset};
use dvi_syntax::ast::{DeclKind, RefId};

pub struct DeprecatedApi;

static META: RuleMeta = RuleMeta {
    id: "uvm-deprecated-api",
    guide: &["FAC-11"],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "Deprecated UVM API is used.",
    why: "These parts of UVM were deprecated in UVM 1.2 and removed in IEEE 1800.2: code using them does not compile with a current UVM library (or with +define+UVM_NO_DEPRECATED).",
    params: &[],
    text_only: false,
};

impl Rule for DeprecatedApi {
    fn meta(&self) -> &'static RuleMeta {
        &META
    }

    fn check(&self, cx: &mut Cx) {
        let a = cx.a;
        let u = a.unit(cx.unit).clone();
        if cx.facts.repeated {
            cx.unknown("repeated inclusion");
            return;
        }
        let uvm = a.lint_facts.uvm_files(a);
        if let Some(pos) = u.positions.get(&cx.file) {
            for &(span, rid, seg) in &pos.refs {
                if !cx.step(1) {
                    return;
                }
                let chain = u.parsed.reference(rid as RefId);
                let Some(s) = chain.segs.get(seg as usize) else { continue };
                if catalog::deprecated(&s.name, false).next().is_none() {
                    continue;
                }
                let Some(target) = a.resolve_ref(cx.unit, rid as RefId, seg as usize) else {
                    cx.unknown_at("unresolved name", span);
                    continue;
                };
                let td = a.decl(target);
                if !uvm.contains(&td.name_span.file) {
                    continue;
                }
                let owner = crate::lint::facts::member_class(a, target).map(|c| a.decl(c).name.clone());
                let Some(entry) = catalog::deprecated(&s.name, false).find(|e| e.owner.map(|o| o.into()) == owner) else { continue };
                if td.kind == DeclKind::Class {
                    continue;
                }
                cx.report(None, span, format!("{} is deprecated UVM API (removed in IEEE 1800.2): use {}", s.name, entry.instead), Vec::new());
            }
        }
        for m in &u.macro_uses {
            if m.name_span.file != cx.file {
                continue;
            }
            let Some(entry) = catalog::deprecated(&m.name, true).next() else { continue };
            if !m.def.as_ref().is_some_and(|d| uvm.contains(&d.name_span.file)) {
                continue;
            }
            cx.report(None, m.call_span, format!("`{} is deprecated UVM API (removed in IEEE 1800.2): use {}", m.name, entry.instead), Vec::new());
        }
    }
}
