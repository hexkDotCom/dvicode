# virtual-on-override

`virtual` is repeated on an override of a UVM virtual method.

- Rulesets: hexk
- Default level: warning
- Style guide: [CLS-4](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`virtual` written on a method that overrides a virtual method of the UVM library
(`build_phase`, `do_copy`, ...), directly or through project classes.

## Why

The override is virtual anyway; the style guide keeps the keyword for new methods that
extensions may override, so that it says something (CLS-4).

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
    virtual function void build_phase(uvm_phase phase);
      super.build_phase(phase);
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
    endfunction
  endclass
endpackage
```

## Fix

The quick fix removes `virtual`.

## Waive

One finding: `// CLS-4 waived: <reason>` on the line before the statement or declaration. The rule: `"virtual-on-override": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
