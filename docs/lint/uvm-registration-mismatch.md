# uvm-registration-mismatch

A UVM registration macro does not fit its class.

- Rulesets: recommended
- Default level: warning
- Style guide: [FAC-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)
- Subchecks: `other-class`, `family`, `param`, `abstract`

## What it finds

A UVM registration macro (`` `uvm_object_utils ``, `` `uvm_component_utils `` and their
`_param`, `_abstract` and `_begin` forms) that does not fit the class it is written in:

- `other-class`: it names another class;
- `family`: the object macro on a component, or the component macro on an object;
- `param`: a name-registering macro on a parameterized class;
- `abstract`: a concrete macro on a `virtual class`, where the UVM library has abstract
  registration (IEEE 1800.2).

## Why

The factory creates what the macro registers. Another class's name makes the factory create
that class; the wrong family or form fails to compile only where the factory is first used, or
registers a parameterized class under one name for every specialization.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    `uvm_object_utils(demo_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    `uvm_component_utils(demo_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```

## Fix

`family` has a quick fix (the macro of the other family, same form); `param` offers the
`_param` macro as a suggestion (it changes how the class is found by name).

## Waive

One finding: `// FAC-2 waived: <reason>` on the line before the statement or declaration. The rule: `"uvm-registration-mismatch": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
