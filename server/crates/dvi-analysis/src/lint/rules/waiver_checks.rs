//! Rules about waivers and suppressions themselves (the guide's §0).

use crate::lint::guide_ids::{guide_level, looks_like_guide_id};
use crate::lint::waivers::{is_template_reason, WaiverKind};
use crate::lint::{rules, Cx, Level, Rule, RuleMeta, Ruleset};

pub struct WaiverWithoutReason;
pub struct WaiverUnknownId;
pub struct WaiverUnattached;
pub struct SuppressionUnmatched;

static WITHOUT_REASON: RuleMeta = RuleMeta {
    id: "waiver-without-reason",
    guide: &[],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "A waiver or suppression gives no reason.",
    why: "The style guide (§0) requires every waiver to say why the rule does not apply; a reason is what reviewers and later readers check.",
    params: &[],
    text_only: true,
};

static UNKNOWN_ID: RuleMeta = RuleMeta {
    id: "waiver-unknown-id",
    guide: &[],
    rulesets: &[Ruleset::Recommended],
    default_level: Level::Warning,
    subchecks: &[],
    summary: "A waiver or suppression names a rule that does not exist.",
    why: "A misspelled rule ID waives nothing, so the finding it was meant for stays (or a later one goes unnoticed).",
    params: &[],
    text_only: true,
};

static UNATTACHED: RuleMeta = RuleMeta {
    id: "waiver-unattached",
    guide: &[],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Info,
    subchecks: &[],
    summary: "A waiver is followed by no statement or declaration it could cover.",
    why: "A waiver covers the statement or declaration right after it; one before `end`, `else` or the end of the file covers nothing.",
    params: &[],
    text_only: true,
};

static UNMATCHED: RuleMeta = RuleMeta {
    id: "suppression-unmatched",
    guide: &[],
    rulesets: &[Ruleset::Hexk],
    default_level: Level::Info,
    subchecks: &[],
    summary: "`dvicode-lint: enable=` without a `disable=` before it.",
    why: "An enable that ends no suppression usually means the disable was removed or misspelled.",
    params: &[],
    text_only: true,
};

/// Whether an ID names something: a lint rule, or a rule of the style guide.
fn known(id: &str) -> bool {
    rules::meta(id).is_some() || looks_like_guide_id(id) && guide_level(id).is_some()
}

impl Rule for WaiverWithoutReason {
    fn meta(&self) -> &'static RuleMeta {
        &WITHOUT_REASON
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        for w in &facts.waivers.items {
            if w.kind != WaiverKind::Enable && is_template_reason(&w.reason) {
                cx.report(None, w.span, "This waiver gives no reason: say why the rule does not apply here", Vec::new());
            }
        }
    }
}

impl Rule for WaiverUnknownId {
    fn meta(&self) -> &'static RuleMeta {
        &UNKNOWN_ID
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        for w in &facts.waivers.items {
            for id in &w.ids {
                if !known(id) {
                    cx.report(None, w.span, format!("No rule is named {id}: this waives nothing"), Vec::new());
                }
            }
        }
    }
}

impl Rule for WaiverUnattached {
    fn meta(&self) -> &'static RuleMeta {
        &UNATTACHED
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        for w in &facts.waivers.items {
            if w.kind == WaiverKind::Guide && w.covers.is_none() {
                cx.report(None, w.span, "This waiver covers nothing: put it right before the statement or declaration it is for", Vec::new());
            }
        }
    }
}

impl Rule for SuppressionUnmatched {
    fn meta(&self) -> &'static RuleMeta {
        &UNMATCHED
    }

    fn check(&self, cx: &mut Cx) {
        let facts = cx.facts.clone();
        for (span, id) in &facts.waivers.unmatched {
            cx.report(None, *span, format!("This enable of {id} ends no disable of it"), Vec::new());
        }
    }
}
