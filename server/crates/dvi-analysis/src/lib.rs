//! Semantic analysis for DVICode: project compilation, name resolution and IDE features.

pub mod ide;
pub mod resolve;
pub mod vfs;
pub mod world;

pub use vfs::{Pin, Vfs};
pub use world::{Analysis, DeclRef, ProjectSetup, Unit, UnitId, UnitKind};
