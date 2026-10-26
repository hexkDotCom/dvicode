//! TLM connections in the Verification Hierarchy: the `connect()` calls the components make
//! (mostly in `connect_phase`), resolved to the ports, exports and imps of the tree, checked
//! against UVM's rules, and what each endpoint may reach.
//!
//! Only what can be shown is an edge: both ends resolved through member handles of the
//! component that makes the call (an array element only as the same select it was made with),
//! a call before the end of elaboration, a receiver allowed to connect to that provider, and
//! a provider whose interface has every operation the requester's has. Everything else is
//! shown with its reason. A link inside an `if`, `case`, or a loop that may not run makes what
//! it leads to "possible".

use super::*;

/// The endpoints the UVM library's own constructors make (the model does not scan the library):
/// class, members (the first is the endpoint; the others are handles to the same one, as
/// `blocking_put_export = put_export`), and the name UVM gives it.
const LIBRARY_ENDPOINTS: &[(&str, &[&str], &str)] = &[
    ("uvm_driver", &["seq_item_port"], "seq_item_port"),
    ("uvm_driver", &["rsp_port"], "rsp_port"),
    ("uvm_push_driver", &["req_export"], "req_export"),
    ("uvm_push_driver", &["rsp_port"], "rsp_port"),
    ("uvm_sequencer", &["seq_item_export"], "seq_item_export"),
    ("uvm_sequencer_param_base", &["rsp_export"], "rsp_export"),
    ("uvm_push_sequencer", &["req_port"], "req_port"),
    ("uvm_subscriber", &["analysis_export"], "analysis_imp"),
    ("uvm_tlm_analysis_fifo", &["analysis_export"], "analysis_export"),
    ("uvm_tlm_fifo_base", &["put_export", "blocking_put_export", "nonblocking_put_export"], "put_export"),
    (
        "uvm_tlm_fifo_base",
        &[
            "get_peek_export",
            "blocking_get_peek_export",
            "nonblocking_get_peek_export",
            "get_export",
            "blocking_get_export",
            "nonblocking_get_export",
            "peek_export",
            "blocking_peek_export",
            "nonblocking_peek_export",
        ],
        "get_peek_export",
    ),
    ("uvm_tlm_fifo_base", &["put_ap"], "put_ap"),
    ("uvm_tlm_fifo_base", &["get_ap"], "get_ap"),
];

/// Connection children listed per node at most.
const MAX_LISTED: usize = 20;
/// Links resolved per model at most.
const MAX_LINKS: usize = 100_000;
/// Implementations listed per endpoint at most.
const MAX_REACHED: usize = 10;
/// Steps of the reach search per model at most.
const MAX_VISITS: usize = 200_000;

/// A `<chain>.connect(<chain>)` in a method body.
#[derive(Debug, Clone)]
pub(super) struct ConnectCall {
    /// The receiver (the requester).
    pub src: Vec<Token>,
    /// The argument (the provider), without a `.provider(...)` name.
    pub dst: Vec<Token>,
    /// The receiver is a chain of handles from the statement's start (`get_x().ap` is not).
    pub src_ok: bool,
    /// The `connect` name.
    pub at: Span,
}

/// A connect call reached while a component's phases run.
#[derive(Debug, Clone)]
pub(super) struct ConnEv {
    pub body: DeclRef,
    pub guards: Vec<Guard>,
    pub call: ConnectCall,
    pub phase: &'static str,
}

/// Whether a statement can start after `t[k]` (`k`: the token before the chain).
fn statement_starts_after(t: &[Token], k: usize) -> bool {
    let b = &t[k];
    if b.is_punct(";") || b.is_punct(")") || b.is_punct("{") {
        return true;
    }
    if ["begin", "end", "else", "do", "fork", "join", "join_any", "join_none", "endcase"].iter().any(|w| b.is_kw(w)) {
        return true;
    }
    // A label: `begin : name`, `name :` before the statement, a `case` item's `:`.
    b.is_punct(":") || (b.is_ident() && k > 0 && t[k - 1].is_punct(":"))
}

/// The `connect()` calls of a method body: (token index, call).
pub(super) fn connect_calls(t: &[Token]) -> Vec<(usize, ConnectCall)> {
    let mut out = Vec::new();
    for i in 2..t.len() {
        if !(t[i].is_ident() && t[i].text == "connect" && t[i - 1].is_punct(".") && t.get(i + 1).is_some_and(|x| x.is_punct("("))) {
            continue;
        }
        // The receiver: names, `.` and `[...]` back from the dot.
        let mut j = i - 1;
        let mut ok = true;
        loop {
            // `j` is at a `.`: what comes before it is a name, perhaps with selects.
            if j == 0 {
                ok = false;
                break;
            }
            let mut k = j - 1;
            while t[k].is_punct("]") {
                let mut depth = 0i32;
                loop {
                    if t[k].is_punct("]") {
                        depth += 1;
                    } else if t[k].is_punct("[") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    if k == 0 {
                        break;
                    }
                    k -= 1;
                }
                if k == 0 || depth != 0 {
                    ok = false;
                    break;
                }
                k -= 1;
            }
            if !ok {
                break;
            }
            if !(t[k].is_ident() || t[k].is_kw("this") || t[k].is_kw("super")) {
                ok = false;
                j = k;
                break;
            }
            j = k;
            if k > 0 && t[k - 1].is_punct(".") {
                j = k - 1;
                continue;
            }
            break;
        }
        // `super.connect(...)`: the base class's callback, no TLM call (`super.ap.connect` is).
        if t[j].is_kw("super") && j + 2 == i {
            continue;
        }
        // What comes before the chain: a statement start, else not a chain of handles
        // (`get_x().ap`, `a[f()].b` were cut above).
        let starts = j == 0 || statement_starts_after(t, j - 1);
        // A receiver that is no chain is shown whole: from the statement's start.
        let from = if ok && starts {
            j
        } else {
            let mut s = j.min(i - 1);
            while s > 0 && !(t[s - 1].is_punct(";") || t[s - 1].is_kw("begin") || t[s - 1].is_kw("end")) {
                s -= 1;
            }
            s
        };
        let src = t[from..i - 1].to_vec();
        // The argument: up to the matching `)`.
        let open = i + 1;
        let close = skip_group(t, open).saturating_sub(1);
        if close <= open || close >= t.len() {
            continue;
        }
        let mut dst = t[open + 1..close].to_vec();
        // `.provider(x)`.
        if dst.len() >= 4 && dst[0].is_punct(".") && dst[1].is_ident() && dst[2].is_punct("(") && dst.last().is_some_and(|x| x.is_punct(")")) {
            dst = dst[3..dst.len() - 1].to_vec();
        }
        out.push((i, ConnectCall { src, dst, src_ok: ok && starts, at: t[i].span }));
    }
    out
}

/// What an endpoint is to UVM: requester (`port`), forwarder (`export`), implementation (`imp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Port,
    Export,
    Imp,
    Unknown,
}

// The operations of TLM interfaces, as UVM's interface masks.
const B_PUT: u32 = 1;
const NB_PUT: u32 = 2;
const B_GET: u32 = 4;
const NB_GET: u32 = 8;
const B_PEEK: u32 = 16;
const NB_PEEK: u32 = 32;
const B_TRANSPORT: u32 = 64;
const NB_TRANSPORT: u32 = 128;
const WRITE: u32 = 256;
const SEQ_ITEM: u32 = 512;

/// An endpoint class's role, interface name and operations, from a UVM class name: the first
/// `port`/`export`/`imp` word is the role (what follows is a `uvm_*_imp_decl` suffix):
/// `uvm_blocking_put_port` → port, `blocking_put`; `uvm_analysis_imp_port` → imp, `analysis`.
fn role_of_name(name: &str) -> Option<(Role, String, u32)> {
    let rest = name.strip_prefix("uvm_")?;
    let words: Vec<&str> = rest.split('_').collect();
    let at = words.iter().position(|w| matches!(*w, "port" | "export" | "imp"))?;
    let role = match words[at] {
        "port" => Role::Port,
        "export" => Role::Export,
        _ => Role::Imp,
    };
    let iface = words[..at].join("_");
    let (blocking, nonblocking) = (iface.starts_with("blocking_"), iface.starts_with("nonblocking_"));
    let base = iface.trim_start_matches("blocking_").trim_start_matches("nonblocking_");
    let both = match base {
        "put" => B_PUT | NB_PUT,
        "get" => B_GET | NB_GET,
        "peek" => B_PEEK | NB_PEEK,
        "get_peek" => B_GET | NB_GET | B_PEEK | NB_PEEK,
        "transport" => B_TRANSPORT | NB_TRANSPORT,
        "master" | "slave" => B_PUT | NB_PUT | B_GET | NB_GET | B_PEEK | NB_PEEK,
        "analysis" => WRITE,
        "seq_item_pull" => SEQ_ITEM,
        _ => return None,
    };
    const BLOCKING: u32 = B_PUT | B_GET | B_PEEK | B_TRANSPORT;
    const NONBLOCKING: u32 = NB_PUT | NB_GET | NB_PEEK | NB_TRANSPORT;
    let mask = if blocking && both & BLOCKING != 0 {
        both & BLOCKING
    } else if nonblocking && both & NONBLOCKING != 0 {
        both & NONBLOCKING
    } else {
        both
    };
    Some((role, iface, mask))
}

/// One connection found, from the component that makes it.
#[derive(Debug, Clone)]
pub(super) struct Link {
    pub from: Result<String, String>,
    pub to: Result<String, String>,
    pub from_text: String,
    pub to_text: String,
    pub at: Span,
    pub conditional: bool,
    /// The `if`/`case` conditions around the call (`?`: not known).
    pub conditions: Vec<String>,
    pub phase: &'static str,
    /// `None`: an edge; else why it is not one.
    pub not: Option<String>,
    /// The component that makes the call.
    pub by: String,
}

impl Link {
    /// Both ends resolved and UVM allows it.
    fn edge(&self) -> bool {
        self.not.is_none() && self.from.is_ok() && self.to.is_ok()
    }
}

/// An endpoint's kind: role, interface name, operations, and whether the UVM library makes it
/// (what reaches it is handled inside the library).
#[derive(Debug, Clone)]
pub(super) struct EndKind {
    pub role: Role,
    pub iface: Option<String>,
    pub mask: Option<u32>,
    pub library: bool,
}

/// The connections of a model and what each endpoint may reach.
#[derive(Default)]
pub(super) struct Links {
    pub links: Vec<Link>,
    /// By endpoint path: indexes of the links that name it (either end).
    pub at: FxHashMap<String, Vec<usize>>,
    /// By component path: the links it makes whose receiver is no endpoint.
    pub loose: FxHashMap<String, Vec<usize>>,
    pub kinds: FxHashMap<String, EndKind>,
    /// What an endpoint may reach.
    pub reach: FxHashMap<String, Reach>,
    /// `connect()` calls that could not be followed (unresolved, unsupported receivers).
    pub unresolved: usize,
    /// The links were cut at their limit.
    pub cut: bool,
}

/// What an endpoint may reach: implementations (and library endpoints), whether only possibly,
/// how many more than listed, and whether the search was cut or met a call it could not follow.
#[derive(Debug, Clone, Default)]
pub(super) struct Reach {
    pub ends: Vec<String>,
    pub possible: bool,
    pub more: usize,
    pub incomplete: bool,
}

impl Analysis {
    /// The library's own endpoints of every component of the tree (as port children).
    pub(super) fn add_library_endpoints(&self, inst: &mut Inst, depth: usize) {
        if depth > 64 {
            return;
        }
        for child in inst.children.iter_mut().filter(|c| c.kind != "port") {
            self.add_library_endpoints(child, depth + 1);
        }
        let Some(class) = inst.class else { return };
        let mut cur = Some(class);
        let mut steps = 0;
        let mut adds: Vec<(DeclRef, Vec<(u32, u32)>, &str)> = Vec::new();
        while let Some(c) = cur {
            steps += 1;
            if steps > 32 {
                break;
            }
            let name = self.decl(c).name.to_string();
            for &(_, members, uvm_name) in LIBRARY_ENDPOINTS.iter().filter(|(l, _, _)| *l == name) {
                let found: Vec<DeclRef> = members.iter().filter_map(|m| self.member_of(c, m)).filter(|m| self.decl(*m).kind == DeclKind::Variable).collect();
                let Some(&main) = found.first() else { continue };
                let keys: Vec<(u32, u32)> = found.iter().map(|m| self.decl_key(*m)).collect();
                let taken = |k: &(u32, u32)| inst.children.iter().any(|x| x.var == Some(*k) || x.aliases.contains(k)) || adds.iter().any(|(_, ks, _)| ks.contains(k));
                if keys.iter().any(taken) {
                    continue;
                }
                adds.push((main, keys, uvm_name));
            }
            cur = self.class_base(c);
        }
        for (m, keys, uvm_name) in adds {
            let class = self.type_of(m).and_then(|t| self.follow_typedef(t));
            inst.children.push(Inst {
                name: uvm_name.to_string(),
                path: format!("{}.{uvm_name}", inst.path),
                path_known: inst.path_known,
                class,
                requested: class,
                kind: "port",
                certain: inst.certain,
                alts: vec![Vec::new()],
                var: Some(keys[0]),
                aliases: keys[1..].to_vec(),
                member: Some(m),
                ..Default::default()
            });
        }
    }

    /// An endpoint's kind, from its class chain.
    fn end_kind(&self, node: &Inst) -> EndKind {
        let mut cur = node.class;
        let mut steps = 0;
        while let Some(c) = cur {
            steps += 1;
            if steps > 32 {
                break;
            }
            if let Some((role, iface, mask)) = role_of_name(&self.decl(c).name) {
                return EndKind { role, iface: Some(iface), mask: Some(mask), library: node.member.is_some() };
            }
            cur = self.class_base(c);
        }
        EndKind { role: Role::Unknown, iface: None, mask: None, library: node.member.is_some() }
    }

    /// The node a chain of handles leads to from `inst` (in the method `body`), or why not.
    fn endpoint<'a>(&self, inst: &'a Inst, body: DeclRef, toks: &[Token]) -> Result<&'a Inst, String> {
        let text = tokens_text(toks);
        let mut t = toks;
        if t.len() >= 2 && (t[0].is_kw("this") || t[0].is_kw("super")) && t[1].is_punct(".") {
            t = &t[2..];
        }
        if t.is_empty() {
            return Err(format!("`{text}` is not a handle"));
        }
        let mut level: &'a [Inst] = &inst.children;
        // The class a name is a member of: the component's, then each node's as made (a handle
        // of a type parameter, `DRIVER_T driver`, is the class the model built there).
        let mut class = inst.class;
        let mut first = true;
        let mut k = 0;
        loop {
            let Some(name) = t.get(k).filter(|x| x.is_ident()) else { return Err(format!("`{text}` is not a chain of handles")) };
            k += 1;
            // Its selects, as written.
            let sel_start = k;
            while t.get(k).is_some_and(|x| x.is_punct("[")) {
                k = skip_group(t, k);
            }
            let select = tokens_text(&t[sel_start..k.min(t.len())]).replace(' ', "");
            let lexical = || self.resolve_at_span(body.unit, name.span);
            let member = || class.and_then(|c| self.member_of(c, &name.text));
            let d = if first { lexical().or_else(member) } else { member().or_else(lexical) };
            let Some(d) = d else { return Err(format!("`{}` is not resolved", name.text)) };
            first = false;
            let key = self.decl_key(d);
            let mut hits = level.iter().filter(|c| c.var == Some(key) || c.aliases.contains(&key));
            let node = match (hits.next(), hits.next()) {
                (Some(n), None) => n,
                (None, _) => return Err(format!("`{}` is no component or port this component makes", name.text)),
                (Some(_), Some(_)) => return Err(format!("`{}` is made in more than one place", name.text)),
            };
            // An array made in a loop is one node (`agents[i]`): only the same select, as its
            // creation's handle has it, is that node.
            let made_select = node.made.as_ref().map(|m| {
                let last = m.cr.lhs.rsplit('.').next().unwrap_or(&m.cr.lhs);
                last.find('[').map(|p| last[p..].replace(' ', "")).unwrap_or_default()
            });
            let made_select = made_select.unwrap_or_default();
            if made_select != select {
                return Err(if made_select.is_empty() {
                    format!("`{}{select}` selects in what is no array of components", name.text)
                } else if select.is_empty() {
                    format!("`{}` is an array (made as `{}{made_select}`)", name.text, name.text)
                } else {
                    format!("`{}{select}`: an element other than the one made as `{}{made_select}` is not followed", name.text, name.text)
                });
            }
            level = &node.children;
            class = node.class;
            match t.get(k) {
                None if node.kind != "port" => return Err(format!("`{text}` is a component, not a port, export or imp")),
                None => return Ok(node),
                Some(x) if x.is_punct(".") => k += 1,
                Some(_) => return Err(format!("`{text}` is not a chain of handles")),
            }
        }
    }

    /// The connections the components of a model make, checked, and what each endpoint may
    /// reach.
    pub(super) fn tlm_links(&self, root: &Inst, scan: &Scan) -> Links {
        let mut l = Links::default();
        self.collect_links(root, &mut l, scan, 0);
        let mut next: FxHashMap<&str, Vec<(&str, bool)>> = FxHashMap::default();
        // Calls from an endpoint that could not be followed: what it reaches is not all known.
        let mut open: FxHashSet<&str> = FxHashSet::default();
        for x in &l.links {
            match (&x.from, &x.to) {
                (Ok(a), Ok(b)) if x.not.is_none() => next.entry(a.as_str()).or_default().push((b.as_str(), x.conditional)),
                (Ok(a), Err(_)) if x.not.is_none() => {
                    open.insert(a.as_str());
                }
                _ => {}
            }
        }
        let mut memo: FxHashMap<String, Reach> = FxHashMap::default();
        let mut visits = 0usize;
        let starts: Vec<String> = l.kinds.iter().filter(|(_, k)| matches!(k.role, Role::Port | Role::Export)).map(|(p, _)| p.clone()).collect();
        for s in starts {
            let mut on_path = FxHashSet::default();
            let r = reach(&s, &next, &open, &l.kinds, &mut memo, &mut on_path, 0, &mut visits);
            l.reach.insert(s, r);
        }
        l
    }

    fn collect_links(&self, inst: &Inst, l: &mut Links, scan: &Scan, depth: usize) {
        if depth > 64 || self.cancelled() {
            l.cut = true;
            return;
        }
        for c in &inst.children {
            if c.kind == "port" {
                l.kinds.insert(c.path.clone(), self.end_kind(c));
            }
        }
        if let Some(built) = &inst.built {
            for ev in &built.connects {
                if l.links.len() >= MAX_LINKS {
                    l.cut = true;
                    break;
                }
                let from_n = if ev.call.src_ok { self.endpoint(inst, ev.body, &ev.call.src) } else { Err(format!("`{}` is not a chain of handles from this component", tokens_text(&ev.call.src))) };
                let to_n = self.endpoint(inst, ev.body, &ev.call.dst);
                let not = if !matches!(ev.phase, "new" | "build_phase" | "connect_phase") {
                    Some(format!("in {}: UVM ignores a connection made after the end of elaboration", ev.phase))
                } else if let (Ok(a), Ok(b)) = (&from_n, &to_n) {
                    let (ka, kb) = (self.end_kind(a), self.end_kind(b));
                    if a.path == b.path {
                        Some("an endpoint cannot connect to itself".into())
                    } else if ka.role == Role::Imp {
                        Some("an imp cannot connect to anything".into())
                    } else if ka.role == Role::Export && kb.role == Role::Port {
                        Some("an export cannot connect to a port".into())
                    } else if let (Some(ma), Some(mb)) = (ka.mask, kb.mask) {
                        // The provider must offer every operation the requester calls.
                        (ma & !mb != 0).then(|| format!("its `{}` interface does not offer what `{}` calls", kb.iface.clone().unwrap_or_default(), ka.iface.clone().unwrap_or_default()))
                    } else {
                        None
                    }
                } else {
                    None
                };
                let from = from_n.map(|n| n.path.clone());
                let to = to_n.map(|n| n.path.clone());
                if from.is_err() || to.is_err() {
                    l.unresolved += 1;
                }
                // A loop that surely runs runs it; an `if`, `case` or another loop may not.
                let ifs = ev.guards.iter().any(|g| !matches!(g, Guard::Loop { .. }));
                let conditional = ifs || !self.loops_sure(&ev.guards, scan);
                let mut conditions = conditions_text(std::slice::from_ref(&ev.guards)).into_iter().next().unwrap_or_default();
                if conditional && !self.loops_sure(&ev.guards, scan) {
                    conditions.push("the loop runs".into());
                }
                let k = l.links.len();
                let link = Link { from_text: tokens_text(&ev.call.src), to_text: tokens_text(&ev.call.dst), from, to, at: ev.call.at, conditional, conditions, phase: ev.phase, not, by: inst.path.clone() };
                let mut named = false;
                let mut ends: Vec<&String> = [&link.from, &link.to].into_iter().flatten().collect();
                ends.dedup();
                for end in ends {
                    l.at.entry(end.clone()).or_default().push(k);
                    named = true;
                }
                if !named || link.from.is_err() {
                    l.loose.entry(inst.path.clone()).or_default().push(k);
                }
                l.links.push(link);
            }
        }
        for c in inst.children.iter().filter(|c| c.kind != "port") {
            self.collect_links(c, l, scan, depth + 1);
        }
    }
}

/// What an endpoint may reach (cut at depth 64 and the visit budget).
#[allow(clippy::too_many_arguments)]
fn reach(at: &str, next: &FxHashMap<&str, Vec<(&str, bool)>>, open: &FxHashSet<&str>, kinds: &FxHashMap<String, EndKind>, memo: &mut FxHashMap<String, Reach>, on_path: &mut FxHashSet<String>, depth: usize, visits: &mut usize) -> Reach {
    if let Some(r) = memo.get(at) {
        return r.clone();
    }
    *visits += 1;
    if depth > 64 || *visits > MAX_VISITS || !on_path.insert(at.to_string()) {
        return Reach { incomplete: true, ..Default::default() };
    }
    let mut r = Reach { incomplete: open.contains(at), ..Default::default() };
    let mut any = false;
    let mut all_possible = true;
    for &(to, cond) in next.get(at).map(|v| v.as_slice()).unwrap_or(&[]) {
        // An imp, or an endpoint the library makes (what it leads to is inside the library),
        // ends the search.
        let terminal = kinds.get(to).is_some_and(|k| k.role == Role::Imp || k.library);
        let sub = if terminal { Reach { ends: vec![to.to_string()], ..Default::default() } } else { reach(to, next, open, kinds, memo, on_path, depth + 1, visits) };
        r.incomplete |= sub.incomplete;
        r.more += sub.more;
        if !sub.ends.is_empty() {
            any = true;
            all_possible &= cond || sub.possible;
        }
        for e in sub.ends {
            if r.ends.contains(&e) {
                continue;
            }
            if r.ends.len() < MAX_REACHED {
                r.ends.push(e);
            } else {
                r.more += 1;
            }
        }
    }
    r.possible = any && all_possible;
    on_path.remove(at);
    if !r.incomplete || depth == 0 {
        memo.insert(at.to_string(), r.clone());
    }
    r
}

/// The connection children of an endpoint node and the status it shows.
pub(super) fn endpoint_view(a: &Analysis, path: &str, l: &Links) -> (Option<String>, Vec<VerifNode>) {
    let kind = l.kinds.get(path).cloned().unwrap_or(EndKind { role: Role::Unknown, iface: None, mask: None, library: false });
    let idx = l.at.get(path).cloned().unwrap_or_default();
    let mut kids = Vec::new();
    for &k in idx.iter().take(MAX_LISTED) {
        kids.push(link_node(a, &l.links[k], Some(path)));
    }
    if idx.len() > MAX_LISTED {
        kids.push(VerifNode { name: format!("… {} more", idx.len() - MAX_LISTED), kind: "connection".into(), ..Default::default() });
    }
    let edge = |k: usize, from: bool| {
        let x = &l.links[k];
        x.edge() && if from { x.from.as_deref() == Ok(path) } else { x.to.as_deref() == Ok(path) }
    };
    let out_edges = idx.iter().filter(|&&k| edge(k, true)).count();
    let in_edges = idx.iter().filter(|&&k| edge(k, false)).count();
    let iface = kind.iface.map(|f| format!("{f} ")).unwrap_or_default();
    let cut = if l.cut { " (connections cut at a limit)" } else { "" };
    let status = match kind.role {
        Role::Imp => {
            if in_edges == 0 {
                format!("{iface}imp · nothing found connecting to it{cut}")
            } else {
                format!("{iface}imp · {in_edges} connection{} to it{cut}", if in_edges == 1 { "" } else { "s" })
            }
        }
        Role::Port | Role::Export => {
            let what = if kind.role == Role::Port { "port" } else { "export" };
            let r = l.reach.get(path).cloned().unwrap_or_default();
            if out_edges == 0 && kind.library && kind.role == Role::Export {
                format!("{iface}{what} · handled inside the UVM library{cut}")
            } else if out_edges == 0 && l.unresolved > 0 {
                format!("{iface}{what} · no connection found ({} connect() call{} not followed){cut}", l.unresolved, if l.unresolved == 1 { "" } else { "s" })
            } else if out_edges == 0 {
                format!("{iface}{what} · no connection found{cut}")
            } else if r.ends.is_empty() && r.incomplete {
                format!("{iface}{what} · what it reaches is not known (a call on the way not followed, or the search cut){cut}")
            } else if r.ends.is_empty() {
                format!("{iface}{what} · reaches no implementation{cut}")
            } else {
                let names: Vec<&str> = r.ends.iter().map(|p| p.strip_prefix("uvm_test_top.").unwrap_or(p)).collect();
                let more = if r.more > 0 { format!(" and {} more", r.more) } else { String::new() };
                let partial = if r.incomplete { " (perhaps more: a call on the way not followed)" } else { "" };
                format!("{iface}{what} · {}→ {}{more}{partial}{cut}", if r.possible { "possibly " } else { "" }, names.join(", "))
            }
        }
        Role::Unknown => "endpoint of an unknown kind".to_string(),
    };
    (Some(status), kids)
}

/// The connections a component makes whose receiver is no endpoint of the tree.
pub(super) fn loose_nodes(a: &Analysis, path: &str, l: &Links) -> Vec<VerifNode> {
    let idx = l.loose.get(path).cloned().unwrap_or_default();
    let mut kids: Vec<VerifNode> = idx.iter().take(MAX_LISTED).map(|&k| link_node(a, &l.links[k], None)).collect();
    if idx.len() > MAX_LISTED {
        kids.push(VerifNode { name: format!("… {} more", idx.len() - MAX_LISTED), kind: "connection".into(), ..Default::default() });
    }
    kids
}

/// A connection as a node: `→ other` / `← other`, `✕` when it is no edge, `?` when unresolved;
/// it opens the `connect()` call.
fn link_node(a: &Analysis, x: &Link, seen_from: Option<&str>) -> VerifNode {
    let short = |p: &str| p.strip_prefix("uvm_test_top.").unwrap_or(p).to_string();
    let end = |r: &Result<String, String>, text: &str| match r {
        Ok(p) => short(p),
        Err(_) => text.to_string(),
    };
    let outgoing = seen_from.is_none_or(|p| x.from.as_deref() == Ok(p));
    let (arrow, other) = if outgoing { ("→", end(&x.to, &x.to_text)) } else { ("←", end(&x.from, &x.from_text)) };
    let reason = match (&x.not, &x.from, &x.to) {
        (Some(n), _, _) => Some(n.clone()),
        (None, Err(e), _) | (None, _, Err(e)) => Some(format!("not followed: {e}")),
        _ => None,
    };
    let mark = match (&x.not, x.from.is_ok() && x.to.is_ok()) {
        (Some(_), _) => "✕ ",
        (None, false) => "? ",
        _ => "",
    };
    let name = if seen_from.is_none() { format!("{mark}{} {arrow} {other}", end(&x.from, &x.from_text)) } else { format!("{mark}{arrow} {other}") };
    let loc = a.to_location(x.at);
    let mut notes = Vec::new();
    if let Some(r) = reason {
        notes.push(r);
    }
    if x.conditional {
        notes.push(if x.conditions.is_empty() || x.conditions.iter().any(|c| c == "?") {
            "under conditions that are not known".to_string()
        } else {
            format!("only when {}", x.conditions.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(" && "))
        });
    }
    if x.phase != "connect_phase" && x.not.is_none() {
        notes.push(format!("made in {}", x.phase));
    }
    VerifNode {
        name,
        path: x.by.clone(),
        class: notes.join(" · "),
        kind: "connection".into(),
        uri: loc.as_ref().map(|l| l.uri.to_string()),
        line: loc.as_ref().map_or(0, |l| l.range.start.line),
        character: loc.as_ref().map_or(0, |l| l.range.start.character),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_interfaces_and_operations_from_uvm_class_names() {
        let r = |n: &str| role_of_name(n).map(|(r, i, m)| (r, i, m));
        assert_eq!(r("uvm_analysis_port"), Some((Role::Port, "analysis".into(), WRITE)));
        assert_eq!(r("uvm_analysis_imp_scb"), Some((Role::Imp, "analysis".into(), WRITE)));
        // A `uvm_analysis_imp_decl(_port)` suffix does not make it a port.
        assert_eq!(r("uvm_analysis_imp_port"), Some((Role::Imp, "analysis".into(), WRITE)));
        assert_eq!(r("uvm_blocking_put_export"), Some((Role::Export, "blocking_put".into(), B_PUT)));
        assert_eq!(r("uvm_get_peek_imp"), Some((Role::Imp, "get_peek".into(), B_GET | NB_GET | B_PEEK | NB_PEEK)));
        assert_eq!(r("uvm_seq_item_pull_imp"), Some((Role::Imp, "seq_item_pull".into(), SEQ_ITEM)));
        assert_eq!(r("uvm_port_base"), None);
        assert_eq!(r("my_port"), None);
        // A get port may use a get_peek export; a blocking put port no non-blocking imp.
        let (_, _, get) = role_of_name("uvm_get_port").unwrap();
        let (_, _, get_peek) = role_of_name("uvm_get_peek_export").unwrap();
        assert_eq!(get & !get_peek, 0);
        let (_, _, bput) = role_of_name("uvm_blocking_put_port").unwrap();
        let (_, _, nbput) = role_of_name("uvm_nonblocking_put_imp").unwrap();
        assert_ne!(bput & !nbput, 0);
    }

    #[test]
    fn statement_starts() {
        let t = dvi_syntax::lexer::lex("if (a) begin end ap.connect(x); case (k) 1: ap.connect(y); endcase ap.connect(z); begin : blk ap.connect(w); end get().ap.connect(v);", 0);
        let calls = connect_calls(&t);
        let ok: Vec<bool> = calls.iter().map(|(_, c)| c.src_ok).collect();
        assert_eq!(ok, [true, true, true, true, false]);
        assert_eq!(tokens_text(&calls[4].1.src), "get().ap");
    }
}
