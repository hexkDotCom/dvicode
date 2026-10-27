# default-sequence

`default_sequence` is set.

- Rulesets: hexk
- Default level: warning
- Style guide: [SEQ-11](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`default_sequence` set through UVM's `uvm_config_db::set` (the field name, third) or
`uvm_resource_db::set` (the name, second) as a string literal. The deprecated `set_config_*`
calls and names built at run time are not followed.

## Why

A default sequence starts stimulus implicitly from configuration; tests start their sequences
explicitly (style guide SEQ-11).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      uvm_config_db#(int)::set(this, "env.sqr.main_phase", "default_sequence", 0);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      uvm_config_db#(int)::set(this, "env", "count", 4);
    endfunction
  endclass
endpackage
```

## Waive

One finding: `// SEQ-11 waived: <reason>` on the line before the statement or declaration. The rule: `"default-sequence": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
