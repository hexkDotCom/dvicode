// Stand-in for the UVM macros used by the demo (see uvm_pkg.sv).
`ifndef DEMO_UVM_MACROS_SVH
`define DEMO_UVM_MACROS_SVH

`define uvm_component_utils(T) typedef uvm_component_registry #(T) type_id;
`define uvm_object_utils(T) typedef uvm_object_registry #(T) type_id;

`endif
