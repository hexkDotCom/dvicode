# create-name-chars

A factory `create` name has characters outside `[a-zA-Z0-9_]` or `__`.

- Rulesets: hexk
- Default level: warning
- Style guide: [NAM-7](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

The name given to UVM's factory `create` as a string literal when it is empty, holds a
character outside `[a-zA-Z0-9_]`, or holds `__`.

## Why

Instance names are parts of hierarchical paths; other characters break config-db glob matching
and tools that read logs (style guide NAM-7).

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
      env = my_env::type_id::create("my env", this);
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

One finding: `// NAM-7 waived: <reason>` on the line before the statement or declaration. The rule: `"create-name-chars": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
