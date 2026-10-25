//! The Register Model view: a UVM register model as its `build()` code constructs it — blocks,
//! maps, registers, memories and fields, with offsets, addresses, access and reset values.
//!
//! The construction code is interpreted in order, statement by statement, over its tokens:
//! object creations (`x = T::type_id::create("x")`), `configure`, `create_map`, `add_reg`,
//! `add_mem`, `add_submap`, `set_base_addr`, the block's own helper methods, `super.build()`,
//! sub-blocks' and register files' `build(...)`, with arguments bound by name, position or
//! default. Loops with constant bounds are unrolled. Values are evaluated when they are
//! constant (literals, parameters, the arguments of the call) and shown as written otherwise;
//! nothing unknown is guessed. Addresses follow UVM 1.2's map translation for what is
//! supported: byte or word addressing, registers wider than the bus, submaps at their parent's
//! offset. Anything else is shown without an address, with a note.

use super::stmt::{self, FrameKind};
use super::views::{call_args, skip_group};
use crate::world::{Analysis, DeclRef};
use dvi_syntax::ast::{dflags, DeclKind};
use dvi_syntax::token::{TokKind, Token};
use dvi_syntax::{FileId, Span};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::sync::Arc;

/// Nested calls followed at most.
const MAX_DEPTH: usize = 8;
/// Tokens interpreted at most, over a whole model.
const MAX_WORK: usize = 10_000_000;
/// Objects (registers, fields...) made at most.
const MAX_OBJECTS: usize = 100_000;
/// Iterations of one loop unrolled at most.
const MAX_ITERATIONS: u128 = 4096;

/// A value of the construction code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Num(u128),
    Str(String),
    /// Not constant: as written.
    Unknown(String),
}

impl Val {
    fn num(&self) -> Option<u128> {
        match self {
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn text(&self) -> String {
        match self {
            Val::Num(n) => format!("{n:#x}"),
            Val::Str(s) => format!("{s:?}"),
            Val::Unknown(s) => s.clone(),
        }
    }
}

/// What a handle of the construction code refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Obj {
    Block(usize),
    Reg(usize),
    Map(usize),
    Field(usize),
    /// A register file: a scope of registers, at no offset of its own.
    RegFile(usize),
}

#[derive(Clone, Debug)]
enum Handle {
    Obj(Obj),
    Val(Val),
}

/// A block as built.
#[derive(Clone, Debug)]
pub struct Block {
    pub name: String,
    pub class: DeclRef,
    pub maps: Vec<usize>,
    pub default_map: Option<usize>,
    pub regs: Vec<usize>,
    /// Where it is made (in its parent's build); none for the top.
    pub at: Option<Span>,
    pub notes: Vec<String>,
    vars: FxHashMap<String, Handle>,
}

/// A register or a memory.
#[derive(Clone, Debug)]
pub struct Reg {
    pub name: String,
    pub class: Option<DeclRef>,
    pub memory: bool,
    /// Bits; for a memory the width of a word.
    pub bits: Val,
    /// A memory's words.
    pub size: Option<Val>,
    pub regfile: Option<String>,
    pub at: Span,
    pub notes: Vec<String>,
    /// The arguments its `build` was called with (its fields are found on demand).
    build_args: Option<Vec<(Option<String>, Val)>>,
    built: bool,
}

/// A map as built.
#[derive(Clone, Debug)]
pub struct Map {
    pub name: String,
    pub block: usize,
    pub base: Val,
    pub n_bytes: Val,
    pub byte_addressing: Val,
    /// `UVM_LITTLE_FIFO`/`UVM_BIG_FIFO`: every word of a wide access at the same address.
    pub fifo: bool,
    pub entries: Vec<Entry>,
    /// Mounted in a parent map, at this offset.
    pub parent: Option<(usize, Val)>,
    pub at: Span,
}

#[derive(Clone, Debug)]
pub enum Entry {
    Reg { reg: usize, offset: Val, rights: Option<String>, unmapped: bool, at: Span, notes: Vec<String> },
    Sub { map: usize, offset: Val, at: Span },
}

/// A field as configured.
#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub name: String,
    pub size: Option<u128>,
    pub lsb: Option<u128>,
    pub access: Option<String>,
    pub volatile: Option<bool>,
    /// `None`: no reset value (`has_reset` is 0) or unknown (see notes).
    pub reset: Option<String>,
    pub has_reset: Option<bool>,
    pub notes: Vec<String>,
    #[serde(skip)]
    pub at: Span,
}

/// A register model as its top block builds it.
#[derive(Clone, Debug, Default)]
pub struct Model {
    /// Maps of each block mounted in each map: `(parent map, block)` → count.
    pub mounts: FxHashMap<(usize, usize), usize>,
    pub blocks: Vec<Block>,
    pub regs: Vec<Reg>,
    pub maps: Vec<Map>,
    /// Why the interpretation stopped early: `depth`, `work` or `objects`.
    pub cut: Option<&'static str>,
}

/// One node of the view.
#[derive(Serialize, Debug, Clone, Default)]
pub struct RegNode {
    pub name: String,
    /// `block`, `map`, `register`, `memory`, `field`, `group`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bits: Option<String>,
    /// Fields: `[msb:lsb]`, access, reset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// A register whose fields are fetched on expansion (`dvicode/registerFields`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub register: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub def_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub def_line: Option<u32>,
    /// Always present, empty or not, as in the other views' trees.
    pub children: Vec<RegNode>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// A register or memory at an address.
#[derive(Serialize, Debug, Clone)]
pub struct AddressHit {
    /// Names of the tree's nodes from the top block to the register (the map of a block
    /// mounted in another map is not a node of its own).
    pub path: Vec<String>,
    /// The map the register is added to.
    pub map: String,
    pub address: String,
    /// How the address is in it, when it is not one of its bus words (a byte of a word, a
    /// memory element).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Lexical environment of a method being interpreted.
#[derive(Clone, Default)]
struct Env {
    locals: FxHashMap<String, Handle>,
    /// The declared width of integral locals (`int unsigned off`): values assigned are cut.
    widths: FxHashMap<String, u128>,
}

/// What `this` is in a method being interpreted.
#[derive(Clone, Copy, Debug)]
enum This {
    Block(usize),
    Reg,
    RegFile(usize),
}

struct Interp<'a> {
    a: &'a Analysis,
    m: Model,
    /// Register files: name, members, class.
    regfiles: Vec<(String, FxHashMap<String, Handle>, Option<DeclRef>)>,
    fields: Vec<Field>,
    work: usize,
    /// `lock_model()` reached in the top block's build.
    locked: bool,
    tokens: FxHashMap<FileId, Arc<Vec<Token>>>,
    /// Macros and package constants being evaluated (a bound on recursive definitions).
    expanding: u8,
    /// The members of the register whose `build` is interpreted (`This::Reg`).
    reg_vars: FxHashMap<String, Handle>,
}

/// Names of the UVM register classes, for what a class derives from.
fn ral_kind(name: &str) -> Option<&'static str> {
    Some(match name {
        "uvm_reg_block" => "block",
        "uvm_reg" => "register",
        "uvm_mem" => "memory",
        "uvm_reg_field" => "field",
        "uvm_reg_file" => "regfile",
        "uvm_reg_map" => "map",
        _ => return None,
    })
}

impl Analysis {
    /// What UVM register class `class` derives from: `block`, `register`, `memory`, `field`,
    /// `regfile` or `map`.
    pub fn ral_kind_of(&self, class: DeclRef) -> Option<&'static str> {
        let mut cur = Some(class);
        let mut n = 0;
        while let Some(c) = cur {
            if let Some(k) = ral_kind(&self.decl(c).name) {
                return Some(k);
            }
            n += 1;
            if n > 32 {
                return None;
            }
            cur = self.class_base(c);
        }
        None
    }

    /// The register block classes of the project, and among them the tops: those no other
    /// block holds as a member.
    pub fn register_blocks(&self) -> (Vec<DeclRef>, Vec<DeclRef>) {
        let mut blocks: Vec<DeclRef> = Vec::new();
        let mut seen = FxHashSet::default();
        let mut todo: Vec<String> = vec!["uvm_reg_block".to_string()];
        while let Some(n) = todo.pop() {
            for c in self.global.subclasses.get(n.as_str()).into_iter().flatten() {
                if seen.insert(self.decl_key(*c)) {
                    blocks.push(*c);
                    todo.push(self.decl(*c).name.to_string());
                }
            }
        }
        let mut held = FxHashSet::default();
        for b in &blocks {
            for m in self.scope_members(*b) {
                let d = self.decl(m);
                if d.kind != DeclKind::Variable {
                    continue;
                }
                if let Some(t) = self.type_of(m).and_then(|t| self.follow_typedef(t)) {
                    if self.ral_kind_of(t) == Some("block") {
                        held.insert(self.decl_key(t));
                    }
                }
            }
        }
        let mut tops: Vec<DeclRef> = blocks.iter().copied().filter(|b| !held.contains(&self.decl_key(*b))).collect();
        let by_name = |v: &mut Vec<DeclRef>| v.sort_by(|x, y| self.decl(*x).name.cmp(&self.decl(*y).name));
        by_name(&mut blocks);
        by_name(&mut tops);
        (blocks, tops)
    }

    /// The block class called `name` (the first top when `None` or not found), with the block
    /// classes and the tops.
    pub fn register_top(&self, name: Option<&str>) -> (Option<DeclRef>, Vec<DeclRef>, Vec<DeclRef>) {
        let (blocks, tops) = self.register_blocks();
        let chosen = name.and_then(|n| blocks.iter().copied().find(|b| self.decl(*b).name == n)).or_else(|| tops.first().copied()).or_else(|| blocks.first().copied());
        (chosen, blocks, tops)
    }

    /// The model `top` (a block class) builds.
    pub fn register_model(&self, top: DeclRef) -> Model {
        let mut it = Interp { a: self, m: Model::default(), regfiles: Vec::new(), fields: Vec::new(), work: 0, locked: false, tokens: FxHashMap::default(), expanding: 0, reg_vars: FxHashMap::default() };
        it.m.blocks.push(Block { name: self.decl(top).name.to_string(), class: top, maps: Vec::new(), default_map: None, regs: Vec::new(), at: None, notes: Vec::new(), vars: FxHashMap::default() });
        it.build_block(0, top, &[], &[], 0);
        let mut mounts: FxHashMap<(usize, usize), usize> = FxHashMap::default();
        for (mi, m) in it.m.maps.iter().enumerate() {
            for e in &m.entries {
                if let Entry::Sub { map, .. } = e {
                    *mounts.entry((mi, it.m.maps[*map].block)).or_default() += 1;
                }
            }
        }
        it.m.mounts = mounts;
        it.m
    }

    /// The fields of register `reg` of a model, from its class's `build` called with the
    /// arguments it was given.
    pub fn register_fields(&self, model: &Model, reg: usize) -> Vec<Field> {
        let Some(r) = model.regs.get(reg) else { return Vec::new() };
        let Some(class) = r.class else { return Vec::new() };
        let mut it = Interp { a: self, m: Model::default(), regfiles: Vec::new(), fields: Vec::new(), work: 0, locked: false, tokens: FxHashMap::default(), expanding: 0, reg_vars: FxHashMap::default() };
        it.m.regs.push(Reg { build_args: None, built: false, ..r.clone() });
        // Fields are made by the register's `build()`: none when the model never calls it.
        if !r.built {
            return Vec::new();
        }
        if let Some(build) = it.find_method(class, "build") {
            let args = r.build_args.clone().unwrap_or_default();
            it.exec_method(build, &args, &[], This::Reg, 0);
        }
        let mut fields = std::mem::take(&mut it.fields);
        let bits = r.bits.num();
        // Checks of the layout.
        let mut used: Vec<(u128, u128, usize)> = Vec::new();
        for (i, f) in fields.iter_mut().enumerate() {
            match (f.lsb, f.size) {
                (Some(_), Some(0)) => f.notes.push("zero width".into()),
                (Some(l), Some(s)) => match l.checked_add(s) {
                    Some(end) => {
                        if bits.is_some_and(|b| end > b) {
                            f.notes.push("beyond the register's width".into());
                        }
                        used.push((l, end, i));
                    }
                    None => f.notes.push("beyond the register's width".into()),
                },
                _ => {}
            }
        }
        used.sort();
        // Every pair that overlaps: each field against the ones still open where it starts.
        let mut overlaps: Vec<usize> = Vec::new();
        let mut open: Vec<(u128, usize)> = Vec::new();
        for &(lo, hi, i) in &used {
            open.retain(|(end, _)| *end > lo);
            for &(_, j) in &open {
                overlaps.push(i);
                overlaps.push(j);
            }
            open.push((hi, i));
        }
        for i in overlaps {
            if !fields[i].notes.iter().any(|n| n == "overlaps another field") {
                fields[i].notes.push("overlaps another field".into());
            }
        }
        fields
    }

    /// The registers and memories at `addr` in the maps the view shows (the top block's):
    /// those with a bus word at that very address, else those with a word that holds that byte
    /// (byte addressing), and memories whose range holds it.
    pub fn registers_at(&self, model: &Model, addr: u128) -> Vec<AddressHit> {
        let mut exact = Vec::new();
        let mut within = Vec::new();
        let Some(top) = model.blocks.first() else { return Vec::new() };
        for &mi in &top.maps {
            let m = &model.maps[mi];
            walk_addresses(model, mi, &[top.name.clone(), m.name.clone()], 0, &mut |path, map, ri, offset| {
                let r = &model.regs[ri];
                let Some(a) = entry_addresses(model, map, ri, offset) else { return };
                let Some(base) = model.maps[a.top].base.num() else { return };
                let at = |x: u128| base.checked_add(x);
                let (Some(lo), Some(hi)) = (at(a.lo), at(a.hi)) else { return };
                let hit = |note: Option<String>| AddressHit { path: path.to_vec(), map: model.maps[map].name.clone(), address: range_text(lo, hi), note };
                if r.memory {
                    let last = a.byte_width.and_then(|bw| hi.checked_add(bw.saturating_sub(1))).unwrap_or(hi);
                    if (lo..=last).contains(&addr) {
                        exact.push(hit(Some("in this memory".into())));
                    }
                    return;
                }
                if a.words.iter().any(|w| at(*w) == Some(addr)) {
                    exact.push(hit(None));
                } else if let Some(bw) = a.byte_width {
                    if let Some(w) = a.words.iter().filter_map(|w| at(*w)).find(|w| *w < addr && addr - *w < bw) {
                        within.push(hit(Some(format!("byte {} of the word at {w:#x}", addr - w))));
                    }
                }
            });
        }
        if exact.is_empty() {
            within
        } else {
            exact
        }
    }

    /// The view's nodes for a model: the top block, its maps with their entries (sub-blocks
    /// at their submaps), and its registers in no map.
    pub fn register_tree(&self, model: &Model, max_nodes: usize) -> (RegNode, bool) {
        let mut left = max_nodes;
        let mut truncated = false;
        let top = self.block_node(model, 0, &mut left, &mut truncated, 0);
        (top, truncated)
    }

    fn block_node(&self, model: &Model, bi: usize, left: &mut usize, truncated: &mut bool, depth: usize) -> RegNode {
        let b = &model.blocks[bi];
        let mut n = self.node_at(b.name.clone(), "block", b.at, Some(b.class));
        n.notes = b.notes.clone();
        if bi == 0 {
            if let Some(cut) = model.cut {
                n.notes.push(format!("construction followed only in part ({cut} limit)"));
            }
        }
        if depth > 16 {
            return n;
        }
        for &mi in &b.maps {
            // A map mounted in a parent shows there.
            if model.maps[mi].parent.is_some() && bi != 0 {
                continue;
            }
            n.children.push(self.map_node(model, mi, left, truncated, depth));
        }
        let mapped: FxHashSet<usize> = model.maps.iter().flat_map(|m| m.entries.iter()).filter_map(|e| if let Entry::Reg { reg, .. } = e { Some(*reg) } else { None }).collect();
        let loose: Vec<usize> = b.regs.iter().copied().filter(|r| !mapped.contains(r)).collect();
        if !loose.is_empty() {
            let mut g = RegNode { name: "not in a map".into(), kind: "group", ..Default::default() };
            for r in loose {
                if *left == 0 {
                    *truncated = true;
                    g.truncated = true;
                    break;
                }
                *left -= 1;
                g.children.push(self.reg_node(model, r, None, None, None));
            }
            n.children.push(g);
        }
        n
    }

    fn map_node(&self, model: &Model, mi: usize, left: &mut usize, truncated: &mut bool, depth: usize) -> RegNode {
        let m = &model.maps[mi];
        let mut n = self.node_at(m.name.clone(), "map", Some(m.at), None);
        n.address = start_of(model, mi).map(|(a, top)| address_text(model, top, a, a));
        n.bits = m.n_bytes.num().map(|b| format!("{} bytes", b));
        if m.parent.is_none() && m.base.num().is_none() {
            n.notes.push(format!("base {} not constant: addresses relative to it", m.base.text()));
        }
        if m.byte_addressing.num() == Some(0) {
            n.notes.push("word addressing".into());
        }
        let mut entries: Vec<(Option<u128>, RegNode)> = Vec::new();
        for e in &m.entries {
            if *left == 0 {
                *truncated = true;
                n.truncated = true;
                break;
            }
            *left -= 1;
            match e {
                Entry::Reg { reg, offset, rights, unmapped, at, notes } => {
                    let range = if *unmapped { None } else { entry_addresses(model, mi, *reg, offset) };
                    let mut rn = self.reg_node(model, *reg, Some(offset), rights.clone(), None);
                    rn.address = range.as_ref().map(|a| address_text(model, a.top, a.lo, a.hi));
                    rn.notes.extend(notes.iter().cloned());
                    if *unmapped {
                        rn.notes.push("unmapped (reached by a front door only)".into());
                    }
                    let (uri, line, character) = self.where_of(*at);
                    rn.uri = uri;
                    rn.line = line;
                    rn.character = character;
                    entries.push((range.map(|a| a.lo), rn));
                }
                Entry::Sub { map, offset, at } => {
                    let start = start_of(model, *map);
                    let mut bn = self.block_node_mounted(model, model.maps[*map].block, *map, left, truncated, depth + 1);
                    bn.name = mounted_name(model, mi, *map);
                    bn.offset = Some(offset.text());
                    bn.address = start.map(|(a, top)| address_text(model, top, a, a));
                    let (uri, line, character) = self.where_of(*at);
                    bn.uri = uri;
                    bn.line = line;
                    bn.character = character;
                    entries.push((start.map(|(a, _)| a), bn));
                }
            }
        }
        // By address where known, in source order otherwise.
        entries.sort_by_key(|(a, _)| a.unwrap_or(u128::MAX));
        n.children = entries.into_iter().map(|(_, e)| e).collect();
        n
    }

    /// A sub-block shown at a submap: its mounted map's entries.
    fn block_node_mounted(&self, model: &Model, bi: usize, mounted: usize, left: &mut usize, truncated: &mut bool, depth: usize) -> RegNode {
        let b = &model.blocks[bi];
        let mut n = self.node_at(b.name.clone(), "block", b.at, Some(b.class));
        n.notes = b.notes.clone();
        if depth > 16 {
            return n;
        }
        let inner = self.map_node(model, mounted, left, truncated, depth);
        n.children = inner.children;
        n.notes.extend(inner.notes.into_iter().filter(|x| x == "word addressing"));
        if inner.truncated {
            n.truncated = true;
        }
        n
    }

    fn reg_node(&self, model: &Model, ri: usize, offset: Option<&Val>, rights: Option<String>, range: Option<(u128, u128)>) -> RegNode {
        let r = &model.regs[ri];
        let mut n = self.node_at(r.name.clone(), if r.memory { "memory" } else { "register" }, Some(r.at), r.class);
        n.offset = offset.map(|o| o.text());
        n.address = range.map(|(lo, hi)| range_text(lo, hi));
        n.rights = rights;
        n.bits = Some(match (&r.bits, &r.size) {
            (b, Some(s)) => format!("{} x {} bits", s.num().map_or_else(|| s.text(), |v| v.to_string()), b.num().map_or_else(|| b.text(), |v| v.to_string())),
            (b, None) => format!("{} bits", b.num().map_or_else(|| b.text(), |v| v.to_string())),
        });
        n.notes = r.notes.clone();
        if let Some(rf) = &r.regfile {
            n.notes.push(format!("in register file {rf}"));
        }
        if offset.is_some_and(|o| o.num().is_none()) {
            n.notes.push("offset not constant".into());
        }
        if !r.memory && r.class.is_some() {
            n.register = Some(ri);
        }
        n
    }

    /// A node with where it is made (`at`) and its class's declaration.
    fn node_at(&self, name: String, kind: &'static str, at: Option<Span>, class: Option<DeclRef>) -> RegNode {
        let mut n = RegNode { name, kind, ..Default::default() };
        if let Some(at) = at {
            let (uri, line, character) = self.where_of(at);
            n.uri = uri;
            n.line = line;
            n.character = character;
        }
        if let Some(c) = class {
            let d = self.decl(c);
            n.class = Some(d.name.to_string());
            if let Some(r) = self.to_range(d.name_span) {
                n.def_uri = Some(crate::ide::url_of(&self.path(d.name_span.file)).to_string());
                n.def_line = Some(r.start.line);
            }
        }
        n
    }

    fn where_of(&self, at: Span) -> (Option<String>, Option<u32>, Option<u32>) {
        match self.to_range(at) {
            Some(r) => (Some(crate::ide::url_of(&self.path(at.file)).to_string()), Some(r.start.line), Some(r.start.character)),
            None => (None, None, None),
        }
    }
}

/// Addresses computed at most for one access (a chain of maps of falling widths multiplies
/// them).
const MAX_ADDRESSES: usize = 4096;

/// The addresses UVM 1.2 gives to `n_bytes` bytes at local address `base`, element
/// `mem_offset`, of map `mi` (`uvm_reg_map::get_physical_addresses`): one address per bus
/// word, `n_bytes` apart with byte addressing and 1 apart without (all the same in a FIFO
/// map), translated through the parent maps (a child address scaled by the child's width in
/// parent words, at its submap's offset). The addresses are in the top map's space, without
/// its base; the top map comes with them. `left`: addresses that may still be made.
fn physical(model: &Model, mi: usize, base: u128, mem_offset: u128, n_bytes: u128, depth: usize, left: &mut usize) -> Option<(Vec<u128>, usize)> {
    let m = &model.maps[mi];
    let bw = m.n_bytes.num().filter(|b| *b > 0)?;
    let mult = if m.byte_addressing.num()? != 0 { bw } else { 1 };
    let local: Vec<u128> = if n_bytes <= bw {
        vec![base.checked_add(mem_offset.checked_mul(mult)?)?]
    } else {
        let n = (n_bytes - 1) / bw + 1;
        if n > *left as u128 {
            return None;
        }
        let first = base.checked_add(mem_offset.checked_mul(n.checked_mul(mult)?)?)?;
        let step = if m.fifo { 0 } else { mult };
        (0..n).map(|i| first.checked_add(i * step)).collect::<Option<_>>()?
    };
    *left = left.checked_sub(local.len())?;
    let Some((p, offset)) = &m.parent else { return Some((local, mi)) };
    if depth > 16 {
        return None;
    }
    let pbw = model.maps[*p].n_bytes.num().filter(|b| *b > 0)?;
    let k = if bw < pbw { 1 } else { (bw - 1) / pbw + 1 };
    let offset = offset.num()?;
    let mut out = Vec::new();
    let mut top = mi;
    for l in local {
        let (a, t) = physical(model, *p, offset.checked_add(l.checked_mul(k)?)?, 0, bw, depth + 1, left)?;
        out.extend(a);
        top = t;
    }
    Some((out, top))
}

/// Where local address 0 of map `mi` is in its top map, and the top map.
fn start_of(model: &Model, mi: usize) -> Option<(u128, usize)> {
    let (a, top) = physical(model, mi, 0, 0, 1, 0, &mut MAX_ADDRESSES.clone())?;
    Some((*a.first()?, top))
}

/// The addresses of a register or memory in its top map (without the top map's base).
struct Addresses {
    /// A register's bus words (a memory's first element's).
    words: Vec<u128>,
    /// First and last address (a memory: of its first and last element).
    lo: u128,
    hi: u128,
    top: usize,
    /// The top map's bus width when it addresses bytes (a word then holds that many addresses).
    byte_width: Option<u128>,
}

/// The addresses of register `ri` at `offset` in map `mi`. A register of unknown width shows
/// its first address; so does a memory of unknown size.
fn entry_addresses(model: &Model, mi: usize, ri: usize, offset: &Val) -> Option<Addresses> {
    let r = &model.regs[ri];
    let off = offset.num()?;
    let n_bytes = r.bits.num().map_or(1, |b| b.div_ceil(8).max(1));
    let (words, top) = physical(model, mi, off, 0, n_bytes, 0, &mut MAX_ADDRESSES.clone())?;
    let last = match r.size.as_ref().and_then(|s| s.num()) {
        Some(size) if r.memory && size > 1 => physical(model, mi, off, size - 1, n_bytes, 0, &mut MAX_ADDRESSES.clone())?.0,
        _ => words.clone(),
    };
    let t = &model.maps[top];
    let byte_width = if t.byte_addressing.num() == Some(0) { None } else { t.n_bytes.num().filter(|b| *b > 1) };
    Some(Addresses { lo: *words.iter().min()?, hi: *last.iter().max()?, words, top, byte_width })
}

/// An address range as shown: with the top map's base, or after its name when it is not
/// constant (`base_addr + 0x10`).
fn address_text(model: &Model, top: usize, lo: u128, hi: u128) -> String {
    let base = &model.maps[top].base;
    match base.num() {
        Some(b) => match (b.checked_add(lo), b.checked_add(hi)) {
            (Some(lo), Some(hi)) => range_text(lo, hi),
            _ => format!("{} + {}", base.text(), range_text(lo, hi)),
        },
        None => format!("{} + {}", base.text(), range_text(lo, hi)),
    }
}

/// The name of the node of a block mounted in map `parent` through its map `child`: the
/// block's name, with the map's when another map of the same block is mounted there too.
fn mounted_name(model: &Model, parent: usize, child: usize) -> String {
    let block = model.maps[child].block;
    let name = &model.blocks[block].name;
    let same = model.mounts.get(&(parent, block)).copied().unwrap_or(0);
    if same > 1 {
        format!("{name} ({})", model.maps[child].name)
    } else {
        name.clone()
    }
}

/// Calls `f(path, map, register, offset)` for each mapped register and memory of map `mi`
/// and the maps mounted in it; `path` names the tree's nodes down to the register.
fn walk_addresses(model: &Model, mi: usize, path: &[String], depth: usize, f: &mut dyn FnMut(&[String], usize, usize, &Val)) {
    let m = &model.maps[mi];
    if depth > 16 {
        return;
    }
    for e in &m.entries {
        match e {
            Entry::Reg { reg, offset, unmapped: false, .. } => {
                let mut p = path.to_vec();
                p.push(model.regs[*reg].name.clone());
                f(&p, mi, *reg, offset);
            }
            Entry::Sub { map, .. } => {
                // The tree shows a mounted block's map as the block itself.
                let mut p = path.to_vec();
                p.push(mounted_name(model, mi, *map));
                walk_addresses(model, *map, &p, depth + 1, f);
            }
            _ => {}
        }
    }
}

fn range_text(lo: u128, hi: u128) -> String {
    if lo == hi {
        format!("{lo:#x}")
    } else {
        format!("{lo:#x}–{hi:#x}")
    }
}

/// Argument names of the UVM calls the interpreter knows, in order.
const CREATE_MAP: &[&str] = &["name", "base_addr", "n_bytes", "endian", "byte_addressing"];
const ADD_REG: &[&str] = &["rg", "offset", "rights", "unmapped", "frontdoor"];
const ADD_MEM: &[&str] = &["mem", "offset", "rights", "unmapped", "frontdoor"];
const ADD_SUBMAP: &[&str] = &["child_map", "offset"];
const FIELD_CONFIGURE: &[&str] = &["parent", "size", "lsb_pos", "access", "volatile", "reset", "has_reset", "is_rand", "individually_accessible"];
const REG_CONFIGURE: &[&str] = &["blk_parent", "regfile_parent", "hdl_path"];

impl<'a> Interp<'a> {
    fn spend(&mut self, n: usize) -> bool {
        self.work += n;
        if self.work > MAX_WORK {
            self.m.cut.get_or_insert("work");
            return false;
        }
        true
    }

    fn objects(&self) -> usize {
        self.m.regs.len() + self.m.blocks.len() + self.m.maps.len() + self.fields.len()
    }

    /// The nearest definition of method `name` for an object of class `class` (its body, for
    /// a prototype).
    fn find_method(&self, class: DeclRef, name: &str) -> Option<DeclRef> {
        let mut cur = Some(class);
        let mut n = 0;
        while let Some(c) = cur {
            if ral_kind(&self.a.decl(c).name).is_some() {
                return None;
            }
            for m in self.a.scope_members(c) {
                let d = self.a.decl(m);
                if d.name == name && matches!(d.kind, DeclKind::Function | DeclKind::Task) {
                    return if d.has(dflags::PROTOTYPE) { self.a.implementation_of(m) } else { Some(m) };
                }
            }
            n += 1;
            if n > 32 {
                return None;
            }
            cur = self.a.class_base(c);
        }
        None
    }

    fn build_block(&mut self, bi: usize, class: DeclRef, args: &[(Option<String>, Val)], handles: &[(Option<String>, Option<Obj>)], depth: usize) {
        // `build` (reggen, ralgen) or `build_phase`.
        let Some(build) = self.find_method(class, "build").or_else(|| self.find_method(class, "build_phase")) else {
            self.m.blocks[bi].notes.push("no build method found".into());
            return;
        };
        self.exec_method(build, args, handles, This::Block(bi), depth);
    }

    /// The tokens of a method's body that are compiled, from its header's end.
    fn body(&mut self, m: DeclRef) -> Vec<Token> {
        let d = self.a.decl(m);
        let file = d.span.file;
        let toks = match self.tokens.get(&file) {
            Some(t) => t.clone(),
            None => {
                let Some(t) = self.a.file_tokens(file) else { return Vec::new() };
                self.tokens.insert(file, t.clone());
                t
            }
        };
        let lo = toks.partition_point(|t| t.span.start < d.span.start);
        let hi = toks.partition_point(|t| t.span.start < d.span.end);
        let off: Vec<Span> = self.a.unit(m.unit).inactive.iter().filter(|s| s.file == file && s.end > d.span.start && s.start < d.span.end).copied().collect();
        toks[lo..hi].iter().filter(|t| !off.iter().any(|s| s.start <= t.span.start && t.span.start < s.end)).cloned().collect()
    }

    /// Interprets method `m` with `this`, its arguments bound from `args` (values) and
    /// `handles` (the objects some of them name).
    fn exec_method(&mut self, m: DeclRef, args: &[(Option<String>, Val)], handles: &[(Option<String>, Option<Obj>)], this: This, depth: usize) {
        if depth > MAX_DEPTH {
            self.m.cut.get_or_insert("depth");
            return;
        }
        // Its statements count as they are interpreted; the call itself once.
        if !self.spend(1) {
            return;
        }
        let t = self.body(m);
        let mut env = Env::default();
        // Formals: by name, by position, else their default.
        let u = self.a.unit(m.unit);
        let formals: Vec<(String, String)> = u.parsed.decl(m.decl).children.iter().map(|c| u.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Argument).map(|c| (c.name.to_string(), c.detail.to_string())).collect();
        let mut pos = args.iter().enumerate().filter(|(_, (n, _))| n.is_none()).map(|(i, _)| i);
        for (name, default) in &formals {
            let j = args.iter().position(|(n, _)| n.as_deref() == Some(name.as_str())).or_else(|| pos.next());
            let h = match j {
                Some(j) => match handles.get(j).and_then(|h| h.1) {
                    Some(o) => Handle::Obj(o),
                    None => Handle::Val(args[j].1.clone()),
                },
                None if !default.is_empty() => Handle::Val(self.eval_text(default, m, &env, this)),
                None => Handle::Val(Val::Unknown(name.clone())),
            };
            env.locals.insert(name.clone(), h);
        }
        // The header ends at its first `;` outside parentheses.
        let Some(kw) = t.iter().position(|x| x.is_kw("function") || x.is_kw("task")) else { return };
        let mut k = kw;
        while k < t.len() && !t[k].is_punct(";") {
            k = if t[k].is_punct("(") { skip_group(&t, k) } else { k + 1 };
        }
        let frames = stmt::walk_method(&t);
        self.exec_range(&t, k + 1, t.len(), m, &mut env, this, depth, frames.as_ref());
    }

    /// Interprets the statements of `t[a..b]`.
    #[allow(clippy::too_many_arguments)]
    fn exec_range(&mut self, t: &[Token], a: usize, b: usize, m: DeclRef, env: &mut Env, this: This, depth: usize, frames: Option<&stmt::StmtMap>) {
        let mut k = a;
        while k < b {
            if self.locked || self.m.cut.is_some() {
                return;
            }
            let x = &t[k];
            // Loops: unrolled when their bounds are constant.
            if (x.is_kw("for") || x.is_kw("foreach")) && t.get(k + 1).is_some_and(|p| p.is_punct("(")) {
                let hdr_end = skip_group(t, k + 1);
                // An unfinished header (while typing): nothing more to interpret.
                if hdr_end > b || hdr_end < k + 3 || !t.get(hdr_end - 1).is_some_and(|c| c.is_punct(")")) {
                    return;
                }
                let body_end = statement_end(t, hdr_end, b);
                self.exec_loop(t, k, hdr_end, body_end, m, env, this, depth, frames);
                k = body_end;
                continue;
            }
            // Control words around statements: both branches of a condition are interpreted.
            if !self.spend(1) {
                return;
            }
            if x.is_kw("if") || x.is_kw("while") || x.is_kw("case") || x.is_kw("casex") || x.is_kw("casez") || x.is_kw("repeat") {
                k = if t.get(k + 1).is_some_and(|p| p.is_punct("(")) { skip_group(t, k + 1) } else { k + 1 };
                continue;
            }
            if x.kind == TokKind::Keyword && matches!(x.text.as_str(), "begin" | "end" | "else" | "fork" | "join" | "join_any" | "join_none" | "endcase" | "default" | "endfunction" | "endtask") {
                k += 1;
                if t.get(k).is_some_and(|c| c.is_punct(":")) {
                    k += 1;
                    // `end : label`
                    if t.get(k).is_some_and(|l| l.is_ident()) && !t.get(k + 1).is_some_and(|p| p.is_punct("=") || p.is_punct("(") || p.is_punct(".")) {
                        k += 1;
                    }
                }
                continue;
            }
            let end = statement_end(t, k, b);
            // Every statement interpreted counts (an unrolled loop's again each time).
            if !self.spend(end.saturating_sub(k).max(1)) {
                return;
            }
            // Under a condition: by the statement map, or (when the map is cut short by code
            // being typed) right after an `if (...)`, an `else` or a case label.
            let conditional = frames.is_some_and(|f| in_condition(f, k)) || k.checked_sub(1).is_some_and(|p| t[p].is_punct(")") || t[p].is_kw("else") || t[p].is_punct(":"));
            self.exec_statement(&t[k..end.min(t.len())], m, env, this, depth, conditional);
            k = end.max(k + 1);
        }
    }

    /// `for (int i = A; i < B; i++) body` and `foreach (arr[i]) body`: the body once per value
    /// of the loop variable when the bounds are constant; once, marked, otherwise.
    #[allow(clippy::too_many_arguments)]
    fn exec_loop(&mut self, t: &[Token], kw: usize, hdr_end: usize, body_end: usize, m: DeclRef, env: &mut Env, this: This, depth: usize, frames: Option<&stmt::StmtMap>) {
        let header = &t[kw + 2..hdr_end.saturating_sub(1)];
        let values: Option<(String, Vec<u128>)> = if t[kw].is_kw("for") { self.for_values(header, m, env, this) } else { self.foreach_values(header, m, env, this) };
        match values {
            Some((var, vals)) => {
                let saved = env.locals.get(&var).cloned();
                for v in vals {
                    if !self.spend(1) {
                        break;
                    }
                    env.locals.insert(var.clone(), Handle::Val(Val::Num(v)));
                    self.exec_range(t, hdr_end, body_end, m, env, this, depth, frames);
                    if self.m.cut.is_some() {
                        break;
                    }
                }
                match saved {
                    Some(s) => env.locals.insert(var, s),
                    None => env.locals.remove(&var),
                };
            }
            None => {
                // Once, with what the loop makes marked.
                let regs_before = self.m.regs.len();
                self.exec_range(t, hdr_end, body_end, m, env, this, depth, frames);
                for r in &mut self.m.regs[regs_before..] {
                    r.notes.push("made in a loop whose bounds are not constant".into());
                    if !r.name.ends_with("[*]") {
                        r.name.push_str("[*]");
                    }
                }
            }
        }
    }

    /// `int i = A; i < B; i++` and its other plain forms (`<=`, `>`, `>=`, `!=`; `i++`, `++i`,
    /// `i--`, `--i`, `i += C`, `i -= C`, `i = i + C`, `i = i - C`): the variable and its values.
    /// Any other condition or step: not unrolled.
    fn for_values(&mut self, header: &[Token], m: DeclRef, env: &Env, this: This) -> Option<(String, Vec<u128>)> {
        let parts: Vec<&[Token]> = header.split(|x| x.is_punct(";")).collect();
        let [init, cond, step] = parts[..] else { return None };
        let eq = init.iter().position(|x| x.is_punct("="))?;
        let var = init[..eq].last().filter(|x| x.is_ident())?.text.to_string();
        let start = i128::try_from(self.eval(&init[eq + 1..], m, env, this).num()?).ok()?;
        // The condition: `var OP limit`.
        let [v, op, limit @ ..] = cond else { return None };
        if v.text != var.as_str() || limit.is_empty() {
            return None;
        }
        let limit = i128::try_from(self.eval(limit, m, env, this).num()?).ok()?;
        // The step.
        let is_var = |x: &Token| x.text == var.as_str();
        let delta: i128 = match step {
            [a, b] if is_var(a) && b.is_punct("++") || a.is_punct("++") && is_var(b) => 1,
            [a, b] if is_var(a) && b.is_punct("--") || a.is_punct("--") && is_var(b) => -1,
            // The step is one constant token: `i = i - 2 + 1` is not `i - (2 + 1)`.
            [a, o, c] if is_var(a) && (o.is_punct("+=") || o.is_punct("-=")) => {
                let c = i128::try_from(self.eval(std::slice::from_ref(c), m, env, this).num()?).ok()?;
                if o.is_punct("+=") { c } else { c.checked_neg()? }
            }
            [a, e, b, o, c] if is_var(a) && e.is_punct("=") && is_var(b) && (o.is_punct("+") || o.is_punct("-")) => {
                let c = i128::try_from(self.eval(std::slice::from_ref(c), m, env, this).num()?).ok()?;
                if o.is_punct("+") { c } else { c.checked_neg()? }
            }
            _ => return None,
        };
        if delta == 0 {
            return None;
        }
        let holds = |i: i128| match op.text.as_str() {
            "<" => i < limit,
            "<=" => i <= limit,
            ">" => i > limit,
            ">=" => i >= limit,
            "!=" => i != limit,
            _ => false,
        };
        if !matches!(op.text.as_str(), "<" | "<=" | ">" | ">=" | "!=") {
            return None;
        }
        let mut out = Vec::new();
        let mut i = start;
        while holds(i) {
            if i < 0 || out.len() as u128 >= MAX_ITERATIONS {
                return None;
            }
            out.push(i as u128);
            i = i.checked_add(delta)?;
        }
        Some((var, out))
    }

    /// `arr[i]` of a member array: `i` and the array's indexes, from its declared dimension
    /// (`[4]`: 0 to 3; `[4:7]`: 4 to 7; `[7:4]`: 7 down to 4).
    fn foreach_values(&mut self, header: &[Token], m: DeclRef, env: &Env, this: This) -> Option<(String, Vec<u128>)> {
        let open = header.iter().position(|x| x.is_punct("["))?;
        let var = header.get(open + 1).filter(|x| x.is_ident())?.text.to_string();
        if !header.get(open + 2).is_some_and(|x| x.is_punct("]")) {
            return None;
        }
        let name = header[..open].last().filter(|x| x.is_ident())?.text.to_string();
        let own = open == 1 || open == 3 && header[0].is_kw("this") && header[1].is_punct(".");
        if own {
            if let Some(vals) = self.declared_indexes(&name, m, env, this) {
                return (vals.len() as u128 <= MAX_ITERATIONS).then_some((var, vals));
            }
        }
        // Otherwise the analysis' loop counting (a size only).
        let toks: &dyn Fn(FileId) -> Option<Arc<Vec<Token>>> = &|f| self.a.file_tokens(f);
        let n = self.a.loop_count("foreach", header, m.unit, toks)?;
        if n as u128 > MAX_ITERATIONS {
            return None;
        }
        Some((var, (0..n as u128).collect()))
    }

    /// The indexes of the first unpacked dimension of member `name` of the method's class.
    fn declared_indexes(&mut self, name: &str, m: DeclRef, env: &Env, this: This) -> Option<Vec<u128>> {
        let class = self.class_of_method(m)?;
        let member = self.a.members_list(class).into_iter().find(|d| self.a.decl(*d).name == name)?;
        let span = self.a.decl(member).name_span;
        let toks = self.a.file_tokens(span.file)?;
        let k = toks.partition_point(|t| t.span.start < span.start);
        if !toks.get(k).is_some_and(|t| t.span == span) || !toks.get(k + 1).is_some_and(|t| t.is_punct("[")) {
            return None;
        }
        let close = skip_group(&toks, k + 1);
        if close < k + 3 || !toks.get(close - 1).is_some_and(|t| t.is_punct("]")) {
            return None;
        }
        let inner = &toks[k + 2..close - 1];
        let range = match top_level(inner, ":") {
            Some(c) => {
                let l = self.eval(&inner[..c], m, env, this).num()?;
                let r = self.eval(&inner[c + 1..], m, env, this).num()?;
                if l.abs_diff(r) >= MAX_ITERATIONS {
                    return None;
                }
                if l <= r { (l..=r).collect() } else { (r..=l).rev().collect() }
            }
            None => {
                let n = self.eval(inner, m, env, this).num()?;
                if n > MAX_ITERATIONS {
                    return None;
                }
                (0..n).collect()
            }
        };
        Some(range)
    }

    /// One statement (without its `;`).
    fn exec_statement(&mut self, s: &[Token], m: DeclRef, env: &mut Env, this: This, depth: usize, conditional: bool) {
        let s = strip_semicolon(s);
        if s.is_empty() || self.objects() > MAX_OBJECTS {
            if self.objects() > MAX_OBJECTS {
                self.m.cut.get_or_insert("objects");
            }
            return;
        }
        // A declaration with a value: `int base = 'h100;`, `uvm_reg_map m = create_map(...)`.
        if let Some(eq) = top_level(s, "=") {
            let lhs = &s[..eq];
            let rhs = &s[eq + 1..];
            if let (Some(w), Some(name)) = (declared_width(lhs), lhs.last().filter(|x| x.is_ident())) {
                env.widths.insert(name.text.to_string(), w);
            }
            let target = assigned_name(lhs, self, m, env, this);
            if let Some(target) = target {
                self.assign(&target, rhs, m, env, this, depth, conditional, s[0].span);
            }
            return;
        }
        // A declaration without a value (`uvm_reg_map m;`, `int unsigned i, j;`): locals.
        let decl_tok = |x: &Token| matches!(x.kind, TokKind::Ident | TokKind::Keyword | TokKind::Number) || ["[", "]", ":", ",", "::"].iter().any(|p| x.is_punct(p));
        if s.len() >= 2 && s.iter().all(decl_tok) && s[0].kind != TokKind::Number && s.last().is_some_and(|x| x.is_ident()) {
            let width = declared_width(s);
            for (i, x) in s.iter().enumerate() {
                let next = s.get(i + 1);
                if i > 0 && x.is_ident() && next.is_none_or(|n| n.is_punct(",") || n.is_punct("[")) {
                    env.locals.insert(x.text.to_string(), Handle::Val(Val::Unknown(x.text.to_string())));
                    if let Some(w) = width {
                        env.widths.insert(x.text.to_string(), w);
                    }
                }
            }
            return;
        }
        // A call: `recv.method(args)`, `method(args)`, `super.method(args)`.
        let Some(open) = s.iter().position(|x| x.is_punct("(")) else { return };
        if open == 0 || !s.last().is_some_and(|x| x.is_punct(")")) {
            return;
        }
        let name_tok = &s[open - 1];
        if !(name_tok.is_ident() || name_tok.is_kw("new")) {
            return;
        }
        let method = name_tok.text.to_string();
        let recv = &s[..open - 1];
        let recv = if recv.last().is_some_and(|x| x.is_punct(".")) { &recv[..recv.len() - 1] } else { recv };
        let args = self.args(s, open, m, env, this);
        let handles = self.arg_handles(s, open, m, env, this);
        self.call(recv, &method, &args, &handles, m, env, this, depth, conditional, s[0].span);
    }

    /// Arguments of the call whose `(` is `s[open]`: name (when given by name) and value.
    fn args(&mut self, s: &[Token], open: usize, m: DeclRef, env: &Env, this: This) -> Vec<(Option<String>, Val)> {
        call_args(s, open)
            .into_iter()
            .map(|a| match a {
                [dot, n, o, inner @ .., c] if dot.is_punct(".") && o.is_punct("(") && c.is_punct(")") => (Some(n.text.to_string()), self.eval(inner, m, env, this)),
                _ => (None, self.eval(a, m, env, this)),
            })
            .collect()
    }

    /// The objects the arguments of a call name (`add_reg(.rg(r_a), ...)`), in order.
    fn arg_handles(&mut self, s: &[Token], open: usize, _m: DeclRef, env: &Env, this: This) -> Vec<(Option<String>, Option<Obj>)> {
        call_args(s, open)
            .into_iter()
            .map(|a| match a {
                [dot, n, o, inner @ .., c] if dot.is_punct(".") && o.is_punct("(") && c.is_punct(")") => (Some(n.text.to_string()), self.object(inner, env, this)),
                _ => (None, self.object(a, env, this)),
            })
            .collect()
    }

    /// The object a path names (`r_a`, `this.r_a`, `rf.r[2]`, `sub.default_map`).
    fn object(&mut self, t: &[Token], env: &Env, this: This) -> Option<Obj> {
        let t = strip_parens(t);
        let path = self.path_parts(t, env, this)?;
        self.resolve_path(&path, env, this)
    }

    /// `a.b[3].c` as `["a", "b[3]", "c"]` with indexes evaluated (`this.` dropped).
    fn path_parts(&mut self, t: &[Token], env: &Env, this: This) -> Option<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut k = 0;
        if t.first().is_some_and(|x| x.is_kw("this")) {
            if !t.get(1).is_some_and(|x| x.is_punct(".")) {
                return None;
            }
            k = 2;
        }
        while k < t.len() {
            let x = &t[k];
            if !x.is_ident() {
                return None;
            }
            let mut name = x.text.to_string();
            k += 1;
            while t.get(k).is_some_and(|y| y.is_punct("[")) {
                let close = skip_group(t, k);
                let inner = &t[k + 1..close.saturating_sub(1)];
                let v = match env.locals.get(inner.first().map(|x| x.text.as_str()).unwrap_or("")) {
                    Some(Handle::Val(Val::Num(n))) if inner.len() == 1 => Some(*n),
                    _ => tokens_number(inner),
                };
                match v {
                    Some(v) => name.push_str(&format!("[{v}]")),
                    None => name.push_str("[*]"),
                }
                k = close;
            }
            out.push(name);
            if k < t.len() {
                if !t[k].is_punct(".") {
                    return None;
                }
                k += 1;
            }
        }
        let _ = this;
        (!out.is_empty()).then_some(out)
    }

    fn resolve_path(&self, path: &[String], env: &Env, this: This) -> Option<Obj> {
        let first = path.first()?;
        let mut cur = match env.locals.get(first) {
            Some(Handle::Obj(o)) => *o,
            Some(Handle::Val(_)) => return None,
            None => match self.vars_of(this).and_then(|v| v.get(first)) {
                Some(Handle::Obj(o)) => *o,
                _ if first == "default_map" => match this {
                    This::Block(b) => Obj::Map(self.m.blocks[b].default_map?),
                    _ => return None,
                },
                _ => return None,
            },
        };
        for p in &path[1..] {
            cur = match (cur, p.as_str()) {
                (Obj::Block(b), "default_map") => Obj::Map(self.m.blocks[b].default_map?),
                (Obj::Block(b), _) => match self.m.blocks[b].vars.get(p) {
                    Some(Handle::Obj(o)) => *o,
                    _ => return None,
                },
                (Obj::RegFile(f), _) => match self.regfiles[f].1.get(p) {
                    Some(Handle::Obj(o)) => *o,
                    _ => return None,
                },
                _ => return None,
            };
        }
        Some(cur)
    }

    fn vars_of(&self, this: This) -> Option<&FxHashMap<String, Handle>> {
        match this {
            This::Block(b) => Some(&self.m.blocks[b].vars),
            This::RegFile(f) => Some(&self.regfiles[f].1),
            This::Reg => Some(&self.reg_vars),
        }
    }

    /// Gives `target` the handle `h`: a local it declares, else as `set_var`.
    fn bind_target(&mut self, target: &Target, this: This, env: &mut Env, h: Handle) {
        if target.local {
            env.locals.insert(target.key.clone(), h);
        } else {
            self.set_var(this, env, &target.key, h);
        }
    }

    fn set_var(&mut self, this: This, env: &mut Env, name: &str, h: Handle) {
        // A local declared in the method, else a member of the object.
        if env.locals.contains_key(name) {
            env.locals.insert(name.to_string(), h);
            return;
        }
        match this {
            This::Block(b) => {
                if name == "default_map" {
                    if let Handle::Obj(Obj::Map(mi)) = h {
                        self.m.blocks[b].default_map = Some(mi);
                    }
                }
                self.m.blocks[b].vars.insert(name.to_string(), h);
            }
            This::RegFile(f) => {
                self.regfiles[f].1.insert(name.to_string(), h);
            }
            This::Reg => {
                self.reg_vars.insert(name.to_string(), h);
            }
        }
    }

    /// `target = rhs`.
    #[allow(clippy::too_many_arguments)]
    fn assign(&mut self, target: &Target, rhs: &[Token], m: DeclRef, env: &mut Env, this: This, _depth: usize, conditional: bool, at: Span) {
        let rhs = strip_parens(rhs);
        // A creation: `T::type_id::create(name, ...)`, `T #(..)::type_id::create(...)`, `new(...)`.
        if let Some((class, args_at)) = self.creation(rhs, target, m) {
            let args = if let Some(open) = args_at { self.args(rhs, open, m, env, this) } else { Vec::new() };
            let name = match args.first() {
                Some((_, Val::Str(s))) => s.clone(),
                _ => target.display.clone(),
            };
            let Some(class) = class else { return };
            let kind = self.a.ral_kind_of(class);
            let obj = match kind {
                Some("block") => {
                    let bi = self.m.blocks.len();
                    let mut notes = Vec::new();
                    if conditional {
                        notes.push("made under a condition".into());
                    }
                    self.m.blocks.push(Block { name, class, maps: Vec::new(), default_map: None, regs: Vec::new(), at: Some(at), notes, vars: FxHashMap::default() });
                    Some(Obj::Block(bi))
                }
                Some("register") | Some("memory") => {
                    let memory = kind == Some("memory");
                    // `new(name, n_bits)` gives its own arguments to the constructor chain.
                    let (bits, size) = if rhs.first().is_some_and(|x| x.is_kw("new")) { self.widths_from(class, &args, memory, 0) } else { self.widths(class, memory) };
                    let ri = self.m.regs.len();
                    let mut notes = Vec::new();
                    if conditional {
                        notes.push("made under a condition".into());
                    }
                    self.m.regs.push(Reg { name, class: Some(class), memory, bits, size, regfile: None, at, notes, build_args: None, built: false });
                    if let This::Block(b) = this {
                        self.m.blocks[b].regs.push(ri);
                    }
                    if let This::RegFile(f) = this {
                        self.m.regs[ri].regfile = Some(self.regfiles[f].0.clone());
                    }
                    Some(Obj::Reg(ri))
                }
                Some("regfile") => {
                    let fi = self.regfiles.len();
                    self.regfiles.push((name, FxHashMap::default(), Some(class)));
                    Some(Obj::RegFile(fi))
                }
                Some("field") => {
                    let fi = self.fields.len();
                    let mut notes = Vec::new();
                    if conditional {
                        notes.push("made under a condition".into());
                    }
                    self.fields.push(Field { name, size: None, lsb: None, access: None, volatile: None, reset: None, has_reset: None, notes, at });
                    Some(Obj::Field(fi))
                }
                _ => None,
            };
            if let Some(o) = obj {
                self.bind_target(target, this, env, Handle::Obj(o));
            }
            return;
        }
        // `m = create_map(...)`.
        if let Some(open) = call_open(rhs, "create_map") {
            let owner = match &rhs[..open.saturating_sub(1)] {
                [] => Some(this),
                [x, d] if x.is_kw("this") && d.is_punct(".") => Some(this),
                r => match self.object(strip_dot(r), env, this) {
                    Some(Obj::Block(b)) => Some(This::Block(b)),
                    _ => None,
                },
            };
            if let Some(This::Block(b)) = owner {
                let args = self.args(rhs, open, m, env, this);
                let get = |i: usize| -> Option<Val> { bind(&args, CREATE_MAP, i) };
                let name = match get(0) {
                    Some(Val::Str(s)) => s,
                    _ => target.display.clone(),
                };
                let mi = self.m.maps.len();
                self.m.maps.push(Map {
                    name,
                    block: b,
                    base: get(1).unwrap_or(Val::Unknown("base_addr".into())),
                    n_bytes: get(2).unwrap_or(Val::Unknown("n_bytes".into())),
                    byte_addressing: get(4).unwrap_or(Val::Num(1)),
                    fifo: match get(3) {
                        Some(Val::Unknown(t)) => matches!(t.trim_start_matches("uvm_pkg::"), "UVM_LITTLE_FIFO" | "UVM_BIG_FIFO"),
                        // `uvm_endianness_e`: UVM_LITTLE_FIFO = 3, UVM_BIG_FIFO = 4.
                        Some(Val::Num(v)) => v == 3 || v == 4,
                        _ => false,
                    },
                    entries: Vec::new(),
                    parent: None,
                    at: rhs[0].span,
                });
                self.m.blocks[b].maps.push(mi);
                if self.m.blocks[b].default_map.is_none() {
                    self.m.blocks[b].default_map = Some(mi);
                }
                self.bind_target(target, this, env, Handle::Obj(Obj::Map(mi)));
            }
            return;
        }
        // An alias of an object (`m = sub.default_map`), else a value.
        if let Some(o) = self.object(rhs, env, this) {
            self.bind_target(target, this, env, Handle::Obj(o));
            return;
        }
        if target.local {
            let v = match (self.eval(rhs, m, env, this), env.widths.get(&target.key)) {
                (Val::Num(n), Some(w)) => Val::Num(mask(n, Some(*w))),
                (v, _) => v,
            };
            env.locals.insert(target.key.clone(), Handle::Val(v));
        }
    }

    /// `rhs` makes an object: its class (when known) and the index of its argument list's `(`.
    fn creation(&self, rhs: &[Token], target: &Target, m: DeclRef) -> Option<(Option<DeclRef>, Option<usize>)> {
        if rhs.first().is_some_and(|x| x.is_kw("new")) {
            let open = rhs.get(1).filter(|x| x.is_punct("(")).map(|_| 1);
            // The class of the variable assigned.
            let class = target.member.and_then(|d| self.a.type_of(d)).and_then(|c| self.a.follow_typedef(c));
            return Some((class, open));
        }
        // `T [#(...)] :: type_id :: create (`
        let create = rhs.iter().position(|x| x.text == "create")?;
        if create < 4 || !rhs[create - 1].is_punct("::") || rhs[create - 2].text != "type_id" || !rhs[create - 3].is_punct("::") {
            return None;
        }
        let open = rhs.get(create + 1).filter(|x| x.is_punct("(")).map(|_| create + 1)?;
        // The class name: the identifier before `::type_id` (or before its `#(...)`).
        let mut k = create - 4;
        if rhs[k].is_punct(")") {
            let mut depth = 0i32;
            loop {
                if rhs[k].is_punct(")") {
                    depth += 1;
                } else if rhs[k].is_punct("(") {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                k = k.checked_sub(1)?;
            }
            k = k.checked_sub(2)?;
        }
        let ty = &rhs[k];
        let class = if ty.is_ident() { self.a.resolve_at_span(m.unit, ty.span).and_then(|c| self.a.follow_typedef(c)) } else { None };
        Some((class.or_else(|| target.member.and_then(|d| self.a.type_of(d)).and_then(|c| self.a.follow_typedef(c))), Some(open)))
    }

    /// A register's width (`n_bits` of its `super.new`), and a memory's size and width.
    fn widths(&mut self, class: DeclRef, memory: bool) -> (Val, Option<Val>) {
        // The factory passes only a name: every other argument of `new` has its default.
        self.widths_from(class, &[], memory, 0)
    }

    /// The width (and a memory's size) the constructor chain of `class` gives when its `new`
    /// is called with `args`: each class's `super.new(...)` with its own arguments bound, up
    /// to `uvm_reg::new(name, n_bits, has_coverage)` or `uvm_mem::new(name, size, n_bits, ...)`.
    fn widths_from(&mut self, class: DeclRef, args: &[(Option<String>, Val)], memory: bool, depth: usize) -> (Val, Option<Val>) {
        let unknown = (Val::Unknown("n_bits".into()), memory.then(|| Val::Unknown("size".into())));
        if depth > 16 {
            return unknown;
        }
        if ral_kind(&self.a.decl(class).name).is_some() {
            return if memory {
                let names = ["name", "size", "n_bits"];
                (bind(args, &names, 2).unwrap_or(unknown.0), Some(bind(args, &names, 1).unwrap_or(Val::Unknown("size".into()))))
            } else {
                (bind(args, &["name", "n_bits", "has_coverage"], 1).unwrap_or(unknown.0), None)
            };
        }
        // The class's own constructor; without one, the base's gets the same arguments.
        let own = self.a.scope_members(class).into_iter().find(|m| {
            let d = self.a.decl(*m);
            d.name == "new" && d.kind == DeclKind::Function
        });
        let Some(new) = own.map(|n| if self.a.decl(n).has(dflags::PROTOTYPE) { self.a.implementation_of(n).unwrap_or(n) } else { n }) else {
            return match self.a.class_base(class) {
                Some(b) => self.widths_from(b, args, memory, depth + 1),
                None => unknown,
            };
        };
        let mut env = Env::default();
        let u = self.a.unit(new.unit);
        let formals: Vec<(String, String)> = u.parsed.decl(new.decl).children.iter().map(|c| u.parsed.decl(*c)).filter(|c| c.kind == DeclKind::Argument).map(|c| (c.name.to_string(), c.detail.to_string())).collect();
        let mut pos = args.iter().filter(|(n, _)| n.is_none());
        for (n, d) in formals {
            let given = args.iter().find(|(an, _)| an.as_deref() == Some(n.as_str())).map(|(_, v)| v.clone()).or_else(|| pos.next().map(|(_, v)| v.clone()));
            let v = match given {
                Some(v) => v,
                None if !d.is_empty() => self.eval_text(&d, new, &env, This::Reg),
                None => Val::Unknown(n.clone()),
            };
            env.locals.insert(n, Handle::Val(v));
        }
        let t = self.body(new);
        let Some(k) = (0..t.len().saturating_sub(3)).find(|&k| t[k].is_kw("super") && t[k + 1].is_punct(".") && t[k + 2].is_kw("new") && t[k + 3].is_punct("(")) else { return unknown };
        let up = self.args(&t, k + 3, new, &env, This::Reg);
        match self.a.class_base(class) {
            Some(b) => self.widths_from(b, &up, memory, depth + 1),
            None => unknown,
        }
    }

    /// `recv.method(args)`.
    #[allow(clippy::too_many_arguments)]
    fn call(&mut self, recv: &[Token], method: &str, args: &[(Option<String>, Val)], handles: &[(Option<String>, Option<Obj>)], m: DeclRef, env: &mut Env, this: This, depth: usize, conditional: bool, at: Span) {
        let recv = strip_dot(recv);
        let on_self = recv.is_empty() || recv.len() == 1 && recv[0].is_kw("this");
        if recv.len() == 1 && recv[0].is_kw("super") {
            // The base class's version, with the same object.
            let Some(class) = self.class_of_method(m) else { return };
            let Some(base) = self.a.class_base(class) else { return };
            if let Some(f) = self.find_method(base, method) {
                self.exec_method(f, args, handles, this, depth + 1);
            }
            return;
        }
        if on_self {
            match method {
                "lock_model" => {
                    // Only a lock that surely runs ends the construction.
                    if matches!(this, This::Block(0)) && !conditional {
                        self.locked = true;
                    }
                }
                "set_default_map" => {
                    if let (This::Block(b), Some((_, Some(Obj::Map(mi))))) = (this, handles.first()) {
                        self.m.blocks[b].default_map = Some(*mi);
                    }
                }
                _ => {
                    // One of its own methods (a helper building part of the model).
                    let class = match this {
                        This::Block(b) => Some(self.m.blocks[b].class),
                        This::RegFile(f) => self.regfiles[f].2.or_else(|| self.class_of_method(m)),
                        This::Reg => self.class_of_method(m),
                    };
                    if let Some(f) = class.and_then(|c| self.find_method(c, method)) {
                        self.exec_method(f, args, handles, this, depth + 1);
                    }
                }
            }
            return;
        }
        let Some(obj) = self.object(recv, env, this) else { return };
        match (obj, method) {
            (Obj::Map(mi), "add_reg") | (Obj::Map(mi), "add_mem") => {
                let names = if method == "add_reg" { ADD_REG } else { ADD_MEM };
                let Some(Obj::Reg(ri)) = bind_obj(handles, names, 0) else { return };
                let offset = bind(args, names, 1).unwrap_or(Val::Unknown("offset".into()));
                let rights = match bind(args, names, 2) {
                    Some(Val::Str(s)) => Some(s),
                    Some(v) => Some(v.text()),
                    None => Some("RW".into()),
                };
                let unmapped = bind(args, names, 3).and_then(|v| v.num()).is_some_and(|v| v != 0);
                let mut notes = Vec::new();
                if conditional {
                    notes.push("added under a condition".into());
                }
                self.m.maps[mi].entries.push(Entry::Reg { reg: ri, offset, rights, unmapped, at, notes });
            }
            (Obj::Map(mi), "add_submap") => {
                let Some(Obj::Map(child)) = bind_obj(handles, ADD_SUBMAP, 0) else { return };
                let offset = bind(args, ADD_SUBMAP, 1).unwrap_or(Val::Unknown("offset".into()));
                // UVM refuses a map that already has a parent; a map in its own chain would loop.
                let mut up = Some(mi);
                let mut steps = 0;
                while let Some(x) = up {
                    if x == child || steps > 64 {
                        return;
                    }
                    up = self.m.maps[x].parent.as_ref().map(|p| p.0);
                    steps += 1;
                }
                if self.m.maps[child].parent.is_some() {
                    return;
                }
                self.m.maps[child].parent = Some((mi, offset.clone()));
                self.m.maps[mi].entries.push(Entry::Sub { map: child, offset, at });
            }
            (Obj::Map(mi), "set_base_addr") => {
                let v = bind(args, &["offset"], 0).unwrap_or(Val::Unknown("offset".into()));
                match self.m.maps[mi].parent {
                    // A mounted map: its offset in the parent changes.
                    Some((p, _)) => {
                        self.m.maps[mi].parent = Some((p, v.clone()));
                        for e in &mut self.m.maps[p].entries {
                            if let Entry::Sub { map, offset, .. } = e {
                                if *map == mi {
                                    *offset = v.clone();
                                }
                            }
                        }
                    }
                    None => self.m.maps[mi].base = v,
                }
            }
            (Obj::Block(b), "build") | (Obj::Block(b), "build_phase") => {
                let class = self.m.blocks[b].class;
                self.build_block(b, class, args, handles, depth + 1);
            }
            (Obj::Reg(ri), "configure") => {
                if let Some(Obj::RegFile(f)) = bind_obj(handles, REG_CONFIGURE, 1) {
                    self.m.regs[ri].regfile = Some(self.regfiles[f].0.clone());
                }
            }
            (Obj::Reg(ri), "build") => {
                self.m.regs[ri].build_args = Some(args.to_vec());
                self.m.regs[ri].built = true;
            }
            (Obj::RegFile(_), "configure") => {}
            (Obj::RegFile(f), _) => {
                // Its `build` (its registers are made in its own scope) or one of its methods
                // (`add_regs(.map(m), .offset(o))` adding them to a map).
                let class = self.regfiles[f].2.or_else(|| self.declared_class(recv, m, this));
                if let Some(bm) = class.and_then(|c| self.find_method(c, method)) {
                    self.exec_method(bm, args, handles, This::RegFile(f), depth + 1);
                }
            }
            (Obj::Block(b), _) if !matches!(method, "configure" | "lock_model" | "set_hdl_path_root" | "add_hdl_path" | "clear_hdl_path") => {
                // One of a sub-block's own methods.
                let class = self.m.blocks[b].class;
                if let Some(f) = self.find_method(class, method) {
                    self.exec_method(f, args, handles, This::Block(b), depth + 1);
                }
            }
            (Obj::Field(fi), "configure") => {
                let get = |i: usize| bind(args, FIELD_CONFIGURE, i);
                let f = &mut self.fields[fi];
                f.size = get(1).and_then(|v| v.num());
                f.lsb = get(2).and_then(|v| v.num());
                f.access = get(3).map(|v| match v {
                    Val::Str(s) => s,
                    v => v.text(),
                });
                f.volatile = get(4).and_then(|v| v.num()).map(|v| v != 0);
                f.has_reset = get(6).and_then(|v| v.num()).map(|v| v != 0);
                let reset = get(5);
                f.reset = match (&reset, f.has_reset) {
                    (_, Some(false)) => None,
                    (Some(Val::Num(n)), _) => Some(format!("{:#x}", mask(*n, f.size))),
                    (Some(Val::Unknown(s)), _) if s == "'1" => f.size.map(|s| format!("{:#x}", mask(u128::MAX, Some(s)))),
                    (Some(v), _) => {
                        f.notes.push("reset not constant".into());
                        Some(v.text())
                    }
                    (None, _) => None,
                };
                if get(1).is_some_and(|v| v.num().is_none()) || get(2).is_some_and(|v| v.num().is_none()) {
                    f.notes.push("size or position not constant".into());
                }
            }
            (Obj::Field(fi), "set_reset") | (Obj::Field(fi), "set_access") => {
                self.fields[fi].notes.push(format!("changed later by {method}"));
            }
            _ => {}
        }
    }

    /// The class a method belongs to (an out-of-block body's class).
    fn class_of_method(&self, m: DeclRef) -> Option<DeclRef> {
        let d = self.a.decl(m);
        if d.has(dflags::OUT_OF_BLOCK) {
            return self.a.owner_class(m);
        }
        d.parent.map(|p| DeclRef { unit: m.unit, decl: p }).filter(|c| matches!(self.a.decl(*c).kind, DeclKind::Class))
    }

    /// The declared class of a member named by `recv` in the class of `m` (`rf` of `rf.build()`).
    fn declared_class(&self, recv: &[Token], m: DeclRef, _this: This) -> Option<DeclRef> {
        let name = recv.iter().rev().find(|x| x.is_ident())?;
        let class = self.class_of_method(m)?;
        let member = self.a.members_list(class).into_iter().find(|d| self.a.decl(*d).name == name.text)?;
        self.a.type_of(member).and_then(|c| self.a.follow_typedef(c))
    }

    /// The value of an expression, in the scope of method `m`.
    fn eval(&mut self, t: &[Token], m: DeclRef, env: &Env, this: This) -> Val {
        let t = strip_parens(t);
        if t.is_empty() {
            return Val::Unknown(String::new());
        }
        if let [s] = t {
            if s.kind == TokKind::String {
                return Val::Str(s.text.trim_matches('"').to_string());
            }
            if s.is_ident() {
                if let Some(Handle::Val(v)) = env.locals.get(s.text.as_str()) {
                    return v.clone();
                }
            }
        }
        let text = super::views::tokens_text(t);
        let mut ev = Eval { it: self, m, env, this, t, k: 0, nest: 0 };
        match ev.expr(0) {
            Some(v) if ev.k == t.len() => Val::Num(v),
            _ => Val::Unknown(text),
        }
    }

    /// The value of the text `text` (a default argument value), in the scope of method `m`.
    fn eval_text(&mut self, text: &str, m: DeclRef, env: &Env, this: This) -> Val {
        let t = dvi_syntax::lexer::lex(text, self.a.decl(m).span.file);
        let t: Vec<Token> = t.into_iter().map(|mut x| {
            // Positions inside the method's file are not those of the text: no resolution by span.
            x.span = Span::default();
            x
        }).collect();
        self.eval(&t, m, env, this)
    }
}

/// The target of an assignment: its key in the scope (`r[3]`), its display name, whether it
/// is a local, and the member variable it names (for its declared class).
struct Target {
    key: String,
    display: String,
    local: bool,
    member: Option<DeclRef>,
}

/// The assignment target `lhs` (`r`, `this.r`, `r[i]`, `int base`, `uvm_reg_map m`).
fn assigned_name(lhs: &[Token], it: &mut Interp, m: DeclRef, env: &Env, this: This) -> Option<Target> {
    let lhs = strip_parens(lhs);
    // A declaration: `<type> name` (the last identifier, no dots).
    let declared = lhs.len() >= 2 && lhs.last().is_some_and(|x| x.is_ident()) && !lhs[lhs.len() - 2].is_punct(".") && !lhs.iter().any(|x| x.is_punct("["));
    if declared {
        let name = lhs.last()?.text.to_string();
        return Some(Target { key: name.clone(), display: name, local: true, member: None });
    }
    let parts = it.path_parts(lhs, env, this)?;
    if parts.len() != 1 {
        return None;
    }
    let key = parts[0].clone();
    let base = key.split('[').next().unwrap_or(&key).to_string();
    let local = env.locals.contains_key(&base);
    let member = it.class_of_method(m).and_then(|c| it.a.members_list(c).into_iter().find(|d| it.a.decl(*d).name == base.as_str()));
    Some(Target { key: key.clone(), display: key, local, member })
}

/// The index after the statement starting at `t[k]` (after its `;`, or after a `begin ... end`
/// block or the statement a control word governs), not beyond `b`.
fn statement_end(t: &[Token], k: usize, b: usize) -> usize {
    let mut j = k;
    if t.get(j).is_some_and(|x| x.is_kw("begin")) {
        let mut depth = 0i32;
        while j < b {
            if t[j].is_kw("begin") || t[j].is_kw("fork") {
                depth += 1;
            } else if t[j].is_kw("end") || t[j].is_kw("join") || t[j].is_kw("join_any") || t[j].is_kw("join_none") {
                depth -= 1;
                if depth == 0 {
                    j += 1;
                    if t.get(j).is_some_and(|x| x.is_punct(":")) {
                        j += 2;
                    }
                    return j.min(b);
                }
            }
            j += 1;
        }
        return b;
    }
    // A control word: its header, then its statement.
    if t.get(j).is_some_and(|x| x.is_kw("for") || x.is_kw("foreach") || x.is_kw("if") || x.is_kw("while") || x.is_kw("repeat")) && t.get(j + 1).is_some_and(|x| x.is_punct("(")) {
        return statement_end(t, skip_group(t, j + 1), b);
    }
    // A macro call is a statement of its own, with or without a `;` (`` `uvm_info(...) ``);
    // so is a macro alone on its line.
    if t.get(j).is_some_and(|x| x.kind == TokKind::Directive) {
        let mut e = if t.get(j + 1).is_some_and(|x| x.is_punct("(") && !x.ws_before()) { skip_group(t, j + 1) } else { j + 1 };
        if t.get(e).is_some_and(|x| x.is_punct(";")) {
            e += 1;
        } else if e == j + 1 && !t.get(e).is_none_or(|x| x.nl_before()) {
            // A macro that starts a longer statement (`` `REG.write(...) ``).
            e = j;
        }
        if e > j {
            return e.min(b);
        }
    }
    while j < b {
        let x = &t[j];
        if x.is_punct("(") || x.is_punct("[") || x.is_punct("{") {
            j = skip_group(t, j);
            continue;
        }
        if x.is_punct(";") {
            return j + 1;
        }
        j += 1;
    }
    b
}

/// Whether token `k` is inside an `if` or `case` branch.
fn in_condition(f: &stmt::StmtMap, k: usize) -> bool {
    let mut cur = f.at.get(k).copied().unwrap_or(u32::MAX);
    let mut n = 0;
    while cur != u32::MAX && n < 64 {
        let fr = &f.frames[cur as usize];
        if matches!(fr.kind, FrameKind::If { .. } | FrameKind::Case { .. }) {
            return true;
        }
        cur = fr.parent.unwrap_or(u32::MAX);
        n += 1;
    }
    false
}

fn strip_semicolon(s: &[Token]) -> &[Token] {
    match s.last() {
        Some(x) if x.is_punct(";") => &s[..s.len() - 1],
        _ => s,
    }
}

/// `t` without parentheses around all of it.
fn strip_parens(mut t: &[Token]) -> &[Token] {
    while t.len() >= 2 && t[0].is_punct("(") && skip_group(t, 0) == t.len() {
        t = &t[1..t.len() - 1];
    }
    t
}

fn strip_dot(t: &[Token]) -> &[Token] {
    match t.last() {
        Some(x) if x.is_punct(".") => &t[..t.len() - 1],
        _ => t,
    }
}

/// The position of `p` outside brackets, if any (`=` of an assignment, not of `==`).
fn top_level(t: &[Token], p: &str) -> Option<usize> {
    let mut k = 0;
    while k < t.len() {
        if t[k].is_punct("(") || t[k].is_punct("[") || t[k].is_punct("{") {
            k = skip_group(t, k);
            continue;
        }
        if t[k].is_punct(p) {
            return Some(k);
        }
        k += 1;
    }
    None
}

/// The index of `name(`'s `(` in `t` when `t` is a call of it (`create_map(...)`,
/// `this.create_map(...)`, `blk.create_map(...)`).
fn call_open(t: &[Token], name: &str) -> Option<usize> {
    let k = t.iter().position(|x| x.text == name)?;
    (t.get(k + 1).is_some_and(|x| x.is_punct("(")) && skip_group(t, k + 1) == t.len()).then_some(k + 1)
}

/// Argument `i` of a call by its UVM signature `names`: given by name, else by position.
fn bind(args: &[(Option<String>, Val)], names: &[&str], i: usize) -> Option<Val> {
    if let Some((_, v)) = args.iter().find(|(n, _)| n.as_deref() == Some(names[i])) {
        return Some(v.clone());
    }
    if args.iter().any(|(n, _)| n.is_some()) && args.iter().position(|(n, _)| n.is_some()).is_some_and(|p| p <= i) {
        return None;
    }
    args.get(i).filter(|(n, _)| n.is_none()).map(|(_, v)| v.clone()).filter(|v| !matches!(v, Val::Unknown(s) if s.is_empty()))
}

fn bind_obj(args: &[(Option<String>, Option<Obj>)], names: &[&str], i: usize) -> Option<Obj> {
    if let Some((_, v)) = args.iter().find(|(n, _)| n.as_deref() == Some(names[i])) {
        return *v;
    }
    args.get(i).filter(|(n, _)| n.is_none()).and_then(|(_, v)| *v)
}

/// The width of an integral type at the start of a declaration (`int unsigned x`, `bit [7:0] y`),
/// when it is a keyword type with constant bounds.
fn declared_width(t: &[Token]) -> Option<u128> {
    let base = t.first().filter(|x| x.kind == TokKind::Keyword)?;
    let w = match base.text.as_str() {
        "int" | "integer" => 32,
        "shortint" => 16,
        "longint" | "time" => 64,
        "byte" => 8,
        "bit" | "logic" | "reg" => {
            // A range `[a:b]` of numbers, or one bit.
            let k = t.iter().position(|x| x.is_punct("["))?;
            match (t.get(k + 1).and_then(|x| tokens_number(std::slice::from_ref(x))), t.get(k + 2).filter(|x| x.is_punct(":")), t.get(k + 3).and_then(|x| tokens_number(std::slice::from_ref(x))), t.get(k + 4).filter(|x| x.is_punct("]"))) {
                (Some(a), Some(_), Some(b), Some(_)) => a.abs_diff(b) + 1,
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(w)
}

fn mask(v: u128, size: Option<u128>) -> u128 {
    match size {
        Some(s) if s < 128 => v & ((1u128 << s) - 1),
        _ => v,
    }
}

/// A single number token's value (an index).
fn tokens_number(t: &[Token]) -> Option<u128> {
    match t {
        [x] if x.kind == TokKind::Number => number(&x.text).map(|(v, _)| v),
        _ => None,
    }
}

/// An integer literal: `16`, `'h10`, `32'h0000_0010`, `8'd255`, `4'b1010`, `'1` (all ones of
/// an unknown width: `None` here). Returns the value and its width when sized. `x`/`z` digits
/// make it unknown.
fn number(s: &str) -> Option<(u128, Option<u32>)> {
    let s: String = s.chars().filter(|c| *c != '_' && !c.is_whitespace()).collect();
    if let Some(q) = s.find('\'') {
        let width = if q == 0 { None } else { s[..q].parse::<u32>().ok() };
        let mut rest = &s[q + 1..];
        if rest.starts_with(['s', 'S']) {
            rest = &rest[1..];
        }
        let (radix, digits) = match rest.chars().next()? {
            'h' | 'H' => (16, &rest[1..]),
            'd' | 'D' => (10, &rest[1..]),
            'b' | 'B' => (2, &rest[1..]),
            'o' | 'O' => (8, &rest[1..]),
            _ => return None,
        };
        let v = u128::from_str_radix(digits, radix).ok()?;
        return Some((width.map_or(v, |w| mask(v, Some(w as u128))), width));
    }
    s.parse::<u128>().ok().map(|v| (v, None))
}

/// A small evaluator: numbers, names (loop variables, arguments, parameters), `+ - * / % <<
/// >> & | ^ ~` and parentheses.
struct Eval<'e, 'a> {
    it: &'e mut Interp<'a>,
    m: DeclRef,
    env: &'e Env,
    this: This,
    t: &'e [Token],
    k: usize,
    /// Parentheses and unary operators entered.
    nest: usize,
}

impl Eval<'_, '_> {
    fn prec(op: &str) -> Option<u8> {
        Some(match op {
            "|" => 1,
            "^" => 2,
            "&" => 3,
            "<<" | ">>" => 4,
            "+" | "-" => 5,
            "*" | "/" | "%" => 6,
            _ => return None,
        })
    }

    fn expr(&mut self, min: u8) -> Option<u128> {
        let mut lhs = self.unary()?;
        while let Some(op) = self.t.get(self.k).filter(|x| x.kind == TokKind::Punct).map(|x| x.text.to_string()) {
            let Some(p) = Self::prec(&op) else { break };
            if p < min {
                break;
            }
            self.k += 1;
            let rhs = self.expr(p + 1)?;
            lhs = match op.as_str() {
                "|" => lhs | rhs,
                "^" => lhs ^ rhs,
                "&" => lhs & rhs,
                "<<" => lhs.checked_shl(u32::try_from(rhs).ok()?)?,
                ">>" => lhs.checked_shr(u32::try_from(rhs).ok()?)?,
                "+" => lhs.checked_add(rhs)?,
                "-" => lhs.checked_sub(rhs)?,
                "*" => lhs.checked_mul(rhs)?,
                "/" => lhs.checked_div(rhs)?,
                "%" => lhs.checked_rem(rhs)?,
                _ => return None,
            };
        }
        Some(lhs)
    }

    fn unary(&mut self) -> Option<u128> {
        let named = self.t.get(self.k).is_some_and(|x| x.kind == TokKind::Directive || x.is_ident());
        let v = self.atom()?;
        // A width a macro or a parameter gives to a based number: `` `ADDR_W'h10 ``, `W'h10`.
        if named {
            if let Some(n) = self.t.get(self.k).filter(|n| n.kind == TokKind::Number && n.text.starts_with('\'')) {
                self.k += 1;
                return number(&n.text).map(|(x, _)| mask(x, Some(v)));
            }
        }
        Some(v)
    }

    fn atom(&mut self) -> Option<u128> {
        self.nest += 1;
        let r = if self.nest > 200 { None } else { self.atom_inner() };
        self.nest -= 1;
        r
    }

    fn atom_inner(&mut self) -> Option<u128> {
        let x = self.t.get(self.k)?.clone();
        self.k += 1;
        if x.is_punct("(") {
            let v = self.expr(0)?;
            if !self.t.get(self.k).is_some_and(|c| c.is_punct(")")) {
                return None;
            }
            self.k += 1;
            return Some(v);
        }
        if x.is_punct("~") {
            return self.unary().map(|v| !v);
        }
        if x.is_punct("+") {
            return self.unary();
        }
        if x.kind == TokKind::Number {
            return number(&x.text).map(|(v, _)| v);
        }
        if x.kind == TokKind::Directive {
            // An object-like macro (`` `BASE_ADDR ``) defined one way.
            let defs = self.it.a.global.macros.get(x.text.as_str())?;
            let first = defs.first()?.clone();
            if first.params.is_some() || !defs.iter().all(|d| d.same_as(&first)) || self.it.expanding >= 8 {
                return None;
            }
            self.it.expanding += 1;
            let v = self.it.eval(&first.body, self.m, self.env, self.this).num();
            self.it.expanding -= 1;
            return v;
        }
        if x.is_ident() {
            // `pkg::NAME`
            if self.t.get(self.k).is_some_and(|c| c.is_punct("::")) && self.t.get(self.k + 1).is_some_and(|c| c.is_ident()) {
                let pkg = x.text.to_string();
                let name = self.t[self.k + 1].text.to_string();
                self.k += 2;
                return self.package_value(&pkg, &name);
            }
            if let Some(Handle::Val(Val::Num(n))) = self.env.locals.get(x.text.as_str()) {
                return Some(*n);
            }
            if self.env.locals.contains_key(x.text.as_str()) {
                return None;
            }
            // A parameter in scope.
            let toks: &dyn Fn(FileId) -> Option<Arc<Vec<Token>>> = &|f| self.it.a.file_tokens(f);
            let v = self.it.a.const_value(std::slice::from_ref(&x), self.m.unit, 0, toks)?;
            return u128::try_from(v).ok();
        }
        let _ = self.this;
        None
    }

    fn package_value(&mut self, pkg: &str, name: &str) -> Option<u128> {
        let p = self.it.a.package(self.m.unit, pkg)?;
        let d = self.it.a.member_of(p, name)?;
        let dd = self.it.a.decl(d);
        if !matches!(dd.kind, DeclKind::Parameter | DeclKind::LocalParam) || dd.detail.is_empty() {
            return None;
        }
        if self.it.expanding >= 8 {
            return None;
        }
        let text = dd.detail.to_string();
        self.it.expanding += 1;
        let v = self.it.eval_text(&text, d, &Env::default(), self.this);
        self.it.expanding -= 1;
        v.num()
    }
}

/// Keeps at most `max` nodes of the tree under `n` (depth first), and the nodes on the path
/// `reveal` (names from `n`'s children down) beyond that; a node that lost children is marked
/// truncated. Whether any was dropped.
pub fn limit_tree(n: &mut RegNode, max: usize, reveal: &[String]) -> bool {
    let mut left = max;
    limit_children(n, &mut left, reveal)
}

fn limit_children(n: &mut RegNode, left: &mut usize, reveal: &[String]) -> bool {
    let mut lost = false;
    let mut below = false;
    let mut kept = Vec::new();
    for mut c in std::mem::take(&mut n.children) {
        let on_path = reveal.first().is_some_and(|r| *r == c.name);
        if *left == 0 && !on_path {
            lost = true;
            continue;
        }
        *left = left.saturating_sub(1);
        below |= limit_children(&mut c, left, if on_path { &reveal[1..] } else { &[] });
        kept.push(c);
    }
    n.children = kept;
    n.truncated |= lost;
    lost || below
}

/// The nodes of `n` whose name (or class) matches `f`, with their ancestors; the matches are
/// counted.
pub fn prune(n: RegNode, f: &super::filter::Filter, matches: &mut usize) -> Option<RegNode> {
    let hit = f.matches(&n.name, '.') || n.class.as_deref().is_some_and(|c| f.matches(c, '.'));
    if hit {
        *matches += 1;
    }
    let kept: Vec<RegNode> = n.children.clone().into_iter().filter_map(|c| prune(c, f, matches)).collect();
    if hit || !kept.is_empty() {
        let mut n = n;
        if !hit {
            n.children = kept;
        }
        Some(n)
    } else {
        None
    }
}
