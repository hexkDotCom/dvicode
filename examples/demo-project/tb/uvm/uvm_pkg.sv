// A tiny stand-in for the UVM library so the demo compiles without UVM_HOME. It has the
// class names and the factory shape (`T::type_id::create(name, parent)`) the demo uses;
// point a real project at the real library with -uvm.
package uvm_pkg;
  `include "uvm_macros.svh"

  virtual class uvm_void;
  endclass

  class uvm_phase;
  endclass

  typedef enum { UVM_PASSIVE, UVM_ACTIVE } uvm_active_passive_enum;

  // What `T::type_id::get()` and `T::get_type()` return.
  virtual class uvm_object_wrapper;
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
    virtual function void connect_phase(uvm_phase phase);
    endfunction
    virtual task run_phase(uvm_phase phase);
    endtask
    virtual task main_phase(uvm_phase phase);
    endtask
    function void set_type_override_by_type(uvm_object_wrapper original_type, uvm_object_wrapper override_type, bit replace = 1);
    endfunction
  endclass

  class uvm_component_registry #(type T = uvm_component) extends uvm_object_wrapper;
    static function T create(string name, uvm_component parent);
      create = new(name, parent);
    endfunction
    static function uvm_object_wrapper get();
    endfunction
  endclass

  class uvm_object_registry #(type T = uvm_object) extends uvm_object_wrapper;
    static function T create(string name = "", uvm_component parent = null);
      create = new(name);
    endfunction
    static function uvm_object_wrapper get();
    endfunction
  endclass

  class uvm_config_db #(type T = int);
    static function void set(uvm_component cntxt, string inst_name, string field_name, T value);
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
    uvm_active_passive_enum is_active = UVM_ACTIVE;
    function new(string name, uvm_component parent);
      super.new(name, parent);
    endfunction
    virtual function uvm_active_passive_enum get_is_active();
      return is_active;
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

  class uvm_sequence_item extends uvm_object;
    function new(string name = "");
      super.new(name);
    endfunction
  endclass

  virtual class uvm_sequence_base extends uvm_sequence_item;
    uvm_component m_sequencer;
    function new(string name = "");
      super.new(name);
    endfunction
    virtual task body();
    endtask
    virtual task start(uvm_component sequencer, uvm_sequence_base parent_sequence = null);
    endtask
  endclass

  virtual class uvm_sequence #(type REQ = uvm_sequence_item) extends uvm_sequence_base;
    function new(string name = "");
      super.new(name);
    endfunction
  endclass

  virtual class uvm_reg_block extends uvm_object;
    function new(string name = "");
      super.new(name);
    endfunction
  endclass

  virtual class uvm_reg_adapter extends uvm_object;
    function new(string name = "");
      super.new(name);
    endfunction
  endclass

  task run_test(string test_name = "");
  endtask
endpackage
