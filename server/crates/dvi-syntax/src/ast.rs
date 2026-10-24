//! Declaration-level syntax tree.
//!
//! The parser keeps what an IDE needs: every named declaration with its scope, type and
//! location, and every identifier reference (as a chain like `a.b[i].c` or `pkg::T`) with the
//! scope it appears in. Expressions themselves are not modelled.

use crate::diag::Diag;
use crate::span::{FileId, Span};
use rustc_hash::FxHashMap;
use smol_str::SmolStr;

pub type DeclId = u32;
pub type RefId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeclKind {
    /// The compilation-unit scope (`$unit`) of a root file.
    Unit,
    Module,
    Interface,
    Program,
    Package,
    Class,
    InterfaceClass,
    Checker,
    Primitive,
    Config,
    Function,
    Task,
    Variable,
    Net,
    Port,
    Parameter,
    LocalParam,
    TypeParam,
    Typedef,
    /// Anonymous `struct`/`union` body; children are its fields.
    StructType,
    /// Anonymous `enum` body; children are its members.
    EnumType,
    EnumMember,
    Field,
    Argument,
    Constraint,
    Covergroup,
    Coverpoint,
    Cross,
    Instance,
    /// Generate block (named or `genblkN`).
    Generate,
    /// `begin`/`fork` block; anonymous blocks have an empty name.
    Block,
    Modport,
    ClockingBlock,
    Property,
    Sequence,
    Let,
    Genvar,
}

impl DeclKind {
    pub fn is_scope(self) -> bool {
        matches!(
            self,
            DeclKind::Unit
                | DeclKind::Module
                | DeclKind::Interface
                | DeclKind::Program
                | DeclKind::Package
                | DeclKind::Class
                | DeclKind::InterfaceClass
                | DeclKind::Checker
                | DeclKind::Function
                | DeclKind::Task
                | DeclKind::StructType
                | DeclKind::EnumType
                | DeclKind::Covergroup
                | DeclKind::Generate
                | DeclKind::Block
                | DeclKind::Modport
                | DeclKind::ClockingBlock
                | DeclKind::Property
                | DeclKind::Sequence
                | DeclKind::Constraint
        )
    }

    pub fn is_type(self) -> bool {
        matches!(
            self,
            DeclKind::Class | DeclKind::InterfaceClass | DeclKind::Typedef | DeclKind::TypeParam
                | DeclKind::StructType | DeclKind::EnumType | DeclKind::Covergroup
        )
    }

    /// Design elements live in the definitions name space.
    pub fn is_definition(self) -> bool {
        matches!(
            self,
            DeclKind::Module | DeclKind::Interface | DeclKind::Program | DeclKind::Primitive
                | DeclKind::Checker | DeclKind::Config
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            DeclKind::Unit => "compilation unit",
            DeclKind::Module => "module",
            DeclKind::Interface => "interface",
            DeclKind::Program => "program",
            DeclKind::Package => "package",
            DeclKind::Class => "class",
            DeclKind::InterfaceClass => "interface class",
            DeclKind::Checker => "checker",
            DeclKind::Primitive => "primitive",
            DeclKind::Config => "config",
            DeclKind::Function => "function",
            DeclKind::Task => "task",
            DeclKind::Variable => "variable",
            DeclKind::Net => "net",
            DeclKind::Port => "port",
            DeclKind::Parameter => "parameter",
            DeclKind::LocalParam => "localparam",
            DeclKind::TypeParam => "type parameter",
            DeclKind::Typedef => "typedef",
            DeclKind::StructType => "struct",
            DeclKind::EnumType => "enum",
            DeclKind::EnumMember => "enum item",
            DeclKind::Field => "field",
            DeclKind::Argument => "argument",
            DeclKind::Constraint => "constraint",
            DeclKind::Covergroup => "covergroup",
            DeclKind::Coverpoint => "coverpoint",
            DeclKind::Cross => "cross",
            DeclKind::Instance => "instance",
            DeclKind::Generate => "generate block",
            DeclKind::Block => "block",
            DeclKind::Modport => "modport",
            DeclKind::ClockingBlock => "clocking block",
            DeclKind::Property => "property",
            DeclKind::Sequence => "sequence",
            DeclKind::Let => "let",
            DeclKind::Genvar => "genvar",
        }
    }
}

pub mod dflags {
    pub const STATIC: u32 = 1;
    pub const LOCAL: u32 = 1 << 1;
    pub const PROTECTED: u32 = 1 << 2;
    pub const VIRTUAL: u32 = 1 << 3;
    pub const PURE: u32 = 1 << 4;
    pub const EXTERN: u32 = 1 << 5;
    pub const RAND: u32 = 1 << 6;
    pub const CONST: u32 = 1 << 7;
    pub const AUTOMATIC: u32 = 1 << 8;
    /// The declaration's name comes from a macro body (not visible in the source).
    pub const FROM_MACRO: u32 = 1 << 9;
    /// Function/task prototype without a body.
    pub const PROTOTYPE: u32 = 1 << 10;
    /// `typedef class C;`
    pub const FORWARD: u32 = 1 << 11;
    /// Class method defined outside its class (`function T C::f`).
    pub const OUT_OF_BLOCK: u32 = 1 << 12;
    pub const DPI: u32 = 1 << 13;
    /// The declaration's type is a virtual interface.
    pub const VIRTUAL_IF: u32 = 1 << 14;
    /// Unpacked dimensions after the name: an array, queue or associative array.
    pub const ARRAY: u32 = 1 << 15;
    /// Number of unpacked dimensions, stored in bits 16..20.
    pub const DIMS_SHIFT: u32 = 16;
    /// Instance created by `bind`: a member of the target module, not of the scope where the
    /// `bind` is written (see [`ParsedUnit::binds`](super::ParsedUnit::binds)).
    pub const BOUND: u32 = 1 << 21;
    /// Contains encrypted text (`` `pragma protect ``), which may declare names we cannot see.
    pub const ENCRYPTED: u32 = 1 << 22;
    /// Instance connecting ports by name with `.*`: its connections name nothing in the source.
    pub const DOT_STAR: u32 = 1 << 23;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortDir {
    Input,
    Output,
    Inout,
    Ref,
    ConstRef,
}

impl PortDir {
    pub fn as_str(self) -> &'static str {
        match self {
            PortDir::Input => "input",
            PortDir::Output => "output",
            PortDir::Inout => "inout",
            PortDir::Ref => "ref",
            PortDir::ConstRef => "const ref",
        }
    }
}

/// The declared type of a declaration.
#[derive(Clone, Debug, Default)]
pub struct TypeRef {
    /// Source text of the type, normalized to single spaces (for hover and outline).
    pub text: SmolStr,
    /// Reference chain naming a user type (`pkg::T`, `C#(int)`), if any.
    pub ref_id: Option<RefId>,
    /// Inline `struct`/`enum` body.
    pub inline: Option<DeclId>,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub pkg: SmolStr,
    pub pkg_span: Span,
    /// `None` for `pkg::*`.
    pub item: Option<SmolStr>,
}

#[derive(Clone, Debug)]
pub struct Decl {
    pub kind: DeclKind,
    pub name: SmolStr,
    pub name_span: Span,
    /// Full extent, in the file of the name.
    pub span: Span,
    pub parent: Option<DeclId>,
    pub children: Vec<DeclId>,
    pub ty: Option<TypeRef>,
    pub extends: Option<TypeRef>,
    pub implements: Vec<TypeRef>,
    pub imports: Vec<Import>,
    /// `export pkg::item;` / `export pkg::*;` in a package; `*::*` has pkg "*".
    pub exports: Vec<Import>,
    pub flags: u32,
    pub dir: Option<PortDir>,
    /// Text for hover and outline: return type and arguments, parameter value, ...
    pub detail: SmolStr,
    /// For out-of-block methods: the class scope chain (`C` in `C::f`).
    pub owner: Option<RefId>,
}

impl Decl {
    pub fn has(&self, f: u32) -> bool {
        self.flags & f != 0
    }

    /// Unpacked dimensions after the declared name.
    pub fn unpacked_dims(&self) -> u32 {
        (self.flags >> dflags::DIMS_SHIFT) & 0xF
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sep {
    None,
    /// `.`
    Dot,
    /// `::`
    Scope,
}

#[derive(Clone, Debug)]
pub struct RefSeg {
    pub name: SmolStr,
    pub span: Span,
    pub sep: Sep,
    /// Followed by a call argument list.
    pub called: bool,
    /// Number of indexes or slices that follow (`a[i][j]` has 2).
    pub selects: u8,
    /// Created by a macro body: has no source location of its own.
    pub from_macro: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// An expression operand.
    Expr,
    /// A type name in a declaration.
    Type,
    /// The module/interface/program name of an instantiation.
    Definition,
    /// `.port(...)` in an instance port list; `inst` is the instance declaration.
    Port { inst: DeclId },
    /// `.param(...)` in an instance parameter list.
    Param { inst: DeclId },
    /// A package name in `import`/`export`.
    Package,
    /// The item of an explicit `import pkg::item`.
    ImportItem,
    /// `extends`/`implements` base.
    Base,
    /// Name inside a `with` constraint/iterator clause; `target` is the chain being randomized.
    With { target: RefId },
    /// A name that may be an implicit net or that the parser could not place reliably;
    /// it is resolved for navigation but never reported as undeclared.
    Loose,
    /// A name written in one scope that belongs to another (a `bind`'s ports and parameters,
    /// a `defparam` path): never reported, and not an implicit net of the scope it is in.
    Foreign,
}

#[derive(Clone, Debug)]
pub struct RefChain {
    pub segs: Vec<RefSeg>,
    /// Innermost declaration scope containing the reference.
    pub scope: DeclId,
    pub kind: RefKind,
}

/// Parse result for one compilation root (the root file and everything it includes).
#[derive(Debug, Default)]
pub struct ParsedUnit {
    pub decls: Vec<Decl>,
    pub refs: Vec<RefChain>,
    pub diags: Vec<Diag>,
    /// Members of each scope by name (enum items are hoisted into the enclosing scope).
    pub members: FxHashMap<DeclId, FxHashMap<SmolStr, Vec<DeclId>>>,
    /// Top-level declarations per file, for the outline.
    pub file_tops: FxHashMap<FileId, Vec<DeclId>>,
    /// `bind target ... inst (...)`: the target (a module name, or an instance path such as
    /// `dut.u_core`) and the instance.
    pub binds: Vec<(SmolStr, DeclId)>,
}

impl ParsedUnit {
    pub const ROOT: DeclId = 0;

    pub fn decl(&self, id: DeclId) -> &Decl {
        &self.decls[id as usize]
    }

    pub fn reference(&self, id: RefId) -> &RefChain {
        &self.refs[id as usize]
    }

    pub fn lookup_member(&self, scope: DeclId, name: &str) -> &[DeclId] {
        // By scope, then by name as `&str`: no key is built (names of register fields are longer
        // than a `SmolStr` holds inline, and this runs for every name resolved).
        self.members.get(&scope).and_then(|m| m.get(name)).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Fills [`ParsedUnit::members`] and [`ParsedUnit::file_tops`].
    pub fn build_index(&mut self) {
        let mut members: FxHashMap<DeclId, FxHashMap<SmolStr, Vec<DeclId>>> = FxHashMap::default();
        for (id, d) in self.decls.iter().enumerate() {
            let id = id as DeclId;
            let Some(parent) = d.parent else { continue };
            // `function void C::f()` is a member of C, not of the enclosing package; a bound
            // instance is a member of the bind target.
            if d.has(dflags::OUT_OF_BLOCK) || d.has(dflags::BOUND) {
                continue;
            }
            if !d.name.is_empty() {
                members.entry(parent).or_default().entry(d.name.clone()).or_default().push(id);
            }
            // Enum items are visible in the scope of the enum type declaration.
            if d.kind == DeclKind::EnumMember {
                let mut p = parent;
                while matches!(self.decls[p as usize].kind, DeclKind::EnumType | DeclKind::Typedef) {
                    match self.decls[p as usize].parent {
                        Some(pp) => p = pp,
                        None => break,
                    }
                }
                if p != parent {
                    members.entry(p).or_default().entry(d.name.clone()).or_default().push(id);
                }
            }
        }
        self.members = members;
        let mut tops: FxHashMap<FileId, Vec<DeclId>> = FxHashMap::default();
        for (id, d) in self.decls.iter().enumerate() {
            if id == 0 {
                continue;
            }
            // Top of a file: the parent is in another file (or is the unit).
            let parent_file = d.parent.map(|p| self.decls[p as usize].name_span.file);
            if d.parent == Some(Self::ROOT) || parent_file != Some(d.name_span.file) {
                tops.entry(d.name_span.file).or_default().push(id as DeclId);
            }
        }
        self.file_tops = tops;
    }
}
