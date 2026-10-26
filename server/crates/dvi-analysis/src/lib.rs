//! Semantic analysis for DVICode: project compilation, name resolution and IDE features.

pub mod ide;
pub mod lint;
pub mod resolve;
pub mod select;
pub mod vfs;
pub mod world;

pub use vfs::{Pin, Vfs};
pub use world::{Analysis, DeclRef, ProjectSetup, Unit, UnitId, UnitKind};
