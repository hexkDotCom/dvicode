//! The lint rules, in the order they run.

mod randomize;
mod registration;
mod text;
mod waiver_checks;

use super::{Rule, RuleMeta};

static RULES: &[&dyn Rule] = &[
    &randomize::RandomizeResultDiscarded,
    &registration::RegistrationMismatch,
    &text::TrailingWhitespace,
    &waiver_checks::WaiverWithoutReason,
    &waiver_checks::WaiverUnknownId,
    &waiver_checks::WaiverUnattached,
    &waiver_checks::SuppressionUnmatched,
];

pub fn all() -> &'static [&'static dyn Rule] {
    RULES
}

pub fn meta(id: &str) -> Option<&'static RuleMeta> {
    RULES.iter().map(|r| r.meta()).find(|m| m.id == id)
}
