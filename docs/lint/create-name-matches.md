# create-name-matches

A factory `create` name differs from the variable it is assigned to.

- Rulesets: hexk
- Default level: warning
- Style guide: [NAM-8](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`x = T::type_id::create("y", ...)` where the literal name is not the name of the variable it
is assigned to. Array elements and names built at run time are not checked.

## Why

Log paths then map directly to code (style guide NAM-8).

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
      env = my_env::type_id::create("environment", this);
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

One finding: `// NAM-8 waived: <reason>` on the line before the statement or declaration. The rule: `"create-name-matches": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
