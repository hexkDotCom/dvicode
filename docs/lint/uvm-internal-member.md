# uvm-internal-member

A UVM internal member (`m_...`) is used.

- Rulesets: hexk
- Default level: warning
- Style guide: [FAC-11](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A reference to a member of the UVM library whose name starts with `m_` (UVM's internals),
except the documented `m_sequencer`.

## Why

UVM's internals change between versions without notice (style guide FAC-11).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    `uvm_object_utils(bus_item)
    function new(string name = "bus_item"); super.new(name); endfunction
    function string label(); return m_name; endfunction
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
    function string label(); return get_name(); endfunction
  endclass
endpackage
```

## Waive

One finding: `// FAC-11 waived: <reason>` on the line before the statement or declaration. The rule: `"uvm-internal-member": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
