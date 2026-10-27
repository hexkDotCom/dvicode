# message-id-get-name

A report message ID is `get_name()`.

- Rulesets: hexk
- Default level: warning
- Style guide: [MSG-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A UVM report macro (`` `uvm_info `` and the others, `_context` forms too) whose message ID is
`get_name()` (with `this.`, `super.` or another receiver).

## Why

Instance names depend on where a class is instantiated and are already printed in the message
header; IDs should be static and functional, or `get_type_name()` (style guide MSG-2).

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
    function void report_phase(uvm_phase phase);
      super.report_phase(phase);
      `uvm_info(get_name(), "done", UVM_LOW)
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
    function void report_phase(uvm_phase phase);
      super.report_phase(phase);
      `uvm_info("TEST_DONE", "done", UVM_LOW)
    endfunction
  endclass
endpackage
```

## Waive

One finding: `// MSG-2 waived: <reason>` on the line before the statement or declaration. The rule: `"message-id-get-name": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
