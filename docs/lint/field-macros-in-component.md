# field-macros-in-component

A `` `uvm_field_* `` macro in a component.

- Rulesets: hexk
- Default level: warning
- Style guide: [FAC-4](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A UVM field macro (`` `uvm_field_int `` and the others) in a component.

## Why

Field macros in a component make it read the configuration database implicitly
(`apply_config_settings`); configuration is explicit in the style guide (FAC-4).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_env extends uvm_env;
    int count;
    `uvm_component_utils_begin(bus_env)
      `uvm_field_int(count, UVM_DEFAULT)
    `uvm_component_utils_end
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_env extends uvm_env;
    int count;
    `uvm_component_utils(bus_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```

## Waive

One finding: `// FAC-4 waived: <reason>` on the line before the statement or declaration. The rule: `"field-macros-in-component": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
