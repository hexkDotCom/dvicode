# uvm-class-not-registered

A concrete UVM class is not registered with the factory.

- Rulesets: hexk
- Default level: warning
- Style guide: [FAC-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A concrete (not `virtual`) class derived from `uvm_object` (components included, TLM ports
not) with no UVM registration macro and no `type_id` of its own. When a project macro in the
class body may register it, the class is counted as unknown instead.

## Why

Only registered classes can be created and overridden through the factory (FAC-1, FAC-2).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    function new(string name = "bus_item"); super.new(name); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    `uvm_object_utils(bus_item)
    function new(string name = "bus_item"); super.new(name); endfunction
  endclass
endpackage
```

## Waive

One finding: `// FAC-2 waived: <reason>` on the line before the statement or declaration. The rule: `"uvm-class-not-registered": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
