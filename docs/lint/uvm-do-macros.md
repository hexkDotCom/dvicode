# uvm-do-macros

A `` `uvm_do ``, `` `uvm_create ``, `` `uvm_send `` or `` `uvm_rand_send `` macro.

- Rulesets: hexk
- Default level: warning
- Style guide: [FAC-7](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

Calls of UVM's `` `uvm_do ``, `` `uvm_create ``, `` `uvm_send `` and `` `uvm_rand_send `` macro
families. Project macros that wrap them are not followed.

## Why

They hide `create`, `start_item`, `randomize` (and whether it succeeded) and `finish_item`;
written out, each step is visible and the randomization is checked (style guide FAC-7).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence #(uvm_sequence_item);
    uvm_sequence_item req;
    task body();
      `uvm_do(req)
    endtask
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence #(uvm_sequence_item);
    task body();
    endtask
  endclass
endpackage
```

## Waive

One finding: `// FAC-7 waived: <reason>` on the line before the statement or declaration. The rule: `"uvm-do-macros": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
