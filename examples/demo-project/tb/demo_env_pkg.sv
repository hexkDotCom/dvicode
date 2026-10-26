// UVM-style environment and tests (shown in the Verification Hierarchy view).
package demo_env_pkg;
  import uvm_pkg::*;
  `include "uvm_macros.svh"

  // Agent configuration (an object, not a component).
  class demo_cfg extends uvm_object;
    `uvm_object_utils(demo_cfg)
    int unsigned n_agents = 2;
    function new(string name = "demo_cfg");
      super.new(name);
    endfunction
  endclass

  class demo_driver extends uvm_driver #(uvm_object);
    `uvm_component_utils(demo_driver)
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class demo_monitor extends uvm_monitor;
    `uvm_component_utils(demo_monitor)
    uvm_analysis_port #(int) ap;
    function new(string name, uvm_component parent);
      super.new(name, parent);
      ap = new("ap", this);
    endfunction
  endclass

  class demo_agent extends uvm_agent;
    `uvm_component_utils(demo_agent)
    demo_driver  drv;
    demo_monitor mon;
    uvm_sequencer #(uvm_object) sqr;
    demo_cfg     cfg;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      // Set by the environment (hover `get` to see where).
      void'(uvm_config_db #(demo_cfg)::get(this, "", "cfg", cfg));
      mon = demo_monitor::type_id::create("mon", this);
      // A passive agent only monitors.
      if (get_is_active() == UVM_ACTIVE) begin
        drv = demo_driver::type_id::create("drv", this);
        sqr = uvm_sequencer #(uvm_object)::type_id::create("sqr", this);
      end
    endfunction
    function void connect_phase(uvm_phase phase);
      // Ports show what they are connected to (Verification Hierarchy, ports shown).
      if (get_is_active() == UVM_ACTIVE)
        drv.seq_item_port.connect(sqr.seq_item_export);
    endfunction
  endclass

  class demo_scoreboard extends uvm_scoreboard;
    `uvm_component_utils(demo_scoreboard)
    uvm_analysis_imp #(int, demo_scoreboard) item_imp;
    function new(string name, uvm_component parent);
      super.new(name, parent);
      item_imp = new("item_imp", this);
    endfunction
    function void write(int t);
    endfunction
  endclass

  // The register model (Register Model view) and the adapter to its bus (UVM objects).
  class demo_ctrl_reg extends uvm_reg;
    `uvm_object_utils(demo_ctrl_reg)
    rand uvm_reg_field enable, mode;
    function new(string name = "demo_ctrl_reg");
      super.new(name, 32, 0);
    endfunction
    virtual function void build();
      enable = uvm_reg_field::type_id::create("enable");
      enable.configure(this, 1, 0, "RW", 0, 1'b0, 1, 1, 0);
      mode = uvm_reg_field::type_id::create("mode");
      mode.configure(this, 2, 4, "RW", 0, 2'h1, 1, 1, 0);
    endfunction
  endclass

  class demo_status_reg extends uvm_reg;
    `uvm_object_utils(demo_status_reg)
    uvm_reg_field level;
    function new(string name = "demo_status_reg");
      super.new(name, 32, 0);
    endfunction
    virtual function void build();
      level = uvm_reg_field::type_id::create("level");
      level.configure(this, 8, 0, "RO", 1, 8'h0, 1, 0, 0);
    endfunction
  endclass

  class demo_regs extends uvm_reg_block;
    `uvm_object_utils(demo_regs)
    rand demo_ctrl_reg   ctrl;
    rand demo_status_reg status[2];
    function new(string name = "demo_regs");
      super.new(name);
    endfunction
    virtual function void build();
      default_map = create_map("bus", 'h4000, 4, UVM_LITTLE_ENDIAN);
      ctrl = demo_ctrl_reg::type_id::create("ctrl");
      ctrl.configure(this);
      ctrl.build();
      default_map.add_reg(ctrl, 'h0, "RW");
      foreach (status[i]) begin
        status[i] = demo_status_reg::type_id::create($sformatf("status%0d", i));
        status[i].configure(this);
        status[i].build();
        default_map.add_reg(status[i], 'h10 + i * 4, "RO");
      end
      lock_model();
    endfunction
  endclass

  class demo_reg_adapter extends uvm_reg_adapter;
    `uvm_object_utils(demo_reg_adapter)
    function new(string name = "demo_reg_adapter");
      super.new(name);
    endfunction
  endclass

  class demo_env extends uvm_env;
    `uvm_component_utils(demo_env)
    demo_agent      agents[2];
    demo_scoreboard sb;
    demo_cfg        cfg;
    demo_regs       regs;
    demo_reg_adapter adapter;
    uvm_sequencer #(uvm_object) vsqr;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    extern function void build_phase(uvm_phase phase);
    virtual function void connect_phase(uvm_phase phase);
      foreach (agents[i])
        agents[i].mon.ap.connect(sb.item_imp);
      adapter = demo_reg_adapter::type_id::create("adapter");
    endfunction
  endclass

  function void demo_env::build_phase(uvm_phase phase);
    super.build_phase(phase);
    cfg = demo_cfg::type_id::create("cfg", this);
    uvm_config_db #(demo_cfg)::set(this, "agent*", "cfg", cfg);
    foreach (agents[i])
      agents[i] = demo_agent::type_id::create($sformatf("agent%0d", i), this);
    sb = new("sb", this);
    regs = demo_regs::type_id::create("regs");
    vsqr = uvm_sequencer #(uvm_object)::type_id::create("vsqr", this);
  endfunction

  // Sequences: a virtual sequence starts two others on the sequencer it runs on.
  class demo_item_seq extends uvm_sequence #(uvm_object);
    `uvm_object_utils(demo_item_seq)
    function new(string name = "demo_item_seq");
      super.new(name);
    endfunction
  endclass

  class demo_vseq extends uvm_sequence #(uvm_object);
    `uvm_object_utils(demo_vseq)
    demo_item_seq first, second;
    function new(string name = "demo_vseq");
      super.new(name);
    endfunction
    virtual task body();
      `uvm_do(first)
      `uvm_do(second)
    endtask
  endclass

  class demo_base_test extends uvm_test;
    `uvm_component_utils(demo_base_test)
    demo_env env;
    demo_vseq vseq;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    virtual function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = demo_env::type_id::create("env", this);
      uvm_config_db #(uvm_object_wrapper)::set(this, "env.vsqr.main_phase", "default_sequence", demo_item_seq::type_id::get());
    endfunction
    virtual task run_phase(uvm_phase phase);
      vseq = demo_vseq::type_id::create("vseq");
      vseq.start(env.vsqr);
    endtask
  endclass

  // Inherits the environment of the base test.
  class demo_smoke_test extends demo_base_test;
    `uvm_component_utils(demo_smoke_test)
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  // Adds a second scoreboard.
  class demo_full_test extends demo_base_test;
    `uvm_component_utils(demo_full_test)
    demo_scoreboard extra_sb;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    virtual function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      extra_sb = demo_scoreboard::type_id::create("extra_sb", this);
    endfunction
  endclass

  // A driver that injects errors, and the test that puts it in every agent.
  class demo_err_driver extends demo_driver;
    `uvm_component_utils(demo_err_driver)
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class demo_err_test extends demo_base_test;
    `uvm_component_utils(demo_err_test)
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    virtual function void build_phase(uvm_phase phase);
      set_type_override_by_type(demo_driver::get_type(), demo_err_driver::get_type());
      super.build_phase(phase);
    endfunction
  endclass
endpackage
