//! Problems reported by the preprocessor, the parser and the analysis.

use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub span: Span,
    pub severity: Severity,
    /// Check id, e.g. `UNDECLARED_IDENTIFIER`.
    pub code: &'static str,
    pub message: String,
}

impl Diag {
    pub fn error(span: Span, code: &'static str, message: impl Into<String>) -> Diag {
        Diag { span, severity: Severity::Error, code, message: message.into() }
    }

    pub fn warning(span: Span, code: &'static str, message: impl Into<String>) -> Diag {
        Diag { span, severity: Severity::Warning, code, message: message.into() }
    }

    pub fn info(span: Span, code: &'static str, message: impl Into<String>) -> Diag {
        Diag { span, severity: Severity::Info, code, message: message.into() }
    }
}
