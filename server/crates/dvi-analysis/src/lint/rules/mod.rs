//! The lint rules, in the order they run.

mod constructor;
mod coverage;
mod deprecated;
mod formatting;
mod hexk_files;
mod hexk_simple;
mod hexk_uvm;
mod layout;
mod naming;
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
    &hexk_uvm::ClassNotRegistered,
    &hexk_uvm::MissingSuperPhaseCall,
    &hexk_uvm::CreatedWithNew,
    &hexk_uvm::InternalMember,
    &hexk_uvm::DirectReportCall,
    &hexk_uvm::MessageIdGetName,
    &hexk_uvm::ObjectionWithoutDescription,
    &hexk_uvm::DefaultSequence,
    &hexk_uvm::PreDoOverride,
    &hexk_uvm::VirtualOnOverride,
    &hexk_uvm::ConstraintName,
    &hexk_uvm::CreateNameChars,
    &hexk_uvm::CreateNameMatches,
    &hexk_files::CompilationUnitImport,
    &hexk_files::ClassOutsidePackage,
    &hexk_files::IncludeGuard,
    &hexk_files::DelayWithoutUnit,
    &hexk_files::TimeunitFirst,
    &hexk_files::FileExtension,
    &naming::NamingCase,
    &naming::NamingPrefix,
    &coverage::WeightGoal,
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
    &formatting::FormatterDrift,
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
