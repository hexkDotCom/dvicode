# objection-without-description

`raise_objection`/`drop_objection` without a description.

- Rulesets: hexk
- Default level: warning
- Style guide: [OBJ-4](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A call of UVM's `raise_objection` or `drop_objection` (on a phase or an objection) whose
description argument is missing or `""`, by position or by name (`.description(...)`).

## Why

The description is what `+UVM_OBJECTION_TRACE` shows when an objection keeps a phase open
(style guide OBJ-4).

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
    task run_phase(uvm_phase phase);
      phase.raise_objection(this);
      phase.drop_objection(this, "test done");
    endtask
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
    task run_phase(uvm_phase phase);
      phase.raise_objection(this, "test running");
      phase.drop_objection(this, "test done");
    endtask
  endclass
endpackage
```

## Waive

One finding: `// OBJ-4 waived: <reason>` on the line before the statement or declaration. The rule: `"objection-without-description": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
