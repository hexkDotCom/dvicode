# direct-report-call

`uvm_report_info/warning/error/fatal` is called directly.

- Rulesets: hexk
- Default level: warning
- Style guide: [FAC-9](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A call of UVM's `uvm_report_info`, `uvm_report_warning`, `uvm_report_error` or
`uvm_report_fatal`. The report macros' own calls are not seen (they are made by the macro
expansion); a project macro that calls them is not followed.

## Why

The report macros skip building the message when its verbosity filters it out; a direct call
always builds it (style guide FAC-9).

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
      uvm_report_info("TEST", "done");
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
      `uvm_info("TEST", "done", UVM_LOW)
    endfunction
  endclass
endpackage
```

## Waive

One finding: `// FAC-9 waived: <reason>` on the line before the statement or declaration. The rule: `"direct-report-call": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
