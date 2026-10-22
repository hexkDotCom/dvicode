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
    /// gave them then), the sequences its run-time phases start, and the default sequences it
    /// sets. `factory`: the overrides of the whole build.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn object_nodes(&self, inst: &Inst, pin: Option<&[String]>, seen: &mut FxHashMap<String, u32>, w: &mut Walk, stack: &mut Vec<(u32, u32)>, truncated: &mut bool, scan: &Scan, factory: &Factory) -> Vec<VerifNode> {
        let Some(built) = inst.built.as_deref() else { return Vec::new() };
        let mut out = Vec::new();
        // Configurations, register models and adapters; sequences made while building join the
        // starts of the run-time phases.
        let mut run: Vec<RunEv> = Vec::new();
        for o in &inst.objs {
            let m = &o.made;
            if o.kind == "sequence" {
                run.push(RunEv { body: m.body, guards: m.alts[0].clone(), phase: "build_phase".into(), what: RunWhat::Alloc(m.cr.clone()) });
                continue;
            }
            let name = m.cr.name.clone().unwrap_or_else(|| m.cr.lhs.trim_start_matches("this.").to_string());
            let var = self.resolve_at_span(m.body.unit, m.cr.lhs_head).map(|v| self.decl_key(v));
            let notes = self.config_notes(built, var, &inst.path);
            let node = self.object_node(&name, &inst.path, o.class, o.requested, o.kind, m.cr.lhs_head, &m.alts, o.overrides.clone(), notes, scan);
            if let Some(n) = self.admit(node, pin, seen, w, truncated, |_, _, _| Vec::new()) {
                out.push(n);
            } else if w.stopped() {
                return out;
            }
        }
        // Sequences the run-time phases start, then default sequences.
        let ctx = SeqCtx { derived_first: &inst.chain, path: &inst.path, kids: &inst.children, parent_sqr: None };
        run.extend(built.run.iter().cloned());
        out.extend(self.sequence_nodes(&run, &ctx, pin, seen, w, stack, truncated, scan, factory));
        out.extend(self.default_sequences(built, &inst.path, &inst.children, pin, seen, w, truncated, scan));
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

    /// The sequences made or started by run-time events (`events`, of a component's phases or a
    /// sequence's methods), one node per variable and class, with where each is started.
    #[allow(clippy::too_many_arguments)]
    fn sequence_nodes(&self, events: &[RunEv], ctx: &SeqCtx, pin: Option<&[String]>, seen: &mut FxHashMap<String, u32>, w: &mut Walk, stack: &mut Vec<(u32, u32)>, truncated: &mut bool, scan: &Scan, factory: &Factory) -> Vec<VerifNode> {
        struct Seq {
            var: Option<(u32, u32)>,
            class: Option<DeclRef>,
            requested: Option<DeclRef>,
            overrides: Vec<OverrideStep>,
            name: String,
            at: Span,
            ways: Vec<Vec<Guard>>,
            starts: Vec<StartInfo>,
            made_here: bool,
        }
        let mut seqs: Vec<Seq> = Vec::new();
        let var_of = |body: DeclRef, span: Span| self.resolve_at_span(body.unit, span);
        let declared_class = |body: DeclRef, span: Span| var_of(body, span).and_then(|v| self.type_of(v)).and_then(|r| self.bind_type_param(r, ctx.derived_first, scan)).filter(|c| self.decl(*c).kind == DeclKind::Class);
        for e in events {
            match &e.what {
                RunWhat::Alloc(cr) => {
                    let is_class = |c: &DeclRef| self.decl(*c).kind == DeclKind::Class;
                    let requested = cr
                        .ty
                        .and_then(|s| self.resolve_at_span(e.body.unit, s))
                        .and_then(|r| self.follow_typedef(r))
                        .and_then(|r| self.bind_type_param(r, ctx.derived_first, scan))
                        .filter(is_class)
                        .or_else(|| declared_class(e.body, cr.lhs_head));
                    let Some(req) = requested.filter(|c| self.object_kind(*c) == Some("sequence")) else { continue };
                    let name = cr.name.clone().unwrap_or_else(|| cr.lhs.trim_start_matches("this.").to_string());
                    let (class, overrides) = if cr.factory { self.factory_resolve(factory, req, None, cr.params) } else { (req, Vec::new()) };
                    let var = var_of(e.body, cr.lhs_head).map(|v| self.decl_key(v));
                    let key = self.decl_key(class);
                    match seqs.iter_mut().find(|s| s.var == var && var.is_some() && s.class.map(|c| self.decl_key(c)) == Some(key)) {
                        Some(s) => {
                            if s.ways.len() < MAX_WAYS && !s.ways.contains(&e.guards) {
                                s.ways.push(e.guards.clone());
                            }
                        }
                        None => seqs.push(Seq { var, class: Some(class), requested: Some(req), overrides, name, at: cr.lhs_head, ways: vec![e.guards.clone()], starts: Vec::new(), made_here: true }),
                    }
                }
                RunWhat::Start(st) => {
                    let var = var_of(e.body, st.var).map(|v| self.decl_key(v));
                    let on = match st.kind {
                        StartKind::Create => None,
                        _ => Some(self.sequencer_text(e.body, st, ctx)),
                    };
                    // `uvm_do` and `uvm_create` make the sequence: one of its declared class.
                    let makes = matches!(st.kind, StartKind::Do | StartKind::Create);
                    let found = seqs.iter().position(|s| s.var.is_some() && s.var == var);
                    let i = match found {
                        Some(i) => i,
                        None => {
                            let Some(class) = declared_class(e.body, st.var).filter(|c| self.object_kind(*c) == Some("sequence")) else { continue };
                            let (built_class, overrides) = if makes { self.factory_resolve(factory, class, None, false) } else { (class, Vec::new()) };
                            seqs.push(Seq { var, class: Some(built_class), requested: Some(class), overrides, name: st.var_text.clone(), at: st.var, ways: Vec::new(), starts: Vec::new(), made_here: makes });
                            seqs.len() - 1
                        }
                    };
                    let s = &mut seqs[i];
                    if makes && s.ways.len() < MAX_WAYS && !s.ways.contains(&e.guards) {
                        s.ways.push(e.guards.clone());
                    }
                    if let Some((on, path)) = on {
                        if s.starts.len() < MAX_WAYS {
                            s.starts.push(StartInfo { on, path, phase: e.phase.clone(), conditions: conditions_text(std::slice::from_ref(&e.guards)) });
                        }
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
            let parent_sqr = node.starts.iter().find(|x| x.path).map(|x| x.on.clone());
            let make_children = |w: &mut Walk, node: &mut VerifNode, child_pin: Option<&[String]>| -> Vec<VerifNode> {
                let Some(c) = cls else { return Vec::new() };
                let key = self.decl_key(c);
                if stack.contains(&key) || stack.len() >= 64 {
                    return Vec::new();
                }
                stack.push(key);
                let chain = self.class_chain(c);
                let run = self.sequence_events(&chain, scan);
                let mut kid_seen = FxHashMap::default();
                let sub = SeqCtx { derived_first: &chain, path: &node.path, kids: &[], parent_sqr: parent_sqr.clone() };
                let kids = self.sequence_nodes(&run, &sub, child_pin, &mut kid_seen, w, stack, &mut node.truncated, scan, factory);
                stack.pop();
                kids
            };
            if let Some(n) = self.admit((node, cls), pin, seen, w, truncated, make_children) {
                out.push(n);
            } else if w.stopped() {
                break;
            }
        }
        out
    }

    /// Where a start runs: a tree path when the sequencer argument is a chain of member
    /// handles that lead to a component of the tree (`env.agt.sqr`), else the argument's text.
    /// Returns (text, is a path).
    fn sequencer_text(&self, body: DeclRef, st: &SeqStart, ctx: &SeqCtx) -> (String, bool) {
        let Some(sqr) = &st.sqr else {
            // `uvm_do(s)` / `uvm_send(s)` in a sequence: the parent sequence's sequencer.
            return match &ctx.parent_sqr {
                Some(p) => (p.clone(), true),
                None => ("the parent sequence's sequencer".into(), false),
            };
        };
        let text = tokens_text(sqr);
        if let [n] = &sqr[..] {
            if n.is_kw("null") {
                return ("null (no sequencer)".into(), false);
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
                    return (text, false);
                }
                names.push(x);
            } else if !x.is_punct(".") {
                return (text, false);
            }
        }
        if names.is_empty() || toks.len() % 2 == 0 {
            return (text, false);
        }
        let mut level: &[Inst] = ctx.kids;
        let mut found: Option<&Inst> = None;
        for n in names {
            let Some(d) = self.resolve_at_span(body.unit, n.span) else { return (text, false) };
            let key = self.decl_key(d);
            let Some(next) = level.iter().find(|c| c.var == Some(key)) else { return (text, false) };
            found = Some(next);
            level = &next.children;
        }
        match found {
            Some(n) => (n.path.clone(), true),
            None => (text, false),
        }
    }

    /// Default sequences a component sets in `uvm_config_db`: under the sequencer when the path
    /// names one of its components, else under the component.
    #[allow(clippy::too_many_arguments)]
    fn default_sequences(&self, built: &Built, path: &str, kids: &[Inst], pin: Option<&[String]>, seen: &mut FxHashMap<String, u32>, w: &mut Walk, truncated: &mut bool, scan: &Scan) -> Vec<VerifNode> {
        let mut out = Vec::new();
        for s in &built.sets {
            if s.set.field.as_deref() != Some("default_sequence") {
                continue;
            }
            let wrapper = s.set.ty.iter().any(|x| x.text == "uvm_object_wrapper");
            let class = if wrapper {
                self.type_handle_class(s.body, &s.set.value)
            } else {
                match &s.set.value[..] {
                    [v] => self.resolve_at_span(s.body.unit, v.span).and_then(|d| self.type_of(d)),
                    _ => None,
                }
            };
            let disabled = matches!(&s.set.value[..], [v] if v.is_kw("null"));
            let Some(inst) = &s.set.inst else { continue };
            // `env.agt.sqr.main_phase`: the sequencer and the phase.
            let (target, phase) = match inst.rsplit_once('.') {
                Some((t, p)) if p.ends_with("_phase") => (t.to_string(), p.to_string()),
                _ => (inst.clone(), "run_phase".to_string()),
            };
            let full = match &s.set.ctx[..] {
                [c] if c.is_kw("this") => format!("{path}.{target}"),
                _ => target.clone(),
            };
            let what = if disabled { format!("default sequence disabled for {full} ({phase})") } else { format!("default sequence for {full} ({phase})") };
            let Some(class) = class.filter(|c| self.object_kind(*c) == Some("sequence")) else {
                if disabled {
                    // Shown as a note on nothing: there is no class to show.
                }
                continue;
            };
            let name = self.decl(class).name.to_string();
            let at = s.set.value.first().map_or(s.set.ty.first().map(|t| t.span).unwrap_or_default(), |t| t.span);
            let (mut node, cls) = self.object_node(&name, path, Some(class), None, "sequence", at, std::slice::from_ref(&s.guards), Vec::new(), vec![what], scan);
            let under = find_path(kids, &full).is_some_and(|k| k.kind == "sequencer");
            node.starts = vec![StartInfo { on: full.clone(), path: under || !full.contains(['*', '?']), phase, conditions: Vec::new() }];
            if let Some(n) = self.admit((node, cls), pin, seen, w, truncated, |_, _, _| Vec::new()) {
                out.push(n);
            }
        }
        out
    }

    /// The class of a type handle (`S::type_id::get()`, `S::get_type()`).
    fn type_handle_class(&self, body: DeclRef, t: &[Token]) -> Option<DeclRef> {
        let k = t.iter().position(|x| x.text == "type_id" || x.text == "get_type")?;
        let x = t.get(k.checked_sub(2)?)?;
        if !x.is_ident() {
            return None;
        }
        self.resolve_at_span(body.unit, x.span).and_then(|r| self.follow_typedef(r)).filter(|r| self.decl(*r).kind == DeclKind::Class)
    }

    /// The run-time events of a sequence class (`chain`: it and its base classes below the UVM
    /// library): those of its `pre_start` ... `post_start` methods.
    fn sequence_events(&self, chain: &[DeclRef], scan: &Scan) -> Vec<RunEv> {
        let mut built = Built::default();
        let mut stack = Vec::new();
        let none = FxHashMap::default();
        scan.component_events.set(0);
        for m in SEQUENCE_METHODS {
            if let Some((body, at)) = self.call_target(m, 0, false, chain, scan) {
                self.run_method(body, at, chain, &none, &[], 0, &mut stack, scan, &mut built, Mode::Run(*m));
            }
        }
        built.run
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
    parent_sqr: Option<String>,
}

/// The component at `path` among `kids` and their descendants.
fn find_path<'a>(kids: &'a [Inst], path: &str) -> Option<&'a Inst> {
    for k in kids {
        if k.path == path {
            return Some(k);
        }
        if path.starts_with(&format!("{}.", k.path)) {
            return find_path(&k.children, path);
        }
    }
    None
}
