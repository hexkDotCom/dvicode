//! SystemVerilog syntax for DVICode: lexer, preprocessor and an error-tolerant parser.

pub mod ast;
pub mod diag;
pub mod lexer;
pub mod parser;
pub mod preproc;
pub mod span;
pub mod token;

pub use diag::{Diag, Severity};
pub use span::{FileId, LineIndex, Span};
pub use token::{TokKind, Token};

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Access to source files for the preprocessor.
pub trait SourceProvider: Sync {
    /// Id of a file path (allocated on first use).
    fn file_id(&self, path: &Path) -> FileId;
    fn path(&self, id: FileId) -> PathBuf;
    fn text(&self, id: FileId) -> Option<Arc<str>>;
    /// Lexed tokens of a file (cached by the implementation).
    fn tokens(&self, id: FileId) -> Option<Arc<Vec<Token>>>;
    fn is_file(&self, path: &Path) -> bool;
}
