//! TLM connections in the Verification Hierarchy: `connect()` calls resolved to ports, exports
//! and imps (also those the UVM library makes), UVM's rules, phases, arrays, and reach.

use dvi_analysis::ide::views::{TreeQuery, VerifNode};
use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn project(name: &str, files: &[(&str, &str)]) -> (Analysis, PathBuf) {
    let root = std::env::temp_dir().join(format!("dvicode-tlm-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    let mut build = String::new();
    for (f, text) in files {
        std::fs::write(root.join(f), text).unwrap();
        build.push_str(f);
        build.push('\n');
    }
    std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
    let cfg = dvi_project::load(&root, None, &FxHashMap::default());
    let a = Analysis::build(Arc::new(Vfs::new()), Arc::new(ProjectSetup::new(&root, cfg, FxHashMap::default())));
    (a, root)
}

const UVM: &str = r#"package uvm_pkg;
  class uvm_object; function new(string name = ""); endfunction endclass
  class uvm_component extends uvm_object; function new(string name, uvm_component parent); super.new(name); endfunction endclass
  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_agent extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_monitor extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_scoreboard extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  virtual class uvm_port_base #(type IF = uvm_object) extends uvm_object;
    function new(string name, uvm_component parent); super.new(name); endfunction
    function void connect(uvm_port_base #(IF) provider); endfunction
  endclass
  class uvm_analysis_port #(type T = int) extends uvm_port_base #(T); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_analysis_export #(type T = int) extends uvm_port_base #(T); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_analysis_imp #(type T = int, type IMP = int) extends uvm_port_base #(T); function new(string name, IMP imp); super.new(name, imp); endfunction endclass
  class uvm_seq_item_pull_port #(type REQ = int, type RSP = REQ) extends uvm_port_base #(REQ); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_seq_item_pull_imp #(type REQ = int, type RSP = REQ, type IMP = int) extends uvm_port_base #(REQ); function new(string name, IMP imp); super.new(name, imp); endfunction endclass
  class uvm_driver #(type REQ = int, type RSP = REQ) extends uvm_component;
    uvm_seq_item_pull_port #(REQ, RSP) seq_item_port;
    uvm_analysis_port #(RSP) rsp_port;
    function new(string name, uvm_component parent); super.new(name, parent); seq_item_port = new("seq_item_port", this); rsp_port = new("rsp_port", this); endfunction
  endclass
  class uvm_sequencer #(type REQ = int, type RSP = REQ) extends uvm_component;
    uvm_seq_item_pull_imp #(REQ, RSP, uvm_sequencer #(REQ, RSP)) seq_item_export;
    function new(string name, uvm_component parent); super.new(name, parent); seq_item_export = new("seq_item_export", this); endfunction
  endclass
  class uvm_tlm_analysis_fifo #(type T = int) extends uvm_component;
    uvm_analysis_imp #(T, uvm_tlm_analysis_fifo #(T)) analysis_export;
    function new(string name, uvm_component parent); super.new(name, parent); analysis_export = new("analysis_export", this); endfunction
  endclass
endpackage
"#;

const TB: &str = r#"package tb_pkg;
  import uvm_pkg::*;
  // What `uvm_analysis_imp_decl(_scb) makes.
  class uvm_analysis_imp_scb #(type T = int, type IMP = int) extends uvm_port_base #(T);
    function new(string name, IMP imp); super.new(name, imp); endfunction
  endclass
  class mon extends uvm_monitor;
    uvm_analysis_port #(int) ap;
    function new(string name, uvm_component parent); super.new(name, parent); ap = new("ap", this); endfunction
  endclass
  class drv extends uvm_driver #(int);
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class agt extends uvm_agent;
    mon m;
    drv d;
    uvm_sequencer #(int) s;
    uvm_analysis_port #(int) ap;
    bit is_active;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      m = mon::type_id::create("m", this);
      d = drv::type_id::create("d", this);
      s = uvm_sequencer #(int)::type_id::create("s", this);
      ap = new("ap", this);
    endfunction
    function void connect_phase(uvm_phase phase);
      m.ap.connect(ap);
      if (is_active) d.seq_item_port.connect(s.seq_item_export);
    endfunction
  endclass
  class scb extends uvm_scoreboard;
    uvm_analysis_imp_scb #(int, scb) in_scb;
    uvm_analysis_imp #(int, scb) other_imp;
    uvm_tlm_analysis_fifo #(int) fifo;
    function new(string name, uvm_component parent);
      super.new(name, parent);
      in_scb = new("in_scb", this);
      other_imp = new("other_imp", this);
    endfunction
    function void build_phase(uvm_phase phase); fifo = new("fifo", this); endfunction
  endclass
  class env extends uvm_env;
    agt agents[2];
    scb sb;
    uvm_analysis_port #(int) open_ap;
    uvm_seq_item_pull_port #(int) pull;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      foreach (agents[i]) agents[i] = agt::type_id::create($sformatf("agt%0d", i), this);
      sb = scb::type_id::create("sb", this);
      open_ap = new("open_ap", this);
      pull = new("pull", this);
    endfunction
    function void connect_phase(uvm_phase phase);
      foreach (agents[i]) agents[i].ap.connect(sb.in_scb);
      agents[0].ap.connect(sb.other_imp);
      sb.in_scb.connect(sb.other_imp);
      open_ap.connect(.provider(sb.fifo.analysis_export));
      pull.connect(sb.other_imp);
      get_port().connect(sb.other_imp);
    endfunction
    function void end_of_elaboration_phase(uvm_phase phase);
      open_ap.connect(sb.other_imp);
    endfunction
  endclass
  class t1 extends uvm_test;
    env e;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); e = env::type_id::create("e", this); endfunction
  endclass
endpackage
"#;

fn find<'a>(n: &'a VerifNode, path: &str) -> Option<&'a VerifNode> {
    if n.path == path && n.kind != "connection" {
        return Some(n);
    }
    n.children.iter().find_map(|c| find(c, path))
}

fn names(n: &VerifNode) -> Vec<String> {
    n.children.iter().filter(|c| c.kind == "connection").map(|c| c.name.clone()).collect()
}

#[test]
fn connections_are_resolved_checked_and_followed() {
    let (a, _) = project("main", &[("uvm_pkg.sv", UVM), ("tb_pkg.sv", TB)]);
    let (h, _) = a.verification_tree(Some("t1"), &TreeQuery::new(10_000));
    let root = &h.roots[0];
    let node = |p: &str| find(root, p).unwrap_or_else(|| panic!("no node {p}"));
    let status = |p: &str| node(p).connection.clone().unwrap_or_default();
    // Child port to the agent's port, then to the scoreboard's imp (a `uvm_*_imp_decl` class).
    assert_eq!(names(node("uvm_test_top.e.agents[i].m.ap")), ["→ e.agents[i].ap"]);
    assert_eq!(status("uvm_test_top.e.agents[i].m.ap"), "analysis port · → e.sb.in_scb");
    assert_eq!(status("uvm_test_top.e.agents[i].ap"), "analysis port · → e.sb.in_scb");
    assert!(status("uvm_test_top.e.sb.in_scb").starts_with("analysis imp · 1 connection to it"), "{}", status("uvm_test_top.e.sb.in_scb"));
    // The library's endpoints: the driver's pull port to the sequencer's imp, only if active.
    assert_eq!(status("uvm_test_top.e.agents[i].d.seq_item_port"), "seq_item_pull port · possibly → e.agents[i].s.seq_item_export");
    let rsp = status("uvm_test_top.e.agents[i].d.rsp_port");
    assert!(rsp.starts_with("analysis port · no connection found (") && rsp.contains("not followed"), "{rsp}");
    // A FIFO's export, given as a named argument.
    assert_eq!(status("uvm_test_top.e.open_ap"), "analysis port · → e.sb.fifo.analysis_export");
    // Rejected and ignored calls are shown, not followed.
    let other = names(node("uvm_test_top.e.sb.other_imp"));
    assert!(other.iter().any(|n| n == "✕ ← e.sb.in_scb"), "{other:?}");
    assert!(other.iter().any(|n| n == "✕ ← e.pull"), "{other:?}");
    assert!(other.iter().any(|n| n == "✕ ← e.open_ap"), "{other:?}");
    let sb_other = node("uvm_test_top.e.sb.other_imp");
    let reasons: Vec<&str> = sb_other.children.iter().map(|c| c.class.as_str()).collect();
    assert!(reasons.iter().any(|r| r.contains("an imp cannot connect")), "{reasons:?}");
    assert!(reasons.iter().any(|r| r.contains("its `analysis` interface does not offer what `seq_item_pull` calls")), "{reasons:?}");
    assert!(reasons.iter().any(|r| r.contains("end_of_elaboration_phase")), "{reasons:?}");
    // Another element of the array, and a receiver that is no chain: not followed, under the
    // component that makes the call.
    let loose = names(node("uvm_test_top.e"));
    assert!(loose.iter().any(|n| n.starts_with("? agents[0].ap → ")), "{loose:?}");
    assert!(loose.iter().any(|n| n.starts_with("? get_port() → ")), "{loose:?}");
    assert_eq!(status("uvm_test_top.e.pull"), format!("seq_item_pull port · no connection found ({} connect() calls not followed)", 2));
}

#[test]
fn without_ports_nothing_is_scanned_or_shown() {
    let (a, _) = project("noports", &[("uvm_pkg.sv", UVM), ("tb_pkg.sv", TB)]);
    let mut q = TreeQuery::new(10_000);
    q.ports = false;
    let (h, _) = a.verification_tree(Some("t1"), &q);
    fn any_conn(n: &VerifNode) -> bool {
        n.kind == "connection" || n.kind == "port" || n.connection.is_some() || n.children.iter().any(any_conn)
    }
    assert!(!any_conn(&h.roots[0]));
}

const UVM2: &str = r#"package uvm_pkg;
  class uvm_object; function new(string name = ""); endfunction endclass
  class uvm_component extends uvm_object; function new(string name, uvm_component parent); super.new(name); endfunction endclass
  class uvm_test extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_env extends uvm_component; function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  virtual class uvm_port_base #(type IF = uvm_object) extends uvm_object;
    function new(string name, uvm_component parent); super.new(name); endfunction
    function void connect(uvm_port_base #(IF) provider); endfunction
  endclass
  class uvm_analysis_port #(type T = int) extends uvm_port_base #(T); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_analysis_export #(type T = int) extends uvm_port_base #(T); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_analysis_imp #(type T = int, type IMP = int) extends uvm_port_base #(T); function new(string name, IMP imp); super.new(name, imp); endfunction endclass
  class uvm_blocking_put_port #(type T = int) extends uvm_port_base #(T); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_get_port #(type T = int) extends uvm_port_base #(T); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_put_imp #(type T = int, type IMP = int) extends uvm_port_base #(T); function new(string name, IMP imp); super.new(name, imp); endfunction endclass
  class uvm_get_peek_imp #(type T = int, type IMP = int) extends uvm_port_base #(T); function new(string name, IMP imp); super.new(name, imp); endfunction endclass
  class uvm_tlm_fifo_base #(type T = int) extends uvm_component;
    uvm_put_imp #(T, uvm_tlm_fifo_base #(T)) put_export, blocking_put_export;
    uvm_get_peek_imp #(T, uvm_tlm_fifo_base #(T)) get_peek_export, get_export;
    function new(string name, uvm_component parent);
      super.new(name, parent);
      put_export = new("put_export", this); blocking_put_export = put_export;
      get_peek_export = new("get_peek_export", this); get_export = get_peek_export;
    endfunction
  endclass
  class uvm_tlm_fifo #(type T = int) extends uvm_tlm_fifo_base #(T); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
  class uvm_sequencer_param_base #(type REQ = int, type RSP = REQ) extends uvm_component;
    uvm_analysis_export #(RSP) rsp_export;
    function new(string name, uvm_component parent); super.new(name, parent); rsp_export = new("rsp_export", this); endfunction
  endclass
  class uvm_sequencer #(type REQ = int, type RSP = REQ) extends uvm_sequencer_param_base #(REQ, RSP); function new(string name, uvm_component parent); super.new(name, parent); endfunction endclass
endpackage
"#;

const TB2: &str = r#"package tb2;
  import uvm_pkg::*;
  class base_c extends uvm_component;
    uvm_analysis_port #(int) ap;
    function new(string name, uvm_component parent); super.new(name, parent); ap = new("ap", this); endfunction
  endclass
  class sink extends uvm_component;
    uvm_analysis_imp #(int, sink) imp;
    function new(string name, uvm_component parent); super.new(name, parent); imp = new("imp", this); endfunction
  endclass
  class derived_c extends base_c;
    sink s;
    uvm_analysis_port #(int) fixed[2];
    uvm_tlm_fifo #(int) f;
    uvm_blocking_put_port #(int) bput;
    uvm_get_port #(int) get;
    uvm_sequencer #(int) sqr;
    uvm_analysis_port #(int) rsp, loop_ap, self_ap, open_ap;
    int n;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      s = sink::type_id::create("s", this);
      fixed[0] = new("first", this);
      f = new("f", this);
      bput = new("bput", this);
      get = new("get", this);
      sqr = uvm_sequencer #(int)::type_id::create("sqr", this);
      rsp = new("rsp", this);
      loop_ap = new("loop_ap", this);
      self_ap = new("self_ap", this);
      open_ap = new("open_ap", this);
    endfunction
    function void connect_phase(uvm_phase phase);
      super.ap.connect(s.imp);
      fixed[0].connect(s.imp);
      bput.connect(f.blocking_put_export);
      get.connect(f.get_export);
      rsp.connect(sqr.rsp_export);
      while (n > 0) loop_ap.connect(s.imp);
      self_ap.connect(self_ap);
      open_ap.connect(get_imp());
    endfunction
    function void start_of_simulation_phase(uvm_phase phase);
      rsp.connect(s.imp);
    endfunction
  endclass
  class t2 extends uvm_test;
    derived_c c;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); c = derived_c::type_id::create("c", this); endfunction
    function void connect_phase(uvm_phase phase); get_port().connect(get_imp()); endfunction
  endclass
endpackage
"#;

#[test]
fn review_cases_of_connections() {
    let (a, _) = project("review", &[("uvm_pkg.sv", UVM2), ("tb2.sv", TB2)]);
    let (h, _) = a.verification_tree(Some("t2"), &TreeQuery::new(10_000));
    let root = &h.roots[0];
    let status = |p: &str| find(root, p).unwrap_or_else(|| panic!("no node {p}")).connection.clone().unwrap_or_default();
    // Through `super.`, and an element made with a constant select (named otherwise).
    assert_eq!(status("uvm_test_top.c.ap"), "analysis port · → c.s.imp");
    assert_eq!(status("uvm_test_top.c.first"), "analysis port · → c.s.imp");
    // A FIFO's put and get/peek exports, by any of their handles: one endpoint each; a get port
    // may use a get_peek imp, a blocking put port a put imp.
    assert_eq!(status("uvm_test_top.c.bput"), "blocking_put port · → c.f.put_export");
    assert_eq!(status("uvm_test_top.c.get"), "get port · → c.f.get_peek_export");
    // The sequencer's response export, handled inside the library.
    assert_eq!(status("uvm_test_top.c.rsp"), "analysis port · → c.sqr.rsp_export");
    assert_eq!(status("uvm_test_top.c.sqr.rsp_export"), "analysis export · handled inside the UVM library");
    // A loop that may not run: possibly.
    assert_eq!(status("uvm_test_top.c.loop_ap"), "analysis port · possibly → c.s.imp");
    // Itself: rejected; to a call: not known, and not counted as a connection.
    assert!(status("uvm_test_top.c.self_ap").starts_with("analysis port · no connection found"), "{}", status("uvm_test_top.c.self_ap"));
    assert!(status("uvm_test_top.c.open_ap").starts_with("analysis port · no connection found"), "{}", status("uvm_test_top.c.open_ap"));
    // super.ap, fixed[0], loop_ap (the late call is ignored).
    assert_eq!(status("uvm_test_top.c.s.imp"), "analysis imp · 3 connections to it");
    let self_links = names(find(root, "uvm_test_top.c.self_ap").unwrap());
    assert_eq!(self_links, ["✕ → c.self_ap"], "one row, rejected");
    // The test's own unfollowed call is shown under it.
    assert!(names(root).iter().any(|n| n.starts_with("? get_port() → ")), "{:?}", names(root));
    // A late phase's call is ignored with objects off too (all late phases are scanned).
    let reasons: Vec<String> = find(root, "uvm_test_top.c.s.imp").unwrap().children.iter().map(|c| c.class.clone()).collect();
    assert!(reasons.iter().any(|r| r.contains("start_of_simulation_phase")), "{reasons:?}");
}

#[test]
fn connections_respect_filters_targets_and_limits() {
    let (a, _) = project("limits", &[("uvm_pkg.sv", UVM2), ("tb2.sv", TB2)]);
    // A filter that matches nothing keeps no port for its connections.
    let mut q = TreeQuery::new(10_000);
    q.filter = dvi_analysis::ide::filter::Filter::new("nothing_matches_this");
    let (h, _) = a.verification_tree(Some("t2"), &q);
    assert!(h.roots.is_empty() || h.roots[0].children.is_empty(), "{:?}", h.roots.first().map(|r| r.children.len()));
    // The node limit counts connections.
    let (h, meta) = a.verification_tree(Some("t2"), &TreeQuery::new(5));
    fn count(n: &VerifNode) -> usize {
        n.children.len() + n.children.iter().map(count).sum::<usize>()
    }
    assert!(count(&h.roots[0]) <= 5 && meta.incomplete.is_some());
    // A class search finds a library endpoint.
    let mut q = TreeQuery::new(10_000);
    q.target = Some(dvi_analysis::ide::views::Target { class: Some("uvm_put_imp".into()), ..Default::default() });
    let (h, _) = a.verification_tree(Some("t2"), &q);
    assert!(find(&h.roots[0], "uvm_test_top.c.f.put_export").is_some());
}
