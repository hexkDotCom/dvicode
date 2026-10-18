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
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      mon = demo_monitor::type_id::create("mon", this);
      drv = demo_driver::type_id::create("drv", this);
      sqr = uvm_sequencer #(uvm_object)::type_id::create("sqr", this);
    endfunction
  endclass

  class demo_scoreboard extends uvm_scoreboard;
    `uvm_component_utils(demo_scoreboard)
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class demo_env extends uvm_env;
    `uvm_component_utils(demo_env)
    demo_agent      agents[2];
    demo_scoreboard sb;
    demo_cfg        cfg;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    extern function void build_phase(uvm_phase phase);
  endclass

  function void demo_env::build_phase(uvm_phase phase);
    super.build_phase(phase);
    cfg = demo_cfg::type_id::create("cfg", this);
    foreach (agents[i])
      agents[i] = demo_agent::type_id::create($sformatf("agent%0d", i), this);
    sb = new("sb", this);
  endfunction

  class demo_base_test extends uvm_test;
    `uvm_component_utils(demo_base_test)
    demo_env env;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    virtual function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = demo_env::type_id::create("env", this);
    endfunction
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
endpackage
