// A tiny stand-in for the UVM library so the demo compiles without UVM_HOME. It has the
// class names and the factory shape (`T::type_id::create(name, parent)`) the demo uses;
// point a real project at the real library with -uvm.
package uvm_pkg;
  `include "uvm_macros.svh"

  virtual class uvm_void;
  endclass

  class uvm_phase;
  endclass

  virtual class uvm_object extends uvm_void;
    function new(string name = "");
    endfunction
  endclass

  class uvm_component extends uvm_object;
    function new(string name, uvm_component parent);
      super.new(name);
    endfunction
    virtual function void build_phase(uvm_phase phase);
    endfunction
    virtual task run_phase(uvm_phase phase);
    endtask
  endclass

  class uvm_component_registry #(type T = uvm_component);
    static function T create(string name, uvm_component parent);
      create = new(name, parent);
    endfunction
  endclass

  class uvm_object_registry #(type T = uvm_object);
    static function T create(string name = "", uvm_component parent = null);
      create = new(name);
    endfunction
  endclass

  class uvm_test extends uvm_component;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class uvm_env extends uvm_component;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class uvm_agent extends uvm_component;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class uvm_monitor extends uvm_component;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class uvm_scoreboard extends uvm_component;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class uvm_driver #(type REQ = uvm_object) extends uvm_component;
    `uvm_component_utils(uvm_driver #(REQ))
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  class uvm_sequencer #(type REQ = uvm_object) extends uvm_component;
    `uvm_component_utils(uvm_sequencer #(REQ))
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
  endclass

  virtual class uvm_port_base #(type T = int) extends uvm_object;
    function new(string name, uvm_component parent);
      super.new(name);
    endfunction
  endclass

  class uvm_analysis_port #(type T = int) extends uvm_port_base #(T);
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    function void write(T t);
    endfunction
  endclass

  task run_test(string test_name = "");
  endtask
endpackage
