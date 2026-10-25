//! Macro definitions by name, as a persistent hash trie.
//!
//! Every compilation unit keeps the table it started with and the one it left behind, and the
//! units of an invocation pass one table along, each adding a few definitions. A copy that
//! shares nothing (a hash map copied on write) made N units over M macros cost N × M in time
//! and memory. In the trie a change copies only the nodes on the path to the name, so each
//! unit's table costs about as much as the definitions it changed, and two tables can be
//! compared (and their differences listed) without visiting the parts they share.

use crate::preproc::{def_identity, MacroDef};
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use smol_str::SmolStr;
use std::hash::Hasher;
use std::ops::ControlFlow;
use std::sync::Arc;

/// Hash bits per level.
const BITS: u32 = 5;
/// Levels that branch on hash bits (13 × 5 covers 64 bits); below them, a collision node
/// holds the names whose hashes are all equal.
const LEVELS: u32 = 13;

fn hash(name: &str) -> u64 {
    let mut h = FxHasher::default();
    h.write(name.as_bytes());
    weaken(h.finish())
}

#[cfg(not(test))]
#[inline]
fn weaken(h: u64) -> u64 {
    h
}

/// Tests mask the hash to make deep paths and collisions common.
#[cfg(test)]
fn weaken(h: u64) -> u64 {
    tests::WEAK.with(|w| w.get()).map_or(h, |m| h & m)
}

fn chunk(h: u64, level: u32) -> u32 {
    debug_assert!(level < LEVELS);
    ((h >> (level * BITS)) & ((1 << BITS) - 1)) as u32
}

/// Position of `bit`'s slot among the slots of `bitmap`.
fn index(bitmap: u32, bit: u32) -> usize {
    (bitmap & (bit - 1)).count_ones() as usize
}

#[cfg(feature = "work-count")]
thread_local! {
    static VISITED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Counts `n` nodes or definitions visited by a comparison or a relocation.
#[inline]
fn visit(_n: usize) {
    #[cfg(feature = "work-count")]
    VISITED.with(|v| v.set(v.get() + _n as u64));
}

/// Nodes and definitions that comparisons and relocations visited on this thread so far
/// (with the `work-count` feature; for tests).
#[cfg(feature = "work-count")]
#[doc(hidden)]
pub fn visited() -> u64 {
    VISITED.with(|v| v.get())
}

#[derive(Clone)]
enum Slot {
    /// A definition; its name is the key.
    Leaf(Arc<MacroDef>),
    /// At least two names share the hash bits so far.
    Node(Arc<Node>),
}

#[derive(Clone)]
enum Node {
    Branch { bitmap: u32, slots: Vec<Slot> },
    /// Names whose hashes are equal (only below the last branching level).
    Collision(Vec<Arc<MacroDef>>),
}

/// What a node becomes once a name is removed from it.
enum Removed {
    Node,
    /// A single definition is left: its parent holds it as a leaf.
    Leaf(Arc<MacroDef>),
}

/// Macro definitions by name (the name of each definition is its key).
///
/// Cloning is cheap and shares the whole table; changing a clone copies only the nodes on the
/// path to the changed name. The shape depends only on the names in the table (a branch left
/// with a single definition is folded into its parent), so tables with the same names have the
/// same layout, and parts that were never changed stay shared.
#[derive(Clone, Default)]
pub struct MacroTable {
    /// A branch (never a leaf) at level 0; `None` when the table is empty.
    root: Option<Arc<Node>>,
    len: usize,
}

impl std::fmt::Debug for MacroTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl MacroTable {
    pub fn new() -> MacroTable {
        MacroTable::default()
    }

    pub fn get(&self, name: &str) -> Option<&Arc<MacroDef>> {
        let h = hash(name);
        let mut node = self.root.as_deref()?;
        let mut level = 0;
        loop {
            match node {
                Node::Branch { bitmap, slots } => {
                    let bit = 1u32 << chunk(h, level);
                    if bitmap & bit == 0 {
                        return None;
                    }
                    match &slots[index(*bitmap, bit)] {
                        Slot::Leaf(d) => return (d.name == name).then_some(d),
                        Slot::Node(n) => {
                            node = n;
                            level += 1;
                        }
                    }
                }
                Node::Collision(defs) => return defs.iter().find(|d| d.name == name),
            }
        }
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Defines `def` under its own name, replacing an earlier definition of that name.
    pub fn insert(&mut self, def: Arc<MacroDef>) {
        let h = hash(&def.name);
        let root = self.root.get_or_insert_with(|| Arc::new(Node::Branch { bitmap: 0, slots: Vec::new() }));
        if insert_in(Arc::make_mut(root), h, 0, def) {
            self.len += 1;
        }
    }

    pub fn remove(&mut self, name: &str) {
        // Nothing is copied when the name is not there.
        if !self.contains_key(name) {
            return;
        }
        self.len -= 1;
        if self.len == 0 {
            self.root = None;
            return;
        }
        let h = hash(name);
        if let Some(root) = &mut self.root {
            // The root stays a branch, even with a single definition left.
            remove_in(Arc::make_mut(root), h, 0, name);
        }
    }

    pub fn clear(&mut self) {
        self.root = None;
        self.len = 0;
    }

    /// The definitions, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&SmolStr, &Arc<MacroDef>)> {
        Iter { stack: self.root.as_deref().map(frame).into_iter().collect() }.map(|d| (&d.name, d))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Identity of the shared table: tables with the same identity are equal (all empty
    /// tables have identity 0). Only meaningful while both tables are alive.
    pub fn identity(&self) -> usize {
        self.root.as_ref().map_or(0, |r| Arc::as_ptr(r) as *const () as usize)
    }

    /// Calls `f(old, new)` for each name whose definition is not the same `Arc` in `self`
    /// (`old`) and in `other` (`new`); either is `None` where the table lacks the name. Parts
    /// the two tables share are skipped, so this costs about as much as the differences.
    /// Stops when `f` breaks; returns whether it did.
    pub fn diff<'a, F>(&'a self, other: &'a MacroTable, mut f: F) -> ControlFlow<()>
    where
        F: FnMut(Option<&'a Arc<MacroDef>>, Option<&'a Arc<MacroDef>>) -> ControlFlow<()>,
    {
        diff_nodes(self.root.as_ref(), other.root.as_ref(), &mut f)
    }

    /// Same macro names with the same parameters and bodies (locations may differ): files
    /// compiled after a table expand identically with either.
    pub fn same_definitions(&self, other: &MacroTable) -> bool {
        self.len == other.len
            && self
                .diff(other, |a, b| match (a, b) {
                    (Some(a), Some(b)) if a.same_as(b) => ControlFlow::Continue(()),
                    _ => ControlFlow::Break(()),
                })
                .is_continue()
    }

    /// [`MacroTable::same_definitions`], remembering in `memo` the parts already compared.
    /// For many pairs of tables that share parts on each side (each unit's table before and
    /// after an update that moved a header's definitions), each pair of shared parts is
    /// compared once rather than once per unit.
    pub fn same_definitions_memo(&self, other: &MacroTable, memo: &mut SameMemo) -> bool {
        self.len == other.len
            && match (&self.root, &other.root) {
                (Some(a), Some(b)) => same_nodes(a, b, memo),
                (a, b) => a.is_none() && b.is_none(),
            }
    }

    /// A copy in which the definitions found in `moved` (keyed by the address of the old
    /// definition, see [`def_identity`]) are replaced, and how many were; `None` (and 0) when
    /// the table has none of them. Parts without a moved definition stay shared.
    pub fn relocated(&self, moved: &FxHashMap<usize, Arc<MacroDef>>) -> (Option<MacroTable>, usize) {
        let mut count = 0;
        match self.root.as_ref().and_then(|r| relocate_node(r, moved, &mut count)) {
            Some(root) => (Some(MacroTable { root: Some(root), len: self.len }), count),
            None => (None, 0),
        }
    }

    /// [`MacroTable::relocated`] of `self`, given that of a table `base` (`base_new` and
    /// `base_count`, as `relocated` returned them for `base`): `base_new` (or `self` when it
    /// is `None`) with the differences between `base` and `self` applied, each new definition
    /// replaced when it moved. When `self` was made from `base` with a few changes, this costs
    /// about as much as those changes, and the result shares the rest with `base_new`.
    pub fn relocated_from(
        &self,
        base: &MacroTable,
        base_new: Option<&MacroTable>,
        base_count: usize,
        moved: &FxHashMap<usize, Arc<MacroDef>>,
    ) -> (Option<MacroTable>, usize) {
        let is_moved = |d: &Arc<MacroDef>| moved.contains_key(&def_identity(d));
        match base_new {
            // `self` has a moved definition only where it differs from `base`.
            None => {
                let mut t = self.clone();
                let mut count = 0;
                let _ = base.diff(self, |_, new| {
                    if let Some(n) = new.and_then(|d| moved.get(&def_identity(d))) {
                        t.insert(n.clone());
                        count += 1;
                    }
                    ControlFlow::Continue(())
                });
                if count == 0 {
                    (None, 0)
                } else {
                    (Some(t), count)
                }
            }
            Some(bn) => {
                let mut t = bn.clone();
                let mut count = base_count;
                let _ = base.diff(self, |old, new| {
                    if old.is_some_and(is_moved) {
                        count -= 1;
                    }
                    match (old, new) {
                        (_, Some(n)) => match moved.get(&def_identity(n)) {
                            Some(r) => {
                                count += 1;
                                t.insert(r.clone());
                            }
                            None => t.insert(n.clone()),
                        },
                        (Some(o), None) => t.remove(&o.name),
                        (None, None) => {}
                    }
                    ControlFlow::Continue(())
                });
                if count == 0 {
                    return (None, 0);
                }
                // A table of its own, as relocating `self` from scratch gives: tables that
                // were different (by identity) stay different.
                if t.identity() == bn.identity() && self.identity() != base.identity() {
                    t.root = t.root.map(|r| Arc::new((*r).clone()));
                }
                (Some(t), count)
            }
        }
    }

    /// `relocated`, made for fewer moved definitions than `moved` now has, brought up to date:
    /// `changes` are the entries of `moved` added or replaced since, each with the name of the
    /// old definition and whether it was new in `moved`.
    pub fn relocated_more(
        &self,
        relocated: (Option<&MacroTable>, usize),
        changes: &[(usize, SmolStr, bool)],
        moved: &FxHashMap<usize, Arc<MacroDef>>,
    ) -> (Option<MacroTable>, usize) {
        visit(changes.len());
        let (mut table, mut count) = (relocated.0.cloned(), relocated.1);
        for (old, name, new) in changes {
            if self.get(name).is_some_and(|d| def_identity(d) == *old) {
                if *new {
                    count += 1;
                }
                table.get_or_insert_with(|| self.clone()).insert(moved[old].clone());
            }
        }
        // A definition, once in `moved`, stays: the count only grows.
        (table, count)
    }

    /// Trie nodes reachable from `tables`, each counted once: how much memory tables that
    /// share parts hold together (for tests).
    #[doc(hidden)]
    pub fn distinct_nodes<'a>(tables: impl IntoIterator<Item = &'a MacroTable>) -> usize {
        fn walk(n: &Arc<Node>, seen: &mut FxHashSet<usize>) {
            if !seen.insert(Arc::as_ptr(n) as usize) {
                return;
            }
            if let Node::Branch { slots, .. } = &**n {
                for s in slots {
                    if let Slot::Node(c) = s {
                        walk(c, seen);
                    }
                }
            }
        }
        let mut seen = FxHashSet::default();
        for t in tables {
            if let Some(r) = &t.root {
                walk(r, &mut seen);
            }
        }
        seen.len()
    }
}

/// Pairs of trie nodes already compared by [`MacroTable::same_definitions_memo`], with the
/// answer. It holds the nodes, so that their addresses stay theirs and nothing changes them
/// in place while it lives.
#[derive(Default)]
pub struct SameMemo(FxHashMap<(usize, usize), (bool, [Arc<Node>; 2])>);

/// Whether two nodes hold the same names with the same definitions (by
/// [`MacroDef::same_as`]). The shape depends only on the names, so a different layout means
/// different names.
fn same_nodes(a: &Arc<Node>, b: &Arc<Node>, memo: &mut SameMemo) -> bool {
    if Arc::ptr_eq(a, b) {
        return true;
    }
    let key = (Arc::as_ptr(a) as usize, Arc::as_ptr(b) as usize);
    if let Some((same, _)) = memo.0.get(&key) {
        return *same;
    }
    visit(1);
    let same = match (&**a, &**b) {
        (Node::Branch { bitmap: ba, slots: sa }, Node::Branch { bitmap: bb, slots: sb }) => {
            ba == bb
                && sa.iter().zip(sb).all(|pair| match pair {
                    (Slot::Leaf(x), Slot::Leaf(y)) => x.name == y.name && (Arc::ptr_eq(x, y) || x.same_as(y)),
                    (Slot::Node(x), Slot::Node(y)) => same_nodes(x, y, memo),
                    _ => false,
                })
        }
        (Node::Collision(da), Node::Collision(db)) => {
            da.len() == db.len() && da.iter().all(|x| db.iter().any(|y| x.name == y.name && (Arc::ptr_eq(x, y) || x.same_as(y))))
        }
        _ => false,
    };
    memo.0.insert(key, (same, [a.clone(), b.clone()]));
    same
}

/// Puts `def` in `node` (at `level`); whether its name is new there.
fn insert_in(node: &mut Node, h: u64, level: u32, def: Arc<MacroDef>) -> bool {
    match node {
        Node::Collision(defs) => match defs.iter_mut().find(|d| d.name == def.name) {
            Some(d) => {
                *d = def;
                false
            }
            None => {
                defs.push(def);
                true
            }
        },
        Node::Branch { bitmap, slots } => {
            let bit = 1u32 << chunk(h, level);
            let i = index(*bitmap, bit);
            if *bitmap & bit == 0 {
                *bitmap |= bit;
                slots.insert(i, Slot::Leaf(def));
                return true;
            }
            let other = match &mut slots[i] {
                Slot::Node(n) => return insert_in(Arc::make_mut(n), h, level + 1, def),
                Slot::Leaf(old) if old.name == def.name => {
                    *old = def;
                    return false;
                }
                Slot::Leaf(old) => old.clone(),
            };
            let oh = hash(&other.name);
            slots[i] = Slot::Node(Arc::new(pair(other, oh, def, h, level + 1)));
            true
        }
    }
}

/// A node at `level` holding two definitions of different names, with hashes `ha` and `hb`.
fn pair(a: Arc<MacroDef>, ha: u64, b: Arc<MacroDef>, hb: u64, level: u32) -> Node {
    if level >= LEVELS {
        return Node::Collision(vec![a, b]);
    }
    let (ca, cb) = (chunk(ha, level), chunk(hb, level));
    if ca == cb {
        return Node::Branch { bitmap: 1 << ca, slots: vec![Slot::Node(Arc::new(pair(a, ha, b, hb, level + 1)))] };
    }
    let slots = if ca < cb { vec![Slot::Leaf(a), Slot::Leaf(b)] } else { vec![Slot::Leaf(b), Slot::Leaf(a)] };
    Node::Branch { bitmap: (1 << ca) | (1 << cb), slots }
}

/// Removes `name`, which `node` (at `level`) holds, keeping the shape canonical.
fn remove_in(node: &mut Node, h: u64, level: u32, name: &str) -> Removed {
    match node {
        Node::Collision(defs) => {
            defs.retain(|d| d.name != name);
            match defs.as_slice() {
                [d] => Removed::Leaf(d.clone()),
                _ => Removed::Node,
            }
        }
        Node::Branch { bitmap, slots } => {
            let bit = 1u32 << chunk(h, level);
            let i = index(*bitmap, bit);
            let leaf = match &mut slots[i] {
                Slot::Leaf(_) => None,
                Slot::Node(n) => match remove_in(Arc::make_mut(n), h, level + 1, name) {
                    // The node keeps its slots: nothing to fold here.
                    Removed::Node => return Removed::Node,
                    Removed::Leaf(d) => Some(d),
                },
            };
            match leaf {
                Some(d) => slots[i] = Slot::Leaf(d),
                None => {
                    slots.remove(i);
                    *bitmap &= !bit;
                }
            }
            match slots.as_slice() {
                [Slot::Leaf(d)] => Removed::Leaf(d.clone()),
                _ => Removed::Node,
            }
        }
    }
}

enum Frame<'a> {
    Slots(std::slice::Iter<'a, Slot>),
    Defs(std::slice::Iter<'a, Arc<MacroDef>>),
}

fn frame(n: &Node) -> Frame<'_> {
    match n {
        Node::Branch { slots, .. } => Frame::Slots(slots.iter()),
        Node::Collision(defs) => Frame::Defs(defs.iter()),
    }
}

struct Iter<'a> {
    stack: Vec<Frame<'a>>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a Arc<MacroDef>;

    fn next(&mut self) -> Option<&'a Arc<MacroDef>> {
        loop {
            let child = match self.stack.last_mut()? {
                Frame::Defs(it) => match it.next() {
                    Some(d) => return Some(d),
                    None => None,
                },
                Frame::Slots(it) => match it.next() {
                    Some(Slot::Leaf(d)) => return Some(d),
                    Some(Slot::Node(n)) => Some(&**n),
                    None => None,
                },
            };
            match child {
                Some(n) => self.stack.push(frame(n)),
                None => {
                    self.stack.pop();
                }
            }
        }
    }
}

type Visit<'a, 'f> = &'f mut dyn FnMut(Option<&'a Arc<MacroDef>>, Option<&'a Arc<MacroDef>>) -> ControlFlow<()>;

fn diff_nodes<'a>(a: Option<&'a Arc<Node>>, b: Option<&'a Arc<Node>>, f: Visit<'a, '_>) -> ControlFlow<()> {
    match (a, b) {
        (None, None) => ControlFlow::Continue(()),
        (Some(x), Some(y)) if Arc::ptr_eq(x, y) => ControlFlow::Continue(()),
        (Some(x), Some(y)) => {
            visit(1);
            match (&**x, &**y) {
                (Node::Branch { bitmap: ba, slots: sa }, Node::Branch { bitmap: bb, slots: sb }) => {
                    let mut bits = ba | bb;
                    while bits != 0 {
                        let bit = bits & bits.wrapping_neg();
                        bits &= !bit;
                        let sx = (ba & bit != 0).then(|| &sa[index(*ba, bit)]);
                        let sy = (bb & bit != 0).then(|| &sb[index(*bb, bit)]);
                        diff_slots(sx, sy, f)?;
                    }
                    ControlFlow::Continue(())
                }
                _ => diff_entries(&gather_node(x), &gather_node(y), f),
            }
        }
        (x, y) => diff_entries(&x.map(|n| gather_node(n)).unwrap_or_default(), &y.map(|n| gather_node(n)).unwrap_or_default(), f),
    }
}

fn diff_slots<'a>(a: Option<&'a Slot>, b: Option<&'a Slot>, f: Visit<'a, '_>) -> ControlFlow<()> {
    match (a, b) {
        (Some(Slot::Node(x)), Some(Slot::Node(y))) => diff_nodes(Some(x), Some(y), f),
        (Some(Slot::Leaf(x)), Some(Slot::Leaf(y))) if x.name == y.name => {
            if Arc::ptr_eq(x, y) {
                ControlFlow::Continue(())
            } else {
                f(Some(x), Some(y))
            }
        }
        _ => diff_entries(&gather_slot(a), &gather_slot(b), f),
    }
}

/// The differences between two small lists of definitions (where the two tries do not have
/// the same layout, one side holds at most one definition, unless names collide).
fn diff_entries<'a>(a: &[&'a Arc<MacroDef>], b: &[&'a Arc<MacroDef>], f: Visit<'a, '_>) -> ControlFlow<()> {
    visit(a.len() + b.len());
    for x in a {
        match b.iter().find(|y| y.name == x.name) {
            Some(y) if Arc::ptr_eq(x, y) => {}
            Some(y) => f(Some(x), Some(y))?,
            None => f(Some(x), None)?,
        }
    }
    for y in b {
        if !a.iter().any(|x| x.name == y.name) {
            f(None, Some(y))?;
        }
    }
    ControlFlow::Continue(())
}

fn gather_slot(s: Option<&Slot>) -> Vec<&Arc<MacroDef>> {
    match s {
        None => Vec::new(),
        Some(Slot::Leaf(d)) => vec![d],
        Some(Slot::Node(n)) => gather_node(n),
    }
}

fn gather_node(n: &Node) -> Vec<&Arc<MacroDef>> {
    let mut out = Vec::new();
    let mut it = Iter { stack: vec![frame(n)] };
    for d in &mut it {
        out.push(d);
    }
    out
}

/// `n` with the moved definitions replaced; `None` when it has none.
fn relocate_node(n: &Arc<Node>, moved: &FxHashMap<usize, Arc<MacroDef>>, count: &mut usize) -> Option<Arc<Node>> {
    visit(1);
    match &**n {
        Node::Collision(defs) => {
            let before = *count;
            let new: Vec<Arc<MacroDef>> = defs
                .iter()
                .map(|d| match moved.get(&def_identity(d)) {
                    Some(r) => {
                        *count += 1;
                        r.clone()
                    }
                    None => d.clone(),
                })
                .collect();
            (*count > before).then(|| Arc::new(Node::Collision(new)))
        }
        Node::Branch { bitmap, slots } => {
            let mut new: Option<Vec<Slot>> = None;
            for (i, s) in slots.iter().enumerate() {
                let r = match s {
                    Slot::Leaf(d) => moved.get(&def_identity(d)).map(|r| {
                        // A moved definition keeps its name, so the layout stays the same.
                        debug_assert_eq!(r.name, d.name);
                        *count += 1;
                        Slot::Leaf(r.clone())
                    }),
                    Slot::Node(c) => relocate_node(c, moved, count).map(Slot::Node),
                };
                if let Some(r) = r {
                    new.get_or_insert_with(|| slots.clone())[i] = r;
                }
            }
            new.map(|slots| Arc::new(Node::Branch { bitmap: *bitmap, slots }))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::preproc::command_line_define;
    use std::cell::Cell;

    thread_local! {
        /// A mask applied to every hash, while set.
        pub(super) static WEAK: Cell<Option<u64>> = const { Cell::new(None) };
    }

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    fn def(name: &str, body: usize) -> Arc<MacroDef> {
        Arc::new(command_line_define(name, Some(&body.to_string())))
    }

    type Model = FxHashMap<SmolStr, Arc<MacroDef>>;

    /// The layout of a trie: branches by bitmap, leaves and collision lists by name.
    fn shape(t: &MacroTable) -> String {
        fn node(n: &Node, out: &mut String) {
            match n {
                Node::Branch { bitmap, slots } => {
                    out.push_str(&format!("[{bitmap:x}"));
                    for s in slots {
                        match s {
                            Slot::Leaf(d) => out.push_str(&format!(" {}", d.name)),
                            Slot::Node(c) => {
                                out.push(' ');
                                node(c, out);
                            }
                        }
                    }
                    out.push(']');
                }
                Node::Collision(defs) => {
                    let mut names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
                    names.sort_unstable();
                    out.push_str(&format!("{{{}}}", names.join(" ")));
                }
            }
        }
        let mut out = String::new();
        if let Some(r) = &t.root {
            node(r, &mut out);
        }
        out
    }

    fn check(t: &MacroTable, m: &Model, names: &[SmolStr]) {
        assert_eq!(t.len(), m.len());
        assert_eq!(t.is_empty(), m.is_empty());
        assert_eq!(t.root.is_none(), m.is_empty());
        let mut seen = 0;
        for (k, d) in t.iter() {
            assert_eq!(k, &d.name);
            assert!(Arc::ptr_eq(d, &m[k]), "{k}");
            seen += 1;
        }
        assert_eq!(seen, m.len());
        for n in names {
            match (t.get(n), m.get(n)) {
                (Some(a), Some(b)) => assert!(Arc::ptr_eq(a, b)),
                (None, None) => {}
                (a, b) => panic!("{n}: {:?} vs {:?}", a.is_some(), b.is_some()),
            }
            assert_eq!(t.contains_key(n), m.contains_key(n));
        }
        // Canonical: the same layout as a table built from scratch.
        let mut fresh = MacroTable::new();
        for d in m.values() {
            fresh.insert(d.clone());
        }
        assert_eq!(shape(t), shape(&fresh));
    }

    fn model_diff(a: &Model, b: &Model) -> Vec<(SmolStr, usize, usize)> {
        let id = |d: Option<&Arc<MacroDef>>| d.map_or(0, def_identity);
        let mut out: Vec<_> = a
            .keys()
            .chain(b.keys().filter(|k| !a.contains_key(*k)))
            .filter(|k| id(a.get(*k)) != id(b.get(*k)))
            .map(|k| (k.clone(), id(a.get(k)), id(b.get(k))))
            .collect();
        out.sort();
        out
    }

    fn table_diff(a: &MacroTable, b: &MacroTable) -> Vec<(SmolStr, usize, usize)> {
        let mut out = Vec::new();
        let _ = a.diff(b, |x, y| {
            let name = x.or(y).unwrap().name.clone();
            out.push((name, x.map_or(0, def_identity), y.map_or(0, def_identity)));
            ControlFlow::Continue(())
        });
        out.sort();
        out
    }

    fn model_same(a: &Model, b: &Model) -> bool {
        a.len() == b.len() && a.iter().all(|(k, x)| b.get(k).is_some_and(|y| x.same_as(y)))
    }

    /// The table `m` describes with each moved definition replaced, and how many were.
    fn model_relocated(m: &Model, moved: &FxHashMap<usize, Arc<MacroDef>>) -> (Model, usize) {
        let mut count = 0;
        let out = m
            .iter()
            .map(|(k, d)| match moved.get(&def_identity(d)) {
                Some(r) => {
                    count += 1;
                    (k.clone(), r.clone())
                }
                None => (k.clone(), d.clone()),
            })
            .collect();
        (out, count)
    }

    fn run(seed: u64, weak: Option<u64>) {
        WEAK.with(|w| w.set(weak));
        let mut rng = Rng(seed);
        let names: Vec<SmolStr> = (0..400).map(|i| SmolStr::new(format!("M{i}"))).collect();
        let mut t = MacroTable::new();
        let mut m = Model::default();
        let mut snaps: Vec<(MacroTable, Model)> = Vec::new();
        for step in 0..3000 {
            match rng.below(100) {
                0 => {
                    t.clear();
                    m.clear();
                }
                1..=55 => {
                    let n = &names[rng.below(names.len())];
                    let d = def(n, rng.below(3));
                    t.insert(d.clone());
                    m.insert(n.clone(), d);
                }
                56..=85 => {
                    let n = &names[rng.below(names.len())];
                    t.remove(n);
                    m.remove(n);
                }
                86..=95 => snaps.push((t.clone(), m.clone())),
                _ => {
                    if !snaps.is_empty() {
                        (t, m) = snaps[rng.below(snaps.len())].clone();
                    }
                }
            }
            if step % 25 == 0 {
                check(&t, &m, &names);
            } else {
                assert_eq!(t.len(), m.len());
            }
        }
        snaps.push((t, m));
        // Persistence: later changes did not touch the snapshots.
        for (s, sm) in &snaps {
            check(s, sm, &names);
        }
        // One memo for all the comparisons, as a loop over the units of an update uses it.
        let mut memo = SameMemo::default();
        for _ in 0..300 {
            let (a, am) = &snaps[rng.below(snaps.len())];
            let (b, bm) = &snaps[rng.below(snaps.len())];
            assert_eq!(table_diff(a, b), model_diff(am, bm));
            assert_eq!(a.same_definitions(b), model_same(am, bm));
            assert_eq!(a.same_definitions_memo(b, &mut memo), model_same(am, bm));
            // Some of the definitions of either table moved.
            let mut moved: FxHashMap<usize, Arc<MacroDef>> = FxHashMap::default();
            for d in am.values().chain(bm.values()) {
                if rng.below(4) == 0 {
                    moved.insert(def_identity(d), def(&d.name, 0));
                }
            }
            let (an, ac) = a.relocated(&moved);
            let (am2, ac2) = model_relocated(am, &moved);
            assert_eq!(ac, ac2);
            assert_eq!(an.is_none(), ac2 == 0);
            if let Some(an) = &an {
                check(an, &am2, &names);
            }
            let (bn, bc) = b.relocated_from(a, an.as_ref(), ac, &moved);
            let (bm2, bc2) = model_relocated(bm, &moved);
            assert_eq!(bc, bc2);
            assert_eq!(bn.is_none(), bc2 == 0);
            if let Some(bn) = &bn {
                check(bn, &bm2, &names);
            }
            // More definitions move, some of them again: brought up to date, `a` is as
            // relocated from scratch.
            let mut more = moved.clone();
            let mut changes = Vec::new();
            for d in am.values().chain(bm.values()) {
                if rng.below(5) == 0 {
                    let first = !more.contains_key(&def_identity(d));
                    more.insert(def_identity(d), def(&d.name, 0));
                    changes.push((def_identity(d), d.name.clone(), first));
                }
            }
            let (an3, ac3) = a.relocated_more((an.as_ref(), ac), &changes, &more);
            let (am3, ac3m) = model_relocated(am, &more);
            assert_eq!(ac3, ac3m);
            assert_eq!(an3.is_none(), ac3m == 0);
            if let Some(an3) = &an3 {
                check(an3, &am3, &names);
            }
        }
        WEAK.with(|w| w.set(None));
    }

    #[test]
    fn behaves_like_a_hash_map() {
        for seed in 1..=6 {
            run(0x9E37_79B9_7F4A_7C15u64.wrapping_mul(seed), None);
        }
    }

    #[test]
    fn behaves_like_a_hash_map_with_deep_paths_and_collisions() {
        // 7 bits: most names share all their hash bits with others (collision nodes); 0x3ff
        // with some bits above: long single-branch paths.
        for (seed, mask) in [(3, 0x7f), (5, 0x3), (7, 0x3ff | (1 << 63)), (11, 0)] {
            run(0x9E37_79B9_7F4A_7C15u64.wrapping_mul(seed), Some(mask));
        }
    }

    #[test]
    fn a_change_copies_only_its_path() {
        let mut t = MacroTable::new();
        for i in 0..10_000 {
            t.insert(def(&format!("HDR_{i}"), i));
        }
        let all = MacroTable::distinct_nodes([&t]);
        let mut u = t.clone();
        u.insert(def("NEW", 1));
        u.insert(def("HDR_14", 2));
        u.remove("HDR_98");
        let added = MacroTable::distinct_nodes([&t, &u]) - all;
        assert!(added <= 3 * LEVELS as usize, "{added} new nodes");
        assert_eq!(table_diff(&t, &u).len(), 3);
        // Relocating `u` from `t` copies only the paths of the differences.
        let moved: FxHashMap<usize, Arc<MacroDef>> =
            (0..100).map(|i| t.get(&format!("HDR_{}", i * 7)).unwrap()).map(|d| (def_identity(d), def(&d.name, 0))).collect();
        let (tn, tc) = t.relocated(&moved);
        assert_eq!(tc, 100);
        let tn = tn.unwrap();
        let (un, uc) = u.relocated_from(&t, Some(&tn), tc, &moved);
        // HDR_0, HDR_7 .. HDR_693 moved; `u` redefines HDR_14 and has no HDR_98.
        assert_eq!(uc, 98);
        let un = un.unwrap();
        let added = MacroTable::distinct_nodes([&tn, &un]) - MacroTable::distinct_nodes([&tn]);
        assert!(added <= 3 * LEVELS as usize, "{added} new nodes");
        assert!(un.get("HDR_7").is_some_and(|d| Arc::ptr_eq(d, &moved[&def_identity(t.get("HDR_7").unwrap())])));
    }

    #[test]
    fn different_tables_relocate_to_different_tables() {
        let a_old = def("A", 1);
        let moved: FxHashMap<usize, Arc<MacroDef>> = [(def_identity(&a_old), def("A", 1))].into_iter().collect();
        let mut before = MacroTable::new();
        before.insert(a_old);
        // A name defined and undefined again: the same definitions, in another table.
        let mut after = before.clone();
        after.insert(def("TMP", 1));
        after.remove("TMP");
        assert_ne!(after.identity(), before.identity());
        assert!(table_diff(&before, &after).is_empty());
        let (bn, bc) = before.relocated(&moved);
        let (an, ac) = after.relocated_from(&before, bn.as_ref(), bc, &moved);
        assert_eq!(ac, 1);
        assert_ne!(an.unwrap().identity(), bn.as_ref().unwrap().identity());
        // The same table relocates to the same table.
        let (sn, _) = before.relocated_from(&before, bn.as_ref(), bc, &moved);
        assert_eq!(sn.unwrap().identity(), bn.as_ref().unwrap().identity());
    }

    #[test]
    fn a_table_without_moved_definitions_is_not_relocated() {
        let (a_old, b) = (def("A", 1), def("B", 1));
        let moved: FxHashMap<usize, Arc<MacroDef>> = [(def_identity(&a_old), def("A", 1))].into_iter().collect();
        let mut before = MacroTable::new();
        before.insert(a_old.clone());
        let (bn, bc) = before.relocated(&moved);
        assert_eq!(bc, 1);
        // The last moved definition removed, redefined, or cleared away: nothing to relocate.
        let mut removed = before.clone();
        removed.remove("A");
        let mut redefined = before.clone();
        redefined.insert(def("A", 2));
        let mut cleared = before.clone();
        cleared.clear();
        cleared.insert(b.clone());
        for after in [&removed, &redefined, &cleared] {
            assert_eq!(after.relocated_from(&before, bn.as_ref(), bc, &moved).1, 0);
            assert!(after.relocated_from(&before, bn.as_ref(), bc, &moved).0.is_none());
        }
        // Kept, or added after a table that had none.
        let mut kept = before.clone();
        kept.insert(b.clone());
        let (kn, kc) = kept.relocated_from(&before, bn.as_ref(), bc, &moved);
        assert_eq!(kc, 1);
        assert!(Arc::ptr_eq(kn.unwrap().get("A").unwrap(), &moved[&def_identity(&a_old)]));
        let mut empty_then = MacroTable::new();
        empty_then.insert(b);
        let (en, ec) = kept.relocated_from(&empty_then, None, 0, &moved);
        assert_eq!(ec, 1);
        assert_eq!(en.unwrap().len(), 2);
    }
}
