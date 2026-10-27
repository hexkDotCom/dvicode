# display-in-uvm-class

`$display` in a class, or another reporting system task in a UVM class.

- Rulesets: hexk
- Default level: warning
- Style guide: [MSG-1](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`$display` (and its `b`, `h`, `o` forms) in any class; in a class derived from a UVM class,
also `$write`, `$monitor`, `$strobe` (and their forms) and `$info`, `$warning`, `$error`,
`$fatal`.

## Why

UVM classes report with the UVM report macros, which the test can filter, count, redirect and
stop on; `$display` bypasses all of that (style guide MSG-1).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void report_phase(uvm_phase phase);
      $display("done");
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void report_phase(uvm_phase phase);
      `uvm_info("ENV", "done", UVM_LOW)
    endfunction
  endclass
endpackage
```

## Waive

One finding: `// MSG-1 waived: <reason>` on the line before the statement or declaration. The rule: `"display-in-uvm-class": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
