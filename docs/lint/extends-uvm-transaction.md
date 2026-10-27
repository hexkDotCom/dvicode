# extends-uvm-transaction

A class extends `uvm_transaction`.

- Rulesets: hexk
- Default level: warning
- Style guide: [TRN-1](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A class whose base is UVM's `uvm_transaction`.

## Why

Sequence items extend `uvm_sequence_item`, which sequences and sequencers work with (style guide
TRN-1).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_transaction;
    function new(string name = "bus_item"); super.new(name); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    function new(string name = "bus_item"); super.new(name); endfunction
  endclass
endpackage
```

## Waive

One finding: `// TRN-1 waived: <reason>` on the line before the statement or declaration. The rule: `"extends-uvm-transaction": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
