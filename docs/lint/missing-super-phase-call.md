# missing-super-phase-call

A function phase does not call `super.<phase>(phase)` first.

- Rulesets: hexk
- Default level: warning
- Style guide: [CMP-4](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A function phase of a component (`build_phase`, `connect_phase`, `end_of_elaboration_phase`,
`start_of_simulation_phase`, `extract_phase`, `check_phase`, `report_phase`, `final_phase`)
whose first statement, after declarations, is not an unconditional `super.<phase>(phase)`.
Task phases are not checked (CMP-4 depends on what the parent's phase does), nor overrides of
a pure virtual method.

## Why

The parent's phase work (UVM's own, or a base class's) is skipped, or runs after the derived
class's (style guide CMP-4).

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
      env = my_env::type_id::create("env", this);
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

## Fix

When the method calls no `super.<phase>` at all, the quick fix inserts the call first.

## Waive

One finding: `// CMP-4 waived: <reason>` on the line before the statement or declaration. The rule: `"missing-super-phase-call": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
