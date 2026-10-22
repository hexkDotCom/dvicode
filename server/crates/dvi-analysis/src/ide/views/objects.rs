//! UVM objects in the Verification Hierarchy (`TreeQuery::objects`): configurations, register
//! models and adapters made while a component builds, and the sequences its run-time phases (and
//! those sequences) start.

use super::*;

/// The phases after build in which objects (not components) are still made.
pub(super) const LATE_PHASES: &[&str] = &["connect_phase", "end_of_elaboration_phase", "start_of_simulation_phase"];

/// The run-time phases, in the order they run.
pub(super) const RUN_PHASES: &[&str] = &[
    "run_phase",
    "pre_reset_phase",
    "reset_phase",
    "post_reset_phase",
    "pre_configure_phase",
    "configure_phase",
    "post_configure_phase",
    "pre_main_phase",
    "main_phase",
    "post_main_phase",
    "pre_shutdown_phase",
    "shutdown_phase",
    "post_shutdown_phase",
];

/// The methods of a sequence that run when it is started.
const SEQUENCE_METHODS: &[&str] = &["pre_start", "pre_body", "body", "post_body", "post_start"];

/// How a sequence is started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StartKind {
    /// `s.start(sqr, ...)`.
    Call,
    /// `` `uvm_do(s) `` and its variants: made and started.
    Do,
    /// `` `uvm_create(s) ``: made, not started.
    Create,
    /// `` `uvm_send(s) ``, `` `uvm_rand_send(s) ``: started, made before.
    Send,
}

/// A sequence started (or made by a macro), from a method body.
#[derive(Debug, Clone)]
pub(super) struct SeqStart {
    pub kind: StartKind,
    /// The sequence variable.
    pub var: Span,
    pub var_text: String,
    /// The sequencer argument, when given (`s.start(env.agt.sqr)`, `` `uvm_do_on(s, sqr) ``).
    pub sqr: Option<Vec<Token>>,
}

/// A `uvm_config_db#(T)::set(ctx, "inst", "field", value)` call.
#[derive(Debug, Clone)]
pub(super) struct ConfigSet {
    /// The tokens of `T`.
    pub ty: Vec<Token>,
    pub ctx: Vec<Token>,
    pub inst: Option<String>,
    pub field: Option<String>,
    pub value: Vec<Token>,
}

/// The macros that make or start a sequence (or an item), and what they do.
fn sequence_macro(name: &str) -> Option<(StartKind, bool)> {
    // (kind, the second argument is the sequencer)
    Some(match name {
        "uvm_do" | "uvm_do_with" | "uvm_do_pri" | "uvm_do_pri_with" => (StartKind::Do, false),
        "uvm_do_on" | "uvm_do_on_with" | "uvm_do_on_pri" | "uvm_do_on_pri_with" => (StartKind::Do, true),
        "uvm_create" => (StartKind::Create, false),
        "uvm_create_on" => (StartKind::Create, true),
        "uvm_send" | "uvm_send_pri" | "uvm_rand_send" | "uvm_rand_send_with" | "uvm_rand_send_pri" | "uvm_rand_send_pri_with" => (StartKind::Send, false),
        _ => return None,
    })
}

/// Sequence starts and macros in a method body's tokens, with the index of each.
pub(super) fn sequence_starts(t: &[Token]) -> Vec<(usize, SeqStart)> {
    let mut out = Vec::new();
    for i in 0..t.len().saturating_sub(1) {
        let x = &t[i];
        if x.kind == TokKind::Directive {
            let Some((kind, on)) = sequence_macro(&x.text) else { continue };
            if !t[i + 1].is_punct("(") {
                continue;
            }
            let args = call_args(t, i + 1);
            let Some([v]) = args.first().map(|a| &a[..]) else { continue };
            if !v.is_ident() {
                continue;
            }
            let sqr = if on { args.get(1).map(|a| a.to_vec()) } else { None };
            out.push((i, SeqStart { kind, var: v.span, var_text: v.text.to_string(), sqr }));
            continue;
        }
        // `s.start(...)`, `this.s.start(...)`; not `a.b.start(...)`.
        if !(x.is_ident() && x.text == "start" && t[i + 1].is_punct("(") && i >= 2 && t[i - 1].is_punct(".")) {
            continue;
        }
        let v = &t[i - 2];
        if !v.is_ident() || (i >= 3 && t[i - 3].is_punct(".") && !(i >= 4 && t[i - 4].is_kw("this"))) {
            continue;
        }
        let args = call_args(t, i + 1);
        let named = args.first().is_some_and(|a| a.first().is_some_and(|y| y.is_punct(".")));
        let sqr = if named {
            args.iter().find_map(|a| match a {
                [dot, n, open, inner @ .., close] if dot.is_punct(".") && open.is_punct("(") && close.is_punct(")") && n.text == "sequencer" => Some(inner.to_vec()),
                _ => None,
            })
        } else {
            args.first().map(|a| a.to_vec())
        };
        out.push((i, SeqStart { kind: StartKind::Call, var: v.span, var_text: v.text.to_string(), sqr }));
    }
    out
}

/// `uvm_config_db#(T)::set(...)` calls in a method body's tokens, with the index of each.
pub(super) fn config_sets(t: &[Token]) -> Vec<(usize, ConfigSet)> {
    let mut out = Vec::new();
    for i in 0..t.len() {
        if !(t[i].is_ident() && t[i].text == "uvm_config_db" && t.get(i + 1).is_some_and(|x| x.is_punct("#")) && t.get(i + 2).is_some_and(|x| x.is_punct("("))) {
            continue;
        }
        let close = skip_group(t, i + 2);
        if !(t.get(close).is_some_and(|x| x.is_punct("::")) && t.get(close + 1).is_some_and(|x| x.text == "set") && t.get(close + 2).is_some_and(|x| x.is_punct("("))) {
            continue;
        }
        let ty = t[i + 3..close - 1].to_vec();
        let args = call_args(t, close + 2);
        if args.len() != 4 {
            continue;
        }
        let lit = |a: &[Token]| match a {
            [s] if s.kind == TokKind::String => Some(s.text.trim_matches('"').to_string()),
            _ => None,
        };
        out.push((i, ConfigSet { ty, ctx: args[0].to_vec(), inst: lit(args[1]), field: lit(args[2]), value: args[3].to_vec() }));
    }
    out
}

impl Analysis {
    /// The kind of object a class is, when the objects switch shows it: `sequence`,
    /// `register model`, `adapter` or `object` (a configuration). `None` for components,
    /// ports, sequence items, registers and other parts of a register model, UVM library classes
    /// other than sequences and register blocks, and classes whose UVM base does not resolve.
    pub(super) fn object_kind(&self, class: DeclRef) -> Option<&'static str> {
        let mut cur = Some(class);
        for _ in 0..32 {
            let c = cur?;
            let kind = match self.decl(c).name.as_str() {
                "uvm_component" | "uvm_port_base" | "uvm_sequence_item" => return None,
                "uvm_reg" | "uvm_reg_field" | "uvm_mem" | "uvm_reg_map" | "uvm_reg_file" | "uvm_vreg" | "uvm_vreg_field" | "uvm_reg_backdoor" | "uvm_reg_cbs" => return None,
                "uvm_sequence_base" => Some("sequence"),
                "uvm_reg_block" => Some("register model"),
                "uvm_reg_adapter" => Some("adapter"),
                "uvm_object" => Some("object"),
                _ => None,
            };
            if let Some(k) = kind {
                // The library's own objects (events, pools) are left out; its sequences are not.
                return (k == "sequence" || k == "register model" || !self.in_uvm_library(class)).then_some(k);
            }
            cur = self.class_base(c);
        }
        None
    }

    /// The object nodes of a component: the objects its build made (with the classes the factory
    /// gave them then), the sequences it makes and starts, and the default sequences shown under
    /// it. `factory`: the overrides of the whole build.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn object_nodes(&self, inst: &Inst, pin: Option<&[String]>, seen: &mut FxHashMap<String, u32>, w: &mut Walk, stack: &mut Vec<(u32, u32)>, truncated: &mut bool, scan: &Scan, factory: &Factory) -> Vec<VerifNode> {
        let Some(built) = inst.built.as_deref() else { return Vec::new() };
        let mut out = Vec::new();
        // Configurations, register models and adapters; sequences made while building join the
        // starts of the run-time phases, with the class the factory gave them then.
        let mut run: Vec<RunEv> = Vec::new();
        for o in &inst.objs {
            let m = &o.made;
            if o.kind == "sequence" {
                let made = MadeSeq { cr: m.cr.clone(), class: o.class, requested: o.requested, overrides: o.overrides.clone() };
                for g in &o.alts {
                    run.push(RunEv { body: m.body, guards: g.clone(), phase: m.stage.phase().into(), what: RunWhat::Made(Box::new(made.clone())) });
                }
                for (a, class, requested, overrides) in &o.alt_made {
                    let made = MadeSeq { cr: a.cr.clone(), class: *class, requested: *requested, overrides: overrides.clone() };
                    for g in &a.alts {
                        run.push(RunEv { body: a.body, guards: g.clone(), phase: a.stage.phase().into(), what: RunWhat::Made(Box::new(made.clone())) });
                    }
                }
                continue;
            }
            let name = m.cr.name.clone().unwrap_or_else(|| m.cr.lhs.trim_start_matches("this.").to_string());
            let var = self.resolve_at_span(m.body.unit, m.cr.lhs_head).map(|v| self.decl_key(v));
            let notes = self.config_notes(built, var, &inst.path);
            let (mut node, cls) = self.object_node(&name, &inst.path, o.class, o.requested, o.kind, m.cr.lhs_head, &o.alts, o.overrides.clone(), notes, scan);
            node.alternatives = o.alternatives.clone();
            if let Some(n) = self.admit((node, cls), pin, seen, w, truncated, |_, _, _| Vec::new()) {
                out.push(n);
            } else if w.stopped() && pin.is_none() {
                return out;
            }
        }
        run.extend(built.run.iter().cloned());
        let ctx = SeqCtx { derived_first: &inst.chain, path: &inst.path, kids: &inst.children, parent: Vec::new() };
        out.extend(self.sequence_nodes(&run, &ctx, pin, seen, w, stack, truncated, scan, factory));
        out.extend(self.default_nodes(inst, pin, seen, w, stack, truncated, scan, factory));
        out
    }

    /// Notes for an object from the `uvm_config_db` sets of its variable.
    fn config_notes(&self, built: &Built, var: Option<(u32, u32)>, path: &str) -> Vec<String> {
        let Some(var) = var else { return Vec::new() };
        let mut notes = Vec::new();
        for s in &built.sets {
            let [v] = &s.set.value[..] else { continue };
            if self.resolve_at_span(s.body.unit, v.span).map(|d| self.decl_key(d)) != Some(var) {
                continue;
            }
            let inst = s.set.inst.clone().unwrap_or_else(|| "?".into());
            let full = match &s.set.ctx[..] {
                [c] if c.is_kw("this") => if inst.is_empty() { path.to_string() } else { format!("{path}.{inst}") },
                [c] if c.is_kw("null") => inst,
                _ => format!("{} + {inst}", tokens_text(&s.set.ctx)),
            };
            let field = s.set.field.clone().unwrap_or_else(|| "?".into());
            notes.push(format!("set in config_db for {full} as \"{field}\""));
        }
        notes
    }

    /// A node for an object.
    #[allow(clippy::too_many_arguments)]
    fn object_node(&self, name: &str, path: &str, cls: Option<DeclRef>, requested: Option<DeclRef>, kind: &str, at: Span, alts: &[Vec<Guard>], overrides: Vec<OverrideStep>, notes: Vec<String>, scan: &Scan) -> (VerifNode, Option<DeclRef>) {
        let loc = self.to_location(at);
        let def = cls.and_then(|c| self.decl_location(c));
        let declared = requested.filter(|r| Some(self.decl_key(*r)) != cls.map(|c| self.decl_key(c))).map(|r| self.decl(r).name.to_string());
        let node = VerifNode {
            name: name.to_string(),
            path: format!("{path}.{name}"),
            class: cls.map(|c| self.decl(c).name.to_string()).unwrap_or_default(),
            kind: kind.into(),
            uri: loc.as_ref().map(|l| l.uri.to_string()),
            line: loc.as_ref().map_or(0, |l| l.range.start.line),
            character: loc.as_ref().map_or(0, |l| l.range.start.character),
            def_uri: def.as_ref().map(|l| l.uri.to_string()),
            def_line: def.as_ref().map_or(0, |l| l.range.start.line),
            conditions: conditions_text(alts),
            count: alts.first().and_then(|a| self.loops_count(a, scan)),
            loops: alts.first().map(|a| a.iter().filter_map(loop_text).collect()).unwrap_or_default(),
            overrides,
            notes,
            object: true,
            declared,
            ..Default::default()
        };
        (node, cls)
    }

    /// Counts and keeps an object node as the component children are kept: within the limits,
    /// matching the filter or target, or with children that do (`children` makes them).
    #[allow(clippy::too_many_arguments)]
    fn admit(&self, (mut node, cls): (VerifNode, Option<DeclRef>), pin: Option<&[String]>, seen: &mut FxHashMap<String, u32>, w: &mut Walk, truncated: &mut bool, children: impl FnOnce(&mut Walk, &mut VerifNode, Option<&[String]>) -> Vec<VerifNode>) -> Option<VerifNode> {
        node.dup = sibling(seen, &node.name);
        let child_pin = pin_below(pin, &keyed(&node.name, node.dup));
        if (w.stopped() || !w.visit()) && child_pin.is_none() {
            *truncated = true;
            return None;
        }
        let Some(reserved) = w.reserve(child_pin.is_some()) else {
            *truncated = true;
            return None;
        };
        let declared = node.declared.clone().unwrap_or_default();
        let matched = match &w.q.target {
            Some(t) => {
                let m = t.class.as_deref().is_some_and(|c| c == node.class || c == declared) && cls.is_some();
                w.count(m)
            }
            None => w.matched(&[(&node.name, '.'), (&node.class, '.'), (&node.path, '.'), (&declared, '.')]),
        };
        node.children = children(w, &mut node, child_pin);
        if matched || !node.children.is_empty() || child_pin.is_some() {
            Some(node)
        } else {
            if reserved {
                w.nodes_left += 1;
            }
            None
        }
    }

    /// The sequences made or started by run-time events (`events`, of a component or a
    /// sequence, in the order they run): one node per variable and class, with where each is
    /// started. A start goes to what the variable last holds: the allocations since its last
    /// unconditional one.
    #[allow(clippy::too_many_arguments)]
    fn sequence_nodes(&self, events: &[RunEv], ctx: &SeqCtx, pin: Option<&[String]>, seen: &mut FxHashMap<String, u32>, w: &mut Walk, stack: &mut Vec<(u32, u32)>, truncated: &mut bool, scan: &Scan, factory: &Factory) -> Vec<VerifNode> {
        struct Seq {
            class: Option<DeclRef>,
            requested: Option<DeclRef>,
            overrides: Vec<OverrideStep>,
            name: String,
            at: Span,
            ways: Vec<Vec<Guard>>,
            starts: Vec<StartInfo>,
            made_here: bool,
        }
        // A node per variable, class and name.
        type Key = (Option<(u32, u32)>, (u32, u32), String);
        let mut seqs: Vec<Seq> = Vec::new();
        let mut by_key: FxHashMap<Key, usize> = FxHashMap::default();
        // What each variable may hold now, and the sequencer `uvm_create_on` gave it.
        let mut current: FxHashMap<(u32, u32), Vec<(usize, Vec<Guard>)>> = FxHashMap::default();
        // Overrides registered at run time, on top of the build's (copied when there are any).
        let mut local: Option<Factory> = None;
        let mut stored: FxHashMap<(u32, u32), (DeclRef, Vec<Token>)> = FxHashMap::default();
        let var_of = |body: DeclRef, span: Span| self.resolve_at_span(body.unit, span).map(|v| self.decl_key(v));
        let declared_class = |body: DeclRef, span: Span| self.resolve_at_span(body.unit, span).and_then(|v| self.type_of(v)).and_then(|r| self.bind_type_param(r, ctx.derived_first, scan)).filter(|c| self.decl(*c).kind == DeclKind::Class);
        let unconditional = |g: &[Guard]| conditions_text(std::slice::from_ref(&g.to_vec())).is_empty();
        #[allow(clippy::too_many_arguments)]
        fn alloc(seqs: &mut Vec<Seq>, by_key: &mut FxHashMap<Key, usize>, current: &mut FxHashMap<(u32, u32), Vec<(usize, Vec<Guard>)>>, var: Option<(u32, u32)>, key: (u32, u32), name: String, new: impl FnOnce() -> Seq, guards: &[Guard], unconditional: bool) -> usize {
            let i = *by_key.entry((var, key, name)).or_insert_with(|| {
                seqs.push(new());
                seqs.len() - 1
            });
            let s = &mut seqs[i];
            let g = guards.to_vec();
            if g.is_empty() {
                s.ways = vec![Vec::new()];
            } else if !s.ways.iter().any(|w| w.is_empty()) && s.ways.len() < MAX_WAYS && !s.ways.contains(&g) {
                s.ways.push(g);
            }
            s.made_here = true;
            if let Some(v) = var {
                let cur = current.entry(v).or_default();
                if unconditional {
                    *cur = vec![(i, guards.to_vec())];
                } else if !cur.iter().any(|(j, g)| *j == i && g == guards) {
                    cur.push((i, guards.to_vec()));
                }
            }
            i
        }
        for e in events {
            if let RunWhat::Override(call) = &e.what {
                let r = Reg { call: call.clone(), body: e.body, guards: e.guards.clone(), stage: Mode::Run("run") };
                if let Some(entry) = self.factory_entry(&r, ctx.path, true, scan) {
                    local.get_or_insert_with(|| factory.clone()).push(entry);
                }
                continue;
            }
            let factory = local.as_ref().unwrap_or(factory);
            // Allocations: (class built, class requested, overrides, name, where).
            let made: Option<(DeclRef, Option<DeclRef>, Vec<OverrideStep>, String, Span, Option<(u32, u32)>)> = match &e.what {
                RunWhat::Made(ms) => ms.class.filter(|c| self.object_kind(*c) == Some("sequence")).map(|c| (c, ms.requested, ms.overrides.clone(), ms.cr.name.clone().unwrap_or_else(|| ms.cr.lhs.trim_start_matches("this.").to_string()), ms.cr.lhs_head, var_of(e.body, ms.cr.lhs_head))),
                RunWhat::Alloc(cr) => {
                    let is_class = |c: &DeclRef| self.decl(*c).kind == DeclKind::Class;
                    let requested = cr
                        .ty
                        .and_then(|s| self.resolve_at_span(e.body.unit, s))
                        .and_then(|r| self.follow_typedef(r))
                        .and_then(|r| self.bind_type_param(r, ctx.derived_first, scan))
                        .filter(is_class)
                        .or_else(|| declared_class(e.body, cr.lhs_head));
                    requested.filter(|c| self.object_kind(*c) == Some("sequence")).map(|req| {
                        let (class, overrides) = if cr.factory { self.factory_resolve(factory, req, None, cr.params || self.parameterized(req)) } else { (req, Vec::new()) };
                        (class, Some(req), overrides, cr.name.clone().unwrap_or_else(|| cr.lhs.trim_start_matches("this.").to_string()), cr.lhs_head, var_of(e.body, cr.lhs_head))
                    })
                }
                RunWhat::Start(st) if matches!(st.kind, StartKind::Do | StartKind::Create) => declared_class(e.body, st.var).filter(|c| self.object_kind(*c) == Some("sequence")).map(|req| {
                    let (class, overrides) = self.factory_resolve(factory, req, None, self.parameterized(req));
                    (class, Some(req), overrides, st.var_text.clone(), st.var, var_of(e.body, st.var))
                }),
                RunWhat::Start(_) | RunWhat::Override(_) => None,
            };
            if let Some((class, requested, overrides, name, at, var)) = made {
                let key_name = name.clone();
                let new = || Seq { class: Some(class), requested, overrides, name, at, ways: Vec::new(), starts: Vec::new(), made_here: true };
                alloc(&mut seqs, &mut by_key, &mut current, var, self.decl_key(class), key_name, new, &e.guards, unconditional(&e.guards));
            }
            let RunWhat::Start(st) = &e.what else { continue };
            let Some(var) = var_of(e.body, st.var) else { continue };
            if st.kind == StartKind::Create {
                // A new sequence: the sequencer `uvm_create_on` gives, or none.
                match &st.sqr {
                    Some(q) => stored.insert(var, (e.body, q.clone())),
                    None => stored.remove(&var),
                };
                continue;
            }
            // Started without an allocation seen: a handle made elsewhere.
            if current.get(&var).map_or(true, |v| v.is_empty()) {
                let Some(class) = declared_class(e.body, st.var).filter(|c| self.object_kind(*c) == Some("sequence")) else { continue };
                let i = *by_key.entry((Some(var), self.decl_key(class), st.var_text.clone())).or_insert_with(|| {
                    seqs.push(Seq { class: Some(class), requested: Some(class), overrides: Vec::new(), name: st.var_text.clone(), at: st.var, ways: Vec::new(), starts: Vec::new(), made_here: false });
                    seqs.len() - 1
                });
                current.insert(var, vec![(i, Vec::new())]);
            }
            // `uvm_send` uses the sequencer `uvm_create_on` gave.
            let (body, sqr) = match (&st.sqr, st.kind) {
                (None, StartKind::Send) => match stored.get(&var) {
                    Some((b, q)) => (*b, Some(q.clone())),
                    None => (e.body, None),
                },
                _ => (e.body, st.sqr.clone()),
            };
            let ons = self.sequencer_text(body, sqr.as_deref(), ctx);
            let conditions = conditions_text(std::slice::from_ref(&e.guards));
            // What the variable may hold where the start is: an allocation in another branch
            // (neither's conditions within the other's) is not.
            let holds = current.get(&var).cloned().unwrap_or_default();
            for (i, g) in holds.iter().filter(|(_, g)| g.starts_with(&e.guards) || e.guards.starts_with(g)) {
                let _ = g;
                for (on, path) in &ons {
                    let s = &mut seqs[*i];
                    if s.starts.len() < MAX_WAYS && !s.starts.iter().any(|x| x.on == *on && x.phase == e.phase && x.conditions == conditions) {
                        s.starts.push(StartInfo { on: on.clone(), path: *path, phase: e.phase.clone(), conditions: conditions.clone() });
                    }
                }
            }
        }
        let mut out = Vec::new();
        for s in seqs {
            let ways = if s.ways.is_empty() { vec![Vec::new()] } else { s.ways.clone() };
            let (mut node, cls) = self.object_node(&s.name, ctx.path, s.class, s.requested, "sequence", s.at, &ways, s.overrides, Vec::new(), scan);
            node.starts = s.starts;
            if !s.made_here {
                node.allocated = Some("elsewhere".into());
            }
            let parent: Vec<(String, bool)> = node.starts.iter().map(|x| (x.on.clone(), x.path)).collect();
            let make_children = |w: &mut Walk, node: &mut VerifNode, child_pin: Option<&[String]>| -> Vec<VerifNode> {
                match cls {
                    Some(c) => self.sequence_children(c, node, parent, child_pin, w, stack, scan, factory),
                    None => Vec::new(),
                }
            };
            if let Some(n) = self.admit((node, cls), pin, seen, w, truncated, make_children) {
                out.push(n);
            } else if w.stopped() && pin.is_none() {
                break;
            }
        }
        out
    }

    /// The sequences a sequence of class `c` makes and starts (`parent`: the sequencers it is
    /// started on, for `uvm_do` without one).
    #[allow(clippy::too_many_arguments)]
    fn sequence_children(&self, c: DeclRef, node: &mut VerifNode, parent: Vec<(String, bool)>, child_pin: Option<&[String]>, w: &mut Walk, stack: &mut Vec<(u32, u32)>, scan: &Scan, factory: &Factory) -> Vec<VerifNode> {
        let key = self.decl_key(c);
        if stack.contains(&key) || stack.len() >= 64 {
            return Vec::new();
        }
        stack.push(key);
        let chain = self.class_chain(c);
        let (run, cut) = self.sequence_events(&chain, scan);
        if cut {
            node.truncated = true;
        }
        let mut kid_seen = FxHashMap::default();
        let path = node.path.clone();
        let sub = SeqCtx { derived_first: &chain, path: &path, kids: &[], parent };
        let kids = self.sequence_nodes(&run, &sub, child_pin, &mut kid_seen, w, stack, &mut node.truncated, scan, factory);
        stack.pop();
        kids
    }

    /// Where a start runs: tree paths when the sequencer argument is a chain of member handles
    /// that lead to a component of the tree (`env.agt.sqr`), else the argument's text; without
    /// an argument, the sequencers of the parent sequence. Each with whether it is a path.
    fn sequencer_text(&self, body: DeclRef, sqr: Option<&[Token]>, ctx: &SeqCtx) -> Vec<(String, bool)> {
        let Some(sqr) = sqr else {
            // `uvm_do(s)` / `uvm_send(s)` in a sequence: the parent sequence's sequencer.
            return if ctx.parent.is_empty() { vec![("the parent sequence's sequencer".into(), false)] } else { ctx.parent.clone() };
        };
        let text = tokens_text(sqr);
        if let [n] = sqr {
            if n.is_kw("null") {
                return vec![("null (no sequencer)".into(), false)];
            }
        }
        // `a.b.c`, optionally after `this.`: each name the variable of a component below.
        let mut toks: &[Token] = sqr;
        if toks.len() >= 2 && toks[0].is_kw("this") && toks[1].is_punct(".") {
            toks = &toks[2..];
        }
        let mut names = Vec::new();
        for (k, x) in toks.iter().enumerate() {
            if k % 2 == 0 {
                if !x.is_ident() {
                    return vec![(text, false)];
                }
                names.push(x);
            } else if !x.is_punct(".") {
                return vec![(text, false)];
            }
        }
        if names.is_empty() || toks.len() % 2 == 0 {
            return vec![(text, false)];
        }
        let mut level: &[Inst] = ctx.kids;
        let mut found: Option<&Inst> = None;
        for n in names {
            let Some(d) = self.resolve_at_span(body.unit, n.span) else { return vec![(text, false)] };
            let key = self.decl_key(d);
            // A handle made in more than one place: which one is not known.
            let mut hits = level.iter().filter(|c| c.var == Some(key));
            let (Some(next), None) = (hits.next(), hits.next()) else { return vec![(text, false)] };
            found = Some(next);
            level = &next.children;
        }
        match found {
            Some(n) => vec![(n.path.clone(), true)],
            None => vec![(text, false)],
        }
    }

    /// The default sequences shown under a component: those set for it (a sequencer), or set by
    /// it for a sequencer the tree does not have.
    #[allow(clippy::too_many_arguments)]
    fn default_nodes(&self, inst: &Inst, pin: Option<&[String]>, seen: &mut FxHashMap<String, u32>, w: &mut Walk, stack: &mut Vec<(u32, u32)>, truncated: &mut bool, scan: &Scan, factory: &Factory) -> Vec<VerifNode> {
        let mut out = Vec::new();
        for d in &inst.defaults {
            let s = &d.set;
            let setter = self.enclosing_class(s.body.unit, s.body.decl).map(|c| self.decl(c).name.to_string()).unwrap_or_default();
            let at = s.set.value.first().map_or(s.set.ty.first().map(|t| t.span).unwrap_or_default(), |t| t.span);
            let disabled = matches!(&s.set.value[..], [v] if v.is_kw("null"));
            let note = match (disabled, d.placed) {
                (true, _) => format!("default sequence disabled for {} ({}) by {setter}", d.target, d.phase),
                (false, true) => format!("default sequence for {} (set in {setter})", d.phase),
                (false, false) => format!("configured default sequence for {} ({})", d.target, d.phase),
            };
            if disabled {
                let (node, _) = self.object_node("default_sequence", &inst.path, None, None, "sequence", at, std::slice::from_ref(&s.guards), Vec::new(), vec![note], scan);
                if let Some(n) = self.admit((node, None), pin, seen, w, truncated, |_, _, _| Vec::new()) {
                    out.push(n);
                }
                continue;
            }
            // A wrapper (`S::type_id::get()`), made through the factory when the phase starts;
            // or a sequence object.
            let wrapper = s.set.ty.iter().any(|x| x.text == "uvm_object_wrapper");
            let (requested, params) = if wrapper {
                match self.type_handle(s.body, &s.set.value) {
                    Some((c, p)) => (Some(c), p),
                    None => (None, false),
                }
            } else {
                // A sequence object: the class it was made as, else its variable's type.
                match &s.set.value[..] {
                    [v] => {
                        let var = self.resolve_at_span(s.body.unit, v.span);
                        (d.made_class.or_else(|| var.and_then(|x| self.type_of(x))), false)
                    }
                    _ => (None, false),
                }
            };
            let Some(req) = requested.filter(|c| self.object_kind(*c) == Some("sequence")) else { continue };
            // The sequencer makes it, named after the type, in its own context.
            let lookup = d.placed.then(|| format!("{}.{}", d.target, self.decl(req).name));
            let (class, overrides) = if wrapper { self.factory_resolve(factory, req, lookup.as_deref(), params || self.parameterized(req)) } else { (req, Vec::new()) };
            let name = self.decl(class).name.to_string();
            let (mut node, cls) = self.object_node(&name, &inst.path, Some(class), Some(req), "sequence", at, std::slice::from_ref(&s.guards), overrides, vec![note], scan);
            node.starts = vec![StartInfo { on: d.target.clone(), path: d.placed, phase: d.phase.clone(), conditions: Vec::new() }];
            let parent = vec![(d.target.clone(), d.placed)];
            let make_children = |w: &mut Walk, node: &mut VerifNode, child_pin: Option<&[String]>| self.sequence_children(class, node, parent, child_pin, w, stack, scan, factory);
            if let Some(n) = self.admit((node, cls), pin, seen, w, truncated, make_children) {
                out.push(n);
            }
        }
        out
    }

    /// The run-time events of a sequence class (`chain`: it and its base classes below the UVM
    /// library): those of its `pre_start` ... `post_start` methods; and whether some were not
    /// followed.
    fn sequence_events(&self, chain: &[DeclRef], scan: &Scan) -> (Vec<RunEv>, bool) {
        let mut built = Built::default();
        let mut stack = Vec::new();
        let none = FxHashMap::default();
        scan.component_events.set(0);
        scan.cut.set(false);
        for m in SEQUENCE_METHODS {
            if let Some((body, at)) = self.call_target(m, 0, false, chain, scan) {
                self.run_method(body, at, chain, &none, &[], 0, &mut stack, scan, &mut built, Mode::Run(*m));
            }
        }
        (built.run, scan.cut.get())
    }

    /// A class and its base classes below the UVM library, most derived first.
    pub(super) fn class_chain(&self, class: DeclRef) -> Vec<DeclRef> {
        let mut chain = Vec::new();
        let mut cur = Some(class);
        while let Some(c) = cur {
            if chain.len() >= 32 || self.in_uvm_library(c) {
                break;
            }
            chain.push(c);
            cur = self.class_base(c);
        }
        chain
    }
}

/// Where sequences are made: the class whose methods make them, the node they go under, the
/// component children (for sequencers), and the sequencer of the sequence they run in.
struct SeqCtx<'a> {
    derived_first: &'a [DeclRef],
    path: &'a str,
    kids: &'a [Inst],
    /// The sequencers of the sequence they run in (each a path or the text of an argument).
    parent: Vec<(String, bool)>,
}

/// A sequence made while a component builds, with the class the factory gave it then.
#[derive(Debug, Clone)]
pub(super) struct MadeSeq {
    pub cr: Creation,
    pub class: Option<DeclRef>,
    pub requested: Option<DeclRef>,
    pub overrides: Vec<OverrideStep>,
}
