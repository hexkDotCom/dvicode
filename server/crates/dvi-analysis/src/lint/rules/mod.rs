//! The lint rules, in the order they run.

mod constructor;
mod deprecated;
mod hexk_simple;
mod layout;
mod overrides;
mod randomize;
mod registration;
mod text;
mod util;
mod waiver_checks;

use super::{Rule, RuleMeta};

static RULES: &[&dyn Rule] = &[
    &randomize::RandomizeResultDiscarded,
    &randomize::RandomizeInAssert,
    &registration::RegistrationMismatch,
    &constructor::IncompatibleConstructor,
    &overrides::OverrideSignature,
    &deprecated::DeprecatedApi,
    &hexk_simple::ProgramBlock,
    &hexk_simple::Timescale,
    &hexk_simple::ExternMethod,
    &hexk_simple::IllegalBins,
    &hexk_simple::ExtendsUvmTransaction,
    &hexk_simple::FieldMacrosInComponent,
    &hexk_simple::UvmDoMacros,
    &hexk_simple::PSequencer,
    &hexk_simple::CastAsTask,
    &hexk_simple::RandomizeVoidCast,
    &hexk_simple::DisplayInUvmClass,
    &hexk_simple::SuppressionNotWaiver,
    &text::TrailingWhitespace,
    &layout::AsciiOnly,
    &layout::FinalNewline,
    &layout::LineLength,
    &layout::EndLabel,
    &layout::EndComment,
    &layout::TodoFormat,
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
