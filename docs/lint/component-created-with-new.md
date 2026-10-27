# component-created-with-new

A component is created with `new` instead of the factory.

- Rulesets: hexk
- Default level: warning
- Style guide: [FAC-1](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`x = new(...)` (or a declaration with `= new`) where `x` is a component: a class derived from
`uvm_component` other than UVM's analysis FIFOs. TLM ports, exports and imps are not
components.

## Why

A component created with `new` cannot be replaced by a factory override from a test (style
guide FAC-1). Sequences, sequence items and configuration objects are also created through the
factory; this rule checks components.

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
      env = new("env", this);
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
      env = my_env::type_id::create("env", this);
    endfunction
  endclass
endpackage
```

## Waive

One finding: `// FAC-1 waived: <reason>` on the line before the statement or declaration. The rule: `"component-created-with-new": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
