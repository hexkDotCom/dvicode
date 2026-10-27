# pre-do-override

`pre_do`, `mid_do` or `post_do` is overridden.

- Rulesets: hexk
- Default level: info
- Style guide: [SEQ-14](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

An override of UVM's sequence hooks `pre_do`, `mid_do` or `post_do` (an information).

## Why

These hooks run inside UVM's item flow; the style guide allows overriding them only to change a
sequence that cannot be edited, together with a factory override (SEQ-14). Waive the finding
where that is the case.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_base;
    `uvm_object_utils(bus_seq)
    function new(string name = "bus_seq"); super.new(name); endfunction
    task pre_do(bit is_item); endtask
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_base;
    `uvm_object_utils(bus_seq)
    function new(string name = "bus_seq"); super.new(name); endfunction
  endclass
endpackage
```

## Waive

One finding: `// SEQ-14 waived: <reason>` on the line before the statement or declaration. The rule: `"pre-do-override": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
